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

/// Everything needed to build one outgoing translation request.
///
/// A struct rather than seven positional parameters: the four `&str`s next
/// to each other were an easy place to transpose a buffer id and a buffer
/// name, and the compiler would not have noticed.
#[derive(Debug, Clone)]
pub struct OutgoingRequest<'a> {
    pub conn_id: &'a str,
    pub buffer_id: &'a str,
    pub buffer_name: &'a str,
    pub buffer_type: &'a BufferType,
    pub nick: &'a str,
    pub text: &'a str,
    /// `true` for `/me`; `text` is then the ACTION's inner prose.
    pub is_action: bool,
    /// The submitting call site's echo intent, carried rather than guessed.
    pub echo: OutgoingEchoPlan,
}

/// The wire payload(s) for one translated outgoing body.
///
/// A plain message is one payload. An ACTION is wrapped in CTCP framing —
/// and wrapping produces ONE string, which a translation longer than its
/// source can push past the byte budget. The send path would then split the
/// whole framed string, so peers would receive a first chunk with an opening
/// delimiter, middle chunks with none, and a last chunk with only the
/// closing one: malformed actions rather than one long one.
///
/// Splitting the BODY and wrapping each chunk keeps every wire line a valid
/// CTCP.
fn wrap_outgoing_body(body: &str, is_action: bool) -> Vec<String> {
    if !is_action {
        return vec![body.to_string()];
    }
    let budget = crate::irc::MESSAGE_MAX_BYTES.saturating_sub(ACTION_WRAPPER_BYTES);
    if body.len() <= budget {
        return vec![format!("\x01ACTION {body}\x01")];
    }
    crate::irc::split_irc_message(body, budget)
        .into_iter()
        .map(|chunk| format!("\x01ACTION {chunk}\x01"))
        .collect()
}

/// Bytes `\x01ACTION ` + `\x01` adds around an action's text.
const ACTION_WRAPPER_BYTES: usize = "\x01ACTION \x01".len();

/// Which client submitted a message.
///
/// Carried so a refusal returns the text to the person who typed it. Without
/// it a message submitted from the browser was restored into the TERMINAL's
/// input line: the web textarea had already been cleared, so the text was
/// lost to its author and surfaced somewhere they were not looking.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum SubmitOrigin {
    #[default]
    Tui,
    Web(String),
    /// A Lua script. Nobody typed this, so there is no composer to restore
    /// it to — putting a script's payload into the user's input line would
    /// hand them text to accidentally send.
    Script,
}

/// What to do with a finished outgoing translation whose target may have
/// been re-keyed while it was running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RedirectVerdict {
    /// Send it — either nothing moved, or the deliver was pointed at where
    /// the conversation is now.
    Proceed,
    /// Refuse. The conversation moved and its window is gone, so the only
    /// name left to address is the abandoned one, which somebody else may
    /// hold.
    Refuse,
}

/// How a translated outgoing message should echo locally.
///
/// Carried from the submitting call site rather than re-derived at delivery.
/// The two paths have genuinely different rules, and guessing from server
/// capabilities alone got both wrong: script sends (which deliberately never
/// echo plaintext) gained an echo, and by-target sends lost the caller's
/// `even_without_encryption` intent.
#[derive(Debug, Clone)]
pub enum OutgoingEchoPlan {
    /// Text typed into the buffer. Echo unless the server echoes it back for
    /// us, and always when the wire was ciphertext (the server echo of
    /// ciphertext is swallowed).
    BufferInput,
    /// A by-target send that supplied a `GatedEcho`. Mirrors
    /// `send_gated_message`'s own rule exactly.
    Gated {
        buffer_id: String,
        message_type: crate::state::buffer::MessageType,
        even_without_encryption: bool,
    },
    /// The caller asked for no local echo at all.
    None,
}

/// Outcome of the outgoing translation gate for one submitted line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutgoingTranslatePolicy {
    /// Not a translated buffer — carry on with the ordinary send.
    NotApplicable,
    /// Translate first. Nothing may reach the wire until the outcome is back.
    Translate,
    /// Cannot be translated and must not be sent as-is. The text goes back
    /// to the user with this reason.
    Refuse(&'static str),
}

/// An incoming line handed to the translation worker.
#[derive(Debug)]
pub struct PendingTranslate {
    pub buffer_id: String,
    pub req: TranslateRequest,
    /// When this line entered the pipeline — see `translate_isolated`.
    pub submitted_at: std::time::Instant,
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
    /// `true` for `/me`. The request carries the ACTION's inner text —
    /// translating the `\\x01ACTION …\\x01` framing would corrupt the CTCP —
    /// and the deliver path re-wraps it.
    pub is_action: bool,
    /// The submitting call site's echo intent, carried rather than guessed.
    pub echo: OutgoingEchoPlan,
    /// The message id reserved when the user pressed Enter. The local echo
    /// takes it so it renders at the position it was submitted from, not
    /// after the lines that arrived while it was translating.
    pub echo_id: u64,
    /// Which client submitted this, so a refusal returns the text there.
    pub origin: SubmitOrigin,
    /// The text to hand back if this send is refused — a form that does the
    /// SAME thing when the user presses Enter on it, not the bare body.
    pub retry_text: String,
    /// The bare body, kept alongside `retry_text` so a retry can be
    /// RE-ADDRESSED at restore time. `retry_text` is a form that was correct
    /// when the message was dispatched; where the user is looking by the
    /// time it fails is a different question — see `deferred_retry_text`.
    pub retry_body: String,
    /// When this line entered the pipeline. The whole `timeout_ms` budget
    /// runs from here, so time spent queueing counts against it — see
    /// `translate_isolated`.
    pub submitted_at: std::time::Instant,
    /// Which session of `conn_id` this was written for. Compared at delivery
    /// so a drop-and-reconnect under the same id cannot put this message on
    /// the replacement session. `None` when there was no handle at all.
    pub conn_generation: Option<u64>,
}

/// Posted by either worker, consumed by the main event loop.
#[derive(Debug)]
pub enum TranslateDeliver {
    /// An incoming line's outcome. The loop folds it into that buffer's
    /// reorder queue and releases whatever that unblocks.
    Incoming {
        buffer_id: String,
        outcome: TranslateOutcome,
        /// When this line was dispatched. Decides whether a query rename
        /// that happened since applies to it — see `redirected_buffer_id`.
        submitted_at: std::time::Instant,
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
    /// `true` for `/me`. The request carries the ACTION's inner text —
    /// translating the `\\x01ACTION …\\x01` framing would corrupt the CTCP —
    /// and the deliver path re-wraps it.
    pub is_action: bool,
    /// The submitting call site's echo intent, carried rather than guessed.
    pub echo: OutgoingEchoPlan,
    /// The message id reserved when the user pressed Enter. The local echo
    /// takes it so it renders at the position it was submitted from, not
    /// after the lines that arrived while it was translating.
    pub echo_id: u64,
    /// Which client submitted this, so a refusal returns the text there.
    pub origin: SubmitOrigin,
    /// The text to hand back if this send is refused — a form that does the
    /// SAME thing when the user presses Enter on it, not the bare body.
    pub retry_text: String,
    /// The bare body, kept alongside `retry_text` so a retry can be
    /// RE-ADDRESSED at restore time. `retry_text` is a form that was correct
    /// when the message was dispatched; where the user is looking by the
    /// time it fails is a different question — see `deferred_retry_text`.
    pub retry_body: String,
    /// When this line entered the pipeline. The whole `timeout_ms` budget
    /// runs from here, so time spent queueing counts against it — see
    /// `translate_isolated`.
    pub submitted_at: std::time::Instant,
    /// Which session of `conn_id` this was written for. Compared at delivery
    /// so a drop-and-reconnect under the same id cannot put this message on
    /// the replacement session. `None` when there was no handle at all.
    pub conn_generation: Option<u64>,
}

/// Channels and shared state the translation workers need. Built once in
/// `App::new` and kept alive for the App's lifetime.
pub struct TranslateRuntime {
    /// `None` when translation is disabled — the gates check this to decide
    /// between dispatching and delivering straight through.
    pub backend: Option<SharedBackend>,
    /// The incoming worker's concurrency limiter, shared so
    /// `/set translate.max_in_flight` can retune it without a restart.
    /// Exposed as a plain `/set` option, so it has to actually do something.
    pub in_flight: Arc<Semaphore>,
    /// Per-request budget in milliseconds, shared with the workers so
    /// `/set translate.timeout_ms` retunes them too rather than only the
    /// queue's display-side expiry.
    pub timeout_ms: Arc<std::sync::atomic::AtomicU64>,
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
        let in_flight = Arc::new(Semaphore::new(cfg.max_in_flight.max(1) as usize));
        let timeout_ms = Arc::new(std::sync::atomic::AtomicU64::new(cfg.timeout_ms.max(1)));

        if let Some(ref b) = backend {
            spawn_incoming_worker(
                incoming_rx,
                Arc::clone(b),
                deliver_tx.clone(),
                Arc::clone(&in_flight),
                Arc::clone(&timeout_ms),
            );
            spawn_outgoing_worker(
                outgoing_rx,
                Arc::clone(b),
                deliver_tx.clone(),
                Arc::clone(&in_flight),
                Arc::clone(&timeout_ms),
            );
        } else {
            // Disabled: drain to nowhere so a `try_send` from the IRC or
            // input paths never backpressures.
            spawn_drain(incoming_rx);
            spawn_drain(outgoing_rx);
        }

        Self {
            backend,
            in_flight,
            timeout_ms,
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
async fn translate_isolated(
    backend: &SharedBackend,
    req: TranslateRequest,
    timeout: &Arc<std::sync::atomic::AtomicU64>,
    submitted_at: std::time::Instant,
) -> TranslateOutcome {
    let id = req.id;
    // The budget runs from SUBMISSION, not from the moment this request
    // reaches the backend. A request can sit for a long time first — behind
    // another on its connection's serial lane, or waiting on the shared
    // `max_in_flight` permit — and starting the clock afterwards makes the
    // configured deadline meaningless: the display reservation expires on
    // schedule while the request keeps running, and the answer arrives long
    // after the user gave up. For an outgoing message that means putting it
    // on the channel minutes late, quite possibly after they retyped it.
    //
    // Read now rather than captured at submission, so `/set
    // translate.timeout_ms` retunes work that is already waiting.
    let total =
        std::time::Duration::from_millis(timeout.load(std::sync::atomic::Ordering::Relaxed).max(1));
    let Some(budget) = total.checked_sub(submitted_at.elapsed()) else {
        // The deadline passed while queueing. Calling the provider now buys
        // an answer nobody can use — the incoming queue has already released
        // this line, and an outgoing send is refused on arrival either way.
        tracing::debug!(id, ?total, "translate: deadline passed while queued");
        return TranslateOutcome::Untranslated {
            id,
            reason: UntranslatedReason::Timeout,
        };
    };
    // The boundary has to cover BUILDING the future, not just polling it. A
    // backend that panics synchronously inside `translate()` — before it ever
    // returns its `BoxFuture` — would otherwise unwind past this: the
    // incoming task would produce no outcome at all (its queue entry sitting
    // pending until the timeout), and an outgoing lane would lose the
    // message outright.
    // The await is bounded. A backend future that never resolves would
    // otherwise hang forever: the queue's expiry releases the DISPLAYED row
    // but cannot cancel the task, so each hung incoming request would hold
    // its semaphore permit for the life of the process, and a hung outgoing
    // request would wedge that connection's serial lane permanently.
    let fut = std::panic::AssertUnwindSafe(async {
        tokio::time::timeout(budget, backend.translate(req)).await
    });
    match fut.catch_unwind().await {
        Ok(Ok(outcome)) => single_line_or_refuse(outcome),
        Ok(Err(_elapsed)) => {
            tracing::warn!(id, ?budget, "translate: backend timed out; abandoning it");
            TranslateOutcome::Untranslated {
                id,
                reason: UntranslatedReason::Timeout,
            }
        }
        Err(_) => {
            tracing::error!(id, "translate: backend panicked on one line");
            TranslateOutcome::Untranslated {
                id,
                reason: UntranslatedReason::Error("backend panic".to_string()),
            }
        }
    }
}

/// Reject a translation that is not one line.
///
/// **The backend is untrusted.** Its output goes onto the IRC socket, and
/// `send_privmsg` only breaks on `\r\n` — a bare `\n` is copied into the
/// trailing parameter verbatim, and a server that accepts bare-LF line
/// endings then reads everything after it as a fresh command. A translation
/// of `hello` coming back as `hello\nJOIN #x` would join a channel.
///
/// Trailing whitespace is trimmed rather than refused, because a model that
/// ends its answer with a newline is producing a correct single-line
/// translation with a stray byte on it. Anything embedded is refused: the
/// contract is one line per request (§2), so a multi-line answer is a broken
/// response whatever it says, and guessing which line the user meant is not
/// something to do with text about to be published under their nick.
fn single_line_or_refuse(outcome: TranslateOutcome) -> TranslateOutcome {
    let TranslateOutcome::Translated { id, text } = outcome else {
        return outcome;
    };
    let trimmed = text.trim_end_matches(['\r', '\n']);
    if trimmed.contains(['\r', '\n']) {
        tracing::error!(
            id,
            "translate: backend returned a multi-line answer; refusing it"
        );
        return TranslateOutcome::Untranslated {
            id,
            reason: UntranslatedReason::Error("backend returned multiple lines".to_string()),
        };
    }
    TranslateOutcome::Translated {
        id,
        text: if trimmed.len() == text.len() {
            text
        } else {
            trimmed.to_string()
        },
    }
}

/// Concurrent up to `max_in_flight`. See the module docs for why this one
/// is not serial.
fn spawn_incoming_worker(
    mut rx: mpsc::Receiver<PendingTranslate>,
    backend: SharedBackend,
    deliver: mpsc::Sender<TranslateDeliver>,
    permits: Arc<Semaphore>,
    timeout_ms: Arc<std::sync::atomic::AtomicU64>,
) {
    tokio::spawn(async move {
        while let Some(pending) = rx.recv().await {
            let Ok(permit) = Arc::clone(&permits).acquire_owned().await else {
                break;
            };
            let backend = Arc::clone(&backend);
            let deliver = deliver.clone();
            let timeout_ms = Arc::clone(&timeout_ms);
            tokio::spawn(async move {
                let _permit = permit;
                let outcome =
                    translate_isolated(&backend, pending.req, &timeout_ms, pending.submitted_at)
                        .await;
                let _ = deliver
                    .send(TranslateDeliver::Incoming {
                        buffer_id: pending.buffer_id,
                        outcome,
                        submitted_at: pending.submitted_at,
                    })
                    .await;
            });
        }
    });
}

/// Fan out to one serial worker PER CONNECTION.
///
/// Serial within a connection is what keeps two outgoing messages in
/// submission order on the wire. Serial ACROSS connections would be
/// head-of-line blocking: one hung request on network A would hold up
/// translated sends on network B, which have nothing to do with it. The
/// design calls for a per-connection FIFO for exactly that reason.
///
/// The dispatcher never awaits a per-connection send. A connection whose
/// queue is full resolves as untranslated, so the deliver path refuses that
/// one message (fail-closed) instead of stalling every other connection
/// behind it.
fn spawn_outgoing_worker(
    mut rx: mpsc::Receiver<PendingOutgoingTranslate>,
    backend: SharedBackend,
    deliver: mpsc::Sender<TranslateDeliver>,
    permits: Arc<Semaphore>,
    timeout_ms: Arc<std::sync::atomic::AtomicU64>,
) {
    /// Per-connection queue depth. Deep enough that normal typing never
    /// trips it; shallow enough that a wedged provider surfaces quickly.
    const PER_CONNECTION_QUEUE: usize = 64;

    tokio::spawn(async move {
        let mut lanes: std::collections::HashMap<
            String,
            mpsc::Sender<PendingOutgoingTranslate>,
        > = std::collections::HashMap::new();

        while let Some(pending) = rx.recv().await {
            let conn_id = pending.conn_id.clone();
            let lane = lanes.entry(conn_id.clone()).or_insert_with(|| {
                let (tx, lane_rx) = mpsc::channel(PER_CONNECTION_QUEUE);
                spawn_connection_lane(
                    lane_rx,
                    Arc::clone(&backend),
                    deliver.clone(),
                    Arc::clone(&permits),
                    Arc::clone(&timeout_ms),
                );
                tx
            });
            match lane.try_send(pending) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(p)) => {
                    tracing::warn!(
                        conn_id = %conn_id,
                        "translate: outgoing lane full, refusing this message"
                    );
                    refuse_outgoing(&deliver, p, "the translation queue for this connection is full")
                        .await;
                }
                Err(mpsc::error::TrySendError::Closed(p)) => {
                    // The lane died; drop it so the next message respawns one.
                    lanes.remove(&conn_id);
                    tracing::error!(conn_id = %conn_id, "translate: outgoing lane died");
                    refuse_outgoing(&deliver, p, "the translation worker died").await;
                }
            }
        }
    });
}

/// One connection's serial worker: submission order in, submission order out.
fn spawn_connection_lane(
    mut rx: mpsc::Receiver<PendingOutgoingTranslate>,
    backend: SharedBackend,
    deliver: mpsc::Sender<TranslateDeliver>,
    permits: Arc<Semaphore>,
    timeout_ms: Arc<std::sync::atomic::AtomicU64>,
) {
    tokio::spawn(async move {
        while let Some(pending) = rx.recv().await {
            // The SAME limiter the incoming worker uses.
            // `translate.max_in_flight` is documented as the provider's
            // concurrency cap, and a per-connection lane that skipped it made
            // the real ceiling `max_in_flight + one per connected network` —
            // silently over the limit on exactly the setups that have several
            // networks open.
            //
            // A lane is serial, so this only ever makes it wait; it holds no
            // other permit while acquiring, so nothing can deadlock behind it.
            let Ok(_permit) = Arc::clone(&permits).acquire_owned().await else {
                break;
            };
            let outcome =
                translate_isolated(&backend, pending.req, &timeout_ms, pending.submitted_at).await;
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
                        is_action: pending.is_action,
                        echo: pending.echo,
                        echo_id: pending.echo_id,
                        origin: pending.origin,
                        retry_text: pending.retry_text,
                        retry_body: pending.retry_body,
                        conn_generation: pending.conn_generation,
                        submitted_at: pending.submitted_at,
                    },
                )))
                .await;
        }
    });
}

/// Report a dispatch-side failure as a normal untranslated outcome, so the
/// deliver path refuses the send and hands the text back rather than the
/// message vanishing between the queues.
async fn refuse_outgoing(
    deliver: &mpsc::Sender<TranslateDeliver>,
    pending: PendingOutgoingTranslate,
    reason: &str,
) {
    let _ = deliver
        .send(TranslateDeliver::Outgoing(Box::new(
            OutgoingTranslateDeliver {
                conn_id: pending.conn_id,
                buffer_id: pending.buffer_id,
                buffer_name: pending.buffer_name,
                buffer_type: pending.buffer_type,
                original_text: pending.original_text,
                outcome: TranslateOutcome::Untranslated {
                    id: pending.req.id,
                    reason: UntranslatedReason::Error(reason.to_string()),
                },
                nick: pending.nick,
                own_mode: pending.own_mode,
                peer_handle: pending.peer_handle,
                show_original: pending.show_original,
                is_action: pending.is_action,
                echo: pending.echo,
                echo_id: pending.echo_id,
                origin: pending.origin,
                retry_text: pending.retry_text,
                retry_body: pending.retry_body,
                conn_generation: pending.conn_generation,
                submitted_at: pending.submitted_at,
            },
        )))
        .await;
}

impl crate::app::App {
    /// Drain one translation deliver from the main-loop arm.
    pub(crate) fn apply_translate_deliver(&mut self, deliver: TranslateDeliver) {
        match deliver {
            TranslateDeliver::Incoming {
                buffer_id,
                outcome,
                submitted_at,
            } => {
                self.resolve_incoming_translation(&buffer_id, outcome, submitted_at);
            }
            TranslateDeliver::Outgoing(mut out) => {
                if self.redirect_outgoing_deliver(&mut out) == RedirectVerdict::Refuse {
                    // The conversation this was addressed to has moved and
                    // its new window is gone. Sending under the old NAME is
                    // the one thing that must not happen: somebody else may
                    // hold that nick now, and this is a private message.
                    self.release_echo_slot_and_drain(&out.buffer_id, out.echo_id);
                    self.abandon_with_text(
                        &out,
                        "the conversation it was addressed to is no longer open",
                    );
                    return;
                }
                self.send_outgoing_translated(&out);
            }
        }
    }

    /// Point a finished outgoing translation at the query buffer as it is
    /// NOW, if its peer renamed while the translation was running.
    ///
    /// The target name matters more than the id: `buffer_name` is what
    /// `send_privmsg` addresses, so leaving it stale sends the message to a
    /// nick its owner no longer answers to — and if somebody else has claimed
    /// it in the meantime, to a stranger.
    fn redirect_outgoing_deliver(&self, out: &mut OutgoingTranslateDeliver) -> RedirectVerdict {
        let Some(new_id) = self
            .state
            .redirected_buffer_id(&out.buffer_id, out.submitted_at)
        else {
            return RedirectVerdict::Proceed;
        };
        let new_id = new_id.to_string();
        let Some(new_name) = self.state.buffers.get(&new_id).map(|b| b.name.clone()) else {
            // Renamed, then closed. The old NAME must not be used either way
            // — somebody may hold that nick now — so this cannot fall through
            // to the ordinary send. Refuse and hand the text back.
            return RedirectVerdict::Refuse;
        };
        tracing::debug!(
            from = %out.buffer_id,
            to = %new_id,
            "translate: following a renamed query for a finished send"
        );
        // The echo plan names its own buffer, which is this same query for
        // every caller that supplies one.
        if let OutgoingEchoPlan::Gated { buffer_id, .. } = &mut out.echo
            && *buffer_id == out.buffer_id
        {
            buffer_id.clone_from(&new_id);
        }
        out.buffer_id = new_id;
        out.buffer_name = new_name;
        RedirectVerdict::Proceed
    }

    /// Fold an incoming outcome into its buffer's queue and release whatever
    /// that unblocks.
    fn resolve_incoming_translation(
        &mut self,
        buffer_id: &str,
        outcome: TranslateOutcome,
        submitted_at: std::time::Instant,
    ) {
        // This line was dispatched under the id the buffer had at the time.
        // If the peer has since renamed, the queue holding its place moved
        // with the buffer, and without following it the outcome lands
        // nowhere and the line it belongs to sits until the timeout.
        let buffer_id = &self
            .state
            .redirected_buffer_id(buffer_id, submitted_at)
            .map_or_else(|| buffer_id.to_string(), ToString::to_string);
        let ready = {
            let Some(queue) = self.state.translate_queues.get_mut(buffer_id) else {
                // The queue is gone — the buffer was closed, or a flush
                // already released this line. Expected, not an error.
                tracing::debug!(
                    buffer_id,
                    id = outcome.id(),
                    "translate: outcome for a buffer with no queue, dropped"
                );
                return;
            };
            let resolution = match outcome {
                TranslateOutcome::Translated { id, text } => (id, Ok(text)),
                TranslateOutcome::Untranslated { id, reason } => (id, Err(reason)),
            };
            if !queue.resolve(resolution.0, resolution.1) {
                tracing::debug!(
                    buffer_id,
                    id = resolution.0,
                    "translate: late outcome for an already-released line, dropped"
                );
            }
            queue.drain_ready()
        };
        self.release_translated(buffer_id, ready);
        self.prune_empty_translate_queues();
    }

    /// Expire timed-out entries and enforce the ceiling on every queue, then
    /// release whatever that unblocked.
    pub(crate) fn tick_translate_queues(&mut self) {
        if self.state.translate_queues.is_empty() {
            return;
        }
        let timeout = std::time::Duration::from_millis(self.config.translate.timeout_ms);
        let max_queue = self.config.translate.max_queue.max(1) as usize;
        let now = std::time::Instant::now();
        let buffer_ids: Vec<String> = self.state.translate_queues.keys().cloned().collect();
        for buffer_id in buffer_ids {
            let ready = {
                let Some(queue) = self.state.translate_queues.get_mut(&buffer_id) else {
                    continue;
                };
                let expired = queue.expire(now, timeout);
                let forced = queue.enforce_ceiling(max_queue);
                if expired > 0 || forced > 0 {
                    tracing::debug!(
                        buffer_id = %buffer_id,
                        expired,
                        forced,
                        "translate: released lines untranslated"
                    );
                }
                queue.drain_ready()
            };
            self.release_translated(&buffer_id, ready);
        }
        self.prune_empty_translate_queues();
    }

    /// Deliver cleared lines to the buffer.
    ///
    /// Calls the `_unshrunk` variant deliberately: the text is final, and
    /// re-entering `add_message` would hand a translated line straight back
    /// to the translation gate. Translation and shrink are mutually
    /// exclusive per line by design — two external round-trips on one line
    /// is worse than losing shrink on translated buffers.
    pub(crate) fn release_translated(
        &mut self,
        buffer_id: &str,
        ready: Vec<crate::translate::queue::ReadyEntry>,
    ) {
        for entry in ready {
            if let Some(reason) = entry.reason.as_ref().filter(|r| r.is_gap()) {
                tracing::debug!(
                    buffer_id,
                    id = entry.id,
                    reason = %reason.label(),
                    "translate: line delivered untranslated"
                );
            }
            self.state.add_message_with_activity_unshrunk(
                buffer_id,
                entry.message,
                entry.activity,
            );
        }
    }

    /// Final stage of the outgoing pipeline once translation has returned.
    ///
    /// Uses the captured `nick` / `own_mode` / `peer_handle` rather than
    /// re-reading state: the user may have `/nick`ed, closed the buffer, or
    /// gained a channel mode during the wait, and reading current state
    /// would produce a local echo inconsistent with what hit the wire — or
    /// leak plaintext for an E2E PM whose Query buffer is gone. Same
    /// reasoning as `send_outgoing_substituted` in `shrink.rs`.
    fn send_outgoing_translated(&mut self, out: &OutgoingTranslateDeliver) {
        // Re-wrap here rather than translating the framing: the request
        // carried the ACTION's inner text, because `\x01ACTION …\x01` handed
        // to a translator comes back as anything but a valid CTCP.
        //
        // Wrapping produces ONE string, and a translation can be longer than
        // its source — enough to push the framed form past the byte budget.
        // The send path would then split the whole framed string, so peers
        // would receive a first chunk with an opening delimiter, middle
        // chunks with none, and a last chunk with only the closing one:
        // malformed actions rather than one long one. Splitting the BODY and
        // wrapping each chunk keeps every wire line a valid CTCP.
        let wrap = |body: &str| wrap_outgoing_body(body, out.is_action);
        let wire_texts = match &out.outcome {
            TranslateOutcome::Translated { text, .. } => wrap(text),
            // The broker decided this line needed no translation, so the
            // original IS the correct thing to send.
            TranslateOutcome::Untranslated { reason, .. } if !reason.is_gap() => {
                wrap(&out.original_text)
            }
            // Everything else is a genuine gap. Do NOT fall back to sending
            // the original: the user asked for this channel to be written in
            // another language, and shipping their untranslated text is
            // sending something other than what they intended. Fail closed,
            // hand the text back, and let them decide.
            TranslateOutcome::Untranslated { reason, .. } => {
                let reason_label = reason.label();
                // No echo will be written, so the place held for it must be
                // given back — leaving it would block the buffer until the
                // queue timeout.
                self.release_echo_slot_and_drain(&out.buffer_id, out.echo_id);
                self.abandon_with_text(out, &format!("translation failed ({reason_label})"));
                return;
            }
        };

        // Belt and braces on the injection guard. `single_line_or_refuse`
        // already rejects a multi-line answer at the seam, but THIS is the
        // last point before bytes reach the socket, and `send_privmsg` only
        // breaks on `\r\n` — a bare `\n` rides into the trailing parameter
        // and a server accepting bare-LF endings reads the remainder as a
        // fresh command. Making the refusal a property of the send rather
        // than of one upstream check is the same reasoning as re-running the
        // E2E gate inside `build_outgoing_translate`.
        if wire_texts
            .iter()
            .any(|w| w.contains(['\r', '\n']))
        {
            tracing::error!(
                target = %out.buffer_name,
                "translate: refusing a wire payload containing a line break"
            );
            self.release_echo_slot_and_drain(&out.buffer_id, out.echo_id);
            self.abandon_with_text(out, "the translation contained a line break");
            return;
        }

        // Late is the same as not at all. The display reservation this send
        // holds expires `timeout_ms` after submission whether or not the
        // translation is back, so once that has passed there is no place left
        // for the echo and the user has already watched their line vanish.
        // Putting it on the channel now would publish a message they may well
        // have retyped.
        //
        // The lane bounds the provider call to the REMAINING budget, so this
        // normally cannot fire; it does when the main loop itself was too
        // busy to drain the deliver channel in time. Refusing then can throw
        // away a translation that finished just inside the deadline — the
        // right trade for a feature whose whole posture is fail-closed, and
        // the user gets their text back either way.
        let budget = std::time::Duration::from_millis(self.config.translate.timeout_ms.max(1));
        if out.submitted_at.elapsed() >= budget {
            self.release_echo_slot_and_drain(&out.buffer_id, out.echo_id);
            self.abandon_with_text(out, "the translation took longer than the timeout");
            return;
        }

        // Two separate questions, both asked BEFORE `plan_translated_wires`.
        // Planning runs `e2e_encrypt_or_passthrough`, which creates or
        // rotates the outgoing session and queues REKEY NOTICEs — planning
        // and then failing to send advances our key past a pending rotate
        // while the peers never receive the new one. This is the same
        // ordering, for the same reason, as `send_gated_message`'s precheck.

        // 1. Is there a connection at all? The handle existed at submission
        //    and is gone now; nothing reached the wire and no echo holds the
        //    text, so discarding it loses a message the user already watched
        //    disappear from their composer.
        if !self.irc_handles.contains_key(&out.conn_id) {
            self.release_echo_slot_and_drain(&out.buffer_id, out.echo_id);
            self.abandon_with_text(out, "the connection is unavailable");
            return;
        }
        // 2. Is it the SAME SESSION this message was written for? A drop and
        //    reconnect under the same `conn_id` installs a replacement
        //    handle, and question 1 cannot tell them apart. Sending a
        //    pre-disconnect line on the new session puts text on a channel
        //    the user may have long since left, minutes after they typed it
        //    and quite possibly after they retyped it by hand.
        if self.connection_generation(&out.conn_id) != out.conn_generation {
            self.release_echo_slot_and_drain(&out.buffer_id, out.echo_id);
            self.abandon_with_text(
                out,
                "the connection was re-established while this was being translated",
            );
            return;
        }

        let Some((wire_lines, plain_echo)) = self.plan_translated_wires(out, &wire_texts) else {
            return;
        };

        let echo_message_enabled = self
            .state
            .connections
            .get(&out.conn_id)
            .is_some_and(|c| c.enabled_caps.contains("echo-message"));
        let is_e2e_encrypted = wire_lines
            .first()
            .is_some_and(|w| w.starts_with("+RPE2E01"));

        // Recorded rather than reported inline: the message the user gets
        // depends on how far the send got, which is only known after the
        // loop, and it has to carry their text with it.
        let mut failure: Option<&'static str> = None;
        let mut sent_any = false;
        for wire in &wire_lines {
            let Some(handle) = self.irc_handles.get(&out.conn_id) else {
                failure = Some("the connection dropped");
                break;
            };
            if handle.sender().send_privmsg(&out.buffer_name, wire).is_err() {
                tracing::warn!(
                    conn_id = %out.conn_id,
                    target = %out.buffer_name,
                    "translate: deferred outgoing send failed"
                );
                failure = Some("the send failed");
                break;
            }
            sent_any = true;
        }
        if sent_any {
            self.note_message_sent(&out.buffer_id);
        }
        // Only drain on success — matching handle_plain_message, which
        // leaves REKEY NOTICEs queued for the next successful send rather
        // than flushing them to peers whose session assumes the triggering
        // ciphertext arrived.
        if failure.is_none() && !self.state.pending_e2e_sends.is_empty() {
            self.drain_pending_e2e_sends();
        }
        if let Some(reason) = failure {
            self.abandon_translated_send(out, sent_any, reason);
            return;
        }
        // Honour the submitting call site's intent. `BufferInput` keeps the
        // rule typed text has always had; `Gated` mirrors
        // `send_gated_message`'s own condition exactly; `None` means the
        // caller (a script) never wanted a plaintext echo and inventing one
        // would double-render for scripts that print their own output.
        let should_echo = match &out.echo {
            OutgoingEchoPlan::None => false,
            OutgoingEchoPlan::BufferInput => !echo_message_enabled || is_e2e_encrypted,
            OutgoingEchoPlan::Gated {
                even_without_encryption,
                ..
            } => is_e2e_encrypted || (*even_without_encryption && !echo_message_enabled),
        };
        if should_echo {
            self.write_translated_local_echo(out, &plain_echo);
        } else if echo_message_enabled && !is_e2e_encrypted {
            // The server owns this row: it will come back as a reflection.
            // Record what to do with it BEFORE anything else, so a server
            // that answers faster than we finish here still finds it.
            //
            // The reservation is deliberately NOT released. The reflection
            // takes the id held for it and fills that place, so the user's
            // own message stays where they typed it instead of landing after
            // the replies that arrived while it was translating. If no
            // reflection ever comes, the queue's expiry clears the barrier
            // like any other stall.
            //
            // Decorating the reflection rather than writing our own row is
            // what keeps the server's `@time` and `@msgid` on the message a
            // CHATHISTORY replay has to dedup against. E2E is excluded
            // because its reflection is ciphertext and already swallowed.
            self.expect_own_reflection(out, &wire_lines);
        } else {
            // Nothing will come back — no echo-message, or a script that
            // wanted no echo — so the place held for it must be given up.
            self.state.release_echo_slot(&out.buffer_id, out.echo_id);
        }
        // Both branches just lifted the barrier this send was holding —
        // by filling it or by giving it up. Whichever it was, the echo and
        // any lines that finished translating behind it are deliverable NOW,
        // and this arm is the one that never comes back to the queue.
        self.drain_translate_queue(&out.buffer_id);
    }

    /// Tell the incoming path how to render `echo-message`'s reflection of
    /// this send, so it shows the original the wire could not carry.
    ///
    /// Only the LAST wire line is decorated. A translation long enough to
    /// split is several reflections and one original; repeating it on each
    /// would say the same thing N times, and putting it on the first would
    /// place it before text it is the original of.
    fn expect_own_reflection(&mut self, out: &OutgoingTranslateDeliver, wire_lines: &[String]) {
        for (i, wire) in wire_lines.iter().enumerate() {
            // Only the LAST line carries the original. A translation long
            // enough to split is several reflections and one original;
            // repeating it on each would say the same thing N times, and
            // putting it on the first would place it before the text it is
            // the original of.
            let display = if i + 1 == wire_lines.len() {
                Self::reflection_display(out, wire)
            } else {
                None
            };
            self.state.decorate_own_echo(
                &out.buffer_id,
                crate::state::AppState::own_echo_decoration(wire.clone(), out.echo_id, display),
            );
        }
    }

    /// What a reflected wire line should show instead of itself, when the
    /// buffer is configured to display the original.
    ///
    /// `None` when the reflection already reads correctly —
    /// `show_original_out` is off, or the translation came back identical to
    /// what was typed.
    fn reflection_display(
        out: &OutgoingTranslateDeliver,
        wire: &str,
    ) -> Option<(String, crate::state::buffer::WireOrigin)> {
        // The BODY, not the frame: an action reflects as `\x01ACTION …\x01`
        // and the row it becomes holds only the inner text, so both the
        // composed display and the recorded wire text are computed on that.
        let body = crate::app::e2e_gate::translatable_outgoing_body(wire).unwrap_or(wire);
        let (display_body, suffix_at) =
            crate::translate::compose_display(body, &out.original_text, out.show_original);
        let suffix_at = suffix_at?;
        let display = if out.is_action {
            format!("\x01ACTION {display_body}\x01")
        } else {
            display_body
        };
        Some((
            display,
            crate::state::buffer::WireOrigin {
                text: body.to_string(),
                suffix_at: Some(suffix_at),
            },
        ))
    }

    /// Report a deferred send that will not happen, and give the user their
    /// text back.
    ///
    /// The text goes in the ERROR ROW **as well as** the composer, and that is
    /// the whole point of this function existing. `restore_input_text_to`
    /// deliberately refuses to overwrite a composer the user has since typed
    /// into — clobbering the next message would be a worse surprise — so on a
    /// deferred failure, which by definition happens seconds after the user
    /// hit Enter, the composer is often busy and the restore is a no-op. A row
    /// that names only the reason then loses the message outright while
    /// telling the user it was handed back.
    ///
    /// `refuse_untranslatable_send_to` has printed both since it was written,
    /// for exactly this reason. The deferred paths did not, and they are the
    /// ones where it matters most.
    fn abandon_with_text(&mut self, out: &OutgoingTranslateDeliver, reason: &str) {
        let restore = self.deferred_retry_text(out);
        // The row shows whatever would be restored; when nothing can be, it
        // shows the bare body and says where it was headed, so the user can
        // still see and re-send it deliberately.
        let shown = restore.clone().unwrap_or_else(|| {
            format!("{body} {dim}(to {target}){rst}",
                body = out.retry_body,
                target = out.buffer_name,
                dim = crate::commands::types::C_DIM,
                rst = crate::commands::types::C_RST,
            )
        });
        self.deliver_translate_error(
            &out.buffer_id,
            &format!(
                "{err}Not sent — {reason}.{rst}\n{dim}Your text:{rst} {shown}",
                err = crate::commands::types::C_ERR,
                dim = crate::commands::types::C_DIM,
                rst = crate::commands::types::C_RST,
            ),
        );
        if let Some(text) = restore {
            let origin = out.origin.clone();
            self.restore_input_text_to(&text, &origin);
        }
    }

    /// The retry text to put back in the composer for a DEFERRED failure,
    /// given where the submitting client is looking NOW.
    ///
    /// `retry_form_for` answers this question at DISPATCH time, which is the
    /// wrong time. The whole point of a deferred send is that seconds pass,
    /// and the user may well have moved to another conversation meanwhile —
    /// switching away is the natural thing to do while waiting. Restoring a
    /// bare body into a composer that now belongs to a different buffer
    /// publishes it there the moment they press Enter: exactly the leak
    /// `retry_form_for` was written to prevent, arriving through the one
    /// door it cannot see.
    ///
    /// `None` means no form would do the right thing from where they are —
    /// an action, whose `/me` acts on the active buffer and has no
    /// re-addressed spelling. The text is in the error row either way, so
    /// declining to restore loses nothing.
    fn deferred_retry_text(&self, out: &OutgoingTranslateDeliver) -> Option<String> {
        let looking_at = match &out.origin {
            // Nobody typed it, so there is nowhere to put it back.
            SubmitOrigin::Script => return None,
            SubmitOrigin::Tui => self.state.active_buffer_id.as_deref(),
            SubmitOrigin::Web(session) => {
                self.web_active_buffers.get(session).map(String::as_str)
            }
        };
        if looking_at == Some(out.buffer_id.as_str()) {
            // Still in the conversation they addressed, so the form computed
            // at dispatch is the one that belongs in this composer.
            return Some(out.retry_text.clone());
        }
        if out.is_action {
            return None;
        }
        if !self.state.buffers.contains_key(&out.buffer_id) {
            // The conversation is gone, so its NAME is no longer proof of
            // anything — on a query it may since have been claimed by
            // somebody else. Re-addressing to it would hand the user a
            // ready-to-send private message aimed at a stranger, which is
            // the leak this whole function exists to prevent.
            return None;
        }
        // Explicitly re-addressed, so it goes where it was always going no
        // matter which buffer they are in when they press Enter.
        Some(format!("/msg {} {}", out.buffer_name, out.retry_body))
    }

    /// Clean up after a send that failed AFTER the session check passed — the
    /// writer task went away between the two, which it can, because it dies
    /// independently of the map entry.
    ///
    /// `sent_any` is what makes the two cases different, and it is the whole
    /// point of this function.
    fn abandon_translated_send(
        &mut self,
        out: &OutgoingTranslateDeliver,
        sent_any: bool,
        reason: &str,
    ) {
        // No echo will be written either way, so the reservation has to go
        // back or this buffer stays barricaded behind it until the queue
        // timeout.
        self.release_echo_slot_and_drain(&out.buffer_id, out.echo_id);
        if !sent_any {
            self.abandon_with_text(out, reason);
            return;
        }
        // A split message that died halfway already put its first chunks
        // on the channel. Restoring the whole line into the composer
        // would invite the user to send those chunks a second time, so it
        // goes to the error row and nowhere else.
        let retry = out.retry_text.clone();
        self.deliver_translate_error(
            &out.buffer_id,
            &format!(
                "{err}Part of the message was sent — the rest was not ({reason}).{rst} \
                 {dim}Not restored to the input line, to avoid sending the \
                 first part twice.{rst}\n{dim}Your text:{rst} {retry}",
                err = crate::commands::types::C_ERR,
                dim = crate::commands::types::C_DIM,
                rst = crate::commands::types::C_RST,
            ),
        );
    }

    /// Assemble the outgoing translation request, capturing every
    /// state-dependent value now.
    ///
    /// The user may `/nick`, close the buffer, or gain a channel mode during
    /// the wait; reading current state at deliver time would produce a local
    /// echo inconsistent with what hit the wire, and could strand the E2E
    /// peer handle for a closed Query. Returns `None` when the buffer is
    /// gone, in which case the caller falls through to the synchronous send.
    pub(crate) fn build_outgoing_translate(
        &mut self,
        req: &OutgoingRequest<'_>,
    ) -> Option<PendingOutgoingTranslate> {
        let OutgoingRequest {
            conn_id,
            buffer_id,
            buffer_name,
            buffer_type,
            nick,
            text,
            is_action,
            echo,
        } = req.clone();
        // Belt and braces. `handle_plain_message` already refuses to reach
        // this function when E2E cannot be ruled out, but this is the point
        // where cleartext becomes a payload bound for a third party, and
        // this function is reachable from anywhere in the crate. A caller
        // that forgets the gate must not be able to leak; re-checking here
        // makes the refusal a property of the function rather than of its
        // call sites. Fail-closed predicate, never the advisory one.
        if self.state.e2e_possible_for_target(conn_id, buffer_name) {
            tracing::warn!(
                target = %buffer_name,
                "translate: refused an outgoing request for a possibly-E2E target"
            );
            return None;
        }
        // An OPEN buffer is not required. A script's `say()` addresses a
        // target by name and has always been able to reach one the user has
        // no window on; refusing here because there is no buffer would drop
        // messages that used to go out, and translation is a setting on the
        // conversation, not on whether it happens to be on screen. All the
        // buffer contributes is the nick list — masking behind the seam
        // simply gets an empty one.
        let known_nicks: Vec<String> = self
            .state
            .buffers
            .get(buffer_id)
            .map(|b| b.users.keys().cloned().collect())
            .unwrap_or_default();
        let network = self
            .state
            .connections
            .get(conn_id)
            .map(|c| c.label.clone())
            .unwrap_or_default();
        let captured_nick = self
            .state
            .connections
            .get(conn_id)
            .map_or_else(|| nick.to_string(), |c| c.nick.clone());
        let captured_own_mode = self.state.nick_prefix(buffer_id, &captured_nick);
        // Resolve the FULL peer handle now, while the buffer still exists —
        // a `/close` during the wait would otherwise leave
        // `e2e_encrypt_or_passthrough` unable to recover the network and
        // fall through to plaintext for an E2E-enabled DM.
        let captured_peer_handle = if *buffer_type == BufferType::Query {
            self.state
                .resolve_query_peer_handle(buffer_id, buffer_name)
                .unwrap_or_else(|e| {
                    tracing::warn!("e2e: failed to resolve DM peer handle for {buffer_name}: {e}");
                    None
                })
        } else {
            None
        };
        // Same resolver as the incoming path, asked for the other
        // direction — the pair is never read per-direction off the config.
        // `outgoing()` returns None when the buffer has no language, because
        // a TARGET cannot be autodetected: there is nothing to detect which
        // language to write in from. Refusing lets the caller fall through
        // to sending untranslated rather than guessing.
        let Some((source_lang, target_lang)) = crate::translate::resolve_langs(
            self.config.translate.buffers.get(buffer_id),
            &self.config.translate.my_lang,
        )
        .outgoing() else {
            tracing::warn!(
                buffer_id,
                "translate: outgoing needs the buffer's language; \
                 set it with /translate addout <target> <lang>"
            );
            return None;
        };

        let id = self.state.next_message_id();
        Some(PendingOutgoingTranslate {
            conn_id: conn_id.to_string(),
            buffer_id: buffer_id.to_string(),
            buffer_name: buffer_name.to_string(),
            buffer_type: buffer_type.clone(),
            original_text: text.to_string(),
            req: TranslateRequest {
                id,
                direction: crate::translate::Direction::Outgoing,
                network,
                target: buffer_name.to_string(),
                nick: captured_nick.clone(),
                text: text.to_string(),
                source_lang,
                target_lang,
                known_nicks,
            },
            nick: captured_nick,
            own_mode: captured_own_mode,
            peer_handle: captured_peer_handle,
            show_original: self.config.translate.show_original_out,
            is_action,
            echo,
            echo_id: id,
            origin: self.submit_origin.clone(),
            // Buffer input retries as itself; by-target sends overwrite this
            // with a re-addressed form in `dispatch_by_target_translation`.
            retry_text: text.to_string(),
            // Never overwritten — this is the raw body a re-addressed retry
            // is rebuilt from if the user has moved on by the time it fails.
            retry_body: text.to_string(),
            submitted_at: std::time::Instant::now(),
            // Captured with everything else that could change during the
            // wait. A reconnect under this same id is a different session,
            // and this message was written for the one in front of the user.
            conn_generation: self.connection_generation(conn_id),
        })
    }

    /// Which session of `conn_id` is live right now, or `None` when there is
    /// no handle at all.
    ///
    /// Both halves matter: `None` at capture and `Some` at delivery means a
    /// connection came up during the wait, which is just as much a different
    /// session as a reconnect is.
    fn connection_generation(&self, conn_id: &str) -> Option<u64> {
        if !self.irc_handles.contains_key(conn_id) {
            return None;
        }
        self.conn_generations.get(conn_id).copied()
    }

    /// What the outgoing gate has decided about one submitted line.
    ///
    /// Extracted so the whole outgoing policy is one readable, testable
    /// statement rather than a chain of conditions inside the submit path.
    /// Every way of NOT completing a required translation has to refuse, and
    /// that is much easier to check when the options are enumerated.
    pub(crate) fn outgoing_translate_policy(
        &self,
        buffer_id: &str,
        text: &str,
        e2e_possible: bool,
    ) -> OutgoingTranslatePolicy {
        // E2E wins: the conversation is simply not translated, and the
        // ordinary (encrypted) send is exactly right.
        if e2e_possible || !self.state.translate_active {
            return OutgoingTranslatePolicy::NotApplicable;
        }
        let Some(cfg) = self.config.translate.buffers.get(buffer_id) else {
            return OutgoingTranslatePolicy::NotApplicable;
        };
        if !cfg.outgoing {
            return OutgoingTranslatePolicy::NotApplicable;
        }
        // From here translation is REQUIRED, so everything below refuses
        // rather than falling through to a plaintext send. Putting text on a
        // channel in a language the user did not choose is the failure the
        // strict outgoing rule exists to prevent, and one they cannot see
        // happen.
        // These two are exactly `multiline::needs_multiline`, split apart so
        // the refusal names the real reason: a long single-line paste is not
        // a "multi-line message", and telling the user it is would send them
        // looking for a newline that is not there.
        if text.contains('\n') {
            return OutgoingTranslatePolicy::Refuse("multi-line messages cannot be translated");
        }
        if text.len() > crate::irc::MESSAGE_MAX_BYTES {
            return OutgoingTranslatePolicy::Refuse(
                "message is too long to translate in one piece",
            );
        }
        if cfg.lang.is_none() {
            return OutgoingTranslatePolicy::Refuse(
                "no target language set for this buffer — \
                 /translate addout <target> <lang>",
            );
        }
        OutgoingTranslatePolicy::Translate
    }

    /// Apply the outgoing translation policy to a send addressed by TARGET
    /// NAME — `/msg`, `/query <nick> <text>`, `/me`, and the script senders.
    ///
    /// Returns `Some(result)` when translation took the send over (dispatched
    /// or refused) and `None` when the caller should carry on with the
    /// ordinary send.
    ///
    /// Gating here rather than at each call site is what stops the per-buffer
    /// `addout` setting depending on HOW the message was submitted — typing
    /// it in the buffer would translate, `/msg`ing the same text would not,
    /// and nothing would say so.
    pub(crate) fn gate_by_target_translation(
        &mut self,
        conn_id: &str,
        target: &str,
        wire_text: &str,
        echo: OutgoingEchoPlan,
    ) -> Option<bool> {
        let body = crate::app::e2e_gate::translatable_outgoing_body(wire_text)?;
        let buffer_id = crate::state::buffer::make_buffer_id(conn_id, target);
        let e2e_possible = self.state.e2e_possible_for_target(conn_id, target);
        let is_action = body.len() != wire_text.len();
        let retry = self.retry_form_for(target, body, is_action);
        match self.outgoing_translate_policy(&buffer_id, body, e2e_possible) {
            OutgoingTranslatePolicy::NotApplicable => None,
            OutgoingTranslatePolicy::Refuse(reason) => {
                Some(self.refuse_untranslatable_send(&retry, reason))
            }
            OutgoingTranslatePolicy::Translate => {
                let buffer_type = if crate::e2e::is_channel_target(target) {
                    BufferType::Channel
                } else {
                    BufferType::Query
                };
                let nick = self
                    .state
                    .connections
                    .get(conn_id)
                    .map(|c| c.nick.clone())
                    .unwrap_or_default();
                Some(self.dispatch_by_target_translation(
                    &OutgoingRequest {
                        conn_id,
                        buffer_id: &buffer_id,
                        buffer_name: target,
                        buffer_type: &buffer_type,
                        nick: &nick,
                        text: body,
                        is_action,
                        echo,
                    },
                    &retry,
                ))
            }
        }
    }

    /// The text to hand back if this send is refused: a form that does the
    /// SAME thing when the user presses Enter on it.
    ///
    /// Restoring the bare body is unsafe. `/msg bob secret` deliberately
    /// leaves the current channel active, so returning just `secret` and
    /// letting the user hit Enter publishes private content to the channel.
    /// `/me waves` would likewise come back as plain text and lose its
    /// action semantics.
    fn retry_form_for(&self, target: &str, body: &str, is_action: bool) -> String {
        if is_action {
            // `/me` acts on the active buffer, which is where it came from.
            return format!("/me {body}");
        }
        let active_is_target = self
            .state
            .active_buffer_id
            .as_ref()
            .and_then(|id| self.state.buffers.get(id))
            .is_some_and(|b| b.name.eq_ignore_ascii_case(target));
        if active_is_target {
            body.to_string()
        } else {
            // Re-addressed explicitly. `/msg` reaches the same destination
            // as `/query <nick> <text>` did, so the retry is equivalent even
            // when the original spelling is not recoverable here.
            format!("/msg {target} {body}")
        }
    }

    /// Dispatch a by-target send (`/msg`, `/query`, `/me`, a script) for
    /// translation. Returns `false` — nothing is on the wire yet.
    ///
    /// Failure here refuses, exactly as the buffer-input path does. A
    /// by-target send is not a lesser send: publishing it untranslated puts
    /// the same wrong-language text on the same channel.
    pub(crate) fn dispatch_by_target_translation(
        &mut self,
        req: &OutgoingRequest<'_>,
        retry: &str,
    ) -> bool {
        let Some(pending) = self.build_outgoing_translate(req) else {
            return self.refuse_untranslatable_send(
                retry,
                "this conversation can no longer be translated",
            );
        };
        let mut pending = pending;
        pending.retry_text = retry.to_string();
        // Hold the display position now, while the order is still known.
        self.state.reserve_echo_slot(req.buffer_id, pending.echo_id);
        match self.translate_outgoing_tx.try_send(pending) {
            Ok(()) => false,
            Err(tokio::sync::mpsc::error::TrySendError::Full(p)) => {
                self.state.release_echo_slot(req.buffer_id, p.echo_id);
                self.refuse_untranslatable_send(retry, "the translation queue is full")
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(p)) => {
                self.state.release_echo_slot(req.buffer_id, p.echo_id);
                self.refuse_untranslatable_send(
                    retry,
                    "the translation worker has died — restart to restore it",
                )
            }
        }
    }

    /// Refuse an outgoing send that cannot be translated, before anything
    /// reaches the wire.
    ///
    /// Always returns `false` (nothing was sent) so submit paths can
    /// `return` it directly.
    pub(crate) fn refuse_untranslatable_send(&mut self, text: &str, reason: &str) -> bool {
        let origin = self.submit_origin.clone();
        self.refuse_untranslatable_send_to(text, reason, &origin)
    }

    /// [`Self::refuse_untranslatable_send`] for a send whose origin was
    /// captured earlier — the deferred path, where the current origin is
    /// whatever happens to be submitting now, not who typed this.
    pub(crate) fn refuse_untranslatable_send_to(
        &mut self,
        text: &str,
        reason: &str,
        origin: &SubmitOrigin,
    ) -> bool {
        // The text is in the error row as well as restored. Restoring alone
        // is not enough: a composer that is no longer empty keeps what the
        // user is typing now (clobbering it would be a worse surprise), and
        // then the refused message would exist nowhere at all.
        crate::commands::helpers::add_local_event(
            self,
            &format!(
                "{err}Not sent — {reason}.{rst} {dim}/translate delout this buffer \
                 to send it as-is.{rst}\n{dim}Your text:{rst} {text}",
                err = crate::commands::types::C_ERR,
                dim = crate::commands::types::C_DIM,
                rst = crate::commands::types::C_RST,
            ),
        );
        self.restore_input_text_to(text, origin);
        false
    }

    /// Put a refused message back in the input line.
    ///
    /// Only into an EMPTY input: the user may have typed something else
    /// while a deferred send was in flight, and clobbering that would be a
    /// second, worse surprise. The text is still visible in the error row
    /// above either way, so it is never truly lost.
    pub(crate) fn restore_input_text_to(&mut self, text: &str, origin: &SubmitOrigin) {
        match origin {
            // Nobody typed it, so there is nowhere to put it back. The error
            // row already names the reason; injecting a script's payload into
            // the user's composer would hand them text to send by accident.
            SubmitOrigin::Script => {}
            SubmitOrigin::Web(session_id) => {
                // Back to the browser that sent it. Putting it in the TUI
                // input instead loses it for its author and makes it appear
                // where nobody is looking.
                self.broadcast_web(crate::web::protocol::WebEvent::RestoreInput {
                    text: text.to_string(),
                    session_id: Some(session_id.clone()),
                });
            }
            SubmitOrigin::Tui => {
                if self.input.value.is_empty() {
                    self.input.value = text.to_string();
                    self.input.cursor_pos = self.input.value.chars().count();
                }
            }
        }
    }


    /// Plan every wire line for a translated outgoing message, and the
    /// plaintext to echo.
    ///
    /// Each payload is planned separately so an action already split to fit
    /// is not re-split across its CTCP framing. Returns `None` when the send
    /// was refused — the refusal, and restoring the user's text, are handled
    /// here.
    fn plan_translated_wires(
        &mut self,
        out: &OutgoingTranslateDeliver,
        wire_texts: &[String],
    ) -> Option<(Vec<String>, String)> {
        let mut wire_lines = Vec::new();
        let mut plain_echo = String::new();
        for wire_text in wire_texts {
            match self.state.e2e_encrypt_or_passthrough(
                &out.buffer_id,
                &out.buffer_name,
                &out.buffer_type,
                wire_text,
                out.peer_handle.as_deref(),
            ) {
                Ok((lines, echo)) => {
                    wire_lines.extend(lines);
                    // Every chunk, not just the first: a long translated
                    // action is split across several wire payloads, and
                    // echoing only the first would show and LOG a fraction
                    // of what the peers received.
                    //
                    // Joined by BODY, not by frame. Concatenating the whole
                    // `\x01ACTION …\x01` payloads would leave delimiters and
                    // ACTION tokens embedded in the middle of the displayed
                    // line, since only the outermost frame is stripped later.
                    let piece = crate::app::e2e_gate::translatable_outgoing_body(&echo)
                        .unwrap_or(&echo);
                    if !plain_echo.is_empty() {
                        plain_echo.push(' ');
                    }
                    plain_echo.push_str(piece);
                }
                Err(reason) => {
                    // Same reasoning as the connection-unavailable branch:
                    // E2E state can change during the wait, nothing reached
                    // the wire, and the composer was cleared at submission.
                    self.release_echo_slot_and_drain(&out.buffer_id, out.echo_id);
                    self.abandon_with_text(out, &reason.user_message());
                    return None;
                }
            }
        }

        Some((wire_lines, plain_echo))
    }

    /// Emit the local echo for a successfully sent translated message.
    #[cfg(test)]
    pub(crate) fn write_translated_local_echo_for_test(
        &mut self,
        out: &OutgoingTranslateDeliver,
        plain_echo: &str,
    ) {
        self.write_translated_local_echo(out, plain_echo);
    }

    fn write_translated_local_echo(&mut self, out: &OutgoingTranslateDeliver, plain_echo: &str) {
        // A gated echo names its own buffer — a script may target a
        // conversation that is not the one the send was addressed through.
        let echo_buffer = match &out.echo {
            OutgoingEchoPlan::Gated { buffer_id, .. } => buffer_id.clone(),
            _ => out.buffer_id.clone(),
        };
        if !self.state.buffers.contains_key(&echo_buffer) {
            crate::commands::helpers::add_local_event(
                self,
                &format!(
                    "{dim}translate: message to {target} sent (buffer was \
                     closed during the translation wait){rst}",
                    target = out.buffer_name,
                    dim = crate::commands::types::C_DIM,
                    rst = crate::commands::types::C_RST,
                ),
            );
            return;
        }
        // An action echoes as its inner text under `MessageType::Action`;
        // showing the raw `\x01ACTION …\x01` would render the framing.
        let echo_body = if out.is_action {
            plain_echo
                .strip_prefix('\x01')
                .and_then(|t| t.strip_suffix('\x01'))
                .and_then(|t| t.strip_prefix("ACTION "))
                .unwrap_or(plain_echo)
        } else {
            plain_echo
        };
        let (echo_text, orig_offset) = if matches!(out.outcome, TranslateOutcome::Translated { .. })
        {
            crate::translate::compose_display(echo_body, &out.original_text, out.show_original)
        } else {
            (echo_body.to_string(), None)
        };
        let local_chunks = if echo_text.len() <= crate::irc::MESSAGE_MAX_BYTES {
            vec![echo_text]
        } else {
            crate::irc::split_irc_message(&echo_text, crate::irc::MESSAGE_MAX_BYTES)
        };
        let nick_mode_str = out.own_mode.map(|c| c.to_string());
        // Only a single-chunk echo can carry this: splitting moves the suffix
        // into the last chunk (so the offset no longer maps) and leaves each
        // chunk holding a fraction of a wire line that was itself split
        // differently (so no chunk's text is a wire text).
        let single_chunk = local_chunks.len() == 1;
        // What the NETWORK carried for this row — our translation, not the
        // original we typed. A CHATHISTORY replay of our own message brings
        // back exactly this, so it is what the row must be keyed by. See
        // `WireOrigin`.
        let wire_origin = single_chunk.then(|| crate::state::buffer::WireOrigin {
            text: echo_body.to_string(),
            suffix_at: orig_offset,
        });
        let message_type = match &out.echo {
            OutgoingEchoPlan::Gated { message_type, .. } => message_type.clone(),
            _ if out.is_action => crate::state::buffer::MessageType::Action,
            _ => crate::state::buffer::MessageType::Message,
        };
        // Every chunk gets its OWN id — two live rows sharing one are the
        // same message to the web client, which would drop all but the
        // first. What keeps them together is the ORDER KEY passed below: the
        // id reserved at submission, under which they all take the one place
        // held for them. See `add_own_message_chunks`.
        let chunks: Vec<crate::state::buffer::Message> = local_chunks
            .into_iter()
            .map(|chunk| crate::state::buffer::Message {
                id: self.state.next_message_id(),
                timestamp: chrono::Utc::now(),
                message_type: message_type.clone(),
                nick: Some(out.nick.clone()),
                nick_mode: nick_mode_str.clone(),
                text: chunk,
                highlight: false,
                event_key: None,
                event_params: None,
                log_msg_id: None,
                log_ref_id: None,
                tags: None,
                wire_origin: wire_origin.clone(),
            })
            .collect();
        // `add_own_message_chunks`, not `add_message`: this echo carries the
        // nick captured at dispatch, so a `/nick` during the wait would make
        // the dispatch gate mistake it for someone else's line and translate
        // our own message a second time.
        self.state
            .add_own_message_chunks(&echo_buffer, out.echo_id, chunks);
    }

    /// Route a translation-pipeline error to the right buffer, falling back
    /// to the active one when the destination is gone.
    fn deliver_translate_error(&mut self, buffer_id: &str, message: &str) {
        let target = if self.state.buffers.contains_key(buffer_id) {
            Some(buffer_id.to_string())
        } else {
            self.state.active_buffer_id.clone()
        };
        let Some(buf_id) = target else {
            tracing::warn!("translate: outgoing error with no target buffer: {message}");
            return;
        };
        let prior = self.state.active_buffer_id.clone();
        self.state.active_buffer_id = Some(buf_id);
        crate::commands::helpers::add_local_event(self, message);
        self.state.active_buffer_id = prior;
    }

    /// Re-derive every translate mirror on `AppState` from the config.
    ///
    /// Idempotent by design — it re-derives rather than undoing a specific
    /// switch — so `/set`, `/translate addin|delin`, and `/reload` can all
    /// call the same function and never drift apart. A per-key arm is
    /// exactly what let `/reload` fall out of step with `/set` for typing.
    ///
    /// `translate_active` stays false when no backend was built at startup:
    /// the worker queues are bound in `App::new`, so flipping the switch at
    /// runtime cannot materialise one.
    pub(crate) fn sync_translate_from_config(&mut self) {
        let has_backend = self.translate_backend.is_some();
        self.state.translate_active = self.config.translate.enabled && has_backend;
        self.state
            .translate_buffers
            .clone_from(&self.config.translate.buffers);
        self.state
            .translate_my_lang
            .clone_from(&self.config.translate.my_lang);
        self.state.translate_show_original_in = self.config.translate.show_original_in;
        self.state.translate_max_queue = self.config.translate.max_queue.max(1) as usize;
        if let Some(budget) = self.translate_timeout_ms.as_ref() {
            budget.store(
                self.config.translate.timeout_ms.max(1),
                std::sync::atomic::Ordering::Relaxed,
            );
        }
        self.retune_translate_concurrency();
    }

    /// Apply `translate.max_in_flight` to the running incoming worker.
    ///
    /// The limiter is built once at startup, so without this the setting is
    /// accepted, persisted, and silently ignored — worse than not offering
    /// it. `Semaphore` can only be nudged by a delta, so the last applied
    /// value is tracked alongside it.
    fn retune_translate_concurrency(&mut self) {
        let Some(limiter) = self.translate_in_flight.as_ref() else {
            return;
        };
        let want = self.config.translate.max_in_flight.max(1) as usize;
        let have = self.translate_in_flight_applied;
        if want == have {
            // A reduction that never took effect is obsolete the moment the
            // target comes back up to what is actually applied. Leaving the
            // debt makes the tick keep forgetting permits as in-flight work
            // returns them, so the limiter settles at the ABANDONED lower
            // value and stays there — permanently enforcing a number the
            // config no longer asks for, with nothing to say why.
            self.translate_in_flight_debt = 0;
            return;
        }
        if want > have {
            limiter.add_permits(want - have);
            self.translate_in_flight_applied = want;
            self.translate_in_flight_debt = 0;
        } else {
            // `forget_permits` can only take permits that are AVAILABLE, and
            // returns how many it actually took — possibly zero when every
            // permit is checked out. Recording the requested value regardless
            // would be wrong twice over: the old concurrency would come back
            // as in-flight work returns its permits, and the next retune
            // would compute its delta from a baseline that never existed.
            let forgotten = limiter.forget_permits(have - want);
            self.translate_in_flight_applied = have - forgotten;
            self.translate_in_flight_debt = (have - want) - forgotten;
        }
        tracing::info!(
            from = have,
            to = want,
            applied = self.translate_in_flight_applied,
            "translate: concurrency retuned"
        );
    }

    /// Collect a concurrency reduction that could not be applied at once.
    ///
    /// Permits held by in-flight requests cannot be forgotten until they come
    /// back, so the outstanding remainder is retried from the tick. Without
    /// this a lowered `max_in_flight` silently reverts as soon as the current
    /// batch finishes.
    pub(crate) fn settle_translate_concurrency_debt(&mut self) {
        if self.translate_in_flight_debt == 0 {
            return;
        }
        let Some(limiter) = self.translate_in_flight.as_ref() else {
            return;
        };
        let forgotten = limiter.forget_permits(self.translate_in_flight_debt);
        if forgotten == 0 {
            return;
        }
        self.translate_in_flight_debt -= forgotten;
        self.translate_in_flight_applied -= forgotten;
        tracing::debug!(
            forgotten,
            remaining = self.translate_in_flight_debt,
            "translate: settled part of a concurrency reduction"
        );
    }

    /// Release whatever one buffer's queue has ready, and drop it if that
    /// emptied it.
    ///
    /// The outgoing delivery arm needs this because it is the only path that
    /// lifts a barrier without going through
    /// [`Self::resolve_incoming_translation`]. Filling or releasing a
    /// reservation that sits at the HEAD makes it — and every resolved line
    /// queued behind it — deliverable at once, and nothing would revisit the
    /// queue until the one-second maintenance tick.
    fn drain_translate_queue(&mut self, buffer_id: &str) {
        let Some(queue) = self.state.translate_queues.get_mut(buffer_id) else {
            return;
        };
        let ready = queue.drain_ready();
        self.release_translated(buffer_id, ready);
        self.prune_empty_translate_queues();
    }

    /// Give a reservation back and release whatever that unblocks.
    ///
    /// Always this pair, never a bare `release_echo_slot`, on the delivery
    /// side: a reservation is a barrier, so dropping one at the head is just
    /// as much a release event as filling it.
    fn release_echo_slot_and_drain(&mut self, buffer_id: &str, id: u64) {
        self.state.release_echo_slot(buffer_id, id);
        self.drain_translate_queue(buffer_id);
    }

    /// Drop queues that have fully drained, so the tick has nothing to walk
    /// on an idle client.
    fn prune_empty_translate_queues(&mut self) {
        self.state
            .translate_queues
            .retain(|_, queue| !queue.is_empty());
    }

    /// Release every queued line for one buffer, untranslated where still
    /// pending, and drop its queue.
    #[allow(dead_code, reason = "called once the flush points are wired")]
    ///
    /// Called on buffer close, `/part`, disconnect, quit, detach, and
    /// `/translate delin`. Pending lines are released rather than dropped —
    /// the user already saw them arrive on the network, and losing them
    /// silently would be worse than showing them untranslated.
    pub(crate) fn flush_translate_queue(&mut self, buffer_id: &str) {
        let Some(mut queue) = self.state.translate_queues.remove(buffer_id) else {
            return;
        };
        let ready = queue.flush_all();
        if !ready.is_empty() {
            tracing::debug!(
                buffer_id,
                count = ready.len(),
                "translate: flushed queued lines"
            );
        }
        self.release_translated(buffer_id, ready);
    }

    /// Follow a re-keyed query buffer in the config that `AppState` cannot
    /// reach.
    ///
    /// Not persisted here. `/translate add*|del*` writes the file, and one of
    /// those will carry the migrated key along the next time the user runs
    /// it; writing `config.toml` in response to somebody else's `/nick` is
    /// disk I/O the user did not ask for, and would race an editor they may
    /// have open. The consequence is that the setting follows the peer for
    /// this session, and a restart keys it by the nick they actually typed —
    /// which is what they wrote down.
    pub(crate) fn drain_pending_buffer_rekeys(&mut self) {
        if self.state.pending_buffer_rekeys.is_empty() {
            return;
        }
        let rekeys = std::mem::take(&mut self.state.pending_buffer_rekeys);
        let mut moved = false;
        for (old_id, new_id) in rekeys {
            if let Some(cfg) = self.config.translate.buffers.remove(&old_id) {
                tracing::debug!(%old_id, %new_id, "translate: following a re-keyed query buffer");
                self.config.translate.buffers.insert(new_id, cfg);
                moved = true;
            }
        }
        if moved {
            // Re-derive the mirrors so `translate_buffers` and the config
            // agree again — `rekey_buffer_state` moved the mirror, and this
            // is what stops the two drifting from here on.
            self.sync_translate_from_config();
        }
    }

    /// Release this buffer's lines that are waiting on an INCOMING
    /// translation, and nothing else.
    ///
    /// For `/translate delin`. The full [`Self::flush_translate_queue`] would
    /// also drop the reservations held by outgoing sends that are still in
    /// flight — a different direction, still enabled, whose echoes would then
    /// render after the replies to them.
    pub(crate) fn flush_pending_translations(&mut self, buffer_id: &str) {
        let ready = {
            let Some(queue) = self.state.translate_queues.get_mut(buffer_id) else {
                return;
            };
            queue.flush_pending()
        };
        if !ready.is_empty() {
            tracing::debug!(
                buffer_id,
                count = ready.len(),
                "translate: released incoming lines on delin"
            );
        }
        self.release_translated(buffer_id, ready);
        self.prune_empty_translate_queues();
    }

    /// Flush the queues of every buffer belonging to one connection.
    ///
    /// Used on disconnect: those lines already arrived, and waiting out the
    /// full timeout for a server that is gone blanks the channel for no
    /// possible benefit.
    pub(crate) fn flush_translate_queues_for_connection(&mut self, conn_id: &str) {
        let belongs = |id: &String, state: &crate::state::AppState| {
            state
                .buffers
                .get(id)
                .is_some_and(|b| b.connection_id == conn_id)
        };
        let buffer_ids: Vec<String> = self
            .state
            .translate_queues
            .keys()
            .filter(|id| belongs(id, &self.state))
            .cloned()
            .collect();
        for buffer_id in buffer_ids {
            self.flush_translate_queue(&buffer_id);
        }
        // Reflection records die with the session that would have sent them.
        // Left behind, a reconnect inside the TTL that resends the same text
        // consumes the STALE record: the new reflection takes the old
        // reserved id and the old suffix, and the new reservation is left
        // blocking the buffer until it times out.
        //
        // Walked separately from the queues, because a record outlives the
        // queue whenever the reservation was the only thing in it — which is
        // the ordinary case for an outgoing send with nothing incoming.
        let stale: Vec<String> = self
            .state
            .own_echo_decorations
            .keys()
            .filter(|id| belongs(id, &self.state))
            .cloned()
            .collect();
        for buffer_id in stale {
            self.state.own_echo_decorations.remove(&buffer_id);
        }
    }

    /// Flush every buffer's queue. Used on quit and detach.
    #[allow(dead_code, reason = "called once the flush points are wired")]
    pub(crate) fn flush_all_translate_queues(&mut self) {
        let buffer_ids: Vec<String> = self.state.translate_queues.keys().cloned().collect();
        for buffer_id in buffer_ids {
            self.flush_translate_queue(&buffer_id);
        }
    }
}

#[cfg(test)]
mod app_tests {
    use super::*;
    use crate::state::buffer::{ActivityLevel, Buffer, BufferType, Message, MessageType};
    use crate::translate::TranslateOutcome;
    use crate::app::input::submit_typing_tests::test_app;
    use crate::translate::queue::{PendingPayload, TranslateQueue};

    const BUF: &str = "test/#dupa";

    /// A dispatch instant safely before anything the test does afterwards,
    /// so a rename that happens during the test counts as "after dispatch".
    fn dispatched_long_ago() -> std::time::Instant {
        std::time::Instant::now()
            .checked_sub(std::time::Duration::from_secs(1))
            .expect("a second before now")
    }

    fn message(id: u64, text: &str) -> Message {
        Message {
            id,
            timestamp: chrono::Utc::now(),
            message_type: MessageType::Message,
            nick: Some("alice".to_string()),
            nick_mode: None,
            text: text.to_string(),
            highlight: false,
            event_key: None,
            event_params: None,
            log_msg_id: None,
            log_ref_id: None,
            tags: None,
            wire_origin: None,
        }
    }

    fn payload(id: u64, text: &str) -> PendingPayload {
        PendingPayload {
            message: message(id, text),
            activity: ActivityLevel::Activity,
            show_original: false,
        }
    }

    /// App with one channel buffer and `count` lines queued for translation.
    fn app_with_queue(count: u64) -> crate::app::App {
        let mut app = test_app();
        app.state
            .add_buffer(Buffer::for_test("test", BufferType::Channel, "#dupa"));
        let mut queue = TranslateQueue::new();
        for id in 1..=count {
            queue.push_pending(id, format!("line {id}"), payload(id, &format!("line {id}")));
        }
        app.state.translate_queues.insert(BUF.to_string(), queue);
        app
    }

    fn shown(app: &crate::app::App) -> Vec<String> {
        app.state.buffers[BUF]
            .messages
            .iter()
            .map(|m| m.text.clone())
            .collect()
    }

    #[test]
    fn an_outcome_releases_only_up_to_the_first_pending_line() {
        let mut app = app_with_queue(3);
        app.apply_translate_deliver(TranslateDeliver::Incoming {
            buffer_id: BUF.to_string(),
            outcome: TranslateOutcome::Translated {
                id: 2,
                text: "second".to_string(),
            },
            submitted_at: dispatched_long_ago(),
        });
        assert!(
            shown(&app).is_empty(),
            "line 2 must wait for line 1 even though it finished first"
        );
        app.apply_translate_deliver(TranslateDeliver::Incoming {
            buffer_id: BUF.to_string(),
            outcome: TranslateOutcome::Translated {
                id: 1,
                text: "first".to_string(),
            },
            submitted_at: dispatched_long_ago(),
        });
        assert_eq!(
            shown(&app),
            vec!["first".to_string(), "second".to_string()],
            "both release, in arrival order"
        );
    }

    #[test]
    fn a_drained_queue_is_pruned() {
        let mut app = app_with_queue(1);
        app.apply_translate_deliver(TranslateDeliver::Incoming {
            buffer_id: BUF.to_string(),
            outcome: TranslateOutcome::Translated {
                id: 1,
                text: "x".to_string(),
            },
            submitted_at: dispatched_long_ago(),
        });
        assert!(
            !app.state.translate_queues.contains_key(BUF),
            "an empty queue must not linger for the tick to walk"
        );
    }

    #[test]
    fn an_outcome_for_a_vanished_queue_is_dropped_without_panicking() {
        let mut app = test_app();
        app.apply_translate_deliver(TranslateDeliver::Incoming {
            buffer_id: "test/#gone".to_string(),
            outcome: TranslateOutcome::Translated {
                id: 1,
                text: "x".to_string(),
            },
            submitted_at: dispatched_long_ago(),
        });
    }

    #[test]
    fn the_tick_releases_a_stuck_head_after_the_timeout() {
        let mut app = app_with_queue(2);
        app.config.translate.timeout_ms = 0;
        app.tick_translate_queues();
        assert_eq!(
            shown(&app),
            vec![
                "line 1 [untranslated: timeout]".to_string(),
                "line 2 [untranslated: timeout]".to_string(),
            ],
            "a timed-out line shows its original, marked, in order"
        );
        assert!(!app.state.translate_queues.contains_key(BUF));
    }

    #[test]
    fn the_tick_enforces_the_ceiling() {
        let mut app = app_with_queue(10);
        // Long timeout, so only the ceiling can release anything.
        app.config.translate.timeout_ms = 600_000;
        app.config.translate.max_queue = 4;
        app.tick_translate_queues();
        assert_eq!(shown(&app).len(), 6, "the six oldest are forced out");
        assert_eq!(
            app.state.translate_queues[BUF].len(),
            4,
            "the queue is bounded afterwards"
        );
    }

    #[test]
    fn the_tick_is_a_no_op_with_no_queues() {
        let mut app = test_app();
        app.tick_translate_queues();
        assert!(app.state.translate_queues.is_empty());
    }

    fn outgoing(text: &str, outcome: TranslateOutcome, show_original: bool) -> OutgoingTranslateDeliver {
        OutgoingTranslateDeliver {
            conn_id: "test".to_string(),
            buffer_id: BUF.to_string(),
            buffer_name: "#dupa".to_string(),
            buffer_type: BufferType::Channel,
            original_text: text.to_string(),
            outcome,
            nick: "me".to_string(),
            own_mode: None,
            peer_handle: None,
            show_original,
            is_action: false,
            echo: OutgoingEchoPlan::BufferInput,
            echo_id: 1,
            origin: SubmitOrigin::Tui,
            retry_text: text.to_string(),
            retry_body: text.to_string(),
            conn_generation: None,
            submitted_at: std::time::Instant::now(),
        }
    }

    fn app_with_buffer() -> crate::app::App {
        let mut app = test_app();
        app.state
            .add_buffer(Buffer::for_test("test", BufferType::Channel, "#dupa"));
        // A real client is always looking at SOMETHING, and for text typed
        // into a buffer that something is the buffer it was typed into.
        // Which conversation the composer belongs to decides how a refused
        // message is handed back — see `deferred_retry_text`.
        app.state.set_active_buffer(BUF);
        app
    }

    #[test]
    fn a_failed_outgoing_translation_sends_nothing_and_returns_the_text() {
        // Sending the original would transmit something other than what the
        // user intended for this channel. Fail closed and hand it back.
        let mut app = app_with_buffer();
        app.apply_translate_deliver(TranslateDeliver::Outgoing(Box::new(outgoing(
            "moje zdanie",
            TranslateOutcome::Untranslated {
                id: 1,
                reason: crate::translate::UntranslatedReason::NoProvider,
            },
            false,
        ))));

        assert_eq!(
            app.input.value, "moje zdanie",
            "the text is back in the input line"
        );
        let last = app.state.buffers[BUF]
            .messages
            .back()
            .expect("an error row explains why");
        assert!(
            last.text.contains("no provider"),
            "the reason is named: {}",
            last.text
        );
    }

    /// Every way a deferred send can be abandoned, as
    /// `(setup, expected reason fragment)`. Each must put the user's text in
    /// the error row, because the composer restore is a no-op whenever the
    /// user has started typing again — which, seconds after they hit Enter,
    /// is the normal case.
    #[test]
    fn every_deferred_failure_names_the_text_it_could_not_send() {
        type Setup = fn() -> (crate::app::App, OutgoingTranslateDeliver);
        let cases: [(&str, Setup); 4] = [
            ("translation failed", || {
                (
                    app_with_buffer(),
                    outgoing(
                        "moje zdanie",
                        TranslateOutcome::Untranslated {
                            id: 1,
                            reason: crate::translate::UntranslatedReason::NoProvider,
                        },
                        false,
                    ),
                )
            }),
            ("the connection is unavailable", || {
                (
                    app_with_buffer(),
                    outgoing(
                        "moje zdanie",
                        TranslateOutcome::Translated {
                            id: 1,
                            text: "mein satz".to_string(),
                        },
                        false,
                    ),
                )
            }),
            ("re-established", || {
                let mut app = app_with_dying_handle(usize::MAX);
                app.conn_generations.insert("test".to_string(), 7);
                let mut out = outgoing(
                    "moje zdanie",
                    TranslateOutcome::Translated {
                        id: 1,
                        text: "mein satz".to_string(),
                    },
                    false,
                );
                out.conn_generation = Some(6); // the session before this one
                (app, out)
            }),
            ("the send failed", || {
                let mut app = app_with_dying_handle(0);
                app.conn_generations.insert("test".to_string(), 1);
                let mut out = outgoing(
                    "moje zdanie",
                    TranslateOutcome::Translated {
                        id: 1,
                        text: "mein satz".to_string(),
                    },
                    false,
                );
                out.conn_generation = Some(1);
                (app, out)
            }),
        ];

        for (fragment, setup) in cases {
            let (mut app, out) = setup();
            // The user started the next message during the wait, so the
            // composer will not be overwritten. This is the case that loses
            // the message when the row carries only a reason.
            app.input.value = "something else".to_string();
            app.state.reserve_echo_slot(BUF, out.echo_id);
            app.apply_translate_deliver(TranslateDeliver::Outgoing(Box::new(out)));

            let rows: Vec<String> = app.state.buffers[BUF]
                .messages
                .iter()
                .map(|m| m.text.clone())
                .collect();
            assert!(
                rows.iter().any(|t| t.contains(fragment)),
                "{fragment:?} must be named: {rows:?}"
            );
            assert!(
                rows.iter().any(|t| t.contains("moje zdanie")),
                "{fragment:?}: the text is unrecoverable unless the row \
                 carries it — the composer is busy: {rows:?}"
            );
            assert_eq!(
                app.input.value, "something else",
                "{fragment:?}: and what the user is typing now is left alone"
            );
        }
    }

    /// An app with a query on `frank` that has since renamed to `frankie`,
    /// exactly as `rekey_buffer_state` leaves things.
    fn app_after_a_query_rename() -> crate::app::App {
        let mut app = app_with_dying_handle(usize::MAX);
        app.conn_generations.insert("test".to_string(), 1);
        // `test/frank` is deliberately absent: a rename removes it.
        app.state
            .add_buffer(Buffer::for_test("test", BufferType::Query, "frankie"));
        app.state.rekey_buffer_state("test/frank", "test/frankie");
        app
    }

    fn query_rows(app: &crate::app::App) -> Vec<String> {
        app.state.buffers["test/frankie"]
            .messages
            .iter()
            .map(|m| m.text.clone())
            .collect()
    }

    /// A failed private message, submitted from `bob`'s query.
    fn refused_query_send(app: &mut crate::app::App) -> Box<OutgoingTranslateDeliver> {
        app.state
            .add_buffer(Buffer::for_test("test", BufferType::Query, "bob"));
        let mut out = outgoing(
            "sekret",
            TranslateOutcome::Untranslated {
                id: 1,
                reason: crate::translate::UntranslatedReason::NoProvider,
            },
            false,
        );
        out.buffer_id = "test/bob".to_string();
        out.buffer_name = "bob".to_string();
        out.buffer_type = BufferType::Query;
        Box::new(out)
    }

    #[test]
    fn a_refused_private_message_is_not_left_aimed_at_another_conversation() {
        // Switching away while a translation runs is the natural thing to do.
        // Restoring the bare body into a composer that now belongs to a
        // PUBLIC channel publishes the private message there the moment the
        // user presses Enter.
        let mut app = app_with_buffer(); // active buffer is #dupa
        let out = refused_query_send(&mut app);
        assert_eq!(
            app.state.active_buffer_id.as_deref(),
            Some(BUF),
            "precondition: the user has moved to the channel"
        );

        app.apply_translate_deliver(TranslateDeliver::Outgoing(out));

        assert_eq!(
            app.input.value, "/msg bob sekret",
            "what comes back must address bob explicitly, so Enter cannot \
             publish it to #dupa"
        );
    }

    #[test]
    fn a_refused_message_comes_back_bare_when_its_conversation_is_still_open() {
        // The re-addressing must not fire when the user never left, or every
        // ordinary refusal would hand back a `/msg` they did not type.
        let mut app = app_with_buffer();
        app.state
            .add_buffer(Buffer::for_test("test", BufferType::Query, "bob"));
        app.state.set_active_buffer("test/bob");
        let mut out = refused_query_send(&mut app);
        out.retry_text = "sekret".to_string();

        app.apply_translate_deliver(TranslateDeliver::Outgoing(out));

        assert_eq!(
            app.input.value, "sekret",
            "still in bob's query, so the composer takes it as typed"
        );
    }

    #[test]
    fn a_refused_action_is_not_restored_from_another_conversation() {
        // `/me` acts on whatever buffer is active, and there is no spelling
        // that re-addresses one. Restoring it would perform the action in
        // the wrong place; the error row keeps the text instead.
        let mut app = app_with_buffer();
        let mut out = refused_query_send(&mut app);
        out.is_action = true;
        out.retry_text = "/me wzdycha".to_string();

        app.apply_translate_deliver(TranslateDeliver::Outgoing(out));

        assert!(
            app.input.value.is_empty(),
            "no safe form exists, so nothing is restored: {:?}",
            app.input.value
        );
        let rows: Vec<String> = app.state.buffers["test/bob"]
            .messages
            .iter()
            .map(|m| m.text.clone())
            .collect();
        assert!(
            rows.iter().any(|t| t.contains("sekret") && t.contains("to bob")),
            "and the row says what it was and where it was going: {rows:?}"
        );
    }

    #[test]
    fn a_reflected_echo_keeps_its_place_among_the_lines_it_was_sent_before() {
        // On an echo-message server the server owns the row, so it arrives
        // seconds later. Releasing the reservation when the send goes out
        // lets everything queued behind it drain first, and the user's own
        // message then appears BELOW the replies to it.
        let mut app = app_with_echo_message();
        let mut queue = TranslateQueue::new();
        queue.reserve(1); // our message, submitted first
        app.state.translate_queues.insert(BUF.to_string(), queue);

        send_translated(&mut app, 1, "moje zdanie", "mein satz");

        // A reply that arrived — and finished translating — during the wait.
        // A LATER id than the echo's, which is what makes it queue behind.
        let reply_id = 5;
        {
            let queue = app
                .state
                .translate_queues
                .get_mut(BUF)
                .expect("the reservation keeps the queue alive");
            queue.push_pending(
                reply_id,
                "antwort".to_string(),
                payload(reply_id, "antwort"),
            );
            queue.resolve(reply_id, Ok("odpowiedz".to_string()));
        }
        app.apply_translate_deliver(TranslateDeliver::Incoming {
            buffer_id: BUF.to_string(),
            outcome: TranslateOutcome::Translated {
                id: reply_id,
                text: "odpowiedz".to_string(),
            },
            submitted_at: std::time::Instant::now(),
        });
        assert!(
            shown(&app).is_empty(),
            "the reply waits behind our reservation: {:?}",
            shown(&app)
        );

        reflect(&mut app, "mein satz", "server-M1");

        assert_eq!(
            shown(&app),
            vec![
                "mein satz [moje zdanie]".to_string(),
                "odpowiedz".to_string(),
            ],
            "our own message first — it was sent first"
        );
    }

    #[test]
    fn a_split_echo_gives_every_chunk_its_own_transport_id() {
        // The web client takes two live rows sharing an id for the same
        // message and drops the second, so conflating the queue's ordering
        // key with the transport id swallowed every chunk after the first —
        // usually including the one carrying ` [original]`.
        let mut app = app_with_dying_handle(usize::MAX);
        app.conn_generations.insert("test".to_string(), 1);
        let long = "wieloslowne zdanie ".repeat(30);
        let mut out = outgoing(
            "krotkie",
            TranslateOutcome::Translated {
                id: 1,
                text: long.trim().to_string(),
            },
            true, // show_original — this is what pushes it over the budget
        );
        out.conn_generation = Some(1);
        app.state.reserve_echo_slot(BUF, 1);

        app.apply_translate_deliver(TranslateDeliver::Outgoing(Box::new(out)));

        let ids: Vec<u64> = app.state.buffers[BUF].messages.iter().map(|m| m.id).collect();
        assert!(ids.len() > 1, "this echo must actually split: {ids:?}");
        let unique: std::collections::HashSet<u64> = ids.iter().copied().collect();
        assert_eq!(
            unique.len(),
            ids.len(),
            "every chunk needs its own transport id or the browser drops it: {ids:?}"
        );
        assert!(
            shown(&app).last().is_some_and(|t| t.contains("[krotkie]")),
            "and the last chunk — the one with the original — survives: {:?}",
            shown(&app)
        );
    }

    #[test]
    fn a_dropped_connection_forgets_what_it_was_waiting_to_reflect() {
        // A reconnect inside the record's TTL that resends the same text
        // would otherwise consume the STALE record: the new reflection takes
        // the old reserved id and suffix, and the new reservation is left
        // blocking the buffer until it times out.
        let mut app = app_with_echo_message();
        send_translated(&mut app, 1, "moje zdanie", "mein satz");
        assert!(
            app.state.own_echo_decorations.contains_key(BUF),
            "precondition: a reflection is expected"
        );

        app.flush_translate_queues_for_connection("test");

        assert!(
            !app.state.own_echo_decorations.contains_key(BUF),
            "the record dies with the session that would have sent it"
        );
    }

    #[test]
    fn restoring_a_reduced_limit_before_it_lands_cancels_the_reduction() {
        // 4 -> 2 with every permit checked out records a debt of 2 and
        // applies nothing. Putting the target back to 4 must drop that debt:
        // otherwise the tick keeps forgetting permits as work returns them,
        // the limiter settles at the ABANDONED 2, and stays there while the
        // config says 4.
        let mut app = test_app();
        let limiter = std::sync::Arc::new(tokio::sync::Semaphore::new(4));
        let held: Vec<_> = (0..4)
            .map(|_| std::sync::Arc::clone(&limiter).try_acquire_owned().expect("permit"))
            .collect();
        app.translate_in_flight = Some(std::sync::Arc::clone(&limiter));
        app.translate_in_flight_applied = 4;

        app.config.translate.max_in_flight = 2;
        app.sync_translate_from_config();
        assert_eq!(
            app.translate_in_flight_debt, 2,
            "nothing could be forgotten while every permit is out"
        );

        app.config.translate.max_in_flight = 4;
        app.sync_translate_from_config();
        assert_eq!(
            app.translate_in_flight_debt, 0,
            "the reduction was abandoned before it ever took effect"
        );

        drop(held);
        app.settle_translate_concurrency_debt();
        assert_eq!(
            limiter.available_permits(),
            4,
            "so all four permits stay available, as the config asks"
        );
    }

    #[test]
    fn a_translation_longer_than_the_source_is_split_before_it_is_sent() {
        // The policy refuses a SOURCE line over the budget, but a
        // translation can be longer than what it translates. Every wire
        // payload still has to fit — `send_privmsg` breaks on CRLF only,
        // never by length.
        let mut app = app_with_dying_handle(usize::MAX);
        app.conn_generations.insert("test".to_string(), 1);
        let sender = app.irc_handles["test"].sender().clone();
        let long = "wieloslowne zdanie ".repeat(40);
        assert!(
            long.len() > crate::irc::MESSAGE_MAX_BYTES,
            "precondition: this translation does not fit one line"
        );
        let mut out = outgoing(
            "krotkie",
            TranslateOutcome::Translated {
                id: 1,
                text: long.trim().to_string(),
            },
            false,
        );
        out.conn_generation = Some(1);

        app.apply_translate_deliver(TranslateDeliver::Outgoing(Box::new(out)));

        let wire: Vec<String> = sender.captured().iter().map(ToString::to_string).collect();
        assert!(wire.len() > 1, "it must go out in pieces: {}", wire.len());
        for line in &wire {
            assert!(
                line.len() <= crate::irc::MESSAGE_MAX_BYTES + "PRIVMSG #dupa :\r\n".len(),
                "a payload over the budget would be truncated by the server: \
                 {} bytes",
                line.len()
            );
        }
    }

    #[test]
    fn a_backend_cannot_inject_an_irc_command_through_a_line_break() {
        // The backend is untrusted and its output goes on the socket.
        // `send_privmsg` only breaks on \r\n, so a bare \n rides into the
        // trailing parameter — and a server accepting bare-LF endings reads
        // everything after it as a fresh command.
        let mut app = app_with_dying_handle(usize::MAX);
        app.conn_generations.insert("test".to_string(), 1);
        let sender = app.irc_handles["test"].sender().clone();
        let mut out = outgoing(
            "moje zdanie",
            TranslateOutcome::Translated {
                id: 1,
                text: "hello\nJOIN #evil".to_string(),
            },
            false,
        );
        out.conn_generation = Some(1);
        app.state.reserve_echo_slot(BUF, 1);

        app.apply_translate_deliver(TranslateDeliver::Outgoing(Box::new(out)));

        let wire: Vec<String> = sender.captured().iter().map(ToString::to_string).collect();
        assert!(
            wire.is_empty(),
            "nothing containing a line break may reach the socket: {wire:?}"
        );
        assert_eq!(
            app.input.value, "moje zdanie",
            "and the user's text comes back rather than being lost"
        );
    }

    #[test]
    fn a_stale_send_is_refused_rather_than_delivered_to_the_new_nick() {
        // `frank` renamed to `frankie` while this was translating, and their
        // window has since closed. The only name left to address is `frank`
        // — which somebody else may now hold. This is a private message.
        let mut app = app_with_dying_handle(usize::MAX);
        app.conn_generations.insert("test".to_string(), 1);
        app.state.rekey_buffer_state("test/frank", "test/frankie");
        // Renamed, then closed: no `test/frankie` buffer exists.
        let sender = app.irc_handles["test"].sender().clone();
        let mut out = outgoing(
            "moje zdanie",
            TranslateOutcome::Translated {
                id: 1,
                text: "mein satz".to_string(),
            },
            false,
        );
        out.buffer_id = "test/frank".to_string();
        out.buffer_name = "frank".to_string();
        out.buffer_type = BufferType::Query;
        out.conn_generation = Some(1);
        out.submitted_at = dispatched_long_ago();

        app.apply_translate_deliver(TranslateDeliver::Outgoing(Box::new(out)));

        let wire: Vec<String> = sender.captured().iter().map(ToString::to_string).collect();
        assert!(
            wire.is_empty(),
            "a private message must not go to whoever holds the old nick now: {wire:?}"
        );
        assert!(
            app.input.value.is_empty(),
            "and it is NOT handed back as a ready-to-send `/msg frank …`, \
             which would be the same leak one keystroke away: {:?}",
            app.input.value
        );
        let rows = shown(&app);
        assert!(
            rows.iter().any(|t| t.contains("moje zdanie")),
            "the text is still recoverable from the error row: {rows:?}"
        );
    }

    #[test]
    fn a_split_echo_stays_in_one_piece_around_an_incoming_line() {
        // `show_original_out` appends the original, which routinely pushes a
        // translated echo past the byte budget. All its chunks belong to the
        // ONE reserved place: fresh ids for the continuations let a line that
        // arrived during the translation sort between them, splitting the
        // user's own sentence around somebody else's reply.
        let mut app = app_with_dying_handle(usize::MAX);
        app.conn_generations.insert("test".to_string(), 1);
        let mut queue = TranslateQueue::new();
        queue.reserve(1); // our echo, submitted first
        app.state.translate_queues.insert(BUF.to_string(), queue);
        // A reply that arrived while we were translating: a LATER id.
        let reply_id = app.state.next_message_id();
        app.state.add_message_with_activity(
            BUF,
            message(reply_id, "a reply that arrived meanwhile"),
            ActivityLevel::Activity,
        );

        let long = "wieloslowne zdanie ".repeat(30);
        let mut out = outgoing(
            "krotkie",
            TranslateOutcome::Translated {
                id: 1,
                text: long.trim().to_string(),
            },
            true, // show_original — this is what makes it split
        );
        out.conn_generation = Some(1);
        app.apply_translate_deliver(TranslateDeliver::Outgoing(Box::new(out)));

        let rows = shown(&app);
        let reply_at = rows
            .iter()
            .position(|t| t.contains("a reply that arrived meanwhile"))
            .expect("the reply is displayed");
        assert!(
            reply_at > 0,
            "our own message comes first — it was submitted first: {rows:?}"
        );
        assert_eq!(
            reply_at,
            rows.len() - 1,
            "and ALL of it comes first: the reply must not land inside it: {rows:?}"
        );
    }

    #[test]
    fn a_translation_that_came_back_too_late_is_not_sent() {
        // Its display reservation expired on schedule, so there is nowhere
        // for the echo to go and the user has already watched the line
        // vanish. Sending now publishes a message they may have retyped.
        let mut app = app_with_dying_handle(usize::MAX);
        app.conn_generations.insert("test".to_string(), 1);
        app.config.translate.timeout_ms = 500;
        let sender = app.irc_handles["test"].sender().clone();
        let mut out = outgoing(
            "moje zdanie",
            TranslateOutcome::Translated {
                id: 1,
                text: "mein satz".to_string(),
            },
            false,
        );
        out.conn_generation = Some(1);
        out.submitted_at = std::time::Instant::now()
            .checked_sub(std::time::Duration::from_secs(30))
            .expect("30s before now");
        app.state.reserve_echo_slot(BUF, 1);

        app.apply_translate_deliver(TranslateDeliver::Outgoing(Box::new(out)));

        assert!(
            sender.captured().is_empty(),
            "nothing may reach the wire this late: {:?}",
            sender.captured()
        );
        assert_eq!(
            app.input.value, "moje zdanie",
            "and the text comes back rather than being lost"
        );
        assert_eq!(queued(&app), 0, "the reservation is given back");
    }

    #[test]
    fn a_configured_target_translates_without_an_open_buffer() {
        // A script's `say()` addresses a target by name and has always been
        // able to reach one the user has no window on. Requiring an open
        // buffer here dropped those sends outright — and translation is a
        // property of the conversation, not of whether it is on screen.
        let mut app = app_with_outgoing(Some("de"));
        assert!(
            !app.state.buffers.contains_key("test/#nowhere"),
            "precondition: no buffer for this target"
        );
        set_langs_for(&mut app, "test/#nowhere", Some("de"));

        let pending = app.build_outgoing_translate(&OutgoingRequest {
            conn_id: "test",
            buffer_id: "test/#nowhere",
            buffer_name: "#nowhere",
            buffer_type: &BufferType::Channel,
            nick: "me",
            text: "moje zdanie",
            is_action: false,
            echo: OutgoingEchoPlan::None,
        });

        let pending = pending.expect("a configured target must still be translated");
        assert_eq!(pending.req.text, "moje zdanie");
        assert_eq!(pending.req.target_lang, "de");
        assert!(
            pending.req.known_nicks.is_empty(),
            "no buffer means no nick list — which is all the buffer supplied"
        );
    }

    #[test]
    fn an_outgoing_send_follows_a_query_that_renamed_mid_translation() {
        // The message was addressed to `frank`; by the time the translation
        // came back they are `frankie`. Sending to the old nick reaches
        // nobody — or, if somebody else has claimed it, a stranger.
        let mut app = app_after_a_query_rename();
        let sender = app.irc_handles["test"].sender().clone();
        let mut out = outgoing(
            "moje zdanie",
            TranslateOutcome::Translated {
                id: 1,
                text: "mein satz".to_string(),
            },
            false,
        );
        out.buffer_id = "test/frank".to_string();
        out.buffer_name = "frank".to_string();
        out.buffer_type = BufferType::Query;
        out.conn_generation = Some(1);
        // Submitted while they were still `frank` — which is what makes the
        // rename apply to this message.
        out.submitted_at = dispatched_long_ago();

        app.apply_translate_deliver(TranslateDeliver::Outgoing(Box::new(out)));

        let wire: Vec<String> = sender.captured().iter().map(ToString::to_string).collect();
        assert!(
            wire.iter().any(|w| w.contains("frankie")),
            "addressed to who they are now: {wire:?}"
        );
        assert!(
            !wire.iter().any(|w| w.contains("PRIVMSG frank ")),
            "and never to the nick they left behind: {wire:?}"
        );
        assert!(
            query_rows(&app).iter().any(|t| t == "mein satz"),
            "the echo lands in the renamed buffer: {:?}",
            query_rows(&app)
        );
    }

    #[test]
    fn an_incoming_outcome_follows_a_query_that_renamed_mid_translation() {
        // The queue moved with the buffer, so an outcome addressed to the old
        // id finds nothing and its line sits until the timeout — rendering a
        // perfectly good translation as `[untranslated: timeout]`.
        let mut app = app_with_buffer();
        app.state
            .add_buffer(Buffer::for_test("test", BufferType::Query, "frank"));
        let mut queue = TranslateQueue::new();
        queue.push_pending(1, "guten tag".to_string(), payload(1, "guten tag"));
        app.state
            .translate_queues
            .insert("test/frank".to_string(), queue);

        // The rename itself: the old buffer goes, the new one arrives.
        app.state.buffers.shift_remove("test/frank");
        app.state
            .add_buffer(Buffer::for_test("test", BufferType::Query, "frankie"));
        app.state.rekey_buffer_state("test/frank", "test/frankie");

        app.apply_translate_deliver(TranslateDeliver::Incoming {
            buffer_id: "test/frank".to_string(), // dispatched under the old id
            outcome: TranslateOutcome::Translated {
                id: 1,
                text: "dzien dobry".to_string(),
            },
            submitted_at: dispatched_long_ago(),
        });

        assert_eq!(
            query_rows(&app),
            vec!["dzien dobry".to_string()],
            "the outcome finds its moved queue: {:?}",
            query_rows(&app)
        );
    }

    #[test]
    fn a_redirect_applies_by_dispatch_time_not_by_which_buffers_exist() {
        // Somebody else takes the abandoned nick and the user opens a query
        // with THEM. Both conversations now key off `test/frank` at
        // different times, and only the timestamp separates them.
        let mut app = app_after_a_query_rename();
        let before_the_rename = dispatched_long_ago();
        app.state
            .add_buffer(Buffer::for_test("test", BufferType::Query, "frank"));

        assert_eq!(
            app.state
                .redirected_buffer_id("test/frank", before_the_rename),
            Some("test/frankie"),
            "work from before the rename belongs to the peer who moved — \
             even though a live buffer now sits under the old id"
        );
        assert_eq!(
            app.state
                .redirected_buffer_id("test/frank", std::time::Instant::now()),
            None,
            "work dispatched since belongs to whoever holds the nick now"
        );
    }

    #[test]
    fn every_translate_mirror_is_derived_from_the_config_in_one_place() {
        // `App::new` used to hand-copy these, and the copy fell behind:
        // `translate_max_queue` sat at its hardcoded default until the user
        // happened to run `/set`, `/reload` or `/translate`. With the ceiling
        // enforced on every insertion that is simply the wrong bound from
        // startup. Deriving them all through one function is what stops the
        // next mirror doing the same.
        let mut app = test_app();
        app.state.translate_max_queue = 999; // a stale value from anywhere
        app.config.translate.max_queue = 7;
        app.config.translate.show_original_in = false;
        app.config.translate.my_lang = "cs".to_string();

        app.sync_translate_from_config();

        assert_eq!(app.state.translate_max_queue, 7);
        assert!(!app.state.translate_show_original_in);
        assert_eq!(app.state.translate_my_lang, "cs");
    }

    #[test]
    fn a_second_rename_does_not_widen_the_first_redirect() {
        // frank -> frankie -> frankie2. Somebody claims `frank` in between,
        // and the user sends them a translated private message. That message
        // was dispatched AFTER the first rename, so no redirect covers it —
        // unless repointing the old redirect restamped it, which would make
        // it look like it was created by the SECOND rename and hand the
        // stranger's message to the original peer.
        let mut app = app_with_buffer();
        app.state.rekey_buffer_state("test/frank", "test/frankie");
        let submitted_between = std::time::Instant::now();
        app.state.rekey_buffer_state("test/frankie", "test/frankie2");

        assert_eq!(
            app.state
                .redirected_buffer_id("test/frank", submitted_between),
            None,
            "a message to whoever holds `frank` NOW must not follow the peer \
             who left before it was sent"
        );
        // The redirect still works for what it was made for.
        assert_eq!(
            app.state
                .redirected_buffer_id("test/frank", dispatched_long_ago()),
            Some("test/frankie2"),
            "and work from before the first rename still follows the peer, \
             all the way to where they are now"
        );
    }

    #[test]
    fn a_rekeyed_query_keeps_translating_after_a_sync() {
        // `sync_translate_from_config` re-derives the state mirror from the
        // config, so moving only the mirror is undone by the next `/set` or
        // `/reload`. The config key the App owns has to move too.
        let mut app = app_with_buffer();
        app.config.translate.enabled = true;
        app.translate_backend = Some(std::sync::Arc::new(
            crate::translate::backend::StubBackend::new(0, 0),
        ));
        app.config.translate.buffers.insert(
            "test/frank".to_string(),
            crate::config::TranslateBufferConfig {
                incoming: true,
                outgoing: false,
                lang: Some("de".to_string()),
                my_lang: None,
            },
        );
        app.sync_translate_from_config();
        app.state
            .pending_buffer_rekeys
            .push(("test/frank".to_string(), "test/frankie".to_string()));

        app.drain_pending_buffer_rekeys();

        assert!(
            app.config.translate.buffers.contains_key("test/frankie"),
            "the config key follows the conversation"
        );
        assert!(!app.config.translate.buffers.contains_key("test/frank"));
        // The mirror must survive a re-derive, which is the whole point.
        app.sync_translate_from_config();
        assert!(
            app.state.translate_buffers.contains_key("test/frankie"),
            "and a later sync does not undo it"
        );
    }

    #[test]
    fn a_reconnect_during_translation_does_not_send_on_the_new_session() {
        // The pre-disconnect message must not appear on the session that
        // replaced it: the user may have left that channel, and minutes may
        // have passed.
        let mut app = app_with_dying_handle(usize::MAX);
        app.conn_generations.insert("test".to_string(), 2);
        let mut out = outgoing(
            "moje zdanie",
            TranslateOutcome::Translated {
                id: 1,
                text: "mein satz".to_string(),
            },
            false,
        );
        out.conn_generation = Some(1);
        let sender = app.irc_handles["test"].sender().clone();
        app.state.reserve_echo_slot(BUF, 1);

        app.apply_translate_deliver(TranslateDeliver::Outgoing(Box::new(out)));

        assert!(
            sender.captured().is_empty(),
            "nothing may reach the replacement session: {:?}",
            sender.captured()
        );
        assert_eq!(
            queued(&app),
            0,
            "and the reservation is given back rather than stalling the buffer"
        );
    }

    #[test]
    fn a_send_on_the_same_session_still_goes_out() {
        // The guard above must not refuse the ordinary case, or it would
        // pass by refusing everything.
        let mut app = app_with_dying_handle(usize::MAX);
        app.conn_generations.insert("test".to_string(), 2);
        let mut out = outgoing(
            "moje zdanie",
            TranslateOutcome::Translated {
                id: 1,
                text: "mein satz".to_string(),
            },
            false,
        );
        out.conn_generation = Some(2);
        let sender = app.irc_handles["test"].sender().clone();

        app.apply_translate_deliver(TranslateDeliver::Outgoing(Box::new(out)));

        let wire: Vec<String> = sender.captured().iter().map(ToString::to_string).collect();
        assert!(
            wire.iter().any(|w| w.contains("mein satz")),
            "the translation reaches the wire: {wire:?}"
        );
    }

    #[test]
    fn a_failed_outgoing_translation_does_not_clobber_new_typing() {
        // The user may have typed something else during the wait; replacing
        // it would be a second, worse surprise. The text is still visible in
        // the error row, so it is never truly lost.
        let mut app = app_with_buffer();
        app.input.value = "something else".to_string();
        app.apply_translate_deliver(TranslateDeliver::Outgoing(Box::new(outgoing(
            "moje zdanie",
            TranslateOutcome::Untranslated {
                id: 1,
                reason: crate::translate::UntranslatedReason::Timeout,
            },
            false,
        ))));
        assert_eq!(app.input.value, "something else");
    }

    fn outgoing_with_echo(
        text: &str,
        outcome: TranslateOutcome,
        echo: OutgoingEchoPlan,
    ) -> OutgoingTranslateDeliver {
        let mut d = outgoing(text, outcome, false);
        d.echo = echo;
        d
    }

    #[test]
    fn a_short_action_is_one_well_formed_ctcp() {
        let wires = super::wrap_outgoing_body("waves hello", true);
        assert_eq!(wires, vec!["\x01ACTION waves hello\x01".to_string()]);
    }

    #[test]
    fn a_plain_message_is_never_wrapped() {
        let wires = super::wrap_outgoing_body("hello there", false);
        assert_eq!(wires, vec!["hello there".to_string()]);
    }

    #[test]
    fn an_overlong_action_splits_into_several_valid_ctcps() {
        // A translation can be longer than its source. Splitting the framed
        // string instead of the body would hand peers a first chunk with an
        // opening delimiter and a last with only the closing one.
        let body = "abcdefghij ".repeat(80);
        let wires = super::wrap_outgoing_body(body.trim(), true);
        assert!(wires.len() > 1, "this body must actually split");
        for wire in &wires {
            assert!(wire.starts_with("\x01ACTION "), "each chunk opens: {wire:?}");
            assert!(wire.ends_with('\x01'), "each chunk closes: {wire:?}");
            assert!(
                wire.len() <= crate::irc::MESSAGE_MAX_BYTES,
                "each chunk fits the budget: {} bytes",
                wire.len()
            );
        }
        // No words are lost or reordered. Compared word-wise, not
        // byte-wise: `split_irc_message` consumes the whitespace it breaks
        // on, which is its existing intended behaviour.
        let words: Vec<&str> = wires
            .iter()
            .flat_map(|w| {
                w.strip_prefix("\x01ACTION ")
                    .and_then(|t| t.strip_suffix('\x01'))
                    .expect("well formed")
                    .split_whitespace()
            })
            .collect();
        assert_eq!(
            words,
            body.split_whitespace().collect::<Vec<_>>(),
            "every word survives the split, in order"
        );
    }

    #[test]
    fn a_script_send_that_wants_no_echo_does_not_get_one() {
        // Script plaintext sends historically never echoed; inventing one
        // double-renders for scripts that print their own output.
        let mut app = app_with_buffer();
        let before = app.state.buffers[BUF].messages.len();
        app.apply_translate_deliver(TranslateDeliver::Outgoing(Box::new(outgoing_with_echo(
            "moje zdanie",
            TranslateOutcome::Translated {
                id: 1,
                text: "mein satz".to_string(),
            },
            OutgoingEchoPlan::None,
        ))));
        // The send itself fails (no IRC handle) and reports that, but no
        // echo row may be added on top.
        let texts: Vec<String> = app.state.buffers[BUF]
            .messages
            .iter()
            .skip(before)
            .map(|m| m.text.clone())
            .collect();
        assert!(
            texts.iter().all(|t| !t.contains("mein satz")),
            "no echo was requested: {texts:?}"
        );
    }

    #[test]
    fn a_gated_echo_uses_the_callers_buffer_and_message_type() {
        let mut app = app_with_buffer();
        app.state
            .add_buffer(Buffer::for_test("test", BufferType::Query, "bob"));
        let plan = OutgoingEchoPlan::Gated {
            buffer_id: "test/bob".to_string(),
            message_type: MessageType::Action,
            even_without_encryption: true,
        };
        app.write_translated_local_echo_for_test(
            &outgoing_with_echo(
                "macha",
                TranslateOutcome::Translated {
                    id: 1,
                    text: "winkt".to_string(),
                },
                plan,
            ),
            "winkt",
        );
        let msg = app.state.buffers["test/bob"]
            .messages
            .back()
            .expect("echoed into the caller's buffer, not the send target");
        assert_eq!(msg.text, "winkt");
        assert_eq!(msg.message_type, MessageType::Action);
    }

    #[test]
    fn a_filtered_outgoing_line_is_sent_as_the_original() {
        // Filtered means the broker correctly decided no translation was
        // needed, so the original IS the right thing to send. With no IRC
        // handle the send fails, which is exactly what proves we got as far
        // as attempting it rather than refusing up front.
        let mut app = app_with_buffer();
        app.apply_translate_deliver(TranslateDeliver::Outgoing(Box::new(outgoing(
            "moin",
            TranslateOutcome::Untranslated {
                id: 1,
                reason: crate::translate::UntranslatedReason::Filtered,
            },
            false,
        ))));
        let last = app.state.buffers[BUF].messages.back().unwrap();
        assert!(
            last.text.contains("the connection is unavailable"),
            "a filtered line reaches the send attempt rather than being refused: {}",
            last.text
        );
        assert_eq!(
            app.input.value, "moin",
            "and since the connection vanished mid-flight, the text comes back \
             instead of being lost"
        );
    }

    /// `app_with_buffer` plus an IRC handle whose sender accepts
    /// `ok_before_failure` frames and then fails — a writer task that goes
    /// away AFTER the precheck found the handle present.
    fn app_with_dying_handle(ok_before_failure: usize) -> crate::app::App {
        let mut app = app_with_buffer();
        app.irc_handles.insert(
            "test".to_string(),
            crate::irc::handle::IrcHandle::new(
                "test".to_string(),
                crate::irc::handle::IrcSender::capturing_then_failing(ok_before_failure),
                None,
                None,
            ),
        );
        app
    }

    /// How many entries — reservations included — are still parked in this
    /// buffer's queue.
    fn queued(app: &crate::app::App) -> usize {
        app.state
            .translate_queues
            .get(BUF)
            .map_or(0, crate::translate::queue::TranslateQueue::len)
    }

    #[test]
    fn a_send_that_fails_outright_returns_the_text_and_frees_the_queue() {
        // The handle was present at the precheck and the send failed anyway.
        // Nothing reached the wire and the composer was cleared at submit, so
        // the message exists nowhere unless it comes back.
        let mut app = app_with_dying_handle(0);
        app.state.reserve_echo_slot(BUF, 1);
        app.apply_translate_deliver(TranslateDeliver::Outgoing(Box::new(outgoing(
            "moje zdanie",
            TranslateOutcome::Translated {
                id: 1,
                text: "mein satz".to_string(),
            },
            false,
        ))));
        assert_eq!(
            app.input.value, "moje zdanie",
            "nothing was sent, so the text must come back"
        );
        assert_eq!(
            queued(&app),
            0,
            "the reservation is given back — leaving it barricades the buffer \
             until the queue timeout"
        );
    }

    #[test]
    fn a_send_that_fails_halfway_keeps_the_text_out_of_the_composer() {
        // A split message whose first chunk is already on the channel. Handing
        // the whole line back invites the user to press Enter and publish that
        // first chunk a second time.
        let mut app = app_with_dying_handle(1);
        app.state.reserve_echo_slot(BUF, 1);
        let long = "wieloslowne zdanie ".repeat(40);
        app.apply_translate_deliver(TranslateDeliver::Outgoing(Box::new(outgoing(
            "krotkie",
            TranslateOutcome::Translated {
                id: 1,
                text: long.trim().to_string(),
            },
            false,
        ))));
        assert!(
            app.input.value.is_empty(),
            "part of it is already published — do not offer to send that twice: {:?}",
            app.input.value
        );
        let rows: Vec<String> = app.state.buffers[BUF]
            .messages
            .iter()
            .map(|m| m.text.clone())
            .collect();
        assert!(
            rows.iter().any(|t| t.contains("Part of the message was sent")),
            "the partial send is named rather than silently dropped: {rows:?}"
        );
        assert!(
            rows.iter().any(|t| t.contains("krotkie")),
            "and the text is still visible somewhere: {rows:?}"
        );
        assert_eq!(
            queued(&app),
            0,
            "the reservation is given back on this path too"
        );
    }

    #[test]
    fn an_echo_releases_the_lines_queued_behind_it_at_once() {
        // The reservation is a barrier. Filling it makes the echo AND every
        // line that finished translating behind it deliverable — and the
        // outgoing arm is the one that never revisits the queue, so waiting
        // for the maintenance tick would blank the channel for a full second.
        let mut app = app_with_dying_handle(usize::MAX);
        let mut queue = TranslateQueue::new();
        queue.reserve(1);
        queue.push_pending(2, "line two".to_string(), payload(2, "line two"));
        queue.resolve(2, Ok("linia dwa".to_string()));
        app.state.translate_queues.insert(BUF.to_string(), queue);

        app.apply_translate_deliver(TranslateDeliver::Outgoing(Box::new(outgoing(
            "moje zdanie",
            TranslateOutcome::Translated {
                id: 1,
                text: "mein satz".to_string(),
            },
            false,
        ))));

        assert_eq!(
            shown(&app),
            vec!["mein satz".to_string(), "linia dwa".to_string()],
            "our own line, then the reply that was waiting behind it — with no \
             tick in between"
        );
        assert!(
            !app.state.translate_queues.contains_key(BUF),
            "and the drained queue is pruned"
        );
    }

    #[test]
    fn a_released_reservation_also_releases_what_was_behind_it() {
        // Same barrier, lifted the other way: the send is refused, so no echo
        // fills the slot. The line queued behind it must still come out now.
        let mut app = app_with_buffer();
        let mut queue = TranslateQueue::new();
        queue.reserve(1);
        queue.push_pending(2, "line two".to_string(), payload(2, "line two"));
        queue.resolve(2, Ok("linia dwa".to_string()));
        app.state.translate_queues.insert(BUF.to_string(), queue);

        app.apply_translate_deliver(TranslateDeliver::Outgoing(Box::new(outgoing(
            "moje zdanie",
            TranslateOutcome::Untranslated {
                id: 1,
                reason: crate::translate::UntranslatedReason::NoProvider,
            },
            false,
        ))));

        assert!(
            shown(&app).contains(&"linia dwa".to_string()),
            "the waiting line is released as soon as the barrier goes: {:?}",
            shown(&app)
        );
    }

    fn set_buffer_langs(app: &mut crate::app::App, lang: Option<&str>, my_lang: Option<&str>) {
        set_langs_for(app, BUF, lang);
        if let Some(mine) = my_lang
            && let Some(cfg) = app.config.translate.buffers.get_mut(BUF)
        {
            cfg.my_lang = Some(mine.to_string());
        }
    }

    /// Configure one buffer id for translation in both directions.
    fn set_langs_for(app: &mut crate::app::App, buffer_id: &str, lang: Option<&str>) {
        app.config.translate.buffers.insert(
            buffer_id.to_string(),
            crate::config::TranslateBufferConfig {
                incoming: true,
                outgoing: true,
                lang: lang.map(str::to_string),
                my_lang: None,
            },
        );
    }

    fn build(app: &mut crate::app::App) -> Option<PendingOutgoingTranslate> {
        app.build_outgoing_translate(&OutgoingRequest {
            conn_id: "test",
            buffer_id: BUF,
            buffer_name: "#dupa",
            buffer_type: &BufferType::Channel,
            nick: "me",
            text: "moje zdanie",
            is_action: false,
            echo: OutgoingEchoPlan::BufferInput,
        })
    }

    #[test]
    fn outgoing_translates_out_of_our_language_into_the_buffers() {
        // THE regression. These were inverted: a message typed in our own
        // language went to the broker labelled as the CHANNEL's language and
        // asked to become ours — a no-op at best, garbage at worst. It also
        // made writing to two channels in two languages impossible, since
        // the target came from a single global setting.
        let mut app = app_with_buffer();
        app.config.translate.my_lang = "pl".to_string();
        set_buffer_langs(&mut app, Some("de"), None);

        let pending = build(&mut app).expect("a request is built");
        assert_eq!(
            pending.req.source_lang.as_deref(),
            Some("pl"),
            "outgoing starts in OUR language"
        );
        assert_eq!(
            pending.req.target_lang, "de",
            "and lands in the language this buffer speaks"
        );
    }

    #[test]
    fn two_buffers_can_have_two_different_outgoing_targets() {
        // The user's actual objection: one global target_lang made this
        // impossible.
        let mut app = app_with_buffer();
        app.state
            .add_buffer(Buffer::for_test("test", BufferType::Channel, "#otro"));
        app.config.translate.my_lang = "pl".to_string();
        set_buffer_langs(&mut app, Some("de"), None);
        app.config.translate.buffers.insert(
            "test/#otro".to_string(),
            crate::config::TranslateBufferConfig {
                incoming: true,
                outgoing: true,
                lang: Some("es".to_string()),
                my_lang: None,
            },
        );

        let de = build(&mut app).expect("german buffer");
        let es = app
            .build_outgoing_translate(&OutgoingRequest {
                conn_id: "test",
                buffer_id: "test/#otro",
                buffer_name: "#otro",
                buffer_type: &BufferType::Channel,
                nick: "me",
                text: "moje zdanie",
                is_action: false,
                echo: OutgoingEchoPlan::BufferInput,
            })
            .expect("spanish buffer");
        assert_eq!(de.req.target_lang, "de");
        assert_eq!(es.req.target_lang, "es");
        assert_eq!(de.req.source_lang.as_deref(), Some("pl"));
        assert_eq!(es.req.source_lang.as_deref(), Some("pl"));
    }

    #[test]
    fn a_per_buffer_override_wins_for_outgoing_too() {
        let mut app = app_with_buffer();
        app.config.translate.my_lang = "pl".to_string();
        set_buffer_langs(&mut app, Some("zh"), Some("en"));
        let pending = build(&mut app).expect("a request is built");
        assert_eq!(
            pending.req.source_lang.as_deref(),
            Some("en"),
            "we write this buffer in English, not the global Polish"
        );
        assert_eq!(pending.req.target_lang, "zh");
    }

    #[test]
    fn outgoing_is_refused_when_the_buffer_has_no_language() {
        // There is nothing to detect a WRITE language from, so building a
        // request could only ever produce a wrong target.
        let mut app = app_with_buffer();
        set_buffer_langs(&mut app, None, None);
        assert!(build(&mut app).is_none());
    }

    use crate::app::translate::OutgoingTranslatePolicy as P;

    /// App with `#dupa` configured for outgoing translation.
    fn app_with_outgoing(lang: Option<&str>) -> crate::app::App {
        let mut app = app_with_buffer();
        app.state.set_active_buffer(BUF);
        app.state.translate_active = true;
        app.config.translate.enabled = true;
        app.config.translate.my_lang = "pl".to_string();
        set_buffer_langs(&mut app, lang, None);
        app
    }

    #[test]
    fn policy_translates_a_normal_line_on_an_enabled_buffer() {
        // The happy path must hold, or the refusals below could all pass by
        // refusing everything.
        let app = app_with_outgoing(Some("de"));
        assert_eq!(
            app.outgoing_translate_policy(BUF, "moje zdanie", false),
            P::Translate
        );
    }

    #[test]
    fn policy_refuses_multiline_rather_than_sending_it_untranslated() {
        let app = app_with_outgoing(Some("de"));
        let P::Refuse(reason) = app.outgoing_translate_policy(BUF, "pierwsza\ndruga", false)
        else {
            panic!("multi-line must be refused, not sent");
        };
        assert!(reason.contains("multi-line"), "got: {reason}");
    }

    #[test]
    fn policy_refuses_a_line_too_long_to_translate() {
        let app = app_with_outgoing(Some("de"));
        let long = "a".repeat(crate::irc::MESSAGE_MAX_BYTES + 1);
        let P::Refuse(reason) = app.outgoing_translate_policy(BUF, &long, false) else {
            panic!("an oversized line must be refused, not sent");
        };
        assert!(reason.contains("too long"), "got: {reason}");
    }

    #[test]
    fn policy_refuses_a_buffer_with_no_target_language() {
        // `outgoing = true` with no language is reachable by hand-editing
        // config.toml, and /translate list already reports it. A target
        // cannot be guessed, so it is a refusal, not permission to send.
        let app = app_with_outgoing(None);
        let P::Refuse(reason) = app.outgoing_translate_policy(BUF, "moje zdanie", false) else {
            panic!("a missing target language must be refused, not sent");
        };
        assert!(reason.contains("no target language"), "got: {reason}");
    }

    #[test]
    fn policy_steps_aside_for_e2e_so_the_encrypted_send_proceeds() {
        // Not a refusal: the conversation is simply never translated, and
        // the ordinary encrypted send is exactly right.
        let app = app_with_outgoing(Some("de"));
        assert_eq!(
            app.outgoing_translate_policy(BUF, "moje zdanie", true),
            P::NotApplicable
        );
    }

    #[test]
    fn policy_steps_aside_when_the_buffer_has_no_outgoing_flag() {
        let mut app = app_with_outgoing(Some("de"));
        app.config
            .translate
            .buffers
            .get_mut(BUF)
            .expect("configured")
            .outgoing = false;
        assert_eq!(
            app.outgoing_translate_policy(BUF, "moje zdanie", false),
            P::NotApplicable
        );
    }

    #[test]
    fn policy_steps_aside_when_the_master_switch_is_off() {
        let mut app = app_with_outgoing(Some("de"));
        app.state.translate_active = false;
        assert_eq!(
            app.outgoing_translate_policy(BUF, "moje zdanie", false),
            P::NotApplicable
        );
    }

    #[test]
    fn a_refusal_restores_the_text_and_names_the_reason() {
        let mut app = app_with_outgoing(Some("de"));
        let sent = app.refuse_untranslatable_send("moje zdanie", "the translation queue is full");
        assert!(!sent, "a refusal never reports a send");
        assert_eq!(app.input.value, "moje zdanie", "the text comes back");
        let last = app.state.buffers[BUF]
            .messages
            .back()
            .expect("an error row")
            .text
            .clone();
        assert!(last.contains("queue is full"), "got: {last}");
    }

    #[test]
    fn the_deliver_channel_stays_open_when_translation_is_disabled() {
        // The DEFAULT configuration. With no worker holding a sender, the
        // receiver would see a closed channel and `recv()` would return
        // immediately forever, spinning the main select! at 100% CPU.
        let mut app = test_app();
        assert!(
            app.translate_backend.is_none(),
            "precondition: the feature is off"
        );
        assert!(
            app.translate_deliver_rx.try_recv().is_err_and(|e| matches!(
                e,
                tokio::sync::mpsc::error::TryRecvError::Empty
            )),
            "the channel must be EMPTY, never Disconnected — a disconnected \
             receiver makes the event loop spin"
        );
    }

    #[test]
    fn a_refusal_puts_the_text_in_the_error_row_too() {
        // Restoring alone is not enough: a composer the user has started
        // typing into keeps what is there, and then the refused message
        // would exist nowhere at all.
        let mut app = app_with_outgoing(Some("de"));
        app.input.value = "something else".to_string();
        app.refuse_untranslatable_send("moje zdanie", "the translation queue is full");
        let last = app.state.buffers[BUF].messages.back().unwrap();
        assert!(
            last.text.contains("moje zdanie"),
            "the text is recoverable from the error row: {}",
            last.text
        );
        assert_eq!(app.input.value, "something else", "and typing is untouched");
    }

    #[test]
    fn a_refused_msg_comes_back_re_addressed_not_as_bare_text() {
        // `/msg bob secret` deliberately leaves the CURRENT channel active.
        // Returning just `secret` and letting the user press Enter would
        // publish private content to that channel.
        let app = app_with_outgoing(Some("de"));
        assert_eq!(
            app.retry_form_for("bob", "secret", false),
            "/msg bob secret",
            "the retry keeps its destination"
        );
    }

    #[test]
    fn a_refused_action_comes_back_as_an_action() {
        let app = app_with_outgoing(Some("de"));
        assert_eq!(
            app.retry_form_for("#dupa", "waves hello", true),
            "/me waves hello",
            "otherwise it loses its action semantics"
        );
    }

    #[test]
    fn text_for_the_active_buffer_comes_back_unadorned() {
        let mut app = app_with_outgoing(Some("de"));
        app.state.set_active_buffer(BUF);
        assert_eq!(app.retry_form_for("#dupa", "moje zdanie", false), "moje zdanie");
    }

    #[test]
    fn a_script_refusal_never_touches_the_users_composer() {
        // Nobody typed it, so there is nowhere to put it back — and putting
        // a script's payload in the composer hands the user text to send by
        // accident.
        let mut app = app_with_outgoing(Some("de"));
        app.submit_origin = SubmitOrigin::Script;
        app.refuse_untranslatable_send("payload from a script", "the translation queue is full");
        assert!(app.input.value.is_empty());
    }

    #[test]
    fn a_web_submission_is_returned_to_that_browser_not_the_terminal() {
        // The browser cleared its composer on submit, so restoring into the
        // TUI input loses the text for its author AND drops it somewhere
        // nobody is looking.
        let mut app = app_with_outgoing(Some("de"));
        app.submit_origin = SubmitOrigin::Web("alice-session".to_string());
        let mut rx = app.web_broadcaster.subscribe();

        app.refuse_untranslatable_send("moje zdanie", "the translation queue is full");

        assert!(
            app.input.value.is_empty(),
            "the terminal input must not be touched"
        );
        let (text, session) = match rx.try_recv().expect("an event was broadcast") {
            crate::web::protocol::WebEvent::RestoreInput { text, session_id } => (text, session_id),
            other => panic!("expected RestoreInput, got {other:?}"),
        };
        assert_eq!(text, "moje zdanie");
        assert_eq!(session.as_deref(), Some("alice-session"));
    }

    /// An app on an `echo-message` server, with `#dupa` set to show the
    /// original on outgoing lines.
    fn app_with_echo_message() -> crate::app::App {
        let mut app = app_with_dying_handle(usize::MAX);
        let mut conn = crate::app::input::submit_typing_tests::make_connection();
        conn.id = "test".to_string();
        conn.nick = "me".to_string();
        conn.enabled_caps.insert("echo-message".to_string());
        app.state.add_connection(conn);
        app.config.translate.show_original_out = true;
        app
    }

    /// The server reflecting one of our own PRIVMSGs back at us, carrying
    /// the `@msgid` a real one would.
    fn reflect(app: &mut crate::app::App, text: &str, msgid: &str) {
        let prefix = irc::proto::Prefix::Nickname(
            "me".to_string(),
            "me".to_string(),
            "example.org".to_string(),
        );
        crate::irc::events::handle_irc_message(
            &mut app.state,
            "test",
            &irc::proto::Message {
                tags: Some(vec![irc::proto::message::Tag(
                    "msgid".to_string(),
                    Some(msgid.to_string()),
                )]),
                prefix: Some(prefix),
                command: irc::proto::Command::PRIVMSG("#dupa".to_string(), text.to_string()),
            },
        );
    }

    /// The rows in `#dupa`, as `(text, msgid)`.
    fn rows_with_ids(app: &crate::app::App) -> Vec<(String, Option<String>)> {
        app.state.buffers[BUF]
            .messages
            .iter()
            .map(|m| {
                (
                    m.text.clone(),
                    m.tags.as_ref().and_then(|t| t.get("msgid")).cloned(),
                )
            })
            .collect()
    }

    /// Send one translated line on an echo-message server.
    fn send_translated(app: &mut crate::app::App, id: u64, original: &str, translated: &str) {
        let mut out = outgoing(
            original,
            TranslateOutcome::Translated {
                id,
                text: translated.to_string(),
            },
            true, // show_original
        );
        out.echo_id = id;
        app.apply_translate_deliver(TranslateDeliver::Outgoing(Box::new(out)));
    }

    #[test]
    fn showing_the_original_outgoing_survives_echo_message() {
        // The wire carries only the translation, so the server's reflection
        // cannot render the ` [original]` suffix the user configured. It is
        // DECORATED rather than replaced by a local row: the reflection is
        // the copy that carries the server's msgid and timestamp, and a
        // locally-authored row would match nothing on a later replay.
        let mut app = app_with_echo_message();
        send_translated(&mut app, 1, "moje zdanie", "mein satz");
        assert!(
            shown(&app).is_empty(),
            "no local row — the server owns this one: {:?}",
            shown(&app)
        );

        reflect(&mut app, "mein satz", "server-M1");

        assert_eq!(
            rows_with_ids(&app),
            vec![(
                "mein satz [moje zdanie]".to_string(),
                Some("server-M1".to_string())
            )],
            "one row, decorated, still carrying the server's msgid"
        );
        let row = app.state.buffers[BUF].messages.back().expect("the row");
        let origin = row
            .wire_origin
            .as_ref()
            .expect("a decorated row records what the wire carried");
        assert_eq!(
            origin.text, "mein satz",
            "identity is the wire text, so a replay of this line dedups"
        );
        assert_eq!(origin.suffix_at, Some("mein satz".len()));
    }

    #[test]
    fn echo_message_still_owns_the_echo_when_the_original_is_not_shown() {
        // Nothing to add, so nothing is filed and the reflection is shown
        // exactly as it always was.
        let mut app = app_with_echo_message();
        app.config.translate.show_original_out = false;
        let mut out = outgoing(
            "moje zdanie",
            TranslateOutcome::Translated {
                id: 1,
                text: "mein satz".to_string(),
            },
            false, // show_original
        );
        out.echo_id = 1;
        app.apply_translate_deliver(TranslateDeliver::Outgoing(Box::new(out)));
        assert!(shown(&app).is_empty(), "no local row: {:?}", shown(&app));

        reflect(&mut app, "mein satz", "server-M1");

        assert_eq!(shown(&app), vec!["mein satz".to_string()]);
        assert!(
            app.state.buffers[BUF]
                .messages
                .back()
                .is_some_and(|m| m.wire_origin.is_none()),
            "an undecorated reflection IS its own wire text"
        );
    }

    #[test]
    fn a_reflection_that_was_never_filed_is_still_shown() {
        // Fail-open. A miss — a netsplit between send and echo, a server
        // that rewrites what it reflects, someone else's line — renders
        // plain. The mechanism can lose a suffix, never a message.
        let mut app = app_with_echo_message();
        send_translated(&mut app, 1, "moje zdanie", "mein satz");

        reflect(&mut app, "etwas ganz anderes", "server-M9");

        assert_eq!(
            shown(&app),
            vec!["etwas ganz anderes".to_string()],
            "an unmatched line is displayed, undecorated"
        );
    }

    #[test]
    fn each_reflection_consumes_one_record_so_the_second_send_still_shows() {
        // Sending the same text twice files two decorations. If a reflection
        // peeked instead of consuming, the second would be decorated from
        // the first record and the third — a stranger's identical line —
        // would be decorated too.
        let mut app = app_with_echo_message();
        send_translated(&mut app, 1, "moje zdanie", "mein satz");
        send_translated(&mut app, 2, "moje zdanie", "mein satz");

        reflect(&mut app, "mein satz", "server-M1");
        reflect(&mut app, "mein satz", "server-M2");
        reflect(&mut app, "mein satz", "server-M3");

        assert_eq!(
            shown(&app),
            vec![
                "mein satz [moje zdanie]".to_string(),
                "mein satz [moje zdanie]".to_string(),
                // Records exhausted: this one is not ours to decorate.
                "mein satz".to_string(),
            ],
            "each record is spent exactly once"
        );
    }

    #[test]
    fn a_script_send_that_wants_no_echo_still_gets_none_with_the_original_on() {
        // `show_original_out` must not hand an echo to a caller that
        // explicitly asked for none — that is the double-render the
        // `None` plan exists to prevent.
        let mut app = app_with_echo_message();
        app.apply_translate_deliver(TranslateDeliver::Outgoing(Box::new(outgoing_with_echo(
            "moje zdanie",
            TranslateOutcome::Translated {
                id: 1,
                text: "mein satz".to_string(),
            },
            OutgoingEchoPlan::None,
        ))));
        assert!(
            shown(&app).is_empty(),
            "no echo was wanted and none was written: {:?}",
            shown(&app)
        );
    }

    #[test]
    fn a_refused_slash_command_from_the_web_goes_back_to_that_browser() {
        // The web composer dispatches `/msg` as `RunCommand`, not
        // `SendMessage` — but both end in the same `handle_submit` and the
        // same outgoing translation gate. Without the origin scope on this
        // arm too, the refusal restores the text into the TERMINAL's input
        // line: lost for its author, dropped where nobody is looking.
        let mut app = app_with_outgoing(None); // no target language → refuse
        app.irc_handles.insert(
            "test".to_string(),
            crate::irc::handle::IrcHandle::new(
                "test".to_string(),
                crate::irc::handle::IrcSender::capturing(0),
                None,
                None,
            ),
        );
        let mut rx = app.web_broadcaster.subscribe();

        app.handle_web_command(
            crate::web::protocol::WebCommand::RunCommand {
                buffer_id: BUF.to_string(),
                text: "/msg #dupa moje zdanie".to_string(),
            },
            "alice-session",
        );

        assert!(
            app.input.value.is_empty(),
            "the terminal input belongs to whoever is sitting at it: {:?}",
            app.input.value
        );
        let restored: Vec<(String, Option<String>)> = std::iter::from_fn(|| rx.try_recv().ok())
            .filter_map(|ev| match ev {
                crate::web::protocol::WebEvent::RestoreInput { text, session_id } => {
                    Some((text, session_id))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            restored,
            // The bare body, because `/msg`ing the buffer you are already in
            // retries correctly as plain text — see `retry_form_for`.
            vec![(
                "moje zdanie".to_string(),
                Some("alice-session".to_string())
            )],
            "the retry form goes back to the browser that submitted it"
        );
        assert_eq!(
            app.submit_origin,
            SubmitOrigin::Tui,
            "and the scope is closed again afterwards, so a later terminal \
             send is not attributed to this browser"
        );
    }

    #[test]
    fn a_refusal_does_not_clobber_something_typed_since() {
        let mut app = app_with_outgoing(Some("de"));
        app.input.value = "something else".to_string();
        app.refuse_untranslatable_send("moje zdanie", "the translation queue is full");
        assert_eq!(app.input.value, "something else");
    }

    #[test]
    fn raising_max_in_flight_takes_effect_without_a_restart() {
        // It is offered as an ordinary /set option, so accepting it and
        // silently ignoring it is worse than not offering it at all.
        let mut app = app_with_buffer();
        app.translate_in_flight = Some(Arc::new(Semaphore::new(4)));
        app.translate_in_flight_applied = 4;
        app.config.translate.max_in_flight = 9;
        app.sync_translate_from_config();
        assert_eq!(
            app.translate_in_flight.as_ref().unwrap().available_permits(),
            9
        );
        assert_eq!(app.translate_in_flight_applied, 9);
    }

    #[test]
    fn lowering_max_in_flight_takes_effect_without_a_restart() {
        let mut app = app_with_buffer();
        app.translate_in_flight = Some(Arc::new(Semaphore::new(8)));
        app.translate_in_flight_applied = 8;
        app.config.translate.max_in_flight = 2;
        app.sync_translate_from_config();
        assert_eq!(
            app.translate_in_flight.as_ref().unwrap().available_permits(),
            2
        );
    }

    #[test]
    fn a_reduction_blocked_by_in_flight_work_is_carried_and_settled_later() {
        // `forget_permits` can only take what is available. Recording the
        // requested value anyway would let the old concurrency creep back as
        // in-flight work returns its permits.
        let mut app = app_with_buffer();
        let sem = Arc::new(Semaphore::new(8));
        // Six permits are checked out, so only two can be forgotten now.
        let held = sem.clone().try_acquire_many_owned(6).expect("6 available");
        app.translate_in_flight = Some(Arc::clone(&sem));
        app.translate_in_flight_applied = 8;
        app.config.translate.max_in_flight = 2;
        app.sync_translate_from_config();

        assert_eq!(sem.available_permits(), 0, "both spare permits were taken");
        assert_eq!(app.translate_in_flight_applied, 6, "only what really happened");
        assert_eq!(app.translate_in_flight_debt, 4, "the rest is owed");

        // The in-flight work finishes and its permits come back.
        drop(held);
        app.settle_translate_concurrency_debt();
        assert_eq!(app.translate_in_flight_applied, 2, "the target is reached");
        assert_eq!(app.translate_in_flight_debt, 0);
        assert_eq!(sem.available_permits(), 2);
    }

    #[test]
    fn settling_is_a_no_op_with_no_debt() {
        let mut app = app_with_buffer();
        let sem = Arc::new(Semaphore::new(4));
        app.translate_in_flight = Some(Arc::clone(&sem));
        app.translate_in_flight_applied = 4;
        app.settle_translate_concurrency_debt();
        assert_eq!(sem.available_permits(), 4);
    }

    #[test]
    fn retuning_to_the_same_value_is_a_no_op() {
        let mut app = app_with_buffer();
        app.translate_in_flight = Some(Arc::new(Semaphore::new(4)));
        app.translate_in_flight_applied = 4;
        app.config.translate.max_in_flight = 4;
        app.sync_translate_from_config();
        assert_eq!(
            app.translate_in_flight.as_ref().unwrap().available_permits(),
            4
        );
    }

    #[test]
    fn the_by_target_gate_takes_over_a_translatable_send() {
        let mut app = app_with_outgoing(Some("de"));
        let (tx, mut rx) = mpsc::channel(4);
        app.translate_outgoing_tx = tx;
        let handled = app.gate_by_target_translation(
            "test",
            "#dupa",
            "moje zdanie",
            OutgoingEchoPlan::BufferInput,
        );
        assert_eq!(handled, Some(false), "taken over, nothing on the wire yet");
        assert_eq!(rx.try_recv().expect("dispatched").req.text, "moje zdanie");
    }

    #[test]
    fn the_by_target_gate_unwraps_an_action() {
        let mut app = app_with_outgoing(Some("de"));
        let (tx, mut rx) = mpsc::channel(4);
        app.translate_outgoing_tx = tx;
        let handled =
            app.gate_by_target_translation(
                "test",
                "#dupa",
                "\x01ACTION waves hello\x01",
                OutgoingEchoPlan::BufferInput,
            );
        assert_eq!(handled, Some(false));
        let pending = rx.try_recv().expect("dispatched");
        assert_eq!(pending.req.text, "waves hello");
        assert!(pending.is_action);
    }

    #[test]
    fn the_by_target_gate_steps_aside_for_an_untranslated_buffer() {
        let mut app = app_with_buffer();
        assert_eq!(
            app.gate_by_target_translation(
                "test",
                "#dupa",
                "hello there",
                OutgoingEchoPlan::BufferInput
            ),
            None,
            "the ordinary send must proceed"
        );
    }

    #[test]
    fn the_by_target_gate_ignores_non_action_ctcp() {
        // Protocol, not prose — a translated VERSION reply is nonsense.
        let mut app = app_with_outgoing(Some("de"));
        assert_eq!(
            app.gate_by_target_translation(
                "test",
                "#dupa",
                "\x01VERSION\x01",
                OutgoingEchoPlan::BufferInput
            ),
            None
        );
    }

    #[test]
    fn an_action_is_dispatched_as_its_inner_text() {
        // `/me` must reach the broker as prose, not as CTCP framing.
        let mut app = app_with_outgoing(Some("de"));
        let (tx, mut rx) = mpsc::channel(4);
        app.translate_outgoing_tx = tx;

        let sent = app.dispatch_by_target_translation(
            &OutgoingRequest {
                conn_id: "test",
                buffer_id: BUF,
                buffer_name: "#dupa",
                buffer_type: &BufferType::Channel,
                nick: "me",
                text: "waves hello",
                is_action: true,
                echo: OutgoingEchoPlan::BufferInput,
            },
            "/me waves hello",
        );
        assert!(!sent, "nothing is on the wire yet");
        let pending = rx.try_recv().expect("dispatched");
        assert_eq!(pending.req.text, "waves hello", "no CTCP framing in the request");
        assert!(pending.is_action, "the shape is carried for re-wrapping");
        assert_eq!(pending.req.target_lang, "de");
    }

    #[test]
    fn a_by_target_send_refuses_when_it_cannot_translate() {
        // A by-target send is not a lesser send: publishing it untranslated
        // puts the same wrong-language text on the same channel.
        let mut app = app_with_outgoing(Some("de"));
        let (tx, rx) = mpsc::channel(1);
        app.translate_outgoing_tx = tx;
        drop(rx);

        let sent =
            app.dispatch_by_target_translation(
                &OutgoingRequest {
                    conn_id: "test",
                    buffer_id: BUF,
                    buffer_name: "#dupa",
                    buffer_type: &BufferType::Channel,
                    nick: "me",
                    text: "moje zdanie",
                    is_action: false,
                    echo: OutgoingEchoPlan::BufferInput,
                },
                "moje zdanie",
            );
        assert!(!sent);
        assert_eq!(app.input.value, "moje zdanie", "the text comes back");
    }

    #[test]
    fn build_outgoing_translate_refuses_a_possibly_e2e_target() {
        // The caller already gates, but this is where cleartext becomes a
        // payload bound for a third party, so the refusal must be a property
        // of the function rather than of its call sites.
        let mut app = app_with_buffer();
        let db = crate::storage::db::open_database(false).unwrap();
        let keyring =
            crate::e2e::keyring::Keyring::new(std::sync::Arc::new(std::sync::Mutex::new(db)));
        let mgr = crate::e2e::manager::E2eManager::load_or_init(keyring).unwrap();
        // Derive the network exactly as the production path does, so the
        // config we install is the one the gate will look up.
        let network = app
            .state
            .connections
            .get("test")
            .map_or_else(String::new, |c| c.label.clone());
        mgr.keyring()
            .set_channel_config(&crate::e2e::keyring::ChannelConfig {
                channel: crate::e2e::scoped_context(&network, "#dupa"),
                enabled: true,
                mode: crate::e2e::keyring::ChannelMode::Normal,
            })
            .unwrap();
        app.state.e2e_manager = Some(std::sync::Arc::new(mgr));

        let pending = app.build_outgoing_translate(&OutgoingRequest {
            conn_id: "test",
            buffer_id: BUF,
            buffer_name: "#dupa",
            buffer_type: &BufferType::Channel,
            nick: "me",
            text: "moje zdanie",
            is_action: false,
            echo: OutgoingEchoPlan::BufferInput,
        });
        assert!(
            pending.is_none(),
            "no payload may be built for a possibly-E2E target"
        );
    }

    #[test]
    fn flushing_a_buffer_releases_its_pending_lines_in_order() {
        let mut app = app_with_queue(3);
        app.flush_translate_queue(BUF);
        assert_eq!(
            shown(&app),
            vec![
                "line 1 [untranslated: timeout]".to_string(),
                "line 2 [untranslated: timeout]".to_string(),
                "line 3 [untranslated: timeout]".to_string(),
            ],
            "pending lines are released, never dropped"
        );
        assert!(!app.state.translate_queues.contains_key(BUF));
    }

    #[test]
    fn closing_a_buffer_drops_its_queue() {
        // The buffer is gone, so releasing into it would be refused anyway
        // and logging under it would orphan the rows. What must not happen
        // is the queue outliving the buffer.
        let mut app = app_with_queue(3);
        app.state.remove_buffer(BUF);
        assert!(
            !app.state.translate_queues.contains_key(BUF),
            "the queue must not outlive its buffer"
        );
    }

    #[test]
    fn flushing_all_queues_covers_every_buffer() {
        let mut app = app_with_queue(1);
        let other = "test/#other";
        app.state
            .add_buffer(Buffer::for_test("test", BufferType::Channel, "#other"));
        let mut queue = TranslateQueue::new();
        queue.push_pending(9, "other line".to_string(), payload(9, "other line"));
        app.state.translate_queues.insert(other.to_string(), queue);

        app.flush_all_translate_queues();
        assert!(app.state.translate_queues.is_empty());
        assert_eq!(app.state.buffers[other].messages.len(), 1);
    }
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
                    submitted_at: std::time::Instant::now(),
                })
                .await
                .expect("worker alive");
        }
        let mut seen = Vec::new();
        for _ in 0..3 {
            match rt.deliver_rx.recv().await {
                Some(TranslateDeliver::Incoming { outcome, buffer_id, .. }) => {
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
                    submitted_at: std::time::Instant::now(),
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

    fn outgoing_for(conn: &str, id: u64, text: &str) -> PendingOutgoingTranslate {
        PendingOutgoingTranslate {
            conn_id: conn.to_string(),
            buffer_id: format!("{conn}/#chan"),
            buffer_name: "#chan".to_string(),
            buffer_type: BufferType::Channel,
            original_text: text.to_string(),
            req: req(id, text),
            nick: "me".to_string(),
            own_mode: None,
            peer_handle: None,
            show_original: false,
            is_action: false,
            echo: OutgoingEchoPlan::BufferInput,
            echo_id: 1,
            origin: SubmitOrigin::Tui,
            retry_text: text.to_string(),
            retry_body: text.to_string(),
            conn_generation: None,
            submitted_at: std::time::Instant::now(),
        }
    }

    #[tokio::test]
    async fn a_slow_connection_does_not_block_another_connections_send() {
        // The point of a per-connection FIFO: one hung request on network A
        // must not hold up translated sends on network B, which have
        // nothing to do with it.
        let mut rt = TranslateRuntime::with_backend(
            Some(Arc::new(StubBackend::with_jitter(&[400, 0]))),
            &cfg(4),
        );
        // First into lane "slow" takes the 400 ms slot; second into lane
        // "fast" takes the 0 ms one.
        rt.outgoing_tx
            .send(outgoing_for("slow", 1, "hello world"))
            .await
            .expect("worker alive");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        rt.outgoing_tx
            .send(outgoing_for("fast", 2, "hello world"))
            .await
            .expect("worker alive");

        let first = tokio::time::timeout(
            std::time::Duration::from_millis(250),
            rt.deliver_rx.recv(),
        )
        .await
        .expect("the fast connection must not wait on the slow one")
        .expect("an outcome");
        let TranslateDeliver::Outgoing(d) = first else {
            panic!("expected an outgoing deliver");
        };
        assert_eq!(d.conn_id, "fast", "the unrelated connection finished first");
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
                    is_action: false,
                    echo: OutgoingEchoPlan::BufferInput,
                    echo_id: 1,
                    origin: SubmitOrigin::Tui,
                    retry_text: "hello world".to_string(),
                    retry_body: "hello world".to_string(),
                    conn_generation: None,
                    submitted_at: std::time::Instant::now(),
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
    async fn outgoing_lanes_obey_the_shared_concurrency_cap() {
        // `translate.max_in_flight` is documented as the PROVIDER's
        // concurrency cap. A per-connection lane that skipped the limiter
        // made the real ceiling `max_in_flight + one per network`, which is
        // over the limit exactly on the setups that have several open.
        //
        // One permit, two connections, 60 ms each: sharing the limiter means
        // they run one after the other, so the pair cannot finish in one
        // request's time.
        let mut rt = TranslateRuntime::with_backend(
            Some(Arc::new(StubBackend::with_jitter(&[60]))),
            &cfg(1),
        );
        let started = tokio::time::Instant::now();
        for (i, conn) in ["a", "b"].iter().enumerate() {
            rt.outgoing_tx
                .send(outgoing_for(conn, i as u64 + 1, "hello world"))
                .await
                .expect("worker alive");
        }
        for _ in 0..2 {
            rt.deliver_rx.recv().await.expect("outcome");
        }
        let elapsed = started.elapsed();
        assert!(
            elapsed >= std::time::Duration::from_millis(100),
            "two lanes finished in {elapsed:?} with one permit — they are \
             not sharing the limiter"
        );
    }

    #[tokio::test]
    async fn separate_connections_still_run_concurrently_when_permits_allow() {
        // The cap must not turn the per-connection lanes back into one
        // serial queue: with permits to spare, two networks translate at the
        // same time, which is the whole reason the lanes are per connection.
        let mut rt = TranslateRuntime::with_backend(
            Some(Arc::new(StubBackend::with_jitter(&[60]))),
            &cfg(4),
        );
        let started = tokio::time::Instant::now();
        for (i, conn) in ["a", "b"].iter().enumerate() {
            rt.outgoing_tx
                .send(outgoing_for(conn, i as u64 + 1, "hello world"))
                .await
                .expect("worker alive");
        }
        for _ in 0..2 {
            rt.deliver_rx.recv().await.expect("outcome");
        }
        let elapsed = started.elapsed();
        assert!(
            elapsed < std::time::Duration::from_millis(110),
            "two networks took {elapsed:?} — one is blocking the other"
        );
    }

    #[tokio::test]
    async fn a_request_that_waited_out_its_budget_is_not_sent_to_the_provider() {
        // The budget runs from SUBMISSION. With one permit and a backend
        // slower than the timeout, the second request's deadline is already
        // gone by the time a permit frees up — calling the provider then buys
        // an answer nobody can use, and (outgoing) would put the message on
        // the channel long after the user gave up on it.
        let mut cfg = cfg(1);
        cfg.timeout_ms = 40;
        let mut rt =
            TranslateRuntime::with_backend(Some(Arc::new(StubBackend::with_jitter(&[80]))), &cfg);
        for id in 1..=2u64 {
            rt.incoming_tx
                .send(PendingTranslate {
                    buffer_id: "b".to_string(),
                    req: req(id, "hello world"),
                    submitted_at: std::time::Instant::now(),
                })
                .await
                .expect("worker alive");
        }
        let started = tokio::time::Instant::now();
        let mut outcomes = Vec::new();
        for _ in 0..2 {
            match rt.deliver_rx.recv().await {
                Some(TranslateDeliver::Incoming { outcome, .. }) => outcomes.push(outcome),
                other => panic!("expected an incoming deliver, got {other:?}"),
            }
        }
        let elapsed = started.elapsed();
        assert!(
            outcomes.iter().all(|o| matches!(
                o,
                TranslateOutcome::Untranslated {
                    reason: crate::translate::UntranslatedReason::Timeout,
                    ..
                }
            )),
            "both must time out: {outcomes:?}"
        );
        // The point is WHERE the second one timed out. A budget that starts
        // when the provider is called gives it a fresh 40 ms of its own, so
        // the pair takes ~80 ms; a budget that runs from submission finds its
        // deadline already gone and returns without calling out at all.
        assert!(
            elapsed < std::time::Duration::from_millis(70),
            "the pair took {elapsed:?} — the second request was still sent to \
             the provider after its deadline had passed"
        );
    }

    #[tokio::test]
    async fn a_request_inside_its_budget_still_reaches_the_provider() {
        // The deadline check must not refuse the ordinary case, or it would
        // pass by refusing everything.
        let mut cfg = cfg(1);
        cfg.timeout_ms = 500;
        let mut rt =
            TranslateRuntime::with_backend(Some(Arc::new(StubBackend::with_jitter(&[0]))), &cfg);
        rt.incoming_tx
            .send(PendingTranslate {
                buffer_id: "b".to_string(),
                req: req(1, "hello world"),
                submitted_at: std::time::Instant::now(),
            })
            .await
            .expect("worker alive");
        match rt.deliver_rx.recv().await {
            Some(TranslateDeliver::Incoming { outcome, .. }) => assert!(
                matches!(outcome, TranslateOutcome::Translated { .. }),
                "got {outcome:?}"
            ),
            other => panic!("expected an incoming deliver, got {other:?}"),
        }
    }

    #[test]
    fn a_multi_line_answer_is_refused_but_a_trailing_newline_is_trimmed() {
        use crate::translate::UntranslatedReason;
        // Embedded breaks are a broken response whatever they say — the
        // contract is one line per request, and guessing which line the user
        // meant is not a thing to do with text about to be published under
        // their nick. A TRAILING newline is a correct answer with a stray
        // byte on it.
        let refused = super::single_line_or_refuse(TranslateOutcome::Translated {
            id: 1,
            text: "hello\nJOIN #evil".to_string(),
        });
        assert!(
            matches!(
                refused,
                TranslateOutcome::Untranslated {
                    reason: UntranslatedReason::Error(_),
                    ..
                }
            ),
            "got {refused:?}"
        );

        let trimmed = super::single_line_or_refuse(TranslateOutcome::Translated {
            id: 2,
            text: "mein satz\r\n".to_string(),
        });
        assert!(
            matches!(&trimmed, TranslateOutcome::Translated { text, .. } if text == "mein satz"),
            "got {trimmed:?}"
        );

        let clean = super::single_line_or_refuse(TranslateOutcome::Translated {
            id: 3,
            text: "mein satz".to_string(),
        });
        assert!(
            matches!(&clean, TranslateOutcome::Translated { text, .. } if text == "mein satz"),
            "an ordinary answer is untouched: {clean:?}"
        );
    }

    /// A backend that answers with whatever it is handed at construction —
    /// standing in for a compromised or simply broken provider.
    #[derive(Debug)]
    struct EchoingBackend(String);

    impl crate::translate::backend::TranslateBackend for EchoingBackend {
        fn translate(
            &self,
            req: crate::translate::TranslateRequest,
        ) -> futures::future::BoxFuture<'_, TranslateOutcome> {
            let text = self.0.clone();
            Box::pin(async move { TranslateOutcome::Translated { id: req.id, text } })
        }
    }

    #[tokio::test]
    async fn a_multi_line_answer_never_leaves_the_worker_as_a_translation() {
        // The guard has to be wired into the worker, not merely to exist:
        // this is the boundary where a third party's bytes enter a process
        // that will put them on an IRC socket.
        let mut rt = TranslateRuntime::with_backend(
            Some(Arc::new(EchoingBackend("hello\nJOIN #evil".to_string()))),
            &cfg(4),
        );
        rt.incoming_tx
            .send(PendingTranslate {
                buffer_id: "b".to_string(),
                req: req(1, "hello world"),
                submitted_at: std::time::Instant::now(),
            })
            .await
            .expect("worker alive");
        match rt.deliver_rx.recv().await {
            Some(TranslateDeliver::Incoming { outcome, .. }) => assert!(
                matches!(
                    outcome,
                    TranslateOutcome::Untranslated {
                        reason: crate::translate::UntranslatedReason::Error(_),
                        ..
                    }
                ),
                "a multi-line answer must not emerge as a Translated outcome: {outcome:?}"
            ),
            other => panic!("expected an incoming deliver, got {other:?}"),
        }
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
                    submitted_at: std::time::Instant::now(),
                })
                .await
                .expect("drain keeps the channel open");
        }
    }
}

#[cfg(test)]
mod ordering_integration {
    use super::*;
    use crate::config::{TranslateBufferConfig, TranslateConfig};
    use crate::state::buffer::{ActivityLevel, Buffer, BufferType, Message, MessageType};
    use crate::translate::backend::StubBackend;

    const BUF: &str = "test/#dupa";

    fn message(id: u64, text: &str) -> Message {
        Message {
            id,
            timestamp: chrono::Utc::now(),
            message_type: MessageType::Message,
            nick: Some("alice".to_string()),
            nick_mode: None,
            text: text.to_string(),
            highlight: false,
            event_key: None,
            event_params: None,
            log_msg_id: None,
            log_ref_id: None,
            tags: None,
            wire_origin: None,
        }
    }

    /// The headline guarantee, end to end: a burst whose translations come
    /// back deliberately out of order must still render in arrival order.
    ///
    /// The jitter sequence is what gives the test its teeth — with a uniform
    /// delay the burst would finish in order by accident and prove nothing.
    #[tokio::test]
    async fn a_burst_with_out_of_order_returns_preserves_arrival_order() {
        const LINES: u64 = 20;

        let cfg = TranslateConfig {
            enabled: true,
            max_in_flight: 8,
            show_original_in: false,
            ..Default::default()
        };
        // Descending delays: the LAST line dispatched finishes first.
        let delays: Vec<u64> = (0..LINES).map(|i| (LINES - i) * 4).collect();
        let mut rt = TranslateRuntime::with_backend(
            Some(Arc::new(StubBackend::with_jitter(&delays))),
            &cfg,
        );

        let mut state = crate::state::AppState::new();
        state.add_buffer(Buffer::for_test("test", BufferType::Channel, "#dupa"));
        state.translate_incoming_tx = Some(rt.incoming_tx.clone());
        state.translate_active = true;
        state.translate_show_original_in = false;
        state.translate_buffers.insert(
            BUF.to_string(),
            TranslateBufferConfig {
                incoming: true,
                outgoing: false,
                lang: None,
                my_lang: None,
            },
        );

        // Each line's words are reversed by the stub, so "line N of burst"
        // comes back as "burst of N line" — position N is still identifiable
        // and we can prove the translation actually ran.
        for i in 1..=LINES {
            let id = state.next_message_id();
            let msg = message(id, &format!("line {i} of burst"));
            state.add_message_with_activity(BUF, msg, ActivityLevel::Activity);
        }
        assert_eq!(
            state.buffers[BUF].messages.len(),
            0,
            "every line is queued, nothing shown yet"
        );

        // Drain outcomes exactly as the select! arm does.
        let mut arrival_order = Vec::new();
        for _ in 0..LINES {
            let deliver = rt.deliver_rx.recv().await.expect("an outcome per line");
            let TranslateDeliver::Incoming { buffer_id, outcome, .. } = deliver else {
                panic!("expected an incoming outcome");
            };
            arrival_order.push(outcome.id());
            let resolution = match outcome {
                TranslateOutcome::Translated { id, text } => (id, Ok(text)),
                TranslateOutcome::Untranslated { id, reason } => (id, Err(reason)),
            };
            let ready = {
                let queue = state
                    .translate_queues
                    .get_mut(&buffer_id)
                    .expect("the queue exists");
                queue.resolve(resolution.0, resolution.1);
                queue.drain_ready()
            };
            for entry in ready {
                state.add_message_with_activity_unshrunk(&buffer_id, entry.message, entry.activity);
            }
        }

        // The test only means something if the outcomes really did come
        // back out of order. Assert that, so a future change to the stub's
        // timing cannot quietly turn this into a test of nothing.
        assert!(
            arrival_order.windows(2).any(|w| w[0] > w[1]),
            "outcomes arrived in id order ({arrival_order:?}) — the jitter is \
             not producing reordering, so this test proves nothing"
        );

        let shown: Vec<String> = state.buffers[BUF]
            .messages
            .iter()
            .map(|m| m.text.clone())
            .collect();
        assert_eq!(shown.len() as u64, LINES, "every line surfaced");
        for (idx, line) in shown.iter().enumerate() {
            let expected = format!("burst of {} line", idx + 1);
            assert_eq!(
                line, &expected,
                "position {idx} holds the wrong line — ordering broke"
            );
        }
        assert!(
            state.translate_queues[BUF].is_empty(),
            "the queue fully drained"
        );
    }
}
