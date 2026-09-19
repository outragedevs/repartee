mod client;
mod filter;
mod lang;
mod mask;
mod prompt;
mod quality;
mod router;

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use futures::future::BoxFuture;
use thiserror::Error;

use self::client::{FailureKind, ModelClient};
use self::router::Difficulty;
use super::backend::TranslateBackend;
use super::{TranslateOutcome, TranslateRequest, UntranslatedReason};
use crate::config::TranslateAiConfig;

const DEFAULT_TRANSLATION_BUDGET: Duration = Duration::from_secs(5);
const ATTEMPT_HEADROOM: Duration = Duration::from_millis(10);
const MIN_PREFERRED_ATTEMPT_MS: u64 = 500;
const TERMINAL_ATTEMPT_RESERVE: Duration = Duration::from_millis(1_500);

#[derive(Debug, Error)]
pub enum AiBuildError {
    #[error("AI model name is duplicated: {0}")]
    DuplicateModel(String),
    #[error("AI model {0} has an incomplete configuration")]
    InvalidModel(String),
    #[error("AI model {0} has an invalid HTTP(S) base URL")]
    InvalidModelUrl(String),
    #[error("AI policy references an unknown model: {0}")]
    UnknownPolicyModel(String),
    #[error("no AI model in the configured policies has an API key")]
    NoUsableModel,
    #[error("cannot load the AI translation prompt: {0}")]
    Prompt(#[from] std::io::Error),
    #[error("cannot build the AI HTTP client: {0}")]
    Http(#[from] reqwest::Error),
}

pub struct AiBackend {
    models: Vec<ModelClient>,
    easy: Vec<usize>,
    strong: Vec<usize>,
    terminal: HashSet<usize>,
    prompt_template: String,
    preferred_attempt_ms: AtomicU64,
    runtime_config: TranslateAiConfig,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CandidatePolicy {
    Preferred,
    Alternate,
}

#[derive(Debug, Clone, Copy)]
struct Candidate {
    model_index: usize,
    policy: CandidatePolicy,
}

impl AiBackend {
    pub fn new(config: &TranslateAiConfig) -> Result<Self, AiBuildError> {
        let prompt_template = prompt::load_template(&config.prompt_path)?;
        let http = reqwest::Client::builder()
            .user_agent(format!(
                "{}/{}",
                crate::constants::APP_NAME,
                env!("CARGO_PKG_VERSION")
            ))
            .build()?;
        let policy_names: HashSet<&str> = config
            .easy
            .iter()
            .chain(&config.strong)
            .map(String::as_str)
            .collect();
        let mut seen = HashSet::new();
        let mut models = Vec::new();
        let mut indexes = HashMap::new();

        for model in &config.models {
            if !seen.insert(model.name.as_str()) {
                return Err(AiBuildError::DuplicateModel(model.name.clone()));
            }
            if !policy_names.contains(model.name.as_str()) {
                continue;
            }
            if model.name.trim().is_empty()
                || model.base_url.trim().is_empty()
                || model.model.trim().is_empty()
            {
                return Err(AiBuildError::InvalidModel(model.name.clone()));
            }
            validate_model_url(model)?;
            let index = models.len();
            indexes.insert(model.name.clone(), index);
            models.push(ModelClient::new(http.clone(), model.clone()));
        }

        let mut easy = policy_indexes(&config.easy, &indexes)?;
        let mut strong = policy_indexes(&config.strong, &indexes)?;
        let terminal_names = config.terminal.clone().unwrap_or_else(|| {
            config
                .models
                .iter()
                .filter(|model| indexes.contains_key(&model.name) && is_groq_endpoint(model))
                .map(|model| model.name.clone())
                .collect()
        });
        let terminal = policy_indexes(&terminal_names, &indexes)?
            .into_iter()
            .collect();
        if easy.is_empty() || strong.is_empty() {
            if easy.is_empty() {
                easy.clone_from(&strong);
            }
            if strong.is_empty() {
                strong.clone_from(&easy);
            }
        }
        if models.is_empty() || !models.iter().any(ModelClient::is_available) {
            return Err(AiBuildError::NoUsableModel);
        }

        Ok(Self {
            models,
            easy,
            strong,
            terminal,
            prompt_template,
            preferred_attempt_ms: AtomicU64::new(
                config.preferred_attempt_ms.max(MIN_PREFERRED_ATTEMPT_MS),
            ),
            runtime_config: restart_bound_configuration(config),
        })
    }

    pub fn configuration_error(config: &TranslateAiConfig) -> Option<String> {
        Self::new(config).err().map(|error| error.to_string())
    }

    fn reload_credentials(&self, config: &TranslateAiConfig) {
        for client in &self.models {
            let key = config
                .models
                .iter()
                .find(|model| model.name == client.name())
                .map_or("", |model| model.api_key.trim());
            client.refresh_api_key(key);
        }
    }

    async fn translate_one(&self, req: TranslateRequest) -> TranslateOutcome {
        if let Some(outcome) = preflight_outcome(&req) {
            return outcome;
        }

        let masked = mask_request(&req);
        if let Some(outcome) = masked_preflight_outcome(&req, &masked) {
            return outcome;
        }
        let difficulty = router::classify(&req.text, req.source_lang.as_deref());
        let (preferred, alternate) = match difficulty {
            Difficulty::Easy => (&self.easy, &self.strong),
            Difficulty::Strong => (&self.strong, &self.easy),
        };
        let system = prompt::render(
            &self.prompt_template,
            req.source_lang.as_deref(),
            &req.target_lang,
        );
        let mut saw_quality_failure = false;
        let mut saw_daily_limit = false;
        let mut saw_provider_failure = false;

        let deadline = req
            .deadline
            .unwrap_or_else(|| Instant::now() + DEFAULT_TRANSLATION_BUDGET);
        let candidates = self.available_candidates(preferred, alternate);
        for (position, candidate) in candidates.iter().copied().enumerate() {
            let model = &self.models[candidate.model_index];
            let Some(attempt_budget) = self.attempt_budget(deadline, &candidates, position) else {
                saw_provider_failure = true;
                continue;
            };
            let attempt = tokio::time::timeout(
                attempt_budget,
                model.translate(&system, &masked.text),
            )
            .await;
            match attempt {
                Err(_) => {
                    saw_provider_failure = true;
                    tracing::warn!(
                        request_id = req.id,
                        model = model.name(),
                        ?attempt_budget,
                        "translate: AI model attempt timed out"
                    );
                }
                Ok(Ok(output)) => {
                    let (translated, placeholders) = mask::unmask(&masked, &output);
                    match quality::check(&req.text, &req.target_lang, &translated, &placeholders) {
                        Ok(()) => {
                            tracing::debug!(
                                request_id = req.id,
                                model = model.name(),
                                ?difficulty,
                                "translate: AI model accepted"
                            );
                            return TranslateOutcome::Translated {
                                id: req.id,
                                text: translated,
                            };
                        }
                        Err(rejection) => {
                            saw_quality_failure = true;
                            tracing::warn!(
                                request_id = req.id,
                                model = model.name(),
                                ?rejection,
                                "translate: AI answer rejected"
                            );
                        }
                    }
                }
                Ok(Err(error)) => {
                    saw_daily_limit |= error.kind == FailureKind::DailyLimit;
                    saw_provider_failure |= matches!(
                        error.kind,
                        FailureKind::Transient
                            | FailureKind::InvalidResponse
                            | FailureKind::Permanent
                    );
                    tracing::warn!(
                        request_id = req.id,
                        model = model.name(),
                        kind = ?error.kind,
                        error = %error.message,
                        "translate: AI model failed"
                    );
                }
            }
        }

        let reason = failed_policy_reason(
            saw_quality_failure,
            saw_provider_failure,
            saw_daily_limit,
        );
        TranslateOutcome::Untranslated { id: req.id, reason }
    }

    fn attempt_budget(
        &self,
        deadline: Instant,
        candidates: &[Candidate],
        position: usize,
    ) -> Option<Duration> {
        let current = candidates[position];
        let stage_attempts_left = candidates[position..]
            .iter()
            .take_while(|candidate| candidate.policy == current.policy)
            .count();
        let stage = &candidates[position..position + stage_attempts_left];
        let current_is_terminal = self.is_terminal(current.model_index);
        let (regular_attempts_left, terminal_attempts_left) = if current_is_terminal {
            (0, stage.len())
        } else {
            let regular = stage
                .iter()
                .take_while(|candidate| !self.is_terminal(candidate.model_index))
                .count();
            (regular, stage.len() - regular)
        };
        let preferred_attempt =
            Duration::from_millis(self.preferred_attempt_ms.load(Ordering::Relaxed));
        model_attempt_budget(
            deadline.saturating_duration_since(Instant::now()),
            current_is_terminal,
            terminal_attempts_left,
            regular_attempts_left,
            preferred_attempt,
        )
    }

    fn available_candidates(&self, preferred: &[usize], alternate: &[usize]) -> Vec<Candidate> {
        let mut attempted = HashSet::new();
        let mut candidates = Vec::new();
        for (policy, candidate_policy) in [
            (preferred, CandidatePolicy::Preferred),
            (alternate, CandidatePolicy::Alternate),
        ] {
            for terminal in [false, true] {
                candidates.extend(
                    policy
                        .iter()
                        .copied()
                        .filter(|index| {
                            self.is_terminal(*index) == terminal
                                && self.models[*index].is_available()
                                && attempted.insert(*index)
                        })
                        .map(|model_index| Candidate {
                            model_index,
                            policy: candidate_policy,
                        }),
                );
            }
        }
        candidates
    }

    fn is_terminal(&self, index: usize) -> bool {
        self.terminal.contains(&index)
    }
}

fn preflight_outcome(req: &TranslateRequest) -> Option<TranslateOutcome> {
    let unsupported = if lang::supports(&req.target_lang) {
        req.source_lang
            .as_deref()
            .filter(|source| !lang::supports(source))
            .map(|source| ("source", source))
    } else {
        Some(("target", req.target_lang.as_str()))
    };
    if let Some((role, language)) = unsupported {
        tracing::warn!(
            request_id = req.id,
            language_role = role,
            language,
            "translate: language is unsupported by the AI quality gate"
        );
        return Some(TranslateOutcome::Untranslated {
            id: req.id,
            reason: UntranslatedReason::QualityGate,
        });
    }
    filter::should_filter(req).then_some(TranslateOutcome::Untranslated {
        id: req.id,
        reason: UntranslatedReason::Filtered,
    })
}

fn masked_preflight_outcome(
    req: &TranslateRequest,
    masked: &mask::MaskedText,
) -> Option<TranslateOutcome> {
    (!masked.has_translatable_prose()
        || filter::is_already_target(&masked.text, &req.target_lang))
    .then_some(TranslateOutcome::Untranslated {
        id: req.id,
        reason: UntranslatedReason::Filtered,
    })
}

fn failed_policy_reason(
    saw_quality_failure: bool,
    saw_provider_failure: bool,
    saw_daily_limit: bool,
) -> UntranslatedReason {
    if saw_quality_failure {
        UntranslatedReason::QualityGate
    } else if saw_provider_failure {
        UntranslatedReason::Error("all AI models failed".to_string())
    } else if saw_daily_limit {
        UntranslatedReason::DailyLimit
    } else {
        UntranslatedReason::NoProvider
    }
}

fn model_attempt_budget(
    remaining: Duration,
    terminal: bool,
    terminal_attempts_left: usize,
    regular_attempts_left: usize,
    preferred_attempt: Duration,
) -> Option<Duration> {
    let budget = if terminal {
        let divisor = u32::try_from(terminal_attempts_left)
            .unwrap_or(u32::MAX)
            .max(1);
        remaining
            .checked_div(divisor)
            .unwrap_or_default()
            .saturating_sub(ATTEMPT_HEADROOM)
    } else {
        let terminal_count = u32::try_from(terminal_attempts_left).unwrap_or(u32::MAX);
        let regular_count = u32::try_from(regular_attempts_left)
            .unwrap_or(u32::MAX)
            .max(1);
        let total_count = terminal_count.saturating_add(regular_count).max(1);
        let desired_terminal_reserve = TERMINAL_ATTEMPT_RESERVE.saturating_mul(terminal_count);
        let proportional_terminal_reserve = remaining
            .saturating_mul(terminal_count)
            .checked_div(total_count)
            .unwrap_or_default();
        let terminal_reserve = desired_terminal_reserve.min(proportional_terminal_reserve);
        remaining
            .saturating_sub(terminal_reserve)
            .checked_div(regular_count)
            .unwrap_or_default()
            .min(preferred_attempt)
    };
    (!budget.is_zero()).then_some(budget)
}

fn mask_request(req: &TranslateRequest) -> mask::MaskedText {
    let mut nicks = Vec::with_capacity(req.known_nicks.len() + 2);
    nicks.extend(req.known_nicks.iter().cloned());
    nicks.push(req.nick.clone());
    if !crate::irc::formatting::is_channel(&req.target) {
        nicks.push(req.target.clone());
    }
    mask::mask_with_casemapping(&req.text, &nicks, &req.casemapping)
}

fn validate_model_url(model: &crate::config::TranslateAiModelConfig) -> Result<(), AiBuildError> {
    let url = reqwest::Url::parse(model.base_url.trim())
        .map_err(|_| AiBuildError::InvalidModelUrl(model.name.clone()))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(AiBuildError::InvalidModelUrl(model.name.clone()));
    }
    Ok(())
}

fn is_groq_endpoint(model: &crate::config::TranslateAiModelConfig) -> bool {
    reqwest::Url::parse(model.base_url.trim())
        .is_ok_and(|url| {
            url.host_str()
                .is_some_and(|host| host.eq_ignore_ascii_case("api.groq.com"))
        })
}

impl TranslateBackend for AiBackend {
    fn kind(&self) -> crate::translate::backend::BackendKind {
        crate::translate::backend::BackendKind::Ai
    }

    fn is_ready(&self) -> bool {
        self.models.iter().any(ModelClient::is_available)
    }

    fn configuration_matches(&self, config: &crate::config::TranslateConfig) -> bool {
        crate::translate::backend::backend_kind(&config.backend)
            == crate::translate::backend::BackendKind::Ai
            && self.runtime_config == restart_bound_configuration(&config.ai)
    }

    fn refresh_config(&self, config: &crate::config::TranslateConfig) {
        self.reload_credentials(&config.ai);
        self.preferred_attempt_ms.store(
            config.ai.preferred_attempt_ms.max(MIN_PREFERRED_ATTEMPT_MS),
            Ordering::Relaxed,
        );
    }

    fn translate(&self, req: TranslateRequest) -> BoxFuture<'_, TranslateOutcome> {
        Box::pin(self.translate_one(req))
    }
}

fn restart_bound_configuration(config: &TranslateAiConfig) -> TranslateAiConfig {
    let mut config = config.clone();
    config.preferred_attempt_ms = 0;
    for model in &mut config.models {
        model.api_key.clear();
    }
    config
}

fn policy_indexes(
    names: &[String],
    indexes: &HashMap<String, usize>,
) -> Result<Vec<usize>, AiBuildError> {
    names
        .iter()
        .map(|name| {
            indexes
                .get(name)
                .copied()
                .ok_or_else(|| AiBuildError::UnknownPolicyModel(name.clone()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::TranslateAiModelConfig;
    use crate::translate::Direction;
    use axum::Json;
    use axum::Router;
    use axum::http::{HeaderMap, StatusCode, header};
    use axum::response::IntoResponse;
    use axum::routing::post;
    use serde_json::{Value, json};

    fn config_with_key() -> TranslateAiConfig {
        let model = TranslateAiModelConfig {
            name: "test".to_string(),
            base_url: "http://127.0.0.1:1/v1".to_string(),
            model: "test-model".to_string(),
            api_key_env: "TEST_API".to_string(),
            api_key: "secret".to_string(),
            ..TranslateAiModelConfig::default()
        };
        TranslateAiConfig {
            easy: vec!["test".to_string()],
            strong: vec!["test".to_string()],
            terminal: Some(Vec::new()),
            preferred_attempt_ms: 3_000,
            prompt_path: String::new(),
            models: vec![model],
        }
    }

    fn request(text: &str) -> TranslateRequest {
        TranslateRequest {
            id: 1,
            direction: Direction::Incoming,
            network: "test".to_string(),
            target: "#test".to_string(),
            nick: "alice".to_string(),
            text: text.to_string(),
            source_lang: Some("de".to_string()),
            target_lang: "pl".to_string(),
            deadline: None,
            casemapping: "rfc1459".to_string(),
            known_nicks: vec!["alice".to_string()],
        }
    }

    #[test]
    fn refuses_to_build_without_any_policy_model_key() {
        let mut config = config_with_key();
        config.models[0].api_key.clear();
        assert!(matches!(
            AiBackend::new(&config),
            Err(AiBuildError::NoUsableModel)
        ));
    }

    #[test]
    fn refuses_to_build_with_an_unknown_policy_model() {
        let mut config = config_with_key();
        config.easy.push("typo".to_string());
        assert!(matches!(
            AiBackend::new(&config),
            Err(AiBuildError::UnknownPolicyModel(name)) if name == "typo"
        ));
    }

    #[test]
    fn refuses_to_build_with_an_unknown_terminal_model() {
        let mut config = config_with_key();
        config.terminal = Some(vec!["typo".to_string()]);
        assert!(matches!(
            AiBackend::new(&config),
            Err(AiBuildError::UnknownPolicyModel(name)) if name == "typo"
        ));
    }

    #[test]
    fn refuses_to_build_with_a_malformed_model_url() {
        let mut config = config_with_key();
        config.models[0].base_url = "localhost:11434/v1".to_string();
        assert!(matches!(
            AiBackend::new(&config),
            Err(AiBuildError::InvalidModelUrl(name)) if name == "test"
        ));
    }

    #[test]
    fn refuses_to_build_with_a_non_http_model_url() {
        let mut config = config_with_key();
        config.models[0].base_url = "ftp://example.com/v1".to_string();
        assert!(matches!(
            AiBackend::new(&config),
            Err(AiBuildError::InvalidModelUrl(name)) if name == "test"
        ));
    }

    #[test]
    fn selected_policy_finishes_before_the_alternate_policy() {
        let model = |name: &str| TranslateAiModelConfig {
            name: name.to_string(),
            base_url: "http://127.0.0.1:1/v1".to_string(),
            model: name.to_string(),
            api_key_env: "TEST_API".to_string(),
            api_key: "secret".to_string(),
            ..TranslateAiModelConfig::default()
        };
        let config = TranslateAiConfig {
            easy: vec!["terminal".to_string(), "primary".to_string()],
            strong: vec!["alternate".to_string()],
            terminal: Some(vec!["terminal".to_string()]),
            preferred_attempt_ms: 3_000,
            prompt_path: String::new(),
            models: vec![
                model("terminal"),
                model("primary"),
                model("alternate"),
            ],
        };
        let backend = AiBackend::new(&config).expect("valid config");
        let candidates = backend.available_candidates(&backend.easy, &backend.strong);
        let names: Vec<_> = candidates
            .iter()
            .map(|candidate| backend.models[candidate.model_index].name())
            .collect();

        assert_eq!(names, ["primary", "terminal", "alternate"]);
    }

    #[test]
    fn preferred_budget_ignores_an_alternate_beyond_terminal_fallbacks() {
        let mut config = config_with_key();
        let alternate = TranslateAiModelConfig {
            name: "alternate".to_string(),
            base_url: "http://127.0.0.1:1/v1".to_string(),
            model: "alternate".to_string(),
            api_key_env: "TEST_API".to_string(),
            api_key: "secret".to_string(),
            ..TranslateAiModelConfig::default()
        };
        let terminal = TranslateAiModelConfig {
            name: "terminal".to_string(),
            base_url: "http://127.0.0.1:1/v1".to_string(),
            model: "terminal".to_string(),
            api_key_env: "TEST_API".to_string(),
            api_key: "secret".to_string(),
            ..TranslateAiModelConfig::default()
        };
        config.easy = vec!["test".to_string(), "terminal".to_string()];
        config.strong = vec!["alternate".to_string()];
        config.terminal = Some(vec!["terminal".to_string()]);
        config.preferred_attempt_ms = 10_000;
        config.models.extend([alternate, terminal]);
        let backend = AiBackend::new(&config).expect("valid config");
        let candidates = backend.available_candidates(&backend.easy, &backend.strong);
        let budget = backend.attempt_budget(
            Instant::now() + Duration::from_secs(10),
            &candidates,
            0,
        );

        assert!(budget.is_some_and(|value| value >= Duration::from_millis(6_900)));
    }

    #[test]
    fn preferred_budget_ignores_adjacent_regular_alternate_policy() {
        let mut config = config_with_key();
        config.strong = vec!["alternate".to_string()];
        config.terminal = Some(Vec::new());
        config.preferred_attempt_ms = 10_000;
        config.models.push(TranslateAiModelConfig {
            name: "alternate".to_string(),
            base_url: "http://127.0.0.1:1/v1".to_string(),
            model: "alternate".to_string(),
            api_key_env: "TEST_API".to_string(),
            api_key: "secret".to_string(),
            ..TranslateAiModelConfig::default()
        });
        let backend = AiBackend::new(&config).expect("valid config");
        let candidates = backend.available_candidates(&backend.easy, &backend.strong);
        let budget = backend.attempt_budget(
            Instant::now() + Duration::from_secs(10),
            &candidates,
            0,
        );

        assert!(budget.is_some_and(|value| value >= Duration::from_millis(9_900)));
    }

    #[test]
    fn a_legacy_policy_infers_groq_as_terminal() {
        let mut config = config_with_key();
        config.terminal = None;
        config.models[0].base_url = "https://api.groq.com/openai/v1".to_string();

        let backend = AiBackend::new(&config).expect("valid config");

        assert!(backend.is_terminal(0));
    }

    #[test]
    fn a_missing_terminal_policy_remains_distinguishable_from_an_empty_one() {
        let config: TranslateAiConfig = toml::from_str("easy = []\nstrong = []")
            .expect("minimal legacy AI policy");

        assert!(config.terminal.is_none());
    }

    #[test]
    fn regular_attempt_keeps_terminal_time_in_reserve() {
        assert_eq!(
            model_attempt_budget(Duration::from_secs(5), false, 2, 1, Duration::from_secs(3),),
            Some(Duration::from_secs(2))
        );
    }

    #[test]
    fn short_deadline_still_assigns_time_to_a_regular_model() {
        assert_eq!(
            model_attempt_budget(
                Duration::from_millis(500),
                false,
                2,
                3,
                Duration::from_secs(3),
            ),
            Some(Duration::from_millis(100))
        );
    }

    #[test]
    fn regular_attempt_uses_the_configured_budget_when_deadline_allows() {
        assert_eq!(
            model_attempt_budget(Duration::from_secs(13), false, 2, 2, Duration::from_secs(3),),
            Some(Duration::from_secs(3))
        );
    }

    #[test]
    fn terminal_attempts_share_the_reserved_remainder() {
        assert_eq!(
            model_attempt_budget(Duration::from_secs(4), true, 2, 0, Duration::from_secs(3),),
            Some(Duration::from_millis(1_990))
        );
    }

    #[test]
    fn preferred_attempt_budget_refreshes_without_rebuild() {
        let config = config_with_key();
        let backend = AiBackend::new(&config).expect("valid config");
        let mut translate = crate::config::TranslateConfig {
            backend: "ai".to_string(),
            ai: config,
            ..crate::config::TranslateConfig::default()
        };
        translate.ai.preferred_attempt_ms = 4_200;

        backend.refresh_config(&translate);

        assert_eq!(backend.preferred_attempt_ms.load(Ordering::Relaxed), 4_200);
    }

    #[tokio::test]
    async fn filters_before_attempting_network_io() {
        let backend = AiBackend::new(&config_with_key()).expect("valid config");
        assert!(matches!(
            backend.translate(request("lol")).await,
            TranslateOutcome::Untranslated {
                reason: UntranslatedReason::Filtered,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn filters_when_masking_removes_all_prose() {
        let backend = AiBackend::new(&config_with_key()).expect("valid config");

        for text in ["alice:", "#test"] {
            assert!(matches!(
                backend.translate(request(text)).await,
                TranslateOutcome::Untranslated {
                    reason: UntranslatedReason::Filtered,
                    ..
                }
            ));
        }
    }

    #[tokio::test]
    async fn rejects_an_unverifiable_target_before_attempting_network_io() {
        let backend = AiBackend::new(&config_with_key()).expect("valid config");
        let mut req = request("ich glaube das funktioniert wirklich");
        req.target_lang = "xx-INVALID".to_string();

        assert!(matches!(
            backend.translate(req).await,
            TranslateOutcome::Untranslated {
                reason: UntranslatedReason::QualityGate,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn rejects_an_unverifiable_source_before_attempting_network_io() {
        let backend = AiBackend::new(&config_with_key()).expect("valid config");
        let mut req = request("ich glaube das funktioniert wirklich");
        req.source_lang = Some("xx-INVALID".to_string());

        assert!(matches!(
            backend.translate(req).await,
            TranslateOutcome::Untranslated {
                reason: UntranslatedReason::QualityGate,
                ..
            }
        ));
    }

    #[test]
    fn masks_the_speaker_and_query_peer_without_a_nick_list() {
        let mut req = request("alice: guten morgen bob");
        req.target = "bob".to_string();
        req.known_nicks.clear();
        let masked = mask_request(&req);

        assert!(!masked.text.to_ascii_lowercase().contains("alice"));
        assert!(!masked.text.to_ascii_lowercase().contains("bob"));
    }

    #[test]
    fn target_language_preflight_ignores_masked_nicknames() {
        let mut req = request("ich: hello");
        req.direction = Direction::Outgoing;
        req.source_lang = Some("en".to_string());
        req.target_lang = "de".to_string();
        req.known_nicks = vec!["ich".to_string()];

        let outcome = preflight_outcome(&req).or_else(|| {
            let masked = mask_request(&req);
            masked_preflight_outcome(&req, &masked)
        });

        assert!(outcome.is_none(), "nickname biased target detection");
    }

    #[tokio::test]
    async fn a_hanging_regular_model_still_reaches_the_terminal_fallback() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test server");
        let address = listener.local_addr().expect("test server address");
        let app = Router::new()
            .route("/hang/chat/completions", post(hanging_translation))
            .route("/v1/chat/completions", post(successful_translation));
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve test requests");
        });
        let model = |name: &str, path: &str, model: &str| TranslateAiModelConfig {
            name: name.to_string(),
            base_url: format!("http://{address}/{path}"),
            model: model.to_string(),
            api_key_env: "TEST_API".to_string(),
            api_key: "secret".to_string(),
            max_retries: 0,
            ..TranslateAiModelConfig::default()
        };
        let config = TranslateAiConfig {
            easy: vec!["hanging".to_string(), "success".to_string()],
            strong: vec!["hanging".to_string(), "success".to_string()],
            terminal: Some(vec!["success".to_string()]),
            preferred_attempt_ms: 500,
            prompt_path: String::new(),
            models: vec![
                model("hanging", "hang", "hanging-model"),
                model("success", "v1", "success-model"),
            ],
        };
        let backend = AiBackend::new(&config).expect("valid config");
        let mut req = request("ich glaube das funktioniert wirklich");
        req.deadline = Some(Instant::now() + Duration::from_millis(2_200));

        let outcome = backend.translate(req).await;
        server.abort();

        assert!(matches!(outcome, TranslateOutcome::Translated { .. }));
    }

    #[tokio::test]
    async fn a_long_retry_after_does_not_consume_the_fallback_budget() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test server");
        let address = listener.local_addr().expect("test server address");
        let app = Router::new()
            .route("/limited/chat/completions", post(rate_limited_translation))
            .route("/v1/chat/completions", post(successful_translation));
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve test requests");
        });
        let model = |name: &str, path: &str, model: &str| TranslateAiModelConfig {
            name: name.to_string(),
            base_url: format!("http://{address}/{path}"),
            model: model.to_string(),
            api_key_env: "TEST_API".to_string(),
            api_key: "secret".to_string(),
            ..TranslateAiModelConfig::default()
        };
        let config = TranslateAiConfig {
            easy: vec!["limited".to_string(), "success".to_string()],
            strong: vec!["limited".to_string(), "success".to_string()],
            terminal: Some(Vec::new()),
            preferred_attempt_ms: 500,
            prompt_path: String::new(),
            models: vec![
                model("limited", "limited", "limited-model"),
                model("success", "v1", "success-model"),
            ],
        };
        let backend = AiBackend::new(&config).expect("valid config");
        let mut req = request("ich glaube das funktioniert wirklich");
        req.deadline = Some(Instant::now() + Duration::from_millis(400));

        let outcome = backend.translate(req).await;
        server.abort();

        assert!(matches!(outcome, TranslateOutcome::Translated { .. }));
    }

    #[tokio::test]
    async fn falls_back_and_accepts_an_openai_compatible_response() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test server");
        let address = listener.local_addr().expect("test server address");
        let app = Router::new()
            .route(
                "/fail/chat/completions",
                post(|| async { StatusCode::INTERNAL_SERVER_ERROR }),
            )
            .route("/v1/chat/completions", post(successful_translation));
        tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve test request");
        });

        let model = |name: &str, path: &str, model: &str| TranslateAiModelConfig {
            name: name.to_string(),
            base_url: format!("http://{address}/{path}"),
            model: model.to_string(),
            api_key_env: "TEST_API".to_string(),
            api_key: "secret".to_string(),
            max_retries: 0,
            ..TranslateAiModelConfig::default()
        };
        let config = TranslateAiConfig {
            easy: vec!["failed".to_string(), "success".to_string()],
            strong: vec!["failed".to_string(), "success".to_string()],
            terminal: Some(Vec::new()),
            preferred_attempt_ms: 3_000,
            prompt_path: String::new(),
            models: vec![
                model("failed", "fail", "failed-model"),
                model("success", "v1", "success-model"),
            ],
        };
        let backend = AiBackend::new(&config).expect("valid config");

        assert!(matches!(
            backend
                .translate(request("ich glaube das funktioniert wirklich"))
                .await,
            TranslateOutcome::Translated { text, .. }
                if text == "naprawdę wierzę, że to działa i wszystko będzie dobrze"
        ));
    }

    #[tokio::test]
    async fn falls_through_to_the_alternate_policy_after_auth_failure() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test server");
        let address = listener.local_addr().expect("test server address");
        let app = Router::new()
            .route(
                "/auth/chat/completions",
                post(|| async { StatusCode::UNAUTHORIZED }),
            )
            .route("/v1/chat/completions", post(successful_translation));
        tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve test request");
        });
        let model = |name: &str, path: &str, model: &str| TranslateAiModelConfig {
            name: name.to_string(),
            base_url: format!("http://{address}/{path}"),
            model: model.to_string(),
            api_key_env: "TEST_API".to_string(),
            api_key: "secret".to_string(),
            max_retries: 0,
            ..TranslateAiModelConfig::default()
        };
        let config = TranslateAiConfig {
            easy: vec!["auth".to_string()],
            strong: vec!["success".to_string()],
            terminal: Some(Vec::new()),
            preferred_attempt_ms: 3_000,
            prompt_path: String::new(),
            models: vec![
                model("auth", "auth", "auth-model"),
                model("success", "v1", "success-model"),
            ],
        };
        let backend = AiBackend::new(&config).expect("valid config");

        assert!(matches!(
            backend
                .translate(request("ich glaube das funktioniert wirklich"))
                .await,
            TranslateOutcome::Translated { text, .. }
                if text == "naprawdę wierzę, że to działa i wszystko będzie dobrze"
        ));
    }

    #[tokio::test]
    async fn refreshes_a_rotated_key_without_rebuilding_the_backend() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test server");
        let address = listener.local_addr().expect("test server address");
        let app = Router::new().route("/v1/chat/completions", post(rotated_translation));
        tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve test request");
        });

        let mut config = config_with_key();
        config.models[0].base_url = format!("http://{address}/v1");
        config.models[0].api_key = "old-secret".to_string();
        let backend = AiBackend::new(&config).expect("valid config");
        config.models[0].api_key = "new-secret".to_string();
        backend.reload_credentials(&config);

        assert!(matches!(
            backend
                .translate(request("ich glaube das funktioniert wirklich"))
                .await,
            TranslateOutcome::Translated { .. }
        ));
    }

    async fn successful_translation(
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> impl IntoResponse {
        let authorized = headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            == Some("Bearer secret");
        let valid_body = body["model"] == "success-model"
            && body["messages"][1]["content"] == "ich glaube das funktioniert wirklich";
        if !authorized || !valid_body {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "invalid test request" })),
            );
        }
        (
            StatusCode::OK,
            Json(json!({
                "choices": [{
                    "message": {
                        "content": "naprawdę wierzę, że to działa i wszystko będzie dobrze"
                    },
                    "finish_reason": "stop"
                }]
            })),
        )
    }

    async fn rotated_translation(headers: HeaderMap) -> impl IntoResponse {
        let authorized = headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            == Some("Bearer new-secret");
        let status = if authorized {
            StatusCode::OK
        } else {
            StatusCode::UNAUTHORIZED
        };
        (
            status,
            Json(json!({
                "choices": [{
                    "message": {
                        "content": "naprawdę wierzę, że to działa i wszystko będzie dobrze"
                    },
                    "finish_reason": "stop"
                }]
            })),
        )
    }

    async fn hanging_translation() -> StatusCode {
        tokio::time::sleep(Duration::from_secs(2)).await;
        StatusCode::GATEWAY_TIMEOUT
    }

    async fn rate_limited_translation() -> impl IntoResponse {
        (
            StatusCode::TOO_MANY_REQUESTS,
            [(header::RETRY_AFTER, "60")],
            Json(json!({ "error": "slow down" })),
        )
    }
}
