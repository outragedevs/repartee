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

pub mod ai;
pub mod backend;
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
#[expect(
    dead_code,
    reason = "seam contract — a real backend reads every field; the stub only needs id and text"
)]
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
    pub deadline: Option<std::time::Instant>,
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
    /// Broker-side outcomes. Nothing in-tree constructs these yet — the stub
    /// has no quality gate and no quota — but they are part of the contract a
    /// real broker answers with, and both are already handled everywhere an
    /// outcome is consumed.
    #[allow(dead_code, reason = "constructed by a real broker, not by the stub")]
    QualityGate,
    #[allow(dead_code, reason = "constructed by a real broker, not by the stub")]
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

/// The two languages in play for one buffer.
///
/// A buffer has a language PAIR, and the two directions swap it. Modelling
/// it as a pair — resolved once, in one place — is deliberate: treating
/// `source` and `target` as per-direction settings read straight off the
/// config is exactly how the outgoing direction ended up inverted during
/// development, translating our own text as though it were the channel's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LangPair {
    /// What this channel or query speaks. `None` means "let the broker
    /// detect it", which is only meaningful for incoming.
    pub buffer: Option<String>,
    /// What we read and write here.
    pub mine: String,
}

impl LangPair {
    /// Languages for an incoming line: out of the buffer's, into ours.
    #[must_use]
    pub fn incoming(&self) -> (Option<String>, String) {
        (self.buffer.clone(), self.mine.clone())
    }

    /// Languages for an outgoing line: out of ours, into the buffer's.
    ///
    /// `None` when the buffer has no language set. A target cannot be
    /// autodetected — there is nothing to detect which language to WRITE in
    /// from — so the caller must refuse rather than guess.
    #[must_use]
    pub fn outgoing(&self) -> Option<(Option<String>, String)> {
        let target = self.buffer.clone()?;
        Some((Some(self.mine.clone()), target))
    }
}

/// Resolve the language pair for a buffer.
///
/// The single place the per-buffer and global settings are combined. Both
/// dispatch paths go through it, so they cannot drift apart.
#[must_use]
pub fn resolve_langs(
    buffer_cfg: Option<&crate::config::TranslateBufferConfig>,
    global_my_lang: &str,
) -> LangPair {
    LangPair {
        buffer: buffer_cfg.and_then(|c| c.lang.clone()),
        mine: buffer_cfg
            .and_then(|c| c.my_lang.clone())
            .unwrap_or_else(|| global_my_lang.to_string()),
    }
}

/// Mark a line that was NOT translated, and the byte offset where the
/// marker starts so the renderer can dim it.
///
/// Without this a failed translation renders identically to a `Filtered`
/// one — the same original text, no sign anything went wrong. That breaks
/// the rule the whole design rests on: a visible hole beats an invisible
/// lie. The hole has to actually be visible.
///
/// [`UntranslatedReason::Filtered`] is not a gap and gets no marker; it is
/// the broker working correctly.
#[must_use]
pub fn mark_untranslated(original: &str, reason: &UntranslatedReason) -> (String, Option<usize>) {
    if !reason.is_gap() {
        return (original.to_string(), None);
    }
    let offset = original.len();
    (
        format!("{original} [untranslated: {}]", reason.label()),
        Some(offset),
    )
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

    fn buf_cfg(
        lang: Option<&str>,
        my_lang: Option<&str>,
    ) -> crate::config::TranslateBufferConfig {
        crate::config::TranslateBufferConfig {
            incoming: true,
            outgoing: true,
            lang: lang.map(str::to_string),
            my_lang: my_lang.map(str::to_string),
        }
    }

    #[test]
    fn resolve_falls_back_to_the_global_language() {
        let pair = resolve_langs(Some(&buf_cfg(Some("de"), None)), "pl");
        assert_eq!(pair.buffer.as_deref(), Some("de"));
        assert_eq!(pair.mine, "pl");
    }

    #[test]
    fn resolve_honours_a_per_buffer_override() {
        // Reading one channel in a different language than the rest.
        let pair = resolve_langs(Some(&buf_cfg(Some("zh"), Some("en"))), "pl");
        assert_eq!(pair.buffer.as_deref(), Some("zh"));
        assert_eq!(pair.mine, "en", "the buffer override wins over the global");
    }

    #[test]
    fn resolve_with_no_buffer_config_is_all_defaults() {
        let pair = resolve_langs(None, "pl");
        assert_eq!(pair.buffer, None, "unknown buffer language means autodetect");
        assert_eq!(pair.mine, "pl");
    }

    #[test]
    fn incoming_goes_from_the_buffers_language_into_ours() {
        let pair = resolve_langs(Some(&buf_cfg(Some("de"), None)), "pl");
        assert_eq!(pair.incoming(), (Some("de".to_string()), "pl".to_string()));
    }

    #[test]
    fn outgoing_goes_from_ours_into_the_buffers_language() {
        // The exact inversion this pair type exists to prevent: outgoing is
        // the MIRROR of incoming, not a copy of it.
        let pair = resolve_langs(Some(&buf_cfg(Some("de"), None)), "pl");
        assert_eq!(
            pair.outgoing(),
            Some((Some("pl".to_string()), "de".to_string()))
        );
    }

    #[test]
    fn the_two_directions_are_exact_mirrors() {
        let pair = resolve_langs(Some(&buf_cfg(Some("de"), Some("en"))), "pl");
        let (in_src, in_dst) = pair.incoming();
        let (out_src, out_dst) = pair.outgoing().expect("buffer language is set");
        assert_eq!(in_src, Some(out_dst), "incoming source == outgoing target");
        assert_eq!(Some(in_dst), out_src, "incoming target == outgoing source");
    }

    #[test]
    fn outgoing_is_impossible_without_the_buffers_language() {
        // A target cannot be autodetected — there is nothing to detect
        // which language to WRITE in from.
        let pair = resolve_langs(Some(&buf_cfg(None, None)), "pl");
        assert_eq!(pair.incoming(), (None, "pl".to_string()), "incoming still works");
        assert_eq!(pair.outgoing(), None);
    }

    #[test]
    fn a_gap_is_marked_so_the_hole_is_visible() {
        for reason in [
            UntranslatedReason::Timeout,
            UntranslatedReason::NoProvider,
            UntranslatedReason::QualityGate,
            UntranslatedReason::DailyLimit,
            UntranslatedReason::Error("boom".to_string()),
        ] {
            let (text, offset) = mark_untranslated("hola que tal", &reason);
            assert!(
                text.starts_with("hola que tal ["),
                "{reason:?} must be marked: {text}"
            );
            assert!(
                text.contains(&reason.label()),
                "the marker names the reason: {text}"
            );
            let off = offset.expect("the marker is dimmable");
            assert!(text.is_char_boundary(off));
            assert_eq!(&text[..off], "hola que tal");
        }
    }

    #[test]
    fn a_filtered_line_carries_no_marker() {
        // The broker deciding a line needs no translation is a correct
        // outcome, not a gap — marking it would cry wolf on a third of all
        // traffic.
        let (text, offset) = mark_untranslated("moin", &UntranslatedReason::Filtered);
        assert_eq!(text, "moin");
        assert_eq!(offset, None);
    }

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
