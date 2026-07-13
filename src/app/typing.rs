//! Outbound `IRCv3` `+typing`: deciding when to tell the network we are typing.
//!
//! Two layers, because **there is no single "current input"**. The TUI has one,
//! and every web session has its own: `handle_web_command` receives a
//! `session_id` (`src/app/web.rs:413-416`) and `web_active_buffers` maps session
//! to buffer, independently of `state.active_buffer_id`. Two browser tabs can be
//! typing into two buffers while the terminal types into a third.
//!
//! * **Sources** report `(buffer, is-typing, when)`.
//! * **Targets** are buffers. A target's desired state is the *aggregate* of the
//!   sources pointing at it, and the 3s throttle clock is per target.
//!
//! `TypingSender` is pure: it takes `now` and **proposes** notifications. `App`
//! owns the clock, the guards and the socket, and calls `confirm_sent` for the
//! proposals that actually went out. A proposal blocked by a guard is simply
//! re-proposed on the next tick.

use std::collections::HashMap;
use std::time::Instant;

use crate::app::App;
use crate::irc::typing::{self, THROTTLE, TypingState};
use crate::state::buffer::BufferType;

/// Where a typing signal came from. Each web session is its own source.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TypingSource {
    Tui,
    Web(String),
}

#[derive(Debug)]
struct SourceState {
    buffer_id: String,
    /// Its input holds non-empty, non-slash text.
    active: bool,
    /// When its input last changed — drives the `paused` transition.
    last_activity: Instant,
}

#[derive(Debug, Default)]
struct TargetState {
    /// The last state we actually put on the wire for this buffer.
    sent: Option<TypingState>,
    /// The per-target 3s throttle clock.
    last_sent: Option<Instant>,
}

/// Proposes typing notifications. Pure — no clock, no I/O.
///
/// It holds **no flood state**. The budget is the *connection's*, not the
/// machine's: each `IrcHandle` carries its own mirror of that connection's
/// penalty counter and charges it on every send, so the headroom question can
/// only be answered where the connection is known — `App::send_typing`.
#[derive(Debug, Default)]
pub struct TypingSender {
    sources: HashMap<TypingSource, SourceState>,
    targets: HashMap<String, TargetState>,
    /// Real messages we sent, per target — for the §3.1 suppression window.
    last_message: HashMap<String, Instant>,
}

impl TypingSender {
    /// A source's input changed. `active` is the already-computed `should_type`
    /// predicate — the browser applies it locally and sends only the boolean.
    pub fn on_activity(
        &mut self,
        source: TypingSource,
        buffer_id: &str,
        active: bool,
        now: Instant,
    ) -> Vec<(String, TypingState)> {
        let previous = self.sources.insert(
            source,
            SourceState {
                buffer_id: buffer_id.to_string(),
                active,
                last_activity: now,
            },
        );

        let mut dirty = vec![buffer_id.to_string()];
        // A source that moved releases its old target *now*, not at the next
        // tick: the user may switch and type again within the same second, and
        // the old target must still get its `done`.
        if let Some(prev) = previous
            && prev.buffer_id != buffer_id
        {
            dirty.push(prev.buffer_id);
        }
        self.propose(&dirty, now)
    }

    /// A source submitted something. A real message needs no `done` of its own:
    /// its PRIVMSG clears typing at the receivers.
    ///
    /// `sent_message` is **what actually reached the wire**, never what the text
    /// looked like. The two are not the same thing: an E2E-enabled DM whose peer
    /// has not spoken yet is refused by the outbound gate, and a message composed
    /// on a connection that just dropped never leaves either — in both cases the
    /// user pressed Enter on perfectly ordinary text and nothing was said. The
    /// caller therefore runs the submit FIRST and reports its outcome (see
    /// [`App::submit_from_tui`]).
    ///
    /// It gates BOTH of the things that happen here, and for the same reason —
    /// nothing was said on the wire:
    ///
    /// * the §3.1 suppression window (`last_message`), which would otherwise
    ///   falsely mute typing in this buffer for the next 3 seconds; and
    /// * retiring the target (`sent = None`), which throws away the record that
    ///   a `done` is **owed** to the peers. A `/whois` — or a refused message —
    ///   submitted while a `done` is still waiting out its throttle would take
    ///   that `done` with it, and the peers would show us as typing until the
    ///   TTL: 30s if the last state on the wire was `paused`.
    ///
    /// (The flood budget needs no help from here — whatever the submit puts on
    /// the wire is charged by `IrcSender::send` itself.)
    pub fn on_submit(
        &mut self,
        source: &TypingSource,
        buffer_id: &str,
        sent_message: bool,
        now: Instant,
    ) -> Vec<(String, TypingState)> {
        if let Some(s) = self.sources.get_mut(source) {
            s.active = false;
            s.last_activity = now;
        }
        if sent_message {
            return self.on_message_sent(buffer_id, now);
        }
        // Another source may still be typing here, so re-derive rather than assume.
        self.propose(&[buffer_id.to_string()], now)
    }

    /// A real message reached the wire for `buffer_id`, from a path that is not
    /// a submit of this buffer's input: a `/me` or `/msg <peer>` (which speak
    /// into a buffer without being typed in it — the command was), a Lua script
    /// send, or the deferred shrink delivery, which puts the message on the wire
    /// seconds AFTER the Enter that queued it.
    ///
    /// Same two effects as a submit that sent something, minus the source: the
    /// message is the retraction, so the target is retired without a `done`
    /// (coalescing away one that was still pending), and §3.1's window opens.
    ///
    /// Emits nothing itself: `last_message` is recorded before the re-derive, so
    /// the proposal it triggers is always suppressed by the very message we are
    /// recording. It is called anyway, so a target left dirty by a source that is
    /// still typing here is reconsidered at once rather than at the next tick.
    pub fn on_message_sent(
        &mut self,
        buffer_id: &str,
        now: Instant,
    ) -> Vec<(String, TypingState)> {
        self.last_message.insert(buffer_id.to_string(), now);
        // The message itself is the retraction. Retire the target without a
        // `done`, and coalesce away any `done` that was still pending.
        if let Some(t) = self.targets.get_mut(buffer_id) {
            t.sent = None;
        }
        self.propose(&[buffer_id.to_string()], now)
    }

    /// A source went away — a web session disconnected.
    pub fn remove_source(
        &mut self,
        source: &TypingSource,
        now: Instant,
    ) -> Vec<(String, TypingState)> {
        let Some(prev) = self.sources.remove(source) else {
            return Vec::new();
        };
        self.propose(&[prev.buffer_id], now)
    }

    /// The 1s tick: refresh `active`, transition to `paused`, flush a pending
    /// `done` once its throttle window opens.
    pub fn on_tick(&mut self, now: Instant) -> Vec<(String, TypingState)> {
        let mut ids: Vec<String> = self.targets.keys().cloned().collect();
        for s in self.sources.values() {
            if !ids.contains(&s.buffer_id) {
                ids.push(s.buffer_id.clone());
            }
        }
        let due = self.propose(&ids, now);
        self.prune(now);
        due
    }

    /// Record that a proposal actually reached the wire. `App` calls this only
    /// after every guard passed and the send succeeded.
    pub fn confirm_sent(&mut self, buffer_id: &str, state: TypingState, now: Instant) {
        let entry = self.targets.entry(buffer_id.to_string()).or_default();
        entry.last_sent = Some(now);
        entry.sent = (state != TypingState::Done).then_some(state);
    }

    fn propose(&self, buffer_ids: &[String], now: Instant) -> Vec<(String, TypingState)> {
        let mut out = Vec::new();
        for buffer_id in buffer_ids {
            if let Some(state) = self.next_state(buffer_id, now) {
                out.push((buffer_id.clone(), state));
            }
        }
        out
    }

    /// The notification this target needs right now, if any.
    fn next_state(&self, buffer_id: &str, now: Instant) -> Option<TypingState> {
        let desired = self.desired(buffer_id, now);
        let sent = self.targets.get(buffer_id).and_then(|t| t.sent);

        let needed = match (desired, sent) {
            // Nothing to say, nothing outstanding.
            (None, None) => return None,
            // Every source stopped: retract.
            (None, Some(_)) => TypingState::Done,
            // `active` is sent "continuously" — refresh it each window.
            (Some(TypingState::Active), Some(TypingState::Active)) => TypingState::Active,
            (Some(d), Some(s)) if d == s => return None,
            (Some(d), _) => d,
        };

        // The 3s throttle binds EVERY notification, `done` included — the spec is
        // unqualified. A blocked notification is neither dropped nor sent early:
        // desired-vs-sent still differs, so the next tick reconsiders it against
        // whatever is true then. That is also how a `done` gets coalesced away if
        // the user resumes typing before the window opens.
        if self.throttled(buffer_id, now) {
            return None;
        }
        // The message we just sent already announced us (§3.1).
        if self.suppressed_by_message(buffer_id, now) {
            return None;
        }
        // The flood gate is NOT here: it belongs to the target's connection, and
        // this machine does not know which connection a buffer is on.
        // `App::send_typing` asks that connection's handle and simply declines to
        // send; the proposal is then reconsidered on the next tick, exactly like
        // one blocked by the throttle above.
        Some(needed)
    }

    /// What this target's sources collectively imply.
    fn desired(&self, buffer_id: &str, now: Instant) -> Option<TypingState> {
        let mut any = false;
        let mut fresh = false;
        for s in self.sources.values() {
            if s.buffer_id != buffer_id || !s.active {
                continue;
            }
            any = true;
            if now.duration_since(s.last_activity) < THROTTLE {
                fresh = true;
            }
        }
        if !any {
            return None;
        }
        // A source touched its input recently → still `active`. Otherwise text is
        // sitting there untouched → `paused`.
        Some(if fresh {
            TypingState::Active
        } else {
            TypingState::Paused
        })
    }

    fn throttled(&self, buffer_id: &str, now: Instant) -> bool {
        self.targets
            .get(buffer_id)
            .and_then(|t| t.last_sent)
            .is_some_and(|t| now.duration_since(t) < THROTTLE)
    }

    fn suppressed_by_message(&self, buffer_id: &str, now: Instant) -> bool {
        self.last_message
            .get(buffer_id)
            .is_some_and(|t| now.duration_since(*t) < THROTTLE)
    }

    /// The state we last put on the wire for `buffer_id`. `Some` means a
    /// retraction is still **owed** to that target's peers.
    #[cfg(test)]
    pub(crate) fn sent_state(&self, buffer_id: &str) -> Option<TypingState> {
        self.targets.get(buffer_id).and_then(|t| t.sent)
    }

    /// When a real message last went out for `buffer_id` — the §3.1 suppression
    /// window. `None` means nothing has been said here.
    #[cfg(test)]
    pub(crate) fn last_message_at(&self, buffer_id: &str) -> Option<Instant> {
        self.last_message.get(buffer_id).copied()
    }

    /// Whether `source` currently reports itself as typing.
    #[cfg(test)]
    pub(crate) fn source_is_active(&self, source: &TypingSource) -> Option<bool> {
        self.sources.get(source).map(|s| s.active)
    }

    /// Forget a buffer entirely — its sources and target. Called when the
    /// buffer has been closed: there is nothing left to notify.
    pub fn forget_buffer(&mut self, buffer_id: &str) {
        self.sources.retain(|_, s| s.buffer_id != buffer_id);
        self.targets.remove(buffer_id);
        self.last_message.remove(buffer_id);
    }

    /// Drop retired targets and stale message timestamps so the maps stay bounded.
    fn prune(&mut self, now: Instant) {
        let live: Vec<&str> = self.sources.values().map(|s| s.buffer_id.as_str()).collect();
        self.targets.retain(|id, t| {
            t.sent.is_some()
                || live.contains(&id.as_str())
                || t.last_sent
                    .is_some_and(|s| now.duration_since(s) < THROTTLE)
        });
        self.last_message
            .retain(|_, t| now.duration_since(*t) < THROTTLE);
    }
}

impl App {
    /// The terminal's input changed — a keystroke or a paste.
    pub(crate) fn on_input_changed(&mut self) {
        let Some(buffer_id) = self.state.active_buffer_id.clone() else {
            return;
        };
        let active = typing::should_type(&self.input.value);
        let due = self
            .typing
            .on_activity(TypingSource::Tui, &buffer_id, active, Instant::now());
        self.dispatch_typing(due);
    }

    /// A browser session's input changed. It reports only the predicate.
    pub(crate) fn on_web_typing(&mut self, session_id: &str, buffer_id: &str, active: bool) {
        let due = self.typing.on_activity(
            TypingSource::Web(session_id.to_string()),
            buffer_id,
            active,
            Instant::now(),
        );
        self.dispatch_typing(due);
    }

    /// Something was submitted from `source`. `sent_message` is whether a real
    /// message **reached the wire** — see `TypingSender::on_submit`.
    pub(crate) fn on_typing_submit(
        &mut self,
        source: &TypingSource,
        buffer_id: &str,
        sent_message: bool,
    ) {
        let due = self
            .typing
            .on_submit(source, buffer_id, sent_message, Instant::now());
        self.dispatch_typing(due);
    }

    /// A real message reached the wire for `buffer_id` outside a submit of that
    /// buffer's input — see `TypingSender::on_message_sent`.
    pub(crate) fn note_message_sent(&mut self, buffer_id: &str) {
        let due = self.typing.on_message_sent(buffer_id, Instant::now());
        self.dispatch_typing(due);
    }

    /// Run a TUI submit and tell the typing machine what it actually did. Every
    /// `handle_submit` call site in `input.rs` goes through here, or the machine
    /// keeps an `Active` recorded for the target and the next input change
    /// retracts it with a spurious `done` right after the real PRIVMSG.
    ///
    /// The submit runs FIRST because only its outcome — not the text — says
    /// whether anything was said: a message the outbound E2E gate refuses, or one
    /// a dead connection swallows, is ordinary text that never left the process.
    /// Reporting it as sent would clear the `done` we still owe the peers (who
    /// would show us typing until the TTL) and mute our retype for 3 seconds.
    ///
    /// The target buffer is captured BEFORE the submit: a submit can move the
    /// active buffer (`/join`, `/query`), and the typing state belongs to the
    /// buffer the text was typed in. A command's own sends report themselves
    /// through [`App::note_message_sent`] — the handlers are plain
    /// `fn(&mut App, &[String])` and have no outcome to return.
    pub(crate) fn submit_from_tui(&mut self, text: &str) {
        let target = self.state.active_buffer_id.clone();
        let sent_message = self.handle_submit(text);
        if let Some(target) = target {
            self.on_typing_submit(&TypingSource::Tui, &target, sent_message);
        }
    }

    /// A web session disconnected.
    pub(crate) fn on_web_session_gone(&mut self, session_id: &str) {
        let source = TypingSource::Web(session_id.to_string());
        let due = self.typing.remove_source(&source, Instant::now());
        self.dispatch_typing(due);
    }

    /// 1s tick: refresh, pause, flush pending retractions.
    pub(crate) fn typing_tick(&mut self) {
        let due = self.typing.on_tick(Instant::now());
        self.dispatch_typing(due);
    }

    /// Expire *received* typing state (6s active, 30s paused).
    pub(crate) fn expire_typing(&mut self) {
        for buffer_id in self.state.typing.expire(Instant::now()) {
            crate::irc::events::push_typing_web_event(&mut self.state, &buffer_id);
        }
    }

    fn dispatch_typing(&mut self, due: Vec<(String, TypingState)>) {
        let now = Instant::now();
        for (buffer_id, state) in due {
            // The buffer was closed out from under us — nothing left to notify,
            // and re-proposing it every tick forever would be a slow leak.
            if !self.state.buffers.contains_key(&buffer_id) {
                self.typing.forget_buffer(&buffer_id);
                continue;
            }
            if self.send_typing(&buffer_id, state) {
                self.typing.confirm_sent(&buffer_id, state, now);
            }
        }
    }

    /// Send one typing notification if every guard allows it.
    /// Returns whether it reached the wire — the machine only records confirmed sends.
    fn send_typing(&self, buffer_id: &str, state: TypingState) -> bool {
        send_typing_frame(
            &self.state.buffers,
            &self.state.connections,
            &self.config.typing,
            &self.irc_handles,
            buffer_id,
            state,
            Instant::now(),
        )
    }
}

/// The whole of [`App::send_typing`], over borrowed state and an injected clock
/// so the guard chain — the thing that decides whether the user's keystrokes
/// leave this process — is testable against a capturing sender.
fn send_typing_frame(
    buffers: &indexmap::IndexMap<String, crate::state::buffer::Buffer>,
    connections: &HashMap<String, crate::state::connection::Connection>,
    config: &crate::config::TypingConfig,
    handles: &HashMap<String, crate::irc::handle::IrcHandle>,
    buffer_id: &str,
    state: TypingState,
    now: Instant,
) -> bool {
    let Some((target, conn_id)) = typing_send_target(buffers, connections, config, buffer_id) else {
        return false;
    };

    let Some(handle) = handles.get(&conn_id) else {
        return false;
    };
    // Never hand a frame to the transport when THIS connection's budget is
    // tight (§3.1). A frame accepted under pressure is not dropped by the
    // crate — it is buffered and delayed, so it would land stale AND push
    // the user's next real message further back in the queue. The handle's
    // budget sees every send on this connection (registration, the WHO/MODE
    // burst on autojoin, lag PINGs, multiline pastes), and it is a no-op on
    // a connection opened with flood protection off.
    if !handle.sender().has_typing_headroom_at(now) {
        return false;
    }
    if handle
        .sender()
        .send_at(typing::build_tagmsg(&target, state), now)
        .is_err()
    {
        tracing::debug!("failed to send +typing={} to {target}", state.as_str());
        return false;
    }
    true
}

/// Where — if anywhere — a typing notification for `buffer_id` should go.
///
/// Everything `App::send_typing` decides *except* the flood question, which
/// needs the connection's live handle. Free, and over borrowed state, so the
/// guard chain is testable without an `App` (whose constructor touches disk).
///
/// `None` = suppressed. Returns `(target, connection_id)` otherwise: the target
/// is the buffer's channel or nick, which is what a TAGMSG is addressed to.
fn typing_send_target(
    buffers: &indexmap::IndexMap<String, crate::state::buffer::Buffer>,
    connections: &HashMap<String, crate::state::connection::Connection>,
    config: &crate::config::TypingConfig,
    buffer_id: &str,
) -> Option<(String, String)> {
    let buf = buffers.get(buffer_id)?;
    // Server, log, shell and DCC buffers have no channel or nick to TAGMSG.
    let allowed = match buf.buffer_type {
        BufferType::Channel => config.send_channels,
        BufferType::Query => config.send_queries,
        _ => false,
    };
    if !allowed {
        return None;
    }

    let conn = connections.get(&buf.connection_id)?;
    // The handle is inserted at `HandleReady`, as soon as the socket is up and
    // BEFORE CAP negotiation — while a target left holding `sent` by the drop
    // keeps re-proposing on every tick. Without this, the first tick after a
    // reconnect puts a TAGMSG on an unregistered connection (ERR_NOTREGISTERED),
    // and does it against the *previous* session's caps and ISUPPORT — bypassing
    // the two gates below exactly when the server may have changed them.
    if conn.status != crate::state::connection::ConnectionStatus::Connected {
        return None;
    }
    // No `message-tags` cap: the server would reject or ignore a TAGMSG.
    if !conn.enabled_caps.contains("message-tags") {
        return None;
    }
    // CLIENTTAGDENY (ISUPPORT) can forbid `+typing` specifically.
    if !conn.isupport_parsed.client_tag_allowed("typing") {
        return None;
    }

    Some((buf.name.clone(), buf.connection_id.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::irc::handle::{FLOOD_PENALTY_THRESHOLD_MS, IrcHandle, IrcSender};
    use crate::state::buffer::Buffer;
    use crate::state::connection::{Connection, ConnectionStatus};
    use std::time::Duration;

    fn t0() -> Instant {
        Instant::now()
    }

    /// Confirm every proposal, as `App` does when all guards pass.
    fn flush(s: &mut TypingSender, due: &[(String, TypingState)], now: Instant) {
        for (buffer_id, state) in due {
            s.confirm_sent(buffer_id, *state, now);
        }
    }

    fn tui() -> TypingSource {
        TypingSource::Tui
    }

    #[test]
    fn first_keystroke_proposes_active() {
        let mut s = TypingSender::default();
        let now = t0();
        let due = s.on_activity(tui(), "net/#rust", true, now);
        assert_eq!(due, vec![("net/#rust".to_string(), TypingState::Active)]);
        flush(&mut s, &due, now);
    }

    #[test]
    fn keystrokes_within_three_seconds_are_throttled() {
        // Spec: "MUST be throttled so that any typing notification is not sent
        // within 3 seconds of another one for a given target."
        let mut s = TypingSender::default();
        let now = t0();
        let due = s.on_activity(tui(), "net/#rust", true, now);
        flush(&mut s, &due, now);

        let t1 = now + Duration::from_millis(500);
        assert!(s.on_activity(tui(), "net/#rust", true, t1).is_empty());
        let t2 = now + Duration::from_secs(2);
        assert!(s.on_activity(tui(), "net/#rust", true, t2).is_empty());
    }

    #[test]
    fn done_obeys_the_three_second_throttle_and_is_flushed_by_the_tick() {
        // Clearing the input 200ms after `active` must NOT send `done` early —
        // the throttle is unqualified in the spec. It stays pending and the tick
        // emits it once the window opens.
        let mut s = TypingSender::default();
        let now = t0();
        let due = s.on_activity(tui(), "net/#rust", true, now);
        flush(&mut s, &due, now);

        let cleared = now + Duration::from_millis(200);
        assert!(s.on_activity(tui(), "net/#rust", false, cleared).is_empty());
        // Still throttled at 2s.
        assert!(s.on_tick(now + Duration::from_secs(2)).is_empty());
        // At 3s the window opens and the pending `done` goes out.
        let at3 = now + Duration::from_secs(3);
        let due = s.on_tick(at3);
        assert_eq!(due, vec![("net/#rust".to_string(), TypingState::Done)]);
        flush(&mut s, &due, at3);
        // And only once.
        assert!(s.on_tick(now + Duration::from_secs(10)).is_empty());
    }

    #[test]
    fn a_pending_done_is_coalesced_away_if_typing_resumes() {
        // Type, clear, retype — all inside the throttle window. The peer should
        // never see a `done`; the state never actually changed.
        let mut s = TypingSender::default();
        let now = t0();
        let due = s.on_activity(tui(), "net/#rust", true, now);
        flush(&mut s, &due, now);

        s.on_activity(tui(), "net/#rust", false, now + Duration::from_millis(500));
        s.on_activity(tui(), "net/#rust", true, now + Duration::from_secs(1));

        // At 3s the aggregate is `active` again, so a refresh goes out, not a done.
        let at3 = now + Duration::from_secs(3);
        assert_eq!(
            s.on_tick(at3),
            vec![("net/#rust".to_string(), TypingState::Active)]
        );
    }

    #[test]
    fn slash_commands_never_type() {
        // The caller applies `should_type`; this asserts the wiring of that.
        assert!(!crate::irc::typing::should_type("/join #x"));
        assert!(crate::irc::typing::should_type("/me waves"));
    }

    #[test]
    fn tick_resends_active_while_typing_continues() {
        let mut s = TypingSender::default();
        let now = t0();
        let due = s.on_activity(tui(), "net/#rust", true, now);
        flush(&mut s, &due, now);

        // 1s: inside the throttle, nothing.
        assert!(s.on_tick(now + Duration::from_secs(1)).is_empty());
        // Keystroke at 2s keeps it fresh; tick at 3s resends `active`.
        s.on_activity(tui(), "net/#rust", true, now + Duration::from_secs(2));
        assert_eq!(
            s.on_tick(now + Duration::from_secs(3)),
            vec![("net/#rust".to_string(), TypingState::Active)]
        );
    }

    #[test]
    fn tick_sends_paused_once_after_three_idle_seconds() {
        let mut s = TypingSender::default();
        let now = t0();
        let due = s.on_activity(tui(), "net/#rust", true, now);
        flush(&mut s, &due, now);

        // Text still there, no keystroke for 3s → paused, exactly once.
        let at3 = now + Duration::from_secs(3);
        let due = s.on_tick(at3);
        assert_eq!(due, vec![("net/#rust".to_string(), TypingState::Paused)]);
        flush(&mut s, &due, at3);

        assert!(s.on_tick(now + Duration::from_secs(7)).is_empty());
        assert!(s.on_tick(now + Duration::from_secs(30)).is_empty());
    }

    #[test]
    fn switching_buffers_releases_the_old_target_immediately() {
        // The user switches and types in the new buffer before the next tick.
        // The old target must still get its `done` — it must not be forgotten.
        let mut s = TypingSender::default();
        let now = t0();
        let due = s.on_activity(tui(), "net/#rust", true, now);
        flush(&mut s, &due, now);

        let later = now + Duration::from_secs(4);
        let mut due = s.on_activity(tui(), "net/#tokio", true, later);
        due.sort_by(|a, b| a.0.cmp(&b.0)); // TypingState has no Ord — sort by buffer id
        assert_eq!(
            due,
            vec![
                ("net/#rust".to_string(), TypingState::Done),
                ("net/#tokio".to_string(), TypingState::Active),
            ]
        );
    }

    #[test]
    fn an_idle_second_source_does_not_cancel_a_typing_one() {
        // A freshly-opened browser tab reports `typing: false` for the buffer the
        // terminal is actively typing in. The terminal must keep typing.
        let mut s = TypingSender::default();
        let now = t0();
        let due = s.on_activity(TypingSource::Tui, "net/#rust", true, now);
        flush(&mut s, &due, now);

        let tab = TypingSource::Web("tab-1".to_string());
        assert!(
            s.on_activity(tab, "net/#rust", false, now + Duration::from_millis(100)).is_empty(),
            "an idle source must not retract another source's typing"
        );

        // And the aggregate is still `active` at the next window.
        s.on_activity(TypingSource::Tui, "net/#rust", true, now + Duration::from_secs(2));
        assert_eq!(
            s.on_tick(now + Duration::from_secs(3)),
            vec![("net/#rust".to_string(), TypingState::Active)]
        );
    }

    #[test]
    fn done_goes_out_only_when_the_last_source_stops() {
        let mut s = TypingSender::default();
        let now = t0();
        let tab = TypingSource::Web("tab-1".to_string());
        let due = s.on_activity(TypingSource::Tui, "net/#rust", true, now);
        flush(&mut s, &due, now);
        s.on_activity(tab.clone(), "net/#rust", true, now);

        // The terminal stops; the tab is still typing → no done.
        let t4 = now + Duration::from_secs(4);
        s.on_activity(TypingSource::Tui, "net/#rust", false, t4);
        assert_ne!(
            s.on_tick(t4).first().map(|(_, st)| *st),
            Some(TypingState::Done)
        );

        // The tab stops too → done.
        let t8 = now + Duration::from_secs(8);
        s.on_activity(tab, "net/#rust", false, t8);
        assert_eq!(s.on_tick(t8), vec![("net/#rust".to_string(), TypingState::Done)]);
    }

    #[test]
    fn removing_a_source_releases_its_target() {
        // A browser tab closes mid-typing.
        let mut s = TypingSender::default();
        let now = t0();
        let tab = TypingSource::Web("tab-1".to_string());
        let due = s.on_activity(tab.clone(), "net/#rust", true, now);
        flush(&mut s, &due, now);

        let t4 = now + Duration::from_secs(4);
        assert_eq!(
            s.remove_source(&tab, t4),
            vec![("net/#rust".to_string(), TypingState::Done)]
        );
    }

    #[test]
    fn submit_sends_nothing_and_retires_the_target() {
        // The PRIVMSG itself clears typing at the receivers, so `done` is noise.
        let mut s = TypingSender::default();
        let now = t0();
        let due = s.on_activity(tui(), "net/#rust", true, now);
        flush(&mut s, &due, now);

        let t4 = now + Duration::from_secs(4);
        assert!(s.on_submit(&tui(), "net/#rust", true, t4).is_empty());
        assert!(s.on_tick(now + Duration::from_secs(8)).is_empty());
    }

    #[test]
    fn a_local_command_submit_still_owes_the_pending_done() {
        // `active` went out at t=0. At t=1s the input is cleared (Ctrl+U) and a
        // `/whois` typed instead: a `done` is now due but throttled. Submitting
        // that command at t=2s sends NOTHING to the network, so the retraction
        // is still owed — the rationale for retiring the target ("the PRIVMSG
        // itself clears typing at the receivers") only holds when a message
        // actually reached the wire. Dropping `sent` here leaves the peers
        // showing us as typing until the TTL runs out.
        let mut s = TypingSender::default();
        let now = t0();
        let due = s.on_activity(tui(), "net/#rust", true, now);
        flush(&mut s, &due, now);

        s.on_activity(tui(), "net/#rust", false, now + Duration::from_secs(1));
        assert!(
            s.on_submit(&tui(), "net/#rust", false, now + Duration::from_secs(2))
                .is_empty(),
            "still inside the 3s throttle window"
        );

        let at3 = now + Duration::from_secs(3);
        assert_eq!(
            s.on_tick(at3),
            vec![("net/#rust".to_string(), TypingState::Done)],
            "the done we owe the channel must survive a local command"
        );
    }

    #[test]
    fn a_sent_message_suppresses_typing_for_three_seconds() {
        // Flood budget: a TAGMSG costs as much as a short PRIVMSG (spec §3.1).
        // The message itself already announced our presence.
        let mut s = TypingSender::default();
        let now = t0();
        s.on_submit(&tui(), "net/#rust", true, now);

        assert!(s.on_activity(tui(), "net/#rust", true, now + Duration::from_secs(1)).is_empty());
        assert_eq!(
            s.on_activity(tui(), "net/#rust", true, now + Duration::from_secs(3)),
            vec![("net/#rust".to_string(), TypingState::Active)]
        );
    }

    #[test]
    fn a_local_command_does_not_suppress_typing() {
        // /help, /set etc. send nothing to the network: typing right after must
        // not be suppressed. (Whatever a command *does* put on the wire is
        // charged by the connection's own sender, not by this machine.)
        let mut s = TypingSender::default();
        let now = t0();
        s.on_submit(&tui(), "net/#rust", false, now);
        assert_eq!(
            s.on_activity(tui(), "net/#rust", true, now + Duration::from_secs(1)),
            vec![("net/#rust".to_string(), TypingState::Active)]
        );
    }

    #[test]
    fn typing_stops_when_the_connections_flood_budget_runs_low() {
        // A frame handed to the transport under pressure is QUEUED and delayed,
        // not dropped, so `App::send_typing` must refuse it up front. The gate
        // it consults is the connection's own budget.
        let now = t0();
        let conn = IrcSender::capturing(u64::from(FLOOD_PENALTY_THRESHOLD_MS));
        // Two channel WHOs — an ordinary autojoin — cost 6000ms.
        for chan in ["#other", "#more"] {
            conn.send_at(
                ::irc::proto::Command::WHO(Some(chan.to_string()), None),
                now,
            )
            .unwrap();
        }
        assert!(
            !conn.has_typing_headroom_at(now),
            "typing must not be handed to the transport when the budget is tight"
        );

        // Once the penalty has drained, typing resumes.
        assert!(conn.has_typing_headroom_at(now + Duration::from_secs(10)));
    }

    #[test]
    fn typing_is_suppressed_only_on_the_busy_connection() {
        // The regression. The crate's penalty counter lives inside each
        // connection's `Outgoing`, so a burst on Libera says nothing about
        // OFTC's budget. The old app-side mirror was ONE global estimate: two
        // messages on Libera silenced typing in an OFTC query whose budget was
        // untouched.
        let now = t0();
        let threshold = u64::from(FLOOD_PENALTY_THRESHOLD_MS);
        let libera = IrcSender::capturing(threshold);
        let oftc = IrcSender::capturing(threshold);

        // Autojoin on Libera: a WHO per channel. None of this went through the
        // old mirror, which is the second half of the bug — it read 0.
        for chan in ["#rust", "#tokio", "#linux"] {
            libera
                .send_at(
                    ::irc::proto::Command::WHO(Some(chan.to_string()), None),
                    now,
                )
                .unwrap();
        }
        assert!(
            !libera.has_typing_headroom_at(now),
            "the connection that actually spent the budget must be gated"
        );
        assert!(
            oftc.has_typing_headroom_at(now),
            "an idle connection's budget must be untouched by another's traffic"
        );

        // And a TAGMSG for a query on OFTC still reaches the wire.
        oftc.send_at(typing::build_tagmsg("bob", TypingState::Active), now)
            .unwrap();
        let sent = oftc.captured();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].to_string(), "@+typing=active TAGMSG bob\r\n");
    }

    #[test]
    fn the_machine_holds_no_cross_connection_flood_state() {
        // Submits in a buffer on one network must not gate typing in a buffer
        // on another. The budget is the connection's, not the machine's — the
        // machine proposes, `App::send_typing` asks the target's own handle.
        let mut s = TypingSender::default();
        let mut now = t0();
        for _ in 0..3 {
            s.on_submit(&tui(), "libera/#other", true, now);
            now += Duration::from_millis(100);
        }
        assert_eq!(
            s.on_activity(tui(), "oftc/#rust", true, now),
            vec![("oftc/#rust".to_string(), TypingState::Active)],
            "traffic on one connection must not suppress typing on another"
        );
    }

    // ── The privacy gate: `send_typing`'s guard chain ──
    //
    // Everything below drives the real chain over a capturing sender, so each
    // test asserts what did (or did not) reach the wire, not just a bool.

    /// A registered connection `net` with `message-tags` negotiated, one buffer
    /// of every type on it, and a capturing sender behind its handle.
    struct Wired {
        buffers: indexmap::IndexMap<String, Buffer>,
        connections: HashMap<String, Connection>,
        handles: HashMap<String, IrcHandle>,
        config: crate::config::TypingConfig,
        sender: IrcSender,
    }

    impl Wired {
        fn new() -> Self {
            let sender = IrcSender::capturing(u64::from(FLOOD_PENALTY_THRESHOLD_MS));
            let mut buffers = indexmap::IndexMap::new();
            for (name, buffer_type) in [
                ("#rust", BufferType::Channel),
                ("bob", BufferType::Query),
                ("NetServer", BufferType::Server),
                ("carol", BufferType::DccChat),
                ("shell", BufferType::Shell),
                ("Mentions", BufferType::Mentions),
                ("#logged", BufferType::Log),
            ] {
                buffers.insert(format!("net/{name}"), make_buffer(name, buffer_type));
            }
            let mut handles = HashMap::new();
            handles.insert(
                "net".to_string(),
                IrcHandle::new("net".to_string(), sender.clone(), None, None),
            );
            Self {
                buffers,
                connections: HashMap::from([("net".to_string(), make_connection())]),
                handles,
                config: crate::config::TypingConfig::default(),
                sender,
            }
        }

        fn send(&self, buffer_id: &str, state: TypingState, now: Instant) -> bool {
            send_typing_frame(
                &self.buffers,
                &self.connections,
                &self.config,
                &self.handles,
                buffer_id,
                state,
                now,
            )
        }

        /// Exactly what went out, as wire lines.
        fn wire(&self) -> Vec<String> {
            self.sender
                .captured()
                .iter()
                .map(ToString::to_string)
                .collect()
        }

        fn conn_mut(&mut self) -> &mut Connection {
            self.connections.get_mut("net").expect("connection")
        }
    }

    fn make_buffer(name: &str, buffer_type: BufferType) -> Buffer {
        Buffer {
            id: format!("net/{name}"),
            connection_id: "net".to_string(),
            buffer_type,
            name: name.to_string(),
            messages: std::collections::VecDeque::new(),
            activity: crate::state::buffer::ActivityLevel::None,
            unread_count: 0,
            last_read: chrono::Utc::now(),
            topic: None,
            topic_set_by: None,
            users: HashMap::new(),
            modes: None,
            mode_params: None,
            list_modes: HashMap::new(),
            last_speakers: Vec::new(),
            peer_handle: None,
            log_total_lines: None,
            log_oldest_ts: None,
            log_newest_ts: None,
            history_exhausted: false,
            log_initial_loaded: false,
            pin_backlog: false,
        }
    }

    fn make_connection() -> Connection {
        Connection {
            id: "net".to_string(),
            label: "NetServer".to_string(),
            status: ConnectionStatus::Connected,
            own_handle: None,
            nick: "me".to_string(),
            user_modes: String::new(),
            isupport: HashMap::new(),
            isupport_parsed: crate::irc::isupport::Isupport::new(),
            error: None,
            lag: None,
            lag_pending: false,
            reconnect_attempts: 0,
            reconnect_delay_secs: 30,
            next_reconnect: None,
            should_reconnect: true,
            joined_channels: Vec::new(),
            origin_config: crate::config::ServerConfig {
                label: "NetServer".to_string(),
                address: "irc.test.net".to_string(),
                port: 6697,
                tls: true,
                tls_verify: true,
                autoconnect: false,
                channels: vec![],
                nick: None,
                username: None,
                realname: None,
                password: None,
                sasl_user: None,
                sasl_pass: None,
                bind_ip: None,
                encoding: None,
                auto_reconnect: Some(true),
                reconnect_delay: None,
                reconnect_max_retries: None,
                autosendcmd: None,
                sasl_mechanism: None,
                client_cert_path: None,
            },
            local_ip: None,
            enabled_caps: std::collections::HashSet::from(["message-tags".to_string()]),
            chathistory: crate::irc::chathistory::HistoryState::new(),
            who_token_counter: 0,
            silent_who_channels: std::collections::HashSet::new(),
            silent_banlist_channels: std::collections::HashSet::new(),
            multiline: None,
            batch_ref_counter: 0,
        }
    }

    #[test]
    fn a_channel_and_a_query_are_the_only_things_we_ever_type_into() {
        let w = Wired::new();
        let now = t0();

        assert!(w.send("net/#rust", TypingState::Active, now));
        assert!(w.send("net/bob", TypingState::Done, now));
        assert_eq!(
            w.wire(),
            vec![
                "@+typing=active TAGMSG #rust\r\n",
                "@+typing=done TAGMSG bob\r\n",
            ]
        );

        // Everything else has no channel or nick to address a TAGMSG to, and
        // typing into it would leak keystrokes to a peer that never asked.
        for buffer_id in [
            "net/NetServer",
            "net/carol",
            "net/shell",
            "net/Mentions",
            "net/#logged",
            "net/nonexistent",
        ] {
            assert!(
                !w.send(buffer_id, TypingState::Active, now),
                "{buffer_id} must never emit a typing notification"
            );
        }
        assert_eq!(w.wire().len(), 2, "nothing else reached the wire");
    }

    #[test]
    fn send_channels_off_silences_channels_and_leaves_queries_alone() {
        // Swapping the two config branches would still pass a test that only
        // looked at one of them.
        let mut w = Wired::new();
        w.config.send_channels = false;
        let now = t0();

        assert!(!w.send("net/#rust", TypingState::Active, now));
        assert!(w.send("net/bob", TypingState::Active, now));
        assert_eq!(w.wire(), vec!["@+typing=active TAGMSG bob\r\n"]);
    }

    #[test]
    fn send_queries_off_silences_queries_and_leaves_channels_alone() {
        let mut w = Wired::new();
        w.config.send_queries = false;
        let now = t0();

        assert!(!w.send("net/bob", TypingState::Active, now));
        assert!(w.send("net/#rust", TypingState::Active, now));
        assert_eq!(w.wire(), vec!["@+typing=active TAGMSG #rust\r\n"]);
    }

    #[test]
    fn without_the_message_tags_cap_nothing_is_sent() {
        // The server would reject or ignore the TAGMSG.
        let mut w = Wired::new();
        w.conn_mut().enabled_caps.clear();
        assert!(!w.send("net/#rust", TypingState::Active, t0()));
        assert!(w.wire().is_empty());
    }

    #[test]
    fn clienttagdeny_blocks_typing() {
        // `CLIENTTAGDENY=*` blocks everything; `-typing` exempts us again.
        let mut w = Wired::new();
        w.conn_mut().isupport_parsed.parse_tokens(&["CLIENTTAGDENY=*"]);
        assert!(!w.send("net/#rust", TypingState::Active, t0()));
        assert!(w.wire().is_empty());

        w.conn_mut()
            .isupport_parsed
            .parse_tokens(&["CLIENTTAGDENY=*,-typing"]);
        assert!(w.send("net/#rust", TypingState::Active, t0()));
        assert_eq!(w.wire(), vec!["@+typing=active TAGMSG #rust\r\n"]);
    }

    #[test]
    fn an_unregistered_connection_is_never_sent_a_tagmsg() {
        // The handle is re-inserted at `HandleReady` — as soon as the socket is
        // up, BEFORE CAP negotiation and registration. A target still holding
        // `sent = Some(Active)` from before the drop keeps re-proposing on every
        // tick, so without a status check the first tick after the reconnect
        // writes a TAGMSG to an unregistered connection (ERR_NOTREGISTERED),
        // using the PREVIOUS session's caps and ISUPPORT.
        let now = t0();
        for status in [
            ConnectionStatus::Connecting,
            ConnectionStatus::Disconnected,
            ConnectionStatus::Error,
        ] {
            let mut w = Wired::new();
            w.conn_mut().status = status.clone();
            assert!(
                !w.send("net/#rust", TypingState::Active, now),
                "typing must not be sent while the connection is {status:?}"
            );
            assert!(w.wire().is_empty());
        }
    }

    #[test]
    fn a_connection_without_a_handle_sends_nothing() {
        let mut w = Wired::new();
        w.handles.clear();
        assert!(!w.send("net/#rust", TypingState::Active, t0()));
    }

    #[test]
    fn a_tight_flood_budget_keeps_the_tagmsg_off_the_wire() {
        // The handle's budget is the gate — a frame accepted under pressure is
        // queued and delayed, not dropped.
        let w = Wired::new();
        let now = t0();
        for chan in ["#other", "#more"] {
            w.sender
                .send_at(::irc::proto::Command::WHO(Some(chan.to_string()), None), now)
                .expect("capture");
        }
        assert!(!w.send("net/#rust", TypingState::Active, now));
        assert_eq!(w.wire().len(), 2, "only the two WHOs went out");

        // And it resumes once the penalty has drained.
        let later = now + Duration::from_secs(10);
        assert!(w.send("net/#rust", TypingState::Active, later));
        assert_eq!(w.wire().len(), 3);
    }

    #[test]
    fn forget_buffer_stops_a_closed_buffer_from_being_reproposed() {
        // A closed buffer must not leak forever: a source still pointed at it
        // (or a target still holding `sent`) would otherwise get re-proposed
        // on every tick with nowhere to send it.
        let mut s = TypingSender::default();
        let now = t0();
        let due = s.on_activity(tui(), "net/#rust", true, now);
        flush(&mut s, &due, now);

        s.forget_buffer("net/#rust");

        assert!(s.on_tick(now + Duration::from_secs(4)).is_empty());
    }
}
