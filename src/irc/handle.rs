//! The send half of a connection, and the flood budget it charges.
//!
//! The crate's `Outgoing` future keeps a per-connection penalty counter and
//! **delays** — never drops — frames that push it past the threshold
//! (`irc-repartee-1.5.1/src/client/mod.rs`, `Outgoing::poll`). Anything we hand
//! to `Sender::send` while the penalty is high is buffered and arrives late,
//! behind the user's real messages.
//!
//! Opportunistic traffic (`+typing` notifications) therefore has to decide
//! *before* the frame reaches the queue, which means keeping a faithful mirror
//! of that counter on this side. A mirror is only worth anything if it sees
//! **every** send, so the raw `irc::client::Sender` is private to this module
//! and the only way to reach the socket is [`IrcSender::send`], which charges
//! the budget on the way through. There is no bypass to forget about.

use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use irc::proto::{Command, Message};

use crate::irc::typing::{self, TypingState};

/// The crate's default penalty threshold, matching the `IRCd` excess-flood limit.
/// `0` disables throttling entirely (`general.flood_protection = false`).
pub const FLOOD_PENALTY_THRESHOLD_MS: u32 = 10_000;

/// Budget we refuse to spend on typing, so the user's real messages keep theirs.
const RESERVED_MS: u64 = 4_000;

/// Penalty in milliseconds the crate charges for a command.
///
/// Copied verbatim from `Outgoing::command_penalty`
/// (`irc-repartee-1.5.1/src/client/mod.rs`). If that table changes, this must
/// change with it.
#[expect(
    clippy::match_same_arms,
    reason = "the arms mirror the crate's table one-for-one, including the ones \
              that happen to share the catch-all's 2000ms; collapsing them would \
              hide which commands the crate names explicitly and make a future \
              divergence in its table invisible here"
)]
fn command_penalty(command: &Command) -> u64 {
    match command {
        // Connection control and CAP/SASL negotiation are exempt.
        Command::PONG(..)
        | Command::QUIT(..)
        | Command::PASS(..)
        | Command::CAP(..)
        | Command::AUTHENTICATE(..) => 0,
        Command::NICK(..) => 3000,
        Command::PART(..) => 4000,
        // Argument-less WHO/LIST/NAMES are catastrophic on IRCd (10s); with an
        // argument they are 2s.
        Command::WHO(mask, _) | Command::LIST(mask, _) | Command::NAMES(mask, _) => {
            match mask.as_deref() {
                None | Some("") => 10_000,
                Some(_) => 2000,
            }
        }
        Command::WHOIS(..) | Command::WHOWAS(..) | Command::LINKS(..) | Command::STATS(..) => 3000,
        Command::LUSERS(..) | Command::TRACE(..) => 2000,
        Command::USERS(..) | Command::MOTD(..) | Command::INFO(..) => 5000,
        Command::PRIVMSG(..) | Command::NOTICE(..) => 2000,
        // JOIN, KICK, INVITE, MODE, TOPIC, AWAY, `Raw` (our TAGMSG) and the rest.
        _ => 2000,
    }
}

/// Base penalty in milliseconds derived from the serialized length:
/// `(1 + message_bytes / 100) * 1000`.
fn length_penalty(message: &Message) -> u64 {
    let len = u64::try_from(message.to_string().len()).unwrap_or(u64::MAX);
    (1 + len / 100) * 1000
}

/// What the crate's `Outgoing` will charge for `message`.
///
/// A zero `command_penalty` means the crate skips the whole penalty block, so
/// the message costs **nothing at all** — not even its length penalty.
fn message_cost(message: &Message) -> u64 {
    let command = command_penalty(&message.command);
    if command == 0 {
        0
    } else {
        command + length_penalty(message)
    }
}

/// What one `+typing` notification will cost this connection, as a lower bound.
///
/// Derived from the real cost function rather than hard-coded, so it tracks
/// [`command_penalty`]. The target name *can* move the answer: a target long
/// enough to push `@+typing=active TAGMSG <target>\r\n` past 100 bytes (~67
/// bytes of target) adds one more length step, i.e. 4000 instead of 3000. The
/// gate deliberately ignores that: the error is bounded by a single 1000ms step
/// and is absorbed by the 4000ms [`RESERVED_MS`] reserve, so a long target can
/// at worst leave 3000ms of reserve instead of 4000 — never a delayed frame.
/// The *charge* on the way out is always computed from the real message.
fn tagmsg_cost() -> u64 {
    message_cost(&typing::build_tagmsg("#channel", TypingState::Active))
}

/// A faithful mirror of one connection's outgoing penalty counter.
#[derive(Debug)]
struct FloodEstimate {
    penalty_ms: u64,
    last_drain: Option<Instant>,
}

impl FloodEstimate {
    const fn new() -> Self {
        Self {
            penalty_ms: 0,
            last_drain: None,
        }
    }

    /// Penalty drains 1ms per 1ms of elapsed real time.
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

    /// Room for one more TAGMSG without eating into the reserve the user's real
    /// messages need? Callers must have ruled out a zero threshold first.
    fn has_typing_headroom(&mut self, now: Instant, threshold_ms: u64) -> bool {
        self.drain(now);
        self.penalty_ms + tagmsg_cost() <= threshold_ms.saturating_sub(RESERVED_MS)
    }
}

/// Where a charged frame actually goes.
#[derive(Debug, Clone)]
enum Wire {
    Live(irc::client::Sender),
    /// Test double: records frames instead of putting them on a socket. There
    /// is no way to build an `irc::client::Sender` without a live connection.
    #[cfg(test)]
    Capture(Arc<Mutex<Vec<Message>>>),
}

/// The send half of one IRC connection, with its flood budget attached.
///
/// Cloning is cheap and shares the budget — a clone is the *same* connection,
/// so it must charge the same counter.
#[derive(Debug, Clone)]
pub struct IrcSender {
    wire: Wire,
    budget: Arc<Mutex<FloodEstimate>>,
    /// The connection's `flood_penalty_threshold`. `0` = the crate applies no
    /// penalty at all, so there is nothing to mirror and no headroom to run out
    /// of. Held outside the mutex so that case costs neither a lock nor a
    /// serialization.
    threshold_ms: u64,
}

impl IrcSender {
    pub(crate) fn new(sender: irc::client::Sender, penalty_threshold_ms: u64) -> Self {
        Self {
            wire: Wire::Live(sender),
            budget: Arc::new(Mutex::new(FloodEstimate::new())),
            threshold_ms: penalty_threshold_ms,
        }
    }

    /// Put a message on the wire, charging this connection's flood budget.
    pub fn send<M: Into<Message>>(&self, message: M) -> irc::error::Result<()> {
        self.send_at(message, Instant::now())
    }

    /// [`Self::send`] with an injected clock.
    pub(crate) fn send_at<M: Into<Message>>(
        &self,
        message: M,
        now: Instant,
    ) -> irc::error::Result<()> {
        let message = message.into();
        // With the threshold at 0 the crate skips its whole penalty block
        // (`client/mod.rs:1150`), so mirroring it would be a lock and a full
        // `to_string()` per frame for a counter nobody ever reads.
        if self.threshold_ms > 0 {
            let cost = message_cost(&message);
            if cost > 0 {
                self.budget_mut().charge(now, cost);
            }
        }
        match &self.wire {
            Wire::Live(sender) => sender.send(message),
            #[cfg(test)]
            Wire::Capture(frames) => {
                frames
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(message);
                Ok(())
            }
        }
    }

    /// Is there room for one more `+typing` notification on **this** connection
    /// without eating into the budget the user's real messages need?
    #[must_use]
    pub fn has_typing_headroom(&self) -> bool {
        self.has_typing_headroom_at(Instant::now())
    }

    /// [`Self::has_typing_headroom`] with an injected clock.
    pub(crate) fn has_typing_headroom_at(&self, now: Instant) -> bool {
        if self.threshold_ms == 0 {
            return true;
        }
        self.budget_mut()
            .has_typing_headroom(now, self.threshold_ms)
    }

    fn budget_mut(&self) -> std::sync::MutexGuard<'_, FloodEstimate> {
        // Nothing under this lock can panic, so poisoning is unreachable —
        // but a poisoned budget must never take down a send either way.
        self.budget.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Sends a message, splitting on `\r\n` like the crate's `send_privmsg`.
    pub fn send_privmsg<S1, S2>(&self, target: S1, message: S2) -> irc::error::Result<()>
    where
        S1: fmt::Display,
        S2: fmt::Display,
    {
        let message = message.to_string();
        for line in message.split("\r\n") {
            self.send(Command::PRIVMSG(target.to_string(), line.to_string()))?;
        }
        Ok(())
    }

    /// Sends a notice, splitting on `\r\n` like the crate's `send_notice`.
    pub fn send_notice<S1, S2>(&self, target: S1, message: S2) -> irc::error::Result<()>
    where
        S1: fmt::Display,
        S2: fmt::Display,
    {
        let message = message.to_string();
        for line in message.split("\r\n") {
            self.send(Command::NOTICE(target.to_string(), line.to_string()))?;
        }
        Ok(())
    }

    /// Joins a channel or comma-separated chanlist.
    pub fn send_join<S: fmt::Display>(&self, chanlist: S) -> irc::error::Result<()> {
        self.send(Command::JOIN(chanlist.to_string(), None, None))
    }

    /// Quits the server with the given message.
    pub fn send_quit<S: fmt::Display>(&self, msg: S) -> irc::error::Result<()> {
        self.send(Command::QUIT(Some(msg.to_string())))
    }
}

/// Handle to a connected IRC client, holding the connection ID and send half.
#[derive(Debug)]
pub struct IrcHandle {
    pub conn_id: String,
    /// Private on purpose: the only way out is [`IrcSender`], which charges.
    sender: IrcSender,
    /// Local IP of the TCP socket (for DCC own-IP fallback).
    pub local_ip: Option<std::net::IpAddr>,
    /// Handle to the outgoing message task spawned by the irc crate.
    /// Aborted on disconnect to prevent CLOSE-WAIT socket leaks.
    pub outgoing_handle: Option<tokio::task::JoinHandle<()>>,
}

impl IrcHandle {
    /// Takes the connection's [`IrcSender`] rather than minting one, so the
    /// handle **inherits the budget registration already charged**. The crate
    /// charges NICK + USER (~7000ms) before this handle exists; a fresh,
    /// zeroed mirror here would read 0 while the real counter sat near the
    /// threshold, and typing frames sent on that false reading would delay the
    /// user's first real message.
    pub(crate) const fn new(
        conn_id: String,
        sender: IrcSender,
        local_ip: Option<std::net::IpAddr>,
        outgoing_handle: Option<tokio::task::JoinHandle<()>>,
    ) -> Self {
        Self {
            conn_id,
            sender,
            local_ip,
            outgoing_handle,
        }
    }

    /// The charging send half. Cheap to clone when a call site needs to drop
    /// its borrow of `App` before sending.
    pub const fn sender(&self) -> &IrcSender {
        &self.sender
    }
}

#[cfg(test)]
impl IrcSender {
    /// A sender that records frames instead of writing them to a socket.
    pub(crate) fn capturing(penalty_threshold_ms: u64) -> Self {
        Self {
            wire: Wire::Capture(Arc::new(Mutex::new(Vec::new()))),
            budget: Arc::new(Mutex::new(FloodEstimate::new())),
            threshold_ms: penalty_threshold_ms,
        }
    }

    /// This connection's mirrored penalty, as of its last charge or drain.
    pub(crate) fn penalty_ms(&self) -> u64 {
        self.budget_mut().penalty_ms
    }

    /// The frames handed to this sender so far.
    pub(crate) fn captured(&self) -> Vec<Message> {
        match &self.wire {
            // Loudly, rather than as an empty vec: a test that asserted on the
            // frames of an accidentally-live sender would otherwise pass while
            // observing nothing at all.
            Wire::Live(_) => unreachable!("captured() on a live sender — build it with capturing()"),
            Wire::Capture(frames) => frames
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn privmsg(text: &str) -> Message {
        Command::PRIVMSG("#rust".to_string(), text.to_string()).into()
    }

    #[test]
    fn cost_matches_the_crate_for_each_command_class() {
        // `length_penalty` for every frame here is the 1000ms floor (all are
        // well under 100 bytes), so `cost = command_penalty + 1000` — except
        // for the exempt commands, which are charged NOTHING, not even length.
        let cases: Vec<(Message, u64)> = vec![
            // Exempt: charged NOTHING, not even the length penalty.
            (Command::PONG("a".to_string(), None).into(), 0),
            (
                Command::CAP(None, irc::proto::CapSubCommand::END, None, None).into(),
                0,
            ),
            (Command::QUIT(Some("bye".to_string())).into(), 0),
            (Command::PASS("hunter2".to_string()).into(), 0),
            (Command::AUTHENTICATE("PLAIN".to_string()).into(), 0),
            // Named arms.
            (Command::NICK("bob".to_string()).into(), 3000 + 1000),
            (Command::PART("#rust".to_string(), None).into(), 4000 + 1000),
            // WHO / LIST / NAMES: 10s with no mask (or an empty one), 2s with.
            (Command::WHO(None, None).into(), 10_000 + 1000),
            (
                Command::WHO(Some(String::new()), None).into(),
                10_000 + 1000,
            ),
            (
                Command::WHO(Some("#rust".to_string()), None).into(),
                2000 + 1000,
            ),
            (Command::LIST(None, None).into(), 10_000 + 1000),
            (
                Command::LIST(Some(String::new()), None).into(),
                10_000 + 1000,
            ),
            (
                Command::LIST(Some("#rust".to_string()), None).into(),
                2000 + 1000,
            ),
            (Command::NAMES(None, None).into(), 10_000 + 1000),
            (
                Command::NAMES(Some(String::new()), None).into(),
                10_000 + 1000,
            ),
            (
                Command::NAMES(Some("#rust".to_string()), None).into(),
                2000 + 1000,
            ),
            // The 3000 group.
            (Command::WHOIS(None, "bob".to_string()).into(), 3000 + 1000),
            (
                Command::WHOWAS("bob".to_string(), None, None).into(),
                3000 + 1000,
            ),
            (Command::LINKS(None, None).into(), 3000 + 1000),
            (Command::STATS(None, None).into(), 3000 + 1000),
            // The 2000 group named explicitly by the crate.
            (Command::LUSERS(None, None).into(), 2000 + 1000),
            (Command::TRACE(None).into(), 2000 + 1000),
            // The 5000 group.
            (Command::USERS(None).into(), 5000 + 1000),
            (Command::MOTD(None).into(), 5000 + 1000),
            (Command::INFO(None).into(), 5000 + 1000),
            // Messaging.
            (privmsg("hi"), 2000 + 1000),
            (
                Command::NOTICE("#rust".to_string(), "hi".to_string()).into(),
                2000 + 1000,
            ),
            // The catch-all: JOIN and friends, and...
            (
                Command::JOIN("#rust".to_string(), None, None).into(),
                2000 + 1000,
            ),
            (
                // ...TAGMSG — what `+typing` rides on. `Command::Raw` falls to
                // the catch-all 2000, exactly as it does inside the crate.
                Command::Raw("TAGMSG".to_string(), vec!["#rust".to_string()]).into(),
                2000 + 1000,
            ),
        ];
        for (message, expected) in cases {
            assert_eq!(
                message_cost(&message),
                expected,
                "wrong cost for {}",
                message.to_string().trim_end()
            );
        }
    }

    #[test]
    fn length_penalty_is_one_second_per_hundred_bytes() {
        // `PRIVMSG #rust :<text>\r\n` — 16 bytes of envelope, including the
        // CRLF the crate counts. 234 bytes of text makes the serialized frame
        // 250 bytes → (1 + 250 / 100) * 1000 = 3000.
        let message = privmsg(&"x".repeat(234));
        assert_eq!(message.to_string().len(), 250);
        assert_eq!(length_penalty(&message), 3000);
        // And the command penalty rides on top of it.
        assert_eq!(message_cost(&message), 2000 + 3000);
    }

    #[test]
    fn penalty_drains_one_ms_per_ms() {
        let now = Instant::now();
        let mut budget = FloodEstimate::new();
        budget.charge(now, 5000);
        assert_eq!(budget.penalty_ms, 5000);

        budget.drain(now + Duration::from_millis(1500));
        assert_eq!(budget.penalty_ms, 3500);

        // It floors at zero rather than going negative.
        budget.drain(now + Duration::from_mins(1));
        assert_eq!(budget.penalty_ms, 0);
    }

    #[test]
    fn a_disabled_flood_protection_always_has_headroom() {
        // threshold 0 = `general.flood_protection = false`: the crate applies no
        // penalty at all, so the gate must never suppress anything.
        let now = Instant::now();
        let sender = IrcSender::capturing(0);
        for _ in 0..50 {
            sender.send_at(Command::WHO(None, None), now).unwrap();
        }
        assert!(sender.has_typing_headroom_at(now));
    }

    #[test]
    fn headroom_closes_after_a_burst_and_reopens_as_it_drains() {
        let now = Instant::now();
        let sender = IrcSender::capturing(u64::from(FLOOD_PENALTY_THRESHOLD_MS));
        assert!(sender.has_typing_headroom_at(now));

        // One WHO for a channel costs 3000 — the reserve leaves room for at most
        // 3000ms of penalty (10_000 - 4_000 reserve - 3_000 TAGMSG).
        sender
            .send_at(Command::WHO(Some("#rust".to_string()), None), now)
            .unwrap();
        assert!(sender.has_typing_headroom_at(now), "3000ms still fits");

        sender
            .send_at(Command::WHO(Some("#tokio".to_string()), None), now)
            .unwrap();
        assert!(
            !sender.has_typing_headroom_at(now),
            "6000ms of penalty leaves no room for a TAGMSG"
        );

        // 3s of drain brings it back to 3000.
        assert!(sender.has_typing_headroom_at(now + Duration::from_secs(3)));
    }

    #[test]
    fn every_send_charges_the_budget() {
        // The whole point of the private `Sender`: there is no path to the
        // socket that skips the counter. An autojoin WHO burst — which the old
        // app-side mirror never saw — closes the gate.
        let now = Instant::now();
        let sender = IrcSender::capturing(u64::from(FLOOD_PENALTY_THRESHOLD_MS));
        for chan in ["#a", "#b", "#c"] {
            sender
                .send_at(Command::WHO(Some(chan.to_string()), None), now)
                .unwrap();
        }
        assert_eq!(sender.captured().len(), 3, "frames still reach the wire");
        assert!(!sender.has_typing_headroom_at(now));
    }

    #[test]
    fn exempt_commands_are_charged_nothing_at_all() {
        // The crate guards the whole penalty block with `if cmd_cost > 0`, so a
        // long CAP REQ or a QUIT costs zero — not even its length penalty.
        let now = Instant::now();
        let sender = IrcSender::capturing(u64::from(FLOOD_PENALTY_THRESHOLD_MS));
        for _ in 0..20 {
            sender
                .send_at(
                    Command::CAP(None, irc::proto::CapSubCommand::END, None, None),
                    now,
                )
                .unwrap();
        }
        assert!(sender.has_typing_headroom_at(now));
    }
}
