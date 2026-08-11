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
        r"(?im)^\s*(?:(?:voici|ci-dessous)\b.{0,30}?\b)?(?:la\s+)?traduction\s*:",
        r"(?im)^\s*(?:(?:aqu[ií]\s+(?:est[aá]|tienes)|a\s+continuaci[oó]n)\b.{0,30}?\b)?(?:la\s+)?traducci[oó]n\s*:",
        r"(?m)^\s*(?:(?:以下|这是).{0,12})?(?:翻译|翻譯|译文|譯文)(?:如下)?\s*[：:]",
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
        r"(?i)\ben\s+tant\s+qu(?:['’](?:une?\s+)?)?(?:IA|intelligence\s+artificielle|mod[eè]le(?:\s+de\s+langage)?)\b",
        r"(?im)^\s*je\s+ne\s+(?:peux|suis\s+pas\s+en\s+mesure)\b.{0,80}\b(?:demande|contenu|texte)\w*\b",
        r"(?i)\bcomo\s+(?:una?\s+)?(?:IA|inteligencia\s+artificial|modelo(?:\s+de\s+lenguaje)?)\b",
        r"(?im)^\s*no\s+(?:puedo|soy\s+capaz\s+de)\b.{0,80}\b(?:solicitud|contenido|texto)\w*\b",
        r"(?i)作为(?:一个|一名)?(?:人工智能|AI|语言模型)",
        r"(?m)^\s*我(?:无法|不能).{0,80}(?:请求|內容|内容|文本)",
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
        for (target, output) in [
            ("pl", "Oto tłumaczenie: dzień dobry wszystkim"),
            ("en", "Translation: good morning everyone"),
            ("de", "Übersetzung: Guten Morgen zusammen"),
            ("fr", "Voici la traduction : bonjour à tous"),
            ("es", "Aquí está la traducción: buenos días a todos"),
            ("zh", "翻译如下：大家早上好"),
        ] {
            assert_eq!(
                check(
                    "source text requiring translation",
                    target,
                    output,
                    &UnmaskReport::default()
                ),
                Err(Rejection::MetaComment),
                "provider preamble accepted for {target}: {output}"
            );
        }
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
        for (target, output) in [
            ("en", "As an AI, I cannot fulfill this request"),
            ("pl", "Nie mogę zrealizować tej prośby"),
            ("de", "Ich kann diese Anfrage nicht erfüllen"),
            ("fr", "Je ne peux pas satisfaire cette demande"),
            ("es", "No puedo cumplir esta solicitud"),
            ("zh", "我无法满足这个请求"),
        ] {
            assert_eq!(
                check(
                    "source text requiring translation",
                    target,
                    output,
                    &UnmaskReport::default()
                ),
                Err(Rejection::Refusal),
                "provider refusal accepted for {target}: {output}"
            );
        }
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
