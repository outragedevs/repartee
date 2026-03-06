use std::fmt;

/// Parsed extended ban mask.
///
/// Extended bans extend the standard `nick!ident@host` format. When the nick
/// field starts with the extban prefix (commonly `$`), it encodes additional
/// matching criteria:
///
/// - `$a:patrick!*@*`  — match users logged in as "patrick"
/// - `$a!*@*`          — match any logged-in user
/// - `$a:pat*!*@*`     — match accounts starting with "pat"
///
/// The prefix character and available types are advertised by the server
/// via ISUPPORT `EXTBAN=prefix,types`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extban {
    /// The single-character ban type (e.g. `'a'` for account).
    pub ban_type: char,
    /// Optional parameter following `:` (e.g. `"patrick"` in `$a:patrick`).
    pub parameter: Option<String>,
    /// The user (ident) portion of the mask.
    pub user: String,
    /// The host portion of the mask.
    pub host: String,
}

#[allow(dead_code)]
impl Extban {
    /// Create a new `Extban` with the given components.
    #[must_use]
    pub fn new(ban_type: char, param: Option<&str>, user: &str, host: &str) -> Self {
        Self {
            ban_type,
            parameter: param.map(String::from),
            user: user.to_string(),
            host: host.to_string(),
        }
    }

    /// Parse an extban mask string.
    ///
    /// Accepts masks beginning with `$` (the most common extban prefix).
    /// Returns `None` if the mask does not look like an extban.
    #[must_use]
    pub fn parse(mask: &str) -> Option<Self> {
        Self::parse_with_prefix(mask, '$')
    }

    /// Parse an extban mask with a custom prefix character.
    ///
    /// The prefix is the character advertised in ISUPPORT `EXTBAN=prefix,types`.
    /// Common prefixes are `$` and `~`.
    #[must_use]
    pub fn parse_with_prefix(mask: &str, prefix: char) -> Option<Self> {
        let rest = mask.strip_prefix(prefix)?;

        // Must have at least a type character
        let mut chars = rest.chars();
        let ban_type = chars.next()?;

        let remainder = chars.as_str();

        // Split at `!` to separate the type+parameter from user@host
        let (type_part, userhost) = remainder
            .find('!')
            .map_or((remainder, "*@*"), |bang_pos| {
                (&remainder[..bang_pos], &remainder[bang_pos + 1..])
            });

        // Extract optional parameter (after `:`)
        let parameter = if let Some(param) = type_part.strip_prefix(':') {
            if param.is_empty() { None } else { Some(param.to_string()) }
        } else if type_part.is_empty() {
            None
        } else {
            // Unexpected characters between type and `!` without `:`
            // e.g. `$aFOO!*@*` — not a valid extban
            return None;
        };

        // Split user@host
        let (user, host) = userhost.split_once('@').map_or_else(
            || (userhost.to_string(), "*".to_string()),
            |(u, h)| (u.to_string(), h.to_string()),
        );

        Some(Self {
            ban_type,
            parameter,
            user,
            host,
        })
    }

    /// Human-readable display of the extban.
    ///
    /// - `$a:patrick!*@*` -> `"account:patrick (*@*)"`
    /// - `$a!*@*`         -> `"account (any) (*@*)"`
    #[must_use]
    pub fn display_friendly(&self) -> String {
        let type_name = match self.ban_type {
            'a' => "account",
            'c' => "channel",
            'j' => "realname",
            'n' => "nick-change",
            'o' => "oper",
            'r' => "gecos",
            's' => "server",
            _ => return self.display_friendly_unknown(),
        };

        let userhost = self.userhost();
        self.parameter.as_ref().map_or_else(
            || format!("{type_name} (any) ({userhost})"),
            |p| format!("{type_name}:{p} ({userhost})"),
        )
    }

    /// Friendly display for unknown ban types — just use the char.
    fn display_friendly_unknown(&self) -> String {
        let bt = self.ban_type;
        let userhost = self.userhost();
        self.parameter.as_ref().map_or_else(
            || format!("{bt} (any) ({userhost})"),
            |p| format!("{bt}:{p} ({userhost})"),
        )
    }

    /// Format the `user@host` portion.
    fn userhost(&self) -> String {
        format!("{}@{}", self.user, self.host)
    }
}

impl fmt::Display for Extban {
    /// Outputs the IRC-format extban mask: `$a:patrick!*@*`
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "${}", self.ban_type)?;
        if let Some(ref param) = self.parameter {
            write!(f, ":{param}")?;
        }
        write!(f, "!{}@{}", self.user, self.host)
    }
}

/// Format an extban mask for display, showing friendly text alongside the raw mask.
///
/// If the mask is an extban (starts with `$` or the server's extban prefix),
/// returns `"friendly — raw"`. Otherwise returns the raw mask as-is.
#[must_use]
pub fn format_ban_mask(mask: &str, extban_prefix: Option<char>) -> String {
    let prefix = extban_prefix.unwrap_or('$');
    Extban::parse_with_prefix(mask, prefix).map_or_else(
        || mask.to_string(),
        |eb| format!("{} — {mask}", eb.display_friendly()),
    )
}

/// Compose an account extban mask from an account name.
///
/// Uses the `$` prefix by default (standard for most ircds).
/// The extban prefix from ISUPPORT can be passed for correctness.
#[must_use]
pub fn compose_account_ban(account: &str, extban_prefix: Option<char>) -> String {
    let prefix = extban_prefix.unwrap_or('$');
    format!("{prefix}a:{account}!*@*")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_account_with_parameter() {
        let eb = Extban::parse("$a:patrick!*@*").unwrap();
        assert_eq!(eb.ban_type, 'a');
        assert_eq!(eb.parameter.as_deref(), Some("patrick"));
        assert_eq!(eb.user, "*");
        assert_eq!(eb.host, "*");
    }

    #[test]
    fn parse_account_no_parameter() {
        let eb = Extban::parse("$a!*@*").unwrap();
        assert_eq!(eb.ban_type, 'a');
        assert_eq!(eb.parameter, None);
        assert_eq!(eb.user, "*");
        assert_eq!(eb.host, "*");
    }

    #[test]
    fn parse_wildcard_parameter() {
        let eb = Extban::parse("$a:pat*!*@*").unwrap();
        assert_eq!(eb.ban_type, 'a');
        assert_eq!(eb.parameter.as_deref(), Some("pat*"));
        assert_eq!(eb.user, "*");
        assert_eq!(eb.host, "*");
    }

    #[test]
    fn non_extban_returns_none() {
        assert!(Extban::parse("nick!user@host").is_none());
        assert!(Extban::parse("*!*@*.example.com").is_none());
        assert!(Extban::parse("").is_none());
    }

    #[test]
    fn display_format() {
        let eb = Extban::new('a', Some("patrick"), "*", "*");
        assert_eq!(eb.to_string(), "$a:patrick!*@*");
    }

    #[test]
    fn display_format_no_param() {
        let eb = Extban::new('a', None, "*", "*");
        assert_eq!(eb.to_string(), "$a!*@*");
    }

    #[test]
    fn friendly_display_with_param() {
        let eb = Extban::new('a', Some("patrick"), "*", "*");
        assert_eq!(eb.display_friendly(), "account:patrick (*@*)");
    }

    #[test]
    fn friendly_display_no_param() {
        let eb = Extban::new('a', None, "*", "*");
        assert_eq!(eb.display_friendly(), "account (any) (*@*)");
    }

    #[test]
    fn friendly_display_unknown_type() {
        let eb = Extban::new('x', Some("foo"), "*", "*");
        assert_eq!(eb.display_friendly(), "x:foo (*@*)");
    }

    #[test]
    fn parse_with_tilde_prefix() {
        let eb = Extban::parse_with_prefix("~a:patrick!*@*", '~').unwrap();
        assert_eq!(eb.ban_type, 'a');
        assert_eq!(eb.parameter.as_deref(), Some("patrick"));
    }

    #[test]
    fn format_ban_mask_extban() {
        let result = format_ban_mask("$a:patrick!*@*", None);
        assert_eq!(result, "account:patrick (*@*) — $a:patrick!*@*");
    }

    #[test]
    fn format_ban_mask_normal() {
        let result = format_ban_mask("nick!*@*.example.com", None);
        assert_eq!(result, "nick!*@*.example.com");
    }

    #[test]
    fn compose_account_ban_default_prefix() {
        assert_eq!(compose_account_ban("patrick", None), "$a:patrick!*@*");
    }

    #[test]
    fn compose_account_ban_custom_prefix() {
        assert_eq!(compose_account_ban("patrick", Some('~')), "~a:patrick!*@*");
    }

    #[test]
    fn roundtrip_parse_display() {
        let original = "$a:patrick!*@*";
        let eb = Extban::parse(original).unwrap();
        assert_eq!(eb.to_string(), original);
    }

    #[test]
    fn roundtrip_no_param() {
        let original = "$a!*@*";
        let eb = Extban::parse(original).unwrap();
        assert_eq!(eb.to_string(), original);
    }

    #[test]
    fn parse_specific_userhost() {
        let eb = Extban::parse("$a:patrick!ident@host.example.com").unwrap();
        assert_eq!(eb.user, "ident");
        assert_eq!(eb.host, "host.example.com");
    }
}
