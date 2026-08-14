#![allow(clippy::redundant_pub_crate)]

use std::collections::{HashMap, VecDeque};

use super::helpers::add_local_event;
use super::types::{C_CMD, C_DIM, C_ERR, C_HEADER, C_OK, C_RST, C_TEXT, CATEGORY_ORDER, divider};
use crate::app::App;
use crate::state::buffer::{ActivityLevel, Buffer, BufferType, make_buffer_id};

pub(crate) fn cmd_quit(app: &mut App, args: &[String]) {
    if !args.is_empty() {
        app.quit_message = Some(args.join(" "));
    }
    app.should_quit = true;
    // QUIT is sent once in the post-loop cleanup (App::run) to avoid
    // double-QUIT which triggers "Excess Flood" on strict servers.
}

#[expect(
    clippy::missing_const_for_fn,
    reason = "consistent with other command handlers"
)]
pub(crate) fn cmd_detach(app: &mut App, _args: &[String]) {
    app.should_detach = true;
}

pub(crate) fn cmd_help(app: &mut App, args: &[String]) {
    if args.is_empty() {
        show_command_list(app);
    } else {
        let name = args[0].strip_prefix('/').unwrap_or(&args[0]).to_lowercase();
        let subcommand = args.get(1).map(|sub| sub.to_lowercase());
        show_command_help(app, &name, subcommand.as_deref());
    }
}

fn show_command_list(app: &mut App) {
    let commands = super::registry::get_commands();

    add_local_event(app, &divider("Commands"));

    for &cat in CATEGORY_ORDER {
        let cmds_in_cat: Vec<_> = commands
            .iter()
            .filter(|(_, def)| def.category == cat)
            .collect();
        if cmds_in_cat.is_empty() {
            continue;
        }

        add_local_event(app, &format!("  {C_HEADER}[{}]{C_RST}", cat.label()));
        for (name, def) in &cmds_in_cat {
            let aliases = if def.aliases.is_empty() {
                String::new()
            } else {
                format!(" {C_DIM}({}){C_RST}", def.aliases.join(", "))
            };
            add_local_event(
                app,
                &format!(
                    "    {C_CMD}/{name}{C_RST}{aliases} {C_DIM}{}{C_RST}",
                    def.description
                ),
            );
        }
    }

    add_local_event(app, "");
    add_local_event(
        app,
        &format!("  {C_DIM}Type {C_CMD}/help <command>{C_DIM} for detailed help.{C_RST}"),
    );
    add_local_event(app, &divider(""));
}

fn show_command_help(app: &mut App, name: &str, subcommand: Option<&str>) {
    let commands = super::registry::get_commands();

    // Find by name or alias
    let found = commands
        .iter()
        .find(|(cmd_name, def)| *cmd_name == name || def.aliases.contains(&name));

    let Some((cmd_name, def)) = found else {
        add_local_event(
            app,
            &format!("{C_ERR}Unknown command: /{name}. Type /help for a list.{C_RST}"),
        );
        return;
    };

    // Try loading detailed help from docs/commands/*.md
    let doc = super::docs::help(cmd_name);

    if let Some(requested) = subcommand
        && has_structured_subcommands(doc)
    {
        show_subcommand_help(app, cmd_name, doc, requested);
        return;
    }

    add_local_event(app, &divider(&format!("/{cmd_name}")));

    // Description — prefer doc, fall back to registry
    let description = doc.map_or(def.description, |d| d.description.as_str());
    add_local_event(app, &format!("  {C_TEXT}{description}{C_RST}"));
    add_local_event(app, "");

    // Syntax from doc
    if let Some(d) = doc
        && !d.syntax.is_empty()
    {
        for line in d.syntax.lines() {
            add_local_event(app, &format!("  {C_CMD}{line}{C_RST}"));
        }
    }

    if !def.aliases.is_empty() {
        let alias_list: Vec<String> = def.aliases.iter().map(|a| format!("/{a}")).collect();
        add_local_event(
            app,
            &format!("  {C_DIM}Aliases: {}{C_RST}", alias_list.join(", ")),
        );
    }

    // Body (detailed description) from doc
    if let Some(d) = doc {
        add_local_event(app, "");
        for line in d.body.lines() {
            if line.is_empty() {
                add_local_event(app, "");
            } else {
                add_local_event(app, &format!("  {C_TEXT}{line}{C_RST}"));
            }
        }

        // Subcommands
        if !d.subcommands.is_empty() {
            add_local_event(app, "");
            add_local_event(app, &format!("  {C_HEADER}Subcommands:{C_RST}"));
            for sub in &d.subcommands {
                add_local_event(app, &format!("    {C_CMD}{}{C_RST}", sub.name));
                if !sub.description.is_empty() {
                    add_local_event(app, &format!("      {C_DIM}{}{C_RST}", sub.description));
                }
                if !sub.syntax.is_empty() {
                    add_local_event(app, &format!("      {C_CMD}{}{C_RST}", sub.syntax));
                }
            }
        }

        // Examples
        if !d.examples.is_empty() {
            add_local_event(app, "");
            add_local_event(app, &format!("  {C_HEADER}Examples:{C_RST}"));
            for example in &d.examples {
                add_local_event(app, &format!("    {C_CMD}{example}{C_RST}"));
            }
        }

        // See Also
        if !d.see_also.is_empty() {
            add_local_event(app, "");
            add_local_event(
                app,
                &format!("  {C_DIM}See also: {}{C_RST}", d.see_also.join(", ")),
            );
        }
    }

    add_local_event(app, &divider(""));
}

fn has_structured_subcommands(doc: Option<&super::docs::CommandHelp>) -> bool {
    doc.is_some_and(|doc| !doc.subcommands.is_empty())
}

fn show_subcommand_help(
    app: &mut App,
    command: &str,
    doc: Option<&super::docs::CommandHelp>,
    requested: &str,
) {
    let Some(doc) = doc else {
        add_local_event(
            app,
            &format!("{C_ERR}No subcommand help for /{command} {requested}.{C_RST}"),
        );
        return;
    };
    let Some(sub) = super::docs::subcommand(command, requested) else {
        let available = doc
            .subcommands
            .iter()
            .filter_map(|sub| sub.name.split_whitespace().next())
            .collect::<Vec<_>>()
            .join(", ");
        add_local_event(
            app,
            &format!(
                "{C_ERR}Unknown subcommand: /{command} {requested}. Available: {available}{C_RST}"
            ),
        );
        return;
    };

    add_local_event(app, &divider(&format!("/{command} {}", sub.name)));
    if !sub.description.is_empty() {
        add_local_event(app, &format!("  {C_TEXT}{}{C_RST}", sub.description));
    }
    if !sub.syntax.is_empty() {
        add_local_event(app, "");
        for line in sub.syntax.lines() {
            add_local_event(app, &format!("  {C_CMD}{line}{C_RST}"));
        }
    }
    add_local_event(app, &divider(""));
}

pub(crate) fn cmd_clear(app: &mut App, _args: &[String]) {
    let is_mentions = app
        .state
        .active_buffer()
        .is_some_and(|b| b.buffer_type == crate::state::buffer::BufferType::Mentions);
    if let Some(buf) = app.state.active_buffer_mut() {
        buf.messages.clear();
        buf.messages.shrink_to(0);
    }
    // Truncate the mentions DB table when clearing the mentions buffer.
    if is_mentions
        && let Some(storage) = &app.storage
        && let Ok(db) = storage.db.lock()
    {
        crate::storage::query::truncate_mentions(&db).ok();
    }
}

/// Confirmation flag that unlocks closing the protected Mentions window.
/// Deliberately case-sensitive — typing it in full uppercase is the friction.
const CONFIRM_FLAG: &str = "-YES";

/// PART reason used when `/close` is given no reason text.
const DEFAULT_CLOSE_REASON: &str = "Window closed";

/// What `/close` was pointed at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CloseTarget {
    /// No window selector — the active buffer.
    Active,
    /// A single window number: `/wc 22`.
    Window(u32),
    /// An inclusive window-number range: `/wc 22-35`.
    Range { start: u32, end: u32 },
}

/// Parsed `/close` arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CloseArgs {
    target: CloseTarget,
    /// `-YES` was present — unlocks the Mentions guard.
    confirmed: bool,
    /// PART reason for channels; `None` means [`DEFAULT_CLOSE_REASON`].
    reason: Option<String>,
}

/// Recognise a leading window selector: `22` or `22-35`.
///
/// `None` — the token is not numeric at all, so it belongs to the PART reason
/// (`/wc boring in here`). `Some(Err)` — it starts out numeric but is
/// malformed; a fat-fingered range must never silently become a part message.
fn parse_window_selector(token: &str) -> Option<Result<CloseTarget, String>> {
    let numeric = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let (lhs, rhs) = token
        .split_once('-')
        .map_or((token, None), |(l, r)| (l, Some(r)));

    if !numeric(lhs) {
        return None;
    }
    let Ok(start) = lhs.parse::<u32>() else {
        return Some(Err(format!("Window number out of range: {lhs}")));
    };

    let Some(rhs) = rhs else {
        if start == 0 {
            return Some(Err("Window numbers start at 1".to_string()));
        }
        return Some(Ok(CloseTarget::Window(start)));
    };

    // `22-`, `22-x`, `22-35-40` — looked like a range, isn't one.
    if !numeric(rhs) {
        return Some(Err(format!("Invalid window range: {token}")));
    }
    let Ok(end) = rhs.parse::<u32>() else {
        return Some(Err(format!("Window number out of range: {rhs}")));
    };
    if start == 0 {
        return Some(Err("Window numbers start at 1".to_string()));
    }
    if start > end {
        return Some(Err(format!(
            "Invalid window range: {token} — start is past end"
        )));
    }
    Some(Ok(CloseTarget::Range { start, end }))
}

/// Split `/close` arguments into window selector, `-YES` flag and PART reason.
fn parse_close_args(args: &[String]) -> Result<CloseArgs, String> {
    let mut confirmed = false;
    let mut rest: Vec<&str> = Vec::with_capacity(args.len());
    for arg in args {
        if arg == CONFIRM_FLAG {
            confirmed = true;
        } else {
            rest.push(arg.as_str());
        }
    }

    let mut target = CloseTarget::Active;
    if let Some(first) = rest.first().copied() {
        match parse_window_selector(first) {
            Some(Ok(parsed)) => {
                target = parsed;
                rest.remove(0);
            }
            Some(Err(err)) => return Err(err),
            None => {}
        }
    }

    Ok(CloseArgs {
        target,
        confirmed,
        reason: (!rest.is_empty()).then(|| rest.join(" ")),
    })
}

/// Resolve a 1-based window number against the sidebar numbering.
fn window_id(numbered: &[String], num: u32) -> Option<String> {
    let idx = usize::try_from(num.checked_sub(1)?).ok()?;
    numbered.get(idx).cloned()
}

/// Split channel names into `PART` target lists that each fit one IRC line.
///
/// The wire format is `PART <targets> :<reason>\r\n` and the whole line must
/// stay inside [`crate::irc::PROTOCOL_LINE_MAX_BYTES`]. `targmax` is the
/// server's advertised `TARGMAX=PART` limit, if it advertises one. Always
/// emits at least one channel per chunk: if a pathological reason eats the
/// entire budget, one over-long line the server truncates still beats
/// silently dropping the PART.
fn chunk_part_targets(channels: &[String], reason: &str, targmax: Option<usize>) -> Vec<String> {
    const OVERHEAD: usize = "PART ".len() + " :".len() + "\r\n".len();
    let budget = crate::irc::PROTOCOL_LINE_MAX_BYTES.saturating_sub(OVERHEAD + reason.len());
    let max_targets = targmax.unwrap_or(usize::MAX).max(1);

    let mut chunks: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut count = 0usize;

    for name in channels {
        let would_be = if current.is_empty() {
            name.len()
        } else {
            current.len() + 1 + name.len()
        };
        if !current.is_empty() && (would_be > budget || count >= max_targets) {
            chunks.push(std::mem::take(&mut current));
            count = 0;
        }
        if !current.is_empty() {
            current.push(',');
        }
        current.push_str(name);
        count += 1;
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

pub(crate) fn cmd_close(app: &mut App, args: &[String]) {
    let parsed = match parse_close_args(args) {
        Ok(parsed) => parsed,
        Err(err) => {
            add_local_event(app, &format!("{C_ERR}{err}{C_RST}"));
            return;
        }
    };
    let reason = parsed.reason.as_deref();
    let mut summary = None;

    match parsed.target {
        CloseTarget::Active => {
            let Some(buf_id) = app.state.active_buffer_id.clone() else {
                return;
            };
            close_one(app, &buf_id, reason, parsed.confirmed, "/wc -YES");
        }
        CloseTarget::Window(num) => {
            let numbered = app.state.numbered_buffer_ids();
            let Some(buf_id) = window_id(&numbered, num) else {
                add_local_event(
                    app,
                    &format!(
                        "{C_ERR}No such window: {num} — highest window is {}{C_RST}",
                        numbered.len()
                    ),
                );
                return;
            };
            let hint = format!("/wc {num} -YES");
            close_one(app, &buf_id, reason, parsed.confirmed, &hint);
        }
        CloseTarget::Range { start, end } => {
            summary = close_range(app, start, end, reason, parsed.confirmed);
        }
    }

    // Recreate default Status if no real buffers remain. This has to happen
    // before the summary is printed: a range that closed every window leaves
    // no active buffer, and `add_local_event` silently drops messages then.
    app.ensure_default_status();
    if let Some(summary) = summary {
        add_local_event(app, &summary);
    }
}

/// Close one buffer by ID.
///
/// `retry_hint` is the exact command echoed back when the Mentions guard
/// blocks the close, so `/wc` and `/wc 1` each suggest the form that works.
fn close_one(app: &mut App, buf_id: &str, reason: Option<&str>, confirmed: bool, retry_hint: &str) {
    let Some(buf) = app.state.buffers.get(buf_id) else {
        return;
    };
    let buf_type = buf.buffer_type.clone();
    let buf_name = buf.name.clone();
    let conn_id = buf.connection_id.clone();

    match buf_type {
        BufferType::Mentions => {
            if !confirmed {
                add_local_event(
                    app,
                    &format!(
                        "{C_ERR}Mentions is a protected window. Use \
                         {C_CMD}{retry_hint}{C_ERR} if you really want to close it.{C_RST}"
                    ),
                );
                return;
            }
            // Session-only: `display.mentions_buffer` stays on, so `/mentions`
            // — or the next restart — brings the window straight back.
            app.state.remove_buffer(buf_id);
        }
        BufferType::Channel => {
            // Irssi-style fast close: drop the buffer locally first so
            // the UI reacts instantly, then fire-and-forget PART to the
            // server. We do NOT wait for the server-side echo before
            // removing the buffer — on a laggy link the previous
            // behaviour kept a dead window visible for seconds. The
            // echo handler `handle_part` in irc/events.rs also calls
            // `remove_buffer`; that call is now a no-op because the
            // buffer is already gone (remove_buffer is idempotent).
            let reason = reason.unwrap_or(DEFAULT_CLOSE_REASON).to_string();
            if let Some(handle) = app.irc_handles.get(&conn_id) {
                let _ = handle
                    .sender()
                    .send(irc::proto::Command::PART(buf_name, Some(reason)));
            }
            app.state.remove_buffer(buf_id);
        }
        BufferType::Query | BufferType::DccChat => {
            // DCC chat buffers close like query buffers — just remove locally.
            app.state.remove_buffer(buf_id);
        }
        BufferType::Log => {
            // Log buffers are read-only — `/close` just removes them from the
            // sidebar; the underlying SQLite rows are untouched.
            app.state.remove_buffer(buf_id);
        }
        BufferType::Shell => {
            // Shell close is handled by App::close_shell_buffer() — wired in Task 5.
            app.close_shell_buffer(buf_id);
        }
        BufferType::Server | BufferType::Special => {
            let is_disconnected = app.state.connections.get(&conn_id).is_none_or(|c| {
                matches!(
                    c.status,
                    crate::state::connection::ConnectionStatus::Disconnected
                        | crate::state::connection::ConnectionStatus::Error
                )
            });
            if is_disconnected {
                // Remove all buffers for this connection
                let to_remove: Vec<String> = app
                    .state
                    .buffers
                    .keys()
                    .filter(|id| {
                        app.state
                            .buffers
                            .get(id.as_str())
                            .is_some_and(|b| b.connection_id == conn_id)
                    })
                    .cloned()
                    .collect();
                for id in to_remove {
                    app.state.remove_buffer(&id);
                }
                app.state.connections.remove(&conn_id);
            } else {
                add_local_event(
                    app,
                    "Cannot close server buffer while connected. /disconnect first",
                );
            }
        }
    }
}

/// Match an inclusive window range against the current window list.
///
/// Returns the buffer IDs to close, or the message explaining the refusal.
/// Guards run across the whole range BEFORE anything closes — a bulk close
/// that half-succeeded would leave the user guessing which windows went.
fn resolve_range(
    state: &crate::state::AppState,
    start: u32,
    end: u32,
    confirmed: bool,
) -> Result<Vec<String>, String> {
    let numbered = state.numbered_buffer_ids();
    let start_idx = usize::try_from(start.saturating_sub(1)).unwrap_or(usize::MAX);
    if start_idx >= numbered.len() {
        return Err(format!(
            "{C_ERR}No windows in range {start}-{end} — highest window is {}{C_RST}",
            numbered.len()
        ));
    }
    // A range running past the last window closes what exists rather than
    // erroring: `/wc 20-999` is a normal way to say "everything from 20 on".
    let end_idx = usize::try_from(end.saturating_sub(1))
        .unwrap_or(usize::MAX)
        .min(numbered.len() - 1);
    let targets = &numbered[start_idx..=end_idx];

    let blocked = targets.iter().enumerate().find_map(|(offset, id)| {
        let buf = state.buffers.get(id.as_str())?;
        let num = start_idx + offset + 1;
        match buf.buffer_type {
            BufferType::Mentions if !confirmed => Some(format!(
                "{C_ERR}Range {start}-{end} includes the protected Mentions window ({num}). \
                 Use {C_CMD}/wc {start}-{end} -YES{C_ERR} if you really want it gone.{C_RST}"
            )),
            BufferType::Server | BufferType::Special => {
                let label = state
                    .connections
                    .get(&buf.connection_id)
                    .map_or(buf.connection_id.as_str(), |c| c.label.as_str());
                // Closing a server window drops its entire network, including
                // buffers OUTSIDE the range — too destructive to hide behind a
                // flag shared with the Mentions guard.
                Some(format!(
                    "{C_ERR}Range {start}-{end} includes server window {num} ({label}). \
                     Closing it would drop the whole network — close it on its own with \
                     {C_CMD}/wc {num}{C_ERR}.{C_RST}"
                ))
            }
            _ => None,
        }
    });
    if let Some(message) = blocked {
        return Err(message);
    }
    Ok(targets.to_vec())
}

/// Close every window in the inclusive number range `start..=end`.
///
/// Returns the summary line for the caller to print once the window list has
/// settled, or `None` if the range was refused (the refusal is printed here,
/// while there is definitely still a window to print it in).
fn close_range(
    app: &mut App,
    start: u32,
    end: u32,
    reason: Option<&str>,
    confirmed: bool,
) -> Option<String> {
    let targets = match resolve_range(&app.state, start, end, confirmed) {
        Ok(targets) => targets,
        Err(message) => {
            add_local_event(app, &message);
            return None;
        }
    };
    let last_num = start as usize + targets.len().saturating_sub(1);

    // Channels are grouped per connection into `PART #a,#b,#c :reason`.
    // irc-repartee throttles outgoing traffic to `max_messages_in_burst` per
    // `burst_window_length` (15 per 8s by default), so closing 30 channels one
    // PART at a time would trickle out over ~16s and risk an Excess Flood kill.
    let mut part_lists: HashMap<String, Vec<String>> = HashMap::new();
    let mut closed = 0usize;
    for id in &targets {
        let Some(buf) = app.state.buffers.get(id.as_str()) else {
            continue;
        };
        let buf_type = buf.buffer_type.clone();
        let name = buf.name.clone();
        let conn_id = buf.connection_id.clone();
        match buf_type {
            BufferType::Channel => {
                part_lists.entry(conn_id).or_default().push(name);
                app.state.remove_buffer(id);
            }
            BufferType::Shell => app.close_shell_buffer(id),
            _ => app.state.remove_buffer(id),
        }
        closed += 1;
    }

    let reason = reason.unwrap_or(DEFAULT_CLOSE_REASON);
    for (conn_id, channels) in part_lists {
        let targmax = app
            .state
            .connections
            .get(&conn_id)
            .and_then(|c| c.isupport_parsed.targmax("PART"));
        let Some(handle) = app.irc_handles.get(&conn_id) else {
            continue;
        };
        for chanlist in chunk_part_targets(&channels, reason, targmax) {
            let _ = handle
                .sender()
                .send(irc::proto::Command::PART(chanlist, Some(reason.to_string())));
        }
    }

    Some(format!(
        "{C_OK}Closed {closed} window{} ({start}-{last_num}){C_RST}",
        if closed == 1 { "" } else { "s" }
    ))
}

// === Alias commands ===

pub(crate) fn cmd_alias(app: &mut App, args: &[String]) {
    if args.is_empty() {
        // List all aliases
        let mut lines = vec![divider("Aliases")];
        if app.config.aliases.is_empty() {
            lines.push(format!("  {C_DIM}No aliases defined{C_RST}"));
        } else {
            let mut sorted: Vec<_> = app.config.aliases.iter().collect();
            sorted.sort_by_key(|(a, _)| *a);
            for (name, template) in sorted {
                lines.push(format!(
                    "  {C_CMD}/{name}{C_RST} = {C_TEXT}{template}{C_RST}"
                ));
            }
        }
        lines.push(divider(""));
        for line in &lines {
            add_local_event(app, line);
        }
        return;
    }

    // `/alias -name` removes the alias (irssi compat)
    if let Some(removal) = args
        .first()
        .and_then(|a| a.strip_prefix('-'))
        .filter(|_| args.len() == 1)
    {
        let name = removal.to_lowercase();
        if app.config.aliases.remove(&name).is_some() {
            app.cached_config_toml = None;
            let _ = crate::config::save_config(&crate::constants::config_path(), &app.config);
            add_local_event(app, &format!("{C_OK}Removed alias: /{name}{C_RST}"));
        } else {
            add_local_event(app, &format!("{C_ERR}No alias named: /{name}{C_RST}"));
        }
        return;
    }

    // `/alias name` (one arg, no body) — show that specific alias
    if args.len() < 2 {
        let name = args[0].strip_prefix('/').unwrap_or(&args[0]).to_lowercase();
        if let Some(body) = app.config.aliases.get(&name) {
            add_local_event(
                app,
                &format!("  {C_CMD}/{name}{C_RST} = {C_TEXT}{body}{C_RST}"),
            );
        } else {
            add_local_event(app, &format!("{C_ERR}No alias named: /{name}{C_RST}"));
        }
        return;
    }

    let name = args[0].strip_prefix('/').unwrap_or(&args[0]).to_lowercase();
    let template = args[1].clone();

    // Check if it conflicts with a built-in command
    let builtins = super::registry::get_command_names();
    if builtins.contains(&name.as_str()) {
        add_local_event(
            app,
            &format!("{C_ERR}Cannot override built-in command: /{name}{C_RST}"),
        );
        return;
    }

    app.config.aliases.insert(name.clone(), template.clone());
    app.cached_config_toml = None;
    let _ = crate::config::save_config(&crate::constants::config_path(), &app.config);
    add_local_event(app, &format!("{C_OK}Alias /{name} = {template}{C_RST}"));
}

pub(crate) fn cmd_unalias(app: &mut App, args: &[String]) {
    if args.is_empty() {
        add_local_event(app, "Usage: /unalias <name>");
        return;
    }

    let name = args[0].strip_prefix('/').unwrap_or(&args[0]).to_lowercase();

    if app.config.aliases.remove(&name).is_some() {
        app.cached_config_toml = None;
        let _ = crate::config::save_config(&crate::constants::config_path(), &app.config);
        add_local_event(app, &format!("{C_OK}Removed alias: /{name}{C_RST}"));
    } else {
        add_local_event(app, &format!("{C_ERR}No alias named: /{name}{C_RST}"));
    }
}

// === Items command ===

#[expect(
    clippy::too_many_lines,
    reason = "single match dispatching all /items subcommands"
)]
pub(crate) fn cmd_items(app: &mut App, args: &[String]) {
    if args.is_empty() || args[0] == "list" {
        let mut lines = vec![divider("Statusbar Items")];
        if app.config.statusbar.items.is_empty() {
            lines.push(format!("  {C_DIM}No items configured{C_RST}"));
        } else {
            for (i, item) in app.config.statusbar.items.iter().enumerate() {
                let name = statusbar_item_name(item);
                lines.push(format!("  {C_CMD}{}. {name}{C_RST}", i + 1));
            }
        }
        lines.push(format!("  {C_DIM}Available: {AVAILABLE_ITEMS}{C_RST}"));
        lines.push(divider(""));
        for line in &lines {
            add_local_event(app, line);
        }
        return;
    }

    match args[0].as_str() {
        "add" => {
            if args.len() < 2 {
                add_local_event(app, "Usage: /items add <item_name>");
                return;
            }
            let item_name = &args[1];
            match parse_statusbar_item(item_name) {
                Some(item) => {
                    // Check for duplicates
                    if app.config.statusbar.items.contains(&item) {
                        add_local_event(
                            app,
                            &format!("{C_ERR}{item_name} is already in the statusbar{C_RST}"),
                        );
                        return;
                    }
                    app.config.statusbar.items.push(item);
                    save_statusbar(app);
                    add_local_event(app, &format!("{C_OK}Added {item_name} to statusbar{C_RST}"));
                }
                None => {
                    add_local_event(
                        app,
                        &format!(
                            "{C_ERR}Unknown item: {item_name}. Available: {AVAILABLE_ITEMS}{C_RST}"
                        ),
                    );
                }
            }
        }
        "remove" => {
            if args.len() < 2 {
                add_local_event(app, "Usage: /items remove <item_name>");
                return;
            }
            let item_name = &args[1];
            match parse_statusbar_item(item_name) {
                Some(item) => {
                    if let Some(pos) = app.config.statusbar.items.iter().position(|i| *i == item) {
                        app.config.statusbar.items.remove(pos);
                        save_statusbar(app);
                        add_local_event(
                            app,
                            &format!("{C_OK}Removed {item_name} from statusbar{C_RST}"),
                        );
                    } else {
                        add_local_event(
                            app,
                            &format!("{C_ERR}{item_name} is not in the statusbar{C_RST}"),
                        );
                    }
                }
                None => {
                    add_local_event(
                        app,
                        &format!(
                            "{C_ERR}Unknown item: {item_name}. Available: {AVAILABLE_ITEMS}{C_RST}"
                        ),
                    );
                }
            }
        }
        "move" => {
            if args.len() < 3 {
                add_local_event(app, "Usage: /items move <item_name> <position>");
                return;
            }
            let item_name = &args[1];
            let Some(item) = parse_statusbar_item(item_name) else {
                add_local_event(
                    app,
                    &format!(
                        "{C_ERR}Unknown item: {item_name}. Available: {AVAILABLE_ITEMS}{C_RST}"
                    ),
                );
                return;
            };
            let Some(current_pos) = app.config.statusbar.items.iter().position(|i| *i == item)
            else {
                add_local_event(
                    app,
                    &format!("{C_ERR}{item_name} is not in the statusbar{C_RST}"),
                );
                return;
            };
            let Ok(new_pos) = args[2].parse::<usize>() else {
                add_local_event(app, &format!("{C_ERR}Invalid position: {}{C_RST}", args[2]));
                return;
            };
            if new_pos == 0 || new_pos > app.config.statusbar.items.len() {
                add_local_event(
                    app,
                    &format!(
                        "{C_ERR}Position must be 1-{}{C_RST}",
                        app.config.statusbar.items.len()
                    ),
                );
                return;
            }
            let removed = app.config.statusbar.items.remove(current_pos);
            app.config.statusbar.items.insert(new_pos - 1, removed);
            save_statusbar(app);
            add_local_event(
                app,
                &format!("{C_OK}Moved {item_name} to position {new_pos}{C_RST}"),
            );
        }
        "format" => {
            if args.len() < 2 {
                add_local_event(app, "Usage: /items format <item_name> [format_string]");
                return;
            }
            let item_name = args[1].to_lowercase();
            if parse_statusbar_item(&item_name).is_none() {
                add_local_event(
                    app,
                    &format!(
                        "{C_ERR}Unknown item: {item_name}. Available: {AVAILABLE_ITEMS}{C_RST}"
                    ),
                );
                return;
            }
            if args.len() < 3 {
                // Show current format
                let fmt = app
                    .config
                    .statusbar
                    .item_formats
                    .get(&item_name)
                    .map_or("(default)", String::as_str);
                add_local_event(
                    app,
                    &format!("{C_CMD}{item_name}{C_RST} format: {C_TEXT}{fmt}{C_RST}"),
                );
                return;
            }
            let fmt = args[2].clone();
            app.config
                .statusbar
                .item_formats
                .insert(item_name.clone(), fmt.clone());
            save_statusbar(app);
            add_local_event(app, &format!("{C_OK}Set {item_name} format: {fmt}{C_RST}"));
        }
        "separator" => {
            if args.len() < 2 {
                add_local_event(
                    app,
                    &format!(
                        "Current separator: {C_CMD}{}{C_RST}",
                        app.config.statusbar.separator
                    ),
                );
                return;
            }
            app.config.statusbar.separator.clone_from(&args[1]);
            save_statusbar(app);
            add_local_event(app, &format!("{C_OK}Separator set to: {}{C_RST}", args[1]));
        }
        "available" => {
            add_local_event(
                app,
                &format!("Available statusbar items: {C_CMD}{AVAILABLE_ITEMS}{C_RST}"),
            );
        }
        "reset" => {
            app.config.statusbar.items = crate::config::StatusbarConfig::default().items;
            app.config.statusbar.item_formats.clear();
            app.config.statusbar.separator = " | ".to_string();
            save_statusbar(app);
            add_local_event(app, &format!("{C_OK}Statusbar reset to defaults{C_RST}"));
        }
        _ => {
            add_local_event(
                app,
                "Usage: /items [list|add|remove|move|format|separator|available|reset]",
            );
        }
    }
}

const AVAILABLE_ITEMS: &str = "time, nick_info, channel_info, typing, lag, active_windows";

/// Persist a statusbar change and tell open browser tabs about it.
///
/// The web status line renders from `statusbar.items` too, so a mutation that
/// only touched the config would take effect in the terminal and nowhere else
/// until the tab was reloaded — the two UIs must not be able to drift.
fn save_statusbar(app: &mut App) {
    app.cached_config_toml = None;
    let _ = crate::config::save_config(&crate::constants::config_path(), &app.config);
    push_statusbar_web_event(app);
}

/// Queue the current status-line config for the connected web clients.
/// Also called from `/set statusbar.*` and `/reload`, which change the same
/// state by other routes.
pub(crate) fn push_statusbar_web_event(app: &mut App) {
    app.state
        .pending_web_events
        .push(crate::web::protocol::WebEvent::StatusbarConfig {
            items: crate::web::snapshot::statusbar_item_names(&app.config.statusbar),
            enabled: app.config.statusbar.enabled,
        });
}

/// The inverse of [`statusbar_item_name`]. Shared with the web layer so the
/// names on the wire are exactly the names `/items` speaks.
pub(crate) fn parse_statusbar_item(name: &str) -> Option<crate::config::StatusbarItem> {
    use crate::config::StatusbarItem;
    match name.to_lowercase().as_str() {
        "time" => Some(StatusbarItem::Time),
        "nick_info" => Some(StatusbarItem::NickInfo),
        "channel_info" => Some(StatusbarItem::ChannelInfo),
        "typing" => Some(StatusbarItem::Typing),
        "lag" => Some(StatusbarItem::Lag),
        "active_windows" => Some(StatusbarItem::ActiveWindows),
        _ => None,
    }
}

/// The name `/items` (and the web protocol) uses for an item.
pub(crate) const fn statusbar_item_name(item: &crate::config::StatusbarItem) -> &'static str {
    use crate::config::StatusbarItem;
    match item {
        StatusbarItem::Time => "time",
        StatusbarItem::NickInfo => "nick_info",
        StatusbarItem::ChannelInfo => "channel_info",
        StatusbarItem::Typing => "typing",
        StatusbarItem::Lag => "lag",
        StatusbarItem::ActiveWindows => "active_windows",
    }
}

// === Shell commands ===

pub(crate) fn cmd_shell(app: &mut App, args: &[String]) {
    let sub = args.first().map_or("open", String::as_str);
    match sub {
        "open" | "" => shell_open(app, None),
        "cmd" => {
            let command = args.get(1).map(String::as_str);
            if command.is_none() {
                add_local_event(app, &format!("{C_ERR}Usage: /shell cmd <command>{C_RST}"));
                return;
            }
            shell_open(app, command);
        }
        "close" => {
            let shell_buf = app.state.active_buffer().and_then(|buf| {
                if buf.buffer_type == BufferType::Shell {
                    Some(buf.id.clone())
                } else {
                    None
                }
            });
            let Some(buf_id) = shell_buf else {
                add_local_event(app, &format!("{C_ERR}Active buffer is not a shell{C_RST}"));
                return;
            };
            app.close_shell_buffer(&buf_id);
            app.ensure_default_status();
        }
        "list" => {
            let sessions: Vec<(String, String)> = app
                .shell_mgr
                .list_sessions()
                .iter()
                .map(|(id, _, label)| ((*id).to_string(), (*label).to_string()))
                .collect();
            if sessions.is_empty() {
                add_local_event(app, &format!("{C_DIM}No active shell sessions{C_RST}"));
                return;
            }
            add_local_event(app, &format!("{C_HEADER}Shell sessions:{C_RST}"));
            for (id, label) in &sessions {
                add_local_event(
                    app,
                    &format!("  {C_CMD}{id}{C_RST} — {C_TEXT}{label}{C_RST}"),
                );
            }
        }
        _ => {
            // Treat unknown subcommand as a command to run.
            shell_open(app, Some(sub));
        }
    }
}

/// Open a new shell session and create the associated buffer.
fn shell_open(app: &mut App, command: Option<&str>) {
    // Ensure the "Shell" sidebar header exists.
    app.ensure_shell_connection();

    // Compute actual chat area dimensions (matching the ratatui layout).
    // Shell buffers never show the nick list panel.
    let (cols, rows) = crate::ui::layout::compute_chat_area_size(
        app.cached_term_cols,
        app.cached_term_rows,
        app.config.sidepanel.left.visible,
        app.config.sidepanel.left.width,
        false, // shell buffers never show nick list
        0,
    );
    tracing::debug!(
        term_cols = app.cached_term_cols,
        term_rows = app.cached_term_rows,
        left_visible = app.config.sidepanel.left.visible,
        left_width = app.config.sidepanel.left.width,
        pty_cols = cols,
        pty_rows = rows,
        "shell: opening PTY with computed dimensions"
    );

    // Determine the display label from the command basename.
    let base_label = command
        .and_then(|c| std::path::Path::new(c).file_name().and_then(|n| n.to_str()))
        .map(String::from)
        .or_else(|| {
            std::env::var("SHELL").ok().and_then(|s| {
                std::path::Path::new(&s)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(String::from)
            })
        })
        .unwrap_or_else(|| "shell".to_string());

    let buf_name = find_unique_shell_name(app, &base_label);
    let buf_id = make_buffer_id(App::SHELL_CONN_ID, &buf_name);

    match app.shell_mgr.open(cols, rows, command, &buf_id) {
        Ok((_shell_id, _label)) => {
            app.state.add_buffer(Buffer {
                id: buf_id.clone(),
                connection_id: App::SHELL_CONN_ID.to_string(),
                buffer_type: BufferType::Shell,
                name: buf_name,
                messages: VecDeque::new(),
                activity: ActivityLevel::None,
                unread_count: 0,
                last_read: chrono::Utc::now(),
                topic: None,
                topic_set_by: None,
                users: HashMap::new(),
                modes: None,
                mode_params: None,
                list_modes: HashMap::new(),
                last_speakers: Vec::new(),
                peer_handle: None,
                log_total_lines: None,
                log_oldest_ts: None,
                log_newest_ts: None,
                history_exhausted: false,
                log_initial_loaded: false,
                pin_backlog: false,
            });
            app.state.set_active_buffer(&buf_id);
            app.shell_input_active = true;
        }
        Err(e) => {
            add_local_event(app, &format!("{C_ERR}Failed to open shell: {e}{C_RST}"));
        }
    }
}

/// Find a unique shell buffer name, appending " (2)", " (3)", etc. on collision.
fn find_unique_shell_name(app: &App, base: &str) -> String {
    let candidate = make_buffer_id(App::SHELL_CONN_ID, base);
    if !app.state.buffers.contains_key(&candidate) {
        return base.to_string();
    }
    for n in 2..=100 {
        let name = format!("{base} ({n})");
        let candidate = make_buffer_id(App::SHELL_CONN_ID, &name);
        if !app.state.buffers.contains_key(&candidate) {
            return name;
        }
    }
    format!("{base} ({})", app.shell_mgr.session_count() + 1)
}

/// `/emote` opens the picker; `/emote <name>` inserts `:name:` if known, else
/// lists a few matching emote names to the active buffer.
pub(crate) fn cmd_emote(app: &mut App, args: &[String]) {
    if !app.emotes_input_enabled() {
        add_local_event(
            app,
            "Emotes are disabled ([emotes] enabled=false or render=off)",
        );
        return;
    }
    // No argument (or a bare ":" / "::") opens the picker. Matching is
    // case-insensitive (emote names are lowercase).
    let query = args
        .first()
        .map_or(String::new(), |a| a.trim_matches(':').to_ascii_lowercase());
    if query.is_empty() {
        app.open_emote_picker();
        return;
    }
    if let Some(idx) = crate::emotes::resolve(&query) {
        // Reuse the shared insert path (current language; clears stale tab-state).
        app.insert_emote_by_index(idx);
    } else {
        let hits: Vec<&str> = crate::emotes::tag_names()
            .iter()
            .filter(|n| n.contains(query.as_str()))
            .take(10)
            .copied()
            .collect();
        let msg = if hits.is_empty() {
            format!("No emote matches \"{query}\"")
        } else {
            format!("Emotes matching \"{query}\": {}", hits.join(", "))
        };
        add_local_event(app, &msg);
    }
}

/// `/wizard <kind> [args]` — open a guided popup form. Currently only
/// `server [id]` (add, or edit an existing server pre-filled).
pub(crate) fn cmd_wizard(app: &mut App, args: &[String]) {
    match args.first().map(String::as_str) {
        Some("server") => app.open_server_wizard(args.get(1).map(String::as_str)),
        _ => add_local_event(
            app,
            &format!("{C_TEXT}Usage: /wizard server [id]  — open the add/edit-server form{C_RST}"),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::StatusbarItem;

    #[test]
    fn syntax_only_actions_fall_back_to_main_command_help() {
        assert!(!has_structured_subcommands(super::super::docs::help("flood")));
    }

    #[test]
    fn documented_subcommands_use_specialized_help() {
        assert!(has_structured_subcommands(super::super::docs::help("server")));
    }

    #[test]
    fn typing_is_a_manageable_statusbar_item() {
        // /items add typing must work, and the default item must have a name.
        assert_eq!(parse_statusbar_item("typing"), Some(StatusbarItem::Typing));
        assert_eq!(statusbar_item_name(&StatusbarItem::Typing), "typing");
        assert!(AVAILABLE_ITEMS.contains("typing"));
    }
}

#[cfg(test)]
mod close_tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|v| (*v).to_string()).collect()
    }

    fn parse(values: &[&str]) -> CloseArgs {
        parse_close_args(&args(values)).expect("should parse")
    }

    fn parse_err(values: &[&str]) -> String {
        parse_close_args(&args(values)).expect_err("should reject")
    }

    #[test]
    fn bare_close_targets_the_active_window() {
        let parsed = parse(&[]);
        assert_eq!(parsed.target, CloseTarget::Active);
        assert!(!parsed.confirmed);
        assert_eq!(parsed.reason, None);
    }

    #[test]
    fn non_numeric_arguments_stay_a_part_reason() {
        // The pre-existing `/wc <reason>` form must not change meaning.
        let parsed = parse(&["boring", "in", "here"]);
        assert_eq!(parsed.target, CloseTarget::Active);
        assert_eq!(parsed.reason.as_deref(), Some("boring in here"));
    }

    #[test]
    fn single_number_selects_one_window() {
        assert_eq!(parse(&["22"]).target, CloseTarget::Window(22));
    }

    #[test]
    fn number_and_reason_split_apart() {
        let parsed = parse(&["22", "see", "you"]);
        assert_eq!(parsed.target, CloseTarget::Window(22));
        assert_eq!(parsed.reason.as_deref(), Some("see you"));
    }

    #[test]
    fn range_selects_inclusive_span() {
        assert_eq!(
            parse(&["22-35"]).target,
            CloseTarget::Range {
                start: 22,
                end: 35
            }
        );
    }

    #[test]
    fn single_window_range_is_still_a_range() {
        // `/wc 7-7` opts into the strict bulk guards; `/wc 7` does not.
        assert_eq!(
            parse(&["7-7"]).target,
            CloseTarget::Range { start: 7, end: 7 }
        );
    }

    #[test]
    fn confirm_flag_is_accepted_in_any_position() {
        for form in [
            vec!["-YES"],
            vec!["1", "-YES"],
            vec!["-YES", "1"],
            vec!["1-9", "-YES", "bye"],
        ] {
            assert!(parse(&form).confirmed, "{form:?} should confirm");
        }
    }

    #[test]
    fn confirm_flag_is_case_sensitive() {
        // Lowercase is not the confirmation — it falls through to the reason,
        // so a half-hearted `-yes` never closes the Mentions window.
        let parsed = parse(&["-yes"]);
        assert!(!parsed.confirmed);
        assert_eq!(parsed.reason.as_deref(), Some("-yes"));
    }

    #[test]
    fn confirm_flag_is_stripped_from_the_reason() {
        let parsed = parse(&["3-5", "-YES", "cleaning", "up"]);
        assert_eq!(parsed.reason.as_deref(), Some("cleaning up"));
    }

    #[test]
    fn reversed_range_is_rejected() {
        assert!(parse_err(&["35-22"]).contains("start is past end"));
    }

    #[test]
    fn zero_is_rejected() {
        // Window 0 is the Alt+0 Status buffer, which has no sidebar number.
        assert!(parse_err(&["0"]).contains("start at 1"));
        assert!(parse_err(&["0-5"]).contains("start at 1"));
    }

    #[test]
    fn malformed_ranges_are_rejected_not_treated_as_reasons() {
        // Each of these starts numeric, so it is a typo'd range — turning it
        // into a PART reason would part the active channel by surprise.
        for token in ["22-", "22-x", "22-35-40"] {
            assert!(
                parse_err(&[token]).contains("Invalid window range"),
                "{token} should be rejected"
            );
        }
    }

    #[test]
    fn out_of_u32_range_numbers_are_rejected() {
        assert!(parse_err(&["99999999999"]).contains("out of range"));
        assert!(parse_err(&["1-99999999999"]).contains("out of range"));
    }

    #[test]
    fn leading_dash_words_are_reasons_not_selectors() {
        let parsed = parse(&["-brb"]);
        assert_eq!(parsed.target, CloseTarget::Active);
        assert_eq!(parsed.reason.as_deref(), Some("-brb"));
    }

    /// End-to-end through the real command parser: `/close` and its `/wc`
    /// alias must produce identical arguments. They did not — `close` sat in
    /// `GREEDY_COMMANDS`, so the canonical name handed the handler one blob
    /// ("22 see you") while the alias handed it tokens.
    fn parse_line(line: &str) -> CloseArgs {
        let parsed = crate::commands::parser::parse_command(line).expect("is a command");
        parse_close_args(&parsed.args).expect("should parse")
    }

    #[test]
    fn close_and_wc_parse_identically() {
        for (canonical, alias) in [
            ("/close 22", "/wc 22"),
            ("/close 22 see you", "/wc 22 see you"),
            ("/close 3-5", "/wc 3-5"),
            ("/close 3-5 bye", "/wc 3-5 bye"),
            ("/close 1 -YES", "/wc 1 -YES"),
            ("/close -YES", "/wc -YES"),
            ("/close going to bed", "/wc going to bed"),
            ("/close", "/wc"),
        ] {
            assert_eq!(
                parse_line(canonical),
                parse_line(alias),
                "{canonical} and {alias} must agree"
            );
        }
    }

    #[test]
    fn canonical_close_honours_a_selector_before_a_reason() {
        let parsed = parse_line("/close 22 see you");
        assert_eq!(parsed.target, CloseTarget::Window(22));
        assert_eq!(parsed.reason.as_deref(), Some("see you"));

        let parsed = parse_line("/close 3-5 bye");
        assert_eq!(parsed.target, CloseTarget::Range { start: 3, end: 5 });
        assert_eq!(parsed.reason.as_deref(), Some("bye"));

        let parsed = parse_line("/close 1 -YES");
        assert_eq!(parsed.target, CloseTarget::Window(1));
        assert!(parsed.confirmed);
    }

    #[test]
    fn window_id_maps_one_based_numbers() {
        let numbered = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        assert_eq!(window_id(&numbered, 1).as_deref(), Some("a"));
        assert_eq!(window_id(&numbered, 3).as_deref(), Some("c"));
        assert_eq!(window_id(&numbered, 4), None);
        assert_eq!(window_id(&numbered, 0), None);
    }

    #[test]
    fn part_targets_fit_in_one_line_when_short() {
        let channels = args(&["#rust", "#linux", "#bsd"]);
        assert_eq!(
            chunk_part_targets(&channels, "Window closed", None),
            vec!["#rust,#linux,#bsd".to_string()]
        );
    }

    #[test]
    fn part_targets_respect_the_line_limit() {
        let reason = "Window closed";
        let channels: Vec<String> = (0..60).map(|i| format!("#channel-number-{i:03}")).collect();
        let chunks = chunk_part_targets(&channels, reason, None);

        assert!(chunks.len() > 1, "60 long names must not fit one line");
        for chunk in &chunks {
            let line_len = "PART ".len() + chunk.len() + " :".len() + reason.len() + "\r\n".len();
            assert!(line_len <= crate::irc::PROTOCOL_LINE_MAX_BYTES, "{line_len}");
        }
        // Nothing is dropped and order is preserved.
        assert_eq!(chunks.join(",").split(',').count(), channels.len());
        assert_eq!(chunks.join(","), channels.join(","));
    }

    #[test]
    fn part_targets_respect_targmax() {
        let channels = args(&["#a", "#b", "#c", "#d", "#e"]);
        let chunks = chunk_part_targets(&channels, "bye", Some(2));
        assert_eq!(chunks, vec!["#a,#b", "#c,#d", "#e"]);
    }

    #[test]
    fn part_targets_never_drop_a_channel_when_the_reason_eats_the_budget() {
        let reason = "x".repeat(crate::irc::PROTOCOL_LINE_MAX_BYTES * 2);
        let channels = args(&["#a", "#b"]);
        let chunks = chunk_part_targets(&channels, &reason, None);
        assert_eq!(chunks, vec!["#a", "#b"]);
    }

    #[test]
    fn part_targets_of_nothing_send_nothing() {
        assert!(chunk_part_targets(&[], "bye", None).is_empty());
    }

    // === Range resolution ===
    //
    // `App::new` touches disk and has no test constructor, so the guards live
    // in `resolve_range`, which only needs an `AppState`.

    use crate::state::AppState;

    fn test_buffer(conn_id: &str, btype: BufferType, name: &str) -> Buffer {
        Buffer {
            id: make_buffer_id(conn_id, name),
            connection_id: conn_id.to_string(),
            buffer_type: btype,
            name: name.to_string(),
            messages: VecDeque::new(),
            activity: ActivityLevel::None,
            unread_count: 0,
            last_read: chrono::Utc::now(),
            topic: None,
            topic_set_by: None,
            users: HashMap::new(),
            modes: None,
            mode_params: None,
            list_modes: HashMap::new(),
            last_speakers: Vec::new(),
            peer_handle: None,
            log_total_lines: None,
            log_oldest_ts: None,
            log_newest_ts: None,
            history_exhausted: false,
            log_initial_loaded: false,
            pin_backlog: false,
        }
    }

    /// Windows 1..=5: Mentions, server, #a, #b, query.
    fn test_state() -> AppState {
        let mut state = AppState::new();
        state.add_buffer(test_buffer("", BufferType::Mentions, "Mentions"));
        state.add_buffer(test_buffer("libera", BufferType::Server, "libera"));
        state.add_buffer(test_buffer("libera", BufferType::Channel, "#a"));
        state.add_buffer(test_buffer("libera", BufferType::Channel, "#b"));
        state.add_buffer(test_buffer("libera", BufferType::Query, "someone"));
        state
    }

    fn names(state: &AppState, ids: &[String]) -> Vec<String> {
        ids.iter()
            .map(|id| state.buffers.get(id.as_str()).unwrap().name.clone())
            .collect()
    }

    #[test]
    fn range_resolves_to_the_windows_the_sidebar_shows() {
        let state = test_state();
        // Sanity-check the fixture numbering the rest of these tests rely on.
        assert_eq!(
            names(&state, &state.numbered_buffer_ids()),
            vec!["Mentions", "libera", "#a", "#b", "someone"]
        );

        let ids = resolve_range(&state, 3, 5, false).expect("channels and a query are closeable");
        assert_eq!(names(&state, &ids), vec!["#a", "#b", "someone"]);
    }

    #[test]
    fn range_stops_at_the_last_window_instead_of_erroring() {
        let state = test_state();
        let ids = resolve_range(&state, 4, 999, false).expect("clamps to what exists");
        assert_eq!(names(&state, &ids), vec!["#b", "someone"]);
    }

    #[test]
    fn range_entirely_past_the_end_is_an_error() {
        let state = test_state();
        let err = resolve_range(&state, 9, 12, true).expect_err("nothing to close");
        assert!(err.contains("No windows in range 9-12"), "{err}");
        assert!(err.contains("highest window is 5"), "{err}");
    }

    #[test]
    fn range_covering_mentions_is_refused_without_confirmation() {
        let state = test_state();
        let err = resolve_range(&state, 1, 5, false).expect_err("Mentions is protected");
        assert!(err.contains("Mentions window (1)"), "{err}");
        assert!(err.contains("/wc 1-5 -YES"), "{err}");
    }

    #[test]
    fn range_covering_mentions_still_refuses_for_the_server_window() {
        // -YES unlocks Mentions but never a server window: the range 1-5 holds
        // both, so it stays refused — now for the server, not for Mentions.
        let state = test_state();
        let err = resolve_range(&state, 1, 5, true).expect_err("server window blocks it");
        assert!(err.contains("server window 2"), "{err}");
    }

    #[test]
    fn range_of_only_mentions_is_unlocked_by_confirmation() {
        let state = test_state();
        let ids = resolve_range(&state, 1, 1, true).expect("-YES unlocks Mentions");
        assert_eq!(names(&state, &ids), vec!["Mentions"]);
    }

    #[test]
    fn range_covering_a_server_window_is_refused_and_names_it() {
        let state = test_state();
        let err = resolve_range(&state, 2, 4, false).expect_err("server window blocks it");
        assert!(err.contains("server window 2"), "{err}");
        assert!(err.contains("/wc 2"), "{err}");
        // Refusal is all-or-nothing: no partial close happened.
        assert_eq!(state.numbered_buffer_ids().len(), 5);
    }

    #[test]
    fn empty_window_list_refuses_every_range() {
        let state = AppState::new();
        let err = resolve_range(&state, 1, 5, true).expect_err("no windows at all");
        assert!(err.contains("highest window is 0"), "{err}");
    }
}
