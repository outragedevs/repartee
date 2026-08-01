//! Glue between `src/translate/` and the `App` event loop.
//!
//! Shaped after `src/app/shrink.rs`, which solves the same problem for URL
//! shortening: a dispatch gate inside the `add_message` path, background
//! workers, and a deliver channel drained by the main `select!`. The
//! idioms carried over from there — capturing every state-dependent value
//! at dispatch time, per-message panic isolation, draining the queues when
//! the feature is off so `try_send` never backpressures — were each the fix
//! for a real review finding and are load-bearing.
//!
//! Two deliberate divergences from shrink:
//!
//! * **The incoming worker is concurrent, not serial.** Shrink processes
//!   one message at a time, which is fine because URLs are rare. Every line
//!   goes through translation, so a serial pipeline would make latency
//!   cumulative and build an unbounded backlog on a live channel. Display
//!   order is restored by the reorder queue in `src/translate/queue.rs`,
//!   not by serialising here.
//! * **A panicking backend still produces an outcome.** Shrink drops the
//!   message and moves on. Here a dropped outcome would leave its queue
//!   entry pending until the timeout fires, stalling everything behind it,
//!   so the panic is converted into `Untranslated`.
//!
//! The outgoing worker IS serial, exactly as shrink's is, and for the same
//! reason: it is what keeps two outgoing messages in submission order on
//! the wire.

use std::sync::Arc;

use futures::FutureExt;
use tokio::sync::{Semaphore, mpsc};

use crate::state::buffer::BufferType;
use crate::translate::backend::{SharedBackend, StubBackend};
use crate::translate::{TranslateOutcome, TranslateRequest, UntranslatedReason};

/// An incoming line handed to the translation worker.
#[derive(Debug)]
pub struct PendingTranslate {
    pub buffer_id: String,
    pub req: TranslateRequest,
}

/// An outgoing message handed to the translation worker.
///
/// Every field the deliver path would otherwise re-resolve from `state` is
/// captured here at dispatch time, so a `/nick`, a `/close`, or a mode
/// change during the translation wait cannot produce a local echo
/// inconsistent with what reached the wire. See `PendingOutgoing` in
/// `shrink.rs` for the review findings that established this.
#[derive(Debug)]
pub struct PendingOutgoingTranslate {
    pub conn_id: String,
    pub buffer_id: String,
    pub buffer_name: String,
    pub buffer_type: BufferType,
    pub original_text: String,
    pub req: TranslateRequest,
    pub nick: String,
    pub own_mode: Option<char>,
    pub peer_handle: Option<String>,
    pub show_original: bool,
}

/// Posted by either worker, consumed by the main event loop.
#[derive(Debug)]
pub enum TranslateDeliver {
    /// An incoming line's outcome. The loop folds it into that buffer's
    /// reorder queue and releases whatever that unblocks.
    Incoming {
        buffer_id: String,
        outcome: TranslateOutcome,
    },
    /// An outgoing message's outcome. The loop runs the rest of the send
    /// pipeline — E2E encrypt, IRC send, local echo — or refuses.
    Outgoing(Box<OutgoingTranslateDeliver>),
}

#[derive(Debug)]
pub struct OutgoingTranslateDeliver {
    pub conn_id: String,
    pub buffer_id: String,
    pub buffer_name: String,
    pub buffer_type: BufferType,
    /// Kept for the local echo and for restoring the input line when the
    /// send is refused.
    pub original_text: String,
    pub outcome: TranslateOutcome,
    pub nick: String,
    pub own_mode: Option<char>,
    pub peer_handle: Option<String>,
    pub show_original: bool,
}

/// Channels and shared state the translation workers need. Built once in
/// `App::new` and kept alive for the App's lifetime.
pub struct TranslateRuntime {
    /// `None` when translation is disabled — the gates check this to decide
    /// between dispatching and delivering straight through.
    pub backend: Option<SharedBackend>,
    pub incoming_tx: mpsc::Sender<PendingTranslate>,
    pub outgoing_tx: mpsc::Sender<PendingOutgoingTranslate>,
    pub deliver_tx: mpsc::Sender<TranslateDeliver>,
    pub deliver_rx: mpsc::Receiver<TranslateDeliver>,
}

impl TranslateRuntime {
    /// Build the runtime and spawn the workers.
    ///
    /// This branch ships the stub backend: the mechanism is what is being
    /// built, and the stub proves it end to end with no API key. Swapping
    /// in a real broker is a one-line change here.
    pub fn build(cfg: &crate::config::TranslateConfig) -> Self {
        let backend: Option<SharedBackend> = if cfg.enabled {
            Some(Arc::new(StubBackend::new(0, 0)))
        } else {
            None
        };
        Self::with_backend(backend, cfg)
    }

    /// Build with an explicit backend. Used by tests to inject a stub with
    /// jitter or injected failures.
    pub fn with_backend(backend: Option<SharedBackend>, cfg: &crate::config::TranslateConfig) -> Self {
        let (incoming_tx, incoming_rx) = mpsc::channel::<PendingTranslate>(1024);
        let (outgoing_tx, outgoing_rx) = mpsc::channel::<PendingOutgoingTranslate>(256);
        let (deliver_tx, deliver_rx) = mpsc::channel::<TranslateDeliver>(1024);

        if let Some(ref b) = backend {
            spawn_incoming_worker(
                incoming_rx,
                Arc::clone(b),
                deliver_tx.clone(),
                cfg.max_in_flight.max(1) as usize,
            );
            spawn_outgoing_worker(outgoing_rx, Arc::clone(b), deliver_tx.clone());
        } else {
            // Disabled: drain to nowhere so a `try_send` from the IRC or
            // input paths never backpressures.
            spawn_drain(incoming_rx);
            spawn_drain(outgoing_rx);
        }

        Self {
            backend,
            incoming_tx,
            outgoing_tx,
            deliver_tx,
            deliver_rx,
        }
    }
}

fn spawn_drain<T: Send + 'static>(mut rx: mpsc::Receiver<T>) {
    tokio::spawn(async move { while rx.recv().await.is_some() {} });
}

/// Translate one request, converting a panic into an `Untranslated`
/// outcome.
///
/// Every dispatched line MUST produce exactly one outcome. Dropping it —
/// which is what shrink does on panic — would leave the queue entry pending
/// until the timeout fires, stalling every line behind it for the full
/// budget.
async fn translate_isolated(backend: &SharedBackend, req: TranslateRequest) -> TranslateOutcome {
    let id = req.id;
    let fut = std::panic::AssertUnwindSafe(backend.translate(req));
    fut.catch_unwind().await.unwrap_or_else(|_| {
        tracing::error!(id, "translate: backend panicked on one line");
        TranslateOutcome::Untranslated {
            id,
            reason: UntranslatedReason::Error("backend panic".to_string()),
        }
    })
}

/// Concurrent up to `max_in_flight`. See the module docs for why this one
/// is not serial.
fn spawn_incoming_worker(
    mut rx: mpsc::Receiver<PendingTranslate>,
    backend: SharedBackend,
    deliver: mpsc::Sender<TranslateDeliver>,
    max_in_flight: usize,
) {
    tokio::spawn(async move {
        let permits = Arc::new(Semaphore::new(max_in_flight));
        while let Some(pending) = rx.recv().await {
            let Ok(permit) = Arc::clone(&permits).acquire_owned().await else {
                break;
            };
            let backend = Arc::clone(&backend);
            let deliver = deliver.clone();
            tokio::spawn(async move {
                let _permit = permit;
                let outcome = translate_isolated(&backend, pending.req).await;
                let _ = deliver
                    .send(TranslateDeliver::Incoming {
                        buffer_id: pending.buffer_id,
                        outcome,
                    })
                    .await;
            });
        }
    });
}

/// Serial, so two outgoing messages reach IRC in submission order. Nothing
/// incoming participates: an incoming line stuck behind a slow provider
/// must not delay the user's own message.
fn spawn_outgoing_worker(
    mut rx: mpsc::Receiver<PendingOutgoingTranslate>,
    backend: SharedBackend,
    deliver: mpsc::Sender<TranslateDeliver>,
) {
    tokio::spawn(async move {
        while let Some(pending) = rx.recv().await {
            let outcome = translate_isolated(&backend, pending.req).await;
            let _ = deliver
                .send(TranslateDeliver::Outgoing(Box::new(
                    OutgoingTranslateDeliver {
                        conn_id: pending.conn_id,
                        buffer_id: pending.buffer_id,
                        buffer_name: pending.buffer_name,
                        buffer_type: pending.buffer_type,
                        original_text: pending.original_text,
                        outcome,
                        nick: pending.nick,
                        own_mode: pending.own_mode,
                        peer_handle: pending.peer_handle,
                        show_original: pending.show_original,
                    },
                )))
                .await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::translate::Direction;

    fn cfg(max_in_flight: u32) -> crate::config::TranslateConfig {
        crate::config::TranslateConfig {
            enabled: true,
            max_in_flight,
            ..Default::default()
        }
    }

    fn req(id: u64, text: &str) -> TranslateRequest {
        TranslateRequest {
            id,
            direction: Direction::Incoming,
            network: "libera".to_string(),
            target: "#dupa".to_string(),
            nick: "alice".to_string(),
            text: text.to_string(),
            source_lang: None,
            target_lang: "pl".to_string(),
            known_nicks: Vec::new(),
        }
    }

    #[tokio::test]
    async fn incoming_worker_returns_one_outcome_per_line() {
        let mut rt = TranslateRuntime::with_backend(
            Some(Arc::new(StubBackend::new(0, 0))),
            &cfg(4),
        );
        for id in 1..=3u64 {
            rt.incoming_tx
                .send(PendingTranslate {
                    buffer_id: "libera/#dupa".to_string(),
                    req: req(id, "hello world"),
                })
                .await
                .expect("worker alive");
        }
        let mut seen = Vec::new();
        for _ in 0..3 {
            match rt.deliver_rx.recv().await {
                Some(TranslateDeliver::Incoming { outcome, buffer_id }) => {
                    assert_eq!(buffer_id, "libera/#dupa");
                    seen.push(outcome.id());
                }
                other => panic!("expected an incoming outcome, got {other:?}"),
            }
        }
        seen.sort_unstable();
        assert_eq!(seen, vec![1, 2, 3], "every line gets exactly one outcome");
    }

    #[tokio::test]
    async fn incoming_worker_runs_concurrently() {
        // Four lines at 60 ms each must not take 240 ms — if this worker
        // were serial like shrink's, latency would be cumulative and a busy
        // channel would build an unbounded backlog.
        let mut rt = TranslateRuntime::with_backend(
            Some(Arc::new(StubBackend::with_jitter(&[60]))),
            &cfg(4),
        );
        let started = tokio::time::Instant::now();
        for id in 1..=4u64 {
            rt.incoming_tx
                .send(PendingTranslate {
                    buffer_id: "b".to_string(),
                    req: req(id, "hello world"),
                })
                .await
                .expect("worker alive");
        }
        for _ in 0..4 {
            rt.deliver_rx.recv().await.expect("outcome");
        }
        let elapsed = started.elapsed();
        assert!(
            elapsed < std::time::Duration::from_millis(200),
            "four concurrent 60 ms lines took {elapsed:?} — worker looks serial"
        );
    }

    #[tokio::test]
    async fn outgoing_worker_preserves_submission_order() {
        // The first message is slow, the second fast. Serial dispatch is
        // what keeps them in order on the wire.
        let mut rt = TranslateRuntime::with_backend(
            Some(Arc::new(StubBackend::with_jitter(&[40, 0]))),
            &cfg(4),
        );
        for id in 1..=2u64 {
            rt.outgoing_tx
                .send(PendingOutgoingTranslate {
                    conn_id: "c".to_string(),
                    buffer_id: "b".to_string(),
                    buffer_name: "#dupa".to_string(),
                    buffer_type: BufferType::Channel,
                    original_text: "hello world".to_string(),
                    req: req(id, "hello world"),
                    nick: "me".to_string(),
                    own_mode: None,
                    peer_handle: None,
                    show_original: false,
                })
                .await
                .expect("worker alive");
        }
        let mut order = Vec::new();
        for _ in 0..2 {
            match rt.deliver_rx.recv().await {
                Some(TranslateDeliver::Outgoing(d)) => order.push(d.outcome.id()),
                other => panic!("expected an outgoing deliver, got {other:?}"),
            }
        }
        assert_eq!(order, vec![1, 2], "submission order is preserved");
    }

    #[tokio::test]
    async fn disabled_runtime_drains_instead_of_backpressuring() {
        // With no backend the gates never dispatch, but a stale `try_send`
        // must not fill the channel and block the IRC path.
        let cfg = crate::config::TranslateConfig::default();
        let rt = TranslateRuntime::with_backend(None, &cfg);
        assert!(rt.backend.is_none());
        for id in 0..2000u64 {
            rt.incoming_tx
                .send(PendingTranslate {
                    buffer_id: "b".to_string(),
                    req: req(id, "hello world"),
                })
                .await
                .expect("drain keeps the channel open");
        }
    }
}
