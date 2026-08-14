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
use crate::translate::ai::AiBackend;
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
    let budget = outgoing_body_budget(is_action);
    if body.len() <= budget {
        return vec![format!("\x01ACTION {body}\x01")];
    }
    crate::irc::split_irc_message(body, budget)
        .into_iter()
        .map(|chunk| format!("\x01ACTION {chunk}\x01"))
        .collect()
}

/// The byte budget for the PROSE of one wire line.
///
/// An action's CTCP framing rides the same wire line as its body, so its
/// prose budget is smaller by exactly the wrapper. ONE function for the send
/// path and the local-echo chunker, because the two disagreeing is a defect
/// with no visible symptom: the echo's rows have to break where the wire
/// lines broke, or their `WireOrigin` texts stop naming anything a
/// CHATHISTORY replay carries and the author's history stops matching what
/// peers received.
const fn outgoing_body_budget(is_action: bool) -> usize {
    if is_action {
        crate::irc::MESSAGE_MAX_BYTES.saturating_sub(ACTION_WRAPPER_BYTES)
    } else {
        crate::irc::MESSAGE_MAX_BYTES
    }
}

/// Bytes `\x01ACTION ` + `\x01` adds around an action's text.
const ACTION_WRAPPER_BYTES: usize = "\x01ACTION \x01".len();

/// Whether every Repartee client would read this text as RPE2E PROTOCOL
/// rather than prose.
///
/// Matched exactly as the receive paths match it — bare prefixes, no `\x01`
/// involved: `+RPE2E01` opens the ciphertext wire format and `RPEE2E` a
/// handshake, and both are recognised on ANY buffer, E2E-enabled or not,
/// because that is how a peer's first KEYREQ can arrive at all. The
/// E2E–translate exclusion keys on the CONVERSATION and cannot help here:
/// text shaped like this does its damage on conversations where E2E was
/// never enabled.
fn reads_as_rpe2e_protocol(text: &str) -> bool {
    text.starts_with(crate::e2e::handshake::CTCP_TAG)
        || text.starts_with(crate::e2e::wire::WIRE_PREFIX)
}

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
    /// Refuse, carrying what to tell the user. The two refusals have
    /// genuinely different causes — the conversation moved and its window is
    /// gone, or we can no longer tell whether it moved at all — and the row
    /// they produce is routinely the last copy of the user's text, so it had
    /// better not describe the wrong one.
    Refuse(&'static str),
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
    pub in_flight: Arc<TranslateLimiter>,
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
    /// The backend is chosen by NAME, never implied by `translate.enabled`.
    /// `enabled` on its own resolves to no backend. The configured backend is
    /// built once at startup and can be AI, the explicit test stub, or a
    /// future machine-translation implementation behind the same seam.
    pub fn build(cfg: &crate::config::TranslateConfig) -> Self {
        use crate::translate::backend::BackendKind;
        let backend: Option<SharedBackend> = if cfg.enabled {
            match crate::translate::backend::backend_kind(&cfg.backend) {
                BackendKind::Stub => Some(Arc::new(StubBackend::new(0, 0))),
                BackendKind::Ai => match AiBackend::new(&cfg.ai) {
                    Ok(backend) => Some(Arc::new(backend)),
                    Err(error) => {
                        tracing::error!(%error, "translate: cannot build AI backend");
                        None
                    }
                },
                BackendKind::None | BackendKind::Unknown => None,
            }
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
        let in_flight = Arc::new(TranslateLimiter::new(cfg.max_in_flight.max(1) as usize));
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

/// What to tell the user at startup about the backend their config asked for,
/// or `None` when the answer is unremarkable.
///
/// Every outcome of `enabled = true` on this branch is worth a word, and the
/// silent ones are the dangerous ones: translation configured per buffer and
/// simply never happening looks identical to translation happening well, and
/// a stub that reverses word order looks — to the reader on the other side —
/// exactly like a person typing nonsense.
///
/// Pure so the wording is testable without standing up an `App`.
#[must_use]
pub fn startup_backend_notice(cfg: &crate::config::TranslateConfig) -> Option<String> {
    use crate::commands::types::{C_ERR, C_RST};
    use crate::translate::backend::{BackendKind, backend_kind};
    if !cfg.enabled {
        return None;
    }
    Some(match backend_kind(&cfg.backend) {
        BackendKind::None => format!(
            "{C_ERR}translate: enabled, but translate.backend is \"none\" — no \
             translator is installed, so nothing is translated in either \
             direction{C_RST}"
        ),
        // The name is echoed back because the whole failure is a typo, and a
        // message that will not show it makes the user hunt for one.
        BackendKind::Unknown => format!(
            "{C_ERR}translate: unknown translate.backend \"{name}\" — no \
             translator is installed, so nothing is translated. Known: \
             {known}{C_RST}",
            name = crate::commands::helpers::escape_format(cfg.backend.trim()),
            known = crate::translate::backend::BACKEND_NAMES.join(", "),
        ),
        BackendKind::Stub => format!(
            "{C_ERR}translate: the TEST backend is installed — it does not \
             translate, it REVERSES word order, and the result is what gets \
             sent to the network under your nick. Do not leave it on for a \
             real conversation.{C_RST}"
        ),
        BackendKind::Ai => {
            if let Some(error) = AiBackend::configuration_error(&cfg.ai) {
                format!("{C_ERR}translate: AI backend is unavailable — {error}{C_RST}")
            } else {
                return None;
            }
        }
    })
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
    mut req: TranslateRequest,
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
    req.deadline = std::time::Instant::now().checked_add(budget);
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
        Ok(Ok(outcome)) => single_line_or_refuse(id, outcome),
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

/// The most a translation may come back as, in bytes.
///
/// One line goes out, so one line has to come back — but the send path
/// SPLITS anything over the wire budget and ships every chunk. Without a
/// ceiling, a backend answering a three-word line with a megabyte turns one
/// keystroke into thousands of PRIVMSGs: a flood the user's own client
/// commits, under their nick, ending in a server-side kill or a ban. The
/// backend is untrusted (§2.0), and "one line" alone does not bound it.
///
/// Eight wire lines is far past any real translation of a single IRC message
/// — a source line is itself bounded by the same budget, and even the worst
/// expansion between languages, plus the appended original, stays well
/// inside it — and far short of a flood.
const MAX_TRANSLATION_BYTES: usize = 8 * crate::irc::MESSAGE_MAX_BYTES;

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
fn single_line_or_refuse(expected: u64, outcome: TranslateOutcome) -> TranslateOutcome {
    // The id correlates an answer with the line that asked for it, and the
    // backend is untrusted, so it is checked rather than believed. An outcome
    // carrying somebody else's id resolves THAT queue slot: one line's
    // translation is applied to another — published under the user's nick, in
    // the wrong conversation — while the line it belonged to sits pending
    // until the timeout. Both halves of that are silent.
    if outcome.id() != expected {
        tracing::error!(
            expected,
            answered = outcome.id(),
            "translate: backend answered with another request's id; refusing it"
        );
        return TranslateOutcome::Untranslated {
            id: expected,
            reason: UntranslatedReason::Error("backend answered the wrong request".to_string()),
        };
    }
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
    // CTCP framing is the CLIENT's to create, never the backend's.
    //
    // `\x01` is what tells every IRC client that a PRIVMSG is a request
    // rather than prose, and the outgoing path hands a plain translation to
    // `send_privmsg` verbatim. A backend answering with
    // `\x01DCC SEND … \x01` therefore puts a file-transfer offer on the wire
    // under the user's own nick, addressed to whoever they were talking to,
    // and a `\x01` embedded in an ACTION's body closes our framing early and
    // opens a second request behind it. Neither is a translation, so neither
    // is something to sanitise and send: refuse the answer and hand the
    // original back, like any other unusable response.
    //
    // Checked here rather than only at the send because the incoming
    // direction takes the same text — a reply carrying `\x01` renders as a
    // fake ACTION from the peer, attributing words to them they never said.
    if trimmed.contains('\x01') {
        tracing::error!(
            id,
            "translate: backend returned CTCP framing; refusing it"
        );
        return TranslateOutcome::Untranslated {
            id,
            reason: UntranslatedReason::Error("backend returned CTCP framing".to_string()),
        };
    }
    // The RPE2E wire prefixes are protocol without any framing byte, so the
    // `\x01` check above does not see them. A translation beginning with one
    // is swallowed as a handshake or fed to the ciphertext path by every
    // receiving Repartee — our own reflection handling included — instead of
    // rendering as prose, and on the incoming side it would vanish the same
    // way. The shipped stub alone can produce this: it reorders words, so
    // "hello RPEE2E" comes back tag-first. Not a translation anybody can
    // read, so it is refused like any other unusable answer.
    if reads_as_rpe2e_protocol(trimmed) {
        tracing::error!(
            id,
            "translate: backend returned E2E protocol text; refusing it"
        );
        return TranslateOutcome::Untranslated {
            id,
            reason: UntranslatedReason::Error("backend returned E2E protocol text".to_string()),
        };
    }
    // The RPE2E wire prefixes are protocol without any framing byte, so the
    // `\x01` check above does not see them. A translation beginning with one
    // is swallowed as a handshake or fed to the ciphertext path by every
    // receiving Repartee — our own reflection handling included — instead of
    // rendering as prose, and on the incoming side it would vanish the same
    // way. The shipped stub alone can produce this: it reorders words, so
    // "hello RPEE2E" comes back tag-first. Not a translation anybody can
    // read, so it is refused like any other unusable answer.
    // Whitespace-only counts as empty: a line of spaces renders exactly as
    // blank as nothing at all, and is just as useless on the wire. The test
    // is on the whole answer, but only the trailing newlines are actually
    // stripped from what gets sent — leading space in a real translation is
    // the backend's business, not ours to rewrite.
    if trimmed.trim().is_empty() {
        // Nothing usable came back. Accepting it renders an incoming line
        // blank when the original is hidden, and on the outgoing side puts an
        // empty PRIVMSG on the channel, reports the send as done, and throws
        // away the text the user typed. An empty answer is a gap like any
        // other: mark it and hand the original back.
        tracing::error!(id, "translate: backend returned an empty answer; refusing it");
        return TranslateOutcome::Untranslated {
            id,
            reason: UntranslatedReason::Error("backend returned nothing".to_string()),
        };
    }
    if trimmed.len() > MAX_TRANSLATION_BYTES {
        tracing::error!(
            id,
            bytes = trimmed.len(),
            limit = MAX_TRANSLATION_BYTES,
            "translate: backend returned an oversized answer; refusing it"
        );
        return TranslateOutcome::Untranslated {
            id,
            reason: UntranslatedReason::Error("backend answer too long".to_string()),
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

/// The translation concurrency limiter and the two numbers that describe it.
///
/// The ceiling in force is `total - debt`, so those two and the semaphore
/// itself are **one piece of state with one invariant**, not three counters.
/// Every change moves at least two of them: minting a permit raises `total`
/// and the semaphore, retiring one lowers both, cancelling a reduction moves
/// `debt` against `total`. They are therefore behind a lock rather than
/// individually atomic.
///
/// That is the correction to two previous attempts, both of which failed the
/// same way for the same reason:
///
/// - `total` on `App` while the workers retired permits for themselves. It
///   went stale high, the effective ceiling read back as the value from
///   before the reduction, and the next `sync_translate_from_config` — every
///   `/set`, `/reload` and `/translate add*` — applied the same reduction
///   again until no permits were left.
/// - `total` and `debt` as separate atomics. `retune` read `debt`, computed
///   how much to cancel, and subtracted; a worker paying a unit in between
///   made the subtraction underflow to `usize::MAX`. The effective ceiling
///   then reads zero and every worker retires its permit forever.
///
/// The lock is held only across counter arithmetic and non-blocking
/// semaphore calls, never across an await, and a worker takes it once per
/// translation — against a request that takes the better part of a second.
pub struct TranslateLimiter {
    permits: Arc<Semaphore>,
    counts: std::sync::Mutex<Counts>,
}

/// The pair that only means anything together. See [`TranslateLimiter`].
#[derive(Debug, Clone, Copy)]
struct Counts {
    /// Permits minted into the semaphore, less those retired again. NOT the
    /// available count: checked-out permits still belong to the total.
    total: usize,
    /// A reduction recorded but not yet applied, because the permits it
    /// wants were checked out at the time.
    debt: usize,
}

impl Counts {
    /// The ceiling in force. Never zero — see [`TranslateLimiter::retune`].
    const fn effective(self) -> usize {
        self.total.saturating_sub(self.debt)
    }

    /// The invariant every mutation has to leave standing: a reduction can
    /// never promise away more permits than exist, and never the last one.
    ///
    /// Checked after each change rather than reasoned about once. Breaking
    /// it is not a wrong number but a wedged client — `total < debt` makes
    /// the effective ceiling zero and has every worker retire its permit
    /// forever — and both previous attempts at this broke it in a way no
    /// single-threaded test could show.
    fn check(self) {
        debug_assert!(
            self.total > self.debt,
            "translate: concurrency invariant broken (total {}, debt {})",
            self.total,
            self.debt
        );
    }
}

impl TranslateLimiter {
    pub fn new(permits: usize) -> Self {
        let permits = permits.max(1);
        Self {
            permits: Arc::new(Semaphore::new(permits)),
            counts: std::sync::Mutex::new(Counts {
                total: permits,
                debt: 0,
            }),
        }
    }

    /// The counters, recovering from a poisoned lock rather than propagating
    /// the panic.
    ///
    /// A panic elsewhere must not wedge translation for the rest of the
    /// session: the guarded state is two integers, and the worst a torn
    /// update leaves behind is a ceiling off by one, which the next retune
    /// corrects.
    fn counts(&self) -> std::sync::MutexGuard<'_, Counts> {
        self.counts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The ceiling actually in force. Never zero: a reduction never records
    /// a debt that would take it below one, which is also what stops
    /// [`Self::acquire`] retiring its way into a permanent block.
    #[cfg(test)]
    pub fn effective(&self) -> usize {
        self.counts().effective()
    }

    /// Take one permit, first paying off any reduction that could not be
    /// applied when it was configured.
    ///
    /// `Semaphore::forget_permits` only takes permits that are AVAILABLE,
    /// and tokio hands a returned permit directly to the next waiter — so
    /// while anyone is waiting none ever becomes available, and a lowered
    /// `translate.max_in_flight` would go on being ignored for as long as
    /// the traffic lasts, which is exactly when it matters. Retiring permits
    /// HERE, at the one place they are handed out, makes that traffic pay
    /// the debt instead.
    ///
    /// Returns `None` only when the semaphore is closed, which is shutdown.
    pub async fn acquire(&self) -> Option<tokio::sync::OwnedSemaphorePermit> {
        loop {
            let permit = Arc::clone(&self.permits).acquire_owned().await.ok()?;
            // Claim the unit and drop the total together, under the lock,
            // BEFORE the permit is retired. Nothing else can then pay for
            // the same unit or read a total that no longer matches the
            // semaphore.
            let claimed = {
                let mut counts = self.counts();
                let owed = counts.debt > 0;
                if owed {
                    counts.debt -= 1;
                    counts.total -= 1;
                    counts.check();
                }
                owed
            };
            if claimed {
                permit.forget();
                continue;
            }
            return Some(permit);
        }
    }

    /// Move the ceiling to `want`, applying as much of it as possible now.
    pub fn retune(&self, want: usize) {
        // Clamped to one. A debt that covered every permit would have
        // `acquire` retire them all and block every worker for good.
        let want = want.max(1);
        let mut counts = self.counts();
        let have = counts.effective();
        if want == have {
            return;
        }
        if want > have {
            // Cancel an outstanding reduction before minting anything: those
            // permits still exist, they were merely promised away. Raising
            // the target back to where it started is therefore free, and
            // must not leave a debt behind that the workers would go on
            // paying — which would settle the limiter at a value the config
            // no longer asks for.
            let cancel = (want - have).min(counts.debt);
            counts.debt -= cancel;
            let mint = (want - have) - cancel;
            if mint > 0 {
                self.permits.add_permits(mint);
                counts.total += mint;
            }
        } else {
            counts.debt += have - want;
            Self::settle_locked(&self.permits, &mut counts);
        }
        counts.check();
        let effective = counts.effective();
        drop(counts);
        tracing::info!(
            from = have,
            to = want,
            effective,
            "translate: concurrency retuned"
        );
    }

    /// Retire as much of an owed reduction as is idle right now.
    ///
    /// Run from the tick as well, for the case the workers cannot cover: a
    /// reduction made while requests were in flight, after which the traffic
    /// stops. Nothing acquires a permit again, so nothing would pay it off.
    pub fn settle(&self) {
        let mut counts = self.counts();
        Self::settle_locked(&self.permits, &mut counts);
    }

    /// [`Self::settle`] with the lock already held, so `retune` can finish a
    /// reduction without releasing it — and without deadlocking on a
    /// non-reentrant mutex.
    fn settle_locked(permits: &Semaphore, counts: &mut Counts) {
        if counts.debt == 0 {
            return;
        }
        let forgotten = permits.forget_permits(counts.debt);
        if forgotten == 0 {
            return;
        }
        counts.debt -= forgotten;
        counts.total -= forgotten;
        counts.check();
        tracing::debug!(
            forgotten,
            remaining = counts.debt,
            "translate: settled part of a concurrency reduction"
        );
    }

    #[cfg(test)]
    pub fn available(&self) -> usize {
        self.permits.available_permits()
    }

    #[cfg(test)]
    pub fn debt(&self) -> usize {
        self.counts().debt
    }
}

/// Concurrent up to `max_in_flight`. See the module docs for why this one
/// is not serial.
fn spawn_incoming_worker(
    mut rx: mpsc::Receiver<PendingTranslate>,
    backend: SharedBackend,
    deliver: mpsc::Sender<TranslateDeliver>,
    permits: Arc<TranslateLimiter>,
    timeout_ms: Arc<std::sync::atomic::AtomicU64>,
) {
    tokio::spawn(async move {
        while let Some(pending) = rx.recv().await {
            let Some(permit) = permits.acquire().await else {
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
    permits: Arc<TranslateLimiter>,
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
    permits: Arc<TranslateLimiter>,
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
            let Some(_permit) = permits.acquire().await else {
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
                // An UNTRANSLATED outcome is counted here, once, before any
                // early return — its reason is the broker's or the
                // mechanism's own and nothing downstream changes it. The
                // outgoing path never goes through the reorder queue's
                // delivery, so this is the only place it is ever seen.
                //
                // A TRANSLATED outcome is NOT counted yet: this client may
                // still refuse the answer itself (an RPE2E-shaped body, a
                // smuggled control character), and counting those as
                // successes had `/translate status` reporting a healthy
                // provider while every send was being refused — the exact
                // question the tally exists to answer, answered wrong. The
                // send path records it — see `translated_wire_body`.
                if matches!(out.outcome, TranslateOutcome::Untranslated { .. }) {
                    self.state.translate_tally.record_outcome(&out.outcome);
                }
                // This send is back, whatever the outcome, so it no longer
                // holds ordinary sends to its conversation behind it.
                //
                // Cleared under the id the marker LIVES under, which is not
                // always the one this send was dispatched with: a rename moves
                // `outgoing_in_flight` along with the rest of the buffer's
                // state, so after one the entry sits under the new id. Reading
                // the redirect here rather than relying on
                // `redirect_outgoing_deliver` covers the refusal paths too —
                // those return before `out.buffer_id` is updated.
                let marker_id = match self
                    .state
                    .redirected_buffer_id(&out.buffer_id, out.submitted_at)
                {
                    crate::state::BufferRedirect::MovedTo(id) => id.to_string(),
                    crate::state::BufferRedirect::Stays | crate::state::BufferRedirect::Unknown => {
                        out.buffer_id.clone()
                    }
                };
                self.state.clear_outgoing_dispatch(&marker_id);
                if let RedirectVerdict::Refuse(why) = self.redirect_outgoing_deliver(&mut out) {
                    // The conversation this was addressed to has moved and
                    // its new window is gone. Sending under the old NAME is
                    // the one thing that must not happen: somebody else may
                    // hold that nick now, and this is a private message.
                    //
                    // Released BY ID, wherever it lives. On the Unknown
                    // verdict `out.buffer_id` is still the pre-rename id,
                    // but if the conversation really did rename inside the
                    // wait, `rekey_buffer_state` moved the reservation to
                    // the NEW id — a release under the old one is a no-op
                    // and the renamed conversation stays barricaded until
                    // the queue's expiry. Ids are globally unique, so the
                    // scan is exact.
                    if let Some(held_in) = self.state.release_echo_slot_anywhere(out.echo_id) {
                        self.drain_translate_queue(&held_in);
                    }
                    // A refusal of the SEND, not of the answer — the
                    // translation itself came back fine, so it still counts
                    // as one. This arm returns before `translated_wire_body`
                    // (the usual recording point) ever runs.
                    if matches!(out.outcome, TranslateOutcome::Translated { .. }) {
                        self.state.translate_tally.record_outcome(&out.outcome);
                    }
                    self.abandon_with_text(&out, why);
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
        // The recipient LEFT while this was being translated. Nothing points
        // anywhere — a quit is not a move — but the nick it was written to is
        // free from that moment, and the next person to ask the server for it
        // gets it. Sending now would hand private text to whoever that is.
        //
        // A channel is exempt for the reason it is exempt below: `#dupa`
        // cannot be claimed, and the "peer" of a channel quitting means
        // nothing about the channel.
        if out.buffer_type != crate::state::buffer::BufferType::Channel
            && self
                .state
                .query_departed_since(&out.buffer_id, out.submitted_at)
        {
            tracing::warn!(
                buffer_id = %out.buffer_id,
                "translate: the recipient quit during the wait; refusing the send"
            );
            return RedirectVerdict::Refuse(
                "its recipient left the network while it was being translated, \
                 so the nick may since have been taken by somebody else",
            );
        }
        let new_id = match self
            .state
            .redirected_buffer_id(&out.buffer_id, out.submitted_at)
        {
            crate::state::BufferRedirect::Stays => return RedirectVerdict::Proceed,
            crate::state::BufferRedirect::MovedTo(id) => id.to_string(),
            crate::state::BufferRedirect::Unknown => {
                // A CHANNEL cannot have moved. Only `rename_query_buffers`
                // ever re-keys a buffer and it skips everything that is not a
                // Query, so no channel id has ever had a redirect era — which
                // makes `Unknown` here not doubt about where the conversation
                // went but the absence of a question. `#dupa` is `#dupa`: the
                // name cannot be claimed out from under the send the way an
                // abandoned nick can. Refusing dropped a line that was always
                // safe to send, and only for a `translate.timeout_ms` above
                // the five-minute redirect horizon — which nothing forbids.
                //
                // Everything else stays fail-closed, INCLUDING any buffer type
                // added later: refusing a channel line costs a refusal row
                // with the text handed back, and sending a private line to
                // whoever now holds that nick costs the message.
                if out.buffer_type == crate::state::buffer::BufferType::Channel {
                    return RedirectVerdict::Proceed;
                }

                // We cannot say whether this conversation moved: the request
                // outlived the redirect history, which `translate.timeout_ms`
                // is free to allow. Sending under the recorded NAME is the one
                // thing that must not happen — somebody may hold that nick
                // now, and this is a private message.
                tracing::warn!(
                    buffer_id = %out.buffer_id,
                    "translate: send outlived the redirect history; refusing it"
                );
                return RedirectVerdict::Refuse(
                    "it waited longer than this conversation can be traced, so \
                     the nick it was addressed to can no longer be trusted",
                );
            }
        };
        let Some(new_name) = self.state.buffers.get(&new_id).map(|b| b.name.clone()) else {
            // Renamed, then closed. The old NAME must not be used either way
            // — somebody may hold that nick now — so this cannot fall through
            // to the ordinary send. Refuse and hand the text back.
            return RedirectVerdict::Refuse(
                "the conversation it was addressed to is no longer open",
            );
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
        // The retry string was rendered at DISPATCH and spells the old nick.
        // It is handed back to the user as ready-to-send text, so leaving it
        // stale is the same leak as addressing the old name: whenever the
        // author is looking at this conversation `deferred_retry_text`
        // returns it verbatim, and if the abandoned nick has been claimed,
        // pressing Enter delivers the private text to a stranger.
        //
        // Only the re-addressed form carries a nick. Buffer input retries as
        // itself (`retry_text == retry_body`) and an action retries as
        // `/me <body>`, which names nobody — see `retry_form_for`.
        if !out.is_action && out.retry_text != out.retry_body {
            out.retry_text = format!("/msg {new_name} {}", out.retry_body);
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
        // Nothing is sent from here, so an unanswerable redirect is not a
        // safety question: the outcome simply lands nowhere and the line it
        // belongs to is released by the queue's expiry.
        let buffer_id = &match self.state.redirected_buffer_id(buffer_id, submitted_at) {
            crate::state::BufferRedirect::MovedTo(id) => id.to_string(),
            crate::state::BufferRedirect::Stays | crate::state::BufferRedirect::Unknown => {
                buffer_id.to_string()
            }
        };
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
            let (ready, abandoned) = {
                let Some(queue) = self.state.translate_queues.get_mut(&buffer_id) else {
                    continue;
                };
                let expired = queue.expire(now, timeout);
                let forced = queue.enforce_ceiling(max_queue);
                if expired.timed_out > 0 || forced.forced > 0 {
                    tracing::debug!(
                        buffer_id = %buffer_id,
                        expired = expired.timed_out,
                        abandoned = expired.abandoned_reservations.len(),
                        forced = forced.forced,
                        barriers = forced.barriers_lifted,
                        "translate: released lines untranslated"
                    );
                }
                (queue.drain_ready(), expired.abandoned_reservations)
            };
            // Before the release, so a reflection arriving in the same tick
            // cannot take a record whose slot has just gone.
            self.state
                .forget_own_echo_records(&buffer_id, &abandoned);
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
        self.state.deliver_ready(buffer_id, ready);
    }

    /// Final stage of the outgoing pipeline once translation has returned.
    ///
    /// Uses the captured `nick` / `own_mode` / `peer_handle` rather than
    /// re-reading state: the user may have `/nick`ed, closed the buffer, or
    /// gained a channel mode during the wait, and reading current state
    /// would produce a local echo inconsistent with what hit the wire — or
    /// leak plaintext for an E2E PM whose Query buffer is gone. Same
    /// reasoning as `send_outgoing_substituted` in `shrink.rs`.
    /// The body one resolved outgoing outcome should put on the wire, or
    /// `None` after refusing the send and handing the text back.
    ///
    /// This is the last point before bytes reach the socket, so the refusals
    /// live HERE even though `single_line_or_refuse` already ran at the seam
    /// — making them a property of the send rather than of one upstream
    /// check is the same reasoning as re-running the E2E gate inside
    /// `build_outgoing_translate`.
    ///
    /// `send_privmsg` only breaks on `\r\n`: a bare `\n` rides into the
    /// trailing parameter and a server accepting bare-LF endings reads the
    /// remainder as a fresh command. `\x01` is checked on the BODY rather
    /// than on the wire lines because the wire lines are where our OWN
    /// action framing legitimately lives — one arriving inside the body puts
    /// a REQUEST on the wire under the user's nick rather than a message.
    /// The RPE2E prefixes are checked on the BACKEND's text only: the
    /// not-a-gap branch sends the user's own words, which every
    /// non-translated path ships verbatim, and translated buffers must not
    /// hold the user's own prose to a stricter rule.
    /// Count a Translated answer this client refused to use — "refused
    /// output" by [`crate::state::TranslateTally`]'s own definition.
    fn record_refused_answer(&mut self, why: &str) {
        self.state
            .translate_tally
            .record(&crate::translate::queue::ReadyOrigin::Untranslated(
                UntranslatedReason::Error(why.to_string()),
            ));
    }

    fn translated_wire_body<'a>(&mut self, out: &'a OutgoingTranslateDeliver) -> Option<&'a str> {
        // What the tally says about a TRANSLATED outcome is decided here,
        // not at delivery: an answer refused on content grounds is refused
        // output, and counting it as translated had `/translate status`
        // reporting a healthy provider while every send was being refused.
        // Transport failures further down (connection gone, wire write
        // failed) deliberately leave the count as translated — the answer
        // itself came back fine, and the tally reports the TRANSLATOR's
        // health, not the connection's.
        let body: &str = match &out.outcome {
            TranslateOutcome::Translated { text, .. } => {
                if reads_as_rpe2e_protocol(text) {
                    tracing::error!(
                        target = %out.buffer_name,
                        "translate: refusing a wire payload shaped like E2E protocol"
                    );
                    self.record_refused_answer("backend returned E2E protocol text");
                    self.release_echo_slot_and_drain(&out.buffer_id, out.echo_id);
                    self.abandon_with_text(out, "the translation was E2E protocol text");
                    return None;
                }
                text
            }
            // The broker decided this line needed no translation, so the
            // original IS the correct thing to send.
            TranslateOutcome::Untranslated { reason, .. } if !reason.is_gap() => &out.original_text,
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
                return None;
            }
        };
        if body.contains(['\r', '\n', '\x01']) {
            tracing::error!(
                target = %out.buffer_name,
                "translate: refusing a wire payload containing a line break or CTCP framing"
            );
            if matches!(out.outcome, TranslateOutcome::Translated { .. }) {
                self.record_refused_answer("the translation contained a control character");
            }
            self.release_echo_slot_and_drain(&out.buffer_id, out.echo_id);
            self.abandon_with_text(out, "the translation contained a control character");
            return None;
        }
        if matches!(out.outcome, TranslateOutcome::Translated { .. }) {
            self.state.translate_tally.record_outcome(&out.outcome);
        }
        Some(body)
    }

    fn send_outgoing_translated(&mut self, out: &OutgoingTranslateDeliver) {
        let Some(body) = self.translated_wire_body(out) else {
            return;
        };
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
        let wire_texts = wrap_outgoing_body(body, out.is_action);

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
        // The CONSTANT, never a literal: a bumped wire version would leave a
        // hardcoded prefix silently not matching, this branch would file
        // decorations for a ciphertext reflection that is swallowed earlier,
        // and the user's own message would never appear in their buffer.
        let is_e2e_encrypted = wire_lines
            .first()
            .is_some_and(|w| w.starts_with(crate::e2e::wire::WIRE_PREFIX));

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
            let is_last = i + 1 == wire_lines.len();
            let display = if is_last {
                Self::reflection_display(out, wire)
            } else {
                None
            };
            self.state.decorate_own_echo(
                &out.buffer_id,
                crate::state::AppState::own_echo_decoration(
                    wire.clone(),
                    out.echo_id,
                    display,
                    is_last,
                ),
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
        //
        // Escaped before composing, never after: the styling around it is
        // theme codes we mean, and escaping the finished string would show
        // those literally instead. `restore` keeps its raw form for the
        // composer, which is not parsed.
        let escape = crate::commands::helpers::escape_format;
        let shown = restore.as_deref().map_or_else(
            || {
                format!("{body} {dim}(to {target}){rst}",
                    body = escape(&out.retry_body),
                    target = escape(&out.buffer_name),
                    dim = crate::commands::types::C_DIM,
                    rst = crate::commands::types::C_RST,
                )
            },
            escape,
        );
        self.deliver_translate_error(
            &out.buffer_id,
            &format!(
                "{err}Not sent — {reason}.{rst}\n{dim}Your text:{rst} {shown}",
                // The reason can embed a BROKER-authored string — an
                // `UntranslatedReason::Error` carries the provider's own
                // message, and provider errors routinely contain `%`
                // ("quota 100% used"). Unescaped it reaches the theme
                // parser as format codes and mangles or recolors the row.
                reason = escape(reason),
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
                if self.web_buffer_unconfirmed.contains(session) {
                    // We do not know where this tab is, and every form below
                    // depends on knowing. Trusting a stale record hands back
                    // a bare body that goes to whatever conversation the
                    // browser is actually showing.
                    None
                } else {
                    self.web_active_buffers.get(session).map(String::as_str)
                }
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
        // `/msg` names a TARGET, not a network: `cmd_msg` resolves it against
        // `active_conn_id`, whatever that is when Enter is pressed. So the
        // re-addressed form is correct from any buffer ON THIS CONNECTION and
        // from no other — hand it to a composer belonging to a second network
        // and the private text goes to whoever holds that nick THERE.
        //
        // Which is the same leak as the branch above, reached through the
        // other door: that one asks whether the name still means the right
        // person, this one whether it is even being asked on the right
        // network. A user waiting on a translation switching to another
        // server is not exotic — it is what the wait is for.
        //
        // No spelling of `/msg` carries a network, so there is nothing to
        // re-address TO. Decline, exactly as an action does, and leave the
        // text in the error row where it can be copied deliberately.
        let looking_at = looking_at?;
        if self
            .state
            .buffers
            .get(looking_at)
            .is_none_or(|b| b.connection_id != out.conn_id)
        {
            return None;
        }
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
        //
        // Which makes escaping it matter MORE here than anywhere else, not
        // less: the other refusal rows have the composer as a second copy
        // whenever it happens to be empty, and this one deliberately has
        // none. The row is the only copy there will ever be.
        let retry = crate::commands::helpers::escape_format(&out.retry_text);
        self.deliver_translate_error(
            &out.buffer_id,
            &format!(
                "{err}Part of the message was sent — the rest was not ({reason}).{rst} \
                 {dim}Not restored to the input line, to avoid sending the \
                 first part twice.{rst}\n{dim}Your text:{rst} {retry}",
                // Uniform with the other refusal rows — today's callers pass
                // literals, and the escape is what keeps that from being a
                // requirement.
                reason = crate::commands::helpers::escape_format(reason),
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
        let (network, casemapping) = self
            .state
            .connections
            .get(conn_id)
            .map_or_else(
                || (String::new(), "rfc1459".to_string()),
                |connection| {
                    (
                        connection.label.clone(),
                        connection.isupport_parsed.casemapping().to_string(),
                    )
                },
            );
        let captured_nick = self
            .state
            .connections
            .get(conn_id)
            .map_or_else(|| nick.to_string(), |c| c.nick.clone());
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
        // From the MIRROR, never the config: after a peer's `/nick` the config
        // still holds the key the user typed and only the mirror knows where
        // that conversation lives now. Same map the incoming gate reads, so
        // the two directions cannot disagree about which buffer is
        // configured.
        let Some((source_lang, target_lang)) = crate::translate::resolve_langs(
            self.state.translate_buffers.get(buffer_id),
            &self.state.translate_my_lang,
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
                deadline: None,
                casemapping,
                known_nicks,
            },
            nick: captured_nick,
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
        // The mirror, for the reason spelled out in `AppState::translate_buffers`:
        // the config keys a conversation by the nick the user typed, and a
        // peer's `/nick` moves the conversation but not what they wrote.
        // Reading the config here sent a configured query's lines out
        // untranslated from the moment its peer renamed — silently, since the
        // bypass is the ordinary send path.
        let bypass = e2e_possible
            || !self.state.translate_active
            || self
                .state
                .translate_buffers
                .get(buffer_id)
                .is_none_or(|cfg| !cfg.outgoing);
        if bypass {
            // An ordinary send goes straight to the socket, while a
            // translation still in flight for this same conversation is
            // waiting on the provider. The plain one would arrive FIRST and
            // peers would read the two in the opposite order to the one they
            // were typed in — invisibly to the author, whose own buffer shows
            // them correctly, because the reservation still orders the
            // display.
            //
            // Only reachable while outgoing translation is being switched OFF
            // under a send already dispatched — `/translate delout`, `/e2e
            // on`, `translate.enabled false`, a `/reload` — because with it
            // still on the next send takes the Translate path and the
            // connection's serial lane keeps the order. Refusing rather than
            // reordering is the same choice this gate makes everywhere else:
            // a visible refusal beats an invisible wrong, and the wait is
            // bounded by the in-flight send's own timeout.
            if self.state.has_outgoing_in_flight(
                buffer_id,
                std::time::Duration::from_millis(self.config.translate.timeout_ms.saturating_mul(2)),
            ) {
                return OutgoingTranslatePolicy::Refuse(
                    "an earlier message to this conversation is still being \
                     translated — send it again in a moment",
                );
            }
            return OutgoingTranslatePolicy::NotApplicable;
        }
        // The same map the bypass above consulted — not the config, which
        // after a peer's `/nick` is keyed by a name this buffer no longer
        // has. Two lookups in one function reading two different maps is how
        // this went wrong the first time.
        let cfg = self
            .state
            .translate_buffers
            .get(buffer_id)
            .expect("the bypass check above proved this is present");
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
        // Which composer would take this back is a property of the ORIGIN,
        // not of the TUI's active buffer. `restore_input_text_to` routes a
        // web refusal to the web session's recorded buffer, and the two can
        // disagree: `/query bob <text>` switches the TUI's active buffer to
        // bob's query BEFORE the gate runs, so judging by the TUI here
        // returned the bare body while the restore delivered it to the
        // browser's composer — still sitting on whatever channel the command
        // was typed into, where Enter would publish the private text. The
        // form and the destination have to be computed against the SAME
        // buffer, exactly as `deferred_retry_text` does on the deferred
        // path.
        let looking_at = match &self.submit_origin {
            // Nothing is ever restored for a script; the re-addressed form
            // below is what the error row shows, and naming the target there
            // is strictly clearer.
            SubmitOrigin::Script => None,
            SubmitOrigin::Tui => self.state.active_buffer_id.as_deref(),
            SubmitOrigin::Web(session) => {
                if self.web_buffer_unconfirmed.contains(session) {
                    None
                } else {
                    self.web_active_buffers.get(session).map(String::as_str)
                }
            }
        };
        let composer_is_target = looking_at
            .and_then(|id| self.state.buffers.get(id))
            .is_some_and(|b| b.name.eq_ignore_ascii_case(target));
        if composer_is_target {
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
        // And note the WORK, which is a different question from where its row
        // goes — the ceiling may take the reservation back while the send is
        // still out.
        self.state.note_outgoing_dispatch(req.buffer_id);
        match self.translate_outgoing_tx.try_send(pending) {
            Ok(()) => false,
            Err(tokio::sync::mpsc::error::TrySendError::Full(p)) => {
                self.state.release_echo_slot(req.buffer_id, p.echo_id);
                self.state.clear_outgoing_dispatch(req.buffer_id);
                self.refuse_untranslatable_send(retry, "the translation queue is full")
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(p)) => {
                self.state.release_echo_slot(req.buffer_id, p.echo_id);
                self.state.clear_outgoing_dispatch(req.buffer_id);
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
        //
        // Through `deliver_translate_error`, not `add_local_event`: the row
        // has to take its place in the reorder queue when one exists, or it
        // renders above older lines still waiting on their translations.
        let row = format!(
            "{err}Not sent — {reason}.{rst} {dim}/translate delout this buffer \
             to send it as-is.{rst}\n{dim}Your text:{rst} {shown}",
            // The reason too: it can quote broker-authored text, and a
            // `%` in it would be eaten as a theme code — see
            // `abandon_with_text`.
            reason = crate::commands::helpers::escape_format(reason),
            err = crate::commands::types::C_ERR,
            dim = crate::commands::types::C_DIM,
            rst = crate::commands::types::C_RST,
            // Escaped: the row is styled by the theme parser, so a `%` the
            // user typed would be read as a theme code and eaten. The
            // COMPOSER gets the raw text below — it is not parsed.
            shown = crate::commands::helpers::escape_format(text),
        );
        if let Some(active) = self.state.active_buffer_id.clone() {
            self.deliver_translate_error(&active, &row);
        } else {
            tracing::warn!("translate: refusal with no buffer to report into: {row}");
        }
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
                //
                // Bound to the buffer this session is RECORDED at — the one
                // every retry form above was computed against. The tab may
                // switch during the round trip (its SwitchBuffer travels
                // client→server while this event travels server→client, so
                // the two can cross), and a bare retry restored into another
                // conversation's composer publishes it there on the next
                // Enter. The client applies the restore only while this
                // buffer is still its active one; the error row carries the
                // text either way. No record means no destination the form
                // is known to be safe for, so nothing is restored at all.
                let Some(buffer_id) = self.web_active_buffers.get(session_id).cloned() else {
                    return;
                };
                self.broadcast_web(crate::web::protocol::WebEvent::RestoreInput {
                    text: text.to_string(),
                    session_id: Some(session_id.clone()),
                    buffer_id: Some(buffer_id),
                });
            }
            SubmitOrigin::Tui => {
                if self.input.value.is_empty() {
                    self.input.value = text.to_string();
                    // `cursor_pos` is a BYTE offset — `ui/input.rs` advances
                    // it by `len_utf8` and slices `value[..cursor_pos]`. A
                    // char count lands mid-character on non-ASCII text and
                    // the next render frame panics on the slice.
                    self.input.cursor_pos = self.input.value.len();
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
                    //
                    // Concatenated with NOTHING between them. The echo has to
                    // be what the peers received, and the pieces already carry
                    // their own separators: `split_irc_message` breaks after a
                    // word's trailing whitespace, leaving it on the preceding
                    // chunk, and breaks a word too long for one line at a
                    // character boundary with no whitespace at all. Inserting a
                    // space therefore doubled the separator in the first case
                    // and put one INSIDE a word in the second — so the row the
                    // author sees, and the line written to their log, differed
                    // from what went out.
                    let piece = crate::app::e2e_gate::translatable_outgoing_body(&echo)
                        .unwrap_or(&echo);
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
        // A gated echo names its own buffer. Today every caller that
        // supplies one names the buffer the send was addressed through —
        // `redirect_outgoing_deliver` relies on that — but the plan carries
        // the id precisely so a future caller may diverge, so it is honoured
        // rather than assumed away.
        let echo_buffer = match &out.echo {
            OutgoingEchoPlan::Gated { buffer_id, .. } => buffer_id.clone(),
            _ => out.buffer_id.clone(),
        };
        if echo_buffer != out.buffer_id {
            // An echo written elsewhere can never fill the place the SEND
            // buffer holds for it, and nothing later on this path would give
            // it back — the barrier would stand until the queue's expiry
            // with the whole conversation behind it. The caller drains that
            // queue right after this returns.
            self.state.release_echo_slot(&out.buffer_id, out.echo_id);
        }
        if !self.state.buffers.contains_key(&echo_buffer) {
            // No echo will be written, so the place held for it must go
            // back. The buffer being gone does NOT mean the queue is: a
            // by-target send to a conversation with no window reserves into
            // a queue `remove_buffer` never saw, and a `/join` inside the
            // timeout would park the fresh channel's every line behind the
            // dead reservation. The caller drains right after this returns.
            self.state.release_echo_slot(&out.buffer_id, out.echo_id);
            crate::commands::helpers::add_local_event(
                self,
                &format!(
                    "{dim}translate: message to {target} sent (buffer was \
                     closed during the translation wait){rst}",
                    // A channel name may legally contain `%`.
                    target = crate::commands::helpers::escape_format(&out.buffer_name),
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
        // Chunk the BODY at the wire budget, never the composed display.
        //
        // The budget is a property of a PRIVMSG, not of a row on screen, and
        // the body is what the wire carried. Splitting the display instead —
        // translation plus ` [original]`, which `show_original_out` makes the
        // common case — cut at boundaries the wire never used, so no chunk's
        // text was a wire text and none could carry `wire_origin`. That cost
        // the suffix its dimming, and cost every chunk its identity: `dedup`
        // then keys the row by what is DISPLAYED, so a later CHATHISTORY
        // replay of the same message matches nothing and splices a second,
        // untranslated copy in beside it.
        //
        // Chunked this way the rows are the wire lines, one for one. Only the
        // LAST carries the original — the same rule `expect_own_reflection`
        // follows for the reflection of a split send, and for the same reason:
        // repeating it on each would say the same thing N times, and putting
        // it on the first would place it before the text it is the original
        // of. The last row may exceed the wire budget once the suffix is on
        // it, which is correct: nothing sends it.
        //
        // The budget is the BODY's, not the line's: an action's framing rides
        // the same wire line, so its prose broke at a smaller budget. Cutting
        // the echo at the full line budget instead put boundaries where the
        // wire never did — a `/me` body between the two budgets showed the
        // author ONE row where peers received TWO actions.
        let budget = outgoing_body_budget(out.is_action);
        let body_chunks = if echo_body.len() <= budget {
            vec![echo_body.to_string()]
        } else {
            crate::irc::split_irc_message(echo_body, budget)
        };
        let composes = matches!(out.outcome, TranslateOutcome::Translated { .. });
        let last_chunk = body_chunks.len().saturating_sub(1);
        // Whose line this IS, resolved now rather than at dispatch.
        //
        // The wire message carries no sender: the SERVER stamps the prefix as
        // it relays it, using whatever nick the connection holds at that
        // moment. The send happens here, so that is the nick this row was
        // published under and the one every peer saw. A `/nick` during the
        // wait — and the wait is exactly when a user has time to do one —
        // left the echo attributed to a name nobody received the message
        // from, in the buffer and in the log.
        //
        // `conn.nick` is written from the server's own NICK confirmation, not
        // optimistically on `/nick`, so reading it cannot pick up a change the
        // server refused. The channel prefix is re-derived for the same reason
        // and from the same instant.
        //
        // The peer handle stays CAPTURED — see `build_outgoing_translate`. It
        // is not identity for display, it is what keeps an E2E DM encrypted
        // when its Query buffer was closed during the wait, and re-reading it
        // would find nothing and fall through to plaintext.
        let echo_nick = self
            .state
            .connections
            .get(&out.conn_id)
            .map_or_else(|| out.nick.clone(), |c| c.nick.clone());
        let nick_mode_str = self
            .state
            .nick_prefix(&echo_buffer, &echo_nick)
            .map(|c| c.to_string());
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
        let chunks: Vec<crate::state::buffer::Message> = body_chunks
            .into_iter()
            .enumerate()
            .map(|(i, body)| {
                let (text, suffix_at) = if composes && i == last_chunk {
                    crate::translate::compose_display(&body, &out.original_text, out.show_original)
                } else {
                    (body.clone(), None)
                };
                crate::state::buffer::Message {
                    log_key: None,
                    id: self.state.next_message_id(),
                    timestamp: chrono::Utc::now(),
                    message_type: message_type.clone(),
                    nick: Some(echo_nick.clone()),
                    nick_mode: nick_mode_str.clone(),
                    text,
                    highlight: false,
                    event_key: None,
                    event_params: None,
                    log_msg_id: None,
                    log_ref_id: None,
                    tags: None,
                    // What the NETWORK carried for this row — our translation,
                    // not the original we typed. A CHATHISTORY replay of our
                    // own message brings back exactly this, so it is what the
                    // row must be keyed by. See `WireOrigin`.
                    wire_origin: Some(crate::state::buffer::WireOrigin {
                        text: body,
                        suffix_at,
                    }),
                    translation_suffix_at: None,
                }
            })
            .collect();
        // `add_own_message_chunks`, not `add_message`: these rows have to take
        // the ONE place reserved when the user pressed Enter, together, so
        // they keep their position among the lines that arrived while the
        // translation ran. `add_message` would append them at the end and
        // hand them to the translation gate on the way.
        self.state
            .add_own_message_chunks(&echo_buffer, out.echo_id, chunks);
    }

    /// Route a translation-pipeline error to the right buffer, falling back
    /// to the active one when the destination is gone.
    ///
    /// IN ORDER, through the buffer's reorder queue when one exists — the
    /// same treatment the day separator gets, for the same reason. This row
    /// dates the refusal; appended directly it renders ABOVE older lines
    /// still waiting on their translations, silently reordering the timeline
    /// on the very row that is often the only surviving copy of the user's
    /// refused text. With no queue it appears at once, exactly as before.
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
        let id = self.state.next_message_id();
        self.state.add_local_message_in_order(
            &buf_id,
            crate::state::buffer::Message {
                log_key: None,
                id,
                timestamp: chrono::Utc::now(),
                message_type: crate::state::buffer::MessageType::Event,
                nick: None,
                nick_mode: None,
                text: message.to_string(),
                highlight: false,
                event_key: None,
                event_params: None,
                log_msg_id: None,
                log_ref_id: None,
                tags: None,
                wire_origin: None,
                translation_suffix_at: None,
            },
        );
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
        // Normalise the floors HERE, where every config route converges —
        // startup, `/set`, `/reload`, `/translate add*`. `/set` validates
        // (500 ms floor on the timeout), but a hand-edited `config.toml`
        // arrives via serde with no validation at all, and an unfloored
        // `timeout_ms = 0` reaches the queue tick as `Duration::ZERO`:
        // every pending line times out on the next tick and every send is
        // refused — the feature bricked with misleading per-line markers
        // instead of the validation error `/set` would have shown.
        self.config.translate.timeout_ms = self.config.translate.timeout_ms.max(500);
        self.config.translate.ai.preferred_attempt_ms = self
            .config
            .translate
            .ai
            .preferred_attempt_ms
            .max(500);
        self.config.translate.max_in_flight = self.config.translate.max_in_flight.max(1);
        self.config.translate.max_queue = self.config.translate.max_queue.max(1);
        // A backend OBJECT exists only if one was built at startup; the
        // config's answer is re-read every time because turning translation
        // OFF must take effect at once. `/set translate.backend none` and a
        // hand-edited `config.toml` plus `/reload` are both ways of saying
        // "stop sending my lines to a translator", and a switch that only
        // works after a restart is not one a user reaches for when they
        // change their mind mid-conversation. Turning it back ON still needs
        // the restart the workers were bound in — `warn_if_translate_cannot_run`
        // says so.
        let configured_kind =
            crate::translate::backend::backend_kind(&self.config.translate.backend);
        let has_matching_backend = self.translate_backend.as_ref().is_some_and(|backend| {
            backend.kind() == configured_kind && backend.is_ready()
        });
        self.state.translate_active = self.config.translate.enabled && has_matching_backend;
        self.state
            .translate_buffers
            .clone_from(&self.config.translate.buffers);
        // The config keys a conversation by the nick the user typed; a peer's
        // `/nick` moves the conversation, not what they wrote. Re-applied
        // here, over a mirror that was just re-derived, so no `/set`,
        // `/reload` or `/translate` can silently end translation for a
        // conversation whose peer renamed.
        self.apply_translate_follows();

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

    /// Apply `translate.max_in_flight` to the running workers.
    ///
    /// The limiter is built once at startup, so without this the setting is
    /// accepted, persisted, and silently ignored — worse than not offering
    /// it.
    fn retune_translate_concurrency(&self) {
        if let Some(limiter) = self.translate_in_flight.as_ref() {
            limiter.retune(self.config.translate.max_in_flight.max(1) as usize);
        }
    }

    /// Retire whatever an owed reduction can claim right now. Called from the
    /// maintenance tick — see [`TranslateLimiter::settle`].
    pub(crate) fn settle_translate_concurrency_debt(&self) {
        if let Some(limiter) = self.translate_in_flight.as_ref() {
            limiter.settle();
        }
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
    ///
    /// Delegates, because `AppState::remove_buffer` has to be able to do this
    /// for itself: it is reached from the IRC event path (`/part`, a kick),
    /// which cannot call up to the App, and a buffer that disappears with
    /// lines still queued loses them.
    pub(crate) fn flush_translate_queue(&mut self, buffer_id: &str) {
        self.state.flush_translate_queue(buffer_id);
    }

    /// Follow a re-keyed query buffer in the half of the settings that
    /// `AppState` cannot reach.
    ///
    /// The config itself is NOT touched. Moving the key inside
    /// `config.translate.buffers` — which is what this used to do — makes the
    /// migration permanent the moment anything saves the config, and `/set`,
    /// `/translate add*` and several admin commands all save the whole of it.
    /// So somebody else's `/nick` quietly rewrote a line the user had typed,
    /// and after a restart their setting named a nick that existed for five
    /// minutes one afternoon. The documented behaviour is the opposite: the
    /// conversation is followed for this session, and the saved setting still
    /// names the nick they wrote down.
    ///
    /// See [`App::translate_follows`].
    pub(crate) fn drain_pending_buffer_rekeys(&mut self) {
        if self.state.pending_buffer_rekeys.is_empty() {
            return;
        }
        let rekeys = std::mem::take(&mut self.state.pending_buffer_rekeys);
        let mut followed = false;
        for (old_id, new_id) in rekeys {
            // A conversation the user never configured has nothing to follow.
            // Skipping it also bounds the list by the number of configured
            // buffers, so a peer flipping nicks cannot grow it.
            if !self.translate_governs(&old_id) {
                continue;
            }
            tracing::debug!(%old_id, %new_id, "translate: following a re-keyed query buffer");
            self.record_translate_follow(&old_id, &new_id);
            followed = true;
        }
        if followed {
            // Re-derive the mirror so it and the config agree again —
            // `rekey_buffer_state` moved the mirror, and the follows are
            // re-applied over it here.
            self.sync_translate_from_config();
        }
    }

    /// Where the setting saved under `buffer_id` is being applied right now,
    /// when a peer's `/nick` has moved that conversation this session.
    ///
    /// For `/translate list`, which prints the saved keys: without it the list
    /// says `frank` while the user is talking to `frankie` and gives them no
    /// way to tell whether translation is still running.
    pub(crate) fn translate_follow_target(&self, buffer_id: &str) -> Option<&str> {
        self.translate_follows
            .iter()
            .find(|(from, _)| from == buffer_id)
            .map(|(_, to)| to.as_str())
    }

    /// Whether some setting — written by the user or already following a
    /// rename — governs this id.
    fn translate_governs(&self, buffer_id: &str) -> bool {
        self.config.translate.buffers.contains_key(buffer_id)
            || self.translate_follows.iter().any(|(_, to)| to == buffer_id)
    }

    /// Record that the conversation at `old_id` now lives at `new_id`.
    fn record_translate_follow(&mut self, old_id: &str, new_id: &str) {
        // A follow that pointed AT the new id is dead: that conversation has
        // just lost the nick to this one, and applying its settings here
        // would hand one person's translation setup to another.
        self.translate_follows.retain(|(_, to)| to != new_id);
        // A peer who renames twice moves the SAME setting again. Retarget
        // rather than chain, so the list stays one entry per configured
        // conversation and the order it is applied in cannot matter.
        if let Some((_, to)) = self
            .translate_follows
            .iter_mut()
            .find(|(_, to)| to == old_id)
        {
            new_id.clone_into(to);
            return;
        }
        self.translate_follows
            .push((old_id.to_string(), new_id.to_string()));
    }

    /// Re-point the per-buffer mirror at where each followed conversation
    /// lives now. Called from `sync_translate_from_config`, which has just
    /// re-derived that mirror from the config and would otherwise undo every
    /// rename this session followed.
    fn apply_translate_follows(&mut self) {
        for (from, to) in &self.translate_follows {
            if let Some(cfg) = self.state.translate_buffers.remove(from) {
                self.state.translate_buffers.insert(to.clone(), cfg);
            }
        }
    }

    /// Bring a followed setting under the id its conversation lives at now,
    /// because the user is about to change it THERE.
    ///
    /// Until this moment the entry stays keyed by the nick they typed and the
    /// rename is followed at runtime. An explicit `/translate add*|del*` on
    /// the conversation is the user renaming the setting themselves: from
    /// here it is keyed by the nick they are looking at, it goes to disk that
    /// way, and the follow ends.
    ///
    /// Without this the two disagree in the worst possible direction —
    /// `delin` would remove an entry the follow puts straight back, leaving
    /// translation running on a conversation the user has just switched off.
    pub(crate) fn materialize_translate_follow(&mut self, buffer_id: &str) {
        let Some(pos) = self
            .translate_follows
            .iter()
            .position(|(_, to)| to == buffer_id)
        else {
            return;
        };
        let (from, _) = self.translate_follows.remove(pos);
        if let Some(cfg) = self.config.translate.buffers.remove(&from) {
            // Never over an entry the user wrote for this id themselves —
            // that one is the more explicit statement of the two.
            self.config
                .translate
                .buffers
                .entry(buffer_id.to_string())
                .or_insert(cfg);
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
        // Ownership comes from the KEY, not from a live buffer. A queue or a
        // reflection record routinely outlives its window — a send to a
        // conversation that was never opened, or one whose query was closed
        // while the translation ran — and consulting `buffers` skipped
        // exactly those on disconnect. A reconnect inside the record's TTL
        // that resends the same text then consumed the stale record, taking
        // the old reserved id with it and leaving the new reservation
        // blocking the buffer until it timed out.
        let belongs = |id: &String| {
            id.split_once('/')
                .is_some_and(|(owner, _)| owner == conn_id)
        };
        let buffer_ids: Vec<String> = self
            .state
            .translate_queues
            .keys()
            .filter(|id| belongs(id))
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
            .filter(|id| belongs(id))
            .cloned()
            .collect();
        for buffer_id in stale {
            self.state.own_echo_decorations.remove(&buffer_id);
        }
    }

    /// Flush every buffer's queue. Used on quit and detach, at the end of
    /// [`crate::app::App::run`].
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
            log_key: None,
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
            translation_suffix_at: None,
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

    /// What the user would actually read in the row: the theme parser's
    /// output, joined back into plain text.
    fn as_rendered(row_text: &str) -> String {
        crate::theme::parser::parse_format_string(row_text, &[])
            .iter()
            .map(|s| s.text.as_str())
            .collect()
    }

    /// Every character class the two rendering passes can eat, in text a
    /// person might really type: a printf format, a price, a shell variable.
    const TREACHEROUS: &str = "printf(\"%i\") costs $5 or 50% — $*, $[3]0, %Z112233, %N";

    #[test]
    fn a_refused_message_is_readable_in_the_row_exactly_as_it_was_typed() {
        // This row is frequently the ONLY surviving copy: the composer
        // restore declines to clobber text the user has started typing since,
        // which seconds after they pressed Enter is the normal case. A row
        // that silently drops `%i` and the `5` from `$5` is not something
        // they can retype from — and they have no way to know it happened.
        let mut app = app_with_buffer();
        app.refuse_untranslatable_send(TREACHEROUS, "no provider");

        let row = app.state.buffers[BUF]
            .messages
            .back()
            .expect("the refusal row");
        assert!(
            as_rendered(&row.text).contains(TREACHEROUS),
            "the row renders as something other than what was typed:\n  {}",
            as_rendered(&row.text)
        );
    }

    #[test]
    fn a_deferred_refusal_is_readable_in_the_row_exactly_as_it_was_typed() {
        // The deferred path builds its own row, and it is the one that
        // matters most — a deferred failure by definition arrives seconds
        // later, which is exactly when the composer is busy.
        let mut app = app_with_buffer();
        let out = outgoing(
            TREACHEROUS,
            TranslateOutcome::Untranslated {
                id: 1,
                reason: crate::translate::UntranslatedReason::NoProvider,
            },
            false,
        );
        app.apply_translate_deliver(TranslateDeliver::Outgoing(Box::new(out)));

        let row = app.state.buffers[BUF]
            .messages
            .back()
            .expect("the refusal row");
        assert!(
            as_rendered(&row.text).contains(TREACHEROUS),
            "the row renders as something other than what was typed:\n  {}",
            as_rendered(&row.text)
        );
        assert_eq!(
            app.input.value, TREACHEROUS,
            "and the composer gets it RAW — it is not parsed, so escaping it \
             there would hand back text with doubled signs in it"
        );
    }

    #[test]
    fn a_backend_answer_carrying_ctcp_framing_never_reaches_the_wire() {
        // `\x01` is what tells every IRC client that a PRIVMSG is a REQUEST
        // rather than prose. A backend answering with `\x01DCC SEND …\x01`
        // otherwise puts a file-transfer offer on the channel under the
        // user's own nick — one line, correctly correlated, non-empty, well
        // inside the byte ceiling, so every other guard at the seam waves it
        // through.
        //
        // The reason in the row is what distinguishes this from the checks
        // BELOW it: reaching those would have said "the connection is
        // unavailable" instead, since this app has no handle.
        let mut app = app_with_buffer();
        app.apply_translate_deliver(TranslateDeliver::Outgoing(Box::new(outgoing(
            "moje zdanie",
            TranslateOutcome::Translated {
                id: 1,
                text: "\u{1}DCC SEND secrets.txt 2130706433 1024 512\u{1}".to_string(),
            },
            false,
        ))));

        assert_eq!(
            app.input.value, "moje zdanie",
            "the user's own text comes back, as with any other refusal"
        );
        let last = app.state.buffers[BUF]
            .messages
            .back()
            .expect("an error row explains why");
        assert!(
            last.text.contains("control character"),
            "refused before anything else could be tried: {}",
            last.text
        );
    }

    #[test]
    fn a_translation_shaped_like_e2e_protocol_never_reaches_the_wire() {
        // Belt and braces at the send, mirroring the CTCP check: this is the
        // last point before the socket, and the reason in the row is what
        // proves THIS guard fired — reaching the checks below would have
        // said "the connection is unavailable" instead, since this app has
        // no handle.
        let mut app = app_with_buffer();
        app.apply_translate_deliver(TranslateDeliver::Outgoing(Box::new(outgoing(
            "moje zdanie",
            TranslateOutcome::Translated {
                id: 1,
                text: "+RPE2E01 dGFqbmU=".to_string(),
            },
            false,
        ))));

        assert_eq!(
            app.input.value, "moje zdanie",
            "the user's own text comes back, as with any other refusal"
        );
        let last = app.state.buffers[BUF]
            .messages
            .back()
            .expect("an error row explains why");
        assert!(
            last.text.contains("E2E protocol"),
            "refused before anything else could be tried: {}",
            last.text
        );
    }

    #[test]
    fn a_backend_answer_cannot_break_out_of_the_action_framing_we_put_round_it() {
        // The subtler half. `/me` wraps the answer in `\x01ACTION …\x01`
        // ourselves, so a bare `\x01` inside the BODY closes our framing
        // early and opens a second, attacker-chosen request behind it:
        //
        //   \x01ACTION wzdycha\x01 \x01DCC SEND …\x01
        //
        // The guard is therefore on the body, before wrapping — on the wire
        // lines it could not tell our own delimiters from smuggled ones.
        let mut app = app_with_buffer();
        let mut out = outgoing(
            "wzdycha",
            TranslateOutcome::Translated {
                id: 1,
                text: "seufzt\u{1} \u{1}DCC SEND secrets.txt 2130706433 1024 512".to_string(),
            },
            false,
        );
        out.is_action = true;
        out.retry_text = "/me wzdycha".to_string();
        app.apply_translate_deliver(TranslateDeliver::Outgoing(Box::new(out)));

        let rows: Vec<String> = app.state.buffers[BUF]
            .messages
            .iter()
            .map(|m| m.text.clone())
            .collect();
        assert!(
            rows.last().is_some_and(|t| t.contains("control character")),
            "refused, not sanitised and sent: {rows:?}"
        );
    }

    /// A script engine that eats every event it is shown.
    ///
    /// Stands in for the one line of Lua a user writes to hide something —
    /// `hook_signal("privmsg", function() return true end)`. What matters for
    /// the test is only that `emit` says `Suppress`.
    #[derive(Debug)]
    struct SuppressingEngine;

    impl crate::scripting::engine::ScriptEngine for SuppressingEngine {
        fn extension(&self) -> &'static str {
            "test"
        }
        fn load_script(
            &mut self,
            _path: &std::path::Path,
            _api: &crate::scripting::engine::ScriptAPI,
        ) -> color_eyre::Result<crate::scripting::engine::ScriptMeta> {
            unimplemented!("nothing loads this engine from disk")
        }
        fn unload_script(&mut self, _name: &str) -> color_eyre::Result<()> {
            Ok(())
        }
        fn emit(&self, _event: &crate::scripting::event_bus::Event) -> crate::scripting::event_bus::EventResult {
            crate::scripting::event_bus::EventResult::Suppress
        }
        fn handle_command(
            &self,
            _name: &str,
            _args: &[String],
            _connection_id: Option<&str>,
        ) -> Option<crate::scripting::event_bus::EventResult> {
            None
        }
        fn fire_timer(&self, _timer_id: u64) {}
        fn loaded_scripts(&self) -> Vec<crate::scripting::engine::ScriptMeta> {
            vec![crate::scripting::engine::ScriptMeta {
                name: "suppress-everything".to_string(),
                version: None,
                description: None,
                path: std::path::PathBuf::from("suppress-everything.test"),
            }]
        }
    }

    #[test]
    fn a_script_eating_our_reflection_does_not_strand_the_conversation() {
        // The whole path, not the helper: script suppression of a PRIVMSG
        // returns from `handle_irc_event` BEFORE `handle_privmsg` is ever
        // called, so the reflection of a translated send never reaches the
        // code that fills its reservation. Driving `release_suppressed_own_echo`
        // directly would pass with the call site deleted.
        let mut app = app_with_buffer();
        let mut conn = crate::state::events::tests::make_test_connection();
        conn.id = "test".to_string();
        conn.nick = "me".to_string();
        app.state.add_connection(conn);

        let mut manager =
            crate::scripting::engine::ScriptManager::new(std::path::PathBuf::from("/nonexistent"));
        manager.register_engine(Box::new(SuppressingEngine));
        app.script_manager = Some(manager);

        // A place held for our own reflection, with a peer's line already
        // resolved behind it.
        let mut queue = TranslateQueue::new();
        queue.reserve(1);
        queue.push_resolved(2, message(2, "odpowiedz kolegi"), ActivityLevel::Activity);
        app.state.translate_queues.insert(BUF.to_string(), queue);
        app.state.decorate_own_echo(
            BUF,
            crate::state::AppState::own_echo_decoration("mein satz".to_string(), 1, None, true),
        );

        let reflection: ::irc::proto::Message = ":me!u@h PRIVMSG #dupa :mein satz\r\n"
            .parse()
            .expect("valid");
        app.handle_irc_event(crate::irc::IrcEvent::Message(
            "test".to_string(),
            Box::new(reflection),
        ));

        let rows: Vec<String> = app.state.buffers[BUF]
            .messages
            .iter()
            .map(|m| m.text.clone())
            .collect();
        assert_eq!(
            rows,
            vec!["odpowiedz kolegi".to_string()],
            "the script ate our own line, as it asked to — but the reply behind \
             it is not held until the queue's expiry: {rows:?}"
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
    fn restored_text_leaves_the_cursor_on_a_char_boundary_at_the_end() {
        // `cursor_pos` is a BYTE offset — the renderer slices
        // `value[..cursor_pos]` every frame. A char count lands inside a
        // multi-byte character and the next render panics, taking the whole
        // TUI down with the refused message it was handing back.
        let mut app = app_with_buffer();
        app.refuse_untranslatable_send("zażółć gęślą jaźń", "no provider");
        assert_eq!(
            app.input.cursor_pos,
            app.input.value.len(),
            "the cursor sits at the END of the restored text, in bytes"
        );
        assert!(app.input.value.is_char_boundary(app.input.cursor_pos));
    }

    #[test]
    fn a_web_refusal_readdresses_by_the_web_composers_buffer_not_the_tuis() {
        // `/query bob <text>` switches the TUI's active buffer to bob
        // BEFORE the gate runs, so judging "is the composer on the target"
        // by the TUI said yes and returned the BARE body — while the
        // restore was routed to the web session's composer, still sitting
        // on the channel the command was typed into, where Enter publishes
        // the private text. The form and the destination must be computed
        // against the same buffer.
        let mut app = app_with_buffer();
        app.state
            .add_buffer(Buffer::for_test("test", BufferType::Query, "bob"));
        // The TUI — a different head entirely — is looking at bob's query…
        app.state.set_active_buffer("test/bob");
        // …but the SUBMITTING web session's composer is on the channel.
        app.submit_origin = SubmitOrigin::Web("s1".to_string());
        app.web_active_buffers
            .insert("s1".to_string(), BUF.to_string());
        let mut rx = app.web_broadcaster.subscribe();

        let retry = app.retry_form_for("bob", "sekret", false);
        app.refuse_untranslatable_send(&retry, "no target language set for this buffer");

        match rx.try_recv().expect("the refusal reaches the browser") {
            crate::web::protocol::WebEvent::RestoreInput {
                text, buffer_id, ..
            } => {
                assert_eq!(
                    text, "/msg bob sekret",
                    "re-addressed: the composer that takes this back is NOT \
                     on bob's query, so a bare body would publish to #dupa"
                );
                assert_eq!(buffer_id.as_deref(), Some(BUF));
            }
            other => panic!("expected RestoreInput, got {other:?}"),
        }
    }

    #[test]
    fn an_echo_for_a_buffer_that_never_existed_still_frees_its_reservation() {
        // `/translate addout` config outlives its window (`/part` keeps the
        // entry), and a `/msg` to the parted channel reserves into a queue
        // `remove_buffer` never saw. Returning from the missing-buffer echo
        // branch without the release left that reservation barricading the
        // id — and a `/join` inside the timeout parked the fresh channel's
        // every line behind it.
        let mut app = app_with_dying_handle(usize::MAX);
        app.conn_generations.insert("test".to_string(), 1);
        app.state.reserve_echo_slot("test/#gone", 1);
        let mut out = outgoing(
            "moje zdanie",
            TranslateOutcome::Translated {
                id: 1,
                text: "mein satz".to_string(),
            },
            false,
        );
        out.buffer_id = "test/#gone".to_string();
        out.buffer_name = "#gone".to_string();
        out.conn_generation = Some(1);

        app.apply_translate_deliver(TranslateDeliver::Outgoing(Box::new(out)));

        assert!(
            !app.state.translate_queues.contains_key("test/#gone"),
            "the place held for an echo no buffer can take goes back at once"
        );
    }

    #[test]
    fn a_refusal_row_waits_its_turn_behind_lines_still_translating() {
        // The row dates the refusal. Appended directly it renders ABOVE
        // older lines still waiting on their translations — silent
        // reordering on the very row that is often the only surviving copy
        // of the refused text. With no queue it still appears at once.
        let mut app = app_with_queue(1);
        app.state.set_active_buffer(BUF);

        app.refuse_untranslatable_send("tekst", "no provider");
        assert!(
            shown(&app).is_empty(),
            "the row waits for the older line: {:?}",
            shown(&app)
        );

        app.apply_translate_deliver(TranslateDeliver::Incoming {
            buffer_id: BUF.to_string(),
            outcome: TranslateOutcome::Translated {
                id: 1,
                text: "pierwsza".to_string(),
            },
            submitted_at: dispatched_long_ago(),
        });
        let rows = shown(&app);
        assert_eq!(rows.len(), 2, "both are out once the head resolves: {rows:?}");
        assert_eq!(rows[0], "pierwsza", "the older line first");
        assert!(
            rows[1].contains("Not sent") && rows[1].contains("tekst"),
            "the refusal row after it, text intact: {}",
            rows[1]
        );
    }

    #[test]
    fn a_refusal_that_outlived_the_redirect_history_frees_the_moved_reservation() {
        // On the Unknown verdict `out.buffer_id` is still the pre-rename
        // id, but the rename moved the reservation to the NEW id. A release
        // under the old id is a no-op and the renamed conversation stays
        // barricaded until the queue's own expiry. Ids are globally unique,
        // so the release finds it wherever it lives.
        let mut app = app_with_buffer();
        app.state
            .add_buffer(Buffer::for_test("test", BufferType::Query, "bob"));
        // The reservation lives under bob (post-rename)…
        let mut queue = TranslateQueue::new();
        queue.reserve(1);
        queue.push_resolved(2, message(2, "linia za bariera"), ActivityLevel::Activity);
        app.state.translate_queues.insert("test/bob".to_string(), queue);
        // …while the deliver still names alice, dispatched so long ago the
        // redirect history cannot answer for it any more.
        let mut out = outgoing(
            "sekret",
            TranslateOutcome::Translated {
                id: 1,
                text: "geheim".to_string(),
            },
            false,
        );
        out.buffer_id = "test/alice".to_string();
        out.buffer_name = "alice".to_string();
        out.buffer_type = BufferType::Query;
        out.submitted_at = std::time::Instant::now()
            .checked_sub(std::time::Duration::from_secs(301))
            .expect("301s before now");

        app.apply_translate_deliver(TranslateDeliver::Outgoing(Box::new(out)));

        let rows: Vec<String> = app.state.buffers["test/bob"]
            .messages
            .iter()
            .map(|m| m.text.clone())
            .collect();
        assert_eq!(
            rows,
            vec!["linia za bariera".to_string()],
            "the line behind the moved barrier is delivered now, not at expiry"
        );
        assert!(!app.state.translate_queues.contains_key("test/bob"));
    }

    #[test]
    fn a_hand_edited_timeout_below_the_floor_is_clamped_at_sync() {
        // `/set` validates (500 ms floor); a hand-edited config.toml goes
        // through serde with no validation and an unfloored `timeout_ms = 0`
        // reaches the queue tick as Duration::ZERO — every pending line
        // times out on the next tick and every send is refused.
        let mut app = app_with_buffer();
        app.config.translate.timeout_ms = 0;
        app.config.translate.max_in_flight = 0;
        app.config.translate.max_queue = 0;
        app.sync_translate_from_config();
        assert_eq!(app.config.translate.timeout_ms, 500, "the /set floor");
        assert_eq!(app.config.translate.max_in_flight, 1);
        assert_eq!(app.config.translate.max_queue, 1);
    }

    #[test]
    fn a_brokers_error_text_reads_back_verbatim_in_the_refusal_row() {
        // The reason quotes broker-authored text, and provider errors
        // routinely contain `%` ("quota 100% used"). Unescaped it reaches
        // the theme parser as format codes and the row renders mangled.
        let mut app = app_with_buffer();
        app.apply_translate_deliver(TranslateDeliver::Outgoing(Box::new(outgoing(
            "moje zdanie",
            TranslateOutcome::Untranslated {
                id: 1,
                reason: crate::translate::UntranslatedReason::Error(
                    "quota 100% used %N%Z112233".to_string(),
                ),
            },
            false,
        ))));
        let row = app.state.buffers[BUF]
            .messages
            .back()
            .expect("the refusal row");
        assert!(
            as_rendered(&row.text).contains("quota 100% used %N%Z112233"),
            "the broker's words survive the theme parser untouched:\n  {}",
            as_rendered(&row.text)
        );
    }

    #[test]
    fn an_answer_the_client_refused_counts_as_refused_not_translated() {
        // /translate status exists to tell a provider that is down from a
        // filter doing its job. Counting an RPE2E-shaped answer as
        // "translated" reported a healthy provider while every send was
        // being refused.
        let mut app = app_with_buffer();
        app.apply_translate_deliver(TranslateDeliver::Outgoing(Box::new(outgoing(
            "moje zdanie",
            TranslateOutcome::Translated {
                id: 1,
                text: "+RPE2E01 dGFqbmU=".to_string(),
            },
            false,
        ))));
        assert_eq!(app.state.translate_tally.refused, 1);
        assert_eq!(
            app.state.translate_tally.translated, 0,
            "an answer this client would not use is not a success"
        );
    }

    #[test]
    fn a_transport_refusal_still_counts_the_answer_as_translated() {
        // The tally reports the TRANSLATOR's health. A good answer whose
        // SEND failed (connection unavailable here) is still an answer that
        // came back fine — filing it under refused would smear a healthy
        // provider for a flaky connection.
        let mut app = app_with_buffer();
        app.apply_translate_deliver(TranslateDeliver::Outgoing(Box::new(outgoing(
            "moje zdanie",
            TranslateOutcome::Translated {
                id: 1,
                text: "mein satz".to_string(),
            },
            false,
        ))));
        assert_eq!(app.state.translate_tally.translated, 1);
        assert_eq!(app.state.translate_tally.refused, 0);
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
    fn an_echo_written_to_another_buffer_still_frees_the_send_buffers_place() {
        // No caller supplies a divergent `Gated` echo buffer today, but the
        // plan carries its own id so one may. An echo written elsewhere can
        // never fill the place the SEND buffer holds, and nothing later on
        // the success path gives it back — the conversation would sit behind
        // the barrier until the queue's expiry.
        let mut app = app_with_dying_handle(usize::MAX);
        app.conn_generations.insert("test".to_string(), 1);
        app.state
            .add_buffer(Buffer::for_test("test", BufferType::Channel, "#other"));

        let mut queue = TranslateQueue::new();
        queue.reserve(1);
        queue.push_resolved(2, message(2, "odpowiedz kolegi"), ActivityLevel::Activity);
        app.state.translate_queues.insert(BUF.to_string(), queue);

        let mut out = outgoing(
            "moje zdanie",
            TranslateOutcome::Translated {
                id: 1,
                text: "mein satz".to_string(),
            },
            false,
        );
        out.conn_generation = Some(1);
        out.echo = OutgoingEchoPlan::Gated {
            buffer_id: "test/#other".to_string(),
            message_type: MessageType::Message,
            even_without_encryption: true,
        };

        app.apply_translate_deliver(TranslateDeliver::Outgoing(Box::new(out)));

        assert_eq!(
            shown(&app),
            vec!["odpowiedz kolegi".to_string()],
            "the reply behind the barrier is delivered now, not at expiry"
        );
        assert_eq!(queued(&app), 0, "and nothing is left holding the queue open");
        let other: Vec<String> = app.state.buffers["test/#other"]
            .messages
            .iter()
            .map(|m| m.text.clone())
            .collect();
        assert_eq!(
            other,
            vec!["mein satz".to_string()],
            "the echo itself landed where the plan asked"
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
        // otherwise the workers keep retiring permits as they take them, the
        // limiter settles at the ABANDONED 2, and stays there while the
        // config says 4.
        let limiter = TranslateLimiter::new(4);
        let held: Vec<_> = (0..4)
            .map(|_| limiter.permits.clone().try_acquire_owned().expect("permit"))
            .collect();

        limiter.retune(2);
        assert_eq!(
            limiter.debt(),
            2,
            "nothing could be forgotten while every permit is out"
        );

        limiter.retune(4);
        assert_eq!(
            limiter.debt(),
            0,
            "the reduction was abandoned before it ever took effect"
        );

        drop(held);
        limiter.settle();
        assert_eq!(
            limiter.available(),
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
    fn a_long_action_echo_breaks_where_the_wire_actions_broke() {
        // A `/me` pays for its CTCP framing out of the same wire line, so its
        // BODY breaks at a smaller budget than a plain message's. Cutting the
        // echo at the full line budget put row boundaries where the wire never
        // did: for a body between the two budgets the author saw ONE row where
        // peers received TWO actions, and no row's `WireOrigin` named a body
        // any CHATHISTORY replay carries.
        let mut app = app_with_dying_handle(usize::MAX);
        app.conn_generations.insert("test".to_string(), 1);
        let sender = app.irc_handles["test"].sender().clone();
        // Longer than the action body budget, but inside the full line
        // budget — the exact window the two chunkers disagreed over.
        let body = "a".repeat(crate::irc::MESSAGE_MAX_BYTES - 5);
        let mut out = outgoing(
            "krotkie",
            TranslateOutcome::Translated {
                id: 1,
                text: body,
            },
            false,
        );
        out.is_action = true;
        out.conn_generation = Some(1);
        app.state.reserve_echo_slot(BUF, 1);

        app.apply_translate_deliver(TranslateDeliver::Outgoing(Box::new(out)));

        let wire_bodies: Vec<String> = sender
            .captured()
            .iter()
            .map(|m| {
                let line = m.to_string();
                let start = line.find('\u{1}').expect("an action frame");
                let end = line.rfind('\u{1}').expect("a closed action frame");
                line[start..=end]
                    .trim_matches('\u{1}')
                    .strip_prefix("ACTION ")
                    .expect("an ACTION payload")
                    .to_string()
            })
            .collect();
        assert_eq!(
            wire_bodies.len(),
            2,
            "this body must actually split on the wire, or the test proves nothing"
        );

        let echoed: Vec<(String, String)> = app.state.buffers[BUF]
            .messages
            .iter()
            .map(|m| {
                (
                    m.text.clone(),
                    m.wire_origin
                        .as_ref()
                        .expect("an echo row records its wire text")
                        .text
                        .clone(),
                )
            })
            .collect();
        assert_eq!(
            echoed.iter().map(|(t, _)| t.clone()).collect::<Vec<_>>(),
            wire_bodies,
            "the author's rows are the wire bodies, one for one"
        );
        assert_eq!(
            echoed.iter().map(|(_, w)| w.clone()).collect::<Vec<_>>(),
            wire_bodies,
            "and each row is KEYED by the body its wire line carried"
        );
    }

    #[test]
    fn a_translation_shaped_like_e2e_protocol_is_refused_at_the_seam() {
        // `+RPE2E01` and `RPEE2E` are matched as BARE prefixes by every
        // Repartee receive path, on every buffer — that is how a peer's
        // first KEYREQ arrives at all — so no per-buffer E2E setting can
        // defuse text shaped like them. The `\x01` check never sees them.
        // The shipped stub alone can produce one: it reorders words, so
        // "hello RPEE2E" comes back tag-first.
        for proto in ["RPEE2E KEYREQ v=1 c=x p=y", "+RPE2E01 dGFqbmU="] {
            let out = super::single_line_or_refuse(
                1,
                TranslateOutcome::Translated {
                    id: 1,
                    text: proto.to_string(),
                },
            );
            assert!(
                matches!(
                    out,
                    TranslateOutcome::Untranslated {
                        reason: crate::translate::UntranslatedReason::Error(_),
                        ..
                    }
                ),
                "{proto:?} must be refused, not published as prose: {out:?}"
            );
        }
        // Prefix only, exactly as the receive paths match: the tag inside a
        // sentence is somebody talking ABOUT the protocol, and refusing that
        // would eat ordinary conversation.
        let prose = super::single_line_or_refuse(
            1,
            TranslateOutcome::Translated {
                id: 1,
                text: "wspomnial o RPEE2E w rozmowie".to_string(),
            },
        );
        assert!(
            matches!(prose, TranslateOutcome::Translated { .. }),
            "mid-sentence mention is prose: {prose:?}"
        );
    }

    #[test]
    fn an_outcome_answering_another_request_is_refused() {
        // The id is what correlates an answer with the line that asked for
        // it, and the backend is untrusted, so it is checked rather than
        // believed. An outcome carrying somebody else's id would resolve THAT
        // queue slot: one line's translation applied to another — published
        // under the user's nick, in the wrong conversation — while the line
        // it belonged to sits pending until the timeout. Both halves silent.
        let answered = super::single_line_or_refuse(
            7,
            TranslateOutcome::Translated {
                id: 99,
                text: "mein satz".to_string(),
            },
        );
        assert_eq!(answered.id(), 7, "the outcome is re-labelled to the line we asked about");
        assert!(
            matches!(answered, TranslateOutcome::Untranslated { .. }),
            "and it is not used as a translation: {answered:?}"
        );

        // An UNTRANSLATED outcome with a foreign id is just as wrong — it
        // would mark the wrong line as a gap.
        let mislabelled = super::single_line_or_refuse(
            7,
            TranslateOutcome::Untranslated {
                id: 99,
                reason: UntranslatedReason::Filtered,
            },
        );
        assert_eq!(mislabelled.id(), 7);
    }

    #[test]
    fn a_retry_is_re_addressed_when_its_query_was_renamed() {
        // `/msg frank secret` typed elsewhere renders a retry of
        // `/msg frank secret` at dispatch. If frank renames while the
        // translation runs and it then fails, `deferred_retry_text` hands
        // that string straight back whenever the author is looking at the
        // conversation — now keyed under the NEW id. Pressing Enter would
        // send the private text to whoever holds `frank` by then.
        let mut app = app_with_buffer();
        app.state
            .add_buffer(Buffer::for_test("test", BufferType::Query, "frank"));
        let mut out = outgoing(
            "secret",
            TranslateOutcome::Untranslated {
                id: 1,
                reason: UntranslatedReason::Timeout,
            },
            false,
        );
        out.buffer_id = "test/frank".to_string();
        out.buffer_name = "frank".to_string();
        out.retry_text = "/msg frank secret".to_string();
        out.retry_body = "secret".to_string();
        out.submitted_at = dispatched_long_ago();

        app.state
            .add_buffer(Buffer::for_test("test", BufferType::Query, "frankie"));
        app.state.rekey_buffer_state("test/frank", "test/frankie");

        assert_eq!(
            app.redirect_outgoing_deliver(&mut out),
            RedirectVerdict::Proceed
        );
        assert_eq!(
            out.retry_text, "/msg frankie secret",
            "the retry follows the conversation, and stops naming a nick its \
             owner no longer answers to"
        );

        // And the composer really is handed the re-addressed form.
        app.state.set_active_buffer("test/frankie");
        assert_eq!(
            app.deferred_retry_text(&out).as_deref(),
            Some("/msg frankie secret")
        );
    }

    #[test]
    fn an_action_retry_is_left_alone_by_a_rename() {
        // `/me` acts on the active buffer and names nobody, so there is no
        // stale nick in it to rewrite — and rewriting it would turn an
        // action into a plain message.
        let mut app = app_with_buffer();
        app.state
            .add_buffer(Buffer::for_test("test", BufferType::Query, "frank"));
        let mut out = outgoing(
            "wzdycha",
            TranslateOutcome::Untranslated {
                id: 1,
                reason: UntranslatedReason::Timeout,
            },
            false,
        );
        out.is_action = true;
        out.buffer_id = "test/frank".to_string();
        out.buffer_name = "frank".to_string();
        out.retry_text = "/me wzdycha".to_string();
        out.retry_body = "wzdycha".to_string();
        out.submitted_at = dispatched_long_ago();

        app.state
            .add_buffer(Buffer::for_test("test", BufferType::Query, "frankie"));
        app.state.rekey_buffer_state("test/frank", "test/frankie");
        app.redirect_outgoing_deliver(&mut out);

        assert_eq!(out.retry_text, "/me wzdycha");
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
            crate::state::BufferRedirect::MovedTo("test/frankie"),
            "work from before the rename belongs to the peer who moved — \
             even though a live buffer now sits under the old id"
        );
        assert_eq!(
            app.state
                .redirected_buffer_id("test/frank", std::time::Instant::now()),
            crate::state::BufferRedirect::Stays,
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
            crate::state::BufferRedirect::Stays,
            "a message to whoever holds `frank` NOW must not follow the peer \
             who left before it was sent"
        );
        // The redirect still works for what it was made for.
        assert_eq!(
            app.state
                .redirected_buffer_id("test/frank", dispatched_long_ago()),
            crate::state::BufferRedirect::MovedTo("test/frankie2"),
            "and work from before the first rename still follows the peer, \
             all the way to where they are now"
        );
    }

    #[test]
    fn a_reused_nick_does_not_inherit_the_previous_owners_redirect() {
        // Three actors, and the leak the era list exists to stop:
        //
        //   t0  the user sends alice a translated private message
        //   t1  alice renames to alicia
        //   t2  bob claims the freed nick `alice`
        //   t3  bob renames to bobby
        //
        // Both renames are off `test/alice`. With one mapping per id the
        // second overwrites the first, and the t0 message — still in the
        // translator at t3 — resolves to `test/bobby`. The user's private
        // message to alice is then handed to bob.
        let mut app = app_with_buffer();
        let sent_to_alice = std::time::Instant::now();
        app.state
            .add_buffer(Buffer::for_test("test", BufferType::Query, "alice"));
        app.state.rekey_buffer_state("test/alice", "test/alicia");

        let sent_to_bob = std::time::Instant::now();
        app.state
            .add_buffer(Buffer::for_test("test", BufferType::Query, "alice"));
        app.state.rekey_buffer_state("test/alice", "test/bobby");

        assert_eq!(
            app.state.redirected_buffer_id("test/alice", sent_to_alice),
            crate::state::BufferRedirect::MovedTo("test/alicia"),
            "a message dispatched to alice follows ALICE, not whoever \
             occupied her nick afterwards"
        );
        assert_eq!(
            app.state.redirected_buffer_id("test/alice", sent_to_bob),
            crate::state::BufferRedirect::MovedTo("test/bobby"),
            "and one dispatched while bob held the nick follows bob"
        );
        assert_eq!(
            app.state
                .redirected_buffer_id("test/alice", std::time::Instant::now()),
            crate::state::BufferRedirect::Stays,
            "with nothing holding it now, anything sent since stays put"
        );
    }

    #[test]
    fn a_send_that_outlived_the_redirect_history_is_refused() {
        // `translate.timeout_ms` has no upper bound, so a request can stay in
        // flight longer than the five minutes of rename history we keep. Once
        // it has, "no era covers this" no longer means "the conversation
        // never moved" — it means we cannot tell. Sending under the recorded
        // NAME on that answer hands a private message to whoever holds the
        // abandoned nick.
        let app = app_with_buffer();
        let long_ago = std::time::Instant::now()
            .checked_sub(std::time::Duration::from_secs(600))
            .expect("600s before now");

        assert_eq!(
            app.state.redirected_buffer_id("test/frank", long_ago),
            crate::state::BufferRedirect::Unknown,
            "past the history we keep, the question has no answer"
        );
        assert_eq!(
            app.state
                .redirected_buffer_id("test/frank", std::time::Instant::now()),
            crate::state::BufferRedirect::Stays,
            "inside it, no record really does mean it never moved"
        );
    }

    #[test]
    fn an_unanswerable_redirect_refuses_the_send_rather_than_guessing() {
        let mut app = app_with_buffer();
        app.state
            .add_buffer(Buffer::for_test("test", BufferType::Query, "frank"));
        let mut out = outgoing(
            "sekret",
            TranslateOutcome::Translated {
                id: 1,
                text: "geheim".to_string(),
            },
            false,
        );
        out.buffer_id = "test/frank".to_string();
        out.buffer_name = "frank".to_string();
        out.buffer_type = BufferType::Query;
        out.submitted_at = std::time::Instant::now()
            .checked_sub(std::time::Duration::from_secs(600))
            .expect("600s before now");

        assert!(
            matches!(
                app.redirect_outgoing_deliver(&mut out),
                RedirectVerdict::Refuse(_)
            ),
            "an unanswerable redirect must not fall through to the old name"
        );
    }

    #[test]
    fn a_recipient_who_quit_mid_translation_gets_nothing_sent_to_their_nick() {
        // The nick was freed the moment they quit and belongs to whoever
        // asked for it next. Nothing points anywhere — a quit is not a move,
        // so the redirect machinery has no era to offer and would answer
        // `Stays`, which here means "send it to that name" and is exactly the
        // wrong answer.
        let mut app = app_with_buffer();
        app.state
            .add_buffer(Buffer::for_test("test", BufferType::Query, "frank"));
        let mut out = outgoing(
            "sekret",
            TranslateOutcome::Translated {
                id: 1,
                text: "geheim".to_string(),
            },
            false,
        );
        out.buffer_id = "test/frank".to_string();
        out.buffer_name = "frank".to_string();
        out.buffer_type = BufferType::Query;

        // Nobody has left yet: this is an ordinary send.
        assert_eq!(
            app.redirect_outgoing_deliver(&mut out),
            RedirectVerdict::Proceed,
            "precondition: without a departure it goes"
        );

        app.state.note_query_departure("test/frank");
        assert!(
            matches!(
                app.redirect_outgoing_deliver(&mut out),
                RedirectVerdict::Refuse(_)
            ),
            "they left while it was being translated — the name is no longer \
             proof of who receives it"
        );

        // The boundary, stated on purpose: a departure from BEFORE the send
        // was written is not this mechanism's to refuse. Typing into a query
        // whose peer had already gone is the same gamble with or without
        // translation, and an immediate client sends it too.
        out.submitted_at = std::time::Instant::now();
        assert_eq!(
            app.redirect_outgoing_deliver(&mut out),
            RedirectVerdict::Proceed
        );

        // And a channel is not a nick: nobody can claim `#dupa`.
        let mut chan = outgoing(
            "moje zdanie",
            TranslateOutcome::Translated {
                id: 2,
                text: "mein satz".to_string(),
            },
            false,
        );
        app.state.note_query_departure(BUF);
        assert_eq!(
            app.redirect_outgoing_deliver(&mut chan),
            RedirectVerdict::Proceed
        );
    }

    #[test]
    fn a_channel_send_is_not_refused_for_a_redirect_that_could_never_exist() {
        // Only `rename_query_buffers` ever re-keys a buffer, and it skips
        // everything that is not a Query — so no channel id has ever had a
        // redirect era. Past the five-minute horizon `redirected_buffer_id`
        // answers `Unknown` for want of history rather than for want of an
        // answer, and refusing on it threw away a channel line that was
        // always safe to send.
        //
        // Reachable for any `translate.timeout_ms` above that horizon, which
        // nothing forbids: the send's own deadline is the timeout, so a
        // translation back at six minutes on a ten-minute budget is still
        // inside every rule the user configured.
        let app = app_with_buffer();
        let mut out = outgoing(
            "moje zdanie",
            TranslateOutcome::Translated {
                id: 1,
                text: "mein satz".to_string(),
            },
            false,
        );
        out.submitted_at = std::time::Instant::now()
            .checked_sub(std::time::Duration::from_secs(600))
            .expect("600s before now");
        assert_eq!(
            app.state
                .redirected_buffer_id(&out.buffer_id, out.submitted_at),
            crate::state::BufferRedirect::Unknown,
            "precondition: the horizon really has passed"
        );

        assert_eq!(
            app.redirect_outgoing_deliver(&mut out),
            RedirectVerdict::Proceed,
            "#dupa is #dupa — nobody can claim it out from under the send"
        );
        assert_eq!(out.buffer_id, BUF, "and it is still addressed where it was");

        // The same wait on a QUERY stays fail-closed: a nick can change hands
        // and this one is unaccounted for.
        out.buffer_type = BufferType::Query;
        out.buffer_id = "test/frank".to_string();
        out.buffer_name = "frank".to_string();
        assert!(
            matches!(
                app.redirect_outgoing_deliver(&mut out),
                RedirectVerdict::Refuse(_)
            ),
            "a private message may not be addressed to an untraceable nick"
        );
    }

    #[test]
    fn the_day_separator_goes_through_the_queue_when_there_is_one() {
        // Asserted through `check_day_changed` and not through the helper it
        // calls: the helper working proves nothing if the separator path does
        // not use it.
        let mut app = app_with_buffer();
        app.state.reserve_echo_slot(BUF, 1);
        app.last_day = chrono::Local::now()
            .date_naive()
            .pred_opt()
            .expect("yesterday");

        app.check_day_changed();

        assert_eq!(
            app.state.buffers[BUF].messages.len(),
            0,
            "the separator must not render above the line queued before it"
        );
        assert_eq!(
            app.state.translate_queues[BUF].len(),
            2,
            "it took its place behind the reservation instead"
        );
    }

    #[test]
    fn a_disconnect_clears_state_for_conversations_with_no_window() {
        // A queue or a reflection record routinely outlives its window: a
        // send to a conversation that was never opened, or one whose query
        // was closed while the translation ran. Deciding ownership by looking
        // up a live buffer skipped exactly those, so a reconnect inside the
        // record's TTL that resent the same text consumed the STALE record —
        // taking the old reserved id with it and leaving the new reservation
        // blocking the buffer until it timed out.
        let mut app = app_with_buffer();
        app.state.reserve_echo_slot("test/ghost", 1);
        app.state.decorate_own_echo(
            "test/ghost",
            crate::state::AppState::own_echo_decoration(
                "mein satz".to_string(),
                1,
                None,
                true,
            ),
        );
        assert!(
            !app.state.buffers.contains_key("test/ghost"),
            "precondition: no window for this conversation"
        );

        app.flush_translate_queues_for_connection("test");

        assert!(
            !app.state.translate_queues.contains_key("test/ghost"),
            "the queue dies with the session"
        );
        assert!(
            !app.state.own_echo_decorations.contains_key("test/ghost"),
            "and so does the record that would have matched a resend"
        );
    }

    #[test]
    fn a_disconnect_leaves_another_connection_alone() {
        // Ownership by key prefix must not become "everything".
        let mut app = app_with_buffer();
        app.state.reserve_echo_slot("other/#chan", 1);

        app.flush_translate_queues_for_connection("test");

        assert!(
            app.state.translate_queues.contains_key("other/#chan"),
            "a different connection's work is untouched"
        );
    }

    #[test]
    fn closing_a_query_does_not_lose_track_of_a_send_still_in_flight() {
        // A translated private message is in the worker; the user closes the
        // query; the peer then changes nick in a shared channel. With the
        // window gone there is no query buffer for the NICK handler to
        // re-key, so nothing recorded that the conversation moved — and the
        // send went out addressed to the abandoned nick, which somebody else
        // may already hold.
        let mut app = app_with_buffer();
        app.state
            .add_buffer(Buffer::for_test("test", BufferType::Query, "frank"));
        app.state.note_outgoing_dispatch("test/frank");
        let submitted = std::time::Instant::now();

        app.state.remove_buffer("test/frank");
        assert!(
            app.state
                .has_outgoing_in_flight("test/frank", std::time::Duration::from_secs(60)),
            "closing the window does not recall the message"
        );

        // The peer renames somewhere we can still see them.
        crate::irc::events::rename_query_buffers_for_test(
            &mut app.state,
            "test",
            "frank",
            "frankie",
            &[],
        );

        assert_eq!(
            app.state.redirected_buffer_id("test/frank", submitted),
            crate::state::BufferRedirect::MovedTo("test/frankie"),
            "the rename is recorded even with no window to move"
        );

        // And the delivery path refuses rather than addressing `frank`: the
        // conversation moved somewhere that has no window either.
        let mut out = outgoing(
            "sekret",
            TranslateOutcome::Translated {
                id: 1,
                text: "geheim".to_string(),
            },
            false,
        );
        out.buffer_id = "test/frank".to_string();
        out.buffer_name = "frank".to_string();
        out.buffer_type = BufferType::Query;
        out.submitted_at = submitted;
        assert!(matches!(
            app.redirect_outgoing_deliver(&mut out),
            RedirectVerdict::Refuse(_)
        ));
    }

    #[test]
    fn a_reused_nick_that_renames_twice_still_separates_the_eras() {
        // As above, but bob renames again: alice's era must follow ALICE
        // through her own chain while bob's follows his, with neither
        // repointing the other.
        let mut app = app_with_buffer();
        let sent_to_alice = std::time::Instant::now();
        app.state
            .add_buffer(Buffer::for_test("test", BufferType::Query, "alice"));
        app.state.rekey_buffer_state("test/alice", "test/alicia");
        let sent_to_bob = std::time::Instant::now();
        app.state.rekey_buffer_state("test/alice", "test/bobby");
        app.state.rekey_buffer_state("test/alicia", "test/alicja");
        app.state.rekey_buffer_state("test/bobby", "test/robert");

        assert_eq!(
            app.state.redirected_buffer_id("test/alice", sent_to_alice),
            crate::state::BufferRedirect::MovedTo("test/alicja"),
            "alice's era follows alice to her latest nick, in one hop"
        );
        assert_eq!(
            app.state.redirected_buffer_id("test/alice", sent_to_bob),
            crate::state::BufferRedirect::MovedTo("test/robert"),
            "bob's era follows bob to his, independently"
        );
    }

    /// An app with `frank` configured for incoming translation.
    fn app_with_translated_query() -> crate::app::App {
        let mut app = app_with_buffer();
        app.config.translate.enabled = true;
        app.config.translate.backend = "stub".to_string();
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
        app
    }

    #[test]
    fn a_rekeyed_query_keeps_translating_after_a_sync() {
        // `sync_translate_from_config` re-derives the state mirror from the
        // config, so moving only the mirror is undone by the next `/set` or
        // `/reload` — and translation would silently stop mid-conversation
        // because the person on the other end typed `/nick`.
        let mut app = app_with_translated_query();
        app.state
            .pending_buffer_rekeys
            .push(("test/frank".to_string(), "test/frankie".to_string()));

        app.drain_pending_buffer_rekeys();

        assert!(
            app.state.translate_buffers.contains_key("test/frankie"),
            "the conversation is followed"
        );
        // …and STILL followed after every re-derive, which is the whole point.
        app.sync_translate_from_config();
        assert!(app.state.translate_buffers.contains_key("test/frankie"));
        assert!(!app.state.translate_buffers.contains_key("test/frank"));
    }

    #[test]
    fn following_a_nick_change_never_rewrites_what_the_user_typed() {
        // The obvious way to follow a rename is to move the key inside
        // `config.translate.buffers` — and that quietly makes it permanent,
        // because `/set` and half a dozen admin commands save the WHOLE
        // config. A peer who is `frankie` for five minutes one afternoon
        // would then own the user's setting for good, and after a restart
        // translation would follow a nick nobody chose. The documented
        // behaviour is the opposite: followed this session, saved under the
        // name that was typed.
        let mut app = app_with_translated_query();
        app.state
            .pending_buffer_rekeys
            .push(("test/frank".to_string(), "test/frankie".to_string()));
        app.drain_pending_buffer_rekeys();

        assert!(
            app.config.translate.buffers.contains_key("test/frank"),
            "what goes to disk still names the nick the user wrote"
        );
        assert!(
            !app.config.translate.buffers.contains_key("test/frankie"),
            "and nothing was written for a nick they never typed"
        );

        // A second rename moves the same setting again rather than chaining.
        app.state
            .pending_buffer_rekeys
            .push(("test/frankie".to_string(), "test/franek".to_string()));
        app.drain_pending_buffer_rekeys();
        assert_eq!(app.translate_follows.len(), 1, "one entry, retargeted");
        assert!(app.state.translate_buffers.contains_key("test/franek"));
        assert!(app.config.translate.buffers.contains_key("test/frank"));

        // Somebody else renaming ONTO the nick a followed conversation left
        // must not inherit its settings.
        app.state
            .pending_buffer_rekeys
            .push(("test/other".to_string(), "test/franek".to_string()));
        app.config.translate.buffers.insert(
            "test/other".to_string(),
            crate::config::TranslateBufferConfig::default(),
        );
        app.drain_pending_buffer_rekeys();
        assert_eq!(
            app.translate_follows,
            vec![("test/other".to_string(), "test/franek".to_string())],
            "the conversation that lost the nick stops being followed onto it"
        );
    }

    #[test]
    fn a_renamed_peer_is_still_translated_in_both_directions() {
        // The config keeps the key the user typed and only the mirror follows
        // the conversation — so every runtime gate has to read the mirror. The
        // incoming gate did; the outgoing one asked the config, found nothing
        // under the new id, and reported `NotApplicable`, which is the
        // ORDINARY SEND path. The user's line went to the channel in their own
        // language, with no refusal, no marker and nothing on screen to say
        // translation had stopped — because somebody else typed `/nick`.
        let mut app = app_with_buffer();
        app.state
            .add_buffer(Buffer::for_test("test", BufferType::Query, "frank"));
        app.config.translate.enabled = true;
        app.config.translate.backend = "stub".to_string();
        app.translate_backend = Some(std::sync::Arc::new(
            crate::translate::backend::StubBackend::new(0, 0),
        ));
        app.config.translate.my_lang = "pl".to_string();
        app.config.translate.buffers.insert(
            "test/frank".to_string(),
            crate::config::TranslateBufferConfig {
                incoming: true,
                outgoing: true,
                lang: Some("de".to_string()),
                my_lang: None,
            },
        );
        app.sync_translate_from_config();
        assert_eq!(
            app.outgoing_translate_policy("test/frank", "moje zdanie", false),
            OutgoingTranslatePolicy::Translate,
            "precondition: it translates before the rename"
        );

        // frank becomes frankie.
        app.state
            .add_buffer(Buffer::for_test("test", BufferType::Query, "frankie"));
        app.state.rekey_buffer_state("test/frank", "test/frankie");
        app.state
            .pending_buffer_rekeys
            .push(("test/frank".to_string(), "test/frankie".to_string()));
        app.drain_pending_buffer_rekeys();

        assert_eq!(
            app.outgoing_translate_policy("test/frankie", "moje zdanie", false),
            OutgoingTranslatePolicy::Translate,
            "the conversation moved; what the user asked for did not"
        );
        let built = app
            .build_outgoing_translate(&OutgoingRequest {
                conn_id: "test",
                buffer_id: "test/frankie",
                buffer_name: "frankie",
                buffer_type: &BufferType::Query,
                nick: "me",
                text: "moje zdanie",
                is_action: false,
                echo: OutgoingEchoPlan::BufferInput,
            })
            .expect("the request is still built for the renamed query");
        assert_eq!(built.req.target_lang, "de");
        assert_eq!(built.req.source_lang.as_deref(), Some("pl"));
    }

    #[test]
    fn configuring_a_followed_conversation_moves_the_setting_for_good() {
        // `/translate add*|del*` on the conversation IS the user renaming the
        // setting: from there it is keyed by the nick they are looking at and
        // written that way. Without this the two disagree in the worst
        // direction — `delin` removes an entry the follow puts straight back,
        // and translation keeps running on a conversation just switched off.
        let mut app = app_with_translated_query();
        app.state
            .pending_buffer_rekeys
            .push(("test/frank".to_string(), "test/frankie".to_string()));
        app.drain_pending_buffer_rekeys();

        app.materialize_translate_follow("test/frankie");

        assert!(
            app.config.translate.buffers.contains_key("test/frankie"),
            "now it is theirs under this name"
        );
        assert!(!app.config.translate.buffers.contains_key("test/frank"));
        assert!(app.translate_follows.is_empty(), "and nothing is followed");

        // Which is what makes switching it off actually switch it off.
        app.config.translate.buffers.remove("test/frankie");
        app.sync_translate_from_config();
        assert!(!app.state.translate_buffers.contains_key("test/frankie"));
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
    fn a_partial_send_leaves_the_only_copy_of_the_text_readable() {
        // This row is the only copy there will ever be — the composer restore
        // is deliberately skipped here so the user cannot republish the chunks
        // already on the channel. Every other refusal has the composer as a
        // second chance whenever it happens to be empty; this one has none, so
        // a row that silently eats `%i` and the `5` from `$5` is the whole
        // recovery gone.
        let mut app = app_with_dying_handle(1);
        app.state.reserve_echo_slot(BUF, 1);
        let long = "wieloslowne zdanie ".repeat(40);
        let mut out = outgoing(
            TREACHEROUS,
            TranslateOutcome::Translated {
                id: 1,
                text: long.trim().to_string(),
            },
            false,
        );
        out.retry_text = TREACHEROUS.to_string();
        app.apply_translate_deliver(TranslateDeliver::Outgoing(Box::new(out)));

        assert!(
            app.input.value.is_empty(),
            "precondition: nothing is restored on this path"
        );
        let row = app.state.buffers[BUF]
            .messages
            .iter()
            .map(|m| as_rendered(&m.text))
            .find(|t| t.contains("Part of the message was sent"))
            .expect("the partial-send row");
        assert!(
            row.contains(TREACHEROUS),
            "the row renders as something other than what was typed:\n  {row}"
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
        app.sync_translate_from_config();
    }

    /// Configure one buffer id for translation in both directions.
    ///
    /// Through the config AND the sync, exactly as `/translate addin` does:
    /// every runtime gate reads the mirror `sync_translate_from_config`
    /// derives, so a fixture that writes only the config is testing a state
    /// the client is never in.
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
        app.sync_translate_from_config();
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
        app.sync_translate_from_config();

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
    ///
    /// Built the way the client builds it — a named backend, then a sync —
    /// rather than by poking `translate_active`: the sync is what derives
    /// every mirror the gates read, and a fixture that sets one by hand can
    /// pass while the real path is broken.
    fn app_with_outgoing(lang: Option<&str>) -> crate::app::App {
        let mut app = app_with_buffer();
        app.state.set_active_buffer(BUF);
        app.config.translate.enabled = true;
        app.config.translate.backend = "stub".to_string();
        app.translate_backend = Some(std::sync::Arc::new(
            crate::translate::backend::StubBackend::new(0, 0),
        ));
        app.config.translate.my_lang = "pl".to_string();
        set_buffer_langs(&mut app, lang, None);
        assert!(app.state.translate_active, "fixture: translation is live");
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
        app.sync_translate_from_config();
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
        // Every real web submission records its buffer before the submit
        // runs (`confirm_web_buffer`); the restore is bound to that record.
        app.web_active_buffers
            .insert("alice-session".to_string(), BUF.to_string());
        let mut rx = app.web_broadcaster.subscribe();

        app.refuse_untranslatable_send("moje zdanie", "the translation queue is full");

        assert!(
            app.input.value.is_empty(),
            "the terminal input must not be touched"
        );
        let (text, session, buffer) = match rx.try_recv().expect("an event was broadcast") {
            crate::web::protocol::WebEvent::RestoreInput {
                text,
                session_id,
                buffer_id,
            } => (text, session_id, buffer_id),
            other => panic!("expected RestoreInput, got {other:?}"),
        };
        assert_eq!(text, "moje zdanie");
        assert_eq!(session.as_deref(), Some("alice-session"));
        assert_eq!(
            buffer.as_deref(),
            Some(BUF),
            "bound to the composer it belongs in, so a tab that moved on \
             cannot arm another conversation with it"
        );
    }

    #[test]
    fn a_web_restore_with_no_recorded_buffer_is_withheld_entirely() {
        // No record means no destination the retry form is known to be safe
        // for. The error row carries the text, so withholding loses nothing;
        // broadcasting without a binding would have the client restore into
        // whatever composer happens to be active.
        let mut app = app_with_outgoing(Some("de"));
        app.submit_origin = SubmitOrigin::Web("alice-session".to_string());
        let mut rx = app.web_broadcaster.subscribe();

        app.refuse_untranslatable_send("moje zdanie", "the translation queue is full");

        assert!(
            !std::iter::from_fn(|| rx.try_recv().ok()).any(|ev| matches!(
                ev,
                crate::web::protocol::WebEvent::RestoreInput { .. }
            )),
            "nothing is restored when the session's buffer is unknown"
        );
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
    fn a_split_translated_echo_keeps_one_row_per_wire_line() {
        // `show_original_out` appends ` [original]` to the echo, so a message
        // near the wire budget splits on DISPLAY while the wire itself did
        // not, or split elsewhere. Chunking the composed display meant no
        // chunk's text was ever a wire text: each row lost `wire_origin`, so
        // the suffix could not be dimmed and — worse — `dedup_text` fell back
        // to keying the row by what is shown. A later CHATHISTORY replay
        // brings back the wire lines, matches none of them, and splices a
        // second untranslated copy of the message in beside the first.
        let mut app = app_with_dying_handle(usize::MAX);
        let mut conn = crate::app::input::submit_typing_tests::make_connection();
        conn.id = "test".to_string();
        conn.nick = "me".to_string();
        app.state.add_connection(conn);
        app.state.reserve_echo_slot(BUF, 1);

        // Long enough that the translation alone needs two wire lines.
        let translated = "wieloslowne zdanie ".repeat(40).trim().to_string();
        let mut out = outgoing(
            "krotki oryginal",
            TranslateOutcome::Translated {
                id: 1,
                text: translated.clone(),
            },
            true, // show_original_out
        );
        out.echo_id = 1;
        app.apply_translate_deliver(TranslateDeliver::Outgoing(Box::new(out)));

        let rows: Vec<_> = app.state.buffers[BUF].messages.iter().collect();
        assert!(rows.len() > 1, "precondition: this split");
        assert!(
            rows.iter().all(|m| m.wire_origin.is_some()),
            "every row is a wire line, so every row can say which one"
        );

        // The rows ARE the wire lines — the same split the send path used,
        // not a second one taken over the composed display.
        let wire_texts: Vec<&str> = rows
            .iter()
            .filter_map(|m| m.wire_origin.as_ref())
            .map(|o| o.text.as_str())
            .collect();
        let wire_split =
            crate::irc::split_irc_message(&translated, crate::irc::MESSAGE_MAX_BYTES);
        assert_eq!(
            wire_texts, wire_split,
            "the rows are the wire lines, one for one"
        );

        // Only the LAST carries the original — the same rule the reflection
        // path follows, and for the same reason.
        let with_suffix: Vec<usize> = rows
            .iter()
            .enumerate()
            .filter(|(_, m)| m.wire_origin.as_ref().is_some_and(|o| o.suffix_at.is_some()))
            .map(|(i, _)| i)
            .collect();
        assert_eq!(
            with_suffix,
            vec![rows.len() - 1],
            "the original is appended once, to the last row"
        );
        assert!(
            rows.last().expect("rows").text.contains("[krotki oryginal]"),
            "and it is there: {:?}",
            rows.last().expect("rows").text
        );
    }

    #[test]
    fn a_deferred_echo_is_attributed_to_the_nick_the_message_went_out_under() {
        // The wire message carries no sender: the SERVER stamps the prefix as
        // it relays the line, using whatever nick the connection holds then.
        // The send happens AFTER the translation, so a `/nick` during the wait
        // — and the wait is exactly when a user has time for one — means the
        // message went out under the new name and every peer saw that. An echo
        // carrying the nick captured at dispatch attributes the line, in the
        // buffer and in the log, to a name nobody received it from.
        //
        // No echo-message here, so this local row is the only copy the author
        // ever sees.
        let mut app = app_with_dying_handle(usize::MAX);
        let mut conn = crate::app::input::submit_typing_tests::make_connection();
        conn.id = "test".to_string();
        conn.nick = "alice".to_string();
        app.state.add_connection(conn);
        app.state.reserve_echo_slot(BUF, 1);

        let mut out = outgoing("moje zdanie", TranslateOutcome::Translated {
            id: 1,
            text: "mein satz".to_string(),
        }, false);
        out.nick = "alice".to_string(); // captured when Enter was pressed
        out.echo_id = 1;

        // `/nick bob` lands, confirmed by the server, while the translation
        // is still out.
        app.state
            .connections
            .get_mut("test")
            .expect("the connection")
            .nick = "bob".to_string();

        app.apply_translate_deliver(TranslateDeliver::Outgoing(Box::new(out)));

        let row = app.state.buffers[BUF]
            .messages
            .back()
            .expect("the local echo");
        assert_eq!(
            row.nick.as_deref(),
            Some("bob"),
            "the row names who the message was actually published as"
        );
    }

    #[test]
    fn retyping_a_message_whose_echo_was_lost_does_not_wedge_the_buffer() {
        // The natural reaction to a message that never appeared is to send it
        // again — so a lost reflection and a repeat of the same text are not
        // independent events, they are cause and effect.
        //
        // The reservation for the first send times out after `timeout_ms`
        // (5s by default) but the record filed for it stayed matchable for
        // `OWN_ECHO_TTL` (30s). In that gap the SECOND send's reflection
        // matches the FIRST send's record — matching is by wire text, oldest
        // first — so it is decorated with the wrong original and, worse, tries
        // to fill a slot that no longer exists. The second reservation, which
        // it should have filled, is left barricading the buffer for another
        // full timeout.
        let mut app = app_with_echo_message();
        app.config.translate.timeout_ms = 5_000;
        let mut queue = TranslateQueue::new();
        queue.reserve_at(
            1,
            std::time::Instant::now()
                .checked_sub(std::time::Duration::from_secs(6))
                .expect("six seconds ago"),
        );
        app.state.translate_queues.insert(BUF.to_string(), queue);
        send_translated(&mut app, 1, "moje zdanie", "mein satz");

        // The reflection never comes. The sweep gives the place back — and
        // must forget the record that was waiting to fill it.
        app.tick_translate_queues();

        // The user retypes the same thing. Same wire text, a new place held.
        let mut queue = TranslateQueue::new();
        queue.reserve(2);
        app.state.translate_queues.insert(BUF.to_string(), queue);
        send_translated(&mut app, 2, "inne zdanie", "mein satz");

        // A reply arrives and finishes translating while the second send is
        // still out — it is what the barrier is holding back.
        {
            let queue = app
                .state
                .translate_queues
                .get_mut(BUF)
                .expect("the second reservation keeps the queue alive");
            queue.push_pending(9, "antwort".to_string(), payload(9, "antwort"));
            queue.resolve(9, Ok("odpowiedz".to_string()));
        }

        reflect(&mut app, "mein satz", "server-M2");

        let rows = shown(&app);
        assert_eq!(
            rows,
            vec![
                "mein satz [inne zdanie]".to_string(),
                "odpowiedz".to_string(),
            ],
            "the reflection fills the place held by the send it belongs to, \
             carries THAT send's original, and the reply behind it drains: {rows:?}"
        );
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
        let restored: Vec<(String, Option<String>, Option<String>)> =
            std::iter::from_fn(|| rx.try_recv().ok())
                .filter_map(|ev| match ev {
                    crate::web::protocol::WebEvent::RestoreInput {
                        text,
                        session_id,
                        buffer_id,
                    } => Some((text, session_id, buffer_id)),
                    _ => None,
                })
                .collect();
        assert_eq!(
            restored,
            // The bare body, because `/msg`ing the buffer you are already in
            // retries correctly as plain text — see `retry_form_for`.
            vec![(
                "moje zdanie".to_string(),
                Some("alice-session".to_string()),
                // Bound to the buffer the form was computed for, so a tab
                // that switches away mid-flight shows it only in the error
                // row instead of arming another composer with it.
                Some(BUF.to_string()),
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
        app.translate_in_flight = Some(Arc::new(TranslateLimiter::new(4)));
        app.config.translate.max_in_flight = 9;
        app.sync_translate_from_config();
        let limiter = app.translate_in_flight.as_ref().unwrap();
        assert_eq!(limiter.available(), 9);
        assert_eq!(limiter.effective(), 9);
    }

    #[test]
    fn lowering_max_in_flight_takes_effect_without_a_restart() {
        let mut app = app_with_buffer();
        app.translate_in_flight = Some(Arc::new(TranslateLimiter::new(8)));
        app.config.translate.max_in_flight = 2;
        app.sync_translate_from_config();
        assert_eq!(app.translate_in_flight.as_ref().unwrap().available(), 2);
    }

    #[test]
    fn a_reduction_paid_by_the_workers_is_not_applied_a_second_time() {
        // The regression this whole type exists to prevent.
        //
        // A worker paying off a unit of debt retires a real permit. If the
        // running total is not retired with it, the effective ceiling reads
        // back as the ORIGINAL value once the debt is clear — and the next
        // `sync_translate_from_config`, which is every /set, /reload and
        // /translate add*, applies the same reduction again. Two rounds of
        // that forget every permit and translation stops for good.
        let limiter = TranslateLimiter::new(4);
        let held: Vec<_> = (0..4)
            .map(|_| limiter.permits.clone().try_acquire_owned().expect("permit"))
            .collect();

        limiter.retune(2);
        assert_eq!(limiter.debt(), 2, "nothing available, so it is all owed");

        // The workers pay it off as they take permits.
        drop(held);
        limiter.settle();
        assert_eq!(limiter.debt(), 0, "settled from the idle limiter");
        assert_eq!(limiter.available(), 2);
        assert_eq!(
            limiter.effective(),
            2,
            "the ceiling must read back as what was asked for, not as what \
             it was before the reduction"
        );

        // The next config sync asks for the same 2 again.
        limiter.retune(2);
        assert_eq!(
            limiter.available(),
            2,
            "asking for the value already in force must change nothing"
        );
        limiter.retune(2);
        assert_eq!(limiter.available(), 2, "and must stay changing nothing");
    }

    #[tokio::test]
    async fn a_reduction_lands_even_while_the_limiter_stays_busy() {
        // The case `forget_permits` alone can never reach. Tokio hands a
        // returned permit STRAIGHT to the next waiter, so while anything is
        // queued for a permit none is ever "available" — and a lowered
        // `max_in_flight` would go on being ignored for as long as the
        // traffic lasts, which is exactly when the provider's cap matters.
        //
        // Here every permit is out and two more acquirers are already
        // waiting, so the semaphore never once goes idle. The reduction has
        // to be applied by the workers as they take permits.
        let limiter = Arc::new(TranslateLimiter::new(4));
        let held: Vec<_> = (0..4)
            .map(|_| limiter.permits.clone().try_acquire_owned().expect("permit"))
            .collect();
        assert_eq!(limiter.available(), 0, "precondition: fully checked out");

        limiter.retune(2);
        assert_eq!(limiter.debt(), 2, "the whole reduction is owed");

        // Two workers are already queued behind the limiter, so each permit
        // released below is handed to one of them rather than becoming
        // available — which is what defeats `forget_permits`.
        let waiters: Vec<_> = (0..2)
            .map(|_| {
                let limiter = Arc::clone(&limiter);
                tokio::spawn(async move { limiter.acquire().await })
            })
            .collect();
        tokio::task::yield_now().await;

        drop(held);
        for waiter in waiters {
            // Dropped here on purpose: the permit goes back to the limiter,
            // which is where `available` below reads it.
            drop(waiter.await.expect("the task ran").expect("not closed"));
        }

        assert_eq!(
            limiter.debt(),
            0,
            "the traffic that blocked the reduction is what applied it"
        );
        assert_eq!(
            limiter.available(),
            2,
            "and the limiter really is down to the configured two"
        );
        assert_eq!(
            limiter.effective(),
            2,
            "with the accounting agreeing, so a later sync is a no-op"
        );
    }

    #[test]
    fn a_reduction_blocked_by_in_flight_work_is_carried_and_settled_later() {
        // `forget_permits` can only take what is available. Recording the
        // requested value anyway would let the old concurrency creep back as
        // in-flight work returns its permits.
        let limiter = TranslateLimiter::new(8);
        // Six permits are checked out, so only two can be forgotten now.
        let held = limiter
            .permits
            .clone()
            .try_acquire_many_owned(6)
            .expect("6 available");
        limiter.retune(2);

        assert_eq!(limiter.available(), 0, "both spare permits were taken");
        assert_eq!(limiter.debt(), 4, "the rest is owed");
        assert_eq!(limiter.effective(), 2, "but the target is already binding");

        // The in-flight work finishes and its permits come back.
        drop(held);
        limiter.settle();
        assert_eq!(limiter.debt(), 0);
        assert_eq!(limiter.available(), 2);
        assert_eq!(limiter.effective(), 2);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn retuning_while_workers_pay_the_debt_cannot_corrupt_the_ceiling() {
        // The interleaving that wedged this twice, and the reason the
        // counters are behind a lock rather than individually atomic.
        //
        // `retune` upward reads the debt, works out how much of it to
        // cancel, and subtracts. A worker paying a unit in between made that
        // subtraction underflow: the effective ceiling reads zero, every
        // worker then retires the permit it just took, and translation stops
        // for the rest of the session. Nothing single-threaded can show it,
        // so this hammers the two paths against each other — with the
        // invariant asserted after every mutation inside the limiter, so a
        // torn update trips there too.
        let limiter = Arc::new(TranslateLimiter::new(4));

        let workers: Vec<_> = (0..4)
            .map(|_| {
                let limiter = Arc::clone(&limiter);
                tokio::spawn(async move {
                    for _ in 0..300 {
                        let permit = limiter.acquire().await.expect("open");
                        // HOLD it. A reduction taken while permits are out
                        // cannot be settled from idle ones, so it survives as
                        // debt — and the workers paying that debt down is the
                        // half of the race that matters.
                        tokio::time::sleep(std::time::Duration::from_micros(100)).await;
                        drop(permit);
                    }
                })
            })
            .collect();
        let tuner = {
            let limiter = Arc::clone(&limiter);
            tokio::spawn(async move {
                for i in 0..600 {
                    // Down and back up: raising while the workers are paying
                    // is the case that underflowed.
                    limiter.retune(if i % 2 == 0 { 2 } else { 4 });
                    tokio::time::sleep(std::time::Duration::from_micros(50)).await;
                }
            })
        };

        let storm = async {
            for worker in workers {
                worker.await.expect("no worker panicked");
            }
            tuner.await.expect("the tuner ran");
        };
        tokio::time::timeout(std::time::Duration::from_secs(30), storm)
            .await
            .expect("nothing wedged waiting for a permit");

        limiter.retune(4);
        assert_eq!(
            limiter.effective(),
            4,
            "the ceiling still reads what the config asks for"
        );
        // And it can really hand that many out — an underflowed debt leaves
        // `effective` at zero with every acquisition retiring its permit.
        let taking = async {
            let mut held = Vec::new();
            for _ in 0..4 {
                held.push(limiter.acquire().await.expect("open"));
            }
            held.len()
        };
        let taken = tokio::time::timeout(std::time::Duration::from_secs(5), taking)
            .await
            .expect("four permits are still obtainable");
        assert_eq!(taken, 4);
    }

    #[test]
    fn a_bypass_send_cannot_overtake_a_translation_already_in_flight() {
        // Outgoing translation switched off under a send already dispatched
        // — `/translate delout`, `/e2e on`, `translate.enabled false`, a
        // `/reload`. The earlier message stays in the translation lane by
        // design; the next one would take the ordinary path straight to the
        // socket and arrive FIRST, so peers read the two in the opposite
        // order to the one they were typed in. Invisibly to the author,
        // whose own buffer shows them correctly because the reservation
        // still orders the display.
        let mut app = app_with_outgoing(Some("de"));
        app.state.reserve_echo_slot(BUF, 1);
        app.state.note_outgoing_dispatch(BUF);

        // Now outgoing translation goes away under it.
        app.config.translate.buffers.remove(BUF);
        app.sync_translate_from_config();

        assert!(
            matches!(
                app.outgoing_translate_policy(BUF, "moje zdanie", false),
                OutgoingTranslatePolicy::Refuse(_)
            ),
            "the send waits rather than jumping the queue"
        );

        // The queue ceiling may take the display reservation back while the
        // send is still in the translator. That is a position being given up,
        // not work finishing, and reading the two as one thing let the next
        // message overtake it.
        app.state.release_echo_slot(BUF, 1);
        assert!(
            matches!(
                app.outgoing_translate_policy(BUF, "moje zdanie", false),
                OutgoingTranslatePolicy::Refuse(_)
            ),
            "losing the reservation does not mean the send came back"
        );

        // Once the send itself lands, ordinary sends resume immediately.
        app.state.clear_outgoing_dispatch(BUF);
        assert!(matches!(
            app.outgoing_translate_policy(BUF, "moje zdanie", false),
            OutgoingTranslatePolicy::NotApplicable
        ));
    }

    #[test]
    fn a_renamed_query_clears_its_in_flight_marker_where_it_moved_to() {
        // `rekey_buffer_state` moves `outgoing_in_flight` with the rest of the
        // buffer's state, so after a rename the marker sits under the NEW id
        // while the send in the worker still carries the old one. Clearing
        // only the id it was dispatched with leaves the renamed conversation
        // refusing its own ordinary sends until the marker ages out.
        let mut app = app_with_buffer();
        app.state
            .add_buffer(Buffer::for_test("test", BufferType::Query, "frank"));
        app.state.note_outgoing_dispatch("test/frank");
        let submitted = std::time::Instant::now();

        app.state
            .add_buffer(Buffer::for_test("test", BufferType::Query, "frankie"));
        app.state.rekey_buffer_state("test/frank", "test/frankie");
        assert!(
            app.state.has_outgoing_in_flight(
                "test/frankie",
                std::time::Duration::from_secs(60)
            ),
            "precondition: the marker moved with the buffer"
        );

        let mut out = outgoing(
            "sekret",
            TranslateOutcome::Translated {
                id: 1,
                text: "geheim".to_string(),
            },
            false,
        );
        out.buffer_id = "test/frank".to_string();
        out.buffer_name = "frank".to_string();
        out.submitted_at = submitted;
        out.conn_generation = app.connection_generation("test");
        app.apply_translate_deliver(TranslateDeliver::Outgoing(Box::new(out)));

        assert!(
            !app.state.has_outgoing_in_flight(
                "test/frankie",
                std::time::Duration::from_secs(60)
            ),
            "the send came back, so the conversation it moved to is free again"
        );
    }

    #[test]
    fn a_pending_send_holds_up_only_its_own_conversation() {
        // Deliberate scope, pinned so it is not widened by accident.
        //
        // The guard exists to stop a bypass send overtaking an earlier one to
        // the SAME conversation, where the reorder is plainly visible: a reply
        // above the message it answers, to everyone reading. Extending it to
        // the whole connection would refuse ordinary sends to every other
        // buffer whenever one translated send is in flight — which is not a
        // transient window but the steady state of the feature's ordinary
        // configuration, one channel translated and the rest not.
        let mut app = app_with_outgoing(Some("de"));
        app.state
            .add_buffer(Buffer::for_test("test", BufferType::Channel, "#other"));
        app.state.note_outgoing_dispatch(BUF);

        assert!(
            matches!(
                app.outgoing_translate_policy(BUF, "moje zdanie", true),
                OutgoingTranslatePolicy::Refuse(_)
            ),
            "its own conversation waits"
        );
        assert!(
            matches!(
                app.outgoing_translate_policy("test/#other", "moje zdanie", false),
                OutgoingTranslatePolicy::NotApplicable
            ),
            "an unrelated conversation on the same connection does not"
        );
    }

    #[test]
    fn a_stale_in_flight_marker_stops_holding_sends_up() {
        // Every delivery clears a marker, but a path that somehow does not
        // must not refuse this buffer's ordinary sends for the rest of the
        // session — the send it stands for cannot outlive its own budget.
        let mut app = app_with_buffer();
        app.config.translate.timeout_ms = 500;
        app.state
            .outgoing_in_flight
            .entry(BUF.to_string())
            .or_default()
            .push_back(
                std::time::Instant::now()
                    .checked_sub(std::time::Duration::from_secs(30))
                    .expect("30s before now"),
            );

        assert!(
            matches!(
                app.outgoing_translate_policy(BUF, "moje zdanie", false),
                OutgoingTranslatePolicy::NotApplicable
            ),
            "a marker older than any send could be is not believed"
        );
    }

    #[test]
    fn a_buffer_that_never_translated_is_never_held_up() {
        // The guard must not touch the ordinary path. A buffer with no
        // outgoing work in flight has no reservation, so nothing changes for
        // the overwhelming majority of sends.
        let app = app_with_buffer();
        assert!(matches!(
            app.outgoing_translate_policy(BUF, "moje zdanie", false),
            OutgoingTranslatePolicy::NotApplicable
        ));
        // Even with an E2E conversation, which takes the same bypass.
        assert!(matches!(
            app.outgoing_translate_policy(BUF, "moje zdanie", true),
            OutgoingTranslatePolicy::NotApplicable
        ));
    }

    #[test]
    fn a_split_action_echo_is_exactly_what_went_on_the_wire() {
        // The echo is the row the author sees and the line written to their
        // log, so it has to be the message the peers received — nothing
        // added. `split_irc_message` leaves a word's trailing whitespace on
        // the chunk before the break, and breaks a word too long for one line
        // at a character boundary with no whitespace at all. Inserting a
        // separator between the pieces therefore doubled a space in the first
        // case and put one INSIDE a word in the second.
        for body in [
            // Ordinary word-boundary splits.
            "wieloslowne zdanie ".repeat(40),
            // A single word far too long for one line — hard character
            // splits, no whitespace anywhere to rejoin on.
            "z".repeat(crate::irc::MESSAGE_MAX_BYTES * 3),
        ] {
            let mut app = app_with_dying_handle(usize::MAX);
            app.conn_generations.insert("test".to_string(), 1);
            let sender = app.irc_handles["test"].sender().clone();
            let mut out = outgoing(
                "krotkie",
                TranslateOutcome::Translated {
                    id: 1,
                    text: body.trim_end().to_string(),
                },
                false,
            );
            out.is_action = true;
            out.conn_generation = Some(1);
            app.state.reserve_echo_slot(BUF, 1);

            app.apply_translate_deliver(TranslateDeliver::Outgoing(Box::new(out)));

            // Rebuild what the peers actually saw, from the frames sent.
            let wire: Vec<String> = sender.captured().iter().map(ToString::to_string).collect();
            assert!(wire.len() > 1, "precondition: this had to be split");
            let on_the_wire: String = wire
                .iter()
                .filter_map(|frame| frame.split_once(" :").map(|(_, rest)| rest))
                .map(|payload| {
                    let payload = payload.trim_end_matches(['\r', '\n']);
                    crate::app::e2e_gate::translatable_outgoing_body(payload)
                        .unwrap_or(payload)
                        .to_string()
                })
                .collect();

            // The echo is itself broken into display ROWS by the same
            // splitter, so the comparison is row-set against wire-set — both
            // reassembled the way a reader does.
            let echoed: String = app.state.buffers[BUF]
                .messages
                .iter()
                .filter(|m| m.nick.as_deref() == Some("me"))
                .map(|m| m.text.as_str())
                .collect();
            assert!(!echoed.is_empty(), "the send is echoed locally");

            assert_eq!(
                echoed, on_the_wire,
                "what the author sees must be the message the peers got"
            );
        }
    }

    #[test]
    fn a_web_retry_is_withheld_when_the_tab_may_have_moved() {
        // A tab that follows a TUI-driven `ActiveBufferChanged` changes
        // buffer without telling us, and the opt-out is a localStorage flag
        // only the browser can see — so after such a broadcast the recorded
        // buffer is a guess. Handing back a BARE body on a guess puts it in
        // whatever composer the browser is really showing, and Enter sends it
        // there.
        //
        // The re-addressed `/msg` form is not the answer either, and this
        // test used to say it was. `web_run_command` runs the retry with the
        // TAB's buffer active, so `/msg` resolves its target on that buffer's
        // CONNECTION — and not knowing which buffer the tab is showing is
        // exactly not knowing which network it would go out on. Nothing is
        // restored; the text is in the error row.
        let session = "sess-1".to_string();
        let mut app = app_with_buffer();
        app.web_active_buffers.insert(session.clone(), BUF.to_string());
        let mut out = outgoing(
            "moje zdanie",
            TranslateOutcome::Untranslated {
                id: 1,
                reason: UntranslatedReason::Timeout,
            },
            false,
        );
        out.origin = SubmitOrigin::Web(session.clone());

        // Confirmed: the tab told us where it is, so the dispatch form stands.
        assert_eq!(
            app.deferred_retry_text(&out).as_deref(),
            Some("moje zdanie"),
            "a session we have heard from keeps the plain form"
        );

        // The TUI moves; this tab may or may not have followed.
        app.web_buffer_unconfirmed.insert(session);
        assert_eq!(
            app.deferred_retry_text(&out),
            None,
            "unsure where the composer is, so nothing is put in it"
        );
    }

    #[test]
    fn a_retry_is_never_handed_to_a_composer_on_another_network() {
        // `/msg` names a TARGET, not a network — `cmd_msg` resolves it
        // against whatever connection the active buffer belongs to when
        // Enter is pressed. The user switching servers while a translation
        // runs is not exotic; the wait is what gives them time to.
        //
        // So the re-addressed form, which exists to make a refusal safe from
        // any OTHER buffer, is safe only on the same connection. Offered on a
        // second network it hands the user a ready-to-send private message
        // aimed at whoever holds that nick THERE — the same leak the
        // buffer-is-gone branch guards, arriving through the other door.
        let mut app = app_with_buffer();
        app.state
            .add_buffer(Buffer::for_test("test", BufferType::Query, "bob"));
        let mut out = outgoing(
            "sekret",
            TranslateOutcome::Untranslated {
                id: 1,
                reason: UntranslatedReason::Timeout,
            },
            false,
        );
        out.buffer_id = "test/bob".to_string();
        out.buffer_name = "bob".to_string();
        out.buffer_type = BufferType::Query;
        out.retry_body = "sekret".to_string();

        // Same connection, different buffer: re-addressing is what it is for.
        assert_eq!(
            app.deferred_retry_text(&out).as_deref(),
            Some("/msg bob sekret"),
            "still on this network, so naming the target is enough"
        );

        // A second network, and the user is looking at it.
        app.state
            .add_buffer(Buffer::for_test("other", BufferType::Channel, "#inne"));
        app.state.set_active_buffer("other/#inne");
        assert_eq!(
            app.deferred_retry_text(&out),
            None,
            "there is no spelling of /msg that carries a network, so nothing \
             is offered — the text stays in the error row"
        );
    }

    #[test]
    fn a_web_session_at_the_new_buffer_is_not_marked_unsure() {
        // Its own `SwitchBuffer` is what raised the broadcast, and a tab
        // already recorded at the new buffer ends up there whether it follows
        // or not. Without this exemption every web switch would immediately
        // mark itself a guess.
        let mut app = app_with_buffer();
        app.web_active_buffers
            .insert("sess-1".to_string(), BUF.to_string());
        app.web_active_buffers
            .insert("sess-2".to_string(), "test/#other".to_string());
        app.state
            .pending_web_events
            .push(crate::web::protocol::WebEvent::ActiveBufferChanged {
                buffer_id: BUF.to_string(),
            });

        app.drain_pending_web_events();

        assert!(
            !app.web_buffer_unconfirmed.contains("sess-1"),
            "the session already at that buffer stays trusted"
        );
        assert!(
            app.web_buffer_unconfirmed.contains("sess-2"),
            "the one that may have followed does not"
        );
    }

    #[test]
    fn a_shell_switch_no_browser_ever_sees_leaves_every_tab_trusted() {
        // The TUI opening its own `/shell` raises `ActiveBufferChanged` like
        // any other switch, but the event is DROPPED rather than broadcast: a
        // web tab that followed it would render an unusable ShellView and
        // have its shell I/O rejected.
        //
        // An event no browser is ever sent cannot have moved a tab, so
        // doubting every session afterwards invents doubt out of nothing —
        // and the doubt is not free. A session marked unconfirmed has a
        // failed send withheld from its composer by `deferred_retry_text`,
        // which is the one path where getting the text back is the point.
        let mut app = app_with_buffer();
        app.web_active_buffers
            .insert("sess-1".to_string(), BUF.to_string());
        app.state.add_buffer(Buffer::for_test(
            "test",
            BufferType::Shell,
            "shell1",
        ));
        app.state
            .pending_web_events
            .push(crate::web::protocol::WebEvent::ActiveBufferChanged {
                buffer_id: "test/shell1".to_string(),
            });

        app.drain_pending_web_events();

        assert!(
            !app.web_buffer_unconfirmed.contains("sess-1"),
            "the tab was never told about the shell, so it is still where it \
             said it was"
        );

        // The ordinary switch it never sees is the contrast: that one IS
        // broadcast, so the tab may have followed it.
        app.state
            .add_buffer(Buffer::for_test("test", BufferType::Channel, "#inne"));
        app.state
            .pending_web_events
            .push(crate::web::protocol::WebEvent::ActiveBufferChanged {
                buffer_id: "test/#inne".to_string(),
            });
        app.drain_pending_web_events();
        assert!(
            app.web_buffer_unconfirmed.contains("sess-1"),
            "a switch that does reach the browser still makes the record a guess"
        );
    }

    #[test]
    fn taking_the_translator_away_stops_translation_without_a_restart() {
        // Turning translation ON needs a restart — the workers are bound in
        // `App::new`. Turning it OFF must not: `/set translate.backend none`
        // is a user saying "stop sending my lines to a translator", and a
        // switch that waits for a restart keeps shipping them meanwhile.
        let mut app = app_with_buffer();
        app.translate_backend = Some(std::sync::Arc::new(
            crate::translate::backend::StubBackend::new(0, 0),
        ));
        app.config.translate.enabled = true;
        app.config.translate.backend = "stub".to_string();
        app.sync_translate_from_config();
        assert!(app.state.translate_active, "precondition: it is running");

        app.config.translate.backend = "none".to_string();
        app.sync_translate_from_config();
        assert!(
            !app.state.translate_active,
            "the config names no translator, so nothing goes to one"
        );

        // A name this build cannot honour is the same answer, not a fallback
        // to whatever happens to be installed.
        app.config.translate.backend = "gogle".to_string();
        app.sync_translate_from_config();
        assert!(!app.state.translate_active);
    }

    #[test]
    fn changing_from_the_built_stub_to_ai_never_runs_the_stub() {
        let mut app = app_with_buffer();
        app.translate_backend = Some(std::sync::Arc::new(
            crate::translate::backend::StubBackend::new(0, 0),
        ));
        app.config.translate.enabled = true;
        app.config.translate.backend = "stub".to_string();
        app.sync_translate_from_config();
        assert!(app.state.translate_active);

        app.config.translate.backend = "ai".to_string();
        app.sync_translate_from_config();
        assert!(!app.state.translate_active);
    }

    #[test]
    fn changing_from_the_built_ai_to_stub_never_runs_ai() {
        let model = crate::config::TranslateAiModelConfig {
            name: "test".to_string(),
            base_url: "http://127.0.0.1:1/v1".to_string(),
            model: "test-model".to_string(),
            api_key_env: "TEST_API".to_string(),
            api_key: "secret".to_string(),
            ..crate::config::TranslateAiModelConfig::default()
        };
        let ai = crate::config::TranslateAiConfig {
            easy: vec!["test".to_string()],
            strong: vec!["test".to_string()],
            terminal: Some(Vec::new()),
            preferred_attempt_ms: 3_000,
            prompt_path: String::new(),
            models: vec![model],
        };
        let backend = AiBackend::new(&ai).expect("valid AI backend");
        let mut app = app_with_buffer();
        app.translate_backend = Some(std::sync::Arc::new(backend));
        app.config.translate.enabled = true;
        app.config.translate.backend = "ai".to_string();
        app.config.translate.ai = ai;
        app.sync_translate_from_config();
        assert!(app.state.translate_active);

        app.config.translate.backend = "stub".to_string();
        app.sync_translate_from_config();
        assert!(!app.state.translate_active);
    }

    #[test]
    fn a_tab_that_submits_says_where_it_is_and_is_trusted_again() {
        // `web_buffer_unconfirmed` records not knowing which buffer a tab is
        // showing. A submit ANSWERS that: the command carries the buffer its
        // text came from. Leaving the doubt standing after the tab has just
        // spoken is not cosmetic — `deferred_retry_text` withholds a refused
        // message entirely from a session whose buffer it does not know,
        // because it cannot tell which connection the retry would resolve
        // against. The author would not get their own text back, on the one
        // path where getting it back is the point.
        let session = "sess-1";
        let mut app = app_with_buffer();
        app.web_active_buffers
            .insert(session.to_string(), "test/#stale".to_string());
        app.web_buffer_unconfirmed.insert(session.to_string());

        app.handle_web_command(
            crate::web::protocol::WebCommand::SendMessage {
                buffer_id: BUF.to_string(),
                text: "czesc".to_string(),
            },
            session,
        );

        assert!(
            !app.web_buffer_unconfirmed.contains(session),
            "it just told us where it is"
        );
        assert_eq!(
            app.web_active_buffers.get(session).map(String::as_str),
            Some(BUF),
            "and the guess is replaced by what it said, not merely trusted"
        );

        // The same for the `/`-prefixed arm, which is a separate branch.
        app.web_buffer_unconfirmed.insert(session.to_string());
        app.handle_web_command(
            crate::web::protocol::WebCommand::RunCommand {
                buffer_id: "test/#other".to_string(),
                text: "/me macha".to_string(),
            },
            session,
        );
        assert_eq!(
            app.web_active_buffers.get(session).map(String::as_str),
            Some("test/#other"),
            "RunCommand names its buffer too"
        );
        assert!(!app.web_buffer_unconfirmed.contains(session));
    }

    #[test]
    fn an_empty_backend_answer_is_refused() {
        // Accepting it renders an incoming line blank when the original is
        // hidden, and on the outgoing side puts an EMPTY PRIVMSG on the
        // channel, reports the send as done, and throws away what the user
        // typed.
        for answer in ["", "\n", "\r\n", "   \r\n"] {
            let refused = super::single_line_or_refuse(
                1,
                TranslateOutcome::Translated {
                    id: 1,
                    text: answer.to_string(),
                },
            );
            assert!(
                matches!(refused, TranslateOutcome::Untranslated { .. }),
                "an empty answer is a gap, not a translation: {answer:?} -> {refused:?}"
            );
        }
    }

    #[test]
    fn an_empty_translation_never_reaches_the_wire() {
        // The consequence the guard buys: the original comes back to the
        // user instead of an empty line going out under their nick.
        let mut app = app_with_dying_handle(64);
        app.conn_generations.insert("test".to_string(), 1);
        let sender = app.irc_handles["test"].sender().clone();
        let mut out = outgoing(
            "moje zdanie",
            TranslateOutcome::Untranslated {
                id: 1,
                reason: UntranslatedReason::Error("backend returned nothing".to_string()),
            },
            false,
        );
        out.conn_generation = Some(1);
        app.state.reserve_echo_slot(BUF, 1);

        app.apply_translate_deliver(TranslateDeliver::Outgoing(Box::new(out)));

        assert!(
            sender.captured().is_empty(),
            "nothing goes out: {:?}",
            sender.captured()
        );
    }

    #[test]
    fn an_oversized_backend_answer_is_refused() {
        // One line in, one line back — but the send path SPLITS anything
        // over the wire budget and ships every chunk. Without a ceiling a
        // backend answering a three-word line with a megabyte turns one
        // keystroke into thousands of PRIVMSGs, under the user's own nick.
        //
        // Grown in a loop rather than with `repeat`/`vec!` on the constant:
        // a const-sized allocation here is folded into an array big enough
        // to trip `large_stack_arrays` in the test binary.
        let mut huge = String::new();
        while huge.len() <= MAX_TRANSLATION_BYTES {
            huge.push_str("wieloslowne zdanie ");
        }
        let refused = super::single_line_or_refuse(
            1,
            TranslateOutcome::Translated { id: 1, text: huge },
        );
        assert!(
            matches!(refused, TranslateOutcome::Untranslated { .. }),
            "an oversized answer is a gap, not something to publish"
        );

        // A translation that is merely LONGER than its source still passes:
        // expansion between languages is ordinary, flooding is not.
        let mut long = String::new();
        while long.len() < MAX_TRANSLATION_BYTES {
            long.push('a');
        }
        let accepted = super::single_line_or_refuse(
            2,
            TranslateOutcome::Translated { id: 2, text: long },
        );
        assert!(matches!(accepted, TranslateOutcome::Translated { .. }));
    }

    #[test]
    fn an_outgoing_outcome_reaches_the_status_tally() {
        // The outgoing path resolves its own sends and never goes through
        // the reorder queue, so nothing else would ever see this outcome.
        // A user running `addout` alone otherwise gets a status page saying
        // nothing has been through the translator — including while every
        // send is failing, which is when they would look.
        let mut app = app_with_buffer();
        app.state.reserve_echo_slot(BUF, 1);
        let mut out = outgoing(
            "moje zdanie",
            TranslateOutcome::Untranslated {
                id: 1,
                reason: UntranslatedReason::NoProvider,
            },
            false,
        );
        out.conn_generation = app.connection_generation("test");

        app.apply_translate_deliver(TranslateDeliver::Outgoing(Box::new(out)));

        assert_eq!(
            app.state.translate_tally.provider, 1,
            "the failure is visible to /translate status"
        );
        assert_eq!(app.state.translate_tally.translated, 0);
    }

    #[test]
    fn a_reduction_never_takes_the_ceiling_below_one() {
        // `acquire` retires permits until the debt is clear, so a debt that
        // covered every permit would block every worker for good.
        let limiter = TranslateLimiter::new(4);
        let held: Vec<_> = (0..4)
            .map(|_| limiter.permits.clone().try_acquire_owned().expect("permit"))
            .collect();
        limiter.retune(0);
        assert_eq!(limiter.effective(), 1, "clamped to one, not to zero");
        assert!(limiter.debt() < 4, "at least one permit survives the debt");
        drop(held);
        limiter.settle();
        assert_eq!(limiter.available(), 1);
    }

    #[test]
    fn settling_is_a_no_op_with_no_debt() {
        let limiter = TranslateLimiter::new(4);
        limiter.settle();
        assert_eq!(limiter.available(), 4);
    }

    #[test]
    fn retuning_to_the_same_value_is_a_no_op() {
        let mut app = app_with_buffer();
        app.translate_in_flight = Some(Arc::new(TranslateLimiter::new(4)));
        app.config.translate.max_in_flight = 4;
        app.sync_translate_from_config();
        assert_eq!(app.translate_in_flight.as_ref().unwrap().available(), 4);
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
    fn closing_a_buffer_releases_its_queue_and_then_drops_it() {
        // Two things at once, and the order between them is the point: the
        // queued lines are released WHILE the buffer still exists, so they
        // are displayed and logged, and only then does the queue go. It must
        // not outlive its buffer either way.
        let mut app = app_with_queue(3);
        app.state.remove_buffer(BUF);
        assert!(
            !app.state.translate_queues.contains_key(BUF),
            "the queue must not outlive its buffer"
        );
        assert!(
            !app.state.buffers.contains_key(BUF),
            "and the buffer is closed"
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
            deadline: None,
            casemapping: "rfc1459".to_string(),
            known_nicks: Vec::new(),
        }
    }

    #[tokio::test]
    async fn enabling_translation_never_installs_the_test_stub_by_itself() {
        // The stub does not translate — it reverses word order — and its
        // output does not stay on screen: on the outgoing side it is what
        // goes to the channel, under the user's own nick. Whoever turns
        // `translate.enabled` on is asking for a translator, and handing them
        // this one because it is the only implementation that exists corrupts
        // every line they send.
        //
        // So the backend is chosen by NAME, and the name has to be said.
        let plain = crate::config::TranslateConfig {
            enabled: true,
            ..Default::default()
        };
        assert!(
            TranslateRuntime::build(&plain).backend.is_none(),
            "enabled=true alone must install no translator"
        );

        let named = crate::config::TranslateConfig {
            enabled: true,
            backend: "stub".to_string(),
            ..Default::default()
        };
        assert!(
            TranslateRuntime::build(&named).backend.is_some(),
            "and naming it must still get it — the mechanism has to be \
             testable end to end"
        );

        // A typo installs nothing rather than the nearest thing.
        let typo = crate::config::TranslateConfig {
            enabled: true,
            backend: "stubb".to_string(),
            ..Default::default()
        };
        assert!(TranslateRuntime::build(&typo).backend.is_none());

        // And the master switch still wins over a named backend.
        let off = crate::config::TranslateConfig {
            enabled: false,
            backend: "stub".to_string(),
            ..Default::default()
        };
        assert!(TranslateRuntime::build(&off).backend.is_none());
    }

    #[test]
    fn every_way_of_not_translating_says_so_at_startup() {
        // Both silences mislead. A config asking for translation with nothing
        // behind it looks exactly like one that works, and the test backend
        // looks — to the channel — like the user typing sentences backwards.
        let notice = |backend: &str, enabled: bool| {
            startup_backend_notice(&crate::config::TranslateConfig {
                enabled,
                backend: backend.to_string(),
                ..Default::default()
            })
        };

        assert_eq!(notice("none", false), None, "nothing to say when it is off");
        assert_eq!(notice("stub", false), None);

        let none = notice("none", true).expect("enabled with no translator must be reported");
        assert!(none.contains("no translator"), "{none}");

        let unknown = notice("gogle", true).expect("a typo must be reported");
        assert!(
            unknown.contains("gogle"),
            "the name has to be echoed back — the whole failure IS the typo: {unknown}"
        );

        let stub = notice("stub", true).expect("the test backend must be announced");
        assert!(
            stub.contains("REVERSES") && stub.contains("sent to the network"),
            "the warning has to say what it does to outgoing lines: {stub}"
        );
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
        let refused = super::single_line_or_refuse(1, TranslateOutcome::Translated {
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

        let trimmed = super::single_line_or_refuse(2, TranslateOutcome::Translated {
            id: 2,
            text: "mein satz\r\n".to_string(),
        });
        assert!(
            matches!(&trimmed, TranslateOutcome::Translated { text, .. } if text == "mein satz"),
            "got {trimmed:?}"
        );

        let clean = super::single_line_or_refuse(3, TranslateOutcome::Translated {
            id: 3,
            text: "mein satz".to_string(),
        });
        assert!(
            matches!(&clean, TranslateOutcome::Translated { text, .. } if text == "mein satz"),
            "an ordinary answer is untouched: {clean:?}"
        );
    }

    #[test]
    fn ctcp_framing_is_refused_at_the_seam_in_both_directions() {
        use crate::translate::UntranslatedReason;
        // The outgoing risk is a request sent under the user's nick. The
        // INCOMING risk is quieter and is why this lives at the seam rather
        // than only at the send: a reply carrying `\x01ACTION …\x01` renders
        // as an action from the peer, putting words in their mouth in a
        // conversation the user is reading as authentic.
        for text in [
            "\u{1}DCC SEND secrets.txt 2130706433 1024 512\u{1}",
            "\u{1}ACTION nigdy tego nie powiedzial\u{1}",
            "prawdziwe zdanie\u{1}",
        ] {
            let refused = super::single_line_or_refuse(1, TranslateOutcome::Translated {
                id: 1,
                text: text.to_string(),
            });
            assert!(
                matches!(
                    refused,
                    TranslateOutcome::Untranslated {
                        reason: UntranslatedReason::Error(_),
                        ..
                    }
                ),
                "{text:?} must not come back as a translation: {refused:?}"
            );
        }
    }

    /// A backend that answers with whatever it is handed at construction —
    /// standing in for a compromised or simply broken provider.
    #[derive(Debug)]
    struct EchoingBackend(String);

    impl crate::translate::backend::TranslateBackend for EchoingBackend {
        fn kind(&self) -> crate::translate::backend::BackendKind {
            crate::translate::backend::BackendKind::Stub
        }

        fn is_ready(&self) -> bool {
            true
        }

        fn configuration_matches(&self, config: &crate::config::TranslateConfig) -> bool {
            crate::translate::backend::backend_kind(&config.backend)
                == crate::translate::backend::BackendKind::Stub
        }

        fn refresh_config(&self, _config: &crate::config::TranslateConfig) {}

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
            log_key: None,
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
            translation_suffix_at: None,
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
