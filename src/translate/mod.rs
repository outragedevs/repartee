//! Contract between the translation mechanism and whatever performs the
//! actual translation.
//!
//! Deliberately free of `App`, tokio, and I/O so the broker behind the seam
//! can later be an in-process Rust module, a subprocess speaking JSON over
//! stdio, or an HTTP call to a local daemon — none of which changes the
//! mechanism.
//!
//! What lives behind the seam and is explicitly NOT modelled here: language
//! detection, masking of nicks and URLs, model routing, provider policy,
//! retries, and quality gating.

pub mod queue;

/// Which way a line is travelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Incoming,
    Outgoing,
}

/// One line handed to the broker.
///
/// Exactly one line per request. Feeding surrounding lines as context was
/// measured to degrade output, so there is no context field to misuse —
/// its absence is the design, not an omission.
#[derive(Debug, Clone)]
pub struct TranslateRequest {
    /// Correlation key AND display-ordering key, taken from
    /// `AppState::next_message_id()` at the moment the line is queued.
    pub id: u64,
    pub direction: Direction,
    pub network: String,
    /// `#channel` or a nick.
    pub target: String,
    /// The speaker; ourselves when [`Direction::Outgoing`].
    pub nick: String,
    pub text: String,
    /// `None` lets the broker autodetect.
    pub source_lang: Option<String>,
    pub target_lang: String,
    /// The channel's nicklist. Travels in the request because only the
    /// client knows it, and masking behind the seam needs it to protect
    /// nicks from being "translated".
    pub known_nicks: Vec<String>,
}

/// Why a line came back untranslated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UntranslatedReason {
    /// The broker decided this line needs no translation — it is already in
    /// the target language, or it is noise. A CORRECT outcome, not a
    /// failure: it renders as a clean original with no marker.
    Filtered,
    QualityGate,
    DailyLimit,
    NoProvider,
    /// Raised by the mechanism (queue timeout or ceiling), never by the
    /// broker.
    Timeout,
    Error(String),
}

impl UntranslatedReason {
    /// `true` when the user should see a marker.
    ///
    /// [`Self::Filtered`] is the broker working correctly; everything else
    /// is a genuine gap. The rule this encodes is that a visible hole always
    /// beats an invisible lie — it must hold when the mechanism fails, not
    /// only when a model does.
    #[must_use]
    pub const fn is_gap(&self) -> bool {
        !matches!(self, Self::Filtered)
    }

    /// Short human-readable form for status lines and log rows.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::Filtered => "filtered".to_string(),
            Self::QualityGate => "quality gate".to_string(),
            Self::DailyLimit => "daily limit".to_string(),
            Self::NoProvider => "no provider".to_string(),
            Self::Timeout => "timeout".to_string(),
            Self::Error(e) => format!("error: {e}"),
        }
    }
}

/// What the broker hands back for a request.
#[derive(Debug, Clone)]
pub enum TranslateOutcome {
    Translated {
        id: u64,
        text: String,
    },
    Untranslated {
        id: u64,
        reason: UntranslatedReason,
    },
}

impl TranslateOutcome {
    #[must_use]
    pub const fn id(&self) -> u64 {
        match self {
            Self::Translated { id, .. } | Self::Untranslated { id, .. } => *id,
        }
    }
}

/// Build the displayed line and, when the original is appended, the byte
/// offset where its ` [original]` suffix starts.
///
/// The offset is what lets the renderer dim only the appended part. It is
/// deliberately NOT persisted: the stored text is flat (the log records what
/// was actually on screen), so a row reloaded from `SQLite` renders the same
/// characters undimmed.
///
/// Re-deriving the offset by scanning for a trailing `[...]` would be wrong —
/// an ordinary message may legitimately end that way, and the renderer would
/// dim someone else's brackets.
#[must_use]
pub fn compose_display(
    translated: &str,
    original: &str,
    show_original: bool,
) -> (String, Option<usize>) {
    if !show_original || translated == original {
        return (translated.to_string(), None);
    }
    let offset = translated.len();
    (format!("{translated} [{original}]"), Some(offset))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compose_appends_original_in_brackets_when_enabled() {
        let (text, offset) = compose_display("albalb", "blabla", true);
        assert_eq!(text, "albalb [blabla]");
        assert_eq!(offset, Some(6), "offset marks the space before the bracket");
        assert_eq!(&text[offset.unwrap()..], " [blabla]");
    }

    #[test]
    fn compose_omits_original_when_disabled() {
        let (text, offset) = compose_display("albalb", "blabla", false);
        assert_eq!(text, "albalb");
        assert_eq!(offset, None);
    }

    #[test]
    fn compose_omits_original_when_identical_to_translation() {
        // A `Filtered` line resolves to its own text; bracketing it would
        // render "moin [moin]".
        let (text, offset) = compose_display("moin", "moin", true);
        assert_eq!(text, "moin");
        assert_eq!(offset, None);
    }

    #[test]
    fn compose_offset_is_a_char_boundary_for_multibyte_text() {
        // The offset indexes bytes and is handed to the renderer for
        // slicing — a multibyte translation must not produce an offset
        // that panics on `&text[offset..]`.
        let (text, offset) = compose_display("zażółć gęślą", "jaźń", true);
        let off = offset.expect("original is appended");
        assert!(text.is_char_boundary(off));
        assert_eq!(&text[off..], " [jaźń]");
    }

    #[test]
    fn filtered_is_not_a_gap_but_every_other_reason_is() {
        assert!(!UntranslatedReason::Filtered.is_gap());
        for r in [
            UntranslatedReason::QualityGate,
            UntranslatedReason::DailyLimit,
            UntranslatedReason::NoProvider,
            UntranslatedReason::Timeout,
            UntranslatedReason::Error("boom".to_string()),
        ] {
            assert!(r.is_gap(), "{r:?} must be shown to the user");
        }
    }

    #[test]
    fn outcome_exposes_its_correlation_id() {
        assert_eq!(
            TranslateOutcome::Translated {
                id: 7,
                text: "x".to_string()
            }
            .id(),
            7
        );
        assert_eq!(
            TranslateOutcome::Untranslated {
                id: 9,
                reason: UntranslatedReason::Timeout
            }
            .id(),
            9
        );
    }
}
