use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};
use axum_extra::extract::cookie::{Cookie, SameSite};
use color_eyre::eyre::Result;
use rust_embed::Embed;
use serde::Deserialize;
use tokio::sync::{Mutex, mpsc};

use super::auth::{RateLimiter, SessionStore, session_cookie_name, verify_password};
use super::broadcast::WebBroadcaster;
use super::protocol::WebCommand;

/// Read-only snapshot of `AppState` for web handlers.
///
/// Updated periodically (1s tick) by the main event loop.
/// Web handlers read from this rather than locking `AppState` directly.
pub struct WebStateSnapshot {
    pub buffers: Vec<super::protocol::BufferMeta>,
    pub connections: Vec<super::protocol::ConnectionMeta>,
    pub mention_count: u32,
    pub active_buffer_id: Option<String>,
    pub timestamp_format: String,
}

/// Shared state passed to all axum handlers.
pub struct AppHandle {
    pub broadcaster: Arc<WebBroadcaster>,
    pub web_cmd_tx: mpsc::Sender<(WebCommand, String)>,
    pub password: String,
    pub username: String,
    pub session_store: Arc<Mutex<SessionStore>>,
    pub rate_limiter: Arc<Mutex<RateLimiter>>,
    /// `Max-Age` for the session cookie, in seconds.
    pub session_cookie_max_age: i64,
    /// Server-side preview/thumbnail extractor (`None` when `image_previews` disabled).
    pub preview_extractor: Option<Arc<super::preview::WebPreviewExtractor>>,
    /// Snapshot of `AppState` used to build `SyncInit` for newly
    /// connecting clients and to seed each session's initial active
    /// buffer. Updated eagerly on relevant events (see
    /// `App::drain_pending_web_events`); the 1 s tick is only a
    /// safety net for state that has no specific event hook.
    ///
    /// `parking_lot::RwLock` rather than `std::sync::RwLock` so a
    /// brief read from inside the async WS handler doesn't risk
    /// blocking the runtime worker under contention. We never hold
    /// the lock across an `.await`.
    pub web_state_snapshot: Option<Arc<parking_lot::RwLock<WebStateSnapshot>>>,
}

#[derive(Deserialize)]
struct LoginRequest {
    /// Sent by the client; the server only validates the password.
    /// Exists so password managers see a complete credential pair.
    #[serde(default)]
    #[expect(
        dead_code,
        reason = "field is part of wire format but intentionally ignored"
    )]
    username: Option<String>,
    password: String,
}

/// Peer address injected via Extension by the TLS accept loop.
#[derive(Debug, Clone)]
pub struct PeerAddr(pub SocketAddr);

/// POST /api/login — authenticate and set the session cookie.
async fn login_handler(
    peer: Option<axum::Extension<PeerAddr>>,
    headers: axum::http::HeaderMap,
    State(state): State<Arc<AppHandle>>,
    Json(body): Json<LoginRequest>,
) -> Response {
    let ip = peer.map_or_else(|| "unknown".to_string(), |p| p.0.0.ip().to_string());
    let user_agent = headers
        .get(axum::http::header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .chars()
        .take(256)
        .collect::<String>();

    {
        let limiter = state.rate_limiter.lock().await;
        if let Some(remaining) = limiter.check(&ip) {
            return (
                StatusCode::TOO_MANY_REQUESTS,
                Json(serde_json::json!({
                    "error": format!("rate limited, retry in {}s", remaining.as_secs())
                })),
            )
                .into_response();
        }
    }

    if verify_password(&body.password, &state.password) {
        let token = state.session_store.lock().await.create(&user_agent);
        state.rate_limiter.lock().await.record_success(&ip);
        let cookie = Cookie::build((session_cookie_name(), token))
            .http_only(true)
            .secure(true)
            .same_site(SameSite::Strict)
            .path("/")
            .max_age(time::Duration::seconds(state.session_cookie_max_age))
            .build();
        (
            StatusCode::OK,
            [(axum::http::header::SET_COOKIE, cookie.to_string())],
            Json(serde_json::json!({ "ok": true })),
        )
            .into_response()
    } else {
        state.rate_limiter.lock().await.record_failure(&ip);
        (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({ "error": "invalid password" })),
        )
            .into_response()
    }
}

/// `GET /api/login_info` — return the username the login form should pre-fill.
///
/// Unauthenticated by design: it leaks only what the user has set as the
/// "label" for password-manager autofill, never the password.
async fn login_info_handler(State(state): State<Arc<AppHandle>>) -> Response {
    Json(serde_json::json!({ "username": state.username })).into_response()
}

/// POST /api/logout — revoke the current session and clear the cookie.
async fn logout_handler(
    jar: axum_extra::extract::cookie::CookieJar,
    State(state): State<Arc<AppHandle>>,
) -> Response {
    if let Some(token) = jar.get(&session_cookie_name()) {
        state.session_store.lock().await.revoke(token.value());
    }
    let clear = Cookie::build((session_cookie_name(), ""))
        .http_only(true)
        .secure(true)
        .same_site(SameSite::Strict)
        .path("/")
        .max_age(time::Duration::seconds(0))
        .build();
    (
        StatusCode::OK,
        [(axum::http::header::SET_COOKIE, clear.to_string())],
        Json(serde_json::json!({ "ok": true })),
    )
        .into_response()
}

/// GET /api/health — simple health check.
async fn health_handler() -> impl IntoResponse {
    Json(serde_json::json!({ "status": "ok" }))
}

/// Embedded WASM frontend assets (built by `trunk build` in web-ui/).
///
/// When `static/web/` is empty (e.g. during development without running
/// `make wasm`), this serves nothing and the fallback returns 404.
#[derive(Embed)]
#[folder = "static/web/"]
struct WebAssets;

/// Serve embedded static assets from `web-ui/dist/`.
async fn static_handler(Path(path): Path<String>) -> Response {
    serve_embedded(&path)
}

/// Trunk emits content-hashed filenames like `name-abcdef0123456789.ext`
/// (8+ lowercase hex chars before the final extension). Those are
/// immutable: rebuilds produce new filenames, so old ones can be cached
/// forever without staleness risk.
fn is_hashed_asset(path: &str) -> bool {
    let Some(stem) = path.rsplit_once('.').map(|(s, _)| s) else {
        return false;
    };
    let Some(hash) = stem.rsplit_once('-').map(|(_, h)| h) else {
        return false;
    };
    hash.len() >= 8
        && hash
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

/// Serve the index.html for the root path (no-cache to pick up new hashed assets).
async fn index_handler() -> Response {
    match WebAssets::get("index.html") {
        Some(content) => (
            StatusCode::OK,
            [
                (axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8"),
                (axum::http::header::CACHE_CONTROL, "no-cache"),
            ],
            content.data.to_vec(),
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Look up a file in the embedded assets and return it with the correct MIME type.
fn serve_embedded(path: &str) -> Response {
    match WebAssets::get(path) {
        Some(content) => {
            let mime = mime_from_path(path);
            // Trunk-generated hashed filenames are content-addressed and
            // safe to cache aggressively; unhashed paths (fonts/, etc.)
            // get a short-lived cache so updates propagate within an hour.
            let cache_control = if is_hashed_asset(path) {
                "public, max-age=31536000, immutable"
            } else {
                "public, max-age=3600"
            };
            (
                StatusCode::OK,
                [
                    (axum::http::header::CONTENT_TYPE, mime),
                    (axum::http::header::CACHE_CONTROL, cache_control),
                ],
                content.data.to_vec(),
            )
                .into_response()
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Guess MIME type from file extension.
fn mime_from_path(path: &str) -> &'static str {
    match path.rsplit('.').next() {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "application/javascript",
        Some("wasm") => "application/wasm",
        Some("css") => "text/css",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("json") => "application/json",
        Some("ttf") => "font/ttf",
        Some("woff2") => "font/woff2",
        _ => "application/octet-stream",
    }
}

/// GET /favicon.ico — return 204 No Content (no favicon file).
async fn favicon_handler() -> impl IntoResponse {
    StatusCode::NO_CONTENT
}

/// GET /emotes/{file} — serve an embedded emote GIF (e.g. `usmiech.gif`).
///
/// The web frontend embeds the emote name whitelist at build time, so no JSON
/// manifest endpoint is needed — only the bytes are served here.
async fn emote_handler(Path(file): Path<String>) -> Response {
    // `file` is e.g. "usmiech.gif". Strip extension, validate against the registry.
    let name = file.strip_suffix(".gif").unwrap_or(&file);
    crate::emotes::bytes(name).map_or_else(
        || StatusCode::NOT_FOUND.into_response(),
        |data| {
            (
                StatusCode::OK,
                [
                    (axum::http::header::CONTENT_TYPE, "image/gif"),
                    (
                        axum::http::header::CACHE_CONTROL,
                        "public, max-age=31536000, immutable",
                    ),
                ],
                data.into_owned(),
            )
                .into_response()
        },
    )
}

/// Build the axum router with all routes.
pub fn build_router(handle: Arc<AppHandle>) -> Router {
    Router::new()
        .route("/api/login", post(login_handler))
        .route("/api/login_info", get(login_info_handler))
        .route("/api/logout", post(logout_handler))
        .route("/api/health", get(health_handler))
        .route("/api/preview", get(super::preview::preview_handler))
        .route("/ws", get(super::ws::ws_handler))
        .route("/favicon.ico", get(favicon_handler))
        .route("/emotes/{file}", get(emote_handler))
        .route("/", get(index_handler))
        .route("/{*path}", get(static_handler))
        .with_state(handle)
}

/// Start the HTTPS web server as a background tokio task.
///
/// Returns a `JoinHandle` that can be used to monitor the server.
pub async fn start(
    config: &crate::config::WebConfig,
    handle: Arc<AppHandle>,
) -> Result<tokio::task::JoinHandle<()>> {
    let tls_config = super::tls::load_or_generate_tls_config(&config.tls_cert, &config.tls_key)?;
    let router = build_router(handle);

    let addr = format!("{}:{}", config.bind_address, config.port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    let acceptor = tokio_rustls::TlsAcceptor::from(tls_config);

    tracing::info!("web server listening on https://{addr}");

    let join = tokio::spawn(async move {
        loop {
            let (stream, peer) = match listener.accept().await {
                Ok(conn) => conn,
                Err(e) => {
                    tracing::warn!("web accept error: {e}");
                    continue;
                }
            };

            let acceptor = acceptor.clone();
            // Clone router and add peer address as an Extension for this connection.
            let conn_router = router.clone().layer(axum::Extension(PeerAddr(peer)));

            tokio::spawn(async move {
                let tls_stream = match acceptor.accept(stream).await {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::debug!("TLS handshake failed: {e}");
                        return;
                    }
                };

                let io = hyper_util::rt::TokioIo::new(tls_stream);
                let service = hyper_util::service::TowerToHyperService::new(conn_router);

                if let Err(e) = hyper_util::server::conn::auto::Builder::new(
                    hyper_util::rt::TokioExecutor::new(),
                )
                .serve_connection_with_upgrades(io, service)
                .await
                {
                    tracing::debug!("web connection error: {e}");
                }
            });
        }
    });

    Ok(join)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_handle() -> Arc<AppHandle> {
        let (tx, _rx) = mpsc::channel(256);
        Arc::new(AppHandle {
            broadcaster: Arc::new(WebBroadcaster::new(16)),
            web_cmd_tx: tx,
            password: "testpass".to_string(),
            username: "repartee".to_string(),
            session_store: Arc::new(Mutex::new(SessionStore::with_days(vec![0u8; 32], 90))),
            rate_limiter: Arc::new(Mutex::new(RateLimiter::new())),
            session_cookie_max_age: 90 * 86_400,
            preview_extractor: None,
            web_state_snapshot: None,
        })
    }

    /// Build a test router with a fake peer address extension.
    fn test_app(handle: Arc<AppHandle>) -> Router {
        let addr: SocketAddr = "127.0.0.1:12345".parse().unwrap();
        build_router(handle).layer(axum::Extension(PeerAddr(addr)))
    }

    #[tokio::test]
    async fn emote_route_serves_gif_bytes() {
        let app = test_app(make_test_handle());
        let request = axum::http::Request::builder()
            .uri("/emotes/usmiech.gif")
            .body(axum::body::Body::empty())
            .unwrap();
        let response = tower::ServiceExt::oneshot(app, request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(axum::http::header::CONTENT_TYPE)
                .unwrap(),
            "image/gif"
        );
        let body = axum::body::to_bytes(response.into_body(), 1_000_000)
            .await
            .unwrap();
        assert!(body.starts_with(b"GIF"), "served body must be a GIF");
    }

    #[tokio::test]
    async fn emote_route_unknown_is_404() {
        let app = test_app(make_test_handle());
        let request = axum::http::Request::builder()
            .uri("/emotes/definitely_not_real.gif")
            .body(axum::body::Body::empty())
            .unwrap();
        let response = tower::ServiceExt::oneshot(app, request).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn health_endpoint_returns_ok() {
        let app = test_app(make_test_handle());

        let response = axum::http::Request::builder()
            .uri("/api/health")
            .body(axum::body::Body::empty())
            .unwrap();

        let response = tower::ServiceExt::oneshot(app, response).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn login_rejects_wrong_password() {
        let app = test_app(make_test_handle());

        let body = serde_json::json!({"password": "wrong"});
        let request = axum::http::Request::builder()
            .method("POST")
            .uri("/api/login")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(body.to_string()))
            .unwrap();

        let response = tower::ServiceExt::oneshot(app, request).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn index_serves_html() {
        let app = test_app(make_test_handle());

        let request = axum::http::Request::builder()
            .uri("/")
            .body(axum::body::Body::empty())
            .unwrap();

        let response = tower::ServiceExt::oneshot(app, request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[test]
    fn is_hashed_asset_recognises_trunk_filenames() {
        assert!(is_hashed_asset("repartee-web-8f9307fa1b2d7376.js"));
        assert!(is_hashed_asset("base-4afd40405f2555f2.css"));
        assert!(is_hashed_asset("themes-4956c8e94e9a32e3.css"));
    }

    #[test]
    fn is_hashed_asset_rejects_unhashed_paths() {
        assert!(!is_hashed_asset("index.html"));
        assert!(!is_hashed_asset("favicon.ico"));
        assert!(!is_hashed_asset("fonts/FiraCodeNerdFontMono-Regular.ttf"));
        // Short suffixes or uppercase hex are treated as not-hashed.
        assert!(!is_hashed_asset("foo-abc.js"));
        assert!(!is_hashed_asset("foo-ABCDEF12.js"));
    }

    #[tokio::test]
    async fn missing_asset_returns_404() {
        let app = test_app(make_test_handle());

        let request = axum::http::Request::builder()
            .uri("/nonexistent.js")
            .body(axum::body::Body::empty())
            .unwrap();

        let response = tower::ServiceExt::oneshot(app, request).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn login_accepts_correct_password() {
        let app = test_app(make_test_handle());

        let body = serde_json::json!({"username": "anything", "password": "testpass"});
        let request = axum::http::Request::builder()
            .method("POST")
            .uri("/api/login")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(body.to_string()))
            .unwrap();

        let response = tower::ServiceExt::oneshot(app, request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let set_cookie = response
            .headers()
            .get(axum::http::header::SET_COOKIE)
            .and_then(|v| v.to_str().ok())
            .unwrap();
        assert!(set_cookie.contains("HttpOnly"));
        assert!(set_cookie.contains("SameSite=Strict"));
        assert!(
            set_cookie.contains("Max-Age="),
            "session cookie must carry Max-Age so it survives browser restart"
        );
    }

    #[tokio::test]
    async fn login_ignores_username_field() {
        // Server only checks password — any username is accepted.
        let app = test_app(make_test_handle());
        let body = serde_json::json!({"username": "wrong-user", "password": "testpass"});
        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/api/login")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(body.to_string()))
            .unwrap();
        let resp = tower::ServiceExt::oneshot(app, req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn login_works_without_username_field() {
        // Backward-compat: missing username field still accepted.
        let app = test_app(make_test_handle());
        let body = serde_json::json!({"password": "testpass"});
        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/api/login")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(body.to_string()))
            .unwrap();
        let resp = tower::ServiceExt::oneshot(app, req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn login_info_returns_configured_username() {
        let app = test_app(make_test_handle());
        let req = axum::http::Request::builder()
            .uri("/api/login_info")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = tower::ServiceExt::oneshot(app, req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body_bytes = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        assert_eq!(json["username"].as_str(), Some("repartee"));
    }

    #[tokio::test]
    async fn rate_limiter_blocks_after_failure() {
        let handle = make_test_handle();
        let wrong = serde_json::json!({"password": "wrong"});

        // 1st attempt: no prior failures → 401 (wrong password).
        let app = test_app(Arc::clone(&handle));
        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/api/login")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(wrong.to_string()))
            .unwrap();
        let resp = tower::ServiceExt::oneshot(app, req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

        // 2nd attempt: within lockout window → 429 (rate limited).
        let app = test_app(Arc::clone(&handle));
        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/api/login")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(wrong.to_string()))
            .unwrap();
        let resp = tower::ServiceExt::oneshot(app, req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
    }
}
