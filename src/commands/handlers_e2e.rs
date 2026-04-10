#![allow(clippy::redundant_pub_crate)]
//! `/e2e` command handlers for RPE2E v1.0.
//!
//! Subcommand dispatch on a single top-level `/e2e` entry point. Each helper
//! is kept small and delegates to `E2eManager`/`Keyring` for the heavy work.

use super::helpers::add_local_event;
use crate::app::App;
use crate::e2e::crypto::fingerprint::{fingerprint_bip39, fingerprint_hex};
use crate::e2e::keyring::{ChannelConfig, ChannelMode, TrustStatus};

/// Single `/e2e` entry point. Dispatches on the first arg.
pub(crate) fn cmd_e2e(app: &mut App, args: &[String]) {
    let Some(sub) = args.first() else {
        add_local_event(
            app,
            "usage: /e2e <on|off|mode|accept|decline|handshake|revoke|unrevoke|\
             forget|autotrust|list|status|fingerprint|verify|reverify|rotate|export|import>",
        );
        return;
    };
    match sub.as_str() {
        "on" => e2e_on(app),
        "off" => e2e_off(app),
        "mode" => e2e_mode(app, &args[1..]),
        "accept" => e2e_accept(app, &args[1..]),
        "decline" => e2e_decline(app, &args[1..]),
        "handshake" => e2e_handshake(app, &args[1..]),
        "revoke" => e2e_revoke(app, &args[1..]),
        "unrevoke" => e2e_unrevoke(app, &args[1..]),
        "forget" => e2e_forget(app, &args[1..]),
        "autotrust" => e2e_autotrust(app, &args[1..]),
        "list" => e2e_list(app),
        "status" => e2e_status(app),
        "fingerprint" => e2e_fingerprint(app),
        "verify" => e2e_verify(app, &args[1..]),
        "reverify" => e2e_reverify(app, &args[1..]),
        "rotate" => e2e_rotate(app),
        "export" => e2e_export(app, &args[1..]),
        "import" => e2e_import(app, &args[1..]),
        other => add_local_event(app, &format!("unknown /e2e subcommand: {other}")),
    }
}

// ---------- helpers ----------

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
            "[E2E] manager not initialized (check logging.enabled / e2e.enabled)",
        );
    }
    mgr
}

// ---------- on/off/mode ----------

fn e2e_on(app: &mut App) {
    let Some(chan) = current_channel(app) else {
        add_local_event(app, "[E2E] /e2e on: no active channel");
        return;
    };
    let Some(mgr) = require_mgr(app) else { return };
    let cfg = ChannelConfig {
        channel: chan.clone(),
        enabled: true,
        mode: ChannelMode::Normal,
    };
    if let Err(e) = mgr.keyring().set_channel_config(&cfg) {
        add_local_event(app, &format!("[E2E] /e2e on: {e}"));
        return;
    }
    add_local_event(app, &format!("[E2E] enabled on {chan} (mode=normal)"));
}

fn e2e_off(app: &mut App) {
    let Some(chan) = current_channel(app) else {
        add_local_event(app, "[E2E] /e2e off: no active channel");
        return;
    };
    let Some(mgr) = require_mgr(app) else { return };
    let cfg = ChannelConfig {
        channel: chan.clone(),
        enabled: false,
        mode: ChannelMode::Normal,
    };
    if let Err(e) = mgr.keyring().set_channel_config(&cfg) {
        add_local_event(app, &format!("[E2E] /e2e off: {e}"));
        return;
    }
    add_local_event(app, &format!("[E2E] disabled on {chan}"));
}

fn e2e_mode(app: &mut App, args: &[String]) {
    let Some(chan) = current_channel(app) else {
        add_local_event(app, "[E2E] /e2e mode: no active channel");
        return;
    };
    let Some(mode_str) = args.first() else {
        add_local_event(app, "[E2E] usage: /e2e mode <auto-accept|normal|quiet>");
        return;
    };
    let mode = ChannelMode::parse(mode_str);
    let Some(mgr) = require_mgr(app) else { return };
    let cfg = ChannelConfig {
        channel: chan.clone(),
        enabled: true,
        mode,
    };
    if let Err(e) = mgr.keyring().set_channel_config(&cfg) {
        add_local_event(app, &format!("[E2E] /e2e mode: {e}"));
        return;
    }
    add_local_event(app, &format!("[E2E] mode={} on {chan}", mode.as_str()));
}

// ---------- trust transitions ----------

fn e2e_accept(app: &mut App, args: &[String]) {
    let Some(nick) = args.first() else {
        add_local_event(app, "[E2E] usage: /e2e accept <nick>");
        return;
    };
    let Some(chan) = current_channel(app) else {
        add_local_event(app, "[E2E] /e2e accept: no active channel");
        return;
    };
    let Some(mgr) = require_mgr(app) else { return };
    // We match by nick — the keyring key is ident@host, but at command time
    // the user types the nick. Look up the sender's full handle from buffer
    // users if present, otherwise accept any handle starting with the nick.
    let handle = resolve_handle_by_nick(app, &chan, nick).unwrap_or_else(|| nick.clone());
    if let Err(e) = mgr.keyring().update_incoming_status(&handle, &chan, TrustStatus::Trusted) {
        add_local_event(app, &format!("[E2E] /e2e accept: {e}"));
        return;
    }
    add_local_event(app, &format!("[E2E] accepted {nick} ({handle}) on {chan}"));
}

fn e2e_decline(app: &mut App, args: &[String]) {
    let Some(nick) = args.first() else {
        add_local_event(app, "[E2E] usage: /e2e decline <nick>");
        return;
    };
    let Some(chan) = current_channel(app) else {
        return;
    };
    let Some(mgr) = require_mgr(app) else { return };
    let handle = resolve_handle_by_nick(app, &chan, nick).unwrap_or_else(|| nick.clone());
    let _ = mgr
        .keyring()
        .update_incoming_status(&handle, &chan, TrustStatus::Revoked);
    add_local_event(app, &format!("[E2E] declined {nick} on {chan}"));
}

fn e2e_revoke(app: &mut App, args: &[String]) {
    let Some(nick) = args.first() else {
        add_local_event(app, "[E2E] usage: /e2e revoke <nick>");
        return;
    };
    let Some(chan) = current_channel(app) else {
        return;
    };
    let Some(mgr) = require_mgr(app) else { return };
    let handle = resolve_handle_by_nick(app, &chan, nick).unwrap_or_else(|| nick.clone());
    if let Err(e) = mgr
        .keyring()
        .update_incoming_status(&handle, &chan, TrustStatus::Revoked)
    {
        add_local_event(app, &format!("[E2E] revoke: {e}"));
        return;
    }
    if let Err(e) = mgr.keyring().mark_outgoing_pending_rotation(&chan) {
        add_local_event(app, &format!("[E2E] mark_rotation: {e}"));
        return;
    }
    add_local_event(
        app,
        &format!("[E2E] revoked {nick} on {chan} — key will rotate on next message"),
    );
}

fn e2e_unrevoke(app: &mut App, args: &[String]) {
    let Some(nick) = args.first() else {
        add_local_event(app, "[E2E] usage: /e2e unrevoke <nick>");
        return;
    };
    let Some(chan) = current_channel(app) else {
        return;
    };
    let Some(mgr) = require_mgr(app) else { return };
    let handle = resolve_handle_by_nick(app, &chan, nick).unwrap_or_else(|| nick.clone());
    let _ = mgr
        .keyring()
        .update_incoming_status(&handle, &chan, TrustStatus::Trusted);
    add_local_event(app, &format!("[E2E] unrevoked {nick} on {chan}"));
}

fn e2e_forget(app: &mut App, args: &[String]) {
    let Some(nick) = args.first() else {
        add_local_event(app, "[E2E] usage: /e2e forget <nick>");
        return;
    };
    let Some(chan) = current_channel(app) else {
        return;
    };
    let Some(mgr) = require_mgr(app) else { return };
    let handle = resolve_handle_by_nick(app, &chan, nick).unwrap_or_else(|| nick.clone());
    let _ = mgr.keyring().delete_incoming_session(&handle, &chan);
    add_local_event(app, &format!("[E2E] forgot {nick} on {chan}"));
}

// ---------- handshake / rotate ----------

fn e2e_handshake(app: &mut App, args: &[String]) {
    let _ = args; // reserved for future: /e2e handshake <nick>
    let Some(chan) = current_channel(app) else {
        add_local_event(app, "[E2E] /e2e handshake: no active channel");
        return;
    };
    let Some(mgr) = require_mgr(app) else { return };
    match mgr.build_keyreq(&chan) {
        Ok(req) => {
            let ctcp = mgr.encode_keyreq_ctcp(&req);
            add_local_event(
                app,
                &format!("[E2E] KEYREQ built for {chan} ({} bytes, deliver via NOTICE)", ctcp.len()),
            );
            // NOTE: sending the NOTICE requires the active IRC handle
            // plumbing; a follow-up patch wires KEYREQ delivery into the
            // send path. For v0.1 the user can manually `/notice` or rely
            // on auto-handshake triggered by receiving ciphertext from a
            // peer in auto-accept mode.
        }
        Err(e) => add_local_event(app, &format!("[E2E] handshake error: {e}")),
    }
}

fn e2e_rotate(app: &mut App) {
    let Some(chan) = current_channel(app) else {
        add_local_event(app, "[E2E] /e2e rotate: no active channel");
        return;
    };
    let Some(mgr) = require_mgr(app) else { return };
    if let Err(e) = mgr.keyring().mark_outgoing_pending_rotation(&chan) {
        add_local_event(app, &format!("[E2E] rotate: {e}"));
        return;
    }
    add_local_event(app, &format!("[E2E] rotation scheduled for {chan}"));
}

// ---------- listings ----------

fn e2e_list(app: &mut App) {
    let Some(chan) = current_channel(app) else {
        add_local_event(app, "[E2E] /e2e list: no active channel");
        return;
    };
    let Some(mgr) = require_mgr(app) else { return };
    match mgr.keyring().list_trusted_peers_for_channel(&chan) {
        Ok(peers) => {
            if peers.is_empty() {
                add_local_event(app, &format!("[E2E] no trusted peers on {chan}"));
            } else {
                add_local_event(app, &format!("[E2E] trusted peers on {chan}:"));
                for p in peers {
                    add_local_event(
                        app,
                        &format!(
                            "  {}  fp={}  status={:?}",
                            p.handle,
                            hex::encode(p.fingerprint),
                            p.status
                        ),
                    );
                }
            }
        }
        Err(e) => add_local_event(app, &format!("[E2E] list: {e}")),
    }
}

fn e2e_status(app: &mut App) {
    let Some(mgr) = require_mgr(app) else { return };
    let fp = mgr.fingerprint();
    add_local_event(
        app,
        &format!("[E2E] identity fingerprint: {}", fingerprint_hex(&fp)),
    );
    match fingerprint_bip39(&fp) {
        Ok(sas) => add_local_event(app, &format!("[E2E] SAS (6 words): {sas}")),
        Err(e) => add_local_event(app, &format!("[E2E] bip39: {e}")),
    }
}

fn e2e_fingerprint(app: &mut App) {
    let Some(mgr) = require_mgr(app) else { return };
    let fp = mgr.fingerprint();
    add_local_event(
        app,
        &format!("[E2E] my fingerprint (hex): {}", fingerprint_hex(&fp)),
    );
    if let Ok(sas) = fingerprint_bip39(&fp) {
        add_local_event(app, &format!("[E2E] my SAS  (bip39): {sas}"));
    }
}

fn e2e_verify(app: &mut App, args: &[String]) {
    let Some(nick) = args.first() else {
        add_local_event(app, "[E2E] usage: /e2e verify <nick>");
        return;
    };
    let Some(chan) = current_channel(app) else {
        return;
    };
    let Some(mgr) = require_mgr(app) else { return };
    let handle = resolve_handle_by_nick(app, &chan, nick).unwrap_or_else(|| nick.clone());
    match mgr.keyring().get_incoming_session(&handle, &chan) {
        Ok(Some(sess)) => {
            let hex = fingerprint_hex(&sess.fingerprint);
            let sas = fingerprint_bip39(&sess.fingerprint).unwrap_or_else(|_| "—".into());
            add_local_event(app, &format!("[E2E] {nick} fingerprint:"));
            add_local_event(app, &format!("  hex: {hex}"));
            add_local_event(app, &format!("  sas: {sas}"));
            add_local_event(
                app,
                "  Both sides should see the same 6 words (compare out-of-band).",
            );
        }
        Ok(None) => add_local_event(app, &format!("[E2E] no session for {nick} on {chan}")),
        Err(e) => add_local_event(app, &format!("[E2E] verify: {e}")),
    }
}

fn e2e_reverify(app: &mut App, args: &[String]) {
    // Re-verify is identical to /e2e accept for the happy path: it flips the
    // session to Trusted again after a user has confirmed the new key via SAS.
    e2e_accept(app, args);
}

// ---------- autotrust ----------

fn e2e_autotrust(app: &mut App, args: &[String]) {
    let Some(sub) = args.first() else {
        add_local_event(
            app,
            "[E2E] usage: /e2e autotrust <list|add|remove> [scope] [pattern]",
        );
        return;
    };
    let Some(mgr) = require_mgr(app) else { return };
    match sub.as_str() {
        "list" => match mgr.keyring().list_autotrust() {
            Ok(rows) if rows.is_empty() => add_local_event(app, "[E2E] no autotrust rules"),
            Ok(rows) => {
                add_local_event(app, "[E2E] autotrust rules:");
                for (scope, pat) in rows {
                    add_local_event(app, &format!("  {scope}  {pat}"));
                }
            }
            Err(e) => add_local_event(app, &format!("[E2E] autotrust list: {e}")),
        },
        "add" => {
            let Some(scope) = args.get(1) else {
                add_local_event(app, "[E2E] usage: /e2e autotrust add <scope> <pattern>");
                return;
            };
            let Some(pat) = args.get(2) else {
                add_local_event(app, "[E2E] usage: /e2e autotrust add <scope> <pattern>");
                return;
            };
            let now = chrono::Utc::now().timestamp();
            if let Err(e) = mgr.keyring().add_autotrust(scope, pat, now) {
                add_local_event(app, &format!("[E2E] autotrust add: {e}"));
            } else {
                add_local_event(app, &format!("[E2E] autotrust add {scope} {pat}"));
            }
        }
        "remove" => {
            let Some(pat) = args.get(1) else {
                add_local_event(app, "[E2E] usage: /e2e autotrust remove <pattern>");
                return;
            };
            if let Err(e) = mgr.keyring().remove_autotrust(pat) {
                add_local_event(app, &format!("[E2E] autotrust remove: {e}"));
            } else {
                add_local_event(app, &format!("[E2E] autotrust removed {pat}"));
            }
        }
        other => add_local_event(app, &format!("[E2E] unknown autotrust subcmd: {other}")),
    }
}

// ---------- export / import ----------

fn e2e_export(app: &mut App, args: &[String]) {
    let _ = args;
    add_local_event(
        app,
        "[E2E] export: not yet implemented — use `sqlite3 ~/.repartee/logs/messages.db .dump e2e_%`",
    );
}

fn e2e_import(app: &mut App, args: &[String]) {
    let _ = args;
    add_local_event(app, "[E2E] import: not yet implemented");
}

// ---------- internal helpers ----------

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
