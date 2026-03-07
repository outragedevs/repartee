// Scripting API — event names and documentation.
//
// These are the events emitted through the EventBus that scripts can
// hook into. Matches kokoirc's event surface with additions from
// WeeChat/irssi where useful.

/// Standard event names emitted by the core.
///
/// Scripts use `api.on("irc.privmsg", handler)` etc.
pub mod events {
    // ── IRC events ───────────────────────────────────────────
    // Params match kokoirc's event payloads.

    /// A PRIVMSG received.
    /// Params: `connection_id`, nick, ident, hostname, target, message, `is_channel`
    pub const PRIVMSG: &str = "irc.privmsg";

    /// A CTCP ACTION received.
    /// Params: `connection_id`, nick, ident, hostname, target, message, `is_channel`
    pub const ACTION: &str = "irc.action";

    /// A NOTICE received.
    /// Params: `connection_id`, nick, target, message, `from_server`
    pub const NOTICE: &str = "irc.notice";

    /// A user joined a channel.
    /// Params: `connection_id`, nick, ident, hostname, channel, account
    pub const JOIN: &str = "irc.join";

    /// A user parted a channel.
    /// Params: `connection_id`, nick, ident, hostname, channel, message
    pub const PART: &str = "irc.part";

    /// A user quit the network.
    /// Params: `connection_id`, nick, ident, hostname, message
    pub const QUIT: &str = "irc.quit";

    /// A user was kicked.
    /// Params: `connection_id`, nick, ident, hostname, channel, kicked, message
    pub const KICK: &str = "irc.kick";

    /// A user changed nick.
    /// Params: `connection_id`, nick, `new_nick`, ident, hostname
    pub const NICK: &str = "irc.nick";

    /// Channel topic changed.
    /// Params: `connection_id`, nick, channel, topic
    pub const TOPIC: &str = "irc.topic";

    /// Channel/user mode changed.
    /// Params: `connection_id`, nick, target, modes ("+o nick" etc.)
    pub const MODE: &str = "irc.mode";

    /// Invited to a channel.
    /// Params: `connection_id`, nick, channel
    pub const INVITE: &str = "irc.invite";

    /// WALLOPS message.
    /// Params: `connection_id`, nick, message, `from_server`
    pub const WALLOPS: &str = "irc.wallops";

    /// CTCP request received.
    /// Params: `connection_id`, nick, `ctcp_type`, message
    pub const CTCP_REQUEST: &str = "irc.ctcp_request";

    /// CTCP response received.
    /// Params: `connection_id`, nick, `ctcp_type`, message
    pub const CTCP_RESPONSE: &str = "irc.ctcp_response";

    // ── Connection lifecycle ─────────────────────────────────

    /// Successfully connected and registered on a server.
    /// Params: `connection_id`, nick
    pub const CONNECTED: &str = "connected";

    /// Disconnected from a server.
    /// Params: `connection_id`
    pub const DISCONNECTED: &str = "disconnected";

    // ── App / UI events ──────────────────────────────────────

    /// A message was added to a buffer (after processing).
    /// Params: `buffer_id`, `message_type`, nick, text
    /// Planned: requires state→scripting bridge to emit from `add_message`.
    #[expect(dead_code, reason = "planned event — needs state→scripting bridge")]
    pub const MESSAGE_ADD: &str = "message_add";

    /// User switched to a different buffer.
    /// Params: `from_buffer_id`, `to_buffer_id`
    /// Planned: requires state→scripting bridge to emit from `set_active_buffer`.
    #[expect(dead_code, reason = "planned event — needs state→scripting bridge")]
    pub const BUFFER_SWITCH: &str = "buffer_switch";

    /// User typed a command (before execution). Can be suppressed.
    /// Params: command, args, `connection_id`
    pub const COMMAND_INPUT: &str = "command_input";
}

/// Script file layout expected by the Lua engine:
///
/// ```lua
/// -- ~/.repartee/scripts/greet.lua
///
/// meta = {
///   name = "greet",
///   version = "1.0",
///   description = "Auto-greet joiners",
/// }
///
/// function setup(api)
///   api.on("irc.join", function(ev)
///     if ev.nick ~= api.our_nick() then
///       api.irc.say(ev.channel, "Welcome, " .. ev.nick .. "!")
///     end
///   end)
///
///   -- Return a cleanup function (optional)
///   return function()
///     api.log("greet unloaded")
///   end
/// end
/// ```
///
/// The `setup(api)` function receives the full API object.
/// Returning a function registers it as the cleanup handler
/// called on unload/reload.
pub const LUA_SCRIPT_TEMPLATE: &str = r#"-- Script template
-- Save to ~/.repartee/scripts/<name>.lua

meta = {
  name = "example",
  version = "1.0",
  description = "Example script",
}

function setup(api)
  -- Events: api.on(event_name, handler, priority?)
  -- IRC:    api.irc.say(target, text) / .action / .notice / .raw / .join / .part
  -- UI:     api.ui.print(text) / .switch_buffer(id)
  -- State:  api.store.our_nick() / .buffers() / .connections() / .nicks(buffer_id)
  -- Config: api.config.get(key, default) / .set(key, value)
  -- Timers: api.timer(ms, handler) / api.timeout(ms, handler)
  -- Cmds:   api.command(name, { handler=fn, description=str })
  -- Log:    api.log("message")

  api.on("irc.privmsg", function(ev)
    api.log("got message from " .. ev.nick .. ": " .. ev.message)
  end)

  -- Optional cleanup on unload
  return function()
    api.log("script unloaded")
  end
end
"#;
