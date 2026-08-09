use std::sync::{Mutex, RwLock};
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
    api_key: RwLock<String>,
    limiter: RateLimiter,
    disabled: Mutex<Option<FailureKind>>,
}

impl ModelClient {
    pub fn new(http: reqwest::Client, mut config: TranslateAiModelConfig) -> Self {
        let api_key = std::mem::take(&mut config.api_key);
        Self {
            limiter: RateLimiter::new(config.rpm, config.tpm),
            http,
            config,
            api_key: RwLock::new(api_key),
            disabled: Mutex::new(None),
        }
    }

    pub fn name(&self) -> &str {
        &self.config.name
    }

    pub fn is_available(&self) -> bool {
        !self
            .api_key
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_empty()
    }

    pub fn refresh_api_key(&self, api_key: &str) {
        let changed = {
            let mut current = self
                .api_key
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if *current == api_key {
                false
            } else {
                current.clear();
                current.push_str(api_key);
                true
            }
        };
        if changed {
            *self
                .disabled
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        }
    }

    pub async fn translate(&self, system: &str, user: &str) -> Result<String, AttemptFailure> {
        let api_key = self.api_key()?;
        let mut body = ChatRequest {
            model: &self.config.model,
            temperature: Some(0.0),
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
            self.limiter.acquire(estimate).await;
            let response = self
                .http
                .post(format!(
                    "{}/chat/completions",
                    self.config.base_url.trim_end_matches('/')
                ))
                .bearer_auth(&api_key)
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
            let raw = response.text().await.map_err(|error| AttemptFailure {
                kind: FailureKind::InvalidResponse,
                message: error.to_string(),
            })?;

            if status.is_success() {
                return parse_reply(&raw);
            }

            let status_code = status.as_u16();
            if status_code == 400
                && body.temperature.is_some()
                && raw.to_ascii_lowercase().contains("temperature")
            {
                body.temperature = None;
                continue;
            }
            if matches!(status_code, 401..=403) {
                self.disable(FailureKind::Permanent);
                return Err(failure(FailureKind::Permanent, status_code, &raw));
            }
            if status_code == 429 && DAILY_LIMIT.is_match(&raw) {
                self.disable(FailureKind::DailyLimit);
                return Err(failure(FailureKind::DailyLimit, status_code, &raw));
            }
            if status_code == 429 || status.is_server_error() {
                if retries >= self.config.max_retries {
                    return Err(failure(FailureKind::Transient, status_code, &raw));
                }
                retries += 1;
                let delay = retry_after(&headers).unwrap_or_else(|| backoff(retries));
                if status_code == 429 {
                    self.limiter.penalize(delay);
                } else {
                    tokio::time::sleep(delay).await;
                }
                continue;
            }
            return Err(failure(FailureKind::Permanent, status_code, &raw));
        }
    }

    fn api_key(&self) -> Result<String, AttemptFailure> {
        let disabled = *self
            .disabled
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(kind) = disabled {
            return Err(AttemptFailure {
                kind,
                message: "model disabled for this session".to_string(),
            });
        }
        let api_key = self
            .api_key
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if api_key.is_empty() {
            return Err(AttemptFailure {
                kind: FailureKind::Permanent,
                message: "model has no API key".to_string(),
            });
        }
        Ok(api_key)
    }

    fn disable(&self, kind: FailureKind) {
        *self
            .disabled
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(kind);
    }
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
    if choice.finish_reason.as_deref() == Some("length") {
        return Err(AttemptFailure {
            kind: FailureKind::InvalidResponse,
            message: "response was truncated".to_string(),
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
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|seconds| seconds.is_finite() && *seconds >= 0.0)
        .map(Duration::from_secs_f64)
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
                requests: f64::from(rpm) * 0.25,
                tokens: f64::from(tpm) * 0.25,
                last: now,
                blocked_until: now,
            }),
        }
    }

    async fn acquire(&self, estimated_tokens: u32) {
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
                let needed = f64::from(estimated_tokens).min(self.tpm);
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
                    return;
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
        state.blocked_until = state.blocked_until.max(Instant::now() + duration);
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
}
