use std::{sync::Arc, time::Duration};

use axum::{body::to_bytes, extract::{Request, State}, http::StatusCode, response::{IntoResponse, Response}, Json};
use axum_extra::extract::cookie::CookieJar;
use tokio::sync::{Semaphore, broadcast::error::RecvError};

use super::{auth::session_cookie_name, protocol::{WebCommand, WebEvent}, server::AppHandle};
use crate::irc::webpush::WebRequest;

static SLOTS: Semaphore = Semaphore::const_new(16);

pub async fn handler(jar: CookieJar, State(state): State<Arc<AppHandle>>, request: Request) -> Response {
    let Some(token) = jar.get(&session_cookie_name()) else { return StatusCode::UNAUTHORIZED.into_response(); };
    if state.session_store.lock().await.validate(token.value()).is_none() { return StatusCode::UNAUTHORIZED.into_response(); }
    if request.headers().get("X-Push-Intent").is_none_or(|value| value != "1") { return StatusCode::FORBIDDEN.into_response(); }
    let Ok(_permit) = SLOTS.try_acquire() else { return StatusCode::SERVICE_UNAVAILABLE.into_response(); };
    let Ok(Ok(bytes)) = tokio::time::timeout(Duration::from_secs(5), to_bytes(request.into_body(), 8192)).await else {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    };
    let Ok(command) = serde_json::from_slice::<WebRequest>(&bytes) else { return StatusCode::BAD_REQUEST.into_response(); };
    let session_id = uuid::Uuid::new_v4().to_string();
    let request_id = command.request_id.clone();
    let mut receiver = state.broadcaster.subscribe();
    if state.web_cmd_tx.try_send((WebCommand::WebPush(Box::new(command)), session_id.clone())).is_err() {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    let result = tokio::time::timeout(Duration::from_secs(35), async {
        loop {
            match receiver.recv().await {
                Ok(event @ WebEvent::WebPush { .. }) if matches!(&event, WebEvent::WebPush { session_id: owner, request_id: correlation, .. } if owner == &session_id && correlation == &request_id) => return Some(event),
                Ok(_) | Err(RecvError::Lagged(_)) => {},
                Err(RecvError::Closed) => return None,
            }
        }
    }).await;
    match result {
        Ok(Some(event)) => ([(axum::http::header::CACHE_CONTROL, "no-store")], Json(event)).into_response(),
        _ => (StatusCode::GATEWAY_TIMEOUT, "WebPush outcome unknown").into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request as HttpRequest};
    use axum_extra::extract::cookie::Cookie;
    use tokio::sync::{Mutex, mpsc};
    use crate::web::{auth::{RateLimiter, SessionStore}, broadcast::WebBroadcaster};
    use crate::irc::webpush::Status;

    fn setup() -> (Arc<AppHandle>, mpsc::Receiver<(WebCommand, String)>) {
        let (tx, rx) = mpsc::channel(8);
        (Arc::new(AppHandle {
            broadcaster: Arc::new(WebBroadcaster::new(16)), web_cmd_tx: tx,
            password: String::new(), username: String::new(),
            session_store: Arc::new(Mutex::new(SessionStore::with_days(vec![0; 32], 1))),
            rate_limiter: Arc::new(Mutex::new(RateLimiter::new())), session_cookie_max_age: 86400,
            icon_extractor: None, preview_extractor: None, web_state_snapshot: None,
        }), rx)
    }

    fn body(intent: bool) -> Request {
        let mut builder = HttpRequest::builder().method("POST");
        if intent { builder = builder.header("X-Push-Intent", "1"); }
        builder.body(Body::from(r#"{"connection_id":"fixture","request_id":"00000000-0000-0000-0000-000000000001","action":"Get"}"#)).unwrap()
    }

    #[tokio::test]
    async fn endpoint_requires_valid_session_and_explicit_intent_before_dispatch() {
        let (state, mut receive) = setup();
        assert_eq!(handler(CookieJar::new(), State(state.clone()), body(true)).await.status(), StatusCode::UNAUTHORIZED);
        let token = state.session_store.lock().await.create("fixture");
        let jar = CookieJar::new().add(Cookie::new(session_cookie_name(), token));
        assert_eq!(handler(jar, State(state), body(false)).await.status(), StatusCode::FORBIDDEN);
        assert!(receive.try_recv().is_err());
    }

    #[tokio::test]
    async fn endpoint_returns_only_its_correlated_response_without_caching() {
        let (state, mut receive) = setup();
        let token = state.session_store.lock().await.create("fixture");
        let jar = CookieJar::new().add(Cookie::new(session_cookie_name(), token));
        let app = state.clone();
        let reply = tokio::spawn(async move {
            let (WebCommand::WebPush(request), session_id) = receive.recv().await.unwrap() else { panic!("wrong command") };
            for owner in ["other".to_string(), session_id] {
                let _ = app.broadcaster.send(WebEvent::WebPush {
                    connection_id: "fixture".into(), request_id: request.request_id.clone(), session_id: owner,
                    status: Status::Ready, scope: Some("scope".into()), vapid: None, context: None,
                });
            }
        });
        let response = handler(jar, State(state), body(true)).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["cache-control"], "no-store");
        let bytes = to_bytes(response.into_body(), 4096).await.unwrap();
        let event: WebEvent = serde_json::from_slice(&bytes).unwrap();
        assert!(matches!(event, WebEvent::WebPush { session_id, .. } if session_id != "other"));
        reply.await.unwrap();
    }
}
