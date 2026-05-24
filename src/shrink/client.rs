//! HTTP wrapper around the shrink API.
//!
//! Single endpoint we care about: `POST {api_url}/api/links` with the
//! body `{"url": "..."}` and the `X-API-Key` header. Response shape:
//! `{"slug":"...","url":"...","short_url":"..."}`. We don't pass a
//! `slug` field — the spec says the API decides.

use std::time::Duration;

use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::UrlShortening;

#[derive(Debug, Error)]
pub enum ShrinkError {
    /// Shortener is disabled or the API key isn't configured —
    /// callers should treat this as "skip" rather than surface
    /// anything to the user.
    #[error("shrink disabled (no API key)")]
    Disabled,
    /// The HTTP call did not complete in the allotted time.
    #[error("timeout")]
    Timeout,
    /// Network-level failure (DNS, TLS, connection reset, etc.).
    #[error("network: {0}")]
    Network(String),
    /// The API responded but with a non-2xx status.
    #[error("api status {status}: {body}")]
    Api { status: u16, body: String },
    /// Response was 2xx but the JSON body was missing required fields.
    #[error("invalid response: {0}")]
    InvalidResponse(String),
}

#[derive(Serialize)]
struct CreateLinkRequest<'a> {
    url: &'a str,
}

#[derive(Deserialize)]
struct CreateLinkResponse {
    #[serde(default)]
    slug: String,
    #[serde(default)]
    short_url: String,
}

/// Stateless HTTP client for the shrink API. Cheap to clone — the
/// underlying `reqwest::Client` is `Arc`-ed internally and pools
/// connections per host.
#[derive(Clone)]
pub struct ShrinkClient {
    http: Client,
    api_url: String,
    api_key: String,
}

impl ShrinkClient {
    /// Build a client. `api_url` is the base (no `/api/links`
    /// suffix); `api_key` empty disables every subsequent call with
    /// `ShrinkError::Disabled` so callers can short-circuit cleanly.
    #[must_use]
    pub fn new(api_url: String, api_key: String) -> Self {
        // Default reqwest client honours each call's per-request
        // timeout via `RequestBuilder::timeout`; no global timeout
        // here so the caller controls latency budget.
        let http = Client::builder()
            .user_agent(concat!(
                env!("CARGO_PKG_NAME"),
                "/",
                env!("CARGO_PKG_VERSION")
            ))
            .build()
            .unwrap_or_default();
        Self {
            http,
            api_url: api_url.trim_end_matches('/').to_string(),
            api_key,
        }
    }

    /// Shorten one URL. Returns the original URL paired with the
    /// shortened form so the caller can build a render-time
    /// substitution table without re-threading the input string.
    ///
    /// `timeout` is the wall-clock budget for this single call —
    /// outgoing flows pass a short window (default 2 s) because the
    /// user is blocked on it; incoming uses the same window but runs
    /// in the background.
    pub async fn shorten(
        &self,
        url: &str,
        timeout: Duration,
    ) -> Result<UrlShortening, ShrinkError> {
        if self.api_key.is_empty() {
            return Err(ShrinkError::Disabled);
        }
        let endpoint = format!("{}/api/links", self.api_url);
        let body = CreateLinkRequest { url };

        let send = self
            .http
            .post(&endpoint)
            .header("X-API-Key", &self.api_key)
            .json(&body)
            .send();

        let resp = match tokio::time::timeout(timeout, send).await {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => return Err(ShrinkError::Network(e.to_string())),
            Err(_) => return Err(ShrinkError::Timeout),
        };

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ShrinkError::Api {
                status: status.as_u16(),
                body,
            });
        }

        let parsed: CreateLinkResponse = match resp.json().await {
            Ok(p) => p,
            Err(e) => return Err(ShrinkError::InvalidResponse(e.to_string())),
        };

        if parsed.short_url.is_empty() || parsed.slug.is_empty() {
            return Err(ShrinkError::InvalidResponse(
                "missing slug or short_url in response".into(),
            ));
        }

        Ok(UrlShortening {
            original: url.to_string(),
            shortened: parsed.short_url,
        })
    }

    /// 409 from the API means the slug we asked for is already taken.
    /// We never pass a slug, so this should not happen in practice,
    /// but the matcher is useful for tests and for a future `/shrink
    /// --slug=` extension.
    #[must_use]
    pub fn is_slug_conflict(err: &ShrinkError) -> bool {
        matches!(
            err,
            ShrinkError::Api {
                status: 409,
                ..
            }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_when_key_empty() {
        let c = ShrinkClient::new("https://shr.al".into(), String::new());
        // Build a tokio runtime just to drive the future.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let err = rt
            .block_on(c.shorten("https://x.com/long", Duration::from_secs(1)))
            .unwrap_err();
        assert!(matches!(err, ShrinkError::Disabled));
    }

    #[test]
    fn api_url_trailing_slash_stripped() {
        let c = ShrinkClient::new("https://shr.al/".into(), "k".into());
        // Internal field check via private access from the same module:
        // we trust the constructor to keep this normalised so the
        // request endpoint stays `https://shr.al/api/links`.
        assert_eq!(c.api_url, "https://shr.al");
    }

    #[test]
    fn slug_conflict_matcher() {
        let err = ShrinkError::Api {
            status: 409,
            body: "slug already exists".into(),
        };
        assert!(ShrinkClient::is_slug_conflict(&err));
        let other = ShrinkError::Api {
            status: 400,
            body: "bad url".into(),
        };
        assert!(!ShrinkClient::is_slug_conflict(&other));
    }
}
