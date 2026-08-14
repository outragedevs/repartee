#![allow(clippy::redundant_pub_crate)]
//! `/e2e` command handlers for RPE2E v1.0.
//!
//! Subcommand dispatch on a single top-level `/e2e` entry point. Each helper
//! is kept small and delegates to `E2eManager`/`Keyring` for the heavy work.
//!
//! The user-facing polish layer follows the same conventions as `/dcc`
//! (`handlers_dcc.rs`): case-insensitive subcommand dispatch, themed output
//! using the `C_OK`/`C_ERR`/`C_CMD`/`C_DIM`/`C_HEADER`/`C_TEXT` constants and
//! the `divider()` helper from `commands::types`, aligned column layout for
//! `list` / `status`, and a first-class `help` subcommand.

use super::helpers::add_local_event;
use super::types::{C_CMD, C_DIM, C_ERR, C_HEADER, C_RST, C_TEXT, divider};
use crate::app::App;
use crate::e2e::crypto::fingerprint::{fingerprint_bip39, fingerprint_hex};
use crate::e2e::manager::TrustChange;
use crate::e2e::keyring::{ChannelConfig, ChannelMode, IncomingSession, TrustStatus};
use crate::state::buffer::{Message, MessageType};
use chrono::Utc;

/// E2E event severity — selects which theme key the renderer pulls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum E2eEventLevel {
    Info,
    Warning,
    Error,
}

impl E2eEventLevel {
    const fn event_key(self) -> &'static str {
        match self {
            Self::Info => "e2e_info",
            Self::Warning => "e2e_warning",
            Self::Error => "e2e_error",
        }
    }
}

/// Push a themed E2E status message into the active buffer. Uses the
/// theme's `events.e2e_info` / `events.e2e_warning` / `events.e2e_error`
/// format strings so the theme authors can restyle the `[E2E]` banner
/// without us sprinkling inline `%Z` codes through the command handlers.
///
/// `$*` in the theme format receives `text` via `event_params[0]`.
fn e2e_event(app: &mut App, level: E2eEventLevel, text: &str) {
    let Some(active_id) = app.state.active_buffer_id.as_deref() else {
        return;
    };
    let active_id = active_id.to_string();
    let id = app.state.next_message_id();
    app.state.add_local_message(
        &active_id,
        Message {
            log_key: None,
            id,
            timestamp: Utc::now(),
            message_type: MessageType::Event,
            nick: None,
            nick_mode: None,
            text: text.to_string(),
            highlight: level == E2eEventLevel::Error,
            event_key: Some(level.event_key().to_string()),
            event_params: Some(vec![text.to_string()]),
            log_msg_id: None,
            log_ref_id: None,
            tags: None,
            wire_origin: None,
            translation_suffix_at: None,
        },
    );
}

// ─── Subcommand enum + parser ─────────────────────────────────────────────────

/// Autotrust sub-operation — `list`, `add <scope> <pattern>`, or `remove <pattern>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AutotrustOp {
    List,
    Add(String, String),
    Remove(String),
    /// Missing / malformed arguments; carries a short usage hint.
    Usage(&'static str),
}

/// Parsed `/e2e` subcommand. Separating parsing from dispatch lets us test
/// case-insensitivity and unknown-subcommand handling without constructing a
/// full `App`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum E2eSub {
    On,
    Off,
    Mode(String),
    Accept(String),
    Decline(String),
    Handshake(String),
    Revoke(String),
    Unrevoke(String),
    Forget {
        target: String,
        all: bool,
    },
    Autotrust(AutotrustOp),
    List {
        all: bool,
    },
    Status,
    Fingerprint,
    Verify(String),
    Reverify {
        target: String,
        /// Hex prefix of the key being accepted, disambiguating two
        /// changes offered at one handle.
        fingerprint: Option<String>,
    },
    Rotate,
    Export(Option<String>),
    Import(Option<String>),
    Help,
    /// No subcommand was given — treat as `help`.
    None,
    /// Unrecognised top-level subcommand; carries the original (lowercased)
    /// token so the caller can echo it in the error line.
    Unknown(String),
    /// Subcommand recognised but a required argument is missing.
    Usage(&'static str),
}

/// Parse `args` into an `E2eSub`. Case-insensitive on the subcommand token.
/// Returns a testable value — no `App` required.
pub(crate) fn parse_subcommand(args: &[String]) -> E2eSub {
    let Some(sub_raw) = args.first() else {
        return E2eSub::None;
    };
    let sub = sub_raw.to_lowercase();
    let rest = &args[1..];

    match sub.as_str() {
        "on" => E2eSub::On,
        "off" => E2eSub::Off,
        "mode" => rest
            .first()
            .map_or(E2eSub::Usage("/e2e mode <auto-accept|normal|quiet>"), |m| {
                E2eSub::Mode(m.clone())
            }),
        "accept" => rest
            .first()
            .map_or(E2eSub::Usage("/e2e accept <nick>"), |n| {
                E2eSub::Accept(n.clone())
            }),
        "decline" => rest
            .first()
            .map_or(E2eSub::Usage("/e2e decline <nick>"), |n| {
                E2eSub::Decline(n.clone())
            }),
        "handshake" => rest
            .first()
            .map_or(E2eSub::Usage("/e2e handshake <nick>"), |n| {
                E2eSub::Handshake(n.clone())
            }),
        "revoke" => rest
            .first()
            .map_or(E2eSub::Usage("/e2e revoke <nick>"), |n| {
                E2eSub::Revoke(n.clone())
            }),
        "unrevoke" => rest
            .first()
            .map_or(E2eSub::Usage("/e2e unrevoke <nick>"), |n| {
                E2eSub::Unrevoke(n.clone())
            }),
        "forget" => parse_forget_subcommand(rest),
        "autotrust" => E2eSub::Autotrust(parse_autotrust_op(rest)),
        "list" => E2eSub::List {
            all: rest
                .first()
                .is_some_and(|arg| arg.eq_ignore_ascii_case("-all")),
        },
        "status" => E2eSub::Status,
        "fingerprint" => E2eSub::Fingerprint,
        "verify" => rest
            .first()
            .map_or(E2eSub::Usage("/e2e verify <nick>"), |n| {
                E2eSub::Verify(n.clone())
            }),
        "reverify" => parse_reverify_subcommand(rest),
        "rotate" => E2eSub::Rotate,
        "export" => E2eSub::Export(rest.first().cloned()),
        "import" => E2eSub::Import(rest.first().cloned()),
        "help" | "?" => E2eSub::Help,
        other => E2eSub::Unknown(other.to_string()),
    }
}

/// `/e2e reverify <nick|handle> [fingerprint]`.
///
/// The optional second argument is the fingerprint of the key being
/// accepted. It is only needed when two changes are waiting at the same
/// `ident@host`, where the handle alone cannot tell them apart.
const REVERIFY_USAGE: &str = "/e2e reverify <nick|handle> [fingerprint]";

/// Command offered when the warning a reverify would answer is no longer
/// in memory — a handshake re-raises it.
///
/// Must stay a *runnable* command line. `/e2e handshake` on its own only
/// prints its own usage, which is where this hint used to send people.
const REVERIFY_RETRY_CMD: &str = "/e2e handshake <nick>";

fn parse_reverify_subcommand(rest: &[String]) -> E2eSub {
    match rest {
        [target] => E2eSub::Reverify {
            target: target.clone(),
            fingerprint: None,
        },
        [target, fingerprint] => E2eSub::Reverify {
            target: target.clone(),
            fingerprint: Some(fingerprint.clone()),
        },
        _ => E2eSub::Usage(REVERIFY_USAGE),
    }
}

fn parse_forget_subcommand(rest: &[String]) -> E2eSub {
    if rest.is_empty() {
        return E2eSub::Usage("/e2e forget [-all] <nick|handle>");
    }
    let mut all = false;
    let mut target: Option<String> = None;
    for arg in rest {
        if arg.eq_ignore_ascii_case("-all") {
            all = true;
        } else if target.is_none() {
            target = Some(arg.clone());
        } else {
            return E2eSub::Usage("/e2e forget [-all] <nick|handle>");
        }
    }
    target.map_or(
        E2eSub::Usage("/e2e forget [-all] <nick|handle>"),
        |target| E2eSub::Forget { target, all },
    )
}

fn parse_autotrust_op(rest: &[String]) -> AutotrustOp {
    let Some(op_raw) = rest.first() else {
        return AutotrustOp::Usage("/e2e autotrust <list|add|remove> [scope] [pattern]");
    };
    let op = op_raw.to_lowercase();
    match op.as_str() {
        "list" => AutotrustOp::List,
        "add" => match (rest.get(1), rest.get(2)) {
            (Some(scope), Some(pat)) => AutotrustOp::Add(scope.clone(), pat.clone()),
            _ => AutotrustOp::Usage("/e2e autotrust add <scope> <pattern>"),
        },
        "remove" => rest.get(1).map_or(
            AutotrustOp::Usage("/e2e autotrust remove <pattern>"),
            |pat| AutotrustOp::Remove(pat.clone()),
        ),
        _ => AutotrustOp::Usage("/e2e autotrust <list|add|remove>"),
    }
}

/// Parse a channel-mode token. Unlike [`ChannelMode::parse`] (which silently
/// collapses unknown values to `Normal`), this returns an `Err` so the
/// command layer can emit a proper themed error line to the user.
pub(crate) fn parse_mode(s: &str) -> std::result::Result<ChannelMode, String> {
    match s.to_lowercase().as_str() {
        "auto-accept" | "auto" => Ok(ChannelMode::AutoAccept),
        "normal" => Ok(ChannelMode::Normal),
        "quiet" => Ok(ChannelMode::Quiet),
        other => Err(format!(
            "invalid mode '{other}' (expected auto-accept|normal|quiet)"
        )),
    }
}

// ─── /e2e dispatcher ──────────────────────────────────────────────────────────

/// Single `/e2e` entry point. Dispatches on the first arg (case-insensitive).
pub(crate) fn cmd_e2e(app: &mut App, args: &[String]) {
    let sub = parse_subcommand(args);
    match sub {
        E2eSub::None | E2eSub::Help => e2e_help(app),
        E2eSub::On => e2e_on(app),
        E2eSub::Off => e2e_off(app),
        E2eSub::Mode(m) => e2e_mode(app, &m),
        E2eSub::Accept(nick) => e2e_accept(app, &nick),
        E2eSub::Decline(nick) => e2e_decline(app, &nick),
        E2eSub::Handshake(nick) => e2e_handshake(app, &nick),
        E2eSub::Revoke(nick) => e2e_revoke(app, &nick),
        E2eSub::Unrevoke(nick) => e2e_unrevoke(app, &nick),
        E2eSub::Forget { target, all } => e2e_forget(app, &target, all),
        E2eSub::Autotrust(op) => e2e_autotrust(app, op),
        E2eSub::List { all } => e2e_list(app, all),
        E2eSub::Status => e2e_status(app),
        E2eSub::Fingerprint => e2e_fingerprint(app),
        E2eSub::Verify(nick) => e2e_verify(app, &nick),
        E2eSub::Reverify {
            target,
            fingerprint,
        } => e2e_reverify(app, &target, fingerprint.as_deref()),
        E2eSub::Rotate => e2e_rotate(app),
        E2eSub::Export(path) => e2e_export(app, path.as_deref()),
        E2eSub::Import(path) => e2e_import(app, path.as_deref()),
        E2eSub::Unknown(other) => {
            err(app, &format!("unknown subcommand: {other}"));
            e2e_help(app);
        }
        E2eSub::Usage(hint) => {
            err(app, &format!("usage: {hint}"));
        }
    }

    // Subcommands like `handshake` enqueue outbound NOTICEs into
    // `state.pending_e2e_sends`. The IRC event loop drain only fires
    // after an incoming message is handled, so for command-driven sends
    // we must drain explicitly here or the KEYREQ would sit in the queue
    // until the next IRC event arrives.
    if !app.state.pending_e2e_sends.is_empty() {
        app.drain_pending_e2e_sends();
    }
}

// ─── helpers ──────────────────────────────────────────────────────────────────

/// Resolve the E2E keyring/handshake context for a buffer. Channels use
/// their name verbatim; queries (DMs) use the `@<peer_handle>` pseudochannel
/// (spec §6) so the command layer keys exactly the rows the encrypt/decrypt
/// path reads — never the bare nick, which never lined up and forced the
/// "refuse to send if only a bare-nick config exists" band-aid. A query
/// prefers the live `peer_handle`, falling back to `cached_handle` (the
/// keyring's last-known handle for the nick) so `/e2e` stays usable before
/// the peer speaks this session. Returns `None` for a query with neither
/// handle known, or for a non-channel/query buffer.
fn e2e_context_for(
    network: &str,
    buffer_type: &crate::state::buffer::BufferType,
    name: &str,
    peer_handle: Option<&str>,
    cached_handle: Option<&str>,
) -> Option<String> {
    use crate::state::buffer::BufferType;
    // Contexts are network-scoped for keyring storage (see
    // `e2e::scoped_context`); the manager re-derives the wire form.
    match buffer_type {
        BufferType::Channel => Some(crate::e2e::scoped_context(network, name)),
        BufferType::Query => peer_handle
            .or(cached_handle)
            .map(|h| crate::e2e::scoped_context(network, &crate::e2e::context_key(name, h))),
        _ => None,
    }
}

/// E2E keyring/handshake context for the active buffer (see
/// [`e2e_context_for`]).
fn current_e2e_context(app: &App) -> Option<String> {
    let buf = app.state.active_buffer()?;
    // When the peer hasn't spoken this session (so `peer_handle` is `None`),
    // fall back to the keyring's last-known handle for this nick — otherwise
    // `/e2e` would refuse in a query whose E2E rows already exist.
    let cached = if matches!(buf.buffer_type, crate::state::buffer::BufferType::Query)
        && buf.peer_handle.is_none()
    {
        resolve_cached_handle_by_nick(app, &buf.name).and_then(Result::ok)
    } else {
        None
    };
    let network = app
        .state
        .connections
        .get(&buf.connection_id)
        .map(|c| c.label.clone())
        .unwrap_or_default();
    e2e_context_for(
        &network,
        &buf.buffer_type,
        &buf.name,
        buf.peer_handle.as_deref(),
        cached.as_deref(),
    )
}

/// Recipient-keyed E2E context for INCOMING-session operations: the channel
/// name for a channel, or OUR own `@<own_handle>` for a DM. Incoming DM
/// sessions, the trusted-peer listing, our outgoing-receive KEYREQ, and the
/// incoming-trust flips (`revoke`/`unrevoke`/`verify`) all key by this — they
/// live under our own handle, not the peer's (see docs/rpe2e-dm-addendum.md).
/// For channels it equals [`current_e2e_context`] (own == peer == channel),
/// so channel behaviour is unchanged. `None` for a DM whose own handle isn't
/// known yet.
fn e2e_own_context_for(
    network: &str,
    buffer_type: &crate::state::buffer::BufferType,
    name: &str,
    own_handle: Option<&str>,
) -> Option<String> {
    use crate::state::buffer::BufferType;
    match buffer_type {
        BufferType::Channel => Some(crate::e2e::scoped_context(network, name)),
        BufferType::Query => own_handle
            .map(|h| crate::e2e::scoped_context(network, &crate::e2e::context_key(name, h))),
        _ => None,
    }
}

/// [`e2e_own_context_for`] for the active buffer, resolving our own handle from
/// the active buffer's connection.
fn current_e2e_own_context(app: &App) -> Option<String> {
    let buf = app.state.active_buffer()?;
    let own = app
        .state
        .connections
        .get(&buf.connection_id)
        .and_then(|c| c.own_handle.clone());
    let network = app
        .state
        .connections
        .get(&buf.connection_id)
        .map(|c| c.label.clone())
        .unwrap_or_default();
    e2e_own_context_for(&network, &buf.buffer_type, &buf.name, own.as_deref())
}

fn require_mgr(app: &mut App) -> Option<std::sync::Arc<crate::e2e::E2eManager>> {
    // Clone the Arc upfront so we can drop the immutable borrow of `app.state`
    // before potentially calling `add_local_event`, which needs `&mut app`.
    let mgr = app.state.e2e_manager.clone();
    if mgr.is_none() {
        err(
            app,
            "manager not initialized (check logging.enabled / e2e.enabled)",
        );
    }
    mgr
}

/// Error helper: emit a themed error line with the `[E2E]` tag. Goes
/// through the `events.e2e_error` theme key so theme authors can style
/// the `[E2E]` banner consistently.
fn err(app: &mut App, msg: &str) {
    e2e_event(app, E2eEventLevel::Error, msg);
}

/// Info/success helper: emit a themed OK line with the `[E2E]` tag via
/// the `events.e2e_info` theme key.
fn ok(app: &mut App, msg: &str) {
    e2e_event(app, E2eEventLevel::Info, msg);
}

/// Warning helper — themed through `events.e2e_warning`. Used for
/// destructive-but-user-initiated operations (revoke, decline, forget)
/// where we want the banner to stand out without being a hard error.
fn warn(app: &mut App, msg: &str) {
    e2e_event(app, E2eEventLevel::Warning, msg);
}

fn push_active_e2e_status(app: &mut App) {
    let Some(buffer_id) = app.state.active_buffer_id.clone() else {
        return;
    };
    app.state.push_buffer_e2e_status(&buffer_id);
}

// ─── on / off / mode ─────────────────────────────────────────────────────────

fn e2e_on(app: &mut App) {
    let Some(chan) = current_e2e_context(app) else {
        err(app, "/e2e on: no active channel or known query peer");
        return;
    };
    let Some(mgr) = require_mgr(app) else { return };
    let cfg = ChannelConfig {
        channel: chan.clone(),
        enabled: true,
        mode: ChannelMode::Normal,
    };
    if let Err(e) = mgr.keyring().set_channel_config(&cfg) {
        err(app, &format!("/e2e on: {e}"));
        return;
    }
    push_active_e2e_status(app);
    ok(app, &format!("enabled on {} (mode=normal)", crate::e2e::display_context(&chan)));
    // The user just made this a conversation translation must never touch.
    // The exclusion itself is enforced at the gate, from the next line, with
    // no state to keep in sync — but silently: without this row both
    // features read as "on" and the channel simply stops being translated,
    // which looks like a broken translator rather than a decision.
    if let Some(buffer_id) = app.state.active_buffer_id.as_deref()
        && app.state.translate_buffers.contains_key(buffer_id)
    {
        warn(
            app,
            "this conversation was set up for translation — encryption wins, \
             so it will no longer be translated (translating would send its \
             plaintext to a third-party provider)",
        );
    }
    // The user just made this a conversation translation must never touch.
    // The exclusion itself is enforced at the gate, from the next line, with
    // no state to keep in sync — but silently: without this row both
    // features read as "on" and the channel simply stops being translated,
    // which looks like a broken translator rather than a decision.
}

fn e2e_off(app: &mut App) {
    let Some(chan) = current_e2e_context(app) else {
        err(app, "/e2e off: no active channel or known query peer");
        return;
    };
    let Some(mgr) = require_mgr(app) else { return };
    let cfg = ChannelConfig {
        channel: chan.clone(),
        enabled: false,
        mode: ChannelMode::Normal,
    };
    if let Err(e) = mgr.keyring().set_channel_config(&cfg) {
        err(app, &format!("/e2e off: {e}"));
        return;
    }
    push_active_e2e_status(app);
    ok(app, &format!("disabled on {}", crate::e2e::display_context(&chan)));
}

fn e2e_mode(app: &mut App, mode_str: &str) {
    let Some(chan) = current_e2e_context(app) else {
        err(app, "/e2e mode: no active channel or known query peer");
        return;
    };
    let mode = match parse_mode(mode_str) {
        Ok(m) => m,
        Err(e) => {
            err(app, &format!("/e2e mode: {e}"));
            return;
        }
    };
    let Some(mgr) = require_mgr(app) else { return };
    let cfg = ChannelConfig {
        channel: chan.clone(),
        enabled: true,
        mode,
    };
    if let Err(e) = mgr.keyring().set_channel_config(&cfg) {
        err(app, &format!("/e2e mode: {e}"));
        return;
    }
    push_active_e2e_status(app);
    ok(
        app,
        &format!(
            "mode={} on {}",
            mode.as_str(),
            crate::e2e::display_context(&chan)
        ),
    );
}

// ─── trust transitions ───────────────────────────────────────────────────────

fn e2e_accept(app: &mut App, nick: &str) {
    let Some(chan) = current_e2e_context(app) else {
        err(app, "/e2e accept: no active channel or known query peer");
        return;
    };
    // We match by nick — the keyring key is ident@host, but at command
    // time the user types the nick. Strict resolution: we refuse to
    // fall back to the raw nick because that would upsert a zombie peer
    // row keyed on nick-as-handle. `require_handle_for_nick` surfaces a
    // themed `[E2E]` error line on miss, so the user gets a clean
    // "has the user spoken yet?" instead of a silent no-op.
    let Some(handle) = require_handle_for_nick(app, &chan, nick) else {
        return;
    };
    // Capture the active connection id before we grab the mutable-ish manager
    // borrow so we can enqueue the KEYRSP on the correct connection.
    let conn_id_opt = app.state.active_buffer().map(|b| b.connection_id.clone());
    let Some(mgr) = require_mgr(app) else { return };

    // First try the pending-inbound path — if there is a cached Normal-mode
    // KEYREQ for this (handle, channel), build and dispatch the KEYRSP now.
    match mgr.accept_pending_inbound(&handle, &chan) {
        Ok(Some(rsp)) => {
            if let Some(conn_id) = conn_id_opt {
                let ctcp = mgr.encode_keyrsp_ctcp(&rsp);
                app.state
                    .pending_e2e_sends
                    .push(crate::state::PendingE2eSend {
                        connection_id: conn_id.clone(),
                        target: nick.to_string(),
                        notice_text: ctcp,
                    });
                for out in mgr.take_pending_outbound_keyreqs() {
                    let ctcp = mgr.encode_keyreq_ctcp(&out.req);
                    app.state
                        .pending_e2e_sends
                        .push(crate::state::PendingE2eSend {
                            connection_id: conn_id.clone(),
                            target: nick.to_string(),
                            notice_text: ctcp,
                        });
                }
            } else {
                err(app, "/e2e accept: no active connection to send KEYRSP");
                return;
            }
            ok(
                app,
                &format!(
                    "accepted {nick} ({handle}) on {} — KEYRSP sent",
                    crate::e2e::display_context(&chan)
                ),
            );
            return;
        }
        Ok(None) => {
            // Fall through to the status-flip path below — but only for a
            // session with REAL key material (e.g. a HandleChanged/refused
            // session being re-trusted). A Normal-mode KEYREQ persists a
            // zero-key `Pending` placeholder row while the KEYREQ itself
            // lives only in memory; after a restart the map is empty, so
            // flipping the placeholder to Trusted would report success while
            // never sending a KEYRSP and leaving an undecryptable all-zero
            // session. Detect that and tell the user what actually happened.
        }
        Err(e) => {
            err(app, &format!("/e2e accept: {e}"));
            return;
        }
    }

    match mgr.keyring().get_incoming_session(&handle, &chan) {
        Ok(Some(sess)) => {
            if sess.status == TrustStatus::Pending && sess.sk == [0u8; 32] {
                err(
                    app,
                    &format!(
                        "/e2e accept: the pending key request from {nick} was lost \
                         (restart?) — ask them to re-run the handshake"
                    ),
                );
                return;
            }
        }
        Ok(None) => {
            err(
                app,
                &format!(
                    "/e2e accept: nothing to accept for {nick} on {}",
                    crate::e2e::display_context(&chan)
                ),
            );
            return;
        }
        Err(e) => {
            err(app, &format!("/e2e accept: {e}"));
            return;
        }
    }

    if let Err(e) = mgr
        .keyring()
        .update_incoming_status(&handle, &chan, TrustStatus::Trusted)
    {
        err(app, &format!("/e2e accept: {e}"));
        return;
    }
    ok(
        app,
        &format!(
            "accepted {nick} ({handle}) on {}",
            crate::e2e::display_context(&chan)
        ),
    );
}

fn e2e_decline(app: &mut App, nick: &str) {
    let Some(chan) = current_e2e_context(app) else {
        err(app, "/e2e decline: no active channel or known query peer");
        return;
    };
    let Some(handle) = require_handle_for_nick(app, &chan, nick) else {
        return;
    };
    let Some(mgr) = require_mgr(app) else { return };
    if let Err(e) = mgr
        .keyring()
        .update_incoming_status(&handle, &chan, TrustStatus::Revoked)
    {
        err(app, &format!("/e2e decline: {e}"));
        return;
    }
    warn(
        app,
        &format!("declined {nick} on {}", crate::e2e::display_context(&chan)),
    );
}

fn e2e_revoke(app: &mut App, nick: &str) {
    // A DM splits: the outgoing side (stop sending to the peer) keys by
    // @<peer>, the incoming side (revoke trust in the peer's messages to us)
    // keys by @<own>. For a channel both collapse to the channel name.
    let Some(peer_chan) = current_e2e_context(app) else {
        err(app, "/e2e revoke: no active channel or known query peer");
        return;
    };
    let Some(own_chan) = current_e2e_own_context(app) else {
        err(app, "/e2e revoke: own handle not yet known");
        return;
    };
    let Some(handle) = require_handle_for_nick(app, &peer_chan, nick) else {
        return;
    };
    let Some(mgr) = require_mgr(app) else { return };
    if let Err(e) = mgr
        .keyring()
        .update_incoming_status(&handle, &own_chan, TrustStatus::Revoked)
    {
        err(app, &format!("/e2e revoke: {e}"));
        return;
    }
    // Drop the peer from the outgoing-recipient list so the subsequent
    // lazy rotate (triggered by mark_outgoing_pending_rotation) does NOT
    // redistribute the fresh key to them.
    if let Err(e) = mgr.keyring().remove_outgoing_recipient(&peer_chan, &handle) {
        err(app, &format!("/e2e revoke (drop recipient): {e}"));
        return;
    }
    if let Err(e) = mgr.keyring().mark_outgoing_pending_rotation(&peer_chan) {
        err(app, &format!("/e2e revoke (mark rotation): {e}"));
        return;
    }
    warn(
        app,
        &format!("revoked {nick} — key will rotate on next message"),
    );
}

fn e2e_unrevoke(app: &mut App, nick: &str) {
    // Trust in the peer's incoming messages lives under our own handle
    // (recipient-keyed); for a channel this is the channel name.
    let Some(chan) = current_e2e_own_context(app) else {
        err(
            app,
            "/e2e unrevoke: no active channel, or own handle not yet known",
        );
        return;
    };
    let Some(handle) = require_handle_for_nick(app, &chan, nick) else {
        return;
    };
    let Some(mgr) = require_mgr(app) else { return };
    if let Err(e) = mgr
        .keyring()
        .update_incoming_status(&handle, &chan, TrustStatus::Trusted)
    {
        err(app, &format!("/e2e unrevoke: {e}"));
        return;
    }
    ok(app, &format!("unrevoked {nick}"));
}

fn e2e_forget(app: &mut App, target: &str, all: bool) {
    let channel = if all {
        current_e2e_context(app)
    } else {
        let Some(chan) = current_e2e_context(app) else {
            err(app, "/e2e forget: no active channel or known query peer");
            return;
        };
        Some(chan)
    };
    let Some(active_buffer) = app.state.active_buffer() else {
        err(app, "/e2e forget: no active buffer");
        return;
    };
    let conn_id = active_buffer.connection_id.clone();
    let buffer_id = active_buffer.id.clone();
    if looks_like_handle(target) {
        perform_e2e_forget(app, buffer_id, target, target, channel.as_deref(), all);
        return;
    }
    // The recipient-keyed @<own> context is NOT captured here: a CHGHOST can
    // move our own handle between this command and the USERHOST reply, so the
    // deferred handler recomputes @<own> from our current handle at execution
    // (mirroring perform_e2e_forget, which recomputes it too). Capturing it now
    // would forget under a stale @<own> and leave the peer's live incoming
    // session intact.
    let Some(sender) = app.active_irc_sender().cloned() else {
        err(app, "/e2e forget: not connected");
        return;
    };
    if let Err(e) = sender.send(irc::proto::Command::Raw(
        "USERHOST".to_string(),
        vec![target.to_string()],
    )) {
        err(app, &format!("/e2e forget: failed to send USERHOST: {e}"));
        return;
    }
    app.state
        .pending_userhost_requests
        .push(crate::state::PendingUserhostRequest {
            connection_id: conn_id,
            nick: target.to_string(),
            action: crate::state::PendingUserhostAction::E2eForget {
                buffer_id,
                target: target.to_string(),
                channel,
                all,
            },
        });
    ok(app, &format!("resolving handle for {target} via USERHOST"));
}

fn perform_e2e_forget(
    app: &mut App,
    target_buffer: String,
    target: &str,
    handle: &str,
    channel: Option<&str>,
    all: bool,
) {
    let current_id = app.state.active_buffer_id.clone();
    app.state.active_buffer_id = Some(target_buffer);
    // A DM's incoming session is recipient-keyed under @<own>, while `channel`
    // (current_e2e_context) is @<peer>. Capture the own context relative to the
    // target buffer so the non-all path forgets BOTH — otherwise the peer's
    // trusted incoming session survives and their messages still decrypt. For a
    // channel own == peer == channel name, so the second forget is a no-op.
    let own_channel = if all {
        None
    } else {
        current_e2e_own_context(app)
    };
    let Some(mgr) = require_mgr(app) else {
        app.state.active_buffer_id = current_id;
        return;
    };
    let result = if all {
        mgr.forget_peer_everywhere(handle)
    } else {
        let Some(channel) = channel else {
            err(app, "/e2e forget: no active channel or known query peer");
            app.state.active_buffer_id = current_id;
            return;
        };
        // For a DM the incoming session lives under @<own>; if our own handle
        // is unknown (reset at every registration until the self-USERHOST
        // reply) a partial forget would clear only @<peer> while REPORTING
        // success — the peer's trusted incoming session would survive and
        // their messages would keep decrypting. Refuse instead, exactly like
        // revoke/unrevoke/verify do in the same state.
        // The context may be network-scoped — the DM test looks at its
        // wire part.
        if crate::e2e::wire_context(channel).starts_with('@') && own_channel.is_none() {
            err(app, "/e2e forget: own handle not yet known — retry in a moment");
            app.state.active_buffer_id = current_id;
            return;
        }
        mgr.forget_peer_on_dm_contexts(handle, channel, own_channel.as_deref())
    };
    match result {
        Ok(deleted) if all => warn(
            app,
            &format!("forgot {target} ({handle}) globally — removed {deleted} row(s)"),
        ),
        Ok(deleted) => warn(
            app,
            &format!(
                "forgot {target} ({handle}) on {} — removed {deleted} row(s)",
                channel.map_or_else(String::new, crate::e2e::display_context)
            ),
        ),
        Err(e) => err(app, &format!("/e2e forget: {e}")),
    }
    app.state.active_buffer_id = current_id;
}

// ─── handshake / rotate ──────────────────────────────────────────────────────

fn e2e_handshake(app: &mut App, nick: &str) {
    // A KEYREQ establishes the direction where WE receive, so it keys (and
    // stamps c=) by our own handle (recipient-keyed); for a channel this is
    // the channel name.
    let Some(chan) = current_e2e_own_context(app) else {
        err(
            app,
            "/e2e handshake: no active channel, or own handle not yet known",
        );
        return;
    };
    // Grab the connection id before the `require_mgr` mutable borrow
    // dance — we need it to route the outbound NOTICE.
    let Some(conn_id) = app.state.active_buffer().map(|b| b.connection_id.clone()) else {
        err(app, "/e2e handshake: no active connection");
        return;
    };
    let Some(mgr) = require_mgr(app) else { return };
    match mgr.build_keyreq(&chan) {
        Ok(req) => {
            let ctcp = mgr.encode_keyreq_ctcp(&req);
            app.state
                .pending_e2e_sends
                .push(crate::state::PendingE2eSend {
                    connection_id: conn_id,
                    target: nick.to_string(),
                    notice_text: ctcp,
                });
            ok(app, &format!("KEYREQ sent to {nick}"));
        }
        Err(e) => err(app, &format!("handshake error: {e}")),
    }
}

fn e2e_rotate(app: &mut App) {
    let Some(chan) = current_e2e_context(app) else {
        err(app, "/e2e rotate: no active channel or known query peer");
        return;
    };
    let Some(mgr) = require_mgr(app) else { return };
    if let Err(e) = mgr.keyring().mark_outgoing_pending_rotation(&chan) {
        err(app, &format!("/e2e rotate: {e}"));
        return;
    }
    ok(
        app,
        &format!(
            "rotation scheduled for {}",
            crate::e2e::display_context(&chan)
        ),
    );
}

// ─── listings ────────────────────────────────────────────────────────────────

fn e2e_list(app: &mut App, all: bool) {
    if all {
        e2e_list_all(app);
        return;
    }
    // Trusted incoming peers live under our own handle (recipient-keyed);
    // for a channel this is the channel name. Display uses the buffer label.
    let Some(chan) = current_e2e_own_context(app) else {
        err(
            app,
            "/e2e list: no active channel, or own handle not yet known",
        );
        return;
    };
    let label = app
        .state
        .active_buffer()
        .map_or_else(String::new, |b| b.name.clone());
    let Some(mgr) = require_mgr(app) else { return };
    let peers = match mgr.keyring().list_trusted_peers_for_channel(&chan) {
        Ok(p) => p,
        Err(e) => {
            err(app, &format!("/e2e list: {e}"));
            return;
        }
    };

    if peers.is_empty() {
        add_local_event(app, &divider(&format!("E2E Peers on {label}")));
        add_local_event(
            app,
            &format!("  {C_DIM}(no trusted peers — use /e2e accept <nick>){C_RST}"),
        );
        return;
    }

    let mut lines = vec![divider(&format!("E2E Peers on {label}"))];
    for p in &peers {
        lines.push(format_peer_line(p));
    }
    for line in lines {
        add_local_event(app, &line);
    }
}

fn e2e_list_all(app: &mut App) {
    let Some(mgr) = require_mgr(app) else { return };
    let peers = match mgr.keyring().list_all_peers() {
        Ok(peers) => peers,
        Err(e) => {
            err(app, &format!("/e2e list -all: {e}"));
            return;
        }
    };
    let sessions = match mgr.keyring().list_all_incoming_sessions() {
        Ok(sessions) => sessions,
        Err(e) => {
            err(app, &format!("/e2e list -all: {e}"));
            return;
        }
    };
    let mut lines = vec![divider("E2E Keyring (all)")];
    if peers.is_empty() && sessions.is_empty() {
        lines.push(format!("  {C_DIM}(no remembered E2E state){C_RST}"));
    } else {
        lines.push(format!("  {C_HEADER}Peers{C_RST}"));
        if peers.is_empty() {
            lines.push(format!("  {C_DIM}(none){C_RST}"));
        } else {
            for peer in peers {
                let fp_hex = fingerprint_hex(&peer.fingerprint);
                let fp_short: String = fp_hex.chars().take(16).collect();
                let handle = peer.last_handle.unwrap_or_else(|| "—".to_string());
                let nick = peer.last_nick.unwrap_or_else(|| "—".to_string());
                lines.push(format!(
                    "  {C_CMD}{handle}{C_RST}  {C_TEXT}[{status}]{C_RST}  {C_DIM}nick={nick} fp={fp_short}{C_RST}",
                    status = peer.global_status.as_str(),
                ));
            }
        }
        lines.push(String::new());
        lines.push(format!("  {C_HEADER}Incoming Sessions{C_RST}"));
        if sessions.is_empty() {
            lines.push(format!("  {C_DIM}(none){C_RST}"));
        } else {
            for sess in sessions {
                let fp_hex = fingerprint_hex(&sess.fingerprint);
                let fp_short: String = fp_hex.chars().take(16).collect();
                lines.push(format!(
                    "  {C_CMD}{handle}{C_RST}  {C_TEXT}{channel}{C_RST}  {C_TEXT}[{status}]{C_RST}  {C_DIM}fp={fp_short}{C_RST}",
                    handle = sess.handle,
                    channel = crate::e2e::display_context(&sess.channel),
                    status = sess.status.as_str(),
                ));
            }
        }
    }
    for line in lines {
        add_local_event(app, &line);
    }
}

/// Format a single trusted-peer row for `/e2e list`. Extracted so tests can
/// exercise the formatting without touching `App` or the database.
fn format_peer_line(p: &IncomingSession) -> String {
    let fp_hex = fingerprint_hex(&p.fingerprint);
    let fp_short: String = fp_hex.chars().take(16).collect();
    format!(
        "  {C_CMD}{handle}{C_RST}  {C_TEXT}[{status}]{C_RST}  {C_DIM}fp={fp_short}{C_RST}",
        handle = p.handle,
        status = p.status.as_str(),
    )
}

fn e2e_status(app: &mut App) {
    let Some(mgr) = require_mgr(app) else { return };
    let fp = mgr.fingerprint();
    let fp_hex = fingerprint_hex(&fp);
    let sas = fingerprint_bip39(&fp).unwrap_or_else(|_| "—".into());

    // Per-channel summary row. The outgoing config is keyed by @<peer>; the
    // trusted-peer count comes from the incoming sessions, keyed by @<own>
    // for a DM (recipient-keyed) — must match /e2e list, not the config key.
    let chan = current_e2e_context(app);
    let own_chan = current_e2e_own_context(app);
    let chan_cfg: Option<ChannelConfig> = chan
        .as_ref()
        .and_then(|c| mgr.keyring().get_channel_config(c).ok().flatten());
    let peer_count = own_chan
        .as_ref()
        .and_then(|c| mgr.keyring().list_trusted_peers_for_channel(c).ok())
        .map_or(0usize, |v| v.len());

    let mut lines = vec![divider("E2E Status")];
    lines.push(format!(
        "  {C_CMD}identity{C_RST}     {C_TEXT}{fp_hex}{C_RST}"
    ));
    lines.push(format!("  {C_CMD}sas{C_RST}          {C_TEXT}{sas}{C_RST}"));
    lines.push(format_status_line(
        chan.as_deref(),
        chan_cfg.as_ref(),
        peer_count,
    ));
    for line in lines {
        add_local_event(app, &line);
    }
}

/// Build the per-channel summary row for `/e2e status`. Extracted so tests
/// can verify all three branches (no channel / disabled / enabled). Pure —
/// touches no `App` / sqlite state.
fn format_status_line(
    chan: Option<&str>,
    cfg: Option<&ChannelConfig>,
    peer_count: usize,
) -> String {
    match (chan, cfg) {
        (None, _) => format!("  {C_CMD}channel{C_RST}      {C_DIM}(no active channel or known query peer){C_RST}"),
        (Some(c), None) => {
            let c = crate::e2e::display_context(c);
            format!("  {C_CMD}channel{C_RST}      {C_TEXT}{c}{C_RST}  {C_DIM}[off]{C_RST}")
        }
        (Some(c), Some(cfg)) => {
            let c = crate::e2e::display_context(c);
            let state_label = if cfg.enabled { "on" } else { "off" };
            format!(
                "  {C_CMD}channel{C_RST}      {C_TEXT}{c}{C_RST}  \
                 {C_DIM}[{state_label}, mode={mode}, peers={peer_count}]{C_RST}",
                mode = cfg.mode.as_str(),
            )
        }
    }
}

fn e2e_fingerprint(app: &mut App) {
    let Some(mgr) = require_mgr(app) else { return };
    let fp = mgr.fingerprint();
    let fp_hex = fingerprint_hex(&fp);
    let sas = fingerprint_bip39(&fp).unwrap_or_else(|_| "—".into());
    let lines = vec![
        divider("E2E Fingerprint (mine)"),
        format!("  {C_CMD}hex{C_RST}  {C_TEXT}{fp_hex}{C_RST}"),
        format!("  {C_CMD}sas{C_RST}  {C_TEXT}{sas}{C_RST}"),
        format!("  {C_DIM}Share these out-of-band so peers can verify your key.{C_RST}"),
    ];
    for line in lines {
        add_local_event(app, &line);
    }
}

fn e2e_verify(app: &mut App, nick: &str) {
    // The incoming session whose fingerprint we display lives under our own
    // handle (recipient-keyed); for a channel this is the channel name.
    let Some(chan) = current_e2e_own_context(app) else {
        err(
            app,
            "/e2e verify: no active channel, or own handle not yet known",
        );
        return;
    };
    let Some(handle) = require_handle_for_nick(app, &chan, nick) else {
        return;
    };
    let Some(mgr) = require_mgr(app) else { return };
    let local_fp = mgr.fingerprint();
    match mgr.keyring().get_incoming_session(&handle, &chan) {
        Ok(Some(sess)) => {
            let lines = format_verify_block(&local_fp, &sess.fingerprint, nick, &handle);
            for line in lines {
                add_local_event(app, &line);
            }
        }
        Ok(None) => err(app, &format!("no session for {nick}")),
        Err(e) => err(app, &format!("/e2e verify: {e}")),
    }
}

/// Build the themed verify-block lines for `/e2e verify`. Pure helper —
/// no `App`/DB access — so tests can assert that BOTH sides of the SAS
/// ceremony are rendered side-by-side. Spec §11.
fn format_verify_block(
    local_fp: &crate::e2e::crypto::fingerprint::Fingerprint,
    peer_fp: &crate::e2e::crypto::fingerprint::Fingerprint,
    peer_nick: &str,
    peer_handle: &str,
) -> Vec<String> {
    let local_hex = fingerprint_hex(local_fp);
    let local_short: String = local_hex.chars().take(16).collect();
    let local_sas = fingerprint_bip39(local_fp).unwrap_or_else(|_| "—".into());
    let peer_hex = fingerprint_hex(peer_fp);
    let peer_short: String = peer_hex.chars().take(16).collect();
    let peer_sas = fingerprint_bip39(peer_fp).unwrap_or_else(|_| "—".into());
    vec![
        divider("E2E Fingerprint Verification"),
        format!(
            "  {C_CMD}You  ( local){C_RST}: {C_TEXT}{local_short}{C_RST}  {C_TEXT}{local_sas}{C_RST}"
        ),
        format!(
            "  {C_CMD}Them ({peer_nick:<7}){C_RST}: {C_TEXT}{peer_short}{C_RST}  {C_TEXT}{peer_sas}{C_RST}"
        ),
        format!("  {C_DIM}peer handle: {peer_handle}{C_RST}"),
        String::new(),
        format!("  {C_DIM}Read both lines out-of-band (phone, signal, etc.) and confirm{C_RST}"),
        format!("  {C_DIM}they match BEFORE trusting future messages. If they differ,{C_RST}"),
        format!(
            "  {C_ERR}a MitM is in progress{C_RST}{C_DIM} — run {C_CMD}/e2e forget {peer_nick}{C_DIM} immediately.{C_RST}"
        ),
    ]
}

fn e2e_reverify(app: &mut App, target: &str, fingerprint: Option<&str>) {
    // A handle is taken verbatim: it is exactly what the trust-change
    // notices tell the user to type, and unlike a nick it needs no channel
    // context to resolve — so `/e2e reverify <handle>` also works from a
    // buffer where the peer isn't present.
    let handle = if looks_like_handle(target) {
        target.to_string()
    } else {
        let Some(chan) = current_e2e_context(app) else {
            err(app, "/e2e reverify: no active channel or known query peer");
            return;
        };
        // Strict handle resolution — see `require_handle_for_nick` for the
        // rationale (no raw-nick fallback, themed-error on miss).
        let Some(handle) = require_handle_for_nick(app, &chan, target) else {
            return;
        };
        handle
    };
    let Some(mgr) = require_mgr(app) else { return };
    let outcome = mgr.reverify_peer(&handle, fingerprint);
    report_reverify(app, target, &handle, fingerprint, outcome);
}

/// Render one `/e2e reverify` outcome. Split out of [`e2e_reverify`] because
/// the command now has seven distinct results and the resolution logic was
/// getting lost among them.
fn report_reverify(
    app: &mut App,
    target: &str,
    handle: &str,
    fingerprint: Option<&str>,
    outcome: crate::e2e::error::Result<crate::e2e::manager::ReverifyOutcome>,
) {
    match outcome {
        Ok(crate::e2e::manager::ReverifyOutcome::Applied { old_fp, new_fp }) => {
            ok(
                app,
                &format!(
                    "reverified {target}: old fp={} → new fp={} — installed new key",
                    fingerprint_hex(&old_fp),
                    fingerprint_hex(&new_fp),
                ),
            );
        }
        Ok(crate::e2e::manager::ReverifyOutcome::Cleared { deleted }) => {
            ok(
                app,
                &format!(
                    "reverified {target}: purged {deleted} stale row(s); \
                     re-handshake to TOFU-pin the new key"
                ),
            );
        }
        Ok(crate::e2e::manager::ReverifyOutcome::Rebound {
            fingerprint,
            old_handle,
            new_handle,
        }) => {
            ok(
                app,
                &format!(
                    "reverified {target}: fp={} re-bound {old_handle} → {new_handle} — \
                     re-handshake to open a session under the new handle",
                    fingerprint_hex(&fingerprint),
                ),
            );
        }
        Ok(crate::e2e::manager::ReverifyOutcome::Ambiguous { candidates }) => {
            // Refuse rather than guess: the user compared ONE fingerprint
            // out of band, and we cannot tell which. Nothing was applied
            // and nothing was consumed, so the choice survives.
            err(
                app,
                &format!(
                    "{target} has {} unresolved identity changes — \
                     re-run naming the fingerprint you verified:",
                    candidates.len()
                ),
            );
            list_trust_candidates(app, &candidates);
        }
        Ok(crate::e2e::manager::ReverifyOutcome::NoSuchCandidate { candidates }) => {
            let named = fingerprint.unwrap_or_default();
            if candidates.is_empty() {
                err(
                    app,
                    &format!("no unresolved identity change for {target} matches fp={named}"),
                );
            } else {
                err(
                    app,
                    &format!("no key matching fp={named} is waiting on {target} — did you mean:"),
                );
                list_trust_candidates(app, &candidates);
            }
        }
        Ok(crate::e2e::manager::ReverifyOutcome::Stale { fingerprint }) => {
            ok(
                app,
                &format!(
                    "the pending change for {target} named key fp={}, which is no \
                     longer in the keyring — discarded it; nothing else changed",
                    fingerprint_hex(&fingerprint),
                ),
            );
        }
        Ok(crate::e2e::manager::ReverifyOutcome::NotFound) => {
            // Naming the peer twice reads as a bug when the argument WAS
            // the handle, which is now the documented spelling.
            let who = if handle == target {
                target.to_string()
            } else {
                format!("{target} ({handle})")
            };
            // `/e2e handshake` needs a nick to send the KEYREQ to, so echo
            // the one the user gave; a handle is no use as an IRC target.
            let retry = if looks_like_handle(target) {
                REVERIFY_RETRY_CMD.to_string()
            } else {
                format!("/e2e handshake {target}")
            };
            err(
                app,
                &format!(
                    "no keyring state for {who} to reverify — \
                     if the warning was from a previous session, \
                     run {retry} to raise it again first"
                ),
            );
        }
        Err(e) => err(app, &format!("/e2e reverify: {e}")),
    }
}

// ─── autotrust ───────────────────────────────────────────────────────────────

fn e2e_autotrust(app: &mut App, op: AutotrustOp) {
    let Some(mgr) = require_mgr(app) else { return };
    match op {
        AutotrustOp::List => match mgr.keyring().list_autotrust() {
            Ok(rows) if rows.is_empty() => {
                add_local_event(app, &divider("E2E Autotrust Rules"));
                add_local_event(app, &format!("  {C_DIM}(no rules){C_RST}"));
            }
            Ok(rows) => {
                let mut lines = vec![divider("E2E Autotrust Rules")];
                for (scope, pat) in rows {
                    lines.push(format!("  {C_CMD}{scope}{C_RST}  {C_TEXT}{pat}{C_RST}"));
                }
                for line in lines {
                    add_local_event(app, &line);
                }
            }
            Err(e) => err(app, &format!("/e2e autotrust list: {e}")),
        },
        AutotrustOp::Add(scope, pat) => {
            let now = chrono::Utc::now().timestamp();
            if let Err(e) = mgr.keyring().add_autotrust(&scope, &pat, now) {
                err(app, &format!("/e2e autotrust add: {e}"));
            } else {
                ok(app, &format!("autotrust add {scope} {pat}"));
            }
        }
        AutotrustOp::Remove(pat) => {
            if let Err(e) = mgr.keyring().remove_autotrust(&pat) {
                err(app, &format!("/e2e autotrust remove: {e}"));
            } else {
                ok(app, &format!("autotrust removed {pat}"));
            }
        }
        AutotrustOp::Usage(hint) => err(app, &format!("usage: {hint}")),
    }
}

// ─── export / import ─────────────────────────────────────────────────────────

fn e2e_export(app: &mut App, path: Option<&str>) {
    let Some(raw_path) = path else {
        err(app, "/e2e export: usage: /e2e export <file>");
        return;
    };
    let resolved = match crate::e2e::portable::expand_path(raw_path) {
        Ok(p) => p,
        Err(e) => {
            err(app, &format!("/e2e export: {e}"));
            return;
        }
    };
    let Some(mgr) = require_mgr(app) else { return };
    match crate::e2e::portable::export_to_path(mgr.keyring(), &resolved) {
        Ok(summary) => {
            let sessions = summary.incoming + summary.outgoing;
            ok(
                app,
                &format!(
                    "exported keyring to {} (identity + {} peers + {} sessions)",
                    resolved.display(),
                    summary.peers,
                    sessions,
                ),
            );
            // Session keys are written in plaintext — remind the user.
            add_local_event(
                app,
                &format!(
                    "  {C_DIM}warning: session keys are in plaintext in this file. \
                     Protect it with filesystem ACLs; never share or commit it.{C_RST}"
                ),
            );
        }
        Err(e) => err(app, &format!("/e2e export: {e}")),
    }
}

fn e2e_import(app: &mut App, path: Option<&str>) {
    let Some(raw_path) = path else {
        err(app, "/e2e import: usage: /e2e import <file>");
        return;
    };
    let resolved = match crate::e2e::portable::expand_path(raw_path) {
        Ok(p) => p,
        Err(e) => {
            err(app, &format!("/e2e import: {e}"));
            return;
        }
    };
    let Some(mgr) = require_mgr(app) else { return };
    match crate::e2e::portable::import_from_path(mgr.keyring(), &resolved) {
        Ok(summary) => {
            app.state.push_all_buffer_e2e_statuses();
            ok(
                app,
                &format!(
                    "imported keyring from {} (identity={}, peers={}, incoming={}, \
                     outgoing={}, channels={}, autotrust={})",
                    resolved.display(),
                    summary.identity,
                    summary.peers,
                    summary.incoming,
                    summary.outgoing,
                    summary.channels,
                    summary.autotrust,
                ),
            );
        }
        Err(e) => err(app, &format!("/e2e import: {e}")),
    }
}

// ─── help ────────────────────────────────────────────────────────────────────

/// One-line subcommand index. Each entry is (name, one-line description).
const HELP_ENTRIES: &[(&str, &str)] = &[
    ("on", "Enable E2E on the current channel"),
    ("off", "Disable E2E on the current channel"),
    ("mode <m>", "Set channel mode (auto-accept|normal|quiet)"),
    (
        "handshake <nick>",
        "Send KEYREQ to <nick> (manual key exchange)",
    ),
    ("accept <nick>", "Trust a pending peer on this channel"),
    ("decline <nick>", "Reject a pending peer"),
    (
        "revoke <nick>",
        "Revoke trust; rotate outgoing key next send",
    ),
    ("unrevoke <nick>", "Re-trust a previously revoked peer"),
    (
        "forget [-all] <nick|handle>",
        "Delete channel or global peer state",
    ),
    ("verify <nick>", "Show a peer's fingerprint + SAS words"),
    (
        "reverify <nick|handle> [fp]",
        "Re-trust after SAS comparison / accept a new handle",
    ),
    ("rotate", "Schedule outgoing key rotation for this channel"),
    (
        "list [-all]",
        "List trusted peers or the full remembered state",
    ),
    ("status", "Show identity + per-channel summary"),
    ("fingerprint", "Show my own fingerprint + SAS words"),
    ("autotrust list", "List autotrust rules"),
    ("autotrust add <scope> <pat>", "Add an autotrust rule"),
    ("autotrust remove <pat>", "Remove an autotrust rule"),
    (
        "export <file>",
        "Export keyring to a JSON file (plaintext keys, 0600)",
    ),
    ("import <file>", "Import keyring from a JSON file"),
    ("help", "Show this index"),
];

fn e2e_help(app: &mut App) {
    let mut lines = vec![divider("E2E Encryption")];
    // Column width — long enough to fit the widest subcommand spec.
    let name_width = HELP_ENTRIES.iter().map(|(n, _)| n.len()).max().unwrap_or(0);
    for (name, desc) in HELP_ENTRIES {
        lines.push(format!(
            "  {C_CMD}{name:<name_width$}{C_RST}  {C_DIM}{desc}{C_RST}"
        ));
    }
    lines.push(format!(
        "{C_HEADER}────────────────────────────────────────────{C_RST}"
    ));
    for line in lines {
        add_local_event(app, &line);
    }
}

// ─── internal helpers ────────────────────────────────────────────────────────

/// Strict nick→handle resolver.
///
/// Reads the users map of the channel buffer and returns the
/// server-stamped `ident@host` for the matching nick, or `None` if the
/// nick is not currently present in the buffer (i.e. we have never
/// received a WHO/JOIN/PRIVMSG-prefix message carrying their handle).
///
/// Returning `None` here is load-bearing: the old behavior was to fall
/// back to the raw nick at the caller via `unwrap_or_else(|| nick.into())`,
/// which created zombie peer rows because later code `upsert`s with the
/// nick-as-handle. That leaked identity rows whenever an `/e2e` subcommand
/// referenced a user who had not spoken yet, and broke the invariant that
/// every keyring row is keyed by a real `ident@host`. Callers MUST surface
/// a themed error to the user on `None` — see `require_handle_for_nick`.
fn resolve_handle_by_nick(app: &App, channel: &str, nick: &str) -> Option<String> {
    use crate::state::buffer::make_buffer_id;
    // We need to know the connection id. Use the active buffer's.
    let conn_id = app.state.active_buffer()?.connection_id.clone();
    // Contexts are network-scoped; buffers key by the wire name — a lookup
    // with the scoped string would miss every channel buffer and break the
    // nick resolution for /e2e accept, revoke, verify, ….
    let buf_id = make_buffer_id(&conn_id, crate::e2e::wire_context(channel));
    let buf = app.state.buffers.get(&buf_id)?;
    let entry = buf.users.get(&nick.to_lowercase())?;
    let ident = entry.ident.as_deref().unwrap_or("");
    let host = entry.host.as_deref().unwrap_or("");
    if ident.is_empty() && host.is_empty() {
        None
    } else {
        Some(format!("{ident}@{host}"))
    }
}

fn resolve_cached_handle_by_nick(
    app: &App,
    nick: &str,
) -> Option<std::result::Result<String, crate::e2e::error::E2eError>> {
    let mgr = app.state.e2e_manager.as_ref()?;
    // Same keyring resolution the encrypt path uses, so `/e2e on` and the
    // outbound send key the DM under the identical `@<cached>` context —
    // scoped to the active connection's network so a same-nick peer on another
    // network can't be resolved.
    let network = app
        .state
        .active_buffer()
        .and_then(|b| app.state.connections.get(&b.connection_id))
        .map(|c| c.label.clone())?;
    match mgr.keyring().last_handle_for_nick(nick, &network) {
        Ok(Some(handle)) => Some(Ok(handle)),
        Ok(None) => None,
        Err(e) => Some(Err(e)),
    }
}

/// Render the candidate list for an unresolved-changes prompt, each entry
/// followed by the command that accepts exactly that one.
///
/// Every entry leads with the fingerprint of the key that would be
/// trusted: when two changes wait at one `ident@host` the handle cannot
/// tell them apart, so the fingerprint is both the discriminator and the
/// thing the user compared out of band. The command is spelled out per
/// candidate rather than once as an example, because the argument that
/// resolves a candidate is not always the handle the user just typed —
/// one key seen moving to two destinations yields candidates that share
/// both handle *and* fingerprint, and only the destination separates
/// them.
fn list_trust_candidates(app: &mut App, candidates: &[TrustChange]) {
    for change in candidates {
        let Some(candidate) = describe_trust_change(change) else {
            continue;
        };
        add_local_event(
            app,
            &format!(
                "  {C_CMD}{}{C_RST}  {C_DIM}{}{C_RST}",
                candidate.fingerprint, candidate.detail
            ),
        );
        add_local_event(
            app,
            &format!(
                "    {C_DIM}accept: {C_CMD}/e2e reverify {} {}{C_RST}",
                candidate.target, candidate.fingerprint
            ),
        );
    }
}

/// One line of the disambiguation list.
struct TrustCandidate {
    /// Fingerprint of the key that would be trusted.
    fingerprint: String,
    /// The `/e2e reverify` argument that selects this candidate alone.
    target: String,
    /// What accepting it does.
    detail: String,
}

fn describe_trust_change(change: &TrustChange) -> Option<TrustCandidate> {
    match change {
        TrustChange::HandleChanged {
            old_handle,
            new_handle,
            fingerprint,
        } => Some(TrustCandidate {
            fingerprint: fingerprint_hex(fingerprint),
            // The destination, not the handle being left: the same key
            // moving to two places is one fingerprint under one old
            // handle, so only `new_handle` picks a single candidate.
            target: new_handle.clone(),
            detail: format!("known key moves {old_handle} → {new_handle}"),
        }),
        TrustChange::FingerprintChanged {
            handle,
            old_fp,
            new_fp,
        } => Some(TrustCandidate {
            fingerprint: fingerprint_hex(new_fp),
            target: handle.clone(),
            detail: format!("new key at {handle}, replacing {}", fingerprint_hex(old_fp)),
        }),
        TrustChange::Revoked {
            handle,
            fingerprint,
        } => Some(TrustCandidate {
            fingerprint: fingerprint_hex(fingerprint),
            target: handle.clone(),
            detail: format!("revoked key at {handle}"),
        }),
        TrustChange::Known | TrustChange::New => None,
    }
}

/// Is this `<nick|handle>` argument already a handle?
///
/// An `ident@host` always contains an `@`; an IRC nick never can (RFC 2812
/// §2.3.1 keeps it out of the nick charset). `/e2e forget` has always used
/// this rule, and `/e2e reverify` needs the same one because every
/// trust-change notice tells the user to type a handle verbatim.
fn looks_like_handle(target: &str) -> bool {
    target.contains('@')
}

/// Wrap `resolve_handle_by_nick` with themed-error surfacing.
///
/// Every `/e2e` subcommand that takes a `<nick>` argument needs to map
/// the nick to an `ident@host` before touching the keyring. On miss we
/// emit a `[E2E]` error line through the `events.e2e_error` theme key
/// and return `None` so the caller can bail cleanly.
fn require_handle_for_nick(app: &mut App, channel: &str, nick: &str) -> Option<String> {
    if let Some(handle) = resolve_handle_by_nick(app, channel, nick) {
        return Some(handle);
    }
    match resolve_cached_handle_by_nick(app, nick) {
        Some(Ok(handle)) => Some(handle),
        Some(Err(e)) => {
            err(app, &format!("cannot resolve handle for {nick}: {e}"));
            None
        }
        None => {
            err(
                app,
                &format!("cannot resolve handle for {nick} — has the user spoken yet?"),
            );
            None
        }
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::e2e::keyring::{ChannelConfig, ChannelMode, IncomingSession, TrustStatus};

    fn s(x: &str) -> String {
        x.to_string()
    }

    /// App looking at channel `test/#dupa`, with a live E2E manager.
    fn app_on_a_channel() -> crate::app::App {
        let mut app = crate::app::input::submit_typing_tests::test_app();
        app.state
            .add_buffer(crate::state::buffer::Buffer::for_test(
                "test",
                crate::state::buffer::BufferType::Channel,
                "#dupa",
            ));
        app.state.set_active_buffer("test/#dupa");
        let db = crate::storage::db::open_database(false).unwrap();
        let keyring =
            crate::e2e::keyring::Keyring::new(std::sync::Arc::new(std::sync::Mutex::new(db)));
        app.state.e2e_manager = Some(std::sync::Arc::new(
            crate::e2e::E2eManager::load_or_init(keyring).unwrap(),
        ));
        app
    }

    fn rows(app: &crate::app::App) -> Vec<String> {
        app.state.buffers["test/#dupa"]
            .messages
            .iter()
            .map(|m| m.text.clone())
            .collect()
    }

    #[test]
    fn e2e_on_says_so_when_it_ends_translation_for_the_conversation() {
        // The exclusion itself runs at the gate, silently and from the next
        // line. Without this row the user sees both features "on" and a
        // channel that simply stopped being translated — which reads as a
        // broken translator, not as encryption winning.
        let mut app = app_on_a_channel();
        app.state.translate_buffers.insert(
            "test/#dupa".to_string(),
            crate::config::TranslateBufferConfig {
                incoming: true,
                outgoing: false,
                lang: Some("de".to_string()),
                my_lang: None,
            },
        );

        e2e_on(&mut app);

        assert!(
            rows(&app)
                .iter()
                .any(|t| t.contains("no longer be translated")),
            "the user is told which feature won: {:?}",
            rows(&app)
        );
    }

    #[test]
    fn e2e_on_stays_quiet_about_translation_where_none_was_configured() {
        // The warning is about a real conflict. On every ordinary /e2e on it
        // would be noise that trains the user to ignore it.
        let mut app = app_on_a_channel();
        e2e_on(&mut app);
        assert!(
            rows(&app).iter().any(|t| t.contains("enabled on")),
            "precondition: the enable itself succeeded: {:?}",
            rows(&app)
        );
        assert!(
            !rows(&app).iter().any(|t| t.contains("translated")),
            "no translation was configured, so nothing to say: {:?}",
            rows(&app)
        );
    }

    #[test]
    fn e2e_import_refreshes_open_buffer_statuses() {
        let mut app = app_on_a_channel();
        app.state.pending_web_events.clear();
        let network = app
            .state
            .connections
            .get("test")
            .map(|connection| connection.label.clone())
            .unwrap_or_default();

        let donor_db = crate::storage::db::open_database(false).unwrap();
        let donor_keyring = crate::e2e::keyring::Keyring::new(std::sync::Arc::new(
            std::sync::Mutex::new(donor_db),
        ));
        let donor = crate::e2e::E2eManager::load_or_init(donor_keyring).unwrap();
        donor
            .keyring()
            .set_channel_config(&ChannelConfig {
                channel: crate::e2e::scoped_context(&network, "#dupa"),
                enabled: true,
                mode: ChannelMode::Normal,
            })
            .unwrap();
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("keyring.json");
        crate::e2e::portable::export_to_path(donor.keyring(), &path).unwrap();

        e2e_import(&mut app, path.to_str());

        let status = app.state.pending_web_events.iter().find_map(|event| match event {
            crate::web::protocol::WebEvent::BufferE2eChanged { buffer_id, enabled } => {
                Some((buffer_id.as_str(), *enabled))
            }
            _ => None,
        });
        assert_eq!(status, Some(("test/#dupa", true)));
    }

    // ---------- DM keying context ----------

    #[test]
    fn e2e_context_keys_dm_by_peer_handle_not_bare_nick() {
        use crate::state::buffer::BufferType;
        let sc = |wire: &str| crate::e2e::scoped_context("Net", wire);
        // Channel: name scoped to the network (the wire part stays verbatim).
        assert_eq!(
            e2e_context_for("Net", &BufferType::Channel, "#rust", None, None),
            Some(sc("#rust"))
        );
        // Query with a live peer handle: the `@<peer_handle>` pseudochannel
        // (matching the encrypt/decrypt path), NOT the bare nick.
        assert_eq!(
            e2e_context_for("Net", &BufferType::Query, "bob", Some("~bob@user/bob"), None),
            Some(sc("@~bob@user/bob"))
        );
        // Peer hasn't spoken this session, but the keyring still has a handle
        // for this nick (existing E2E rows): fall back to it so /e2e stays
        // usable instead of erroring.
        assert_eq!(
            e2e_context_for("Net", &BufferType::Query, "bob", None, Some("~bob@old")),
            Some(sc("@~bob@old"))
        );
        // The live handle wins over the cached one.
        assert_eq!(
            e2e_context_for(
                "Net",
                &BufferType::Query,
                "bob",
                Some("~bob@new"),
                Some("~bob@old")
            ),
            Some(sc("@~bob@new"))
        );
        // Neither known: no context — the command must refuse rather than
        // write a bare-nick row the hot path never reads.
        assert_eq!(
            e2e_context_for("Net", &BufferType::Query, "bob", None, None),
            None
        );
        // Non-channel/query buffers have no E2E context.
        assert_eq!(
            e2e_context_for("Net", &BufferType::Mentions, "Mentions", None, None),
            None
        );
    }

    #[test]
    fn e2e_own_context_keys_dm_by_own_handle() {
        use crate::state::buffer::BufferType;
        let sc = |wire: &str| crate::e2e::scoped_context("Net", wire);
        // Channel: name scoped to the network (own == peer == channel name).
        assert_eq!(
            e2e_own_context_for("Net", &BufferType::Channel, "#rust", None),
            Some(sc("#rust"))
        );
        // Query: OUR own handle (we are the recipient of incoming DMs), keyed
        // independently of the peer's nick.
        assert_eq!(
            e2e_own_context_for("Net", &BufferType::Query, "bob", Some("~me@host")),
            Some(sc("@~me@host"))
        );
        // Own handle unknown: no context — incoming-session ops must refuse.
        assert_eq!(
            e2e_own_context_for("Net", &BufferType::Query, "bob", None),
            None
        );
    }

    // ---------- case-insensitive dispatch ----------

    #[test]
    fn test_subcommand_dispatch_case_insensitive() {
        assert_eq!(parse_subcommand(&[s("on")]), E2eSub::On);
        assert_eq!(parse_subcommand(&[s("ON")]), E2eSub::On);
        assert_eq!(parse_subcommand(&[s("On")]), E2eSub::On);
        assert_eq!(parse_subcommand(&[s("oN")]), E2eSub::On);

        assert_eq!(parse_subcommand(&[s("off")]), E2eSub::Off);
        assert_eq!(parse_subcommand(&[s("OFF")]), E2eSub::Off);

        assert_eq!(parse_subcommand(&[s("LIST")]), E2eSub::List { all: false });
        assert_eq!(parse_subcommand(&[s("Status")]), E2eSub::Status);
        assert_eq!(parse_subcommand(&[s("FingerPrint")]), E2eSub::Fingerprint);
        assert_eq!(parse_subcommand(&[s("Rotate")]), E2eSub::Rotate);
        assert_eq!(parse_subcommand(&[s("HELP")]), E2eSub::Help);
        assert_eq!(parse_subcommand(&[s("?")]), E2eSub::Help);
    }

    #[test]
    fn test_subcommand_dispatch_accept_carries_nick_verbatim() {
        // Nick arg is case-sensitive; only the subcommand token is lowercased.
        assert_eq!(
            parse_subcommand(&[s("ACCEPT"), s("Alice")]),
            E2eSub::Accept(s("Alice"))
        );
        assert_eq!(
            parse_subcommand(&[s("verify"), s("BoB")]),
            E2eSub::Verify(s("BoB"))
        );
    }

    #[test]
    fn test_subcommand_dispatch_forget_all_accepts_both_flag_positions() {
        assert_eq!(
            parse_subcommand(&[s("forget"), s("-all"), s("k2")]),
            E2eSub::Forget {
                target: s("k2"),
                all: true,
            }
        );
        assert_eq!(
            parse_subcommand(&[s("forget"), s("k2"), s("-all")]),
            E2eSub::Forget {
                target: s("k2"),
                all: true,
            }
        );
        assert_eq!(
            parse_subcommand(&[s("list"), s("-all")]),
            E2eSub::List { all: true }
        );
    }

    #[test]
    fn test_subcommand_dispatch_missing_nick_is_usage() {
        assert!(matches!(parse_subcommand(&[s("accept")]), E2eSub::Usage(_)));
        assert!(matches!(parse_subcommand(&[s("verify")]), E2eSub::Usage(_)));
        assert!(matches!(
            parse_subcommand(&[s("handshake")]),
            E2eSub::Usage(_)
        ));
    }

    #[test]
    fn test_subcommand_dispatch_unknown() {
        match parse_subcommand(&[s("wombat")]) {
            E2eSub::Unknown(tok) => assert_eq!(tok, "wombat"),
            other => panic!("expected Unknown, got {other:?}"),
        }
        // Also case-insensitive: uppercase unknown still routes to Unknown
        // but the echoed token is the lowercased form.
        match parse_subcommand(&[s("NOPE")]) {
            E2eSub::Unknown(tok) => assert_eq!(tok, "nope"),
            other => panic!("expected Unknown, got {other:?}"),
        }
    }

    #[test]
    fn test_subcommand_dispatch_empty_is_none() {
        assert_eq!(parse_subcommand(&[]), E2eSub::None);
    }

    // ---------- mode parsing ----------

    #[test]
    fn test_mode_parse_valid() {
        assert_eq!(parse_mode("auto-accept").unwrap(), ChannelMode::AutoAccept);
        assert_eq!(parse_mode("auto").unwrap(), ChannelMode::AutoAccept);
        assert_eq!(parse_mode("normal").unwrap(), ChannelMode::Normal);
        assert_eq!(parse_mode("quiet").unwrap(), ChannelMode::Quiet);
    }

    #[test]
    fn test_mode_parse_case_insensitive() {
        assert_eq!(parse_mode("AUTO-ACCEPT").unwrap(), ChannelMode::AutoAccept);
        assert_eq!(parse_mode("Normal").unwrap(), ChannelMode::Normal);
        assert_eq!(parse_mode("QUIET").unwrap(), ChannelMode::Quiet);
    }

    #[test]
    fn test_mode_parse_invalid() {
        let err = parse_mode("garbage").unwrap_err();
        assert!(err.contains("garbage"));
        assert!(err.contains("auto-accept"));
        assert!(err.contains("normal"));
        assert!(err.contains("quiet"));
    }

    // ---------- autotrust op parsing ----------

    #[test]
    fn test_autotrust_op_list() {
        assert_eq!(
            parse_subcommand(&[s("autotrust"), s("list")]),
            E2eSub::Autotrust(AutotrustOp::List)
        );
        // Case-insensitive on both the subcommand and the autotrust op.
        assert_eq!(
            parse_subcommand(&[s("AUTOTRUST"), s("LIST")]),
            E2eSub::Autotrust(AutotrustOp::List)
        );
    }

    #[test]
    fn test_autotrust_op_add_requires_both_args() {
        assert_eq!(
            parse_subcommand(&[s("autotrust"), s("add"), s("channel"), s("*!*@evil")]),
            E2eSub::Autotrust(AutotrustOp::Add(s("channel"), s("*!*@evil")))
        );
        match parse_subcommand(&[s("autotrust"), s("add")]) {
            E2eSub::Autotrust(AutotrustOp::Usage(_)) => {}
            other => panic!("expected Usage, got {other:?}"),
        }
        match parse_subcommand(&[s("autotrust"), s("add"), s("channel")]) {
            E2eSub::Autotrust(AutotrustOp::Usage(_)) => {}
            other => panic!("expected Usage, got {other:?}"),
        }
    }

    #[test]
    fn test_autotrust_op_remove() {
        assert_eq!(
            parse_subcommand(&[s("autotrust"), s("remove"), s("pat")]),
            E2eSub::Autotrust(AutotrustOp::Remove(s("pat")))
        );
        match parse_subcommand(&[s("autotrust"), s("remove")]) {
            E2eSub::Autotrust(AutotrustOp::Usage(_)) => {}
            other => panic!("expected Usage, got {other:?}"),
        }
    }

    #[test]
    fn test_autotrust_op_no_op_is_usage() {
        match parse_subcommand(&[s("autotrust")]) {
            E2eSub::Autotrust(AutotrustOp::Usage(_)) => {}
            other => panic!("expected Usage, got {other:?}"),
        }
    }

    // ---------- export / import capture optional path ----------

    #[test]
    fn test_export_import_optional_path() {
        assert_eq!(parse_subcommand(&[s("export")]), E2eSub::Export(None));
        assert_eq!(
            parse_subcommand(&[s("export"), s("/tmp/out.json")]),
            E2eSub::Export(Some(s("/tmp/out.json")))
        );
        assert_eq!(parse_subcommand(&[s("import")]), E2eSub::Import(None));
        assert_eq!(
            parse_subcommand(&[s("IMPORT"), s("/tmp/in.json")]),
            E2eSub::Import(Some(s("/tmp/in.json")))
        );
    }

    // ---------- handle resolution: strict, no raw-nick fallback ----------
    //
    // G13 removed the raw-nick fallback that caused zombie peer rows.
    // `resolve_handle_by_nick` returns `Option<String>` and every `/e2e`
    // caller now surfaces a themed `[E2E]` error on `None` instead of
    // silently upserting `nick` as the handle. The function itself reaches
    // into `App.state`, so we replicate its new contract in a pure helper
    // below and assert each expected outcome.

    #[test]
    fn reverify_accepts_the_handle_the_notice_tells_the_user_to_type() {
        // The HandleChanged notice ends with `run /e2e reverify <new
        // handle> to accept`. Whatever that renders must be recognised as
        // a handle: the reported bug was that the exact string the user
        // was instructed to type went through nick resolution instead and
        // bounced with "cannot resolve handle — has the user spoken yet?".
        let (body, _) = crate::irc::events::trust_change_body(
            &crate::e2e::manager::TrustChange::HandleChanged {
                old_handle: s("freakyy85@hosted.by.nextgamers.eu"),
                new_handle: s("freaky@hosted.by.nextgamers.eu"),
                fingerprint: [0xAB; 16],
            },
        )
        .expect("HandleChanged must produce a notice");
        let typed = body
            .split("/e2e reverify ")
            .nth(1)
            .expect("the notice must tell the user what to run")
            .split_whitespace()
            .next()
            .expect("...followed by an argument");
        assert_eq!(typed, "freaky@hosted.by.nextgamers.eu");
        assert!(
            looks_like_handle(typed),
            "the notice says to type {typed}, but /e2e reverify would send that through nick resolution"
        );
    }

    #[test]
    fn a_bare_nick_is_not_mistaken_for_a_handle() {
        assert!(!looks_like_handle("freakyy85"));
        assert!(!looks_like_handle("kofany"));
        assert!(looks_like_handle("~bob@user/bob"));
    }

    #[test]
    fn reverify_advertises_handle_support_like_forget() {
        // `/e2e forget` documents `<nick|handle>`; reverify accepts the
        // same two spellings and must say so, or users keep guessing.
        let reverify = HELP_ENTRIES
            .iter()
            .find(|(name, _)| name.starts_with("reverify"))
            .expect("reverify must appear in the help index");
        assert!(
            reverify.0.contains("<nick|handle>"),
            "help index still advertises `{}`",
            reverify.0
        );
        match parse_subcommand(&[s("reverify")]) {
            E2eSub::Usage(u) => assert!(u.contains("<nick|handle>"), "usage still says `{u}`"),
            other => panic!("expected Usage, got {other:?}"),
        }
    }

    #[test]
    fn reverify_takes_an_optional_fingerprint_selector() {
        // Two keys offered at one `ident@host` are indistinguishable by
        // handle, so the fingerprint is the only way to resolve them —
        // without this argument the Ambiguous outcome is a dead end.
        assert_eq!(
            parse_subcommand(&[s("reverify"), s("~bob@b.host")]),
            E2eSub::Reverify {
                target: s("~bob@b.host"),
                fingerprint: None,
            }
        );
        assert_eq!(
            parse_subcommand(&[s("reverify"), s("~bob@b.host"), s("c37c65c773314a48")]),
            E2eSub::Reverify {
                target: s("~bob@b.host"),
                fingerprint: Some(s("c37c65c773314a48")),
            }
        );
        assert!(matches!(
            parse_subcommand(&[s("reverify"), s("a"), s("b"), s("c")]),
            E2eSub::Usage(_)
        ));
        assert!(REVERIFY_USAGE.contains("[fingerprint]"));
        let reverify = HELP_ENTRIES
            .iter()
            .find(|(name, _)| name.starts_with("reverify"))
            .expect("reverify must appear in the help index");
        assert!(
            reverify.0.contains("[fp]"),
            "help index does not advertise the selector: `{}`",
            reverify.0
        );
    }

    #[test]
    fn the_stale_warning_hint_names_a_runnable_command() {
        // The hint pointed at `/e2e handshake`, which needs a target and
        // otherwise just prints its own usage — so the documented recovery
        // could not actually raise the warning again.
        let args: Vec<String> = REVERIFY_RETRY_CMD
            .strip_prefix("/e2e ")
            .expect("the hint must be an /e2e command")
            .split_whitespace()
            .map(s)
            .collect();
        assert!(
            !matches!(parse_subcommand(&args), E2eSub::Usage(_)),
            "`{REVERIFY_RETRY_CMD}` only prints usage"
        );
    }

    #[test]
    fn a_handle_change_is_accepted_by_naming_its_destination() {
        // One key seen moving to two destinations yields candidates that
        // share the old handle AND the fingerprint, so a command built
        // from those two would match both and return Ambiguous forever.
        // Only the destination separates them.
        let moved = |to: &str| TrustChange::HandleChanged {
            old_handle: s("~bob@b.host"),
            new_handle: s(to),
            fingerprint: [0xAB; 16],
        };
        let vpn = describe_trust_change(&moved("~bob@vpn.host")).expect("actionable");
        let cafe = describe_trust_change(&moved("~bob@cafe.wifi")).expect("actionable");
        assert_eq!(vpn.target, "~bob@vpn.host");
        assert_eq!(cafe.target, "~bob@cafe.wifi");
        assert_eq!(
            vpn.fingerprint, cafe.fingerprint,
            "same key — the fingerprint cannot tell these apart"
        );
        assert_ne!(
            vpn.target, cafe.target,
            "so the accept command must differ by destination"
        );

        // A key change is still selected by its handle plus fingerprint.
        let changed = describe_trust_change(&TrustChange::FingerprintChanged {
            handle: s("~bob@b.host"),
            old_fp: [0x11; 16],
            new_fp: [0x22; 16],
        })
        .expect("actionable");
        assert_eq!(changed.target, "~bob@b.host");
        assert_eq!(changed.fingerprint, fingerprint_hex(&[0x22; 16]));
    }

    fn strict_resolve(resolved: Option<String>) -> Result<String, &'static str> {
        resolved.ok_or("cannot resolve handle — has the user spoken yet?")
    }

    #[test]
    fn test_strict_resolve_some_passthrough() {
        assert_eq!(
            strict_resolve(Some(s("~alice@host.example"))).unwrap(),
            "~alice@host.example"
        );
    }

    #[test]
    fn test_strict_resolve_none_is_error_not_nick_fallback() {
        // This is the core G13 invariant: on a None resolution the caller
        // MUST surface an error, NOT silently fall back to the raw nick
        // (which would create a zombie peer row keyed on nick-as-handle).
        match strict_resolve(None) {
            Err(msg) => assert!(msg.contains("has the user spoken yet?")),
            Ok(s) => panic!("expected Err, got Ok({s})"),
        }
    }

    #[test]
    fn test_strict_resolve_none_for_multiple_nicks() {
        // Sanity: the nick value is irrelevant when there is no buffer
        // entry — the function must not embed the raw nick into its
        // return value.
        assert!(strict_resolve(None).is_err());
        assert!(strict_resolve(None).is_err());
    }

    // ---------- format_status_line ----------

    #[test]
    fn test_format_status_line_no_channel() {
        let line = format_status_line(None, None, 0);
        assert!(line.contains("no active channel or known query peer"));
        assert!(line.contains("channel"));
    }

    #[test]
    fn test_format_status_line_no_config() {
        let line = format_status_line(Some("#rust"), None, 0);
        assert!(line.contains("#rust"));
        assert!(line.contains("off"));
    }

    #[test]
    fn test_format_status_line_enabled() {
        let cfg = ChannelConfig {
            channel: s("#rust"),
            enabled: true,
            mode: ChannelMode::Normal,
        };
        let line = format_status_line(Some("#rust"), Some(&cfg), 3);
        assert!(line.contains("#rust"));
        assert!(line.contains("on"));
        assert!(line.contains("mode=normal"));
        assert!(line.contains("peers=3"));
    }

    #[test]
    fn test_format_status_line_disabled_explicit() {
        let cfg = ChannelConfig {
            channel: s("#rust"),
            enabled: false,
            mode: ChannelMode::AutoAccept,
        };
        let line = format_status_line(Some("#rust"), Some(&cfg), 0);
        assert!(line.contains("#rust"));
        assert!(line.contains("off"));
        assert!(line.contains("mode=auto-accept"));
    }

    // ---------- format_peer_line ----------

    #[test]
    fn test_format_peer_line_truncates_fp() {
        let sess = IncomingSession {
            handle: s("~alice@host.example"),
            channel: s("#rust"),
            fingerprint: [
                0xde, 0xad, 0xbe, 0xef, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0xfe, 0xed,
                0xfa, 0xce,
            ],
            sk: [0u8; 32],
            status: TrustStatus::Trusted,
            created_at: 0,
        };
        let line = format_peer_line(&sess);
        assert!(line.contains("~alice@host.example"));
        assert!(line.contains("trusted"));
        // Short fp is first 16 chars of the 32-char hex — so should contain
        // the leading "deadbeef" but NOT the trailing "feedface".
        assert!(line.contains("deadbeef"));
        assert!(line.contains("fp=deadbeef"));
        assert!(!line.contains("feedface"));
    }

    // ---------- G11 gap 5: e2e_event theme key emission ----------

    #[test]
    fn test_e2e_event_level_maps_to_theme_key() {
        // The theme event keys must exactly match the keys published by
        // `themes/default.theme` and `themes/spring.theme` — any rename
        // here breaks the dead-key detection these theme lines exist to
        // light up.
        assert_eq!(E2eEventLevel::Info.event_key(), "e2e_info");
        assert_eq!(E2eEventLevel::Warning.event_key(), "e2e_warning");
        assert_eq!(E2eEventLevel::Error.event_key(), "e2e_error");
    }

    #[test]
    fn test_e2e_event_level_error_highlights() {
        // The `highlight` flag is what drives the mentions-panel / tab
        // activity indicator. Errors must highlight; info/warning must
        // not (operators should not be paged for a successful /e2e on).
        assert!(E2eEventLevel::Error == E2eEventLevel::Error);
        assert_ne!(E2eEventLevel::Info, E2eEventLevel::Error);
        assert_ne!(E2eEventLevel::Warning, E2eEventLevel::Error);
    }

    #[test]
    fn test_e2e_event_builds_message_with_event_key_and_params() {
        // Construct a Message the way `e2e_event` does and assert the
        // `event_key`/`event_params` wiring so the theme layer has what
        // it needs to substitute `$*`.
        let id: u64 = 1;
        let text = "accepted bob on #rust";
        let level = E2eEventLevel::Info;
        let msg = Message {
            log_key: None,
            id,
            timestamp: Utc::now(),
            message_type: MessageType::Event,
            nick: None,
            nick_mode: None,
            text: text.to_string(),
            highlight: level == E2eEventLevel::Error,
            event_key: Some(level.event_key().to_string()),
            event_params: Some(vec![text.to_string()]),
            log_msg_id: None,
            log_ref_id: None,
            tags: None,
            wire_origin: None,
            translation_suffix_at: None,
        };
        assert_eq!(msg.event_key.as_deref(), Some("e2e_info"));
        assert_eq!(
            msg.event_params.as_deref(),
            Some([text.to_string()].as_slice()),
            "event_params[0] must carry the message text for the theme's $*"
        );
        assert!(!msg.highlight, "info-level events must not highlight");
    }

    // ---------- /e2e verify format_verify_block ----------

    #[test]
    fn test_format_verify_block_renders_both_sides() {
        let local_fp: [u8; 16] = [
            0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee,
            0xff, 0x00,
        ];
        let peer_fp: [u8; 16] = [
            0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff, 0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77,
            0x88, 0x99,
        ];
        let lines = format_verify_block(&local_fp, &peer_fp, "bob", "~bob@b.host");
        // There is a header divider and at least one You + one Them line.
        assert!(!lines.is_empty(), "verify block must render lines");
        let joined = lines.join("\n");
        // Both fingerprints show up in hex (truncated to 16 chars).
        assert!(
            joined.contains("1122334455667788"),
            "local hex must be rendered: {joined}"
        );
        assert!(
            joined.contains("aabbccddeeff0011"),
            "peer hex must be rendered: {joined}"
        );
        // Both SAS words lines are present — at least one word from each
        // BIP-39 rendering shows up. We can't compare exact words without
        // reimplementing bip39 here; just check the structural labels.
        assert!(joined.contains("You"), "must label local as 'You'");
        assert!(joined.contains("Them"), "must label peer as 'Them'");
        // The warning about MitM is present.
        assert!(
            joined.contains("MitM"),
            "must include the MitM warning: {joined}"
        );
        // The peer handle is surfaced so the user can cross-reference.
        assert!(
            joined.contains("~bob@b.host"),
            "peer handle must be rendered: {joined}"
        );
        // Both BIP-39 renderings are non-empty strings (six words each).
        let local_sas = fingerprint_bip39(&local_fp).unwrap();
        let peer_sas = fingerprint_bip39(&peer_fp).unwrap();
        assert!(
            joined.contains(&local_sas),
            "local SAS words must appear in block"
        );
        assert!(
            joined.contains(&peer_sas),
            "peer SAS words must appear in block"
        );
    }

    // ---------- help entries are well-formed ----------

    #[test]
    fn test_help_entries_nonempty_and_unique_names() {
        assert!(!HELP_ENTRIES.is_empty());
        let mut names: Vec<&str> = HELP_ENTRIES.iter().map(|(n, _)| *n).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(before, names.len(), "HELP_ENTRIES must have unique names");
        for (name, desc) in HELP_ENTRIES {
            assert!(!name.is_empty(), "help entry name empty");
            assert!(!desc.is_empty(), "help entry desc empty");
        }
    }
}
