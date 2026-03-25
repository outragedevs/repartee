# Split app.rs into domain submodules

**Date**: 2026-03-25
**Branch**: `refactor/split-app-rs`
**Type**: Pure file reorganization (Approach A of hybrid plan C)

## Problem

`src/app.rs` is 6,767 lines with 80+ methods spanning 12+ responsibility domains. This violates Rust best practices (Chapter 1.6: break up long functions/files) and makes the code harder to navigate, review, and maintain.

## Solution

Convert `src/app.rs` into `src/app/mod.rs` + 12 domain files. Each file contains an `impl App` block for one responsibility domain. Zero behavioral changes.

## File map

| File | Responsibility | Key methods |
|------|---------------|-------------|
| `mod.rs` | Struct definition, constructor, main event loop | `App` struct, `new()`, `run()`, `run_splash()`, `create_default_status`, `ensure_default_status`, `start_term_reader`, `stop_term_reader`, `recompute_wrap_indent` |
| `irc.rs` | IRC event handling, connections, reconnect | `handle_irc_event`, `setup_connection`, `connect_server_async`, `check_reconnects`, `spawn_reconnect`, `execute_autosendcmd`, `add_event_to_buffer` |
| `input.rs` | Keyboard, mouse, paste handling | `handle_event`, `handle_key`, `handle_paste`, `drain_paste_queue`, `handle_mouse`, `handle_buffer_list_click`, `handle_nick_list_click`, `consume_esc_prefix`, `switch_to_buffer_num`, `reset_sidepanel_scrolls`, `forward_key_to_shell`, `forward_mouse_to_shell`, `update_shell_input_state` |
| `web.rs` | Web frontend commands and events | `broadcast_web`, `start_web_server`, `stop_web_server`, `drain_pending_web_events`, `record_mention`, `handle_web_command`, `web_*` methods |
| `shell.rs` | Shell/PTY session management | `handle_shell_event`, `close_shell_buffer`, `ensure_shell_connection`, `maybe_remove_shell_connection`, `resize_all_shells`, `*broadcast_shell_screen*` methods |
| `dcc.rs` | DCC CHAT event handling | `handle_dcc_event` + helpers |
| `session.rs` | Detach/reattach, Unix socket, shim | `start_socket_listener`, `remove_own_socket`, `handle_shim_connect`, `send_shim_control`, `teardown_shim`, `disconnect_shim`, `perform_detach`, `notify_shim_quit` |
| `image.rs` | Image preview rendering | `show_image_preview`, `refresh_image_protocol`, `dismiss_image_preview`, `image_preview_popup_rect`, `cleanup_image_graphics`, `clear_direct_image_area`, `write_tmux_direct_image`, `handle_preview_event` + free functions |
| `who.rs` | WHO query batching | `queue_channel_query`, `send_channel_query_batch`, `handle_who_batch_complete`, `check_stale_who_batches` |
| `maintenance.rs` | Periodic tick tasks | `handle_netsplit_tick`, `purge_expired_batches`, `maybe_purge_old_events`, `maybe_purge_old_mentions`, `measure_lag` |
| `mentions.rs` | Mentions buffer loading | `load_mentions_history`, `mention_row_to_message` |
| `backlog.rs` | Chat backlog loading from DB | `load_backlog` |
| `scripting.rs` | Script engine wiring, actions, API | `handle_script_action`, `update_script_snapshot`, `build_script_api` + script API closure wiring |

## Rules

1. **No behavioral changes** -- pure method relocation
2. **Visibility**: Private `App` fields accessed from submodules become `pub(crate)`
3. **Imports**: Each file gets its own `use` declarations
4. **Re-exports**: `mod.rs` re-exports `App` so `crate::app::App` still works
5. **Free functions**: Move to their domain file (e.g., image direct-write helpers to `image.rs`)
6. **One commit per extracted file** for reviewability

## External interface

Unchanged. All of these remain valid:
- `crate::app::App`
- `fn cmd_*(app: &mut App, args: &[String])`
- `app::App::new()` / `app::App::remove_own_socket()`

## Out of scope

- Extracting subsystem structs (Approach B) -- future work
- Reducing `App` field count -- future work
- Splitting `irc/events.rs` (5,263 lines, similar problem) -- separate task
