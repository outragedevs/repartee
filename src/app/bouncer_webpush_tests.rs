use super::*;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use p256::elliptic_curve::sec1::ToEncodedPoint as _;
use crate::irc::{IrcEvent, IrcHandle, IrcSender};
use crate::web::protocol::WebCommand;

fn key() -> String {
    URL_SAFE_NO_PAD.encode(p256::SecretKey::from_slice(&[1; 32]).unwrap().public_key().to_encoded_point(false).as_bytes())
}

fn app() -> super::super::App {
    let mut app = crate::app::input::submit_typing_tests::test_app();
    let config = toml::from_str("label='fixture'\naddress='localhost'\nport=6697\ntls=true\nchannels=[]\nbouncer_network_id='1'").unwrap();
    app.setup_connection("test", &config);
    let conn = app.state.connections.get_mut("test").unwrap();
    conn.status = ConnectionStatus::Connected;
    conn.enabled_caps.insert(webpush::CAP.into());
    conn.isupport_parsed.parse_tokens(&[&format!("VAPID={}", key())]);
    let mut handle = IrcHandle::new("test".into(), IrcSender::capturing(0), None, None);
    handle.sasl_authenticated = true;
    app.irc_handles.insert("test".into(), handle);
    app
}

fn unregister(app: &super::super::App) -> WebRequest {
    WebRequest { connection_id: "test".into(), request_id: uuid::Uuid::new_v4().to_string(),
        action: Action::Unregister { scope: app.webpush_configuration("test").unwrap().0, endpoint: "https://push.test/disposable-endpoint".into() } }
}

fn receive(app: &mut super::super::App, wire: &str) {
    app.handle_irc_event(IrcEvent::Message("test".into(), Box::new(wire.parse().unwrap())));
}

fn finish(app: &mut super::super::App) {
    let nonce = app.bouncer_webpush["test"].nonce.clone();
    receive(app, &format!(":bouncer PONG :{nonce}"));
}

#[tokio::test]
async fn webpush_mutations_finish_only_after_matching_ack_and_barrier() {
    let mut app = app();
    let request = unregister(&app);
    let mut web = app.web_broadcaster.subscribe();
    app.handle_web_command(WebCommand::WebPush(Box::new(request.clone())), "one");
    receive(&mut app, "WEBPUSH UNREGISTER https://push.test/disposable-endpoint");
    assert!(web.try_recv().is_err());
    finish(&mut app);
    assert!(matches!(web.try_recv().unwrap(), WebEvent::WebPush { status: Status::Unregistered, session_id, request_id, .. }
        if session_id == "one" && request_id == request.request_id));
    assert!(app.state.buffers.values().all(|buffer| buffer.messages.iter().all(|row| !row.text.contains("disposable-endpoint"))));
}

#[tokio::test]
async fn webpush_timeout_blocks_other_sessions_and_late_reply_can_resolve() {
    let mut app = app();
    let request = unregister(&app);
    let mut web = app.web_broadcaster.subscribe();
    app.handle_webpush_request(&request, "one");
    app.bouncer_webpush.get_mut("test").unwrap().started = Instant::now().checked_sub(Duration::from_secs(31)).unwrap();
    app.tick_bouncer_webpush();
    assert!(matches!(web.try_recv().unwrap(), WebEvent::WebPush { status: Status::Unknown, .. }));
    let second = unregister(&app);
    app.handle_webpush_request(&second, "two");
    assert!(matches!(web.try_recv().unwrap(), WebEvent::WebPush { status: Status::Busy, session_id, .. } if session_id == "two"));
    assert_eq!(app.irc_handles["test"].sender().captured().len(), 2);
    receive(&mut app, "WEBPUSH UNREGISTER https://push.test/disposable-endpoint");
    finish(&mut app);
    assert!(matches!(web.try_recv().unwrap(), WebEvent::WebPush { status: Status::Unregistered, session_id, .. } if session_id == "one"));
}

#[tokio::test]
async fn webpush_configuration_changes_and_failures_never_claim_success() {
    for change in ["scope", "vapid", "cap", "failure", "wrong-endpoint"] {
        let mut app = app();
        let request = unregister(&app);
        let mut web = app.web_broadcaster.subscribe();
        app.handle_webpush_request(&request, "one");
        receive(&mut app, "WEBPUSH UNREGISTER https://push.test/disposable-endpoint");
        match change {
            "scope" => app.state.connections.get_mut("test").unwrap().network_scope = Some("other-account".into()),
            "vapid" => { app.state.connections.get_mut("test").unwrap().isupport_parsed.parse_tokens(&["-VAPID"]); }
            "cap" => { app.state.connections.get_mut("test").unwrap().enabled_caps.remove(webpush::CAP); }
            "failure" => receive(&mut app, "FAIL WEBPUSH INTERNAL_ERROR UNREGISTER :private endpoint must not be displayed"),
            _ => receive(&mut app, "WEBPUSH UNREGISTER https://push.test/wrong"),
        }
        finish(&mut app);
        assert!(matches!(web.try_recv().unwrap(), WebEvent::WebPush { status: Status::Failed, .. }));
    }
}

#[tokio::test]
async fn webpush_rejects_untrusted_transport_auth_and_stale_configuration() {
    for change in ["tls", "verify", "auth", "scope"] {
        let mut app = app();
        let request = unregister(&app);
        let mut web = app.web_broadcaster.subscribe();
        match change {
            "tls" => app.state.connections.get_mut("test").unwrap().origin_config.tls = false,
            "verify" => app.state.connections.get_mut("test").unwrap().origin_config.tls_verify = false,
            "auth" => app.irc_handles.get_mut("test").unwrap().sasl_authenticated = false,
            _ => app.state.connections.get_mut("test").unwrap().network_scope = Some("other".into()),
        }
        app.handle_webpush_request(&request, "one");
        assert!(app.irc_handles["test"].sender().captured().is_empty());
        assert!(matches!(web.try_recv().unwrap(), WebEvent::WebPush { status: Status::Unavailable, .. }));
    }
}

#[tokio::test]
async fn webpush_registration_payload_is_redacted_and_not_stored() {
    let mut app = app();
    let (scope, vapid) = app.webpush_configuration("test").unwrap();
    let subscription = webpush::Subscription { endpoint: "https://push.test/disposable-endpoint".into(), p256dh: key(), auth: URL_SAFE_NO_PAD.encode([2; 16]) };
    let auth = subscription.auth.clone();
    let request = WebRequest { connection_id: "test".into(), request_id: uuid::Uuid::new_v4().to_string(), action: Action::Register { scope, vapid, subscription } };
    let command = WebCommand::WebPush(Box::new(request));
    assert!(!format!("{command:?}").contains(&auth));
    assert!(!format!("{command:?}").contains("disposable-endpoint"));
    let (tx, mut logs) = tokio::sync::mpsc::channel(16);
    app.state.log_tx = Some(tx);
    let before: usize = app.state.buffers.values().map(|buffer| buffer.messages.len()).sum();
    app.handle_web_command(command, "one");
    receive(&mut app, "WEBPUSH REGISTER https://push.test/disposable-endpoint");
    finish(&mut app);
    assert!(logs.try_recv().is_err());
    assert_eq!(app.state.buffers.values().map(|buffer| buffer.messages.len()).sum::<usize>(), before);
    assert!(app.state.buffers.values().all(|buffer| buffer.messages.iter().all(|row| !row.text.contains(&auth) && !row.text.contains("disposable-endpoint"))));
}

#[tokio::test]
async fn webpush_get_is_read_only_and_replacement_cancels_uncertain_request() {
    let mut app = app();
    let mut web = app.web_broadcaster.subscribe();
    let request = WebRequest { connection_id: "test".into(), request_id: uuid::Uuid::new_v4().to_string(), action: Action::Get };
    app.handle_webpush_request(&request, "one");
    assert!(matches!(web.try_recv().unwrap(), WebEvent::WebPush { status: Status::Ready, scope: Some(_), vapid: Some(_), .. }));
    assert!(app.irc_handles["test"].sender().captured().is_empty());
    app.handle_webpush_request(&unregister(&app), "one");
    receive(&mut app, ":Mallory!user@host WEBPUSH UNREGISTER https://push.test/disposable-endpoint");
    assert!(!app.bouncer_webpush["test"].acknowledged);
    app.cancel_connection_attempt("test");
    assert!(!app.bouncer_webpush.contains_key("test"));
    assert!(matches!(web.try_recv().unwrap(), WebEvent::WebPush { status: Status::Unknown, .. }));
}

#[tokio::test]
async fn malformed_request_id_receives_correlated_invalid_response() {
    let mut app = app();
    let mut web = app.web_broadcaster.subscribe();
    let request = WebRequest { connection_id: "test".into(), request_id: "not-a-uuid".into(), action: Action::Get };
    app.handle_webpush_request(&request, "one");
    assert!(matches!(web.try_recv().unwrap(), WebEvent::WebPush { status: Status::Invalid, request_id, session_id, .. }
        if request_id == "not-a-uuid" && session_id == "one"));
    assert!(app.irc_handles["test"].sender().captured().is_empty());
}

#[tokio::test]
async fn webpush_control_sessions_cannot_register_undeliverable_subscriptions() {
    for retains_network_id in [false, true] {
        let mut app = app();
        let request = unregister(&app);
        let config = &mut app.state.connections.get_mut("test").unwrap().origin_config;
        config.bouncer_control = true;
        if !retains_network_id { config.bouncer_network_id = None; }
        let mut web = app.web_broadcaster.subscribe();
        app.handle_webpush_request(&request, "one");
        assert!(matches!(web.try_recv().unwrap(), WebEvent::WebPush { status: Status::Unavailable, .. }));
        assert!(app.irc_handles["test"].sender().captured().is_empty());
    }
}

#[tokio::test]
async fn browser_lookup_resolves_network_scope_and_rejects_wrong_account() {
    let mut app = app();
    let scope = app.webpush_configuration("test").unwrap().0;
    let mut web = app.web_broadcaster.subscribe();
    let mut request = WebRequest { connection_id: "old-transient-id".into(), request_id: uuid::Uuid::new_v4().to_string(), action: Action::Lookup { scope } };
    app.handle_webpush_request(&request, "browser");
    assert!(matches!(web.try_recv().unwrap(), WebEvent::WebPush { connection_id, status: Status::Ready, context: Some(_), .. } if connection_id == "test"));
    request.connection_id = "test".into();
    request.action = Action::Lookup { scope: "different-account".into() };
    app.handle_webpush_request(&request, "browser");
    assert!(matches!(web.try_recv().unwrap(), WebEvent::WebPush { status: Status::Unavailable, context: None, .. }));
    let mut request = unregister(&app);
    request.connection_id.clear();
    app.handle_webpush_request(&request, "browser");
    assert!(app.bouncer_webpush.contains_key("test"));
}
