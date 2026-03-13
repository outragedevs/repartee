#![allow(clippy::redundant_pub_crate)]

use super::helpers::add_local_event;
use super::types::{C_CMD, C_DIM, C_ERR, C_OK, C_RST, C_TEXT, divider};
use crate::app::App;
use crate::dcc::types::DccState;

// ─── /dcc dispatcher ──────────────────────────────────────────────────────────

/// Main `/dcc` command dispatcher.
pub(crate) fn cmd_dcc(app: &mut App, args: &[String]) {
    if args.is_empty() {
        add_local_event(app, "Usage: /dcc <chat|close|list|reject> [args...]");
        return;
    }
    let subcmd = args[0].to_lowercase();
    let sub_args = &args[1..];
    match subcmd.as_str() {
        "chat" => cmd_dcc_chat(app, sub_args),
        "close" => cmd_dcc_close(app, sub_args),
        "list" => cmd_dcc_list(app),
        "reject" => cmd_dcc_reject(app, sub_args),
        _ => add_local_event(app, &format!("Unknown DCC command: {subcmd}")),
    }
}

// ─── /dcc chat ────────────────────────────────────────────────────────────────

fn cmd_dcc_chat(app: &mut App, args: &[String]) {
    // `-passive` flag makes us send a passive/reverse DCC offer instead of
    // opening a listener ourselves — useful when NAT prevents inbound connections.
    let passive = args.first().is_some_and(|a| a == "-passive");
    let nick_args = if passive { &args[1..] } else { args };

    if nick_args.is_empty() {
        if passive {
            add_local_event(app, "Usage: /dcc chat -passive <nick>");
            return;
        }
        // No nick given — accept the most recent pending request.
        let pending = app.dcc.find_latest_pending().map(|r| (r.nick.clone(), r.id.clone()));
        if let Some((nick, id)) = pending {
            accept_dcc_chat(app, &nick, &id);
        } else {
            add_local_event(app, "No pending DCC CHAT requests");
        }
        return;
    }

    let nick = &nick_args[0];

    if !passive {
        // Check whether there is already a pending request from this nick;
        // if so, accept it rather than initiating a duplicate outgoing offer.
        let pending = app.dcc.find_pending(nick).map(|r| (r.nick.clone(), r.id.clone()));
        if let Some((pending_nick, id)) = pending {
            accept_dcc_chat(app, &pending_nick, &id);
            return;
        }
    }

    // No pending request found (or passive was requested) — initiate outgoing.
    let nick = nick.clone();
    initiate_dcc_chat(app, &nick, passive);
}

/// Accept a pending DCC CHAT request (stub — wired in Task 8).
fn accept_dcc_chat(app: &mut App, nick: &str, id: &str) {
    add_local_event(app, &format!("DCC CHAT: accepting from {nick} (id: {id})..."));
    // TCP connection spawning happens in Task 8
}

/// Initiate a new outgoing DCC CHAT (stub — wired in Task 8).
fn initiate_dcc_chat(app: &mut App, nick: &str, passive: bool) {
    if app.dcc.records.len() >= app.dcc.max_connections {
        add_local_event(app, "Maximum DCC connections reached");
        return;
    }
    let mode = if passive { "passive " } else { "" };
    add_local_event(app, &format!("DCC CHAT: initiating {mode}to {nick}..."));
    // TCP connection spawning happens in Task 8
}

// ─── /dcc close ───────────────────────────────────────────────────────────────

fn cmd_dcc_close(app: &mut App, args: &[String]) {
    // Expect: /dcc close chat <nick>
    if args.len() < 2 {
        add_local_event(app, "Usage: /dcc close chat <nick>");
        return;
    }
    if !args[0].eq_ignore_ascii_case("chat") {
        add_local_event(
            app,
            &format!("{}Unknown DCC type: {}{C_RST}", C_ERR, &args[0]),
        );
        return;
    }
    let nick = &args[1];
    match app.dcc.close_by_nick(nick) {
        Some(record) => {
            add_local_event(
                app,
                &format!("{}DCC CHAT with {} closed{C_RST}", C_OK, record.nick),
            );
        }
        None => {
            add_local_event(
                app,
                &format!("{}No DCC CHAT session found for {nick}{C_RST}", C_ERR),
            );
        }
    }
}

// ─── /dcc list ────────────────────────────────────────────────────────────────

fn cmd_dcc_list(app: &mut App) {
    if app.dcc.records.is_empty() {
        add_local_event(app, "No DCC connections");
        return;
    }

    // Collect lines first; we cannot hold a borrow on `app.dcc` while calling
    // `add_local_event(app, ...)` because that requires a mutable borrow.
    let mut lines = vec![divider("DCC Connections")];

    // Sort records by nick for stable display order.
    let mut records: Vec<_> = app.dcc.records.values().collect();
    records.sort_by(|a, b| a.nick.cmp(&b.nick));

    for r in records {
        let state_label = match r.state {
            DccState::WaitingUser => "waiting",
            DccState::Listening => "listening",
            DccState::Connecting => "connecting",
            DccState::Connected => "connected",
        };

        // Duration since connection was established, or since record creation.
        let elapsed_secs = r
            .started
            .map(|t: std::time::Instant| t.elapsed().as_secs())
            .unwrap_or_else(|| r.created.elapsed().as_secs());
        let duration = format_duration(elapsed_secs);

        lines.push(format!(
            "  {C_CMD}{nick}{C_RST}  {C_TEXT}CHAT{C_RST}  \
             {C_DIM}[{state_label}]{C_RST}  {C_DIM}{duration}{C_RST}  \
             {C_DIM}{bytes}B{C_RST}",
            nick = r.nick,
            bytes = r.bytes_transferred,
        ));
    }

    for line in lines {
        add_local_event(app, &line);
    }
}

/// Format a duration in seconds as `Xd Xh Xm Xs` (omitting leading zero units).
fn format_duration(secs: u64) -> String {
    let days = secs / 86400;
    let hours = (secs % 86400) / 3600;
    let mins = (secs % 3600) / 60;
    let s = secs % 60;

    if days > 0 {
        format!("{days}d {hours}h {mins}m {s}s")
    } else if hours > 0 {
        format!("{hours}h {mins}m {s}s")
    } else if mins > 0 {
        format!("{mins}m {s}s")
    } else {
        format!("{s}s")
    }
}

// ─── /dcc reject ──────────────────────────────────────────────────────────────

fn cmd_dcc_reject(app: &mut App, args: &[String]) {
    // Expect: /dcc reject chat <nick>
    if args.len() < 2 {
        add_local_event(app, "Usage: /dcc reject chat <nick>");
        return;
    }
    if !args[0].eq_ignore_ascii_case("chat") {
        add_local_event(
            app,
            &format!("{}Unknown DCC type: {}{C_RST}", C_ERR, &args[0]),
        );
        return;
    }
    let nick = args[1].clone();

    // Remove the record first; even if the IRC send fails the offer is rejected.
    let record = app.dcc.close_by_nick(&nick);
    let nick_str = record.as_ref().map_or(nick.as_str(), |r| r.nick.as_str());

    let reject_ctcp = crate::dcc::protocol::build_dcc_reject();

    // Send the DCC REJECT notice over IRC so the remote client knows we declined.
    if let Some(sender) = app.active_irc_sender() {
        if let Err(e) = sender.send_notice(nick_str, &reject_ctcp) {
            add_local_event(app, &format!("{C_ERR}Failed to send DCC REJECT: {e}{C_RST}"));
        }
    } else {
        add_local_event(app, "Not connected — DCC REJECT not sent");
    }

    add_local_event(
        app,
        &format!("{C_OK}DCC CHAT from {nick_str} rejected{C_RST}"),
    );
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::format_duration;

    #[test]
    fn format_duration_seconds_only() {
        assert_eq!(format_duration(45), "45s");
    }

    #[test]
    fn format_duration_minutes() {
        assert_eq!(format_duration(125), "2m 5s");
    }

    #[test]
    fn format_duration_hours() {
        assert_eq!(format_duration(3661), "1h 1m 1s");
    }

    #[test]
    fn format_duration_days() {
        assert_eq!(format_duration(90061), "1d 1h 1m 1s");
    }

    #[test]
    fn format_duration_zero() {
        assert_eq!(format_duration(0), "0s");
    }
}
