use irc::proto::{Command, Message};

pub const CAP: &str = "draft/metadata-2";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Key {
    Pinned,
    Muted,
    Blocked,
}

impl Key {
    pub const ALL: [Self; 3] = [Self::Pinned, Self::Muted, Self::Blocked];

    pub const fn wire(self) -> &'static str {
        match self {
            Self::Pinned => "soju.im/pinned",
            Self::Muted => "soju.im/muted",
            Self::Blocked => "soju.im/blocked",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|key| key.wire().eq_ignore_ascii_case(value))
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Flags {
    pub pinned: bool,
    pub muted: bool,
    pub blocked: bool,
}

impl Flags {
    pub const fn set(&mut self, key: Key, value: bool) {
        match key {
            Key::Pinned => self.pinned = value,
            Key::Muted => self.muted = value,
            Key::Blocked => self.blocked = value,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Value { target: String, key: Key, value: bool },
    Subscribed(Key),
    Unsubscribed(Key),
    Subscription(Key),
    Failure { code: String, details: String },
    Invalid,
}

pub fn parse(message: &Message) -> Option<Event> {
    if let Command::METADATA(target, None, Some(args)) = &message.command {
        let mut values = vec![target.clone()];
        values.extend_from_slice(args);
        return Some(value(&values));
    }
    let (command, args) = match &message.command {
        Command::Raw(command, args) => (command.to_ascii_uppercase(), args),
        Command::Response(response, args) => ((*response as u16).to_string(), args),
        _ => return None,
    };
    match command.as_str() {
        "METADATA" => Some(value(args)),
        "761" => Some(args.get(1..).map_or(Event::Invalid, value)),
        "770" | "771" | "772" => Some(args.get(1).and_then(|key| Key::parse(key)).map_or(Event::Invalid, |key| {
            match command.as_str() {
                "770" => Event::Subscribed(key),
                "771" => Event::Unsubscribed(key),
                _ => Event::Subscription(key),
            }
        })),
        "FAIL" if args.first().is_some_and(|arg| arg.eq_ignore_ascii_case("METADATA")) => Some(Event::Failure {
            code: args.get(1).cloned().unwrap_or_default(),
            details: args.get(2..).unwrap_or_default().join(" "),
        }),
        _ => None,
    }
}

fn value(args: &[String]) -> Event {
    let Some((target, key)) = args.first().filter(|target| valid_target(target)).zip(args.get(1).and_then(|key| Key::parse(key))) else { return Event::Invalid; };
    if args.get(2).is_none_or(|visibility| visibility != "*") || args.len() > 4 { return Event::Invalid; }
    let value = match args.get(3).map(String::as_str) {
        None | Some("0") => false,
        Some("1") => true,
        _ => return Event::Invalid,
    };
    Event::Value { target: target.clone(), key, value }
}

pub fn valid_target(target: &str) -> bool {
    !target.is_empty() && target != "*" && !target.starts_with(':')
        && !target.chars().any(|ch| ch.is_whitespace() || ch.is_control())
}

#[derive(Debug, Clone, Copy)]
pub enum Request<'a> {
    List(&'a str),
    Get(&'a str),
    Set(&'a str, Key, Option<bool>),
    Clear(&'a str),
    Subscribe,
    Unsubscribe,
    Subscriptions,
}

pub fn request(request: Request<'_>) -> Result<Message, &'static str> {
    let (target, subcommand, tail) = match request {
        Request::List(target) => (target, "LIST", vec![]),
        Request::Get(target) => (target, "GET", Key::ALL.into_iter().map(|key| key.wire().into()).collect()),
        Request::Set(target, key, value) => {
            let mut tail = vec![key.wire().into()];
            if let Some(value) = value { tail.push(if value { "1" } else { "0" }.into()); }
            (target, "SET", tail)
        }
        Request::Clear(target) => (target, "CLEAR", vec![]),
        Request::Subscribe => ("*", "SUB", Key::ALL.into_iter().map(|key| key.wire().into()).collect()),
        Request::Unsubscribe => ("*", "UNSUB", Key::ALL.into_iter().map(|key| key.wire().into()).collect()),
        Request::Subscriptions => ("*", "SUBS", vec![]),
    };
    if !matches!(subcommand, "SUB" | "UNSUB" | "SUBS") && !valid_target(target) { return Err("Invalid metadata target"); }
    let mut args = vec![target.into(), subcommand.into()];
    args.extend(tail);
    let message: Message = Command::Raw("METADATA".into(), args).into();
    if message.to_string().len() > crate::irc::PROTOCOL_LINE_MAX_BYTES { return Err("Metadata request exceeds the IRC line limit"); }
    Ok(message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pinned_provider_updates_and_numerics_without_changing_scope() {
        for wire in ["METADATA #Room soju.im/pinned * 1", ":bouncer 761 * #Room SOJU.IM/PINNED * 1"] {
            assert_eq!(parse(&wire.parse().unwrap()), Some(Event::Value { target: "#Room".into(), key: Key::Pinned, value: true }));
        }
        assert_eq!(parse(&"METADATA Alice soju.im/blocked *".parse().unwrap()), Some(Event::Value { target: "Alice".into(), key: Key::Blocked, value: false }));
        for (numeric, event) in [(770, Event::Subscribed(Key::Muted)), (771, Event::Unsubscribed(Key::Muted)), (772, Event::Subscription(Key::Muted))] {
            assert_eq!(parse(&format!(":bouncer {numeric} * soju.im/muted").parse().unwrap()), Some(event));
        }
        assert_eq!(parse(&"FAIL METADATA INTERNAL_ERROR #Room :Database unavailable".parse().unwrap()), Some(Event::Failure { code: "INTERNAL_ERROR".into(), details: "#Room Database unavailable".into() }));
    }

    #[test]
    fn malformed_or_unrelated_updates_cannot_change_flags() {
        for wire in ["METADATA #a soju.im/muted * true", "METADATA #a soju.im/muted", "METADATA #a unsupported * 1", "METADATA * soju.im/blocked * 1", "METADATA #a soju.im/muted private 1", "METADATA #a soju.im/pinned * 1 extra", ":server 761 * #a"] {
            assert_eq!(parse(&wire.parse().unwrap()), Some(Event::Invalid), "{wire}");
        }
        assert_eq!(parse(&"FAIL SEARCH INTERNAL_ERROR :Unrelated".parse().unwrap()), None);
    }

    #[test]
    fn requests_match_soju_wire_forms_and_reject_injection() {
        assert_eq!(request(Request::Subscribe).unwrap().to_string(), "METADATA * SUB soju.im/pinned soju.im/muted soju.im/blocked\r\n");
        assert_eq!(request(Request::Set("#a", Key::Muted, Some(true))).unwrap().to_string(), "METADATA #a SET soju.im/muted 1\r\n");
        assert_eq!(request(Request::Set("#a", Key::Muted, None)).unwrap().to_string(), "METADATA #a SET soju.im/muted\r\n");
        for target in ["", "*", ":#a", "#a\r\nQUIT", "#a #b"] { assert!(request(Request::Get(target)).is_err()); }
        assert!(request(Request::List(&"a".repeat(512))).is_err());
        assert_eq!(request(Request::Clear("#a")).unwrap().to_string(), "METADATA #a CLEAR\r\n");
        assert_eq!(request(Request::Subscriptions).unwrap().to_string(), "METADATA * SUBS\r\n");
        assert!(request(Request::Unsubscribe).unwrap().to_string().starts_with("METADATA * UNSUB "));
        let mut flags = Flags::default();
        for key in Key::ALL { flags.set(key, true); }
        assert_eq!(flags, Flags { pinned: true, muted: true, blocked: true });
        flags.set(Key::Blocked, false);
        assert!(flags.pinned && flags.muted && !flags.blocked);
        assert_eq!(CAP, "draft/metadata-2");
    }
}
