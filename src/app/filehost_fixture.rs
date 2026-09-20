use super::App;

async fn until(app: &mut App, predicate: impl Fn(&App) -> bool + Send + Sync) {
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        loop {
            while let Ok(event) = app.irc_rx.try_recv() { app.handle_irc_event(event); }
            if predicate(app) { return; }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }).await.expect("filehost fixture timed out");
}

#[tokio::test]
#[ignore = "requires a disposable pinned bouncer fixture"]
async fn pinned_bouncer_filehost() {
    let mut app = super::input::submit_typing_tests::test_app();
    app.config.general.flood_protection = false;
    let mut config: crate::config::ServerConfig = toml::from_str("label='fixture'\naddress='127.0.0.1'\nport=1\ntls=false\nchannels=[]\nbouncer_network_id='1'").unwrap();
    config.port = std::env::var("REPARTEE_BOUNCER_TEST_PORT").unwrap().parse().unwrap();
    let user = std::env::var("REPARTEE_BOUNCER_TEST_USER").unwrap();
    let provider = if std::env::var("REPARTEE_PRESENCE_PROVIDER").as_deref() == Ok("lurker") {
        crate::irc::bouncer::Provider::Lurker
    } else { crate::irc::bouncer::Provider::Soju };
    let legacy = std::env::var("REPARTEE_BOUNCER_TEST_LEGACY").as_deref() == Ok("1");
    if legacy {
        if provider == crate::irc::bouncer::Provider::Lurker {
            config.username = Some("unrelated-user".into());
            config.password = Some(format!("{user}:fixture:password"));
        } else {
            config.bouncer_network_id = None;
            config.username = Some(format!("{user}/fixture"));
            config.password = Some("fixture:password".into());
        }
    } else {
        config.sasl_user = Some(user.clone());
        config.sasl_pass = Some("fixture-password".into());
    }
    app.setup_connection("fixture", &config);
    app.start_connection_attempt("fixture", config);
    until(&mut app, |app| app.state.connections["fixture"].status == crate::state::connection::ConnectionStatus::Connected
        && app.state.connections["fixture"].isupport_parsed.get("soju.im/FILEHOST").is_some()).await;
    assert_eq!(app.irc_handles["fixture"].bouncer_provider, Some(provider));
    assert_eq!(app.irc_handles["fixture"].sasl_authenticated, !legacy);
    app.irc_handles["fixture"].sender().send("JOIN #upload".parse::<irc::proto::Message>().unwrap()).unwrap();
    until(&mut app, |app| app.state.buffers.contains_key("fixture/#upload")).await;
    app.state.set_active_buffer("fixture/#upload");
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("fixture.txt");
    let contents = b"disposable FILEHOST fixture content\n";
    std::fs::write(&path, contents).unwrap();
    super::filehost::command(&mut app, &[path.to_str().unwrap().into(), "text/plain".into()]);
    assert!(app.upload_pending);
    let result = tokio::time::timeout(std::time::Duration::from_secs(20), app.upload_rx.recv()).await.unwrap().unwrap();
    app.finish_upload(result);
    let url = app.input.value.clone();
    assert!(url.starts_with("https://127.0.0.1:"), "native upload failed: {:?}", app.state.buffers["fixture/#upload"].messages.back().map(|message| &message.text));
    let client = crate::filehost::fixture_trust(reqwest::Client::builder()).unwrap().build().unwrap();
    let response = client.get(&url).send().await.unwrap();
    assert!(response.status().is_success());
    assert_eq!(response.bytes().await.unwrap().as_ref(), contents);
    let (response, receive) = tokio::sync::oneshot::channel();
    app.start_web_upload(crate::web::upload::Submission { buffer_id: "fixture/#upload".into(), filename: "web.txt".into(),
        content_type: "text/plain".into(), body: contents.to_vec(), response });
    let result = tokio::time::timeout(std::time::Duration::from_secs(20), app.upload_rx.recv()).await.unwrap().unwrap();
    app.finish_upload(result);
    let web_url = receive.await.unwrap().unwrap();
    assert_eq!(client.get(web_url).send().await.unwrap().bytes().await.unwrap().as_ref(), contents);
    assert_eq!(app.input.value, url);
    let conn = &app.state.connections["fixture"];
    let host = crate::filehost::Filehost::new(conn.isupport_parsed.get("soju.im/FILEHOST").unwrap(), false).unwrap();
    let rejection = host.upload(&crate::filehost::Credentials::Basic { username: user, password: "wrong-disposable-password".into() },
        "denied.txt", "text/plain", contents.to_vec()).await;
    assert!(matches!(rejection, Err(crate::filehost::Error::Http { status: 401 | 403 })));
    if let Ok(script) = std::env::var("REPARTEE_FILEHOST_BROWSER_SCRIPT") {
        super::filehost_browser_fixture::run(&mut app, &script).await;
    }
    app.cancel_connection_attempt("fixture");
    app.irc_handles.remove("fixture");
}
