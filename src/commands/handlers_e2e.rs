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
use super::types::{C_CMD, C_DIM, C_ERR, C_HEADER, C_OK, C_RST, C_TEXT, divider};
use crate::app::App;
use crate::e2e::crypto::fingerprint::{fingerprint_bip39, fingerprint_hex};
use crate::e2e::keyring::{ChannelConfig, ChannelMode, IncomingSession, TrustStatus};

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
    Forget(String),
    Autotrust(AutotrustOp),
    List,
    Status,
    Fingerprint,
    Verify(String),
    Reverify(String),
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
        "forget" => rest
            .first()
            .map_or(E2eSub::Usage("/e2e forget <nick>"), |n| {
                E2eSub::Forget(n.clone())
            }),
        "autotrust" => E2eSub::Autotrust(parse_autotrust_op(rest)),
        "list" => E2eSub::List,
        "status" => E2eSub::Status,
        "fingerprint" => E2eSub::Fingerprint,
        "verify" => rest
            .first()
            .map_or(E2eSub::Usage("/e2e verify <nick>"), |n| {
                E2eSub::Verify(n.clone())
            }),
        "reverify" => rest
            .first()
            .map_or(E2eSub::Usage("/e2e reverify <nick>"), |n| {
                E2eSub::Reverify(n.clone())
            }),
        "rotate" => E2eSub::Rotate,
        "export" => E2eSub::Export(rest.first().cloned()),
        "import" => E2eSub::Import(rest.first().cloned()),
        "help" | "?" => E2eSub::Help,
        other => E2eSub::Unknown(other.to_string()),
    }
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
        E2eSub::Forget(nick) => e2e_forget(app, &nick),
        E2eSub::Autotrust(op) => e2e_autotrust(app, op),
        E2eSub::List => e2e_list(app),
        E2eSub::Status => e2e_status(app),
        E2eSub::Fingerprint => e2e_fingerprint(app),
        E2eSub::Verify(nick) => e2e_verify(app, &nick),
        E2eSub::Reverify(nick) => e2e_reverify(app, &nick),
        E2eSub::Rotate => e2e_rotate(app),
        E2eSub::Export(path) => e2e_export(app, path.as_deref()),
        E2eSub::Import(path) => e2e_import(app, path.as_deref()),
        E2eSub::Unknown(other) => {
            add_local_event(app, &format!("{C_ERR}[E2E] unknown subcommand: {other}{C_RST}"));
            e2e_help(app);
        }
        E2eSub::Usage(hint) => {
            add_local_event(app, &format!("{C_ERR}[E2E] usage: {hint}{C_RST}"));
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

/// Pull the active channel name from the current buffer, if it is one.
fn current_channel(app: &App) -> Option<String> {
    use crate::state::buffer::BufferType;
    let buf = app.state.active_buffer()?;
    if matches!(buf.buffer_type, BufferType::Channel | BufferType::Query) {
        Some(buf.name.clone())
    } else {
        None
    }
}

fn require_mgr(app: &mut App) -> Option<std::sync::Arc<crate::e2e::E2eManager>> {
    // Clone the Arc upfront so we can drop the immutable borrow of `app.state`
    // before potentially calling `add_local_event`, which needs `&mut app`.
    let mgr = app.state.e2e_manager.clone();
    if mgr.is_none() {
        add_local_event(
            app,
            &format!(
                "{C_ERR}[E2E] manager not initialized \
                 (check logging.enabled / e2e.enabled){C_RST}"
            ),
        );
    }
    mgr
}

/// Error helper: emit a themed error line with the `[E2E]` tag.
fn err(app: &mut App, msg: &str) {
    add_local_event(app, &format!("{C_ERR}[E2E] {msg}{C_RST}"));
}

/// Info/success helper: emit a themed OK line with the `[E2E]` tag.
fn ok(app: &mut App, msg: &str) {
    add_local_event(app, &format!("{C_OK}[E2E] {msg}{C_RST}"));
}

// ─── on / off / mode ─────────────────────────────────────────────────────────

fn e2e_on(app: &mut App) {
    let Some(chan) = current_channel(app) else {
        err(app, "/e2e on: no active channel");
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
    ok(app, &format!("enabled on {chan} (mode=normal)"));
}

fn e2e_off(app: &mut App) {
    let Some(chan) = current_channel(app) else {
        err(app, "/e2e off: no active channel");
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
    ok(app, &format!("disabled on {chan}"));
}

fn e2e_mode(app: &mut App, mode_str: &str) {
    let Some(chan) = current_channel(app) else {
        err(app, "/e2e mode: no active channel");
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
    ok(app, &format!("mode={} on {chan}", mode.as_str()));
}

// ─── trust transitions ───────────────────────────────────────────────────────

fn e2e_accept(app: &mut App, nick: &str) {
    let Some(chan) = current_channel(app) else {
        err(app, "/e2e accept: no active channel");
        return;
    };
    let Some(mgr) = require_mgr(app) else { return };
    // We match by nick — the keyring key is ident@host, but at command time
    // the user types the nick. Look up the sender's full handle from buffer
    // users if present, otherwise accept any handle starting with the nick.
    let handle = resolve_handle_by_nick(app, &chan, nick).unwrap_or_else(|| nick.to_string());
    if let Err(e) = mgr
        .keyring()
        .update_incoming_status(&handle, &chan, TrustStatus::Trusted)
    {
        err(app, &format!("/e2e accept: {e}"));
        return;
    }
    ok(app, &format!("accepted {nick} ({handle}) on {chan}"));
}

fn e2e_decline(app: &mut App, nick: &str) {
    let Some(chan) = current_channel(app) else {
        err(app, "/e2e decline: no active channel");
        return;
    };
    let Some(mgr) = require_mgr(app) else { return };
    let handle = resolve_handle_by_nick(app, &chan, nick).unwrap_or_else(|| nick.to_string());
    if let Err(e) = mgr
        .keyring()
        .update_incoming_status(&handle, &chan, TrustStatus::Revoked)
    {
        err(app, &format!("/e2e decline: {e}"));
        return;
    }
    ok(app, &format!("declined {nick} on {chan}"));
}

fn e2e_revoke(app: &mut App, nick: &str) {
    let Some(chan) = current_channel(app) else {
        err(app, "/e2e revoke: no active channel");
        return;
    };
    let Some(mgr) = require_mgr(app) else { return };
    let handle = resolve_handle_by_nick(app, &chan, nick).unwrap_or_else(|| nick.to_string());
    if let Err(e) = mgr
        .keyring()
        .update_incoming_status(&handle, &chan, TrustStatus::Revoked)
    {
        err(app, &format!("/e2e revoke: {e}"));
        return;
    }
    if let Err(e) = mgr.keyring().mark_outgoing_pending_rotation(&chan) {
        err(app, &format!("/e2e revoke (mark rotation): {e}"));
        return;
    }
    ok(
        app,
        &format!("revoked {nick} on {chan} — key will rotate on next message"),
    );
}

fn e2e_unrevoke(app: &mut App, nick: &str) {
    let Some(chan) = current_channel(app) else {
        err(app, "/e2e unrevoke: no active channel");
        return;
    };
    let Some(mgr) = require_mgr(app) else { return };
    let handle = resolve_handle_by_nick(app, &chan, nick).unwrap_or_else(|| nick.to_string());
    if let Err(e) = mgr
        .keyring()
        .update_incoming_status(&handle, &chan, TrustStatus::Trusted)
    {
        err(app, &format!("/e2e unrevoke: {e}"));
        return;
    }
    ok(app, &format!("unrevoked {nick} on {chan}"));
}

fn e2e_forget(app: &mut App, nick: &str) {
    let Some(chan) = current_channel(app) else {
        err(app, "/e2e forget: no active channel");
        return;
    };
    let Some(mgr) = require_mgr(app) else { return };
    let handle = resolve_handle_by_nick(app, &chan, nick).unwrap_or_else(|| nick.to_string());
    if let Err(e) = mgr.keyring().delete_incoming_session(&handle, &chan) {
        err(app, &format!("/e2e forget: {e}"));
        return;
    }
    ok(app, &format!("forgot {nick} on {chan}"));
}

// ─── handshake / rotate ──────────────────────────────────────────────────────

fn e2e_handshake(app: &mut App, nick: &str) {
    let Some(chan) = current_channel(app) else {
        err(app, "/e2e handshake: no active channel");
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
            ok(app, &format!("KEYREQ sent to {nick} for {chan}"));
        }
        Err(e) => err(app, &format!("handshake error: {e}")),
    }
}

fn e2e_rotate(app: &mut App) {
    let Some(chan) = current_channel(app) else {
        err(app, "/e2e rotate: no active channel");
        return;
    };
    let Some(mgr) = require_mgr(app) else { return };
    if let Err(e) = mgr.keyring().mark_outgoing_pending_rotation(&chan) {
        err(app, &format!("/e2e rotate: {e}"));
        return;
    }
    ok(app, &format!("rotation scheduled for {chan}"));
}

// ─── listings ────────────────────────────────────────────────────────────────

fn e2e_list(app: &mut App) {
    let Some(chan) = current_channel(app) else {
        err(app, "/e2e list: no active channel");
        return;
    };
    let Some(mgr) = require_mgr(app) else { return };
    let peers = match mgr.keyring().list_trusted_peers_for_channel(&chan) {
        Ok(p) => p,
        Err(e) => {
            err(app, &format!("/e2e list: {e}"));
            return;
        }
    };

    if peers.is_empty() {
        add_local_event(app, &divider(&format!("E2E Peers on {chan}")));
        add_local_event(
            app,
            &format!("  {C_DIM}(no trusted peers — use /e2e accept <nick>){C_RST}"),
        );
        return;
    }

    let mut lines = vec![divider(&format!("E2E Peers on {chan}"))];
    for p in &peers {
        lines.push(format_peer_line(p));
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

    // Current channel (if any) — used for the per-channel summary row.
    let chan = current_channel(app);
    let chan_cfg: Option<ChannelConfig> = chan
        .as_ref()
        .and_then(|c| mgr.keyring().get_channel_config(c).ok().flatten());
    let peer_count = chan
        .as_ref()
        .and_then(|c| mgr.keyring().list_trusted_peers_for_channel(c).ok())
        .map_or(0usize, |v| v.len());

    let mut lines = vec![divider("E2E Status")];
    lines.push(format!(
        "  {C_CMD}identity{C_RST}     {C_TEXT}{fp_hex}{C_RST}"
    ));
    lines.push(format!("  {C_CMD}sas{C_RST}          {C_TEXT}{sas}{C_RST}"));
    lines.push(format_status_line(chan.as_deref(), chan_cfg.as_ref(), peer_count));
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
        (None, _) => format!(
            "  {C_CMD}channel{C_RST}      {C_DIM}(no active channel){C_RST}"
        ),
        (Some(c), None) => format!(
            "  {C_CMD}channel{C_RST}      {C_TEXT}{c}{C_RST}  {C_DIM}[off]{C_RST}"
        ),
        (Some(c), Some(cfg)) => {
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
        format!(
            "  {C_DIM}Share these out-of-band so peers can verify your key.{C_RST}"
        ),
    ];
    for line in lines {
        add_local_event(app, &line);
    }
}

fn e2e_verify(app: &mut App, nick: &str) {
    let Some(chan) = current_channel(app) else {
        err(app, "/e2e verify: no active channel");
        return;
    };
    let Some(mgr) = require_mgr(app) else { return };
    let handle = resolve_handle_by_nick(app, &chan, nick).unwrap_or_else(|| nick.to_string());
    match mgr.keyring().get_incoming_session(&handle, &chan) {
        Ok(Some(sess)) => {
            let hex = fingerprint_hex(&sess.fingerprint);
            let sas = fingerprint_bip39(&sess.fingerprint).unwrap_or_else(|_| "—".into());
            let lines = vec![
                divider(&format!("E2E Verify {nick}")),
                format!("  {C_CMD}handle{C_RST}  {C_TEXT}{handle}{C_RST}"),
                format!("  {C_CMD}hex{C_RST}     {C_TEXT}{hex}{C_RST}"),
                format!("  {C_CMD}sas{C_RST}     {C_TEXT}{sas}{C_RST}"),
                format!(
                    "  {C_DIM}Both sides should see the same 6 SAS words \
                     (compare out-of-band).{C_RST}"
                ),
            ];
            for line in lines {
                add_local_event(app, &line);
            }
        }
        Ok(None) => err(app, &format!("no session for {nick} on {chan}")),
        Err(e) => err(app, &format!("/e2e verify: {e}")),
    }
}

fn e2e_reverify(app: &mut App, nick: &str) {
    // Re-verify is identical to /e2e accept for the happy path: it flips the
    // session to Trusted again after a user has confirmed the new key via SAS.
    e2e_accept(app, nick);
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
                    lines.push(format!(
                        "  {C_CMD}{scope}{C_RST}  {C_TEXT}{pat}{C_RST}"
                    ));
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

fn e2e_export(app: &mut App, _path: Option<&str>) {
    err(
        app,
        "/e2e export: not yet implemented — use `sqlite3 \
         ~/.repartee/logs/messages.db .dump e2e_%`",
    );
}

fn e2e_import(app: &mut App, _path: Option<&str>) {
    err(app, "/e2e import: not yet implemented");
}

// ─── help ────────────────────────────────────────────────────────────────────

/// One-line subcommand index. Each entry is (name, one-line description).
const HELP_ENTRIES: &[(&str, &str)] = &[
    ("on", "Enable E2E on the current channel"),
    ("off", "Disable E2E on the current channel"),
    ("mode <m>", "Set channel mode (auto-accept|normal|quiet)"),
    ("handshake <nick>", "Send KEYREQ to <nick> (manual key exchange)"),
    ("accept <nick>", "Trust a pending peer on this channel"),
    ("decline <nick>", "Reject a pending peer"),
    ("revoke <nick>", "Revoke trust; rotate outgoing key next send"),
    ("unrevoke <nick>", "Re-trust a previously revoked peer"),
    ("forget <nick>", "Delete a peer's session on this channel"),
    ("verify <nick>", "Show a peer's fingerprint + SAS words"),
    ("reverify <nick>", "Re-trust after SAS comparison"),
    ("rotate", "Schedule outgoing key rotation for this channel"),
    ("list", "List trusted peers on this channel"),
    ("status", "Show identity + per-channel summary"),
    ("fingerprint", "Show my own fingerprint + SAS words"),
    ("autotrust list", "List autotrust rules"),
    ("autotrust add <scope> <pat>", "Add an autotrust rule"),
    ("autotrust remove <pat>", "Remove an autotrust rule"),
    ("export [path]", "Export keyring as JSON (placeholder)"),
    ("import [path]", "Import keyring from JSON (placeholder)"),
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

/// Best-effort nick→handle resolver. Reads the users map of the channel buffer
/// and returns the first entry whose nick matches (case-insensitively).
fn resolve_handle_by_nick(app: &App, channel: &str, nick: &str) -> Option<String> {
    use crate::state::buffer::make_buffer_id;
    // We need to know the connection id. Use the active buffer's.
    let conn_id = app.state.active_buffer()?.connection_id.clone();
    let buf_id = make_buffer_id(&conn_id, channel);
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

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::e2e::keyring::{ChannelConfig, ChannelMode, IncomingSession, TrustStatus};

    fn s(x: &str) -> String {
        x.to_string()
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

        assert_eq!(parse_subcommand(&[s("LIST")]), E2eSub::List);
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
    fn test_subcommand_dispatch_missing_nick_is_usage() {
        assert!(matches!(
            parse_subcommand(&[s("accept")]),
            E2eSub::Usage(_)
        ));
        assert!(matches!(
            parse_subcommand(&[s("verify")]),
            E2eSub::Usage(_)
        ));
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

    // ---------- handle resolution fallback ----------
    //
    // `resolve_handle_by_nick` reaches into `App.state`, so direct unit testing
    // requires a full App. We instead replicate its fallback contract in a
    // pure helper and test *that*: when resolution yields None, the caller
    // must use the raw nick as the handle.

    fn resolve_or_fallback(resolved: Option<String>, nick: &str) -> String {
        resolved.unwrap_or_else(|| nick.to_string())
    }

    #[test]
    fn test_handle_resolution_fallback_none_uses_nick() {
        assert_eq!(resolve_or_fallback(None, "alice"), "alice");
        assert_eq!(resolve_or_fallback(None, "Bob"), "Bob");
    }

    #[test]
    fn test_handle_resolution_fallback_some_passthrough() {
        assert_eq!(
            resolve_or_fallback(Some(s("~alice@host")), "alice"),
            "~alice@host"
        );
    }

    // ---------- format_status_line ----------

    #[test]
    fn test_format_status_line_no_channel() {
        let line = format_status_line(None, None, 0);
        assert!(line.contains("no active channel"));
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
