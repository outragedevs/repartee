use super::App;
use crate::irc::IrcEvent;

fn app() -> App {
    let mut app = crate::app::input::submit_typing_tests::test_app();
    let config = toml::from_str("label='fixture'\naddress='127.0.0.1'\nport=1\ntls=false\nchannels=[]\nnick='me'").unwrap();
    for id in ["fixture", "other"] { app.setup_connection(id, &config); }
    app
}

fn receive(app: &mut App, wire: &str) {
    app.handle_irc_event(IrcEvent::Message("fixture".into(), Box::new(wire.parse().unwrap())));
}

fn value<'a>(app: &'a App, key: &str) -> Option<&'a str> {
    app.state.connections["fixture"].isupport_parsed.get(key)
}

#[tokio::test]
async fn isupport_burst_is_atomic_incremental_and_isolated() {
    let mut app = app();
    receive(&mut app, ":s 005 me CHANTYPES=#& WHOX NICKLEN=20 :supported tokens");
    receive(&mut app, ":s BATCH +a draft/isupport");
    receive(&mut app, "@batch=a :s 005 me -WHOX NICKLEN=30 :supported tokens");
    assert_eq!(value(&app, "NICKLEN"), Some("20"));
    assert!(value(&app, "WHOX").is_some());
    receive(&mut app, "@batch=a :s 005 me STATUSMSG=@+ MODES=5");
    receive(&mut app, ":s BATCH -a");
    assert_eq!(value(&app, "NICKLEN"), Some("30"));
    assert_eq!(value(&app, "MODES"), Some("5"));
    assert_eq!(value(&app, "CHANTYPES"), Some("#&"));
    assert_eq!(value(&app, "WHOX"), None);
    assert_eq!(app.state.connections["other"].isupport_parsed.get("NICKLEN"), None);
    receive(&mut app, ":s 005 me NICKLEN=40 :supported tokens");
    assert_eq!(value(&app, "NICKLEN"), Some("40"));
}

#[tokio::test]
async fn isupport_empty_malformed_nested_and_orphan_bursts_do_not_mutate() {
    for lines in [
        vec![":s BATCH +a draft/isupport", "@batch=a :s 005 me NICKLEN=30 -WHOX=value :supported tokens", ":s BATCH -a"],
        vec![":s BATCH +a draft/isupport", "@batch=a :s 005 me NICKLEN=30 :supported tokens", "@batch=a :s BATCH -unknown", ":s BATCH -a"],
        vec![":s BATCH +a draft/isupport", ":s BATCH -a"],
        vec![":s BATCH +a draft/isupport", "@batch=a :s 005 me NICKLEN=30 :supported tokens", "@batch=a :s NOTICE me :invalid", ":s BATCH -a"],
        vec![":s BATCH +a draft/isupport", "@batch=a :s 005 me NICKLEN=30 =bad :supported tokens", ":s BATCH -a"],
        vec![":s BATCH +a draft/isupport", "@batch=a :s BATCH +b draft/isupport", "@batch=b :s 005 me NICKLEN=30 :supported tokens", ":s BATCH -b", ":s BATCH -a"],
        vec!["@batch=missing :s BATCH +a draft/isupport", "@batch=a :s 005 me NICKLEN=30 :supported tokens", ":s BATCH -a"],
        vec![":s BATCH +a draft/isupport", "@batch=a :s BATCH +b vendor/unknown", "@batch=b :s 005 me NICKLEN=30 :supported tokens", ":s BATCH -b", ":s BATCH -a"],
    ] {
        let mut app = app();
        receive(&mut app, ":s 005 me NICKLEN=20 :supported tokens");
        for line in lines { receive(&mut app, line); }
        assert_eq!(value(&app, "NICKLEN"), Some("20"));
    }
}

#[tokio::test]
async fn isupport_expired_and_overflow_bursts_do_not_apply_partial_updates() {
    for expired in [false, true] {
        let mut app = app();
        receive(&mut app, ":s BATCH +a draft/isupport");
        receive(&mut app, "@batch=a :s 005 me NICKLEN=30 :supported tokens");
        if expired {
            let batch = app.batch_trackers.get_mut("fixture").unwrap().end_batch("a").unwrap();
            app.dispatch_expired_batch_set(vec![("fixture".into(), "a".into(), batch)]);
        } else {
            for _ in 0..4096 { receive(&mut app, "@batch=a :s 005 me MODES=5 :supported tokens"); }
            receive(&mut app, ":s BATCH -a");
        }
        assert_eq!(value(&app, "NICKLEN"), None);
    }
}

#[tokio::test]
async fn isupport_pre_registration_survives_welcome_and_labeled_parent() {
    let mut app = app();
    receive(&mut app, ":s BATCH +outer labeled-response");
    receive(&mut app, "@batch=outer :s BATCH +a draft/isupport");
    receive(&mut app, "@batch=a :s 005 * NICKLEN=30 :supported tokens");
    receive(&mut app, ":s BATCH -a");
    receive(&mut app, ":s BATCH -outer");
    app.handle_irc_event(IrcEvent::Connected("fixture".into(), ["batch".into(), "draft/extended-isupport".into()].into(), None));
    assert_eq!(value(&app, "NICKLEN"), Some("30"));
    app.handle_irc_event(IrcEvent::Disconnected("fixture".into(), None));
    assert_eq!(value(&app, "NICKLEN"), None);
}

#[tokio::test]
#[ignore = "requires disposable pinned bouncer; scripts/test_bouncer_binding.py"]
async fn pinned_bouncer_extended_isupport() {
    let mut app = app();
    let mut config = app.state.connections["fixture"].origin_config.clone();
    config.port = std::env::var("REPARTEE_BOUNCER_TEST_PORT").unwrap().parse().unwrap();
    config.bouncer_network_id = Some(std::env::var("REPARTEE_BOUNCER_TEST_NETID").unwrap());
    config.sasl_user = Some(std::env::var("REPARTEE_BOUNCER_TEST_USER").unwrap());
    config.sasl_pass = Some("fixture-password".into());
    let soju = std::env::var("REPARTEE_BOUNCER_TEST_PROVIDER").unwrap() == "soju";
    app.setup_connection("fixture", &config);
    let general = crate::config::GeneralConfig { flood_protection: false, ..Default::default() };
    let (handle, mut events) = crate::irc::connect_server("fixture", &config, &general).await.unwrap();
    let sender = handle.sender().clone();
    app.irc_handles.insert("fixture".into(), handle);
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while let Some(event) = events.recv().await {
            let complete = matches!(&event, IrcEvent::Message(_, msg) if matches!(&msg.command, irc::proto::Command::Response(irc::proto::Response::RPL_ENDOFMOTD | irc::proto::Response::ERR_NOMOTD, _)));
            app.handle_irc_event(event);
            if complete { break; }
        }
        assert_eq!(value(&app, "BOUNCER_NETID"), config.bouncer_network_id.as_deref());
        assert_eq!(app.state.connections["fixture"].enabled_caps.contains("draft/extended-isupport"), soju);
        if !soju { return; }
        app.state.connections.get_mut("fixture").unwrap().isupport_parsed.parse_tokens(&["BOUNCER_NETID=999", "FIXTURE_UNMENTIONED=keep"]);
        sender.send(irc::proto::Command::Raw("ISUPPORT".into(), vec![])).unwrap();
        let mut opened = false;
        while let Some(event) = events.recv().await {
            let closed = if let IrcEvent::Message(_, msg) = &event
                && let irc::proto::Command::BATCH(reference, kind, _) = &msg.command
            {
                if reference.starts_with('+') && kind.as_ref().is_some_and(|kind| kind.to_str().eq_ignore_ascii_case("draft/isupport")) { opened = true; }
                opened && reference.starts_with('-')
            } else { false };
            app.handle_irc_event(event);
            if closed {
                assert_eq!(value(&app, "FIXTURE_UNMENTIONED"), Some("keep"));
                assert_eq!(value(&app, "BOUNCER_NETID"), config.bouncer_network_id.as_deref());
                return;
            }
            assert_eq!(value(&app, "BOUNCER_NETID"), Some("999"));
        }
        panic!("ISUPPORT reply did not complete");
    }).await.unwrap();
}

#[tokio::test]
async fn isupport_final_network_label_is_applied_once_and_disconnect_discards_open_burst() {
    let mut app = app();
    let mut config = app.state.connections["fixture"].origin_config.clone();
    config.label = "irc.example.test".into();
    config.address.clone_from(&config.label);
    app.setup_connection("fixture", &config);
    receive(&mut app, ":s BATCH +a draft/isupport");
    receive(&mut app, "@batch=a :s 005 me NETWORK=Intermediate :supported tokens");
    receive(&mut app, "@batch=a :s 005 me NETWORK=Final :supported tokens");
    assert_eq!(app.state.connections["fixture"].label, "irc.example.test");
    receive(&mut app, ":s BATCH -a");
    assert_eq!(app.state.connections["fixture"].label, "Final");
    assert!(app.state.buffers.contains_key("fixture/final"));
    assert!(!app.state.buffers.contains_key("fixture/intermediate"));
    receive(&mut app, ":s BATCH +rename draft/isupport");
    receive(&mut app, "@batch=rename :s 005 me NETWORK=Later :supported tokens");
    receive(&mut app, ":s BATCH -rename");
    assert_eq!(app.state.connections["fixture"].label, "Later");
    assert!(app.state.buffers.contains_key("fixture/later"));
    receive(&mut app, ":s 005 me NETWORK=LATER :supported tokens");
    assert_eq!(app.state.buffers["fixture/later"].name, "LATER");
    receive(&mut app, ":s BATCH +remove draft/isupport");
    receive(&mut app, "@batch=remove :s 005 me -NETWORK :supported tokens");
    receive(&mut app, ":s BATCH -remove");
    assert_eq!(app.state.connections["fixture"].label, "irc.example.test");
    app.state.connections.get_mut("fixture").unwrap().label = "Manual".into();
    receive(&mut app, ":s 005 me NETWORK=Ignored :supported tokens");
    assert_eq!(app.state.connections["fixture"].label, "Manual");
    receive(&mut app, ":s BATCH +b draft/isupport");
    receive(&mut app, "@batch=b :s 005 me NICKLEN=30 :supported tokens");
    app.handle_irc_event(IrcEvent::Disconnected("fixture".into(), None));
    app.handle_irc_event(IrcEvent::Connected("fixture".into(), ["batch".into(), "draft/extended-isupport".into()].into(), None));
    receive(&mut app, ":s BATCH -b");
    assert_eq!(value(&app, "NICKLEN"), None);
}

#[tokio::test]
async fn isupport_preserves_explicit_dotted_labels() {
    let mut app = app();
    let mut config = app.state.connections["fixture"].origin_config.clone();
    config.label = "My.Network".into();
    app.setup_connection("fixture", &config);
    receive(&mut app, ":s BATCH +a draft/isupport");
    receive(&mut app, "@batch=a :s 005 me NETWORK=ServerNetwork :supported tokens");
    receive(&mut app, ":s BATCH -a");
    assert_eq!(app.state.connections["fixture"].label, "My.Network");
}

#[tokio::test]
async fn isupport_label_collision_preserves_query_and_web_selection() {
    use crate::state::buffer::{Buffer, BufferType};
    let mut app = app();
    let mut config = app.state.connections["fixture"].origin_config.clone();
    config.label.clone_from(&config.address);
    app.setup_connection("fixture", &config);
    let old_id = crate::state::buffer::make_buffer_id("fixture", &config.label);
    app.state.add_buffer(Buffer::for_test("fixture", BufferType::Query, "alice"));
    app.web_active_buffers.insert("browser".into(), old_id.clone());
    let mut events = app.web_broadcaster.subscribe();
    receive(&mut app, ":s BATCH +a draft/isupport");
    receive(&mut app, "@batch=a :s 005 me NETWORK=alice :supported tokens");
    receive(&mut app, ":s BATCH -a");
    assert_eq!(app.state.connections["fixture"].label, "alice [2]");
    assert_eq!(app.state.buffers["fixture/alice"].buffer_type, BufferType::Query);
    assert_eq!(app.web_active_buffers["browser"], "fixture/alice [2]");
    let mut renamed = false;
    while let Ok(event) = events.try_recv() {
        if let crate::web::protocol::WebEvent::BufferRenamed { old_id: before, new_id, .. } = event {
            renamed |= before == old_id && new_id == "fixture/alice [2]";
        }
    }
    assert!(renamed);
}

#[tokio::test]
async fn network_icon_command_uses_atomic_connection_scoped_state() {
    let mut app = app();
    app.state.set_active_buffer("fixture/fixture");
    receive(&mut app, ":s 005 me draft/ICON=https://example.org/{size}.png?x=100%25 :supported tokens");
    app.handle_submit("/server icon");
    assert!(app.state.buffers["fixture/fixture"].messages.back().unwrap().text.contains("https://example.org/128.png?x=100%%25"));
    receive(&mut app, ":s BATCH +icon draft/isupport");
    receive(&mut app, "@batch=icon :s 005 me -draft/ICON :supported tokens");
    assert!(app.state.connections["fixture"].isupport_parsed.network_icon(32).is_some());
    receive(&mut app, ":s BATCH -icon");
    app.handle_submit("/server icon");
    assert!(app.state.buffers["fixture/fixture"].messages.back().unwrap().text.contains("has not advertised"));
    assert_eq!(app.state.connections["other"].isupport_parsed.network_icon(32), None);
}

#[tokio::test]
async fn network_icon_web_events_and_snapshot_follow_atomic_updates_and_disconnect() {
    use crate::web::protocol::WebEvent;
    let mut app = app();
    app.state.web_icon_extractor = Some(std::sync::Arc::new(crate::web::preview::WebPreviewExtractor::new(vec![1; 32], 3, 10)));
    receive(&mut app, ":s 005 me draft/ICON=https://example.org/{size}.svg :supported tokens");
    let initial = crate::web::snapshot::network_icon_url(&app.state, "fixture").unwrap();
    assert!(initial.starts_with("/api/network-icon?h="));
    assert!(!initial.contains("example.org"));
    assert!(crate::web::snapshot::network_icon_url(&app.state, "other").is_none());
    receive(&mut app, ":s BATCH +icon draft/isupport");
    receive(&mut app, "@batch=icon :s 005 me -draft/ICON :supported tokens");
    assert_eq!(crate::web::snapshot::network_icon_url(&app.state, "fixture"), Some(initial));
    receive(&mut app, ":s BATCH -icon");
    assert!(crate::web::snapshot::network_icon_url(&app.state, "fixture").is_none());
    receive(&mut app, ":s 005 me draft/ICON=https://example.org/new.png :supported tokens");
    assert!(crate::web::snapshot::network_icon_url(&app.state, "fixture").is_some());
    crate::irc::events::handle_disconnected(&mut app.state, "fixture", None);
    assert!(app.state.pending_web_events.iter().any(|event| matches!(event, WebEvent::NetworkIcon { conn_id, icon_url: None } if conn_id == "fixture")));
    assert!(crate::web::snapshot::network_icon_url(&app.state, "fixture").is_none());
}
