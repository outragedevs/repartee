use std::sync::LazyLock;

use regex::Regex;

use super::lang::SupportedLanguage;
use super::mask::UnmaskReport;

static META: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"(?im)^\s*(?:oto|poni[żz]ej|tutaj)\b.{0,30}?\b(?:t[łl]umaczenie|przek[łl]ad)",
        r"(?im)^\s*(?:here\s+(?:is|are|'s)|below\s+is)\s+(?:the\s+)?translation\s*:",
        r"(?im)^\s*(?:the\s+)?translation\s*:",
        r"(?im)^\s*(?:t[łl]umaczenie|przek[łl]ad|[üu]bersetzung)\s*:",
    ]
    .into_iter()
    .map(|pattern| Regex::new(pattern).expect("valid regex"))
    .collect()
});
static REFUSAL: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"(?i)\bas\s+an?\s+AI(?:\s+language\s+model)?\b",
        r"(?i)\bjako\s+(?:model(?:\s+j[ęe]zykowy)?|sztuczna\s+inteligencja)\b",
        r"(?i)\bals\s+(?:KI|AI|Sprachmodell)\b",
        r"(?im)^\s*I\s+(?:cannot|can't|won't|am\s+unable\s+to|'m\s+unable\s+to)\s+(?:assist|comply|fulfil|fulfill|provide)\b.{0,80}\b(?:request|content|text)\b",
        r"(?im)^\s*(?:nie\s+mog[ęe]|nie\s+jestem\s+w\s+stanie)\s+(?:pom[oó]c|spe[łl]ni[ćc]|zrealizowa[ćc])\b.{0,80}\b(?:pro[śs]b|żądan|tre[śs][ćc]|tekst)\w*\b",
        r"(?im)^\s*ich\s+kann\b.{0,80}\b(?:Anfrage|Aufforderung|Inhalt|Text)\b.{0,30}\bnicht\b",
    ]
    .into_iter()
    .map(|pattern| Regex::new(pattern).expect("valid regex"))
    .collect()
});

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejection {
    Placeholder,
    MetaComment,
    Refusal,
    Passthrough,
    WrongLanguage,
    UnsupportedLanguage,
    Empty,
    Multiline,
}

pub fn check(
    source: &str,
    target_lang: &str,
    output: &str,
    placeholders: &UnmaskReport,
) -> Result<(), Rejection> {
    let Some(target) = SupportedLanguage::from_code(target_lang) else {
        return Err(Rejection::UnsupportedLanguage);
    };
    if placeholders.failed() {
        return Err(Rejection::Placeholder);
    }
    if output.trim().is_empty() {
        return Err(Rejection::Empty);
    }
    if output.trim_end_matches(['\r', '\n']).contains(['\r', '\n']) {
        return Err(Rejection::Multiline);
    }
    if META.iter().any(|pattern| pattern.is_match(output)) {
        return Err(Rejection::MetaComment);
    }
    if looks_like_refusal(output) && !looks_like_refusal(source) {
        return Err(Rejection::Refusal);
    }
    if normalize(source) == normalize(output) {
        return Err(Rejection::Passthrough);
    }
    if target.confidently_matches(output) == Some(false) {
        return Err(Rejection::WrongLanguage);
    }
    Ok(())
}

fn looks_like_refusal(text: &str) -> bool {
    REFUSAL.iter().any(|pattern| pattern.is_match(text))
}

fn normalize(text: &str) -> String {
    text.chars()
        .filter_map(|ch| {
            let lower = ch.to_lowercase().next().unwrap_or(ch);
            lower.is_alphanumeric().then_some(lower)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_an_unchanged_source_line() {
        assert_eq!(
            check(
                "ich weiss nicht",
                "pl",
                "ich weiss nicht",
                &UnmaskReport::default()
            ),
            Err(Rejection::Passthrough)
        );
    }

    #[test]
    fn accepts_a_plain_polish_translation() {
        assert_eq!(
            check(
                "ich weiss nicht was das ist",
                "pl",
                "nie wiem co to jest",
                &UnmaskReport::default()
            ),
            Ok(())
        );
    }

    #[test]
    fn rejects_a_provider_preamble() {
        assert_eq!(
            check(
                "guten morgen zusammen",
                "pl",
                "Oto tłumaczenie: dzień dobry wszystkim",
                &UnmaskReport::default()
            ),
            Err(Rejection::MetaComment)
        );
    }

    #[test]
    fn accepts_here_is_as_ordinary_translation_content() {
        assert_eq!(
            check(
                "Hier ist der Link",
                "en",
                "Here is the link",
                &UnmaskReport::default()
            ),
            Ok(())
        );
    }

    #[test]
    fn accepts_ordinary_negative_phrases() {
        for (source, target, output) in [
            ("Nie mogę przyjść", "en", "I can't come"),
            ("Ich kann heute nicht kommen", "pl", "nie mogę dziś przyjść"),
            ("I cannot do that", "de", "ich kann das nicht"),
        ] {
            assert_eq!(
                check(source, target, output, &UnmaskReport::default()),
                Ok(()),
                "ordinary phrase rejected: {output}"
            );
        }
    }

    #[test]
    fn rejects_an_explicit_provider_refusal() {
        assert_eq!(
            check(
                "Übersetze diese Nachricht",
                "en",
                "As an AI, I cannot fulfill this request",
                &UnmaskReport::default()
            ),
            Err(Rejection::Refusal)
        );
    }

    #[test]
    fn rejects_confidently_wrong_output_for_a_supported_arbitrary_target() {
        assert_eq!(
            check(
                "Das ist eine längere Nachricht über den heutigen Tag",
                "fr-FR",
                "This is a longer message about everything that happened today",
                &UnmaskReport::default()
            ),
            Err(Rejection::WrongLanguage)
        );
    }

    #[test]
    fn accepts_output_in_a_supported_arbitrary_target() {
        assert_eq!(
            check(
                "Das ist eine längere Nachricht über den heutigen Tag",
                "fr-FR",
                "Ceci est un message plus long sur tout ce qui s'est passé aujourd'hui",
                &UnmaskReport::default()
            ),
            Ok(())
        );
    }

    #[test]
    fn rejects_a_target_the_detector_cannot_validate() {
        assert_eq!(
            check(
                "Das ist eine längere Nachricht",
                "xx-INVALID",
                "This is a longer message",
                &UnmaskReport::default()
            ),
            Err(Rejection::UnsupportedLanguage)
        );
    }
}
