use indexmap::IndexMap;
use std::collections::{HashMap, VecDeque};

use tokio::sync::mpsc;

pub mod buffer;
pub mod connection;
pub mod events;
pub mod sorting;
pub mod typing;

use buffer::Buffer;
use connection::Connection;
use connection::ConnectionStatus;

use crate::config::IgnoreEntry;
use crate::e2e::E2eManager;
use crate::irc::flood::FloodState;
use crate::irc::netsplit::NetsplitState;
use crate::scripting::engine::{BufferInfo, ConnectionInfo, NickInfo, ScriptStateSnapshot};
use crate::storage::LogRow;

/// A queued outbound IRC NOTICE produced by the E2E event handlers.
/// Drained by `App::drain_pending_e2e_sends` after each
/// `handle_irc_message`, mirroring the `pending_web_events` pattern so
/// event handlers can produce outbound traffic without holding a mutable
/// borrow of `App`.
/// A conversation needing a post-handshake `CHATHISTORY` gap-fill — see
/// `AppState::pending_e2e_gapfills`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingE2eGapfill {
    /// Connection the conversation lives on.
    pub connection_id: String,
    /// The `CHATHISTORY` target: the peer's nick for a DM, the channel name
    /// for a channel.
    pub target: String,
}

#[derive(Debug, Clone)]
pub struct PendingE2eSend {
    /// Connection the NOTICE must be shipped over.
    pub connection_id: String,
    /// NOTICE target — peer nick for handshake replies.
    pub target: String,
    /// Full CTCP-framed body ready to hand to `send_notice` — i.e.
    /// already wrapped in `\x01RPEE2E ...\x01`.
    pub notice_text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingUserhostAction {
    E2eForget {
        buffer_id: String,
        target: String,
        /// Peer context `@<peer>` (resolved from the active buffer at queue time).
        channel: Option<String>,
        all: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingUserhostRequest {
    pub connection_id: String,
    pub nick: String,
    pub action: PendingUserhostAction,
}

/// How to render one reflected wire line — see
/// [`AppState::own_echo_decorations`].
#[derive(Debug, Clone)]
pub struct OwnEchoDecoration {
    /// The wire line exactly as we sent it, which is what the reflection
    /// carries. For an action this is the framed `\x01ACTION …\x01`, because
    /// matching happens before the frame is stripped.
    pub wire_text: String,
    /// The message id reserved for this row when the user pressed Enter.
    ///
    /// The reflection takes it instead of a fresh one, so it fills the place
    /// held for it in the reorder queue. Without that the barrier is
    /// released empty, everything queued behind it drains first, and the
    /// user's own message lands after the replies to it — the exact
    /// reordering the reservation exists to prevent.
    pub echo_id: u64,
    /// What to display instead — framed the same way, so a decorated action
    /// still parses as one — together with what to record as the row's wire
    /// origin.
    ///
    /// `None` when the reflection already reads correctly and only its
    /// POSITION needed arranging.
    pub display: Option<(String, buffer::WireOrigin)>,
    filed_at: std::time::Instant,
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "top-level app state aggregates independent feature flags"
)]
pub struct AppState {
    pub connections: HashMap<String, Connection>,
    pub buffers: IndexMap<String, Buffer>,
    pub active_buffer_id: Option<String>,
    pub previous_buffer_id: Option<String>,
    pub message_counter: u64,
    /// Flood detection state (global, not per-connection).
    pub flood_state: FloodState,
    /// Netsplit detection state (global, not per-connection).
    pub netsplit_state: NetsplitState,
    /// Whether flood protection is enabled (from config).
    pub flood_protection: bool,
    pub flood_exemptions: Vec<String>,
    /// Ignore rules (from config).
    pub ignores: Vec<IgnoreEntry>,
    /// Sender for the storage writer. When `Some`, messages are logged to `SQLite`.
    pub log_tx: Option<mpsc::Sender<LogRow>>,
    /// Worker-queue sender for incoming-message shrink dispatch.
    /// `None` when the feature is disabled (no API key, master switch
    /// off, etc.). Pushed to from `add_message_with_activity` when
    /// `shrink_incoming_active` is true and the message text has at
    /// least one URL of length ≥ `shrink_min_url_length`. The worker
    /// substitutes, then forwards a `ShrinkDeliver::Incoming` back to
    /// the main loop which calls `state.add_message_with_activity`.
    pub shrink_incoming_tx: Option<mpsc::Sender<crate::app::shrink::PendingIncoming>>,
    /// True when shrink incoming substitution should be applied to
    /// live PRIVMSG/ACTION/NOTICE messages. Mirror of
    /// `(config.shrink.enabled && config.shrink.incoming_enabled &&
    /// SHRINK_API_KEY is configured)`. Synced from `/set` so a
    /// runtime flip takes effect without restart.
    pub shrink_incoming_active: bool,
    /// URL length threshold mirrored from `config.shrink.min_url_length`.
    pub shrink_min_url_length: u32,
    /// Worker-queue sender for incoming translation dispatch. `None` when
    /// the feature is disabled. Same shape as `shrink_incoming_tx`: the
    /// synchronous `add_message` path decides between an immediate add and
    /// a deferred translate without reaching into `App`.
    pub translate_incoming_tx: Option<mpsc::Sender<crate::app::translate::PendingTranslate>>,
    /// Per-buffer reorder queues. A buffer appears here only while it has
    /// lines in flight; absent means "deliver straight through".
    ///
    /// Note this is keyed by buffer id and NOT cleared when translation is
    /// turned off — a queue with entries still has to drain in order.
    pub translate_queues: HashMap<String, crate::translate::queue::TranslateQueue>,
    /// Mirror of `config.translate.enabled && a backend exists`. Synced from
    /// `/set` so a runtime flip needs no restart, exactly as
    /// `shrink_incoming_active` is.
    pub translate_active: bool,
    /// Mirror of `config.translate.buffers`, keyed by buffer id.
    pub translate_buffers: HashMap<String, crate::config::TranslateBufferConfig>,
    /// Mirror of `config.translate.my_lang`.
    pub translate_my_lang: String,
    /// Mirror of `config.translate.show_original_in`. Captured per line at
    /// dispatch, so a mid-flight `/set` cannot make a queued line render
    /// differently from how it was queued.
    pub translate_show_original_in: bool,
    /// Mirror of `config.translate.max_queue`. The ceiling has to be
    /// enforced at INSERTION, not only on the maintenance tick: a stalled
    /// provider plus a busy channel can put far more than this in a queue
    /// between two ticks, and the setting is documented as a bound on memory
    /// and on how far behind the display can fall.
    pub translate_max_queue: usize,
    /// How to render `echo-message`'s reflection of a translated line we
    /// sent, keyed by buffer, oldest first.
    ///
    /// The wire carries only the translation, so the reflection cannot show
    /// the ` [original]` suffix `show_original_out` asks for. The fix is to
    /// **decorate the reflection**, not to replace it with a local row: the
    /// reflection is the copy that carries the server's `@time` and `@msgid`,
    /// and those are what a later CHATHISTORY replay of the same message
    /// dedups against. A locally-authored row has a local clock and no msgid,
    /// so it matches nothing and the message comes back a second time.
    ///
    /// Bounded and time-limited, because a reflection that never arrives (a
    /// netsplit between send and echo) must not accumulate. A miss renders
    /// the reflection undecorated, which is exactly what every non-translated
    /// send does — the mechanism can lose a suffix, never a message.
    pub own_echo_decorations: HashMap<String, VecDeque<OwnEchoDecoration>>,
    /// Query buffers that were re-keyed, `old_id -> (new_id, when)`.
    ///
    /// Work already handed to the translation workers carries the buffer id
    /// and target name it was dispatched with. A peer's `/nick` moves the
    /// buffer out from under it, so without a redirect an incoming outcome
    /// finds no queue and times out, and an outgoing send addresses a nick
    /// its owner no longer answers to — which, if somebody else has taken it
    /// in the meantime, means sending it to a stranger.
    pub buffer_redirects: HashMap<String, (String, std::time::Instant)>,
    /// Query buffers re-keyed by a peer's nick change, as `(old_id, new_id)`.
    ///
    /// Drained by the App after each IRC message, the same way
    /// `pending_web_events` is. The state-side maps are moved immediately in
    /// `rekey_buffer_state`; this exists for the half the App owns —
    /// `config.translate.buffers`, whose key must move too or the next
    /// `sync_translate_from_config` re-derives the mirror from the stale
    /// config and silently ends translation for that conversation.
    pub pending_buffer_rekeys: Vec<(String, String)>,
    /// Message types excluded from logging (e.g. "event" to skip quit/join/nick fan-out).
    pub log_exclude_types: Vec<String>,
    /// Maximum messages per buffer (FIFO eviction). 0 = unlimited.
    pub scrollback_limit: usize,
    /// Pending web events to broadcast after IRC event processing.
    /// Drained by `App` after each `handle_irc_message` call.
    pub pending_web_events: Vec<crate::web::protocol::WebEvent>,
    /// Pending E2E CTCP NOTICE sends produced by the event handlers.
    /// Drained by `App::drain_pending_e2e_sends` right after
    /// `drain_pending_web_events`. Same pattern as `pending_web_events`.
    pub pending_e2e_sends: Vec<PendingE2eSend>,
    /// Conversations whose incoming E2E session was just installed by a KEYRSP.
    /// Drained by the app loop right after `drain_pending_e2e_sends`: each
    /// entry re-runs the query's `CHATHISTORY` gap-fill so the message that
    /// TRIGGERED the handshake (shown only as the transient
    /// "[E2E: awaiting session with …]" placeholder) is re-fetched, decrypted
    /// under the fresh session, and spliced in — without this the first DM of
    /// every new session is lost.
    pub pending_e2e_gapfills: Vec<PendingE2eGapfill>,
    pub pending_userhost_requests: Vec<PendingUserhostRequest>,
    /// Who is typing, per buffer (`IRCv3` `+typing`). Ephemeral — never persisted.
    pub typing: typing::TypingTracker,
    /// Mirror of `config.typing.show`. `events.rs` has no access to `AppConfig`,
    /// so this follows the same config→state sync as `scrollback_limit`.
    /// It gates *ingestion*, not just rendering — see spec §5.
    pub typing_show: bool,
    /// Nick color HSL saturation (synced from config for mention line formatting).
    pub nick_color_sat: f32,
    /// Nick color HSL lightness (synced from config for mention line formatting).
    pub nick_color_lit: f32,
    /// RPE2E manager, initialized once storage is up. `None` when the
    /// `[e2e] enabled = false` config switch disables E2E entirely.
    pub e2e_manager: Option<std::sync::Arc<E2eManager>>,
    /// When set, `add_message` skips `MessageType::Event` lines so script
    /// suppress hides the JOIN/PART/QUIT/MODE/etc. event display while the
    /// underlying state mutation still runs. Set/cleared around a single
    /// `handle_irc_message` call by the IRC dispatcher.
    pub suppress_event_display: bool,
    /// When `Some`, every `WireMessage` constructed for the web frontend has
    /// its `previews` populated by this extractor. `None` = web image
    /// previews disabled.
    pub web_preview_extractor: Option<std::sync::Arc<crate::web::preview::WebPreviewExtractor>>,
}

impl AppState {
    /// Build a lightweight snapshot of the current state for script callbacks.
    pub fn script_snapshot(&self) -> ScriptStateSnapshot {
        let connections: Vec<ConnectionInfo> = self
            .connections
            .values()
            .map(|c| ConnectionInfo {
                id: c.id.clone(),
                label: c.label.clone(),
                nick: c.nick.clone(),
                connected: c.status == ConnectionStatus::Connected,
                user_modes: c.user_modes.clone(),
            })
            .collect();

        let buffers: Vec<BufferInfo> = self
            .buffers
            .values()
            .map(|b| {
                let bt = match b.buffer_type {
                    buffer::BufferType::Mentions => "mentions",
                    buffer::BufferType::Server => "server",
                    buffer::BufferType::Channel => "channel",
                    buffer::BufferType::Query => "query",
                    buffer::BufferType::DccChat => "dcc_chat",
                    buffer::BufferType::Special => "special",
                    buffer::BufferType::Shell => "shell",
                    buffer::BufferType::Log => "log",
                };
                BufferInfo {
                    id: b.id.clone(),
                    connection_id: b.connection_id.clone(),
                    name: b.name.clone(),
                    buffer_type: bt.to_string(),
                    topic: b.topic.clone(),
                    unread_count: b.unread_count,
                }
            })
            .collect();

        let mut buffer_nicks: HashMap<String, Vec<NickInfo>> = HashMap::new();
        for (buf_id, buf) in &self.buffers {
            if !buf.users.is_empty() {
                let nicks = buf
                    .users
                    .values()
                    .map(|e| NickInfo {
                        nick: e.nick.clone(),
                        prefix: e.prefix.clone(),
                        modes: e.modes.clone(),
                        away: e.away,
                    })
                    .collect();
                buffer_nicks.insert(buf_id.clone(), nicks);
            }
        }

        ScriptStateSnapshot {
            active_buffer_id: self.active_buffer_id.clone(),
            connections,
            buffers,
            buffer_nicks,
            script_config: HashMap::new(),
            app_config_toml: None,
        }
    }
}
