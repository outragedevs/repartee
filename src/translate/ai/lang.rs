use std::sync::LazyLock;

use regex::Regex;

static TOKEN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[^\W\d_]+").expect("valid regex"));
static STRIP: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i:https?)://\S+|__[A-Z]+\d+__|«[A-Z]+\d+»|[#@]\S+")
        .expect("valid regex")
});
static DE_TRANSLIT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b\w*(?:fuer|ueber|koenn|moecht|schoen|muess|waere|haett|laeuft|luefter|gruen|hoer|zurueck|natuerlich|spaet|naechst|aehnlich|stueck|wuerd)\w*\b",
    )
    .expect("valid regex")
});

const DE_STOP: &str = "der die das den dem des ein eine einen einem eines und oder aber nicht ist sind war waren bin bist hab habe hast hat haben hatte hatten wird werden wurde kann kannst koennen koennte muss musst muessen soll sollte will willst wollen ich du er sie es wir ihr mir mich dir dich uns euch sich mein dein sein ihre nen ne noch schon mal auch nur wenn dann weil dass denn doch halt eben nein wie was wer wo warum hier da dort auf in an mit von zu zum zur fuer ueber unter bei nach aus vor um ohne gegen so sehr mehr weniger immer nie jetzt heute gestern morgen gut schlecht viel wenig alles nichts etwas man beim vom im am zwar sondern also gerade wieder keine kein nix garnix vielleicht wirklich eigentlich einfach";
const PL_STOP: &str = "nie tak jest sa byc byl byla bylo ale to co sie na w z do od za po przez o jak zeby aby juz tylko jeszcze bardzo mozna trzeba ma mam masz mamy maja mnie ci ty ja on ona ono my wy oni tego tej tym tych ktory ktora ktore gdzie kiedy dlaczego teraz dzis wczoraj jutro dobrze zle duzo malo wszystko nic cos ktos i albo lub wiec no tez ten ta te przy pod nad bez dla oraz czy bo jesli gdy jednak wlasnie chyba moze musi bedzie beda byli mial miala mieli";
const EN_STOP: &str = "the of and to is are was were be been you it that for with have has had this these those not but or if then because what when where how why there here they them their we our your my do does did can could would should will just now about from into out up down all some any more most very like get got make no so as at on in by an a i me he she his her him one two new good well time man made which who its only also than over after such";

const DE_NGRAMS: &[(&str, f32)] = &[
    ("sch", 1.0),
    ("cht", 1.0),
    ("ung", 0.8),
    ("eit", 0.6),
    ("ich", 0.6),
    ("ein", 0.5),
    ("aeh", 1.0),
    ("oe", 0.7),
    ("ue", 0.7),
    ("ae", 0.5),
];
const PL_NGRAMS: &[(&str, f32)] = &[
    ("cz", 1.0),
    ("sz", 1.0),
    ("rz", 0.9),
    ("prz", 1.2),
    ("dz", 0.7),
    ("scz", 1.0),
    ("ie", 0.3),
    ("wia", 0.6),
    ("nia", 0.6),
];
const EN_NGRAMS: &[(&str, f32)] = &[
    ("the", 1.0),
    ("ing", 1.0),
    ("tion", 1.2),
    ("ough", 1.2),
    ("wh", 0.4),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KnownLanguage {
    De,
    Pl,
    En,
}

impl KnownLanguage {
    pub fn from_code(code: &str) -> Option<Self> {
        match code
            .trim()
            .split(['-', '_'])
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str()
        {
            "de" | "deu" => Some(Self::De),
            "pl" | "pol" => Some(Self::Pl),
            "en" | "eng" => Some(Self::En),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SupportedLanguage {
    detector: whatlang::Lang,
    known: Option<KnownLanguage>,
}

impl SupportedLanguage {
    pub fn from_code(code: &str) -> Option<Self> {
        let primary = code
            .trim()
            .split(['-', '_'])
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        let iso = match primary.len() {
            2 => isolang::Language::from_639_1(&primary),
            3 => isolang::Language::from_639_3(&primary),
            _ => None,
        }?;
        let detector_code = match iso.to_639_3() {
            "fas" => "pes",
            "nor" => "nob",
            "zho" => "cmn",
            code => code,
        };
        Some(Self {
            detector: whatlang::Lang::from_code(detector_code)?,
            known: KnownLanguage::from_code(&primary),
        })
    }

    pub fn confidently_matches(self, text: &str) -> Option<bool> {
        if let Some(known) = self.known {
            let detected = detect(text);
            return (!detected.uncertain).then_some(detected.language == Some(known));
        }
        let stripped = STRIP.replace_all(text, " ");
        let detected = whatlang::detect(&stripped)?;
        detected
            .is_reliable()
            .then_some(detected.lang() == self.detector)
    }
}

pub fn supports(code: &str) -> bool {
    SupportedLanguage::from_code(code).is_some()
}

#[derive(Debug, Clone, Copy)]
pub struct Detection {
    pub language: Option<KnownLanguage>,
    pub uncertain: bool,
    top_score: f32,
    margin: f32,
    token_count: usize,
}

impl Detection {
    pub fn likely_matches(self, language: KnownLanguage) -> bool {
        self.language == Some(language)
            && (!self.uncertain
                || (self.token_count >= 2 && self.top_score >= 1.0 && self.margin >= 0.3))
    }
}

pub fn detect(text: &str) -> Detection {
    let lower = STRIP.replace_all(text, " ").to_lowercase();
    let tokens: Vec<String> = TOKEN.find_iter(&lower).map(|m| fold(m.as_str())).collect();
    let mut scores = [0.0_f32; 3];

    for ch in lower.chars() {
        if "ąćęłńóśźż".contains(ch) {
            scores[1] += 2.0;
        } else if "äöüß".contains(ch) {
            scores[0] += 2.0;
        }
    }

    for token in &tokens {
        let present = [
            contains_word(DE_STOP, token),
            contains_word(PL_STOP, token),
            contains_word(EN_STOP, token),
        ];
        let count = present.iter().filter(|&&hit| hit).count();
        if count > 0 {
            let weight = match count {
                1 => 1.0,
                2 => 0.5,
                _ => 1.0 / 3.0,
            };
            for (score, hit) in scores.iter_mut().zip(present) {
                if hit {
                    *score += weight;
                }
            }
        }
    }

    let joined = tokens.join(" ");
    add_ngrams(&mut scores[0], &joined, DE_NGRAMS);
    add_ngrams(&mut scores[1], &joined, PL_NGRAMS);
    add_ngrams(&mut scores[2], &joined, EN_NGRAMS);
    let transliterated = u16::try_from(DE_TRANSLIT.find_iter(&lower).count()).unwrap_or(u16::MAX);
    scores[0] += 2.0 * f32::from(transliterated);

    let mut ranked = [
        (KnownLanguage::De, scores[0]),
        (KnownLanguage::Pl, scores[1]),
        (KnownLanguage::En, scores[2]),
    ];
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1));
    let total: f32 = scores.iter().sum();
    let margin = if total > 0.0 {
        (ranked[0].1 - ranked[1].1) / total
    } else {
        0.0
    };

    Detection {
        language: (total > 0.0).then_some(ranked[0].0),
        uncertain: tokens.len() < 4 || total < 2.5 || margin < 0.2,
        top_score: ranked[0].1,
        margin,
        token_count: tokens.len(),
    }
}

fn contains_word(words: &str, needle: &str) -> bool {
    words.split_whitespace().any(|word| word == needle)
}

fn add_ngrams(score: &mut f32, text: &str, ngrams: &[(&str, f32)]) {
    for &(needle, weight) in ngrams {
        let occurrences = u16::try_from(text.match_indices(needle).count()).unwrap_or(u16::MAX);
        *score += f32::from(occurrences) * weight;
    }
}

fn fold(text: &str) -> String {
    let mut folded = String::with_capacity(text.len());
    for ch in text.chars() {
        folded.push_str(match ch {
            'ą' | 'ä' => "a",
            'ć' => "c",
            'ę' => "e",
            'ł' => "l",
            'ń' => "n",
            'ó' | 'ö' => "o",
            'ś' => "s",
            'ź' | 'ż' => "z",
            'ü' => "u",
            'ß' => "ss",
            _ => {
                folded.push(ch);
                continue;
            }
        });
    }
    folded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_confident_polish_text() {
        let result = detect("nie wiem co teraz będzie dobrze");
        assert_eq!(result.language, Some(KnownLanguage::Pl));
    }

    #[test]
    fn treats_short_text_as_uncertain() {
        assert!(detect("nie wiem").uncertain);
    }

    #[test]
    fn recognizes_a_decisive_short_target_phrase() {
        let detected = detect("nie wiem");
        assert!(
            detected.likely_matches(KnownLanguage::Pl),
            "unexpected detection: {detected:?}"
        );
    }

    #[test]
    fn does_not_trust_an_ambiguous_single_word() {
        assert!(!detect("ja").likely_matches(KnownLanguage::Pl));
    }

    #[test]
    fn resolves_iso_codes_and_regional_tags_supported_by_the_detector() {
        assert_eq!(
            ["es", "fra", "zh-CN", "fa_IR", "no-NO"]
                .map(SupportedLanguage::from_code)
                .map(|language| language.is_some()),
            [true; 5]
        );
    }

    #[test]
    fn rejects_codes_the_detector_cannot_validate() {
        assert!(!supports("xx-INVALID"));
    }

    #[test]
    fn recognizes_two_and_three_letter_known_language_codes() {
        assert_eq!(
            ["de", "deu", "pl", "pol", "en", "eng"].map(KnownLanguage::from_code),
            [
                Some(KnownLanguage::De),
                Some(KnownLanguage::De),
                Some(KnownLanguage::Pl),
                Some(KnownLanguage::Pl),
                Some(KnownLanguage::En),
                Some(KnownLanguage::En),
            ]
        );
    }
}
