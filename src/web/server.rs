use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};
use color_eyre::eyre::Result;
use rust_embed::Embed;
use serde::Deserialize;
use tokio::sync::{Mutex, mpsc};

use super::auth::{RateLimiter, SessionStore, verify_password};
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
    pub session_store: Arc<Mutex<SessionStore>>,
    pub rate_limiter: Arc<Mutex<RateLimiter>>,
    /// Periodic snapshot of `AppState` for `SyncInit` / `FetchNickList`.
    pub web_state_snapshot: Option<Arc<std::sync::RwLock<WebStateSnapshot>>>,
}

#[derive(Deserialize)]
struct LoginRequest {
    password: String,
}

/// Peer address injected via Extension by the TLS accept loop.
#[derive(Debug, Clone)]
struct PeerAddr(SocketAddr);

/// POST /api/login — authenticate and return a session token.
async fn login_handler(
    peer: Option<axum::Extension<PeerAddr>>,
    State(state): State<Arc<AppHandle>>,
    Json(body): Json<LoginRequest>,
) -> impl IntoResponse {
    let ip = peer.map_or_else(|| "unknown".to_string(), |p| p.0 .0.ip().to_string());

    {
        let limiter = state.rate_limiter.lock().await;
        if let Some(remaining) = limiter.check(&ip) {
            return (
                StatusCode::TOO_MANY_REQUESTS,
                Json(serde_json::json!({
                    "error": format!("rate limited, retry in {}s", remaining.as_secs())
                })),
            );
        }
    }

    if verify_password(&body.password, &state.password) {
        let token = state.session_store.lock().await.create(&ip);
        state.rate_limiter.lock().await.record_success(&ip);
        (
            StatusCode::OK,
            Json(serde_json::json!({ "token": token })),
        )
    } else {
        state.rate_limiter.lock().await.record_failure(&ip);
        (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({ "error": "invalid password" })),
        )
    }
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
            (
                StatusCode::OK,
                [(axum::http::header::CONTENT_TYPE, mime)],
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

/// Build the axum router with all routes.
pub fn build_router(handle: Arc<AppHandle>) -> Router {
    Router::new()
        .route("/api/login", post(login_handler))
        .route("/api/health", get(health_handler))
        .route("/ws", get(super::ws::ws_handler))
        .route("/favicon.ico", get(favicon_handler))
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
            session_store: Arc::new(Mutex::new(SessionStore::with_hours(24))),
            rate_limiter: Arc::new(Mutex::new(RateLimiter::new())),
            web_state_snapshot: None,
        })
    }

    /// Build a test router with a fake peer address extension.
    fn test_app(handle: Arc<AppHandle>) -> Router {
        let addr: SocketAddr = "127.0.0.1:12345".parse().unwrap();
        build_router(handle).layer(axum::Extension(PeerAddr(addr)))
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

        let body = serde_json::json!({"password": "testpass"});
        let request = axum::http::Request::builder()
            .method("POST")
            .uri("/api/login")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(body.to_string()))
            .unwrap();

        let response = tower::ServiceExt::oneshot(app, request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
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
