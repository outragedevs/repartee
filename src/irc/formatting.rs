/// Strip mIRC formatting codes from text.
///
/// Removes:
/// - \x02 (bold)
/// - \x1D (italic)
/// - \x1F (underline)
/// - \x1E (strikethrough)
/// - \x16 (reverse)
/// - \x0F (reset)
/// - \x03N[,N] (mIRC color codes)
/// - \x04RRGGBB[,RRGGBB] (hex color codes)
pub fn strip_irc_formatting(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        match chars[i] {
            '\x02' | '\x1D' | '\x1F' | '\x1E' | '\x16' | '\x0F' => {
                // Skip formatting toggle / reset characters
                i += 1;
            }
            '\x03' => {
                // mIRC color code: \x03[N[N]][,N[N]]
                i += 1;
                // Consume up to 2 foreground digits
                let mut digits = 0;
                while i < len && chars[i].is_ascii_digit() && digits < 2 {
                    i += 1;
                    digits += 1;
                }
                // Optional comma + up to 2 background digits
                if i < len && chars[i] == ',' {
                    // Peek ahead: only consume if followed by a digit
                    if i + 1 < len && chars[i + 1].is_ascii_digit() {
                        i += 1; // skip comma
                        let mut bg_digits = 0;
                        while i < len && chars[i].is_ascii_digit() && bg_digits < 2 {
                            i += 1;
                            bg_digits += 1;
                        }
                    }
                }
            }
            '\x04' => {
                // Hex color code: \x04RRGGBB[,RRGGBB]
                i += 1;
                // Consume up to 6 hex digits for foreground
                let mut hex_digits = 0;
                while i < len && chars[i].is_ascii_hexdigit() && hex_digits < 6 {
                    i += 1;
                    hex_digits += 1;
                }
                // Optional comma + up to 6 hex digits for background
                if i < len && chars[i] == ','
                    && i + 1 < len && chars[i + 1].is_ascii_hexdigit()
                {
                    i += 1; // skip comma
                    let mut bg_hex = 0;
                    while i < len && chars[i].is_ascii_hexdigit() && bg_hex < 6 {
                        i += 1;
                        bg_hex += 1;
                    }
                }
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }

    out
}

/// Split nick prefix characters (~&@%+) from a nick string.
///
/// Returns `(prefix, nick)` where prefix contains any leading
/// status characters and nick is the remainder.
///
/// Uses a hardcoded set of common prefixes. For dynamic prefix
/// handling based on ISUPPORT PREFIX, see `parse_names_entry`.
#[allow(dead_code)]
pub fn split_nick_prefix(nick_with_prefix: &str) -> (String, &str) {
    const PREFIXES: &str = "~&@%+";
    let mut prefix = String::new();
    let mut start = 0;
    for (i, c) in nick_with_prefix.char_indices() {
        if PREFIXES.contains(c) {
            prefix.push(c);
            start = i + c.len_utf8();
        } else {
            break;
        }
    }
    (prefix, &nick_with_prefix[start..])
}

/// Convert nick prefix characters to their corresponding channel mode letters.
///
/// ~ -> q (owner), & -> a (admin), @ -> o (op), % -> h (halfop), + -> v (voice)
///
/// Uses a hardcoded mapping. For dynamic mapping based on ISUPPORT PREFIX,
/// see `parse_names_entry`.
#[allow(dead_code)]
pub fn prefix_to_mode(prefix: &str) -> String {
    prefix
        .chars()
        .map(|c| match c {
            '~' => 'q',
            '&' => 'a',
            '@' => 'o',
            '%' => 'h',
            '+' => 'v',
            other => other,
        })
        .collect()
}

/// Return the highest-ranked prefix character for the given mode string.
///
/// `prefix_order` defines the ranking from highest to lowest (e.g., "~&@%+").
pub fn get_highest_prefix(modes: &str, prefix_order: &str) -> String {
    for prefix_char in prefix_order.chars() {
        let mode_char = match prefix_char {
            '~' => 'q',
            '&' => 'a',
            '@' => 'o',
            '%' => 'h',
            '+' => 'v',
            c => c,
        };
        if modes.contains(mode_char) {
            return prefix_char.to_string();
        }
    }
    String::new()
}

/// Check whether a target string refers to a channel (starts with #, &, +, or !).
pub fn is_channel(target: &str) -> bool {
    target.starts_with('#')
        || target.starts_with('&')
        || target.starts_with('+')
        || target.starts_with('!')
}

/// Extract the nickname from an IRC `Prefix`.
///
/// For `Nickname(nick, _, _)` returns the nick.
/// For `ServerName(name)` returns the server name.
pub fn extract_nick(prefix: Option<&irc::proto::Prefix>) -> Option<String> {
    use irc::proto::Prefix;
    prefix.map(|p| match p {
        Prefix::Nickname(nick, _, _) => nick.clone(),
        Prefix::ServerName(name) => name.clone(),
    })
}

/// Extract nick, ident, and hostname from an IRC prefix.
///
/// Returns `(nick, ident, hostname)`. For server prefixes, ident and hostname
/// are empty strings.
pub fn extract_nick_userhost(prefix: Option<&irc::proto::Prefix>) -> (String, String, String) {
    use irc::proto::Prefix;
    match prefix {
        Some(Prefix::Nickname(nick, user, host)) => {
            (nick.clone(), user.clone(), host.clone())
        }
        Some(Prefix::ServerName(name)) => (name.clone(), String::new(), String::new()),
        None => (String::new(), String::new(), String::new()),
    }
}

/// Check whether a prefix represents a server (no `!` in it).
pub const fn is_server_prefix(prefix: Option<&irc::proto::Prefix>) -> bool {
    use irc::proto::Prefix;
    match prefix {
        Some(Prefix::ServerName(_)) | None => true,
        Some(Prefix::Nickname(_, user, _)) => user.is_empty(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // === strip_irc_formatting ===

    #[test]
    fn strip_plain_text_unchanged() {
        assert_eq!(strip_irc_formatting("hello world"), "hello world");
    }

    #[test]
    fn strip_bold() {
        assert_eq!(strip_irc_formatting("\x02bold\x02"), "bold");
    }

    #[test]
    fn strip_italic() {
        assert_eq!(strip_irc_formatting("\x1Ditalic\x1D"), "italic");
    }

    #[test]
    fn strip_underline() {
        assert_eq!(strip_irc_formatting("\x1Funderline\x1F"), "underline");
    }

    #[test]
    fn strip_strikethrough() {
        assert_eq!(strip_irc_formatting("\x1Estrike\x1E"), "strike");
    }

    #[test]
    fn strip_reverse() {
        assert_eq!(strip_irc_formatting("\x16reverse\x16"), "reverse");
    }

    #[test]
    fn strip_reset() {
        assert_eq!(strip_irc_formatting("text\x0F more"), "text more");
    }

    #[test]
    fn strip_mirc_fg_color() {
        assert_eq!(strip_irc_formatting("\x034red text"), "red text");
    }

    #[test]
    fn strip_mirc_fg_bg_color() {
        assert_eq!(strip_irc_formatting("\x034,2text"), "text");
    }

    #[test]
    fn strip_mirc_two_digit_color() {
        assert_eq!(strip_irc_formatting("\x0312blue\x03"), "blue");
    }

    #[test]
    fn strip_mirc_fg_bg_two_digit() {
        assert_eq!(strip_irc_formatting("\x0304,12text\x03"), "text");
    }

    #[test]
    fn strip_color_without_digits_is_reset() {
        // \x03 with no digits is a color reset
        assert_eq!(strip_irc_formatting("before\x03after"), "beforeafter");
    }

    #[test]
    fn strip_hex_color() {
        assert_eq!(strip_irc_formatting("\x04FF0000red"), "red");
    }

    #[test]
    fn strip_hex_fg_bg_color() {
        assert_eq!(strip_irc_formatting("\x04FF0000,00FF00text"), "text");
    }

    #[test]
    fn strip_combined_formatting() {
        // Bold + color + underline
        assert_eq!(
            strip_irc_formatting("\x02\x034,2\x1Fhello\x0F world"),
            "hello world"
        );
    }

    #[test]
    fn strip_comma_not_followed_by_digit() {
        // \x034,text — the comma is NOT a bg separator because no digit follows
        assert_eq!(strip_irc_formatting("\x034,text"), ",text");
    }

    // === split_nick_prefix ===

    #[test]
    fn split_no_prefix() {
        let (prefix, nick) = split_nick_prefix("alice");
        assert_eq!(prefix, "");
        assert_eq!(nick, "alice");
    }

    #[test]
    fn split_op_prefix() {
        let (prefix, nick) = split_nick_prefix("@alice");
        assert_eq!(prefix, "@");
        assert_eq!(nick, "alice");
    }

    #[test]
    fn split_multiple_prefixes() {
        let (prefix, nick) = split_nick_prefix("~&@bob");
        assert_eq!(prefix, "~&@");
        assert_eq!(nick, "bob");
    }

    #[test]
    fn split_voice_prefix() {
        let (prefix, nick) = split_nick_prefix("+voice");
        assert_eq!(prefix, "+");
        assert_eq!(nick, "voice");
    }

    #[test]
    fn split_halfop_prefix() {
        let (prefix, nick) = split_nick_prefix("%halfy");
        assert_eq!(prefix, "%");
        assert_eq!(nick, "halfy");
    }

    // === prefix_to_mode ===

    #[test]
    fn prefix_to_mode_op() {
        assert_eq!(prefix_to_mode("@"), "o");
    }

    #[test]
    fn prefix_to_mode_voice() {
        assert_eq!(prefix_to_mode("+"), "v");
    }

    #[test]
    fn prefix_to_mode_multiple() {
        assert_eq!(prefix_to_mode("~&@%+"), "qaohv");
    }

    #[test]
    fn prefix_to_mode_empty() {
        assert_eq!(prefix_to_mode(""), "");
    }

    // === get_highest_prefix ===

    #[test]
    fn highest_prefix_op() {
        assert_eq!(get_highest_prefix("ov", "~&@%+"), "@");
    }

    #[test]
    fn highest_prefix_owner() {
        assert_eq!(get_highest_prefix("qov", "~&@%+"), "~");
    }

    #[test]
    fn highest_prefix_voice_only() {
        assert_eq!(get_highest_prefix("v", "~&@%+"), "+");
    }

    #[test]
    fn highest_prefix_none() {
        assert_eq!(get_highest_prefix("", "~&@%+"), "");
    }

    // === is_channel ===

    #[test]
    fn is_channel_hash() {
        assert!(is_channel("#rust"));
    }

    #[test]
    fn is_channel_ampersand() {
        assert!(is_channel("&local"));
    }

    #[test]
    fn is_channel_plus() {
        assert!(is_channel("+modeless"));
    }

    #[test]
    fn is_channel_bang() {
        assert!(is_channel("!safe"));
    }

    #[test]
    fn is_channel_nick() {
        assert!(!is_channel("alice"));
    }

    // === extract_nick ===

    #[test]
    fn extract_nick_with_host() {
        use irc::proto::Prefix;
        let prefix = Prefix::Nickname("nick".into(), "user".into(), "host".into());
        assert_eq!(extract_nick(Some(&prefix)), Some("nick".to_string()));
    }

    #[test]
    fn extract_nick_server_name() {
        use irc::proto::Prefix;
        let prefix = Prefix::ServerName("irc.server.com".into());
        assert_eq!(extract_nick(Some(&prefix)), Some("irc.server.com".to_string()));
    }

    #[test]
    fn extract_nick_none() {
        assert_eq!(extract_nick(None), None);
    }

    // === is_server_prefix ===

    #[test]
    fn is_server_prefix_true_for_server() {
        use irc::proto::Prefix;
        let prefix = Prefix::ServerName("irc.server.com".into());
        assert!(is_server_prefix(Some(&prefix)));
    }

    #[test]
    fn is_server_prefix_false_for_nick() {
        use irc::proto::Prefix;
        let prefix = Prefix::Nickname("nick".into(), "user".into(), "host".into());
        assert!(!is_server_prefix(Some(&prefix)));
    }

    #[test]
    fn is_server_prefix_true_for_none() {
        assert!(is_server_prefix(None));
    }
}
