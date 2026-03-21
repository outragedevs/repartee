use std::collections::HashMap;

/// Structured representation of ISUPPORT (005) tokens received from the server.
///
/// Servers send one or more `RPL_ISUPPORT` (005) lines during connection
/// registration.  Each line carries a list of tokens in one of three forms:
///
/// - `KEY=VALUE` — sets a parameter
/// - `KEY`       — boolean flag (present = enabled)
/// - `-KEY`      — negates / removes a previously advertised token
///
/// This struct accumulates tokens across multiple 005 lines and provides
/// typed accessors for the most commonly used parameters.
#[derive(Debug, Clone, Default)]
pub struct Isupport {
    /// Raw token storage.  For `KEY=VALUE` the value is `Some(value)`;
    /// for bare `KEY` the value is `Some("")`; removed keys are absent.
    tokens: HashMap<String, String>,
}

#[allow(dead_code)]
impl Isupport {
    /// Create a new, empty `Isupport`.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Merge tokens from a single `RPL_ISUPPORT` line.
    ///
    /// Accepts the token slice **after** stripping the nickname prefix and the
    /// trailing "are supported by this server" text — i.e. just the bare
    /// `KEY`, `KEY=VALUE`, and `-KEY` items.
    pub fn parse_tokens(&mut self, tokens: &[&str]) {
        for &token in tokens {
            if let Some(negated) = token.strip_prefix('-') {
                self.tokens.remove(negated);
            } else if let Some((key, value)) = token.split_once('=') {
                self.tokens.insert(key.to_string(), value.to_string());
            } else {
                self.tokens.insert(token.to_string(), String::new());
            }
        }
    }

    /// Parse `PREFIX=(modes)prefixes` into a vec of `(mode_char, prefix_char)`
    /// in rank order (highest privilege first).
    ///
    /// Example: `PREFIX=(ov)@+` → `[('o', '@'), ('v', '+')]`
    ///
    /// Returns the default `[('o', '@'), ('v', '+')]` if PREFIX is absent.
    #[must_use]
    pub fn prefix_map(&self) -> Vec<(char, char)> {
        let Some(value) = self.tokens.get("PREFIX") else {
            // RFC 2812 default
            return vec![('o', '@'), ('v', '+')];
        };

        // Format: (modes)prefixes  — e.g. "(qaohv)~&@%+"
        let Some(rest) = value.strip_prefix('(') else {
            return vec![('o', '@'), ('v', '+')];
        };
        let Some((modes, prefixes)) = rest.split_once(')') else {
            return vec![('o', '@'), ('v', '+')];
        };

        modes.chars().zip(prefixes.chars()).collect()
    }

    /// Parse `CHANMODES=A,B,C,D` into four groups.
    ///
    /// - **A** — list modes (e.g. `b`, `e`, `I`)
    /// - **B** — modes that always take a parameter (e.g. `k`)
    /// - **C** — modes that take a parameter only when set (e.g. `l`)
    /// - **D** — modes that never take a parameter (e.g. `n`, `t`, `s`)
    ///
    /// Returns `("", "", "", "")` if CHANMODES is absent.
    #[must_use]
    pub fn chanmode_types(&self) -> (String, String, String, String) {
        let Some(value) = self.tokens.get("CHANMODES") else {
            return (String::new(), String::new(), String::new(), String::new());
        };

        let mut parts = value.splitn(4, ',');
        let a = parts.next().unwrap_or("").to_string();
        let b = parts.next().unwrap_or("").to_string();
        let c = parts.next().unwrap_or("").to_string();
        let d = parts.next().unwrap_or("").to_string();
        (a, b, c, d)
    }

    /// The server-advertised network name, if any.
    #[must_use]
    pub fn network(&self) -> Option<&str> {
        self.tokens.get("NETWORK").map(String::as_str)
    }

    /// Whether the server supports WHOX (extended WHO queries).
    #[must_use]
    pub fn has_whox(&self) -> bool {
        self.tokens.contains_key("WHOX")
    }

    /// Maximum number of modes that can be changed in a single MODE command.
    /// Defaults to 3.
    #[must_use]
    pub fn max_modes(&self) -> usize {
        self.tokens
            .get("MODES")
            .and_then(|v| v.parse().ok())
            .unwrap_or(3)
    }

    /// Characters that can prefix a channel name in a PRIVMSG target to
    /// restrict delivery to users with that status (e.g. `@+`).
    /// Defaults to `""`.
    #[must_use]
    pub fn statusmsg(&self) -> &str {
        self.tokens.get("STATUSMSG").map_or("", String::as_str)
    }

    /// The case-mapping model used by the server for nick/channel comparison.
    /// Common values: `rfc1459`, `ascii`, `strict-rfc1459`.
    /// Defaults to `rfc1459`.
    #[must_use]
    pub fn casemapping(&self) -> &str {
        self.tokens
            .get("CASEMAPPING")
            .map_or("rfc1459", String::as_str)
    }

    /// Maximum length of a channel name.  Defaults to 200.
    #[must_use]
    pub fn channel_len(&self) -> usize {
        self.tokens
            .get("CHANNELLEN")
            .and_then(|v| v.parse().ok())
            .unwrap_or(200)
    }

    /// Maximum length of a nickname.  Defaults to 9 (RFC 2812).
    #[must_use]
    pub fn nick_len(&self) -> usize {
        self.tokens
            .get("NICKLEN")
            .and_then(|v| v.parse().ok())
            .unwrap_or(9)
    }

    /// Maximum length of a channel topic.  Defaults to 307.
    #[must_use]
    pub fn topic_len(&self) -> usize {
        self.tokens
            .get("TOPICLEN")
            .and_then(|v| v.parse().ok())
            .unwrap_or(307)
    }

    /// Allowed channel-name prefix characters.  Defaults to `#&`.
    #[must_use]
    pub fn chan_types(&self) -> &str {
        self.tokens.get("CHANTYPES").map_or("#&", String::as_str)
    }

    /// Parse `EXTBAN=prefix,types` into `(prefix_char, types_string)`.
    ///
    /// Example: `EXTBAN=~,qaojrsnSR` → `Some(('~', "qaojrsnSR"))`
    ///
    /// Returns `None` if EXTBAN is not advertised or malformed.
    #[must_use]
    pub fn extban(&self) -> Option<(char, String)> {
        let value = self.tokens.get("EXTBAN")?;
        let (prefix_str, types) = value.split_once(',')?;
        let prefix = prefix_str.chars().next()?;
        Some((prefix, types.to_string()))
    }

    /// Whether the server supports multi-target MODE queries.
    ///
    /// Checks `TARGMAX` for a `MODE` entry. If `TARGMAX` is present but
    /// `MODE` is not listed, the server does not support multi-target MODE
    /// (e.g. Solanum/Libera rejects with 479 "Illegal channel name").
    /// If `TARGMAX` is absent entirely (e.g. `IRCnet`), assumes multi-target
    /// MODE is supported (`IRCnet` ircd 2.12 handles it fine).
    #[must_use]
    pub fn supports_multi_target_mode(&self) -> bool {
        let Some(targmax) = self.tokens.get("TARGMAX") else {
            return true; // no TARGMAX → assume multi-target is fine
        };
        // TARGMAX=NAMES:1,LIST:1,KICK:1,...,MODE:4
        // If MODE is listed with a limit > 0, multi-target is supported.
        // If MODE is not listed at all, the server doesn't support it.
        targmax.split(',').any(|entry| {
            entry
                .split_once(':')
                .is_some_and(|(cmd, limit)| cmd == "MODE" && limit != "1" && limit != "0")
        })
    }

    /// Look up a raw token value by key.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&str> {
        self.tokens.get(key).map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_key_value_tokens() {
        let mut is = Isupport::new();
        is.parse_tokens(&["NETWORK=Libera.Chat", "MODES=4", "CHANTYPES=#"]);

        assert_eq!(is.network(), Some("Libera.Chat"));
        assert_eq!(is.max_modes(), 4);
        assert_eq!(is.chan_types(), "#");
    }

    #[test]
    fn parse_bare_token() {
        let mut is = Isupport::new();
        is.parse_tokens(&["WHOX", "SAFELIST"]);

        assert!(is.has_whox());
        assert_eq!(is.get("SAFELIST"), Some(""));
    }

    #[test]
    fn negated_token() {
        let mut is = Isupport::new();
        is.parse_tokens(&["WHOX"]);
        assert!(is.has_whox());

        is.parse_tokens(&["-WHOX"]);
        assert!(!is.has_whox());
    }

    #[test]
    fn prefix_parsing_standard() {
        let mut is = Isupport::new();
        is.parse_tokens(&["PREFIX=(ov)@+"]);

        let map = is.prefix_map();
        assert_eq!(map, vec![('o', '@'), ('v', '+')]);
    }

    #[test]
    fn prefix_parsing_extended() {
        let mut is = Isupport::new();
        is.parse_tokens(&["PREFIX=(qaohv)~&@%+"]);

        let map = is.prefix_map();
        assert_eq!(
            map,
            vec![('q', '~'), ('a', '&'), ('o', '@'), ('h', '%'), ('v', '+'),]
        );
    }

    #[test]
    fn prefix_default_when_absent() {
        let is = Isupport::new();
        let map = is.prefix_map();
        assert_eq!(map, vec![('o', '@'), ('v', '+')]);
    }

    #[test]
    fn chanmodes_parsing() {
        let mut is = Isupport::new();
        is.parse_tokens(&["CHANMODES=beI,k,l,imnpstaqrRcOAQKVCuzNSMTGZ"]);

        let (a, b, c, d) = is.chanmode_types();
        assert_eq!(a, "beI");
        assert_eq!(b, "k");
        assert_eq!(c, "l");
        assert_eq!(d, "imnpstaqrRcOAQKVCuzNSMTGZ");
    }

    #[test]
    fn chanmodes_default_when_absent() {
        let is = Isupport::new();
        let (a, b, c, d) = is.chanmode_types();
        assert!(a.is_empty());
        assert!(b.is_empty());
        assert!(c.is_empty());
        assert!(d.is_empty());
    }

    #[test]
    fn casemapping_values() {
        let mut is = Isupport::new();
        assert_eq!(is.casemapping(), "rfc1459");

        is.parse_tokens(&["CASEMAPPING=ascii"]);
        assert_eq!(is.casemapping(), "ascii");
    }

    #[test]
    fn defaults_for_missing_tokens() {
        let is = Isupport::new();
        assert_eq!(is.network(), None);
        assert!(!is.has_whox());
        assert_eq!(is.max_modes(), 3);
        assert_eq!(is.statusmsg(), "");
        assert_eq!(is.casemapping(), "rfc1459");
        assert_eq!(is.channel_len(), 200);
        assert_eq!(is.nick_len(), 9);
        assert_eq!(is.topic_len(), 307);
        assert_eq!(is.chan_types(), "#&");
        assert_eq!(is.extban(), None);
    }

    #[test]
    fn extban_parsing() {
        let mut is = Isupport::new();
        is.parse_tokens(&["EXTBAN=~,qaojrsnSR"]);

        let (prefix, types) = is.extban().expect("EXTBAN should parse");
        assert_eq!(prefix, '~');
        assert_eq!(types, "qaojrsnSR");
    }

    #[test]
    fn extban_missing() {
        let is = Isupport::new();
        assert_eq!(is.extban(), None);
    }

    #[test]
    fn statusmsg_parsing() {
        let mut is = Isupport::new();
        is.parse_tokens(&["STATUSMSG=@+"]);
        assert_eq!(is.statusmsg(), "@+");
    }

    #[test]
    fn length_tokens() {
        let mut is = Isupport::new();
        is.parse_tokens(&["NICKLEN=30", "CHANNELLEN=50", "TOPICLEN=390"]);
        assert_eq!(is.nick_len(), 30);
        assert_eq!(is.channel_len(), 50);
        assert_eq!(is.topic_len(), 390);
    }

    #[test]
    fn multiple_parse_calls_merge() {
        let mut is = Isupport::new();
        is.parse_tokens(&["NETWORK=Freenode", "MODES=3"]);
        is.parse_tokens(&["NETWORK=Libera.Chat", "WHOX"]);

        // Second call should overwrite NETWORK
        assert_eq!(is.network(), Some("Libera.Chat"));
        // MODES from first call should persist
        assert_eq!(is.max_modes(), 3);
        // WHOX from second call should be present
        assert!(is.has_whox());
    }

    #[test]
    fn full_real_world_isupport() {
        let mut is = Isupport::new();
        // Typical Libera.Chat ISUPPORT tokens (spread across multiple lines)
        is.parse_tokens(&[
            "CALLERID",
            "CASEMAPPING=rfc1459",
            "DEAF=D",
            "KICKLEN=255",
            "MODES=4",
        ]);
        is.parse_tokens(&[
            "PREFIX=(ov)@+",
            "STATUSMSG=@+",
            "EXCEPTS=e",
            "INVEX=I",
            "CHANMODES=eIbq,k,flj,CFLMPQScgimnprstuz",
        ]);
        is.parse_tokens(&[
            "CHANTYPES=#",
            "NETWORK=Libera.Chat",
            "NICKLEN=16",
            "CHANNELLEN=50",
            "TOPICLEN=390",
            "WHOX",
        ]);

        assert_eq!(is.casemapping(), "rfc1459");
        assert_eq!(is.max_modes(), 4);
        assert_eq!(is.prefix_map(), vec![('o', '@'), ('v', '+')]);
        assert_eq!(is.statusmsg(), "@+");
        assert_eq!(is.chan_types(), "#");
        assert_eq!(is.network(), Some("Libera.Chat"));
        assert_eq!(is.nick_len(), 16);
        assert_eq!(is.channel_len(), 50);
        assert_eq!(is.topic_len(), 390);
        assert!(is.has_whox());

        let (a, b, c, d) = is.chanmode_types();
        assert_eq!(a, "eIbq");
        assert_eq!(b, "k");
        assert_eq!(c, "flj");
        assert_eq!(d, "CFLMPQScgimnprstuz");
    }

    #[test]
    fn multi_target_mode_with_targmax_no_mode() {
        // Libera: TARGMAX lists commands but not MODE → multi-target NOT supported
        let mut is = Isupport::new();
        is.parse_tokens(&["TARGMAX=NAMES:1,LIST:1,KICK:1,WHOIS:1,PRIVMSG:4,NOTICE:4,ACCEPT:,MONITOR:"]);
        assert!(!is.supports_multi_target_mode());
    }

    #[test]
    fn multi_target_mode_with_targmax_mode_listed() {
        // Hypothetical server with MODE in TARGMAX
        let mut is = Isupport::new();
        is.parse_tokens(&["TARGMAX=NAMES:1,MODE:4,PRIVMSG:4"]);
        assert!(is.supports_multi_target_mode());
    }

    #[test]
    fn multi_target_mode_without_targmax() {
        // IRCnet: no TARGMAX at all → assume multi-target is fine
        let is = Isupport::new();
        assert!(is.supports_multi_target_mode());
    }

    #[test]
    fn multi_target_mode_targmax_mode_limited_to_one() {
        let mut is = Isupport::new();
        is.parse_tokens(&["TARGMAX=MODE:1,PRIVMSG:4"]);
        assert!(!is.supports_multi_target_mode());
    }

    #[test]
    fn multi_target_mode_targmax_mode_unlimited() {
        // Empty limit after colon means unlimited per IRC spec
        let mut is = Isupport::new();
        is.parse_tokens(&["TARGMAX=MODE:,PRIVMSG:4"]);
        assert!(is.supports_multi_target_mode());
    }
}
