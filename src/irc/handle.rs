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
//! of that counter on this side.
//!
//! # The governing rule: the mirror must never under-read
//!
//! Over-charging is safe — it only suppresses **our own** typing, which is
//! opportunistic by definition. Under-charging is harmful: it lets us hand a
//! TAGMSG to a queue that is already throttling, where it is buffered, arrives
//! stale, and pushes the user's next real message further back. Wherever a
//! frame's exact cost cannot be computed, round **up**.
//!
//! # What actually reaches the socket
//!
//! Two writers share this connection's charged lane (`tx_outgoing`), and the
//! mirror has to see both:
//!
//! 1. **Us.** The raw `irc::client::Sender` is private to this module, so the
//!    only way out of repartee is [`IrcSender::send`], which charges on the way
//!    through.
//! 2. **The crate itself.** `client.sender()` hands us a *clone* of
//!    `tx_outgoing`; the crate keeps its own clone inside `ClientState` and
//!    emits frames from `ClientState::handle_message` on every inbound message
//!    — CTCP auto-replies, the autojoin JOIN batch at `ENDOFMOTD`, NICK retries
//!    on `ERR_NICKNAMEINUSE`. Those never pass through [`IrcSender`], but they
//!    DO charge the real counter. [`CrateEcho`] predicts them from the inbound
//!    message that triggers them and [`IrcSender::charge`] books them, so the
//!    mirror stays level with reality.
//!
//! The crate's internal pinger is **not** a third writer: `Connection::new` is
//! given `tx_priority_outgoing`, and `Outgoing::poll_priority_message` writes
//! that lane without touching `penalty`. Its PINGs and PONGs cost nothing, so
//! there is nothing to mirror.
//!
//! # The one place the mirror can still under-read
//!
//! At `ENDOFMOTD` the crate also re-joins any channel in its own `chanlists`
//! that is not in the config. `chanlists` is populated from inbound JOIN and
//! `RPL_NAMREPLY`, so it is empty until we are actually in a channel — which
//! means the rejoin batch is empty on the first `ENDOFMOTD` of a connection,
//! and a connection only ever sees one. A server that sent a second MOTD on a
//! live connection would leave us under-charged by that batch until it drained.
//! Bounded, and the crate builds a fresh `Client` per connect, so it does not
//! arise in practice.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use irc::proto::{Command, Message, Response};

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

    /// Book a frame against this connection's budget **without sending it**.
    ///
    /// For the frames the *crate* puts on our charged lane by itself (see the
    /// module docs): the socket write already happened inside the crate, so all
    /// that is left is to keep the mirror level with the real counter. Charging
    /// nothing here is exactly the bypass this module exists to close.
    pub(crate) fn charge(&self, message: &Message, now: Instant) {
        // Threshold 0 = the crate skips its whole penalty block, so there is no
        // counter to mirror.
        if self.threshold_ms == 0 {
            return;
        }
        let cost = message_cost(message);
        if cost > 0 {
            self.budget_mut().charge(now, cost);
        }
    }

    /// Is there room for one more `+typing` notification on **this** connection
    /// without eating into the budget the user's real messages need?
    ///
    /// The clock is injected: the caller (`App::send_typing`) charges the same
    /// `now` to the send that follows, and its tests drive both from a fixed
    /// origin rather than the wall clock.
    #[must_use]
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

/// The crate's default CTCP SOURCE reply (`Config::source`). We never set
/// `source`, so this is what it answers with.
const CRATE_DEFAULT_SOURCE: &str = "https://github.com/aatxe/irc";

/// The crate's default CTCP USERINFO reply (`Config::user_info`). We never set
/// `user_info`, so this is what it answers with.
const CRATE_DEFAULT_USER_INFO: &str = "";

/// Everything the crate needs in order to decide what to send on its own,
/// mirrored from the `Config` we hand `Client::from_config` in
/// [`crate::irc::connect_server`]. If that `Config` grows a field the crate auto-sends
/// from (`umodes`, `nick_password`, …), it has to be mirrored here too or the
/// budget under-reads.
#[derive(Debug, Clone)]
pub struct CrateEchoConfig {
    pub ctcp_version: String,
    pub username: String,
    pub realname: String,
    pub channels: Vec<String>,
    pub channel_keys: HashMap<String, String>,
    pub alt_nicks: Vec<String>,
}

/// Predicts the frames the crate emits by itself, from the inbound message that
/// triggers them.
///
/// A verbatim mirror of `ClientState::handle_message`
/// (`irc-repartee-1.5.1/src/client/mod.rs`) — every arm of that match which can
/// reach `self.send`. Feed it **every** inbound message, in order, and charge
/// what it returns; see the module docs for why.
///
/// Two of the crate's arms are omitted because they cannot fire for us:
/// `send_nick_password` and `send_umodes` both early-return on an empty config
/// field, and [`crate::irc::connect_server`] sets neither (`..Config::default()`).
#[derive(Debug)]
pub struct CrateEcho {
    config: CrateEchoConfig,
    /// Mirrors `ClientState::alt_nick_index` — the crate walks the alt-nick list
    /// once and then gives up, so retry N costs nothing after the list runs out.
    alt_nick_index: usize,
}

impl CrateEcho {
    pub(crate) const fn new(config: CrateEchoConfig) -> Self {
        Self {
            config,
            alt_nick_index: 0,
        }
    }

    /// The frames the crate will put on the charged lane in response to
    /// `inbound`. Empty for the overwhelming majority of messages.
    pub(crate) fn frames_for(&mut self, inbound: &Message) -> Vec<Message> {
        match &inbound.command {
            Command::PRIVMSG(target, body) => self
                .ctcp_reply(inbound, target, body)
                .into_iter()
                .collect(),
            Command::Response(Response::RPL_ENDOFMOTD | Response::ERR_NOMOTD, _) => {
                self.autojoin_frames()
            }
            Command::Response(Response::ERR_NICKNAMEINUSE | Response::ERR_ERRONEOUSNICKNAME, _) => {
                self.nick_retry().into_iter().collect()
            }
            _ => Vec::new(),
        }
    }

    /// The NOTICE the crate's `handle_ctcp` auto-answers an inbound CTCP query
    /// with — `None` if it will stay quiet.
    fn ctcp_reply(&self, inbound: &Message, target: &str, body: &str) -> Option<Message> {
        if !body.starts_with('\u{001}') {
            return None;
        }
        // Verbatim from the crate: strip the leading \x01 and a trailing one.
        let end = if body.ends_with('\u{001}') && body.len() > 1 {
            body.len() - 1
        } else {
            body.len()
        };
        let tokens: Vec<&str> = body.get(1..end)?.split(' ').collect();

        // The crate keys the reply target off a literal '#', not the full
        // channel-prefix set, and falls back to the sender's nick otherwise.
        // Mirror what it does, not what it arguably should do.
        let resp = if target.starts_with('#') {
            target
        } else {
            inbound.source_nickname()?
        };

        let reply = self.ctcp_answer(&tokens)?;
        Some(Command::NOTICE(resp.to_string(), format!("\u{001}{reply}\u{001}")).into())
    }

    /// The crate's `handle_ctcp` reply body, format string for format string.
    fn ctcp_answer(&self, tokens: &[&str]) -> Option<String> {
        let query = tokens.first()?;
        let cfg = &self.config;
        if query.eq_ignore_ascii_case("FINGER") {
            Some(format!("FINGER :{} ({})", cfg.realname, cfg.username))
        } else if query.eq_ignore_ascii_case("VERSION") {
            Some(format!("VERSION {}", cfg.ctcp_version))
        } else if query.eq_ignore_ascii_case("SOURCE") {
            Some(format!("SOURCE {CRATE_DEFAULT_SOURCE}"))
        } else if query.eq_ignore_ascii_case("PING") && tokens.len() > 1 {
            // The echoed token is peer-controlled and can be ~490 bytes, which
            // is worth several length steps — so it is measured, never assumed.
            Some(format!("PING {}", tokens[1]))
        } else if query.eq_ignore_ascii_case("TIME") {
            // The crate stamps `Local::now().to_rfc2822()`. Only the *length*
            // reaches the budget and rfc2822 is fixed-width to within a byte, so
            // our own clock reading costs what the crate's will.
            Some(format!("TIME :{}", chrono::Local::now().to_rfc2822()))
        } else if query.eq_ignore_ascii_case("USERINFO") {
            Some(format!("USERINFO :{CRATE_DEFAULT_USER_INFO}"))
        } else {
            None
        }
    }

    /// The batched autojoin the crate sends at `ENDOFMOTD`.
    fn autojoin_frames(&self) -> Vec<Message> {
        build_batched_joins(&self.config.channels, &self.config.channel_keys)
            .into_iter()
            .map(|(chanlist, keylist)| Command::JOIN(chanlist, keylist, None).into())
            .collect()
    }

    /// The NICK the crate retries with after `ERR_NICKNAMEINUSE`.
    fn nick_retry(&mut self) -> Option<Message> {
        let alt = self.config.alt_nicks.get(self.alt_nick_index)?;
        let message = Command::NICK(alt.clone()).into();
        self.alt_nick_index += 1;
        Some(message)
    }
}

/// The crate's autojoin batcher, mirrored from `Client::build_batched_joins`
/// (`irc-repartee-1.5.1/src/client/mod.rs`).
///
/// Mirrored exactly rather than bounded from above, because the bound is not
/// tight: ten channels go out as *one* ~7000ms JOIN, but charging them as ten
/// separate `JOIN`s would book 31 seconds and mute typing for the first minute
/// of every session. Rounding up is the rule where the exact cost is unknowable
/// — this one is knowable.
fn build_batched_joins(
    channels: &[String],
    channel_keys: &HashMap<String, String>,
) -> Vec<(String, Option<String>)> {
    // "JOIN " = 5 bytes, "\r\n" = 2 bytes → 505 bytes for payload.
    const BUDGET: usize = 512 - 7;

    /// Total payload size: chanlist [+ " " + keylist].
    const fn payload(chans: usize, keys: usize, has_keys: bool) -> usize {
        if has_keys { chans + 1 + keys } else { chans }
    }

    /// Close the batch under construction and start a fresh one.
    fn flush<'a>(
        chans: &mut Vec<&'a str>,
        keys: &mut Vec<&'a str>,
        chan_len: &mut usize,
        key_len: &mut usize,
        out: &mut Vec<(String, Option<String>)>,
    ) {
        if chans.is_empty() {
            return;
        }
        let chanlist = chans.join(",");
        let keylist = (!keys.is_empty()).then(|| keys.join(","));
        out.push((chanlist, keylist));
        chans.clear();
        keys.clear();
        *chan_len = 0;
        *key_len = 0;
    }

    if channels.is_empty() {
        return Vec::new();
    }

    // Partition into keyed and keyless, preserving config order within groups.
    let mut keyed: Vec<(&str, &str)> = Vec::new();
    let mut keyless: Vec<&str> = Vec::new();
    for chan in channels {
        match channel_keys.get(chan.as_str()) {
            Some(key) => keyed.push((chan, key)),
            None => keyless.push(chan),
        }
    }

    let mut batches: Vec<(String, Option<String>)> = Vec::new();
    let mut batch_chans: Vec<&str> = Vec::new();
    let mut batch_keys: Vec<&str> = Vec::new();
    let mut chan_len: usize = 0;
    let mut key_len: usize = 0;

    // Keyed channels first — they must precede keyless for positional keys.
    for (chan, key) in &keyed {
        let grown_chans = if batch_chans.is_empty() {
            chan.len()
        } else {
            chan_len + 1 + chan.len()
        };
        let grown_keys = if batch_keys.is_empty() {
            key.len()
        } else {
            key_len + 1 + key.len()
        };

        if !batch_chans.is_empty() && payload(grown_chans, grown_keys, true) > BUDGET {
            flush(
                &mut batch_chans,
                &mut batch_keys,
                &mut chan_len,
                &mut key_len,
                &mut batches,
            );
            chan_len = chan.len();
            key_len = key.len();
        } else {
            chan_len = grown_chans;
            key_len = grown_keys;
        }
        batch_chans.push(chan);
        batch_keys.push(key);
    }

    // Keyless channels fill the remaining space in the current batch.
    for chan in &keyless {
        let grown_chans = if batch_chans.is_empty() {
            chan.len()
        } else {
            chan_len + 1 + chan.len()
        };
        let has_keys = !batch_keys.is_empty();

        if !batch_chans.is_empty() && payload(grown_chans, key_len, has_keys) > BUDGET {
            flush(
                &mut batch_chans,
                &mut batch_keys,
                &mut chan_len,
                &mut key_len,
                &mut batches,
            );
            chan_len = chan.len();
        } else {
            chan_len = grown_chans;
        }
        batch_chans.push(chan);
    }

    flush(
        &mut batch_chans,
        &mut batch_keys,
        &mut chan_len,
        &mut key_len,
        &mut batches,
    );

    batches
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

    // ── The crate's own frames (see the module docs) ──

    fn echo_config() -> CrateEchoConfig {
        CrateEchoConfig {
            ctcp_version: "repartee 1.2.3".to_string(),
            username: "bob".to_string(),
            realname: "Bob Bobson".to_string(),
            channels: vec!["#rust".to_string()],
            channel_keys: HashMap::new(),
            alt_nicks: vec!["bob_".to_string(), "bob__".to_string()],
        }
    }

    /// An inbound CTCP query from `carol`, as the crate sees it.
    fn inbound_ctcp(target: &str, body: &str) -> Message {
        Message {
            tags: None,
            prefix: Some(irc::proto::Prefix::Nickname(
                "carol".to_string(),
                "~carol".to_string(),
                "example.org".to_string(),
            )),
            command: Command::PRIVMSG(target.to_string(), format!("\u{001}{body}\u{001}")),
        }
    }

    fn response(code: Response) -> Message {
        Command::Response(code, vec!["bob".to_string(), "done".to_string()]).into()
    }

    #[test]
    fn an_inbound_ctcp_version_charges_the_connection_budget() {
        // The bypass this closes: the crate answers VERSION with a NOTICE from
        // its own clone of `tx_outgoing`. That frame never passes through
        // `IrcSender`, but it DOES charge the real penalty counter — so a mirror
        // that ignored it would read 0 while the connection was already loaded.
        let now = Instant::now();
        let sender = IrcSender::capturing(u64::from(FLOOD_PENALTY_THRESHOLD_MS));
        let mut echo = CrateEcho::new(echo_config());
        assert_eq!(sender.penalty_ms(), 0);

        for frame in echo.frames_for(&inbound_ctcp("bob", "VERSION")) {
            sender.charge(&frame, now);
        }

        // NOTICE (2000) + the length step (1000): the reply is well under 100b.
        assert_eq!(sender.penalty_ms(), 3000);
        // And nothing was *sent* — the crate already wrote it to the socket.
        assert!(sender.captured().is_empty());
    }

    #[test]
    fn a_burst_of_inbound_ctcp_pings_closes_the_typing_headroom() {
        // The reported failure, verbatim: a peer sends 6 CTCP PINGs in a second,
        // the crate auto-replies to all 6, the real counter is ~18_000ms — over
        // the 10_000ms threshold, so `Outgoing` is already buffering. Before this
        // fix the mirror read 0 and we kept feeding it a TAGMSG every 3s.
        let now = Instant::now();
        let sender = IrcSender::capturing(u64::from(FLOOD_PENALTY_THRESHOLD_MS));
        let mut echo = CrateEcho::new(echo_config());
        assert!(sender.has_typing_headroom_at(now));

        for _ in 0..6 {
            for frame in echo.frames_for(&inbound_ctcp("bob", "PING 1234567890")) {
                sender.charge(&frame, now);
            }
        }

        assert_eq!(sender.penalty_ms(), 6 * 3000);
        assert!(
            !sender.has_typing_headroom_at(now),
            "typing must not be handed to a queue the crate is already throttling"
        );
    }

    #[test]
    fn the_ctcp_queries_the_crate_answers_are_exactly_these() {
        // Pinned against `handle_ctcp`. A query the crate ignores must charge
        // nothing (over-charging every inbound PRIVMSG would mute typing on any
        // busy channel), and one it answers must charge.
        let mut echo = CrateEcho::new(echo_config());
        for query in [
            "VERSION",
            "SOURCE",
            "PING token",
            "TIME",
            "FINGER",
            "USERINFO",
            // Case-insensitive, like the crate's `eq_ignore_ascii_case`.
            "version",
            "uSeRiNfO",
        ] {
            assert_eq!(
                echo.frames_for(&inbound_ctcp("bob", query)).len(),
                1,
                "the crate auto-answers {query}"
            );
        }
        for query in [
            "ACTION waves",
            "DCC CHAT chat 1 2",
            "CLIENTINFO",
            // PING with no token: the crate requires `tokens.len() > 1`.
            "PING",
            "",
        ] {
            assert!(
                echo.frames_for(&inbound_ctcp("bob", query)).is_empty(),
                "the crate stays quiet for {query:?}"
            );
        }
        // And an ordinary, non-CTCP PRIVMSG is free.
        assert!(
            echo.frames_for(&Command::PRIVMSG("#rust".into(), "hello".into()).into())
                .is_empty()
        );
    }

    #[test]
    fn a_ctcp_ping_echo_is_measured_not_assumed() {
        // The echoed token is peer-controlled. A ~400-byte one costs four extra
        // length steps, and assuming the 1000ms floor would under-read by 4000.
        let mut echo = CrateEcho::new(echo_config());
        let frames = echo.frames_for(&inbound_ctcp("bob", &format!("PING {}", "x".repeat(400))));
        assert_eq!(frames.len(), 1);
        // `NOTICE bob :\x01PING <400 x's>\x01\r\n` → 425 bytes → 5 length steps.
        assert_eq!(message_cost(&frames[0]), 2000 + 5000);
    }

    #[test]
    fn a_channel_ctcp_is_answered_to_the_channel_not_the_sender() {
        // The crate keys the reply target off a literal '#'. Mirroring it wrongly
        // would only change the target's length, but the whole point of this file
        // is that it mirrors what the crate does, not what we assume.
        let mut echo = CrateEcho::new(echo_config());
        let frames = echo.frames_for(&inbound_ctcp("#rust", "VERSION"));
        assert_eq!(frames.len(), 1);
        assert_eq!(
            frames[0].to_string(),
            "NOTICE #rust :\u{001}VERSION repartee 1.2.3\u{001}\r\n"
        );

        let frames = echo.frames_for(&inbound_ctcp("bob", "VERSION"));
        assert_eq!(
            frames[0].to_string(),
            "NOTICE carol :\u{001}VERSION repartee 1.2.3\u{001}\r\n"
        );
    }

    #[test]
    fn the_autojoin_batch_is_charged_at_endofmotd() {
        // repartee hands `channels` to the crate on purpose, so the JOIN batch is
        // the crate's frame, not ours — nothing charged it before.
        let now = Instant::now();
        let sender = IrcSender::capturing(u64::from(FLOOD_PENALTY_THRESHOLD_MS));
        let mut config = echo_config();
        config.channels = vec!["#rust".to_string(), "#tokio".to_string()];
        let mut echo = CrateEcho::new(config);

        let frames = echo.frames_for(&response(Response::RPL_ENDOFMOTD));
        assert_eq!(frames.len(), 1, "both channels ride one batched JOIN");
        assert_eq!(frames[0].to_string(), "JOIN #rust,#tokio\r\n");

        for frame in &frames {
            sender.charge(frame, now);
        }
        assert_eq!(sender.penalty_ms(), 2000 + 1000);

        // ERR_NOMOTD is the crate's other trigger for the same batch.
        assert_eq!(echo.frames_for(&response(Response::ERR_NOMOTD)).len(), 1);
        // Nothing else in the stream moves the budget.
        assert!(
            echo.frames_for(&response(Response::RPL_WELCOME))
                .is_empty()
        );
    }

    #[test]
    fn autojoin_keys_come_first_and_batches_split_at_the_crates_budget() {
        // Mirrors `build_batched_joins`: keyed channels precede keyless (their
        // keys are positional), and a batch flushes when the payload would pass
        // 505 bytes. Getting the split wrong changes the frame count and so the
        // charge — the whole reason this is mirrored rather than bounded.
        let mut keys = HashMap::new();
        keys.insert("#secret".to_string(), "hunter2".to_string());
        let batches = build_batched_joins(
            &[
                "#open".to_string(),
                "#secret".to_string(),
                "#other".to_string(),
            ],
            &keys,
        );
        assert_eq!(
            batches,
            vec![(
                "#secret,#open,#other".to_string(),
                Some("hunter2".to_string())
            )]
        );

        // 60 channels of 9 bytes each ("#chan0000") → 599 payload bytes, so the
        // crate splits: 50 fit in 505 (50*9 + 49 commas = 499), the 51st does not.
        let many: Vec<String> = (0..60).map(|i| format!("#chan{i:04}")).collect();
        let batches = build_batched_joins(&many, &HashMap::new());
        assert_eq!(batches.len(), 2);
        assert_eq!(batches[0].0.split(',').count(), 50);
        assert_eq!(batches[0].0.len(), 499);
        assert_eq!(batches[1].0.split(',').count(), 10);

        assert!(build_batched_joins(&[], &HashMap::new()).is_empty());
    }

    #[test]
    fn nick_retries_walk_the_alt_list_once_and_then_cost_nothing() {
        // The crate bumps `alt_nick_index` per retry and returns `NoUsableNick`
        // once the list runs out — it stops sending, so we must stop charging.
        let mut echo = CrateEcho::new(echo_config());
        let first = echo.frames_for(&response(Response::ERR_NICKNAMEINUSE));
        assert_eq!(first[0].to_string(), "NICK bob_\r\n");
        let second = echo.frames_for(&response(Response::ERR_ERRONEOUSNICKNAME));
        assert_eq!(second[0].to_string(), "NICK bob__\r\n");
        assert!(
            echo.frames_for(&response(Response::ERR_NICKNAMEINUSE))
                .is_empty(),
            "the alt list is exhausted — the crate sends nothing more"
        );
    }

    #[test]
    fn a_cloned_sender_shares_the_connections_budget() {
        // The load-bearing invariant of the wiring in `connect_server`: the reader
        // task books the crate's frames through a CLONE of the connection's
        // sender, and the handle keeps the original. A clone with a budget of its
        // own would charge them where nobody reads — the bypass would still be
        // wide open, and every test above would still pass.
        let now = Instant::now();
        let sender = IrcSender::capturing(u64::from(FLOOD_PENALTY_THRESHOLD_MS));
        let reader_task_copy = sender.clone();

        let mut echo = CrateEcho::new(echo_config());
        for frame in echo.frames_for(&inbound_ctcp("bob", "VERSION")) {
            reader_task_copy.charge(&frame, now);
        }

        // The handle is built from the ORIGINAL, and must see the charge.
        let handle = IrcHandle::new("net".to_string(), sender, None, None);
        assert_eq!(
            handle.sender().penalty_ms(),
            3000,
            "a clone IS the connection — it must charge the same counter"
        );
    }

    #[test]
    fn a_disabled_flood_protection_charges_nothing_for_the_crates_frames_either() {
        let now = Instant::now();
        let sender = IrcSender::capturing(0);
        let mut echo = CrateEcho::new(echo_config());
        for frame in echo.frames_for(&inbound_ctcp("bob", "VERSION")) {
            sender.charge(&frame, now);
        }
        assert_eq!(sender.penalty_ms(), 0);
        assert!(sender.has_typing_headroom_at(now));
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
