use std::sync::LazyLock;

use regex::Regex;

use super::lang::{self, KnownLanguage};
use super::mask::UnmaskReport;

static META: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"(?im)^\s*(?:oto|poni[żz]ej|tutaj)\b.{0,30}?\b(?:t[łl]umaczenie|przek[łl]ad)",
        r"(?im)^\s*here\s+(?:is|are|'s)\b",
        r"(?im)^\s*(?:the\s+)?translation\s*:",
        r"(?im)^\s*(?:t[łl]umaczenie|przek[łl]ad|[üu]bersetzung)\s*:",
        r"(?im)^\s*(?:uwaga|note|hinweis|anmerkung|uwagi)\s*:",
        r"(?i)[(\[]\s*(?:note|uwaga|hinweis|anmerkung|nb)\s*[:.]",
        r"(?i)\b(?:the\s+)?(?:line|text|sentence)\s+is\s+(?:already|written)\b",
        r"(?i)\b(?:per|as\s+per)\s+instructions?\b",
        r"(?i)\b(?:wariant|opcja|option|alternatyw\w*)\s*\d",
    ]
    .into_iter()
    .map(|pattern| Regex::new(pattern).expect("valid regex"))
    .collect()
});
static REFUSAL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(?:nie\s+mog[ęe]|nie\s+jestem\s+w\s+stanie|nie\s+b[ęe]d[ęe]|I\s+(?:cannot|can't|won't|am\s+unable|'m\s+unable)|inappropriate|nieodpowiedni\w*|obra[źz]liw\w*|as\s+an\s+AI|jako\s+(?:model|sztuczna)|ich\s+kann\s+(?:das\s+)?nicht)\b",
    )
    .expect("valid regex")
});

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejection {
    Placeholder,
    MetaComment,
    Refusal,
    Passthrough,
    WrongLanguage,
    Empty,
    Multiline,
}

pub fn check(
    source: &str,
    target_lang: &str,
    output: &str,
    placeholders: &UnmaskReport,
) -> Result<(), Rejection> {
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
    if REFUSAL.is_match(output) {
        return Err(Rejection::Refusal);
    }
    if normalize(source) == normalize(output) {
        return Err(Rejection::Passthrough);
    }
    if let Some(target) = KnownLanguage::from_code(target_lang) {
        let detected = lang::detect(output);
        if !detected.uncertain && detected.language != Some(target) {
            return Err(Rejection::WrongLanguage);
        }
    }
    Ok(())
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
}
