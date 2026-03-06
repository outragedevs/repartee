# Architecture

Technical overview of repartee's internal design.

## Technology stack

| Component | Technology |
|---|---|
| Language | Rust 2024 edition |
| TUI framework | ratatui 0.30+ with crossterm backend |
| Async runtime | tokio (full features) |
| IRC protocol | `irc` crate v1.1.0 |
| Scripting | Lua 5.4 via mlua 0.11 |
| Storage | SQLite via rusqlite (bundled) |
| Encryption | AES-256-GCM via aes-gcm |
| Error handling | color-eyre + thiserror |
| Config | TOML via serde |

## TEA architecture

repartee follows The Elm Architecture (TEA) pattern:

```
Model → Message → Update → View
```

- **Model**: `AppState` — all application state (buffers, connections, config)
- **Message**: Events from IRC, keyboard, mouse, timers
- **Update**: Event handlers that transform state
- **View**: ratatui rendering functions that read state

State is UI-agnostic — the `state/` module has no ratatui imports.

## Module structure

```
src/
  main.rs              # Entry point, terminal setup
  app.rs               # Main event loop, App struct
  constants.rs         # APP_NAME and global constants
  config/              # TOML config loading
  state/               # Application state (buffers, connections, sorting)
  irc/                 # IRC connection, event handling, formatting
    events.rs          # IRC message → state update
    flood.rs           # Flood protection
    netsplit.rs        # Netsplit detection
    ignore.rs          # Ignore list matching
    formatting.rs      # IRC formatting helpers
  ui/                  # TUI rendering
    layout.rs          # Screen layout + regions
    buffer_list.rs     # Left sidebar
    nick_list.rs       # Right sidebar
    chat_view.rs       # Message display
    input.rs           # Command input + tab completion
    status_line.rs     # Bottom status bar
    topic_bar.rs       # Top topic display
    message_line.rs    # Single message rendering
    styled_text.rs     # Format string → ratatui spans
  theme/               # Theme loading + format string parser
  scripting/           # Lua scripting engine
    engine.rs          # ScriptEngine trait + ScriptManager
    api.rs             # Event names
    event_bus.rs       # Priority-ordered event dispatch
    lua/               # Lua 5.4 backend (mlua)
  storage/             # SQLite logging
    db.rs              # Database schema + migrations
    writer.rs          # Batched async writer
    query.rs           # Search + read queries
    crypto.rs          # AES-256-GCM encryption
    types.rs           # LogRow, StoredMessage
  commands/            # Command system
    parser.rs          # /command arg parsing
    registry.rs        # Command registration
    handlers_irc.rs    # IRC commands (/join, /msg, etc.)
    handlers_ui.rs     # UI commands (/clear, /close, etc.)
    handlers_admin.rs  # Admin commands (/set, /reload, etc.)
    docs.rs            # Command documentation loader
```

## Event flow

1. **Terminal events** (keyboard, mouse) arrive via crossterm `event::poll` in a `spawn_blocking` thread
2. Events are sent through a tokio mpsc channel to the main loop
3. **IRC events** arrive via the `irc` crate's async reader, converted to `IrcEvent` enum variants
4. The main loop in `App::run()` processes all events sequentially, updating `AppState`
5. After each event batch, the UI is re-rendered from the current state

## IRC connection layer

Each server connection spawns:
- An async reader task that receives IRC messages and sends `IrcEvent` to a shared mpsc channel
- Messages are sent through the `irc` crate's `Sender` stored in the connection state

All connections share a single event channel, with events tagged by `connection_id`.

## Scripting architecture

```
ScriptManager
  └── Vec<Box<dyn ScriptEngine>>
        └── LuaEngine (mlua)
              ├── Per-script Lua environments (sandboxed)
              ├── Event handlers (priority-sorted, per-event)
              └── Command handlers
```

The `ScriptEngine` trait allows adding new languages (Rhai, etc.) by implementing the trait and registering with `ScriptManager`.

## Storage pipeline

```
AppState::add_message()
  → log_tx (tokio mpsc sender)
  → WriterTask (batches 50 rows / 1 second)
  → SQLite (WAL mode, FTS5 index)
```

Messages flow from the UI thread to the writer task asynchronously, ensuring zero UI blocking on disk I/O.
