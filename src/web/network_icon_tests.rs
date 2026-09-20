use super::*;
use crate::web::{
    auth::{RateLimiter, SessionStore},
    broadcast::WebBroadcaster,
    preview::WebPreviewExtractor,
};
use axum::{Router, body::Body, http::Request, routing::get};
use tower::ServiceExt;

#[tokio::test]
#[expect(clippy::too_many_lines, reason = "single disposable HTTP fixture and transport assertions")]
async fn icon_proxy_authenticates_bounds_and_sandboxes_images() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let upstream = tokio::spawn(async move {
        let app = Router::new()
            .route("/icon", get(|| async { ([(header::CONTENT_TYPE, "image/svg+xml")], "<svg xmlns='http://www.w3.org/2000/svg' width='20' height='20'><rect width='20' height='20' fill='red'/></svg>") }))
            .route("/large", get(|| async { ([(header::CONTENT_TYPE, "image/png")], vec![0_u8; 1_048_577]) }))
            .route("/html", get(|| async { ([(header::CONTENT_TYPE, "text/html")], "<html>not an icon</html>") }))
            .route("/redirect", get(|| async { (StatusCode::FOUND, [(header::LOCATION, "http://127.0.0.1/private")]) }));
        axum::serve(listener, app).await.unwrap();
    });
    let mut extractor = WebPreviewExtractor::new(vec![1; 32], 3, 10);
    extractor.http = reqwest::Client::builder()
        .no_proxy()
        .resolve("icon.fixture", address)
        .redirect(super::super::redirect_policy())
        .build()
        .unwrap();
    let sessions = Arc::new(tokio::sync::Mutex::new(SessionStore::with_days(
        vec![2; 32],
        1,
    )));
    let token = sessions.lock().await.create("icon fixture");
    let (tx, _rx) = tokio::sync::mpsc::channel(1);
    let extractor = Arc::new(extractor);
    let handle = Arc::new(AppHandle {
        broadcaster: Arc::new(WebBroadcaster::new(16)),
        web_cmd_tx: tx,
        password: "fixture".into(),
        username: "fixture".into(),
        session_store: sessions,
        rate_limiter: Arc::new(tokio::sync::Mutex::new(RateLimiter::new())),
        session_cookie_max_age: 86400,
        icon_extractor: Some(Arc::clone(&extractor)),
        preview_extractor: None,
        web_state_snapshot: None,
    });
    let router = crate::web::server::build_router(handle);
    let cookie = format!("{}={token}", session_cookie_name());
    let icon = extractor
        .register_network_icon(&format!("http://icon.fixture:{}/icon", address.port()))
        .unwrap();
    let response = router
        .clone()
        .oneshot(Request::builder().uri(&icon).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    for (path, status) in [
        ("icon", StatusCode::OK),
        ("large", StatusCode::BAD_GATEWAY),
        ("html", StatusCode::BAD_GATEWAY),
        ("redirect", StatusCode::BAD_GATEWAY),
    ] {
        let url = extractor
            .register_network_icon(&format!("http://icon.fixture:{}/{path}", address.port()))
            .unwrap();
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(url)
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), status, "{path}");
        if status == StatusCode::OK {
            assert_eq!(response.headers()[header::CONTENT_TYPE], "image/svg+xml");
            assert_eq!(
                response.headers()[header::CONTENT_SECURITY_POLICY],
                "sandbox; default-src 'none'; style-src 'unsafe-inline'; img-src data:; font-src data:"
            );
            assert_eq!(
                response.headers()[header::CACHE_CONTROL],
                "private, max-age=3600"
            );
            assert_eq!(
                response.headers()[header::X_CONTENT_TYPE_OPTIONS],
                "nosniff"
            );
        }
    }
    let response = router
        .oneshot(
            Request::builder()
                .uri("/api/network-icon?h=unknown")
                .header(header::COOKIE, cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert!(
        extractor
            .register_network_icon("http://127.0.0.1/private")
            .is_none()
    );
    upstream.abort();
}
