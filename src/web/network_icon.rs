use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum_extra::extract::cookie::CookieJar;

use super::{PreviewQuery, validate_url_shape_str};
use crate::image_preview::fetch::{FetchConfig, FetchResult, fetch_direct_image};
use crate::web::auth::session_cookie_name;
use crate::web::server::AppHandle;

pub async fn handler(
    jar: CookieJar,
    State(state): State<Arc<AppHandle>>,
    Query(query): Query<PreviewQuery>,
) -> Response {
    let Some(token) = jar.get(&session_cookie_name()) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    if state
        .session_store
        .lock()
        .await
        .validate(token.value())
        .is_none()
    {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let Some(extractor) = state.icon_extractor.as_ref() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some(url) = extractor.lookup(&query.h) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let config = FetchConfig {
        timeout_secs: 10,
        max_file_size: 1_048_576,
        url_validator: Some(validate_url_shape_str),
    };
    fetch_direct_image(&url, &config, &extractor.http)
        .await
        .map_or_else(|_| StatusCode::BAD_GATEWAY.into_response(), image_response)
}

fn image_response(image: FetchResult) -> Response {
    let mime = image
        .content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if !matches!(
        mime.as_str(),
        "image/png"
            | "image/jpeg"
            | "image/gif"
            | "image/webp"
            | "image/avif"
            | "image/svg+xml"
            | "image/x-icon"
            | "image/vnd.microsoft.icon"
    ) {
        return StatusCode::UNSUPPORTED_MEDIA_TYPE.into_response();
    }
    (
        [
            (header::CONTENT_TYPE, mime),
            (header::CACHE_CONTROL, "private, max-age=3600".to_owned()),
            (
                header::CONTENT_SECURITY_POLICY,
                "sandbox; default-src 'none'; style-src 'unsafe-inline'; img-src data:; font-src data:".to_owned(),
            ),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_owned()),
            (header::REFERRER_POLICY, "no-referrer".to_owned()),
        ],
        image.data,
    )
        .into_response()
}

#[cfg(test)]
#[path = "network_icon_tests.rs"]
mod tests;
