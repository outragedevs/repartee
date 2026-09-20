use super::App;
use crate::irc::{IrcEvent, IrcHandle, IrcSender};

fn receive(app: &mut App, id: &str, wire: &str) {
    app.handle_irc_event(IrcEvent::Message(
        id.into(),
        Box::new(wire.parse().unwrap()),
    ));
}

fn setup() -> App {
    let mut app = super::input::submit_typing_tests::test_app();
    for id in ["first", "second"] {
        let config = toml::from_str(&format!("label='{id}'\naddress='127.0.0.1'\nport=1\ntls=false\nchannels=[]\nnick='me'\nbouncer_network_id='1'")).unwrap();
        app.setup_connection(id, &config);
        let conn = app.state.connections.get_mut(id).unwrap();
        conn.nick = "me".into();
        conn.status = crate::state::connection::ConnectionStatus::Connected;
        conn.enabled_caps
            .extend(["batch".into(), "labeled-response".into()]);
        app.irc_handles.insert(
            id.into(),
            IrcHandle::new(id.into(), IrcSender::capturing(0), None, None),
        );
        receive(&mut app, id, ":me!u@h JOIN #origin");
    }
    app.state.set_active_buffer("first/#origin");
    app
}

fn request(app: &mut App) -> String {
    app.execute_command(&crate::commands::parser::parse_command("/whois Alice").unwrap());
    crate::irc::labels::message_label(app.irc_handles["first"].sender().captured().last().unwrap())
        .unwrap()
        .to_string()
}

#[tokio::test]
async fn nested_whois_returns_to_origin_without_changing_active_buffer_or_logging() {
    let mut app = setup();
    let label = request(&mut app);
    app.state.set_active_buffer("second/#origin");
    let (tx, mut log_rx) = tokio::sync::mpsc::channel(64);
    app.state.log_tx = Some(tx);
    app.web_broadcaster = std::sync::Arc::new(crate::web::broadcast::WebBroadcaster::new(128));
    let mut web_rx = app.web_broadcaster.subscribe();
    receive(
        &mut app,
        "first",
        &format!("@label={label} :server BATCH +outer labeled-response"),
    );
    receive(
        &mut app,
        "first",
        "@batch=outer :server BATCH +inner vendor/wrapper",
    );
    receive(
        &mut app,
        "first",
        "@batch=inner :server 311 me Alice user host * :Alice Real Name",
    );
    receive(&mut app, "first", "@batch=outer :server BATCH -inner");
    assert!(app.labeled_requests["first"].is_pending(&label));
    receive(&mut app, "first", "@batch=outer :server 318 me Alice :End");
    receive(&mut app, "first", ":server BATCH -outer");
    assert!(!app.labeled_requests["first"].is_pending(&label));
    assert_eq!(
        app.state.active_buffer_id.as_deref(),
        Some("second/#origin")
    );
    assert!(app.state.irc_reply_buffer.is_none());
    let events: Vec<_> = std::iter::from_fn(|| web_rx.try_recv().ok()).collect();
    assert!(events.iter().any(|event| matches!(event, crate::web::protocol::WebEvent::NewMessage { buffer_id, message } if buffer_id == "first/#origin" && message.text.contains("Alice"))));
    assert!(!events.iter().any(|event| matches!(event, crate::web::protocol::WebEvent::NewMessage { buffer_id, .. } if buffer_id.starts_with("second/"))));
    assert!(log_rx.try_recv().is_err());
}

#[tokio::test]
async fn ack_finishes_request_without_displaying_a_message() {
    let mut app = setup();
    let label = request(&mut app);
    let count = app.state.buffers["first/#origin"].messages.len();
    receive(&mut app, "first", &format!("@label={label} :server ACK"));
    assert!(!app.labeled_requests["first"].is_pending(&label));
    assert_eq!(app.state.buffers["first/#origin"].messages.len(), count);
}

#[tokio::test]
async fn expired_closed_and_foreign_contexts_fall_back_to_the_origin_server() {
    for mode in ["expired", "closed", "foreign"] {
        let mut app = setup();
        let label = request(&mut app);
        if mode == "expired" {
            app.labeled_requests
                .get_mut("first")
                .unwrap()
                .expire(std::time::Instant::now() + std::time::Duration::from_mins(2));
        } else if mode == "closed" {
            app.state.buffers.shift_remove("first/#origin");
        }
        app.state.set_active_buffer("second/#origin");
        let id = if mode == "foreign" { "second" } else { "first" };
        let (tx, mut log_rx) = tokio::sync::mpsc::channel(64);
        app.state.log_tx = Some(tx);
        receive(
            &mut app,
            id,
            &format!("@label={label} :server 401 me Alice :No such nick"),
        );
        assert!(
            app.state.buffers[&format!("{id}/{id}")]
                .messages
                .iter()
                .any(|message| message.text.contains("Alice"))
        );
        assert!(
            !app.state.buffers["second/#origin"]
                .messages
                .iter()
                .any(|message| message.text.contains("Alice"))
        );
        assert!(log_rx.try_recv().is_err());
    }
}

#[tokio::test]
async fn capability_loss_disconnect_and_failed_send_clear_pending_context() {
    let mut app = setup();
    let _ = request(&mut app);
    receive(&mut app, "first", ":server CAP me DEL :labeled-response");
    assert!(!app.labeled_requests.contains_key("first"));
    app.execute_command(&crate::commands::parser::parse_command("/whois Bob").unwrap());
    assert!(
        crate::irc::labels::message_label(
            app.irc_handles["first"].sender().captured().last().unwrap()
        )
        .is_none()
    );
    receive(&mut app, "first", ":server CAP me ACK :labeled-response");
    let _ = request(&mut app);
    app.handle_irc_event(IrcEvent::Disconnected("first".into(), None));
    assert!(!app.labeled_requests.contains_key("first"));
    let mut app = setup();
    app.irc_handles.insert(
        "first".into(),
        IrcHandle::new(
            "first".into(),
            IrcSender::capturing_then_failing(0),
            None,
            None,
        ),
    );
    assert!(
        app.send_active_labeled_request(irc::proto::Command::WHOIS(None, "Alice".into()))
            .is_err()
    );
    assert_eq!(app.labeled_requests["first"].pending_count(), 0);
}

#[tokio::test]
async fn web_command_keeps_its_buffer_context_independent_of_terminal_selection() {
    let mut app = setup();
    app.state.set_active_buffer("second/#origin");
    app.handle_web_command(
        crate::web::protocol::WebCommand::RunCommand {
            buffer_id: "first/#origin".into(),
            text: "/whois Alice".into(),
        },
        "browser",
    );
    let frames = app.irc_handles["first"].sender().captured();
    let label = crate::irc::labels::message_label(frames.last().unwrap()).unwrap();
    assert_eq!(
        app.state.active_buffer_id.as_deref(),
        Some("second/#origin")
    );
    receive(
        &mut app,
        "first",
        &format!("@label={label} :server 401 me Alice :No such nick"),
    );
    assert!(
        app.state.buffers["first/#origin"]
            .messages
            .iter()
            .any(|message| message.text.contains("Alice"))
    );
    assert!(
        !app.state.buffers["second/#origin"]
            .messages
            .iter()
            .any(|message| message.text.contains("Alice"))
    );
    assert_eq!(
        app.state.active_buffer_id.as_deref(),
        Some("second/#origin")
    );
}

#[tokio::test]
async fn unlabeled_replies_never_cross_networks_and_preserve_direct_irc_logging() {
    for bouncer in [true, false] {
        let mut app = setup();
        let conn = app.state.connections.get_mut("first").unwrap();
        conn.enabled_caps.remove("labeled-response");
        if !bouncer {
            conn.origin_config.bouncer_network_id = None;
        }
        app.state
            .connections
            .get_mut("second")
            .unwrap()
            .origin_config
            .bouncer_network_id = None;
        app.state.set_active_buffer("second/#origin");
        let (tx, mut log_rx) = tokio::sync::mpsc::channel(64);
        app.state.log_tx = Some(tx);
        app.web_broadcaster = std::sync::Arc::new(crate::web::broadcast::WebBroadcaster::new(128));
        let mut web_rx = app.web_broadcaster.subscribe();
        receive(
            &mut app,
            "first",
            ":server 311 me Alice user host * :unlabeled-reply",
        );
        receive(&mut app, "first", ":server 318 me Alice :End");
        assert!(
            app.state.buffers["first/first"]
                .messages
                .iter()
                .any(|message| message.text.contains("unlabeled-reply"))
        );
        assert!(
            !app.state.buffers["second/#origin"]
                .messages
                .iter()
                .any(|message| message.text.contains("Alice"))
        );
        assert_eq!(
            app.state.active_buffer_id.as_deref(),
            Some("second/#origin")
        );
        let events: Vec<_> = std::iter::from_fn(|| web_rx.try_recv().ok()).collect();
        assert!(events.iter().any(|event| matches!(event, crate::web::protocol::WebEvent::NewMessage { buffer_id, message } if buffer_id == "first/first" && message.text.contains("unlabeled-reply"))));
        assert_eq!(log_rx.try_recv().is_err(), bouncer);
    }
}

#[tokio::test]
async fn bouncer_server_replies_are_not_persisted_without_labels() {
    for control in [true, false] {
        let mut app = setup();
        let config = &mut app
            .state
            .connections
            .get_mut("first")
            .unwrap()
            .origin_config;
        config.bouncer_control = control;
        if control {
            config.bouncer_network_id = None;
        }
        app.state.set_active_buffer("first/first");
        let (tx, mut log_rx) = tokio::sync::mpsc::channel(64);
        app.state.log_tx = Some(tx);
        receive(
            &mut app,
            "first",
            ":server 311 me Alice user host * :server-buffer-reply",
        );
        assert!(
            app.state.buffers["first/first"]
                .messages
                .iter()
                .any(|message| message.text.contains("server-buffer-reply"))
        );
        assert!(log_rx.try_recv().is_err());
    }
}

#[tokio::test]
async fn unlabeled_replies_keep_the_active_buffer_on_the_same_network() {
    let mut app = setup();
    receive(
        &mut app,
        "first",
        ":server 311 me Alice user host * :same-network-reply",
    );
    assert!(
        app.state.buffers["first/#origin"]
            .messages
            .iter()
            .any(|message| message.text.contains("same-network-reply"))
    );
}

#[tokio::test]
async fn redaction_web_request_and_rejection_keep_origin_without_local_deletion() {
    let mut app = setup();
    app.state
        .connections
        .get_mut("first")
        .unwrap()
        .enabled_caps
        .extend(["draft/message-redaction".into(), "message-tags".into()]);
    receive(
        &mut app,
        "first",
        "@msgid=original :Alice!u@h PRIVMSG #origin :keep this message",
    );
    app.state.set_active_buffer("second/#origin");
    app.handle_web_command(
        crate::web::protocol::WebCommand::RunCommand {
            buffer_id: "first/#origin".into(),
            text: "/redact #origin original optional reason".into(),
        },
        "browser",
    );
    let frames = app.irc_handles["first"].sender().captured();
    let frame = frames.last().unwrap();
    assert_eq!(
        frame.command,
        irc::proto::Command::Raw(
            "REDACT".into(),
            vec![
                "#origin".into(),
                "original".into(),
                "optional reason".into()
            ]
        )
    );
    let label = crate::irc::labels::message_label(frame).unwrap();
    assert_eq!(app.state.redaction_registry.deletion_count(), 0);
    receive(
        &mut app,
        "first",
        &format!(
            "@label={label} :server FAIL REDACT REDACT_FORBIDDEN #origin original :not allowed %N"
        ),
    );
    assert!(
        app.state.buffers["first/#origin"]
            .messages
            .iter()
            .any(|message| message.text == "keep this message")
    );
    assert!(
        app.state.buffers["first/#origin"]
            .messages
            .iter()
            .any(|message| message.text.contains("REDACT_FORBIDDEN")
                && message.text.ends_with("%%N"))
    );
    assert!(
        !app.state.buffers["second/#origin"]
            .messages
            .iter()
            .any(|message| message.text.contains("REDACT_FORBIDDEN"))
    );
    assert_eq!(
        app.state.active_buffer_id.as_deref(),
        Some("second/#origin")
    );
    assert_eq!(app.state.redaction_registry.deletion_count(), 0);
}

#[tokio::test]
async fn redaction_received_before_cap_loss_survives_history_batch_completion() {
    let mut app = setup();
    app.state
        .connections
        .get_mut("first")
        .unwrap()
        .enabled_caps
        .extend(["draft/message-redaction".into(), "message-tags".into()]);
    receive(
        &mut app,
        "first",
        ":server BATCH +history chathistory #origin",
    );
    receive(
        &mut app,
        "first",
        "@batch=history;msgid=gone;time=2024-01-01T00:00:01.000Z :Alice!u@h PRIVMSG #origin :never expose this",
    );
    receive(
        &mut app,
        "first",
        "@batch=history :Alice!u@h REDACT #origin gone",
    );
    receive(
        &mut app,
        "first",
        ":server CAP me DEL :draft/message-redaction",
    );
    assert!(
        !app.state.connections["first"]
            .enabled_caps
            .contains("draft/message-redaction")
    );
    receive(&mut app, "first", ":server BATCH -history");
    assert!(
        !app.state.buffers["first/#origin"]
            .messages
            .iter()
            .any(|message| message.text == "never expose this")
    );
    assert!(
        app.state.buffers["first/#origin"]
            .messages
            .iter()
            .any(|message| message.text == "Message deleted by Alice")
    );
}

#[tokio::test]
async fn nested_history_keeps_redaction_after_cache_pressure() {
    let mut app = setup();
    app.state
        .connections
        .get_mut("first")
        .unwrap()
        .enabled_caps
        .extend(["draft/message-redaction".into(), "message-tags".into()]);
    receive(
        &mut app,
        "first",
        ":server BATCH +history chathistory #origin",
    );
    receive(&mut app, "first", "@batch=history :server BATCH +child labeled-response");
    receive(
        &mut app,
        "first",
        "@batch=child;msgid=gone;time=2024-01-01T00:00:01.000Z :Alice!u@h PRIVMSG #origin :never expose this",
    );
    receive(
        &mut app,
        "first",
        "@batch=history :Alice!u@h REDACT #origin gone",
    );
    receive(
        &mut app,
        "first",
        ":server CAP me DEL :draft/message-redaction",
    );
    assert!(
        !app.state.connections["first"]
            .enabled_caps
            .contains("draft/message-redaction")
    );
    receive(&mut app, "first", ":server BATCH -child");
    for id in 0..10_000 {
        app.state.redaction_registry.redact(
            ("pressure".into(), "#channel".into(), id.to_string()), "deleted".into(),
        );
    }
    receive(&mut app, "first", ":server BATCH -history");
    assert!(
        !app.state.buffers["first/#origin"]
            .messages
            .iter()
            .any(|message| message.text == "never expose this")
    );
    assert!(
        app.state.buffers["first/#origin"]
            .messages
            .iter()
            .any(|message| message.text == "Message deleted by Alice")
    );
}

#[tokio::test]
async fn multiline_opener_retains_deletion_before_fragments_arrive() {
    let mut app = setup();
    app.state.connections.get_mut("first").unwrap().enabled_caps
        .extend(["draft/message-redaction".into(), "message-tags".into()]);
    receive(&mut app, "first", ":server BATCH +history chathistory #origin");
    receive(&mut app, "first", "@batch=history;msgid=multi-ID;time=2024-01-01T00:00:01.000Z :Alice!u@h BATCH +multi draft/multiline #origin");
    receive(&mut app, "first", ":Alice!u@h REDACT #origin multi-ID");
    for id in 0..10_000 {
        app.state.redaction_registry.redact(
            ("pressure".into(), "#channel".into(), id.to_string()), "deleted".into(),
        );
    }
    receive(&mut app, "first", "@batch=multi :Alice!u@h PRIVMSG #origin :hidden first line");
    receive(&mut app, "first", "@batch=multi :Alice!u@h PRIVMSG #origin :hidden second line");
    receive(&mut app, "first", ":server BATCH -multi");
    receive(&mut app, "first", ":server BATCH -history");
    let messages = &app.state.buffers["first/#origin"].messages;
    assert!(messages.iter().any(|message| message.text == "Message deleted by Alice"));
    assert!(!messages.iter().any(|message| message.text.contains("hidden")));
}
