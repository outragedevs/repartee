use std::sync::LazyLock;

use regex::Regex;

use super::lang::KnownLanguage;

static RECURRING: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(?:nich|nen|ned|ooch|wa|keen|icke|haste|weeste|bissu|dit|wat)\b")
        .expect("valid regex")
});
static UMLAUT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b\p{L}*(?:ae|oe|ue)\p{L}*\b").expect("valid regex"));
static ODD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b\p{L}*(?:hh|zsch|ausn|essn|eusch)\p{L}*\b").expect("valid regex")
});

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Difficulty {
    Easy,
    Strong,
}

pub fn classify(text: &str, source_lang: Option<&str>) -> Difficulty {
    if source_lang.is_some_and(|lang| KnownLanguage::from_code(lang) != Some(KnownLanguage::De)) {
        return Difficulty::Easy;
    }
    let score = RECURRING.find_iter(text).count()
        + UMLAUT.find_iter(text).count()
        + 2 * ODD.find_iter(text).count();
    if score >= 2 {
        Difficulty::Strong
    } else {
        Difficulty::Easy
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_distorted_german_to_the_strong_policy() {
        assert_eq!(
            classify("haste ooch keen bock uff die luefter", Some("de")),
            Difficulty::Strong
        );
    }

    #[test]
    fn does_not_apply_the_german_router_to_polish_input() {
        assert_eq!(classify("nie wiem co będzie", Some("pl")), Difficulty::Easy);
    }

    #[test]
    fn routes_regional_and_three_letter_german_tags_to_the_strong_policy() {
        for source_lang in ["de-DE", "de_DE", "deu"] {
            assert_eq!(
                classify("haste ooch keen bock uff die luefter", Some(source_lang)),
                Difficulty::Strong
            );
        }
    }
}
