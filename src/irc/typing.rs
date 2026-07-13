//! `IRCv3` `typing` client tag — <https://ircv3.net/specs/client-tags/typing>.
//!
//! Typing status rides on `TAGMSG` and depends only on the `message-tags`
//! capability, which repartee already negotiates. This module is the pure
//! protocol layer: no state, no I/O, no clock.

use std::collections::HashMap;
use std::time::Duration;

use crate::irc::formatting::is_channel;

/// The ratified tag name — the only one we ever send.
pub const TAG: &str = "+typing";

/// The pre-ratification name. Accepted on receive for interop with older
/// clients; never sent, because emitting both would double our TAGMSG volume
/// and therefore our flood cost.
pub const TAG_LEGACY: &str = "+draft/typing";

/// Spec: "Input event handlers MUST be throttled so that any `typing`
/// notification is not sent within 3 seconds of another one for a given target."
/// This binds `done` too, not just `active` — see §4.5.
pub const THROTTLE: Duration = Duration::from_secs(3);

/// Spec: a receiver assumes typing has stopped once "at least 6 seconds have
/// passed since the last `typing=active` notification was received".
pub const ACTIVE_TTL: Duration = Duration::from_secs(6);

/// Spec: ditto, "at least 30 seconds" for `typing=paused`.
pub const PAUSED_TTL: Duration = Duration::from_secs(30);

/// The three values the `typing` tag may carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypingState {
    Active,
    Paused,
    Done,
}

impl TypingState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Paused => "paused",
            Self::Done => "done",
        }
    }

    /// Values are matched exactly. The message-tags spec makes tag names
    /// "case-sensitive opaque identifiers" and nothing licenses folding values,
    /// so `ACTIVE` is not `active`.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "active" => Some(Self::Active),
            "paused" => Some(Self::Paused),
            "done" => Some(Self::Done),
            _ => None,
        }
    }

    /// How long a received state survives without a refresh. `Done` is not a
    /// state that lingers — it is an instruction to forget.
    #[must_use]
    pub const fn ttl(self) -> Option<Duration> {
        match self {
            Self::Active => Some(ACTIVE_TTL),
            Self::Paused => Some(PAUSED_TTL),
            Self::Done => None,
        }
    }
}

/// Read a typing state out of already-extracted message tags.
///
/// The ratified `+typing` wins over the legacy `+draft/typing`. An absent,
/// empty, or unrecognised value yields `None`: the message-tags spec makes
/// empty and missing values equivalent, and neither carries a state.
#[must_use]
pub fn parse_typing(tags: &HashMap<String, String>) -> Option<TypingState> {
    tags.get(TAG)
        .or_else(|| tags.get(TAG_LEGACY))
        .map(String::as_str)
        .and_then(TypingState::parse)
}

/// Build `@+typing=<state> TAGMSG <target>`.
///
/// `TAGMSG` has no `Command` variant in `irc-proto-repartee`, so it rides
/// `Command::Raw`, which stringifies verbatim. The crate's `Display for Message`
/// serializes the tag with spec-correct escaping — see spec §2.2/§2.3.
#[must_use]
pub fn build_tagmsg(target: &str, state: TypingState) -> irc::proto::Message {
    irc::proto::Message {
        tags: Some(vec![irc::proto::message::Tag(
            TAG.to_string(),
            Some(state.as_str().to_string()),
        )]),
        prefix: None,
        command: irc::proto::Command::Raw("TAGMSG".to_string(), vec![target.to_string()]),
    }
}

/// The slash commands that are *messages* rather than commands, with the
/// trailing space that proves they carry text.
///
/// `/action` is a registered alias of `/me` (`src/commands/registry.rs`), so it
/// produces exactly the same CTCP ACTION on the wire. Anything that is true of
/// one must be true of the other, or composing `/action waves` announces no
/// typing while `/me waves` does.
const MESSAGE_COMMANDS: [&str; 2] = ["/me ", "/action "];

/// Whether the current input text should produce typing notifications.
///
/// Spec: `typing=active` is sent "while the user is making updates to the
/// text-input field **and the text is not a '/slash command'**". `/me` (and its
/// `/action` alias) is a message rather than a command, so it counts as typing;
/// a bare `/me` with no text does not.
///
/// The command parser lowercases command names before dispatch
/// (`src/commands/parser.rs`), so `/ME waves` executes as a `/me` action —
/// this predicate must match it case-insensitively too, or an action typed in
/// caps would be misclassified as a command and never announce typing.
#[must_use]
pub fn should_type(input: &str) -> bool {
    if input.is_empty() {
        return false;
    }
    !input.starts_with('/')
        || MESSAGE_COMMANDS.iter().any(|cmd| {
            input
                .get(..cmd.len())
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case(cmd))
        })
}

/// Resolve a `TAGMSG` target that may carry a `STATUSMSG` prefix (`@#chan`).
///
/// Deliberately conservative, because `is_channel` accepts `#`, `&`, `+` **and**
/// `!`: `&local` and `+modeless` are *channels*, not status-prefixed targets.
/// So: strip at most one leading character, only if the server advertised it in
/// `STATUSMSG`, and only if what remains is still a channel. Anything else is
/// returned untouched.
#[must_use]
pub fn strip_statusmsg<'a>(target: &'a str, statusmsg: &str) -> &'a str {
    if statusmsg.is_empty() {
        return target;
    }
    let mut chars = target.chars();
    let Some(first) = chars.next() else {
        return target;
    };
    if !statusmsg.contains(first) {
        return target;
    }
    let rest = chars.as_str();
    if is_channel(rest) {
        rest
    } else if matches!(first, '#' | '&' | '+' | '!') {
        // First is both a channel prefix and statusmsg prefix (ambiguous).
        // Don't strip unless remainder is clearly a channel.
        target
    } else {
        // First is statusmsg-only (like @), so it's safe to strip.
        rest
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_round_trips_through_str() {
        for s in [TypingState::Active, TypingState::Paused, TypingState::Done] {
            assert_eq!(TypingState::parse(s.as_str()), Some(s));
        }
    }

    #[test]
    fn parse_rejects_junk_and_wrong_case() {
        // Tag values are matched exactly; nothing licenses case folding.
        assert_eq!(TypingState::parse("ACTIVE"), None);
        assert_eq!(TypingState::parse("typing"), None);
        assert_eq!(TypingState::parse(""), None);
        assert_eq!(TypingState::parse("active "), None);
    }

    #[test]
    fn ttls_match_the_spec() {
        assert_eq!(TypingState::Active.ttl(), Some(Duration::from_secs(6)));
        assert_eq!(TypingState::Paused.ttl(), Some(Duration::from_secs(30)));
        assert_eq!(TypingState::Done.ttl(), None);
    }

    #[test]
    fn parse_typing_reads_the_ratified_tag() {
        let tags = HashMap::from([("+typing".to_string(), "active".to_string())]);
        assert_eq!(parse_typing(&tags), Some(TypingState::Active));
    }

    #[test]
    fn parse_typing_accepts_the_legacy_draft_tag() {
        // Pre-ratification clients still emit `+draft/typing` (spec §1.4).
        let tags = HashMap::from([("+draft/typing".to_string(), "paused".to_string())]);
        assert_eq!(parse_typing(&tags), Some(TypingState::Paused));
    }

    #[test]
    fn parse_typing_ignores_absent_empty_and_unknown() {
        assert_eq!(parse_typing(&HashMap::new()), None);
        let empty = HashMap::from([("+typing".to_string(), String::new())]);
        assert_eq!(parse_typing(&empty), None);
        let junk = HashMap::from([("+typing".to_string(), "wat".to_string())]);
        assert_eq!(parse_typing(&junk), None);
    }

    #[test]
    fn build_tagmsg_matches_the_spec_wire_format() {
        let msg = build_tagmsg("#rust", TypingState::Active);
        assert_eq!(msg.to_string(), "@+typing=active TAGMSG #rust\r\n");
        let msg = build_tagmsg("alice", TypingState::Done);
        assert_eq!(msg.to_string(), "@+typing=done TAGMSG alice\r\n");
    }

    #[test]
    fn build_tagmsg_round_trips_through_the_parser() {
        // Guards the assumption the whole feature rests on: the crate's Display
        // serializes tags, and its FromStr reads them back.
        let wire = build_tagmsg("#rust", TypingState::Paused).to_string();
        let parsed: irc::proto::Message = wire.parse().expect("parses");
        let tags = parsed
            .tags
            .expect("has tags")
            .into_iter()
            .filter_map(|t| Some((t.0, t.1?)))
            .collect::<HashMap<String, String>>();
        assert_eq!(parse_typing(&tags), Some(TypingState::Paused));
    }

    #[test]
    fn should_type_excludes_slash_commands_but_not_actions() {
        assert!(should_type("hello"));
        assert!(should_type("/me waves")); // an action is a message
        assert!(!should_type(""));
        assert!(!should_type("/join #rust"));
        assert!(!should_type("/me")); // bare command, no text
    }

    #[test]
    fn should_type_recognizes_me_regardless_of_case() {
        // The command parser lowercases command names (parser.rs), so `/ME
        // waves` executes as a /me action — the predicate must match it too,
        // or an action typed in caps never announces typing.
        assert!(should_type("/ME waves"));
        assert!(!should_type("/ME")); // bare command, no text, still not typing
    }

    #[test]
    fn should_type_recognizes_the_action_alias_of_me() {
        // `/action` is a registered alias of `/me` (registry.rs) and produces
        // the identical CTCP ACTION. Treating it as a plain command meant
        // `/action waves` announced no typing at all while `/me waves` did.
        assert!(should_type("/action waves"));
        assert!(should_type("/ACTION waves")); // the parser lowercases the verb
        assert!(should_type("/AcTiOn waves"));
        assert!(!should_type("/action")); // bare command, no text
        // A longer command that merely starts with the same letters is still a
        // command: the trailing space in the pattern is what separates them.
        assert!(!should_type("/actionfoo bar"));
        assert!(!should_type("/mention bob"));
    }

    #[test]
    fn strip_statusmsg_removes_one_advertised_prefix() {
        assert_eq!(strip_statusmsg("@#rust", "@+"), "#rust");
        assert_eq!(strip_statusmsg("+#rust", "@+"), "#rust");
    }

    #[test]
    fn strip_statusmsg_leaves_real_channel_prefixes_alone() {
        // `&` and `+` are CHANNEL prefixes (is_channel accepts # & + !).
        // Stripping them would turn a channel into a query.
        assert_eq!(strip_statusmsg("&local", "@+"), "&local");
        assert_eq!(strip_statusmsg("+modeless", "@+"), "+modeless");
        assert_eq!(strip_statusmsg("#rust", "@+"), "#rust");
    }

    #[test]
    fn strip_statusmsg_strips_at_most_one_character() {
        // `@@#rust` is not a thing; strip one, and only if what's left is a channel.
        assert_eq!(strip_statusmsg("@@#rust", "@+"), "@#rust");
        // Nothing is stripped when the server advertises no STATUSMSG.
        assert_eq!(strip_statusmsg("@#rust", ""), "@#rust");
        // A nick is left alone.
        assert_eq!(strip_statusmsg("alice", "@+"), "alice");
    }
}
