use super::*;
use crate::irc::{IrcEvent, IrcHandle, IrcSender};

fn app() -> super::super::App {
    let mut app = crate::app::input::submit_typing_tests::test_app();
    let config = toml::from_str("label='fixture'\naddress='localhost'\nport=6697\ntls=true\nchannels=[]\nbouncer_control=true").unwrap();
    app.setup_connection("test", &config);
    let conn = app.state.connections.get_mut("test").unwrap();
    conn.status = ConnectionStatus::Connected;
    conn.enabled_caps.extend([CAP.into(), "batch".into()]);
    let mut handle = IrcHandle::new("test".into(), IrcSender::capturing(0), None, None);
    handle.sasl_authenticated = true;
    app.irc_handles.insert("test".into(), handle);
    app.state.set_active_buffer("test/fixture");
    app
}

fn receive(app: &mut super::super::App, wire: &str) {
    app.handle_irc_event(IrcEvent::Message("test".into(), Box::new(wire.parse().unwrap())));
}

fn finish(app: &mut super::super::App) {
    let nonce = app.bouncer_certificates["test"].nonce.clone();
    receive(app, &format!(":bouncer PONG :{nonce}"));
}

fn contains(app: &super::super::App, text: &str) -> bool {
    app.state.buffers["test/fixture"].messages.iter().any(|row| row.text.contains(text))
}

#[tokio::test]
async fn certificate_commands_require_actual_sasl_and_bouncer_caps() {
    let mut app = app();
    app.irc_handles.get_mut("test").unwrap().sasl_authenticated = false;
    app.state.connections.get_mut("test").unwrap().enabled_caps.insert("sasl".into());
    command(&mut app, &[]);
    assert!(app.irc_handles["test"].sender().captured().is_empty());
    app.irc_handles.get_mut("test").unwrap().sasl_authenticated = true;
    command(&mut app, &[]);
    assert_eq!(app.irc_handles["test"].sender().captured().len(), 2);
    receive(&mut app, ":bouncer CAP * DEL :soju.im/client-cert");
    receive(&mut app, ":bouncer BATCH +empty soju.im/client-cert");
    receive(&mut app, ":bouncer BATCH -empty");
    finish(&mut app);
    assert!(contains(&app, "did not complete successfully"));
    assert!(!contains(&app, "Pinned certificates:"));
}

#[tokio::test]
async fn certificate_list_waits_for_batch_and_round_trip_and_escapes_names() {
    let mut app = app();
    command(&mut app, &[]);
    receive(&mut app, ":bouncer BATCH +certs soju.im/client-cert");
    receive(&mut app, &format!("@batch=certs CLIENTCERT LIST {} :name=phone\\s100%\\:ready;time=2026-09-20T00:00:00Z;unknown=ignored", "ab".repeat(64)));
    receive(&mut app, ":bouncer BATCH -certs");
    assert!(!contains(&app, "Pinned certificates:"));
    finish(&mut app);
    assert!(contains(&app, "Pinned certificates: 1"));
    assert!(contains(&app, "phone 100%%;ready"));
    assert!(!contains(&app, "unknown=ignored"));
}

#[tokio::test]
async fn certificate_empty_list_and_incomplete_batches_are_distinct() {
    for closed in [false, true] {
        let mut app = app();
        command(&mut app, &[]);
        receive(&mut app, ":bouncer BATCH +certs soju.im/client-cert");
        if closed { receive(&mut app, ":bouncer BATCH -certs"); }
        finish(&mut app);
        assert_eq!(contains(&app, "Pinned certificates: 0"), closed);
        assert_eq!(contains(&app, "did not complete successfully"), !closed);
    }
}

#[tokio::test]
async fn certificate_mutations_need_matching_reply_and_do_not_change_config() {
    let mut app = app();
    command(&mut app, &["create".into(), "desktop".into()]);
    receive(&mut app, ":Mallory!u@h CLIENTCERT CREATE");
    finish(&mut app);
    assert!(!contains(&app, "Current TLS client certificate pinned"));
    command(&mut app, &["create".into(), "desktop".into()]);
    receive(&mut app, "CLIENTCERT CREATE");
    finish(&mut app);
    assert!(contains(&app, "Current TLS client certificate pinned"));
    assert!(app.state.connections["test"].origin_config.sasl_mechanism.is_none());
    command(&mut app, &["delete".into(), "ab".repeat(64)]);
    receive(&mut app, &format!("CLIENTCERT DELETE {}", "cd".repeat(64)));
    finish(&mut app);
    assert!(!contains(&app, "Certificate removed"));
}

#[tokio::test]
async fn certificate_timeout_does_not_allow_ambiguous_retry() {
    let mut app = app();
    command(&mut app, &["delete".into()]);
    app.bouncer_certificates.get_mut("test").unwrap().started = Instant::now().checked_sub(Duration::from_secs(31)).unwrap();
    app.tick_bouncer_certificates();
    let count = app.irc_handles["test"].sender().captured().len();
    command(&mut app, &["delete".into()]);
    assert_eq!(app.irc_handles["test"].sender().captured().len(), count);
    receive(&mut app, "FAIL CLIENTCERT NOCERT DELETE :No TLS certificate");
    finish(&mut app);
    assert!(!contains(&app, "Certificate removed"));
    assert!(contains(&app, "NOCERT"));
    command(&mut app, &[]);
    app.cancel_connection_attempt("test");
    assert!(!app.bouncer_certificates.contains_key("test"));
}

#[tokio::test]
async fn dynamic_certificate_cap_is_requested_for_control_and_bound_bouncers() {
    let mut app = app();
    app.state.connections.get_mut("test").unwrap().enabled_caps.remove(CAP);
    receive(&mut app, ":bouncer CAP * NEW :soju.im/client-cert");
    assert!(app.irc_handles["test"].sender().captured().iter().any(|message| message.to_string().contains("CAP REQ") && message.to_string().contains("soju.im/client-cert")));
}

#[test]
fn certificate_parameters_reject_invalid_fingerprints_and_unescape_known_metadata() {
    assert!(!fingerprint("aa"));
    assert!(!fingerprint(&"xz".repeat(64)));
    assert_eq!(attribute("a\\sb\\:c\\\\d\\ne"), "a b;c\\d e");
    assert!(certificate_row(&["not-a-fingerprint".into()]).is_none());
}

#[tokio::test]
async fn unsolicited_create_does_not_invalidate_list_or_delete() {
    let mut app = app();
    command(&mut app, &[]);
    receive(&mut app, ":bouncer BATCH +certs soju.im/client-cert");
    receive(&mut app, "CLIENTCERT CREATE");
    receive(&mut app, ":bouncer BATCH -certs");
    finish(&mut app);
    assert!(contains(&app, "Pinned certificates: 0"));
    assert!(contains(&app, "Bouncer confirmed"));
    command(&mut app, &["delete".into()]);
    receive(&mut app, "CLIENTCERT CREATE");
    receive(&mut app, &format!("CLIENTCERT DELETE {}", "ab".repeat(64)));
    finish(&mut app);
    assert!(contains(&app, "Certificate removed"));
    assert!(!contains(&app, "did not complete successfully"));
}

#[tokio::test]
async fn current_certificate_delete_accepts_bare_ack_only_for_implicit_target() {
    for explicit in [false, true] {
        let mut app = app();
        let mut args = vec!["delete".into()];
        if explicit { args.push("ab".repeat(64)); }
        command(&mut app, &args);
        receive(&mut app, "CLIENTCERT DELETE");
        finish(&mut app);
        assert_eq!(contains(&app, "Certificate removed"), !explicit);
        assert_eq!(contains(&app, "did not complete successfully"), explicit);
    }
}

#[tokio::test]
async fn certificate_management_requires_verified_tls_even_after_sasl() {
    for (tls, verify) in [(false, true), (true, false)] {
        let mut app = app();
        let config = &mut app.state.connections.get_mut("test").unwrap().origin_config;
        config.tls = tls;
        config.tls_verify = verify;
        for args in [vec!["list".into()], vec!["create".into(), "device".into()], vec!["delete".into()]] {
            command(&mut app, &args);
        }
        assert!(app.irc_handles["test"].sender().captured().is_empty());
        assert!(!app.bouncer_certificates.contains_key("test"));
        assert!(contains(&app, "verified TLS"));
    }
}
