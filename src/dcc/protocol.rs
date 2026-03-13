use std::net::{IpAddr, Ipv4Addr};

/// Fake IP sent in passive DCC CTCP offers. The peer connects back to us,
/// so the IP in the CTCP body is irrelevant — 1.1.1.1 is the conventional
/// placeholder recognised by most clients.
pub const PASSIVE_FAKE_IP: IpAddr = IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1));

/// Encode an IP address for use in a DCC CTCP message body.
///
/// IPv4 is transmitted as a 32-bit network-order integer written as a decimal
/// string (`(o1 << 24) | (o2 << 16) | (o3 << 8) | o4`).
/// IPv6 is transmitted as standard colon-hex notation because there is no
/// agreed-upon numeric encoding and modern clients accept the literal.
pub fn encode_ip(ip: &IpAddr) -> String {
    match ip {
        IpAddr::V4(v4) => {
            let octets = v4.octets();
            let n: u32 = (u32::from(octets[0]) << 24)
                | (u32::from(octets[1]) << 16)
                | (u32::from(octets[2]) << 8)
                | u32::from(octets[3]);
            n.to_string()
        }
        IpAddr::V6(v6) => v6.to_string(),
    }
}

/// Decode an IP address from a DCC CTCP message body.
///
/// If the string contains `:` it is treated as IPv6 colon-hex notation.
/// Otherwise it is parsed as a decimal u32 and converted to IPv4.
/// Returns `Err(String)` with a descriptive message when parsing fails.
pub fn decode_ip(s: &str) -> Result<IpAddr, String> {
    if s.contains(':') {
        // IPv6 literal
        s.parse::<IpAddr>()
            .map_err(|e| format!("invalid IPv6 address {s:?}: {e}"))
    } else {
        // DCC numeric IPv4 — parse as u32 then split into octets
        let n: u32 = s
            .parse()
            .map_err(|e| format!("invalid DCC IP {s:?}: {e}"))?;
        let o1 = ((n >> 24) & 0xff) as u8;
        let o2 = ((n >> 16) & 0xff) as u8;
        let o3 = ((n >> 8) & 0xff) as u8;
        let o4 = (n & 0xff) as u8;
        Ok(IpAddr::V4(Ipv4Addr::new(o1, o2, o3, o4)))
    }
}

/// Parsed representation of a DCC CTCP message.
pub struct DccCtcpMessage {
    /// Sub-type, normalised to uppercase (e.g. `"CHAT"`).
    /// Reserved for future DCC SEND support — currently always `"CHAT"`.
    #[allow(dead_code)]
    pub dcc_type: String,
    /// Remote address (IPv4 or IPv6).
    pub addr: IpAddr,
    /// Remote port. 0 means passive/reverse DCC.
    pub port: u16,
    /// Passive token, present when `port == 0` and a trailing integer follows.
    pub passive_token: Option<u32>,
}

/// Parse a raw CTCP body (without `\x01` delimiters) as a DCC CHAT request.
///
/// Expected formats:
/// - Active:  `DCC CHAT CHAT <addr> <port>`
/// - Passive: `DCC CHAT CHAT <addr> 0 <token>`
///
/// Returns `None` if the body is not a DCC request, if the sub-command is not
/// `CHAT`, or if required fields are missing or malformed.
pub fn parse_dcc_ctcp(body: &str) -> Option<DccCtcpMessage> {
    let mut parts = body.split_ascii_whitespace();

    // First token must be "DCC" (case-insensitive)
    let prefix = parts.next()?;
    if !prefix.eq_ignore_ascii_case("DCC") {
        return None;
    }

    // Second token is the DCC type — we only handle CHAT
    let dcc_type_raw = parts.next()?;
    if !dcc_type_raw.eq_ignore_ascii_case("CHAT") {
        return None;
    }
    let dcc_type = dcc_type_raw.to_ascii_uppercase();

    // Third token is the protocol argument; for CHAT it is also "chat" (ignored)
    let _proto_arg = parts.next()?;

    // Fourth token is the address
    let addr_str = parts.next()?;
    let addr = decode_ip(addr_str).ok()?;

    // Fifth token is the port
    let port_str = parts.next()?;
    let port: u16 = port_str.parse().ok()?;

    // Optional sixth token is the passive token (only meaningful when port == 0)
    let passive_token: Option<u32> = parts.next().and_then(|t| t.parse().ok());

    Some(DccCtcpMessage {
        dcc_type,
        addr,
        port,
        passive_token,
    })
}

/// Build a CTCP DCC CHAT message, including the surrounding `\x01` delimiters.
///
/// Active (port > 0): `\x01DCC CHAT CHAT <addr> <port>\x01`
/// Passive (port == 0, token present): `\x01DCC CHAT CHAT <addr> 0 <token>\x01`
pub fn build_dcc_chat_ctcp(ip: &IpAddr, port: u16, token: Option<u32>) -> String {
    let addr_str = encode_ip(ip);
    token.map_or_else(
        || format!("\x01DCC CHAT CHAT {addr_str} {port}\x01"),
        |t| format!("\x01DCC CHAT CHAT {addr_str} {port} {t}\x01"),
    )
}

/// Build a CTCP DCC REJECT message, including the surrounding `\x01` delimiters.
///
/// The literal body `DCC REJECT CHAT chat` is what most clients (mIRC, `HexChat`,
/// irssi) emit to decline an incoming DCC CHAT offer.
pub fn build_dcc_reject() -> String {
    "\x01DCC REJECT CHAT chat\x01".to_owned()
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    // IP encoding ─────────────────────────────────────────────────────────────

    #[test]
    fn encode_ipv4_localhost() {
        assert_eq!(
            encode_ip(&IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))),
            "2130706433"
        );
    }

    #[test]
    fn encode_ipv4_192_168() {
        assert_eq!(
            encode_ip(&IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100))),
            "3232235876"
        );
    }

    #[test]
    fn encode_ipv6() {
        assert_eq!(encode_ip(&IpAddr::V6(Ipv6Addr::LOCALHOST)), "::1");
    }

    #[test]
    fn encode_passive_fake_ip() {
        assert_eq!(encode_ip(&PASSIVE_FAKE_IP), "16843009");
    }

    // IP decoding ─────────────────────────────────────────────────────────────

    #[test]
    fn decode_ipv4_localhost() {
        assert_eq!(
            decode_ip("2130706433").unwrap(),
            IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))
        );
    }

    #[test]
    fn decode_ipv4_192() {
        assert_eq!(
            decode_ip("3232235876").unwrap(),
            IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100))
        );
    }

    #[test]
    fn decode_ipv6_short() {
        assert_eq!(decode_ip("::1").unwrap(), IpAddr::V6(Ipv6Addr::LOCALHOST));
    }

    #[test]
    fn decode_ipv6_full() {
        let expected: IpAddr = "2001:db8::1".parse().unwrap();
        assert_eq!(decode_ip("2001:db8::1").unwrap(), expected);
    }

    #[test]
    fn decode_invalid() {
        assert!(decode_ip("not_an_ip").is_err());
    }

    // CTCP parsing ────────────────────────────────────────────────────────────

    #[test]
    fn parse_active_chat() {
        let msg = parse_dcc_ctcp("DCC CHAT CHAT 3232235876 12345").unwrap();
        assert_eq!(msg.addr, IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100)));
        // Keep one-assertion-per-test spirit; port and token checked separately below.
    }

    /// Companion to `parse_active_chat` — verifies port is decoded correctly.
    #[test]
    fn parse_active_chat_port() {
        let msg = parse_dcc_ctcp("DCC CHAT CHAT 3232235876 12345").unwrap();
        assert_eq!(msg.port, 12345);
    }

    /// Companion to `parse_active_chat` — verifies no passive token is present.
    #[test]
    fn parse_active_chat_no_token() {
        let msg = parse_dcc_ctcp("DCC CHAT CHAT 3232235876 12345").unwrap();
        assert!(msg.passive_token.is_none());
    }

    #[test]
    fn parse_passive_chat() {
        let msg = parse_dcc_ctcp("DCC CHAT CHAT 16843009 0 42").unwrap();
        assert_eq!(msg.addr, PASSIVE_FAKE_IP);
    }

    #[test]
    fn parse_passive_chat_token() {
        let msg = parse_dcc_ctcp("DCC CHAT CHAT 16843009 0 42").unwrap();
        assert_eq!(msg.passive_token, Some(42));
    }

    #[test]
    fn parse_lowercase_chat() {
        // Parser must be case-insensitive; dcc_type must come back uppercase.
        let msg = parse_dcc_ctcp("DCC CHAT chat 2130706433 5000").unwrap();
        assert_eq!(msg.dcc_type, "CHAT");
    }

    #[test]
    fn parse_ipv6_chat() {
        let msg = parse_dcc_ctcp("DCC CHAT CHAT ::1 5000").unwrap();
        assert_eq!(msg.addr, IpAddr::V6(Ipv6Addr::LOCALHOST));
    }

    #[test]
    fn parse_not_dcc() {
        assert!(parse_dcc_ctcp("VERSION").is_none());
    }

    #[test]
    fn parse_not_chat() {
        // DCC SEND must be rejected — we only handle CHAT.
        assert!(parse_dcc_ctcp("DCC SEND file.txt 2130706433 5000 1024").is_none());
    }

    // CTCP building ───────────────────────────────────────────────────────────

    #[test]
    fn build_active_chat() {
        let ip = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100));
        assert_eq!(
            build_dcc_chat_ctcp(&ip, 12345, None),
            "\x01DCC CHAT CHAT 3232235876 12345\x01"
        );
    }

    #[test]
    fn build_passive_chat() {
        assert_eq!(
            build_dcc_chat_ctcp(&PASSIVE_FAKE_IP, 0, Some(42)),
            "\x01DCC CHAT CHAT 16843009 0 42\x01"
        );
    }

    #[test]
    fn build_reject() {
        assert_eq!(build_dcc_reject(), "\x01DCC REJECT CHAT chat\x01");
    }
}
