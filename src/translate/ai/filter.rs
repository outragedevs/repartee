use std::collections::HashSet;
use std::sync::LazyLock;

use regex::Regex;

use super::lang::{self, KnownLanguage};
use crate::translate::{Direction, TranslateRequest};

static TOKEN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[^\W\d_]+").expect("valid regex"));
static URL_ONLY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^\s*(?:https?://\S+(?:\s*\[[^\]]*\])?\s*)+$").expect("valid regex")
});
static CODE: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"(?i)\w+@[\w.-]+[\s:].*\$\s",
        r"(?i)^\s*[-dlbcps][rwxstST-]{9}\s",
        r"(?i)^\s*\$\s+\S",
        r"(?i)\bfn\s+\w+\s*\(",
        r"(?i)\b(?:impl|trait|struct|enum)\s+\w+",
        r"(?i)println!|eprintln!|unwrap\(\)|-&gt;|->\s*\w+\s*\{",
        r#"(?i)#include\s*[<\"]"#,
        r"(?i)^\s*(?:def|class)\s+\w+\s*[(:]",
        r"(?i)^\s*(?:sudo|apt|apt-get|git|cd|ls|cat|grep|awk|sed|gcc|make|nm|strip|chmod|systemctl|docker|ssh|scp|curl|wget|dd|mount|lsblk|dmesg)\s+-?\w",
        r"(?i)\|\s*(?:wc|grep|head|tail|sort|uniq|less)\b",
        r"^\s*\d+\s*$",
        r"^[\w./-]+:\d+:\d+:\s",
    ]
    .into_iter()
    .map(|pattern| Regex::new(pattern).expect("valid regex"))
    .collect()
});
static VERBS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    "ist sind bin bist seid war warst waren hab habe hast hat haben hatte hatten wird wirst werden werde wurde wurden kann kannst koennen koennte konnte muss musst muessen sollte soll will willst wollen mag moechte weiss weisst wissen gibt gibts geht gehts kommt kommst kommen macht machst machen mach sagt sagen sag sieht sehen seh denke denkst glaub glaube glaubst brauch brauche braucht nimmt nimm laeuft lauft steht liegt bleibt findet find funktioniert klappt passt fehlt hilft schaut guck kuck bau baut nutzt nutze teste testet jest sa bylo byl byla mam masz ma mamy macie maja moge mozesz moze mozemy musze musisz musi wiem wiesz wie robie robisz robi ide idziesz idzie widze widzisz widzi daj dam dasz da bedzie beda mial miala mieli"
        .split_whitespace()
        .collect()
});

pub fn should_filter(req: &TranslateRequest) -> bool {
    let text = req.text.trim();
    if text.is_empty()
        || URL_ONLY.is_match(text)
        || CODE.iter().any(|pattern| pattern.is_match(text))
        || is_emote_only(text)
        || is_onomatopoeia(text)
    {
        return true;
    }

    if let Some(target) = KnownLanguage::from_code(&req.target_lang) {
        let detected = lang::detect(text);
        if !detected.uncertain && detected.language == Some(target) {
            return true;
        }
    }

    req.direction == Direction::Incoming && is_short_without_verb(text)
}

fn words(text: &str) -> Vec<&str> {
    TOKEN.find_iter(text).map(|m| m.as_str()).collect()
}

fn is_emote_only(text: &str) -> bool {
    words(text).is_empty()
}

fn is_short_without_verb(text: &str) -> bool {
    let words = words(text);
    words.len() < 3
        && !words.iter().any(|word| {
            let lower = word.to_ascii_lowercase();
            VERBS.contains(lower.as_str())
                || lower
                    .strip_suffix('s')
                    .is_some_and(|stem| VERBS.contains(stem))
        })
}

fn is_onomatopoeia(text: &str) -> bool {
    let words = words(text);
    if words.is_empty() {
        return false;
    }
    let hits = words
        .iter()
        .filter(|word| is_stretched(word) || is_reduplicated(word))
        .count();
    hits * 2 >= words.len()
}

fn is_stretched(word: &str) -> bool {
    let mut previous = None;
    let mut run = 0;
    word.chars().any(|ch| {
        if previous == Some(ch) {
            run += 1;
        } else {
            previous = Some(ch);
            run = 1;
        }
        run >= 3
    })
}

fn is_reduplicated(word: &str) -> bool {
    let lower: Vec<char> = word.to_lowercase().chars().collect();
    (2..=6).any(|width| {
        lower.len() >= width * 2
            && lower.len().is_multiple_of(width)
            && lower[width..]
                .chunks(width)
                .all(|chunk| chunk == &lower[..chunk.len()])
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(direction: Direction, text: &str) -> TranslateRequest {
        TranslateRequest {
            id: 1,
            direction,
            network: String::new(),
            target: "#x".to_string(),
            nick: "alice".to_string(),
            text: text.to_string(),
            source_lang: Some("de".to_string()),
            target_lang: "pl".to_string(),
            known_nicks: Vec::new(),
        }
    }

    #[test]
    fn filters_short_incoming_noise() {
        assert!(should_filter(&request(Direction::Incoming, "moin")));
    }

    #[test]
    fn keeps_short_outgoing_phrases() {
        assert!(!should_filter(&request(
            Direction::Outgoing,
            "good morning"
        )));
    }

    #[test]
    fn filters_urls_before_calling_a_provider() {
        assert!(should_filter(&request(
            Direction::Incoming,
            "https://example.com/x"
        )));
    }

    #[test]
    fn keeps_palindromes_that_are_not_reduplicated() {
        assert!(!should_filter(&request(
            Direction::Outgoing,
            "level radar"
        )));
    }
}
