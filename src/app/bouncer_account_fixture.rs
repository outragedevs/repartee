use std::time::Duration;

use super::App;
use crate::state::buffer::make_buffer_id;
use crate::state::connection::ConnectionStatus;

#[derive(serde::Deserialize)]
struct Account {
    user: String,
    networks: Vec<u64>,
}

async fn until(app: &mut App, context: &str, predicate: impl Fn(&App) -> bool + Send + Sync) {
    until_with_reconnect(app, context, predicate, false).await;
}

async fn until_with_reconnect(app: &mut App, context: &str, predicate: impl Fn(&App) -> bool + Send + Sync, reconnect: bool) {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            while let Ok(event) = app.irc_rx.try_recv() { app.handle_irc_event(event); }
            app.tick_bouncer_presence();
            app.tick_bouncer_metadata();
            app.drain_pending_web_events();
            if predicate(app) { return; }
            if reconnect { app.check_reconnects(); }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.unwrap_or_else(|_| panic!("account matrix timed out: {context}; nicks: {:?}",
        app.state.connections.iter().map(|(id, conn)| (id, &conn.nick)).collect::<Vec<_>>()));
}

fn children(app: &App, parent: &str) -> Vec<String> {
    app.bouncer_children.iter().filter(|(_, child)| child.parent == parent)
        .map(|(id, _)| id.clone()).collect()
}

fn verify_rows(app: &App, id: &str, target: &str, expected: usize) {
    let child = &app.bouncer_children[id];
    let prefix = format!("matrix-{}-{}-", child.parent, child.network.id);
    let buffer = &app.state.buffers[&make_buffer_id(id, target)];
    assert_eq!(buffer.messages.len(), expected);
    assert!(buffer.messages.iter().all(|message| message.text.starts_with(&prefix)), "cross-account/network rows in {target}");
}

#[tokio::test]
#[ignore = "requires the pinned two-account bouncer fixture"]
async fn pinned_bouncer_account_matrix() {
    let accounts: Vec<Account> = serde_json::from_str(&std::env::var("REPARTEE_BOUNCER_MATRIX_ACCOUNTS").unwrap()).unwrap();
    assert_eq!(accounts.len(), 2);
    let mut app = super::input::submit_typing_tests::test_app();
    app.config.general.flood_protection = false;
    app.state.scrollback_limit = 1000;
    app.config.display.backlog_lines = 200;
    let (log_tx, mut log_rx) = tokio::sync::mpsc::channel(4096);
    app.state.log_tx = Some(log_tx);
    for (index, account) in accounts.iter().enumerate() {
        let mut config: crate::config::ServerConfig = toml::from_str(
            "label='Same account label'\naddress='127.0.0.1'\nport=1\ntls=false\nchannels=['#must-not-autojoin']\nbouncer_control=true\nreconnect_delay=1",
        ).unwrap();
        config.port = std::env::var("REPARTEE_BOUNCER_TEST_PORT").unwrap().parse().unwrap();
        config.sasl_user = Some(account.user.clone());
        config.sasl_pass = Some("fixture-password".into());
        app.setup_connection(&index.to_string(), &config);
        app.start_connection_attempt(&index.to_string(), config);
    }
    until(&mut app, "four generated networks", |app| {
        app.bouncer_children.len() == 4 && app.bouncer_children.keys().all(|id|
            app.state.connections[id].status == ConnectionStatus::Connected
            && app.history_discovery.get(id).is_some_and(|discovery| discovery.finished)
            && ["history-peer", "#history-channel"].iter().all(|target|
                app.state.buffers.get(&make_buffer_id(id, target)).is_some_and(|buffer| buffer.messages.len() == 200)
                && !app.state.connections[id].chathistory.any_in_flight(target)))
    }).await;
    let mut all = Vec::new();
    for (index, account) in accounts.iter().enumerate() {
        let ids = children(&app, &index.to_string());
        assert_eq!(ids.len(), 2);
        for id in &ids {
            assert!(account.networks.iter().any(|network| network.to_string() == app.bouncer_children[id].network.id));
            assert!(app.state.connections[id].origin_config.channels.is_empty());
        }
        all.extend(ids);
    }
    let scopes: std::collections::HashSet<_> = all.iter().map(|id| app.state.connections[id].network_key()).collect();
    assert_eq!(scopes.len(), 4);
    for id in &all {
        verify_rows(&app, id, "history-peer", 200);
        verify_rows(&app, id, "#history-channel", 200);
        assert!(app.fetch_older_via_chathistory(&make_buffer_id(id, "history-peer")));
        app.state.set_active_buffer(&make_buffer_id(id, "history-peer"));
        super::server_search::command(&mut app, &["between".into(), "#history-channel".into(), "2024-01-01T00:00:10Z".into(),
            "2024-01-01T00:00:20Z".into(), "3".into()]);
    }
    assert_eq!(app.server_search.len(), 4);
    until(&mut app, "concurrent BEFORE and BETWEEN", |app| {
        app.server_search.is_empty() && all.iter().all(|id|
            app.state.buffers[&make_buffer_id(id, "history-peer")].messages.len() == 300
            && !app.state.connections[id].chathistory.any_in_flight("history-peer"))
    }).await;
    for id in &all {
        verify_rows(&app, id, "history-peer", 300);
        verify_rows(&app, id, "#history-channel", 200);
        let child = &app.bouncer_children[id];
        let rows: Vec<_> = app.state.buffers[&app.server_search_views[id]].messages.iter()
            .filter(|message| message.text.starts_with("matrix-")).map(|message| message.text.clone()).collect();
        assert_eq!(rows, (11..14).map(|index| format!("matrix-{}-{}-{index}", child.parent, child.network.id)).collect::<Vec<_>>());
    }
    verify_read_isolation(&mut app, &all).await;
    verify_live_isolation(&mut app, &all).await;
    let untouched: Vec<_> = children(&app, "1").into_iter()
        .map(|id| (app.conn_generations[&id], app.state.connections[&id].network_key().to_string(), id)).collect();
    app.irc_handles["0"].sender().send("QUIT :fixture reconnect".parse::<irc::proto::Message>().unwrap()).unwrap();
    until(&mut app, "first account disconnect", |app| app.state.connections["0"].status == ConnectionStatus::Disconnected).await;
    let config = app.state.connections["0"].origin_config.clone();
    app.start_connection_attempt("0", config);
    until(&mut app, "first account reconnect", |app| {
        app.state.connections["0"].status == ConnectionStatus::Connected
            && children(app, "0").iter().all(|id| app.state.connections[id].status == ConnectionStatus::Connected)
    }).await;
    for (generation, scope, id) in untouched {
        assert_eq!(app.conn_generations[&id], generation);
        assert_eq!(app.state.connections[&id].network_key(), scope);
        verify_rows(&app, &id, "history-peer", 300);
    }
    verify_network_lifecycle(&mut app, &accounts).await;
    verify_provider_restart(&mut app).await;
    verify_restored_traffic(&mut app).await;
    if let Ok(script) = std::env::var("REPARTEE_MATRIX_BROWSER_SCRIPT") {
        app.web_broadcaster = std::sync::Arc::new(crate::web::broadcast::WebBroadcaster::new(128));
        super::filehost_browser_fixture::run(&mut app, &script).await;
    }
    while let Ok(row) = log_rx.try_recv() {
        assert!(!["history-peer", "#history-channel", "*search*"].contains(&row.buffer.as_str()), "unexpected persistent row: {row:?}");
        assert!(!row.text.contains("matrix-"));
    }
    for parent in ["0", "1"] {
        app.suspend_bouncer_children(parent);
        app.cancel_connection_attempt(parent);
        app.irc_handles.remove(parent);
    }
}

async fn verify_live_isolation(app: &mut App, all: &[String]) {
    let port: u16 = std::env::var("REPARTEE_MATRIX_UPSTREAM_PORT").unwrap().parse().unwrap();
    let soju = std::env::var("REPARTEE_MATRIX_PROVIDER").unwrap() == "soju";
    let path = std::path::PathBuf::from(std::env::var("REPARTEE_MATRIX_UPSTREAM_EVENTS").unwrap());
    for id in all {
        let child = &app.bouncer_children[id];
        let request = serde_json::json!({"action":"online", "account":child.parent.parse::<usize>().unwrap(),
            "network":child.network.id.parse::<u64>().unwrap(), "name":child.network.name(), "port":port,
            "nick":format!("matrix-{}-{}", child.parent, child.network.id)});
        control(app, &request).await;
        if !soju && let Some(handle) = app.irc_handles.get(id) {
            handle.sender().send(irc::proto::Command::Raw("VERSION".into(), vec![])).unwrap();
        }
    }
    until_with_reconnect(app, "four real upstream registrations", |app| all.iter().all(|id| {
        let child = &app.bouncer_children[id];
        app.state.connections[id].nick == format!("matrix-{}-{}", child.parent, child.network.id)
    }), true).await;
    for id in all {
        let text = format!("matrix-live-{id}");
        app.irc_handles[id].sender().send(irc::proto::Command::PRIVMSG("Alice".into(), text.clone())).unwrap();
        until(app, "own live echo", |app| app.state.buffers.get(&make_buffer_id(id, "Alice"))
            .is_some_and(|buffer| buffer.messages.iter().any(|row| row.text == text))).await;
        assert_eq!(app.state.buffers[&make_buffer_id(id, "Alice")].name, "Alice");
        for other in all {
            if other != id {
                assert!(app.state.buffers.values().filter(|buffer| buffer.connection_id == *other)
                    .all(|buffer| buffer.messages.iter().all(|row| row.text != text)));
            }
        }
    }
    verify_live_presence(app, all, &path, soju).await;
    let selected = &all[0];
    if soju {
        app.state.set_active_buffer(&make_buffer_id(selected, "Alice"));
        app.handle_submit("/bmeta Alice pin on");
        until(app, "selected network metadata", |app| app.state.metadata_flags(selected, "Alice").pinned).await;
        for other in all {
            if other != selected { assert!(!app.state.metadata_flags(other, "Alice").pinned); }
        }
    } else {
        assert!(all.iter().all(|id| !app.state.connections[id].enabled_caps.contains(crate::irc::metadata::CAP)));
    }
    let child = &app.bouncer_children[selected];
    let generation = app.conn_generations[selected];
    let request = serde_json::json!({"account":child.parent.parse::<usize>().unwrap(),
        "network":child.network.id.parse::<u64>().unwrap(), "name":child.network.name(), "port":port,
        "nick":format!("matrix-{}-{}", child.parent, child.network.id)});
    let mut offline = request.clone();
    offline["action"] = "offline".into();
    control(app, &offline).await;
    let mut online = request;
    online["action"] = "online".into();
    control(app, &online).await;
    if !soju && let Some(handle) = app.irc_handles.get(selected) {
        handle.sender().send(irc::proto::Command::Raw("VERSION".into(), vec![])).unwrap();
    }
    until(app, "selected upstream returned", |_| {
        std::fs::read_to_string(&path).unwrap_or_default().lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .filter(|row| row["nick"] == online["nick"] && row.get("away").is_some())
            .map(|row| row["connection"].as_u64().unwrap()).collect::<std::collections::HashSet<_>>().len() >= 2
    }).await;
    until_with_reconnect(app, "selected downstream reattached", |app| app.state.connections[selected].status == ConnectionStatus::Connected
        && (soju || app.conn_generations[selected] > generation)
        && app.irc_handles.contains_key(selected), true).await;
    app.irc_handles[selected].sender().send(irc::proto::Command::PRIVMSG("Alice".into(), "matrix-after-upstream-reconnect".into())).unwrap();
    until(app, "traffic after upstream reconnect", |app| app.state.buffers[&make_buffer_id(selected, "Alice")]
        .messages.iter().any(|row| row.text == "matrix-after-upstream-reconnect")).await;
    let rows: Vec<serde_json::Value> = std::fs::read_to_string(path).unwrap().lines()
        .map(|line| serde_json::from_str(line).unwrap()).collect();
    assert!(rows.iter().all(|row| row.get("join").is_none()), "unexpected upstream autojoin");
    for id in all {
        let child = &app.bouncer_children[id];
        let nick = format!("matrix-{}-{}", child.parent, child.network.id);
        assert_eq!(rows.iter().filter(|row| row["outgoing"] == format!("matrix-live-{id}") && row["nick"] == nick).count(), 1);
    }
}

async fn verify_read_isolation(app: &mut App, all: &[String]) {
    let id = children(app, "0")[0].clone();
    let config = app.state.connections[&id].origin_config.clone();
    let (observer, mut observer_events) = crate::irc::connect_server("observer", &config, &app.config.general).await.unwrap();
    let message = &app.state.buffers[&make_buffer_id(&id, "history-peer")].messages[149];
    let (message_id, timestamp) = (message.id, message.timestamp.timestamp_millis());
    app.mark_visible_message_read(&make_buffer_id(&id, "history-peer"), message_id);
    until(app, "own read confirmation", |app| app.confirmed_read_marker(&id, "history-peer") == Some(timestamp)).await;
    tokio::time::timeout(Duration::from_secs(10), async {
        let stamp = format!("timestamp={}", crate::irc::chathistory::rfc3339_millis(timestamp));
        loop {
            let event = observer_events.recv().await.expect("observer disconnected");
            if let crate::irc::IrcEvent::Message(_, message) = event
                && let irc::proto::Command::Raw(command, args) = &message.command
                && command == "MARKREAD" && args == &["history-peer".to_string(), stamp.clone()] { break; }
        }
    }).await.expect("independent client missed read marker");
    for other in all {
        if *other != id {
            assert_ne!(app.confirmed_read_marker(other, "history-peer"), Some(timestamp));
            assert_eq!(app.state.buffers[&make_buffer_id(other, "history-peer")].unread_count, 300);
        }
    }
    drop(observer);
}

async fn control(app: &mut App, request: &serde_json::Value) -> serde_json::Value {
    let path = std::path::PathBuf::from(std::env::var("REPARTEE_BOUNCER_MATRIX_CONTROL").unwrap());
    let response = path.with_extension("response");
    if response.exists() { std::fs::remove_file(&response).unwrap(); }
    let pending = path.with_extension("pending");
    std::fs::write(&pending, serde_json::to_vec(request).unwrap()).unwrap();
    std::fs::rename(pending, &path).unwrap();
    until(app, "provider mutation response", |_| response.exists()).await;
    let result: serde_json::Value = serde_json::from_slice(&std::fs::read(response).unwrap()).unwrap();
    assert!(result.get("error").is_none(), "mutation failed: {result}");
    result
}

async fn verify_network_lifecycle(app: &mut App, accounts: &[Account]) {
    let network = accounts[0].networks[0].to_string();
    let id = children(app, "0").into_iter().find(|id| app.bouncer_children[id].network.id == network).unwrap();
    let name = app.bouncer_children[&id].network.name().to_string();
    let old_scope = app.state.connections[&id].network_key().to_string();
    let generation = app.conn_generations[&id];
    let other: Vec<_> = app.bouncer_children.keys().filter(|other| **other != id)
        .map(|id| (id.clone(), app.conn_generations[id], app.state.connections[id].network_key().to_string())).collect();
    control(app, &serde_json::json!({"action":"rename", "account":0, "network":accounts[0].networks[0], "name":name})).await;
    until(app, "network rename", |app| app.bouncer_children[&id].network.name() == "renamed").await;
    assert_eq!(app.conn_generations[&id], generation);
    assert_eq!(app.state.connections[&id].network_key(), old_scope);
    assert!(app.state.connections[&id].label.starts_with("renamed ["));
    control(app, &serde_json::json!({"action":"delete", "account":0, "network":accounts[0].networks[0]})).await;
    until(app, "network deletion", |app| !app.bouncer_children.contains_key(&id)).await;
    assert!(!app.state.connections.contains_key(&id));
    assert!(app.state.buffers.values().all(|buffer| buffer.connection_id != id));
    let port: u16 = std::env::var("REPARTEE_MATRIX_UPSTREAM_PORT").unwrap().parse().unwrap();
    let result = control(app, &serde_json::json!({"action":"create", "account":0, "port":port})).await;
    let new_network = result["network"].as_u64().unwrap().to_string();
    assert_ne!(new_network, network);
    until(app, "network recreation", |app| app.bouncer_children.iter().any(|(id, child)|
        child.parent == "0" && child.network.id == new_network
            && app.state.connections[id].status == ConnectionStatus::Connected
            && app.history_discovery.get(id).is_some_and(|discovery| discovery.finished))).await;
    let new_id = children(app, "0").into_iter().find(|id| app.bouncer_children[id].network.id == new_network).unwrap();
    assert_ne!(app.state.connections[&new_id].network_key(), old_scope);
    let deleted_prefix = format!("matrix-0-{network}-");
    assert!(app.state.buffers.values().filter(|buffer| buffer.connection_id == new_id)
        .all(|buffer| buffer.messages.iter().all(|message| !message.text.starts_with(&deleted_prefix))));
    for (id, generation, scope) in other {
        assert_eq!(app.conn_generations[&id], generation);
        assert_eq!(app.state.connections[&id].network_key(), scope);
    }
}

async fn verify_provider_restart(app: &mut App) {
    let scopes: Vec<_> = app.bouncer_children.keys().map(|id|
        (id.clone(), app.conn_generations[id], app.state.connections[id].network_key().to_string())).collect();
    let path = std::path::PathBuf::from(std::env::var("REPARTEE_BOUNCER_MATRIX_CONTROL").unwrap());
    std::fs::write(path.with_extension("restart"), "restart").unwrap();
    until(app, "provider process restart", |_| path.with_extension("restarted").exists()).await;
    let response: serde_json::Value = serde_json::from_slice(&std::fs::read(path.with_extension("restarted")).unwrap()).unwrap();
    assert_eq!(response["ok"], true, "restart failed: {response}");
    until(app, "both accounts disconnected", |app| ["0", "1"].iter().all(|id|
        app.state.connections[*id].status == ConnectionStatus::Disconnected)).await;
    for parent in ["0", "1"] {
        let config = app.state.connections[parent].origin_config.clone();
        app.start_connection_attempt(parent, config);
    }
    until(app, "all networks restored after provider restart", |app| scopes.iter().all(|(id, old, _)|
        app.conn_generations.get(id).is_some_and(|generation| generation > old)
        && app.state.connections[id].status == ConnectionStatus::Connected
        && app.history_discovery.get(id).is_some_and(|discovery| discovery.finished))).await;
    assert_eq!(app.bouncer_children.len(), 4);
    for (id, _, scope) in scopes {
        assert_eq!(app.state.connections[&id].network_key(), scope);
        let prefix = format!("matrix-{}-{}-", app.bouncer_children[&id].parent, app.bouncer_children[&id].network.id);
        for target in ["history-peer", "#history-channel"] {
            if let Some(buffer) = app.state.buffers.get(&make_buffer_id(&id, target)) {
                assert!(buffer.messages.iter().all(|message| message.text.starts_with(&prefix)));
                let unique: std::collections::HashSet<_> = buffer.messages.iter().map(|message| &message.text).collect();
                assert_eq!(unique.len(), buffer.messages.len());
            }
        }
    }
}

async fn verify_restored_traffic(app: &mut App) {
    let restored: Vec<_> = app.bouncer_children.keys().cloned().collect();
    for id in &restored {
        let text = format!("matrix-incoming-after-restart-{id}");
        app.irc_handles[id].sender().send(irc::proto::Command::PRIVMSG("FixtureControl".into(), text.clone())).unwrap();
        until(app, "incoming traffic after provider restart", |app| app.state.buffers.get(&make_buffer_id(id, "Alice"))
            .is_some_and(|buffer| buffer.messages.iter().any(|row| row.text == text))).await;
        for other in &restored {
            if other != id {
                assert!(app.state.buffers.values().filter(|buffer| buffer.connection_id == *other)
                    .all(|buffer| buffer.messages.iter().all(|row| row.text != text)));
            }
        }
    }
    let events = std::fs::read_to_string(std::env::var("REPARTEE_MATRIX_UPSTREAM_EVENTS").unwrap()).unwrap();
    assert!(events.lines().map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .all(|row| row.get("join").is_none()), "unexpected upstream autojoin after provider restart");
}

async fn verify_live_presence(app: &mut App, all: &[String], path: &std::path::Path, soju: bool) {
    let selected = &all[0];
    app.handle_web_command(crate::web::protocol::WebCommand::WebConnect { initial_buffer_id: None }, "matrix-browser");
    app.handle_web_command(crate::web::protocol::WebCommand::Presence { present: true }, "matrix-browser");
    for id in all { assert!(app.set_bouncer_away(id, None)); }
    until(app, "all upstreams present", |app| {
        let rows: Vec<serde_json::Value> = std::fs::read_to_string(path).unwrap_or_default().lines()
            .filter_map(|line| serde_json::from_str(line).ok()).collect();
        all.iter().all(|id| {
            let child = &app.bouncer_children[id];
            let nick = format!("matrix-{}-{}", child.parent, child.network.id);
            rows.iter().rev().find(|row| row["nick"] == nick && row.get("away").is_some())
                .is_some_and(|row| row["away"].is_null())
        })
    }).await;
    assert!(app.set_bouncer_away(selected, Some("matrix-manual-away")));
    until(app, "manual away isolation", |app| {
        let rows: Vec<serde_json::Value> = std::fs::read_to_string(path).unwrap_or_default().lines()
            .filter_map(|line| serde_json::from_str(line).ok()).collect();
        all.iter().all(|id| {
            let child = &app.bouncer_children[id];
            let nick = format!("matrix-{}-{}", child.parent, child.network.id);
            let expected = id == selected || (!soju && child.parent == app.bouncer_children[selected].parent);
            rows.iter().rev().find(|row| row["nick"] == nick && row.get("away").is_some())
                .is_some_and(|row| row["away"].is_null() != expected)
        })
    }).await;
    assert!(app.set_bouncer_away(selected, None));
}
