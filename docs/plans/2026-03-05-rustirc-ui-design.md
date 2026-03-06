# repartee UI & Architecture Design

**Date:** 2026-03-05
**Status:** Approved

## Overview

Rust IRC client — port of kokoirc (TypeScript/OpenTUI/Bun) to Rust/ratatui/tokio.
The app name is not finalized — all references use an `APP_NAME` constant.

## Tech Stack

- **TUI**: ratatui 0.30 + crossterm 0.29 (event-stream)
- **Async**: tokio 1.50
- **IRC**: `irc` crate 1.1
- **Config**: TOML (same format as kokoirc), credentials in `.env`
- **Theming**: TOML `.theme` files with irssi-compatible format strings
- **Error handling**: color-eyre + thiserror

## Project Structure

```
src/
  main.rs                    — entry point, terminal setup, panic hook
  app.rs                     — App struct: owns state + event loop (TEA pattern)
  constants.rs               — APP_NAME, version, paths

  state/                     — UI-AGNOSTIC app state (future: shared with web frontend)
    mod.rs                   — AppState struct
    buffer.rs                — Buffer, Message, NickEntry, ActivityLevel
    connection.rs            — Connection, ConnectionStatus
    sorting.rs               — buffer + nick sort logic
    events.rs                — state mutation methods

  config/
    mod.rs                   — AppConfig types + TOML loading
    defaults.rs              — DEFAULT_CONFIG
    env.rs                   — .env credential loading

  theme/
    mod.rs                   — ThemeFile, ThemeColors, StyledSpan types
    loader.rs                — TOML theme loading + defaults
    parser.rs                — format string engine

  irc/
    mod.rs                   — connection manager
    events.rs                — IRC event → state mutations
    formatting.rs            — strip formatting, prefix/mode helpers, timestamps
    flood.rs                 — antiflood detection
    netsplit.rs              — netsplit detection + batching
    ignore.rs                — ignore list matching

  commands/
    mod.rs                   — command registry + dispatch
    parser.rs                — /command parsing
    registry.rs              — all command definitions
    helpers.rs               — utility functions

  ui/                        — ratatui TUI frontend
    mod.rs                   — draw() dispatch + terminal management
    layout.rs                — AppLayout
    topic_bar.rs             — 1-line topic display
    chat_view.rs             — scrollable message area
    message_line.rs          — single message rendering
    buffer_list.rs           — left sidebar
    nick_list.rs             — right sidebar
    status_line.rs           — configurable status bar
    input.rs                 — command input + tab completion + history
    styled_text.rs           — StyledSpan[] → ratatui Spans converter

  scripting/                 — plugin/scripting API (future)
    mod.rs                   — ScriptManager placeholder
    api.rs                   — ScriptAPI trait definition
    event_bus.rs             — EventBus for script hooks

  image_preview/             — image preview system (future)
    mod.rs                   — placeholder
```

## Theming System

### Theme File Format

TOML with sections: `[meta]`, `[colors]`, `[abstracts]`, `[formats.messages]`,
`[formats.events]`, `[formats.sidepanel]`, `[formats.nicklist]`.

Compatible with kokoirc's `default.theme`.

### Format String Engine (3 stages)

1. **resolve_abstractions(format, abstracts)** — expands `{name args}` recursively (max depth 10)
2. **substitute_vars(input, params)** — `$0`-`$9`, `$*`, `$[N]0` padding
3. **parse_format_string(input)** — char-by-char walk producing `Vec<StyledSpan>`

Supported codes:
- `%Z` RRGGBB — 24-bit foreground
- `%z` RRGGBB — 24-bit background
- `%k`-`%w` / `%K`-`%W` — irssi 16-color
- `%_` bold, `%u` underline, `%i` italic, `%d` dim (toggles)
- `%N`/`%n` reset, `%|` indent (skip), `%%` literal
- mIRC: `\x02` bold, `\x03[N[,N]]` color, `\x04[HEX[,HEX]]` hex,
  `\x0F` reset, `\x16` reverse, `\x1D` italic, `\x1E` dim, `\x1F` underline

## State Model

UI-agnostic `AppState` containing:
- `connections: HashMap<String, Connection>`
- `buffers: IndexMap<String, Buffer>` (ordered)
- `active_buffer_id: Option<String>`
- `config: AppConfig`
- `theme: ThemeFile`

Buffer IDs: `"connectionId/name.lowercase"`.

### Activity Levels
None=0, Events=1, Highlight=2, Activity=3, Mention=4. Only escalates.
Reset on buffer activation.

### Sorting
- Buffers: connection label → sort group (Server/Channel/Query/Special) → name alpha
- Nicks: prefix rank (ISUPPORT PREFIX) → nick alpha case-insensitive

## UI Layout

```
┌──────────────────────────────────────────────────────────┐
│ TopicBar (height=1, bg_alt)  channel — topic text        │
├────────────┬──────────────────────────┬──────────────────┤
│ BufferList │      ChatView            │  NickList        │
│ left panel │   (scrollable, Fill)     │  right panel     │
│ width=N    │                          │  (channels only) │
├────────────┴──────────────────────────┴──────────────────┤
│ StatusLine (height=1)  [time | nick | channel | lag | …] │
│ CommandInput (height=1)  [$server❯ ][input____________]  │
└──────────────────────────────────────────────────────────┘
```

### Components

- **TopicBar**: Paragraph, parsed IRC colors in topic
- **BufferList**: List + ListState, themed per activity level
- **ChatView**: Custom scrollable, sticky-to-bottom, scrollback limit
- **NickList**: List + ListState, sorted by prefix rank
- **StatusLine**: Styled Spans, 1s tick for clock
- **CommandInput**: Custom widget, cursor tracking, prompt template

### Scrolling
- `offset` from bottom, `sticky` flag
- PageUp/Down adjusts offset, End snaps to bottom
- New own message snaps to bottom

### Keybindings

| Key | Action |
|-----|--------|
| Ctrl+Q | Quit |
| Esc+1..9, Esc+0 | Switch to buffer 1-10 |
| Alt+1..9 | Switch to buffer 1-9 |
| Esc+Left/Right | Prev/next buffer |
| Tab | Tab completion (nick/command/subcommand) |
| Up/Down | History navigation |
| PageUp/PageDown | Scroll chat |
| End | Snap to bottom |
| Enter | Submit input |

### Mouse Support

| Action | Behavior |
|--------|----------|
| Click buffer list item | Switch buffer |
| Click nick list entry | Open/switch to query |
| Click status bar activity | Switch to buffer |
| Click message URL | Image preview (future) |
| Scroll wheel ChatView | Scroll, disables sticky |
| Scroll wheel BufferList | Scroll buffer list |
| Scroll wheel NickList | Scroll nick list |
| Drag panel border | Resize sidepanel (persists to config) |

## Event Loop

```
tokio::select! {
    terminal event → handle_terminal_event (keys, mouse, resize)
    irc message    → handle_irc_message (via mpsc from connection tasks)
    1s tick        → handle_tick (clock, lag pings, netsplit flush)
}
```

Each IRC connection is a spawned tokio task sending messages via mpsc channel.

## Future Provisions

- **Scripting**: EventBus trait + ScriptAPI trait stubbed in `scripting/`
- **Image Preview**: Module stubbed in `image_preview/`
- **Web Frontend**: State is UI-agnostic; can be wrapped in Arc<RwLock<>> and exposed via WebSocket

## Dependencies (all latest as of 2026-03-05)

```toml
ratatui = "0.30"
crossterm = { version = "0.29", features = ["event-stream"] }
tokio = { version = "1.50", features = ["full"] }
irc = "1.1"
color-eyre = "0.6"
serde = { version = "1.0", features = ["derive"] }
toml = "1.0"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
futures = "0.3"
chrono = { version = "0.4", features = ["serde"] }
dirs = "6"
unicode-width = "0.2"
thiserror = "2.0"
indexmap = { version = "2", features = ["serde"] }
```
