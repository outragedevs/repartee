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

/// Shared state passed to all axum handlers.
pub struct AppHandle {
    pub broadcaster: Arc<WebBroadcaster>,
    pub web_cmd_tx: mpsc::UnboundedSender<(WebCommand, String)>,
    pub password: String,
    pub session_store: Arc<Mutex<SessionStore>>,
    pub rate_limiter: Arc<Mutex<RateLimiter>>,
}

#[derive(Deserialize)]
struct LoginRequest {
    password: String,
}

/// POST /api/login — authenticate and return a session token.
async fn login_handler(
    State(state): State<Arc<AppHandle>>,
    Json(body): Json<LoginRequest>,
) -> impl IntoResponse {
    // Rate limit check (use a placeholder IP for now — will be extracted from request later).
    let ip = "unknown".to_string();

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
/// When the `web-ui/dist/` directory doesn't exist (e.g. during development
/// without running trunk), this serves nothing and the fallback returns 404.
#[derive(Embed)]
#[folder = "web-ui/dist/"]
#[allow(clippy::empty_structs_with_brackets)]
struct WebAssets;

/// Serve embedded static assets from `web-ui/dist/`.
async fn static_handler(Path(path): Path<String>) -> Response {
    serve_embedded(&path)
}

/// Serve the index.html for the root path.
async fn index_handler() -> Response {
    serve_embedded("index.html")
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
        _ => "application/octet-stream",
    }
}

/// Build the axum router with all routes.
pub fn build_router(handle: Arc<AppHandle>) -> Router {
    Router::new()
        .route("/api/login", post(login_handler))
        .route("/api/health", get(health_handler))
        // WebSocket endpoint will be added in ws.rs
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
            let (stream, _peer) = match listener.accept().await {
                Ok(conn) => conn,
                Err(e) => {
                    tracing::warn!("web accept error: {e}");
                    continue;
                }
            };

            let acceptor = acceptor.clone();
            let router = router.clone();

            tokio::spawn(async move {
                let tls_stream = match acceptor.accept(stream).await {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::debug!("TLS handshake failed: {e}");
                        return;
                    }
                };

                let io = hyper_util::rt::TokioIo::new(tls_stream);
                let service = hyper_util::service::TowerToHyperService::new(router);

                if let Err(e) = hyper_util::server::conn::auto::Builder::new(
                    hyper_util::rt::TokioExecutor::new(),
                )
                .serve_connection(io, service)
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
        let (tx, _rx) = mpsc::unbounded_channel();
        Arc::new(AppHandle {
            broadcaster: Arc::new(WebBroadcaster::new(16)),
            web_cmd_tx: tx,
            password: "testpass".to_string(),
            session_store: Arc::new(Mutex::new(SessionStore::new())),
            rate_limiter: Arc::new(Mutex::new(RateLimiter::new())),
        })
    }

    #[tokio::test]
    async fn health_endpoint_returns_ok() {
        let handle = make_test_handle();
        let app = build_router(handle);

        let response = axum::http::Request::builder()
            .uri("/api/health")
            .body(axum::body::Body::empty())
            .unwrap();

        let response = tower::ServiceExt::oneshot(app, response).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn login_rejects_wrong_password() {
        let handle = make_test_handle();
        let app = build_router(handle);

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
        let handle = make_test_handle();
        let app = build_router(handle);

        let request = axum::http::Request::builder()
            .uri("/")
            .body(axum::body::Body::empty())
            .unwrap();

        let response = tower::ServiceExt::oneshot(app, request).await.unwrap();
        // Should return 200 if dist/index.html exists (placeholder or real build).
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn missing_asset_returns_404() {
        let handle = make_test_handle();
        let app = build_router(handle);

        let request = axum::http::Request::builder()
            .uri("/nonexistent.js")
            .body(axum::body::Body::empty())
            .unwrap();

        let response = tower::ServiceExt::oneshot(app, request).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn login_accepts_correct_password() {
        let handle = make_test_handle();
        let app = build_router(handle);

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
}
