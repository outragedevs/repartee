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
        r"(?i)^\s*sudo\s+(?:-\S+\s+)*\S+",
        r"(?i)^\s*git\s+(?:add|bisect|branch|checkout|clone|commit|diff|fetch|grep|init|log|merge|mv|pull|push|rebase|remote|reset|restore|revert|show|stash|status|switch|tag|worktree)\b",
        r"(?i)^\s*make(?:\s+(?:all|build|check|clean|clippy|docs|fmt|install|release|test|wasm|web))?\s*$",
        r"(?i)^\s*(?:apt|apt-get)\s+(?:install|remove|update|upgrade|search|show|purge)\b",
        r"(?i)^\s*(?:docker\s+(?:build|run|compose|exec|images|logs|ps|pull|push|stop)|systemctl\s+(?:start|stop|restart|status|enable|disable|daemon-reload))\b",
        r"(?i)^\s*(?:cd|ls|cat|grep|awk|sed|gcc|nm|strip|chmod|ssh|scp|curl|wget|dd|mount|lsblk|dmesg)\s+(?:--?\S+|[./~]\S+|\S+=\S+)",
        r"(?i)\|\s*(?:wc|grep|head|tail|sort|uniq|less)\b",
        r"^\s*\d+\s*$",
        r"^[\w./-]+:\d+:\d+:\s",
    ]
    .into_iter()
    .map(|pattern| Regex::new(pattern).expect("valid regex"))
    .collect()
});
static SHORT_NOISE: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    "haha hehe hihi lol rofl lmao xd xdd"
        .split_whitespace()
        .collect()
});
static TECHNICAL_PROSE: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    "use uses need needs buy buys get gets want wants have has is are ist sind brauche braucht nutze nutzt użyj"
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
        || is_technical_only(text)
    {
        return true;
    }

    if let Some(target) = KnownLanguage::from_code(&req.target_lang) {
        let detected = lang::detect(text);
        if detected.likely_matches(target) {
            return true;
        }
    }

    req.direction == Direction::Incoming && is_short_noise(text)
}

fn is_technical_only(text: &str) -> bool {
    let tokens: Vec<&str> = text.split_whitespace().collect();
    if tokens.is_empty()
        || tokens.len() > 3
        || tokens
            .iter()
            .any(|token| TECHNICAL_PROSE.contains(token.to_ascii_lowercase().as_str()))
    {
        return false;
    }
    let mut has_letter = false;
    let mut has_digit = false;
    for token in tokens {
        if !token
            .chars()
            .all(|ch| ch.is_alphanumeric() || matches!(ch, '-' | '+' | '.' | '_' | '/'))
        {
            return false;
        }
        let token_has_letter = token.chars().any(char::is_alphabetic);
        let token_has_digit = token.chars().any(|ch| ch.is_ascii_digit());
        if token_has_letter
            && !token_has_digit
            && !token.chars().next().is_some_and(char::is_uppercase)
        {
            return false;
        }
        has_letter |= token_has_letter;
        has_digit |= token_has_digit;
    }
    has_letter && has_digit && lang::detect(text).language.is_none()
}

fn words(text: &str) -> Vec<&str> {
    TOKEN.find_iter(text).map(|m| m.as_str()).collect()
}

fn is_emote_only(text: &str) -> bool {
    words(text).is_empty()
}

fn is_short_noise(text: &str) -> bool {
    let words = words(text);
    !words.is_empty()
        && words.len() < 3
        && words
            .iter()
            .all(|word| SHORT_NOISE.contains(word.to_ascii_lowercase().as_str()))
}

fn is_onomatopoeia(text: &str) -> bool {
    let words = words(text);
    if words.is_empty() {
        return false;
    }
    words
        .iter()
        .all(|word| is_stretched(word) || is_reduplicated(word) || is_known_noise(word))
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
        run >= 4
    })
}

fn is_reduplicated(word: &str) -> bool {
    let lower: Vec<char> = word.to_lowercase().chars().collect();
    (2..=6).any(|width| {
        lower.len() >= width * 3
            && lower.len().is_multiple_of(width)
            && lower[width..]
                .chunks(width)
                .all(|chunk| chunk == &lower[..chunk.len()])
    })
}

fn is_known_noise(word: &str) -> bool {
    SHORT_NOISE.contains(word.to_ascii_lowercase().as_str())
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
            deadline: None,
            known_nicks: Vec::new(),
        }
    }

    #[test]
    fn filters_short_incoming_noise() {
        assert!(should_filter(&request(Direction::Incoming, "lol")));
    }

    #[test]
    fn keeps_short_outgoing_phrases() {
        assert!(!should_filter(&request(
            Direction::Outgoing,
            "good morning"
        )));
    }

    #[test]
    fn keeps_short_incoming_phrases_that_carry_meaning() {
        for text in ["guten Morgen", "keine Ahnung"] {
            assert!(
                !should_filter(&request(Direction::Incoming, text)),
                "meaningful phrase filtered: {text}"
            );
        }
    }

    #[test]
    fn filters_urls_before_calling_a_provider() {
        assert!(should_filter(&request(
            Direction::Incoming,
            "https://example.com/x"
        )));
    }

    #[test]
    fn filters_a_short_phrase_already_in_a_three_letter_target_language() {
        let mut req = request(Direction::Outgoing, "nie wiem");
        req.target_lang = "pol".to_string();
        assert!(should_filter(&req));
    }

    #[test]
    fn filters_a_technical_product_name() {
        assert!(should_filter(&request(Direction::Outgoing, "Shure SM58")));
    }

    #[test]
    fn keeps_ordinary_prose_that_mentions_a_product_number() {
        assert!(!should_filter(&request(Direction::Outgoing, "Use SM58")));
    }

    #[test]
    fn keeps_palindromes_that_are_not_reduplicated() {
        assert!(!should_filter(&request(
            Direction::Outgoing,
            "level radar"
        )));
    }

    #[test]
    fn keeps_make_sure_as_ordinary_prose() {
        assert!(!should_filter(&request(
            Direction::Outgoing,
            "make sure this works"
        )));
    }

    #[test]
    fn keeps_words_with_lexical_letter_repetition() {
        for text in ["mama kommt", "Schifffahrt beginnt"] {
            assert!(
                !should_filter(&request(Direction::Outgoing, text)),
                "ordinary prose filtered: {text}"
            );
        }
    }

    #[test]
    fn filters_unambiguous_repeated_chat_noise() {
        assert!(should_filter(&request(
            Direction::Outgoing,
            "hahaha brrrrt"
        )));
    }
}
