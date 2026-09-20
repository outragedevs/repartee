use std::collections::HashMap;

use crate::state::{AppState, buffer::BufferType};

pub fn resolve(
    state: &AppState,
    connection_id: &str,
    wire_target: &str,
    prefix: Option<&irc::proto::Prefix>,
    tags: Option<&HashMap<String, String>>,
    history_target: Option<&str>,
) -> Option<String> {
    let user_source = match prefix? {
        irc::proto::Prefix::Nickname(nick, user, host) => {
            !nick.is_empty() && (!user.is_empty() || !host.is_empty() || !nick.contains('.'))
        }
        irc::proto::Prefix::ServerName(name) => !name.contains('.'),
    };
    if !user_source {
        return None;
    }
    let connection = state.connections.get(connection_id)?;
    if !connection.enabled_caps.contains("message-tags") {
        return None;
    }
    let support = &connection.isupport_parsed;
    let chantypes = support.get("CHANTYPES").unwrap_or("#&+!");
    let is_channel = |name: &str| {
        name.chars()
            .next()
            .is_some_and(|first| chantypes.contains(first))
    };
    let mut target = wire_target;
    while !is_channel(target) && target.starts_with(|ch| support.statusmsg().contains(ch)) {
        target = target.get(1..)?;
    }
    if target.is_empty()
        || is_channel(target)
        || target.starts_with(['$', ':'])
        || target
            .chars()
            .any(|ch| ch <= ' ' || matches!(ch, '\u{7f}' | ',' | '*' | '?' | '@' | '!'))
    {
        return None;
    }
    let tags = tags?;
    let context = tags
        .get("+channel-context")
        .or_else(|| tags.get("+draft/channel-context"))?;
    if !is_channel(context)
        || context.len() > support.channel_len()
        || context
            .chars()
            .any(|ch| ch <= ' ' || matches!(ch, '\u{7f}' | ','))
    {
        return None;
    }
    let folded = super::isupport::casefold(context, support.casemapping());
    if let Some(history_target) = history_target
        && (!is_channel(history_target)
            || super::isupport::casefold(history_target, support.casemapping()) != folded)
    {
        return None;
    }
    state
        .buffers
        .values()
        .find(|buffer| {
            buffer.connection_id == connection_id
                && buffer.buffer_type == BufferType::Channel
                && super::isupport::casefold(&buffer.name, support.casemapping()) == folded
        })
        .map(|buffer| buffer.name.clone())
        .or_else(|| history_target.map(str::to_owned))
}
