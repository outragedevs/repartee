use super::*;
use crate::app::App;

async fn until(app: &mut App, predicate: impl Fn(&App) -> bool + Send + Sync) {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            while let Ok(event) = app.irc_rx.try_recv() { app.handle_irc_event(event); }
            app.tick_bouncer_webpush();
            if predicate(app) { return; }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }).await.expect("WebPush fixture timed out");
}

async fn connect(config: &crate::config::ServerConfig) -> App {
    let mut app = crate::app::input::submit_typing_tests::test_app();
    app.config.general.flood_protection = false;
    app.setup_connection("fixture", config);
    app.start_connection_attempt("fixture", config.clone());
    until(&mut app, |app| app.webpush_configuration("fixture").is_some()).await;
    app
}

async fn request(app: &mut App, action: Action, expected: Status) {
    let mut receiver = app.web_broadcaster.subscribe();
    let request = WebRequest { connection_id: "fixture".into(), request_id: uuid::Uuid::new_v4().to_string(), action };
    app.handle_web_command(crate::web::protocol::WebCommand::WebPush(Box::new(request.clone())), "fixture-browser");
    assert!(app.bouncer_webpush.contains_key("fixture"));
    until(app, |app| !app.bouncer_webpush.contains_key("fixture")).await;
    let responses: Vec<_> = std::iter::from_fn(|| receiver.try_recv().ok()).filter_map(|event| match event {
        WebEvent::WebPush { request_id, status, session_id, .. } if request_id == request.request_id => {
            assert_eq!(session_id, "fixture-browser"); Some(status)
        }
        _ => None,
    }).collect();
    assert_eq!(responses, [expected]);
}

fn count(root: &std::path::Path) -> i64 {
    let db = rusqlite::Connection::open_with_flags(root.join("main.db"), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    db.query_row("SELECT COUNT(*) FROM WebPushSubscription", [], |row| row.get(0)).unwrap()
}

#[tokio::test]
#[ignore = "requires pinned disposable Soju with encrypted HTTPS push receiver"]
async fn pinned_bouncer_webpush() {
    let root = std::path::PathBuf::from(std::env::var("REPARTEE_WEBPUSH_FIXTURE").unwrap());
    let subscription: webpush::Subscription = serde_json::from_slice(&std::fs::read(root.join("subscription.json")).unwrap()).unwrap();
    let mut config: crate::config::ServerConfig = toml::from_str("label='fixture'\naddress='127.0.0.1'\nport=1\ntls=true\nchannels=[]\nbouncer_network_id='1'\nsasl_mechanism='PLAIN'").unwrap();
    config.port = std::env::var("REPARTEE_BOUNCER_TEST_PORT").unwrap().parse().unwrap();
    config.sasl_user = Some("fixture".into());
    config.sasl_pass = Some("fixture-password".into());
    let mut app = connect(&config).await;
    let (scope, vapid) = app.webpush_configuration("fixture").unwrap();
    std::fs::write(root.join("expected-vapid"), &vapid).unwrap();
    let (logs, mut logged) = tokio::sync::mpsc::channel(64);
    app.state.log_tx = Some(logs);
    request(&mut app, Action::Register { scope: scope.clone(), vapid: vapid.clone(), subscription: subscription.clone() }, Status::Registered).await;
    assert_eq!(count(&root), 1);
    let payloads = std::fs::read_to_string(root.join("received.jsonl")).unwrap();
    let first: serde_json::Value = serde_json::from_str(payloads.lines().next().unwrap()).unwrap();
    assert_eq!(first["verified_vapid"], true);
    assert!(first["payload"].as_str().unwrap().contains("NOTE WEBPUSH REGISTERED"));
    request(&mut app, Action::Register { scope: scope.clone(), vapid: vapid.clone(), subscription: subscription.clone() }, Status::Registered).await;
    assert_eq!(count(&root), 1);
    assert_eq!(std::fs::read_to_string(root.join("received.jsonl")).unwrap().lines().count(), 1);
    app.irc_handles["fixture"].sender().send(Command::PRIVMSG("FixtureControl".into(), "search-private".into())).unwrap();
    until(&mut app, |_| std::fs::read_to_string(root.join("received.jsonl")).unwrap().lines().any(|line| {
        let value: serde_json::Value = serde_json::from_str(line).unwrap();
        value["verified_vapid"] == true && value["payload"].as_str().is_some_and(|text| text.contains("private needle incoming"))
    })).await;
    assert!(logged.try_recv().is_err());
    app.cancel_connection_attempt("fixture");
    app.irc_handles.remove("fixture");
    let mut app = connect(&config).await;
    assert_eq!(app.webpush_configuration("fixture"), Some((scope.clone(), vapid.clone())));
    assert_eq!(count(&root), 1);
    request(&mut app, Action::Unregister { scope: scope.clone(), endpoint: subscription.endpoint.clone() }, Status::Unregistered).await;
    assert_eq!(count(&root), 0);
    request(&mut app, Action::Unregister { scope: scope.clone(), endpoint: subscription.endpoint.clone() }, Status::Unregistered).await;
    let mut missing = subscription.clone();
    missing.endpoint = missing.endpoint.replace("/subscription", "/expired");
    request(&mut app, Action::Register { scope, vapid, subscription: missing }, Status::Failed).await;
    assert_eq!(count(&root), 0);
    app.cancel_connection_attempt("fixture");
    app.irc_handles.remove("fixture");
}
