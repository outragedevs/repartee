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

/// Mirrors `flood_penalty_threshold` in `src/irc/mod.rs:396`.
const PENALTY_THRESHOLD_MS: u64 = 10_000;
/// `command_penalty` for `Command::Raw` (2000) + `length_penalty` for a short
/// frame (1000) — `irc-repartee-1.5.1/src/client/mod.rs:992-1045`.
const TAGMSG_COST_MS: u64 = 3_000;
/// The same, for a short PRIVMSG.
const MESSAGE_COST_MS: u64 = 3_000;
/// Budget we refuse to spend on typing, so the user's real messages keep theirs.
const RESERVED_MS: u64 = 4_000;

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

/// A conservative mirror of the crate's outgoing penalty counter.
///
/// This has to exist because `Sender::send` feeds an `UnboundedSender`
/// (`client/mod.rs:940-947`): a frame handed over while the penalty is high is
/// not dropped, it is **buffered and delayed** (`client/mod.rs:1166-1180`). A
/// typing notification that arrives late is worse than none, and it spends
/// budget the user's next real message needs. So the decision not to send has to
/// be taken here, before the frame reaches the queue.
///
/// If the crate's penalty formula changes, this must change with it.
#[derive(Debug, Default)]
struct FloodEstimate {
    penalty_ms: u64,
    last_drain: Option<Instant>,
}

impl FloodEstimate {
    fn drain(&mut self, now: Instant) {
        if let Some(last) = self.last_drain {
            let elapsed = u64::try_from(now.duration_since(last).as_millis()).unwrap_or(u64::MAX);
            self.penalty_ms = self.penalty_ms.saturating_sub(elapsed);
        }
        self.last_drain = Some(now);
    }

    fn charge(&mut self, now: Instant, cost_ms: u64) {
        self.drain(now);
        self.penalty_ms = self.penalty_ms.saturating_add(cost_ms);
    }

    /// Room for one more TAGMSG without eating into the reserve?
    fn has_headroom(&mut self, now: Instant) -> bool {
        self.drain(now);
        self.penalty_ms + TAGMSG_COST_MS <= PENALTY_THRESHOLD_MS.saturating_sub(RESERVED_MS)
    }
}

/// Proposes typing notifications. Pure — no clock, no I/O.
#[derive(Debug, Default)]
pub struct TypingSender {
    sources: HashMap<TypingSource, SourceState>,
    targets: HashMap<String, TargetState>,
    /// Real messages we sent, per target — for the §3.1 suppression window.
    last_message: HashMap<String, Instant>,
    flood: FloodEstimate,
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

    /// A source submitted a message. Sends nothing: the PRIVMSG itself clears
    /// typing at the receivers.
    pub fn on_submit(
        &mut self,
        source: &TypingSource,
        buffer_id: &str,
        now: Instant,
    ) -> Vec<(String, TypingState)> {
        self.last_message.insert(buffer_id.to_string(), now);
        self.flood.charge(now, MESSAGE_COST_MS);
        if let Some(s) = self.sources.get_mut(source) {
            s.active = false;
            s.last_activity = now;
        }
        // Retire our state for this target without a `done`.
        if let Some(t) = self.targets.get_mut(buffer_id) {
            t.sent = None;
        }
        // Another source may still be typing here, so re-derive rather than assume.
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
    /// after every guard passed and `Sender::send` succeeded.
    pub fn confirm_sent(&mut self, buffer_id: &str, state: TypingState, now: Instant) {
        self.flood.charge(now, TAGMSG_COST_MS);
        let entry = self.targets.entry(buffer_id.to_string()).or_default();
        entry.last_sent = Some(now);
        entry.sent = (state != TypingState::Done).then_some(state);
    }

    fn propose(&mut self, buffer_ids: &[String], now: Instant) -> Vec<(String, TypingState)> {
        let mut out = Vec::new();
        for buffer_id in buffer_ids {
            if let Some(state) = self.next_state(buffer_id, now) {
                out.push((buffer_id.clone(), state));
            }
        }
        out
    }

    /// The notification this target needs right now, if any.
    fn next_state(&mut self, buffer_id: &str, now: Instant) -> Option<TypingState> {
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
        // Never hand a frame to the transport when the budget is tight (§3.1).
        if !self.flood.has_headroom(now) {
            return None;
        }
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

    /// A message was submitted from `source`.
    pub(crate) fn on_typing_submit(&mut self, source: &TypingSource, buffer_id: &str) {
        let due = self.typing.on_submit(source, buffer_id, Instant::now());
        self.dispatch_typing(due);
    }

    /// Note a TUI-driven submit for the active buffer. Every `handle_submit`
    /// call site in `input.rs` must go through this first, or the machine keeps
    /// an `Active` recorded for the target and the next input change retracts
    /// it with a spurious `done` right after the real PRIVMSG.
    pub(crate) fn note_tui_submit(&mut self) {
        if let Some(buffer_id) = self.state.active_buffer_id.clone() {
            self.on_typing_submit(&TypingSource::Tui, &buffer_id);
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
            if self.send_typing(&buffer_id, state) {
                self.typing.confirm_sent(&buffer_id, state, now);
            }
        }
    }

    /// Send one typing notification if every guard allows it.
    /// Returns whether it reached the wire — the machine only records confirmed sends.
    fn send_typing(&self, buffer_id: &str, state: TypingState) -> bool {
        let Some(buf) = self.state.buffers.get(buffer_id) else {
            return false;
        };
        // Server, log, shell and DCC buffers have no channel or nick to TAGMSG.
        let allowed = match buf.buffer_type {
            BufferType::Channel => self.config.typing.send_channels,
            BufferType::Query => self.config.typing.send_queries,
            _ => false,
        };
        if !allowed {
            return false;
        }
        let target = buf.name.clone();
        let conn_id = buf.connection_id.clone();

        let Some(conn) = self.state.connections.get(&conn_id) else {
            return false;
        };
        if !conn.enabled_caps.contains("message-tags") {
            return false;
        }
        if !conn.isupport_parsed.client_tag_allowed("typing") {
            return false;
        }

        let Some(handle) = self.irc_handles.get(&conn_id) else {
            return false;
        };
        if handle
            .sender
            .send(typing::build_tagmsg(&target, state))
            .is_err()
        {
            tracing::debug!("failed to send +typing={} to {target}", state.as_str());
            return false;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
        assert!(s.on_submit(&tui(), "net/#rust", t4).is_empty());
        assert!(s.on_tick(now + Duration::from_secs(8)).is_empty());
    }

    #[test]
    fn a_sent_message_suppresses_typing_for_three_seconds() {
        // Flood budget: a TAGMSG costs as much as a short PRIVMSG (spec §3.1).
        // The message itself already announced our presence.
        let mut s = TypingSender::default();
        let now = t0();
        s.on_submit(&tui(), "net/#rust", now);

        assert!(s.on_activity(tui(), "net/#rust", true, now + Duration::from_secs(1)).is_empty());
        assert_eq!(
            s.on_activity(tui(), "net/#rust", true, now + Duration::from_secs(3)),
            vec![("net/#rust".to_string(), TypingState::Active)]
        );
    }

    #[test]
    fn typing_stops_when_the_flood_budget_runs_low() {
        // A frame handed to Sender::send under pressure is QUEUED and delayed,
        // not dropped (client/mod.rs:1166-1180). So we must refuse to send here.
        let mut s = TypingSender::default();
        let mut now = t0();
        // Burn the budget with real messages in another buffer.
        for _ in 0..3 {
            s.on_submit(&tui(), "net/#other", now);
            now += Duration::from_millis(100);
        }
        // Typing in a fresh buffer is now refused: no headroom left.
        assert!(
            s.on_activity(tui(), "net/#rust", true, now).is_empty(),
            "typing must not be handed to the transport when the budget is tight"
        );

        // Once the penalty has drained, typing resumes.
        let later = now + Duration::from_secs(10);
        assert_eq!(
            s.on_activity(tui(), "net/#rust", true, later),
            vec![("net/#rust".to_string(), TypingState::Active)]
        );
    }
}
