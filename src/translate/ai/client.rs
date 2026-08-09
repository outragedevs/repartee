use std::sync::Mutex;
use std::time::{Duration, Instant};

use regex::Regex;
use reqwest::header::HeaderMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::TranslateAiModelConfig;

static DAILY_LIMIT: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
    Regex::new(r"(?i)per\s*day|\b(?:TPD|RPD)\b|daily\s+(?:limit|quota)|limit:\s*0\b")
        .expect("valid regex")
});
static THINK: std::sync::LazyLock<Regex> =
    std::sync::LazyLock::new(|| Regex::new(r"(?is)<think>.*?</think>\s*").expect("valid regex"));
static THINK_OPEN: std::sync::LazyLock<Regex> =
    std::sync::LazyLock::new(|| Regex::new(r"(?is)<think>.*\z").expect("valid regex"));

const MAX_RESPONSE_BYTES: usize = 64 * 1024;
const RETRY_AFTER_OVERFLOW_PENALTY: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    DailyLimit,
    Permanent,
    Transient,
    InvalidResponse,
}

#[derive(Debug)]
pub struct AttemptFailure {
    pub kind: FailureKind,
    pub message: String,
}

pub struct ModelClient {
    http: reqwest::Client,
    config: TranslateAiModelConfig,
    state: Mutex<ClientState>,
    limiter: RateLimiter,
}

struct ClientState {
    api_key: String,
    generation: u64,
    disabled: Option<FailureKind>,
    supports_temperature: bool,
}

struct Credential {
    api_key: String,
    generation: u64,
}

impl ModelClient {
    pub fn new(http: reqwest::Client, mut config: TranslateAiModelConfig) -> Self {
        let api_key = std::mem::take(&mut config.api_key);
        Self {
            limiter: RateLimiter::new(config.rpm, config.tpm),
            http,
            config,
            state: Mutex::new(ClientState {
                api_key,
                generation: 0,
                disabled: None,
                supports_temperature: true,
            }),
        }
    }

    pub fn name(&self) -> &str {
        &self.config.name
    }

    pub fn is_available(&self) -> bool {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        !state.api_key.is_empty() && state.disabled.is_none()
    }

    pub fn refresh_api_key(&self, api_key: &str) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.api_key != api_key {
            state.api_key.clear();
            state.api_key.push_str(api_key);
            state.generation = state.generation.wrapping_add(1);
            state.disabled = None;
        }
    }

    pub async fn translate(&self, system: &str, user: &str) -> Result<String, AttemptFailure> {
        let mut body = ChatRequest {
            model: &self.config.model,
            temperature: self.temperature(),
            max_tokens: self.config.max_output_tokens.max(1),
            messages: [
                ChatMessage {
                    role: "system",
                    content: system,
                },
                ChatMessage {
                    role: "user",
                    content: user,
                },
            ],
            reasoning_effort: self.config.reasoning_effort.as_deref(),
            provider: self.config.provider.as_ref(),
        };
        let estimate = estimate_tokens(system)
            .saturating_add(estimate_tokens(user))
            .saturating_add(self.config.max_output_tokens.max(1));
        let mut retries = 0;

        loop {
            self.limiter.acquire(estimate).await?;
            let credential = self.credential()?;
            let response = self
                .http
                .post(format!(
                    "{}/chat/completions",
                    self.config.base_url.trim().trim_end_matches('/')
                ))
                .bearer_auth(&credential.api_key)
                .json(&body)
                .send()
                .await;

            let response = match response {
                Ok(response) => response,
                Err(error) => {
                    if retries >= self.config.max_retries {
                        return Err(AttemptFailure {
                            kind: FailureKind::Transient,
                            message: error.to_string(),
                        });
                    }
                    retries += 1;
                    tokio::time::sleep(backoff(retries)).await;
                    continue;
                }
            };

            let status = response.status();
            let headers = response.headers().clone();
            self.limiter.sync(&headers);
            let response_body = read_response_body(response).await?;

            if status.is_success() {
                return parse_success_body(&response_body);
            }
            let raw = response_body.text;

            let status_code = status.as_u16();
            if status_code == 400
                && body.temperature.is_some()
                && raw.to_ascii_lowercase().contains("temperature")
            {
                self.disable_temperature();
                body.temperature = None;
                continue;
            }
            if matches!(status_code, 401..=403) {
                if self.disable_if_current(FailureKind::Permanent, credential.generation) {
                    return Err(failure(FailureKind::Permanent, status_code, &raw));
                }
                continue;
            }
            if status_code == 429 && DAILY_LIMIT.is_match(&raw) {
                if self.disable_if_current(FailureKind::DailyLimit, credential.generation) {
                    return Err(failure(FailureKind::DailyLimit, status_code, &raw));
                }
                continue;
            }
            if status_code == 429 || status.is_server_error() {
                let next_retry = retries.saturating_add(1);
                let delay = retry_after(&headers).unwrap_or_else(|| backoff(next_retry));
                if status_code == 429 {
                    self.limiter.penalize(delay);
                }
                if retries >= self.config.max_retries {
                    return Err(failure(FailureKind::Transient, status_code, &raw));
                }
                retries = next_retry;
                if status_code != 429 {
                    tokio::time::sleep(delay).await;
                }
                continue;
            }
            return Err(failure(FailureKind::Permanent, status_code, &raw));
        }
    }

    fn credential(&self) -> Result<Credential, AttemptFailure> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(kind) = state.disabled {
            return Err(AttemptFailure {
                kind,
                message: "model disabled for this session".to_string(),
            });
        }
        if state.api_key.is_empty() {
            return Err(AttemptFailure {
                kind: FailureKind::Permanent,
                message: "model has no API key".to_string(),
            });
        }
        Ok(Credential {
            api_key: state.api_key.clone(),
            generation: state.generation,
        })
    }

    fn disable_if_current(&self, kind: FailureKind, generation: u64) -> bool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.generation != generation {
            return false;
        }
        state.disabled = Some(kind);
        true
    }

    fn temperature(&self) -> Option<f32> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.supports_temperature.then_some(0.0)
    }

    fn disable_temperature(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.supports_temperature = false;
    }
}

struct ResponseBody {
    text: String,
    exceeded_limit: bool,
}

fn parse_success_body(response: &ResponseBody) -> Result<String, AttemptFailure> {
    if response.exceeded_limit {
        return Err(AttemptFailure {
            kind: FailureKind::InvalidResponse,
            message: format!("response body exceeds the {MAX_RESPONSE_BYTES}-byte limit"),
        });
    }
    parse_reply(&response.text)
}

async fn read_response_body(
    mut response: reqwest::Response,
) -> Result<ResponseBody, AttemptFailure> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
    {
        return Ok(ResponseBody {
            text: String::new(),
            exceeded_limit: true,
        });
    }

    let capacity = response
        .content_length()
        .and_then(|length| usize::try_from(length).ok())
        .unwrap_or_default()
        .min(MAX_RESPONSE_BYTES);
    let mut bytes = Vec::with_capacity(capacity);
    while let Some(chunk) = response.chunk().await.map_err(|error| AttemptFailure {
        kind: FailureKind::InvalidResponse,
        message: error.to_string(),
    })? {
        let remaining = MAX_RESPONSE_BYTES.saturating_sub(bytes.len());
        if chunk.len() > remaining {
            bytes.extend_from_slice(&chunk[..remaining]);
            return Ok(ResponseBody {
                text: String::from_utf8_lossy(&bytes).into_owned(),
                exceeded_limit: true,
            });
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(ResponseBody {
        text: String::from_utf8_lossy(&bytes).into_owned(),
        exceeded_limit: false,
    })
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    max_tokens: u32,
    messages: [ChatMessage<'a>; 2],
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    provider: Option<&'a crate::config::TranslateAiProviderConfig>,
}

#[derive(Serialize)]
struct ChatMessage<'a> {
    role: &'static str,
    content: &'a str,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    message: ResponseMessage,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct ResponseMessage {
    content: Option<Value>,
}

fn parse_reply(raw: &str) -> Result<String, AttemptFailure> {
    let parsed: ChatResponse = serde_json::from_str(raw).map_err(|error| AttemptFailure {
        kind: FailureKind::InvalidResponse,
        message: error.to_string(),
    })?;
    let choice = parsed
        .choices
        .into_iter()
        .next()
        .ok_or_else(|| AttemptFailure {
            kind: FailureKind::InvalidResponse,
            message: "response has no choices".to_string(),
        })?;
    if let Some(reason) = choice.finish_reason.as_deref()
        && reason != "stop"
    {
        return Err(AttemptFailure {
            kind: FailureKind::InvalidResponse,
            message: format!("response ended with finish reason {reason}"),
        });
    }
    let content = choice
        .message
        .content
        .and_then(|value| value.as_str().map(str::to_string))
        .ok_or_else(|| AttemptFailure {
            kind: FailureKind::InvalidResponse,
            message: "response has no text content".to_string(),
        })?;
    let content = THINK.replace_all(&content, "");
    Ok(THINK_OPEN.replace_all(&content, "").trim().to_string())
}

fn failure(kind: FailureKind, status: u16, body: &str) -> AttemptFailure {
    AttemptFailure {
        kind,
        message: format!(
            "HTTP {status}: {}",
            body.chars().take(400).collect::<String>()
        ),
    }
}

fn estimate_tokens(text: &str) -> u32 {
    u32::try_from(text.chars().count().div_ceil(3)).unwrap_or(u32::MAX)
}

fn backoff(retry: u32) -> Duration {
    let exponent = retry.saturating_sub(1).min(5);
    Duration::from_millis((1_u64 << exponent) * 250)
}

fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| retry_after_value(value, chrono::Utc::now()))
}

fn retry_after_value(value: &str, now: chrono::DateTime<chrono::Utc>) -> Option<Duration> {
    let value = value.trim();
    if let Some(seconds) = value
        .parse::<f64>()
        .ok()
        .filter(|seconds| seconds.is_finite() && *seconds >= 0.0)
    {
        return Duration::try_from_secs_f64(seconds).ok();
    }
    let deadline = chrono::DateTime::parse_from_rfc2822(value)
        .ok()?
        .with_timezone(&chrono::Utc);
    Some(
        deadline
            .signed_duration_since(now)
            .to_std()
            .unwrap_or(Duration::ZERO),
    )
}

struct RateLimiter {
    rpm: f64,
    tpm: f64,
    state: Mutex<Bucket>,
}

struct Bucket {
    requests: f64,
    tokens: f64,
    last: Instant,
    blocked_until: Instant,
}

impl RateLimiter {
    fn new(rpm: u32, tpm: u32) -> Self {
        let now = Instant::now();
        Self {
            rpm: f64::from(rpm),
            tpm: f64::from(tpm),
            state: Mutex::new(Bucket {
                requests: f64::from(rpm),
                tokens: f64::from(tpm),
                last: now,
                blocked_until: now,
            }),
        }
    }

    async fn acquire(&self, estimated_tokens: u32) -> Result<(), AttemptFailure> {
        let needed = f64::from(estimated_tokens);
        if self.tpm > 0.0 && needed > self.tpm {
            return Err(AttemptFailure {
                kind: FailureKind::Permanent,
                message: format!(
                    "estimated request cost of {estimated_tokens} tokens exceeds configured TPM limit of {}",
                    self.tpm
                ),
            });
        }
        loop {
            let wait = {
                let mut state = self
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let now = Instant::now();
                let elapsed = now.saturating_duration_since(state.last).as_secs_f64();
                state.last = now;
                if self.rpm > 0.0 {
                    state.requests = (state.requests + elapsed * self.rpm / 60.0).min(self.rpm);
                }
                if self.tpm > 0.0 {
                    state.tokens = (state.tokens + elapsed * self.tpm / 60.0).min(self.tpm);
                }
                if now < state.blocked_until {
                    state
                        .blocked_until
                        .saturating_duration_since(now)
                        .min(Duration::from_secs(10))
                } else if (self.rpm == 0.0 || state.requests >= 1.0)
                    && (self.tpm == 0.0 || state.tokens >= needed)
                {
                    if self.rpm > 0.0 {
                        state.requests -= 1.0;
                    }
                    if self.tpm > 0.0 {
                        state.tokens -= needed;
                    }
                    return Ok(());
                } else {
                    let request_wait = if self.rpm > 0.0 && state.requests < 1.0 {
                        (1.0 - state.requests) * 60.0 / self.rpm
                    } else {
                        0.0
                    };
                    let token_wait = if self.tpm > 0.0 && state.tokens < needed {
                        (needed - state.tokens) * 60.0 / self.tpm
                    } else {
                        0.0
                    };
                    Duration::from_secs_f64(request_wait.max(token_wait).clamp(0.05, 10.0))
                }
            };
            tokio::time::sleep(wait).await;
        }
    }

    fn sync(&self, headers: &HeaderMap) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.rpm > 0.0
            && let Some(value) = remaining(headers, "x-ratelimit-remaining-requests")
        {
            state.requests = state.requests.min(value);
        }
        if self.tpm > 0.0
            && let Some(value) = remaining(headers, "x-ratelimit-remaining-tokens")
        {
            state.tokens = state.tokens.min(value);
        }
    }

    fn penalize(&self, duration: Duration) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = Instant::now();
        let deadline = now
            .checked_add(duration)
            .or_else(|| now.checked_add(RETRY_AFTER_OVERFLOW_PENALTY))
            .unwrap_or(now);
        state.blocked_until = state.blocked_until.max(deadline);
    }
}

fn remaining(headers: &HeaderMap, name: &'static str) -> Option<f64> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::convert::Infallible;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::body::Body;
    use axum::extract::State;
    use axum::http::{HeaderMap as AxumHeaderMap, StatusCode, header};
    use axum::response::{IntoResponse, Response};
    use axum::routing::post;
    use axum::{Json, Router};
    use serde_json::json;
    use tokio::sync::Notify;

    struct RotationState {
        old_key_seen: Notify,
        release_old_request: Notify,
    }

    struct TemperatureState {
        requests: AtomicUsize,
    }

    #[test]
    fn strips_a_think_block_from_content() {
        let raw = r#"{"choices":[{"message":{"content":"<think>secret</think> dzień dobry"},"finish_reason":"stop"}]}"#;
        assert_eq!(parse_reply(raw).unwrap(), "dzień dobry");
    }

    #[test]
    fn rejects_a_truncated_answer() {
        let raw = r#"{"choices":[{"message":{"content":"dzień"},"finish_reason":"length"}]}"#;
        assert_eq!(
            parse_reply(raw).unwrap_err().kind,
            FailureKind::InvalidResponse
        );
    }

    #[test]
    fn rejects_every_non_stop_finish_reason() {
        for reason in ["content_filter", "tool_calls", "provider_error"] {
            let raw = format!(
                r#"{{"choices":[{{"message":{{"content":"częściowa odpowiedź"}},"finish_reason":"{reason}"}}]}}"#
            );
            assert_eq!(
                parse_reply(&raw).unwrap_err().kind,
                FailureKind::InvalidResponse
            );
        }
    }

    #[test]
    fn accepts_a_response_without_a_finish_reason() {
        let raw = r#"{"choices":[{"message":{"content":"dzień dobry"}}]}"#;
        assert_eq!(parse_reply(raw).unwrap(), "dzień dobry");
    }

    #[test]
    fn parses_retry_after_http_date() {
        let now = chrono::DateTime::parse_from_rfc2822("Sun, 06 Nov 1994 08:49:35 GMT")
            .expect("valid date")
            .with_timezone(&chrono::Utc);
        assert_eq!(
            retry_after_value("Sun, 06 Nov 1994 08:49:37 GMT", now),
            Some(Duration::from_secs(2))
        );
    }

    #[test]
    fn rejects_an_unrepresentable_numeric_retry_after() {
        assert_eq!(retry_after_value("1e300", chrono::Utc::now()), None);
    }

    #[tokio::test]
    async fn low_rate_limiter_allows_the_first_request_immediately() {
        let limiter = RateLimiter::new(1, 4);
        tokio::time::timeout(Duration::from_millis(100), limiter.acquire(4))
            .await
            .expect("initial rate-limit credit should cover one request")
            .expect("request should fit within the token limit");
    }

    #[tokio::test]
    async fn rejects_a_request_larger_than_the_tpm_limit() {
        let limiter = RateLimiter::new(1, 100);
        let error = limiter
            .acquire(1_000)
            .await
            .expect_err("oversized request must not reach the provider");

        assert_eq!(error.kind, FailureKind::Permanent);
        assert!(error.message.contains("1000 tokens"));
    }

    #[tokio::test]
    async fn remembers_that_a_provider_rejects_temperature() {
        let state = Arc::new(TemperatureState {
            requests: AtomicUsize::new(0),
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test server");
        let address = listener.local_addr().expect("test server address");
        let router = Router::new()
            .route("/v1/chat/completions", post(temperature_response))
            .with_state(Arc::clone(&state));
        let server = tokio::spawn(async move {
            axum::serve(listener, router)
                .await
                .expect("serve test requests");
        });
        let client = ModelClient::new(
            reqwest::Client::new(),
            TranslateAiModelConfig {
                name: "temperature-test".to_string(),
                base_url: format!("http://{address}/v1"),
                model: "test-model".to_string(),
                api_key: "secret".to_string(),
                max_retries: 0,
                ..TranslateAiModelConfig::default()
            },
        );

        client
            .translate("translate", "hallo")
            .await
            .expect("compatibility retry should succeed");
        client
            .translate("translate", "noch einmal")
            .await
            .expect("subsequent request should omit temperature");
        server.abort();

        assert_eq!(state.requests.load(Ordering::Relaxed), 3);
    }

    #[tokio::test]
    async fn final_rate_limit_response_penalizes_the_shared_limiter() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test server");
        let address = listener.local_addr().expect("test server address");
        let router = Router::new().route("/v1/chat/completions", post(rate_limited_response));
        let server = tokio::spawn(async move {
            axum::serve(listener, router)
                .await
                .expect("serve test requests");
        });
        let client = ModelClient::new(
            reqwest::Client::new(),
            TranslateAiModelConfig {
                name: "final-rate-limit-test".to_string(),
                base_url: format!("http://{address}/v1"),
                model: "test-model".to_string(),
                api_key: "secret".to_string(),
                max_retries: 0,
                ..TranslateAiModelConfig::default()
            },
        );

        client
            .translate("translate", "hallo")
            .await
            .expect_err("the provider always rate-limits this request");
        let remaining = client
            .limiter
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .blocked_until
            .saturating_duration_since(Instant::now());
        server.abort();

        assert!(remaining > Duration::from_secs(59));
    }

    #[tokio::test]
    async fn rejects_a_chunked_response_above_the_body_limit() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test server");
        let address = listener.local_addr().expect("test server address");
        let router = Router::new().route("/v1/chat/completions", post(oversized_response));
        let server = tokio::spawn(async move {
            axum::serve(listener, router)
                .await
                .expect("serve test requests");
        });
        let client = ModelClient::new(
            reqwest::Client::new(),
            TranslateAiModelConfig {
                name: "body-limit-test".to_string(),
                base_url: format!("http://{address}/v1"),
                model: "test-model".to_string(),
                api_key: "secret".to_string(),
                max_retries: 0,
                ..TranslateAiModelConfig::default()
            },
        );

        let error = client
            .translate("translate", "hallo")
            .await
            .expect_err("oversized response must fail");
        server.abort();

        assert_eq!(error.kind, FailureKind::InvalidResponse);
        assert!(error.message.contains("65536-byte limit"));
    }

    #[tokio::test]
    async fn stale_auth_failure_retries_with_the_rotated_key() {
        let state = Arc::new(RotationState {
            old_key_seen: Notify::new(),
            release_old_request: Notify::new(),
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test server");
        let address = listener.local_addr().expect("test server address");
        let router = Router::new()
            .route("/v1/chat/completions", post(rotated_key_response))
            .with_state(Arc::clone(&state));
        let server = tokio::spawn(async move {
            axum::serve(listener, router)
                .await
                .expect("serve test requests");
        });
        let client = Arc::new(ModelClient::new(
            reqwest::Client::new(),
            TranslateAiModelConfig {
                name: "rotation-test".to_string(),
                base_url: format!("http://{address}/v1"),
                model: "test-model".to_string(),
                api_key: "old-secret".to_string(),
                max_retries: 0,
                ..TranslateAiModelConfig::default()
            },
        ));
        let request = tokio::spawn({
            let client = Arc::clone(&client);
            async move { client.translate("translate", "hallo").await }
        });

        tokio::time::timeout(Duration::from_secs(2), state.old_key_seen.notified())
            .await
            .expect("old-key request reached server");
        client.refresh_api_key("new-secret");
        state.release_old_request.notify_one();
        let translated = tokio::time::timeout(Duration::from_secs(2), request)
            .await
            .expect("translation completed")
            .expect("translation task did not panic")
            .expect("rotated key succeeded");
        server.abort();

        assert_eq!(translated, "przetłumaczono");
        assert!(client.is_available());
    }

    async fn rotated_key_response(
        State(state): State<Arc<RotationState>>,
        headers: AxumHeaderMap,
    ) -> impl IntoResponse {
        let authorization = headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok());
        if authorization == Some("Bearer old-secret") {
            state.old_key_seen.notify_one();
            state.release_old_request.notified().await;
            return (
                StatusCode::UNAUTHORIZED,
                Json(json!({ "error": "stale key" })),
            );
        }
        if authorization == Some("Bearer new-secret") {
            return (
                StatusCode::OK,
                Json(json!({
                    "choices": [{
                        "message": { "content": "przetłumaczono" },
                        "finish_reason": "stop"
                    }]
                })),
            );
        }
        (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "unexpected key" })),
        )
    }

    async fn oversized_response() -> Response {
        let chunks = (0..=MAX_RESPONSE_BYTES / 1024).map(|_| Ok::<_, Infallible>(vec![b'x'; 1024]));
        Response::new(Body::from_stream(futures::stream::iter(chunks)))
    }

    async fn rate_limited_response() -> impl IntoResponse {
        (
            StatusCode::TOO_MANY_REQUESTS,
            [(header::RETRY_AFTER, "60")],
            Json(json!({ "error": "slow down" })),
        )
    }

    async fn temperature_response(
        State(state): State<Arc<TemperatureState>>,
        Json(body): Json<Value>,
    ) -> impl IntoResponse {
        state.requests.fetch_add(1, Ordering::Relaxed);
        if body.get("temperature").is_some() {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "temperature is not supported" })),
            );
        }
        (
            StatusCode::OK,
            Json(json!({
                "choices": [{
                    "message": { "content": "przetłumaczono" },
                    "finish_reason": "stop"
                }]
            })),
        )
    }
}
