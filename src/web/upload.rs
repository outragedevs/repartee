use std::sync::{Arc, Mutex};

use axum::{body::to_bytes, extract::{Query, Request, State}, http::StatusCode, response::{IntoResponse, Response}};
use axum_extra::extract::cookie::CookieJar;
use tokio::sync::{Semaphore, oneshot};

use super::{auth::session_cookie_name, protocol::WebCommand, server::AppHandle};

static UPLOAD_SLOT: Semaphore = Semaphore::const_new(1);

pub struct Submission {
    pub buffer_id: String,
    pub filename: String,
    pub content_type: String,
    pub body: Vec<u8>,
    pub response: oneshot::Sender<Result<String, String>>,
}

impl std::fmt::Debug for Submission {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("UploadSubmission")
    }
}

#[derive(serde::Deserialize)]
pub struct Metadata {
    buffer_id: String,
    filename: String,
}

pub async fn handler(
    jar: CookieJar,
    State(state): State<Arc<AppHandle>>,
    Query(metadata): Query<Metadata>,
    request: Request,
) -> Response {
    let Some(token) = jar.get(&session_cookie_name()) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    if state.session_store.lock().await.validate(token.value()).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if request.headers().get("X-Upload-Intent").is_none_or(|value| value != "1") {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Ok(_permit) = UPLOAD_SLOT.try_acquire() else {
        return (StatusCode::CONFLICT, "An upload is already running").into_response();
    };
    let content_type = request.headers().get("Content-Type").and_then(|value| value.to_str().ok())
        .unwrap_or("application/octet-stream").to_string();
    let body = match tokio::time::timeout(std::time::Duration::from_secs(30),
        to_bytes(request.into_body(), crate::filehost::MAX_UPLOAD_BYTES)).await {
        Ok(Ok(body)) => body,
        Ok(Err(_)) => return StatusCode::PAYLOAD_TOO_LARGE.into_response(),
        Err(_) => return StatusCode::REQUEST_TIMEOUT.into_response(),
    };
    let (response, receive) = oneshot::channel();
    let submission = Submission { buffer_id: metadata.buffer_id, filename: metadata.filename,
        content_type, body: body.to_vec(), response };
    let command = WebCommand::UploadFile { submission: Arc::new(Mutex::new(Some(submission))) };
    if state.web_cmd_tx.try_send((command, "http-upload".into())).is_err() {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    match tokio::time::timeout(std::time::Duration::from_secs(150), receive).await {
        Ok(Ok(Ok(url))) => (StatusCode::CREATED, url).into_response(),
        Ok(Ok(Err(error))) => (StatusCode::BAD_REQUEST, error).into_response(),
        _ => (StatusCode::GATEWAY_TIMEOUT, "Upload outcome is unknown; do not automatically retry").into_response(),
    }
}
