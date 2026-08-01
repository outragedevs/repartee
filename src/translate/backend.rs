//! The seam.
//!
//! Everything the mechanism knows about translating is this trait. The
//! parser, filter, masking, difficulty router, provider policy, HTTP client
//! and quality gate all live behind it and are specified separately.
//!
//! The future is boxed rather than returned via RPITIT so the trait stays
//! object-safe: the runtime holds an `Arc<dyn TranslateBackend>` and the
//! implementation can be swapped — stub, an AI module, a plain machine
//! translation API — without the choice leaking into `App`'s type.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use futures::future::BoxFuture;

use super::{TranslateOutcome, TranslateRequest, UntranslatedReason};

/// Translates one line.
///
/// Implementations must never return a translation they are not confident
/// in: a fluent sentence that means something else is the failure mode a
/// reader cannot detect. Returning
/// [`TranslateOutcome::Untranslated`] with a reason is always the better
/// answer — a visible hole beats an invisible lie.
pub trait TranslateBackend: Send + Sync + 'static {
    fn translate(&self, req: TranslateRequest) -> BoxFuture<'_, TranslateOutcome>;
}

/// Shared handle to whichever backend is configured.
pub type SharedBackend = Arc<dyn TranslateBackend>;

/// Exercises the whole mechanism with no API key and no network.
///
/// It exists so the parts that are easy to get wrong — ordered release
/// under out-of-order returns, a timeout releasing a stuck head, ceiling
/// overflow, the fail-closed E2E gate, the outgoing refusal — can be proven
/// before any real provider is written.
///
/// The transformation (reversing word order) is deterministic and visibly
/// different from the input, so a line that was NOT translated is obvious
/// on screen during manual testing.
pub struct StubBackend {
    delay: Duration,
    /// Every Nth call fails with [`UntranslatedReason::NoProvider`].
    /// 0 disables injection.
    fail_every: u64,
    /// Per-call delays, cycled. Lets a test make later lines finish before
    /// earlier ones deterministically — no randomness, so the ordering
    /// assertions are stable.
    jitter: Vec<Duration>,
    calls: AtomicU64,
}

impl StubBackend {
    #[must_use]
    pub const fn new(delay_ms: u64, fail_every: u64) -> Self {
        Self {
            delay: Duration::from_millis(delay_ms),
            fail_every,
            jitter: Vec::new(),
            calls: AtomicU64::new(0),
        }
    }

    /// Cycle through `delays_ms` instead of using a fixed delay, so a burst
    /// of lines completes out of order on purpose.
    #[must_use]
    pub fn with_jitter(delays_ms: &[u64]) -> Self {
        Self {
            delay: Duration::ZERO,
            fail_every: 0,
            jitter: delays_ms.iter().copied().map(Duration::from_millis).collect(),
            calls: AtomicU64::new(0),
        }
    }

    fn delay_for(&self, call: u64) -> Duration {
        if self.jitter.is_empty() {
            return self.delay;
        }
        let idx = usize::try_from((call - 1) % self.jitter.len() as u64).unwrap_or(0);
        self.jitter[idx]
    }
}

impl TranslateBackend for StubBackend {
    fn translate(&self, req: TranslateRequest) -> BoxFuture<'_, TranslateOutcome> {
        Box::pin(async move {
            let n = self.calls.fetch_add(1, Ordering::Relaxed) + 1;
            let delay = self.delay_for(n);
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
            if self.fail_every > 0 && n.is_multiple_of(self.fail_every) {
                return TranslateOutcome::Untranslated {
                    id: req.id,
                    reason: UntranslatedReason::NoProvider,
                };
            }
            let words: Vec<&str> = req.text.split_whitespace().collect();
            if words.len() < 2 {
                // Stands in for the broker's filter: a one-word line is
                // exactly the "moin"-shaped traffic the real filter drops.
                return TranslateOutcome::Untranslated {
                    id: req.id,
                    reason: UntranslatedReason::Filtered,
                };
            }
            TranslateOutcome::Translated {
                id: req.id,
                text: words.into_iter().rev().collect::<Vec<_>>().join(" "),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::translate::Direction;

    fn req(id: u64, text: &str) -> TranslateRequest {
        TranslateRequest {
            id,
            direction: Direction::Incoming,
            network: "libera".to_string(),
            target: "#dupa".to_string(),
            nick: "alice".to_string(),
            text: text.to_string(),
            source_lang: None,
            target_lang: "pl".to_string(),
            known_nicks: Vec::new(),
        }
    }

    #[tokio::test]
    async fn stub_translates_deterministically() {
        let b = StubBackend::new(0, 0);
        match b.translate(req(1, "hello world")).await {
            TranslateOutcome::Translated { id, text } => {
                assert_eq!(id, 1);
                assert_eq!(text, "world hello", "stub reverses word order");
            }
            TranslateOutcome::Untranslated { reason, .. } => {
                panic!("expected Translated, got Untranslated({reason:?})")
            }
        }
    }

    #[tokio::test]
    async fn stub_injects_failures_on_a_fixed_cadence() {
        let b = StubBackend::new(0, 2);
        assert!(matches!(
            b.translate(req(1, "a b")).await,
            TranslateOutcome::Translated { .. }
        ));
        assert!(matches!(
            b.translate(req(2, "a b")).await,
            TranslateOutcome::Untranslated {
                reason: UntranslatedReason::NoProvider,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn stub_filters_a_single_word_line() {
        let b = StubBackend::new(0, 0);
        assert!(matches!(
            b.translate(req(3, "moin")).await,
            TranslateOutcome::Untranslated {
                reason: UntranslatedReason::Filtered,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn jitter_makes_a_later_call_finish_first() {
        // Underpins the ordering integration test: without this the burst
        // would complete in order by accident and prove nothing.
        let b = Arc::new(StubBackend::with_jitter(&[60, 0]));
        let slow = Arc::clone(&b);
        let fast = Arc::clone(&b);
        let started = tokio::time::Instant::now();
        let (first, second) = tokio::join!(
            async move {
                let out = slow.translate(req(1, "a b")).await;
                (out.id(), started.elapsed())
            },
            async move {
                // Give the first call time to claim the 60 ms slot.
                tokio::time::sleep(Duration::from_millis(5)).await;
                let out = fast.translate(req(2, "c d")).await;
                (out.id(), started.elapsed())
            }
        );
        assert_eq!(first.0, 1);
        assert_eq!(second.0, 2);
        assert!(
            second.1 < first.1,
            "line 2 must finish before line 1: {:?} vs {:?}",
            second.1,
            first.1
        );
    }

    #[tokio::test]
    async fn backend_is_object_safe() {
        // The runtime holds `Arc<dyn TranslateBackend>`; if the trait ever
        // stops being object-safe this fails to compile.
        let b: SharedBackend = Arc::new(StubBackend::new(0, 0));
        assert_eq!(b.translate(req(1, "a b")).await.id(), 1);
    }
}
