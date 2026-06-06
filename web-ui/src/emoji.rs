//! Thin helpers over the `emojis` crate for the web UTF-8 emoji picker.

/// Categories shown as tabs, in display order. Each maps to an `emojis::Group`.
pub const GROUPS: &[(&str, emojis::Group)] = &[
    ("Smileys", emojis::Group::SmileysAndEmotion),
    ("People", emojis::Group::PeopleAndBody),
    ("Animals", emojis::Group::AnimalsAndNature),
    ("Food", emojis::Group::FoodAndDrink),
    ("Travel", emojis::Group::TravelAndPlaces),
    ("Activities", emojis::Group::Activities),
    ("Objects", emojis::Group::Objects),
    ("Symbols", emojis::Group::Symbols),
    ("Flags", emojis::Group::Flags),
];

/// Emoji characters in a group, in the crate's canonical order.
pub fn in_group(group: emojis::Group) -> Vec<&'static str> {
    group.emojis().map(emojis::Emoji::as_str).collect()
}

/// All emoji whose name or any shortcode contains the (case-insensitive) needle.
pub fn search(needle: &str) -> Vec<&'static str> {
    let n = needle.trim().to_ascii_lowercase();
    if n.is_empty() {
        return Vec::new();
    }
    emojis::iter()
        .filter(|e| {
            e.name().to_ascii_lowercase().contains(&n)
                || e.shortcodes().any(|s| s.to_ascii_lowercase().contains(&n))
        })
        .map(emojis::Emoji::as_str)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_finds_grinning_face() {
        assert!(search("grinning").iter().any(|&e| e == "\u{1F600}"));
    }

    #[test]
    fn search_matches_shortcode() {
        // ":+1:" / "thumbsup" → 👍
        assert!(search("thumbsup").iter().any(|&e| e == "\u{1F44D}"));
    }

    #[test]
    fn empty_search_is_empty() {
        assert!(search("").is_empty());
    }

    #[test]
    fn every_group_has_emoji() {
        for (_, g) in GROUPS {
            assert!(!in_group(*g).is_empty());
        }
    }
}
