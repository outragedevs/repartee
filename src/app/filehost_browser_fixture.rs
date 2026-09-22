use std::sync::Arc;

use super::App;

pub async fn run(app: &mut App, script: &str) {
    run_with_tick(app, script, |_| {}).await;
}

pub async fn run_with_tick(app: &mut App, script: &str, tick: fn(&mut App)) {
    use crate::web::{auth::{SessionStore, RateLimiter}, server::{AppHandle, WebStateSnapshot}};
    let sessions = Arc::new(tokio::sync::Mutex::new(SessionStore::with_days(vec![0; 32], 1)));
    let cookie = sessions.lock().await.create("disposable browser fixture");
    let snapshot = Arc::new(parking_lot::RwLock::new(WebStateSnapshot {
        buffers: Vec::new(), connections: Vec::new(), mention_count: 0,
        active_buffer_id: None, timestamp_format: "%H:%M".into(), emotes_enabled: false, emotes_input_enabled: false,
        typing: std::collections::HashMap::new(), statusbar_items: Vec::new(), statusbar_enabled: false,
        keyboard: crate::keybindings::KeyboardConfig::default(),
    }));
    app.web_state_snapshot = Some(Arc::clone(&snapshot));
    app.refresh_web_state_snapshot();
    let handle = Arc::new(AppHandle {
        broadcaster: Arc::clone(&app.web_broadcaster), web_cmd_tx: app.web_cmd_tx.clone(),
        password: "disposable-fixture".into(), username: "fixture".into(),
        session_store: sessions, rate_limiter: Arc::new(tokio::sync::Mutex::new(RateLimiter::new())),
        session_cookie_max_age: 86_400, icon_extractor: None, preview_extractor: None, web_state_snapshot: Some(snapshot),
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, crate::web::server::build_router(handle)).await.unwrap();
    });
    let mut browser = tokio::process::Command::new("node").arg(script)
        .env("REPARTEE_BROWSER_FIXTURE_URL", format!("http://{address}"))
        .env("REPARTEE_BROWSER_FIXTURE_STORAGE_KEY", format!("{}-session", crate::constants::APP_NAME))
        .env("REPARTEE_BROWSER_FIXTURE_COOKIE", cookie)
        .env("REPARTEE_BROWSER_FIXTURE_COOKIE_NAME", crate::web::auth::session_cookie_name())
        .kill_on_drop(true).spawn().unwrap();
    let outcome = tokio::time::timeout(std::time::Duration::from_mins(2), async {
        loop {
            while let Ok(event) = app.irc_rx.try_recv() { app.handle_irc_event(event); }
            while let Ok((command, session)) = app.web_cmd_rx.try_recv() { app.handle_web_command(command, &session); }
            while let Ok(result) = app.upload_rx.try_recv() { app.finish_upload(result); }
            app.tick_bouncer_webpush();
            tick(app);
            app.drain_pending_web_events();
            if let Some(status) = browser.try_wait().unwrap() { return status; }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }).await;
    server.abort();
    assert!(outcome.expect("browser fixture timed out").success());
}
