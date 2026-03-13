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

/// Accept a pending DCC CHAT request.
fn accept_dcc_chat(app: &mut App, nick: &str, id: &str) {
    let Some(record) = app.dcc.records.get(id).cloned() else {
        add_local_event(app, &format!("{C_ERR}No pending DCC CHAT for {nick}{C_RST}"));
        return;
    };

    // Passive DCC (port == 0, has token): we become the listener and the
    // remote peer connects to us once we reply with our address + token.
    if record.port == 0 && record.passive_token.is_some() {
        let own_ip = resolve_own_ip(app);
        let bind_port = pick_bind_port(app.dcc.port_range);
        let bind_addr = std::net::SocketAddr::new(
            own_ip.unwrap_or(std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED)),
            bind_port,
        );

        // Bind listener synchronously so we can extract the actual port.
        let listener = match std::net::TcpListener::bind(bind_addr) {
            Ok(l) => l,
            Err(e) => {
                add_local_event(
                    app,
                    &format!("{C_ERR}DCC CHAT bind error: {e}{C_RST}"),
                );
                return;
            }
        };
        let local_port = match listener.local_addr() {
            Ok(a) => a.port(),
            Err(e) => {
                add_local_event(
                    app,
                    &format!("{C_ERR}DCC CHAT local_addr error: {e}{C_RST}"),
                );
                return;
            }
        };
        listener.set_nonblocking(true).ok();
        let tokio_listener = match tokio::net::TcpListener::from_std(listener) {
            Ok(l) => l,
            Err(e) => {
                add_local_event(
                    app,
                    &format!("{C_ERR}DCC CHAT async listener error: {e}{C_RST}"),
                );
                return;
            }
        };

        // Update record state
        if let Some(rec) = app.dcc.records.get_mut(id) {
            rec.state = crate::dcc::types::DccState::Listening;
        }

        // Send CTCP response with our address + token
        let advertise_ip = own_ip.unwrap_or(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST));
        let token = record.passive_token;
        let ctcp = crate::dcc::protocol::build_dcc_chat_ctcp(&advertise_ip, local_port, token);
        if let Some(sender) = app.active_irc_sender()
            && let Err(e) = sender.send_privmsg(nick, &ctcp)
        {
            add_local_event(app, &format!("{C_ERR}Failed to send DCC response: {e}{C_RST}"));
            return;
        }

        // Create line sender channel and spawn listener task
        let (line_tx, line_rx) = tokio::sync::mpsc::unbounded_channel();
        app.dcc.chat_senders.insert(id.to_string(), line_tx);

        let task_id = id.to_string();
        let event_tx = app.dcc.dcc_tx.clone();
        let timeout_dur = std::time::Duration::from_secs(app.dcc.timeout_secs);
        tokio::spawn(async move {
            crate::dcc::chat::listen_for_chat(task_id, tokio_listener, timeout_dur, event_tx, line_rx)
                .await;
        });

        add_local_event(
            app,
            &format!("DCC CHAT: listening on port {local_port} for {nick} (passive)..."),
        );
    } else {
        // Active DCC: connect to the remote peer's address + port.
        if let Some(rec) = app.dcc.records.get_mut(id) {
            rec.state = crate::dcc::types::DccState::Connecting;
        }

        let (line_tx, line_rx) = tokio::sync::mpsc::unbounded_channel();
        app.dcc.chat_senders.insert(id.to_string(), line_tx);

        let task_id = id.to_string();
        let event_tx = app.dcc.dcc_tx.clone();
        let timeout_dur = std::time::Duration::from_secs(app.dcc.timeout_secs);
        let addr = std::net::SocketAddr::new(record.addr, record.port);
        tokio::spawn(async move {
            crate::dcc::chat::connect_for_chat(task_id, addr, timeout_dur, event_tx, line_rx)
                .await;
        });

        add_local_event(
            app,
            &format!("DCC CHAT: connecting to {nick} ({}:{})...", record.addr, record.port),
        );
    }
}

/// Initiate a new outgoing DCC CHAT.
#[allow(clippy::too_many_lines)]
fn initiate_dcc_chat(app: &mut App, nick: &str, passive: bool) {
    if app.dcc.records.len() >= app.dcc.max_connections {
        add_local_event(app, "Maximum DCC connections reached");
        return;
    }

    let Some(conn_id) = app.active_conn_id().map(str::to_owned) else {
        add_local_event(app, "No active connection");
        return;
    };

    if passive {
        // Passive/reverse DCC: send CTCP with fake IP + port 0 + token.
        // The remote peer will set up a listener and reply with their address.
        let token: u32 = rand::random::<u32>() % 64;
        let id = app.dcc.generate_id(nick);
        let record = crate::dcc::types::DccRecord {
            id: id.clone(),
            dcc_type: crate::dcc::types::DccType::Chat,
            nick: nick.to_string(),
            conn_id,
            addr: crate::dcc::protocol::PASSIVE_FAKE_IP,
            port: 0,
            state: crate::dcc::types::DccState::WaitingUser,
            passive_token: Some(token),
            created: std::time::Instant::now(),
            started: None,
            bytes_transferred: 0,
            mirc_ctcp: true,
            ident: String::new(),
            host: String::new(),
        };
        app.dcc.records.insert(id, record);

        let ctcp =
            crate::dcc::protocol::build_dcc_chat_ctcp(&crate::dcc::protocol::PASSIVE_FAKE_IP, 0, Some(token));
        if let Some(sender) = app.active_irc_sender()
            && let Err(e) = sender.send_privmsg(nick, &ctcp)
        {
            add_local_event(app, &format!("{C_ERR}Failed to send DCC offer: {e}{C_RST}"));
            return;
        }
        add_local_event(
            app,
            &format!("DCC CHAT: sent passive offer to {nick} (token {token})"),
        );
    } else {
        // Active DCC: bind a listener, send CTCP with our IP + port.
        let own_ip = resolve_own_ip(app);
        let bind_port = pick_bind_port(app.dcc.port_range);
        let bind_addr = std::net::SocketAddr::new(
            own_ip.unwrap_or(std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED)),
            bind_port,
        );

        let listener = match std::net::TcpListener::bind(bind_addr) {
            Ok(l) => l,
            Err(e) => {
                add_local_event(
                    app,
                    &format!("{C_ERR}DCC CHAT bind error: {e}{C_RST}"),
                );
                return;
            }
        };
        let local_port = match listener.local_addr() {
            Ok(a) => a.port(),
            Err(e) => {
                add_local_event(
                    app,
                    &format!("{C_ERR}DCC CHAT local_addr error: {e}{C_RST}"),
                );
                return;
            }
        };
        listener.set_nonblocking(true).ok();
        let tokio_listener = match tokio::net::TcpListener::from_std(listener) {
            Ok(l) => l,
            Err(e) => {
                add_local_event(
                    app,
                    &format!("{C_ERR}DCC CHAT async listener error: {e}{C_RST}"),
                );
                return;
            }
        };

        let id = app.dcc.generate_id(nick);
        let advertise_ip = own_ip.unwrap_or(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST));
        let record = crate::dcc::types::DccRecord {
            id: id.clone(),
            dcc_type: crate::dcc::types::DccType::Chat,
            nick: nick.to_string(),
            conn_id,
            addr: advertise_ip,
            port: local_port,
            state: crate::dcc::types::DccState::Listening,
            passive_token: None,
            created: std::time::Instant::now(),
            started: None,
            bytes_transferred: 0,
            mirc_ctcp: true,
            ident: String::new(),
            host: String::new(),
        };
        app.dcc.records.insert(id.clone(), record);

        // Send CTCP DCC CHAT offer
        let ctcp = crate::dcc::protocol::build_dcc_chat_ctcp(&advertise_ip, local_port, None);
        if let Some(sender) = app.active_irc_sender()
            && let Err(e) = sender.send_privmsg(nick, &ctcp)
        {
            add_local_event(app, &format!("{C_ERR}Failed to send DCC offer: {e}{C_RST}"));
            return;
        }

        // Create line sender channel and spawn listener task
        let (line_tx, line_rx) = tokio::sync::mpsc::unbounded_channel();
        app.dcc.chat_senders.insert(id.clone(), line_tx);

        let event_tx = app.dcc.dcc_tx.clone();
        let timeout_dur = std::time::Duration::from_secs(app.dcc.timeout_secs);
        tokio::spawn(async move {
            crate::dcc::chat::listen_for_chat(id, tokio_listener, timeout_dur, event_tx, line_rx)
                .await;
        });

        add_local_event(
            app,
            &format!("DCC CHAT: listening on port {local_port} for {nick}..."),
        );
    }
}

/// Resolve the IP address to advertise in DCC offers.
///
/// Priority: config override > 127.0.0.1 fallback.
/// Logs an actionable warning when no explicit IP is set, since localhost
/// only works for same-machine connections.
fn resolve_own_ip(app: &App) -> Option<std::net::IpAddr> {
    if let Some(ip) = app.dcc.own_ip {
        return Some(ip);
    }
    tracing::warn!(
        "DCC: no own_ip configured — using 127.0.0.1 which only works locally. \
         Set dcc.own_ip to your public IP for remote DCC connections: /set dcc.own_ip <ip>"
    );
    None
}

/// Pick a port to bind for DCC listening.
///
/// Returns 0 for (0,0) (OS-assigned), the single port for (N,N),
/// or a random port within the range.
fn pick_bind_port(range: (u16, u16)) -> u16 {
    match range {
        (0, 0) => 0,
        (lo, hi) if lo == hi => lo,
        (lo, hi) => {
            use rand::RngExt;
            rand::rng().random_range(lo..=hi)
        }
    }
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
                &format!("{C_ERR}No DCC CHAT session found for {nick}{C_RST}"),
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
            .map_or_else(|| r.created.elapsed().as_secs(), |t: std::time::Instant| t.elapsed().as_secs());
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
