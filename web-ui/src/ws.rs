use std::cell::RefCell;

use futures::{SinkExt, StreamExt, channel::mpsc, future};
use gloo_net::websocket::{Message, futures::WebSocket};
use leptos::prelude::*;

use crate::protocol::{WebCommand, WebEvent};
use crate::state::AppState;

// Module-level sender for commands to the WebSocket loop.
//
// Stored in a `thread_local` instead of Leptos context because
// `use_context` is unreliable inside Leptos 0.7 `Callback::run()`
// and `Effect` scopes — the owner chain doesn't always propagate.
// WASM is single-threaded, so `thread_local` is safe and always accessible.
thread_local! {
    static CMD_TX: RefCell<Option<mpsc::UnboundedSender<String>>> = const { RefCell::new(None) };
}

/// Connect to the WebSocket server and spawn the message loop.
///
/// Creates a fresh command channel on each call — safe to call
/// multiple times (e.g., after token expiry and re-login).
pub fn connect(state: &AppState) {
    let token = state.token.get_untracked();
    let Some(token) = token else { return };

    // Fresh command channel for this connection.
    let (cmd_tx, cmd_rx) = mpsc::unbounded::<String>();
    CMD_TX.with(|cell| *cell.borrow_mut() = Some(cmd_tx));

    let Some(window) = web_sys::window() else {
        web_sys::console::warn_1(&"no window object".into());
        return;
    };
    let location = window.location();
    let host = location.host().unwrap_or_default();
    let url = format!("wss://{host}/ws?token={token}");

    let state = state.clone();

    leptos::task::spawn_local(async move {
        match WebSocket::open(&url) {
            Ok(ws) => {
                state.error.set(None);
                // Don't set connected=true yet — wait for SyncInit.
                run_ws_loop(ws, &state, cmd_rx).await;
                let was_connected = state.connected.get_untracked();
                state.connected.set(false);
                if !was_connected {
                    // Never received SyncInit → server rejected (401/expired token).
                    state.token.set(None);
                }
            }
            Err(e) => {
                let msg = format!("{e}");
                state.token.set(None);
                state.error.set(Some(format!("WebSocket error: {msg}")));
                state.connected.set(false);
            }
        }
    });
}

/// Send a WebCommand to the server via the global command channel.
pub fn send_command(cmd: &WebCommand) {
    CMD_TX.with(|cell| {
        let tx = cell.borrow();
        let Some(tx) = tx.as_ref() else {
            web_sys::console::warn_1(&"command sender not initialized".into());
            return;
        };
        match serde_json::to_string(cmd) {
            Ok(json) => {
                if let Err(e) = tx.unbounded_send(json) {
                    web_sys::console::warn_1(&format!("cmd send failed: {e}").into());
                }
            }
            Err(e) => {
                web_sys::console::warn_1(&format!("cmd serialize failed: {e}").into());
            }
        }
    });
}

/// Main WebSocket event loop — polls commands and server messages concurrently.
async fn run_ws_loop(
    ws: WebSocket,
    state: &AppState,
    mut cmd_rx: mpsc::UnboundedReceiver<String>,
) {
    let (mut ws_tx, mut ws_rx) = ws.split();

    loop {
        let cmd_next = cmd_rx.next();
        let ws_next = ws_rx.next();
        futures::pin_mut!(cmd_next, ws_next);

        match future::select(cmd_next, ws_next).await {
            // Outgoing command from UI.
            future::Either::Left((Some(json), _)) => {
                if let Err(e) = ws_tx.send(Message::Text(json)).await {
                    web_sys::console::warn_1(&format!("ws send failed: {e}").into());
                    break;
                }
            }
            // Command channel closed.
            future::Either::Left((None, _)) => break,

            // Incoming server message.
            future::Either::Right((Some(Ok(Message::Text(text))), _)) => {
                match serde_json::from_str::<WebEvent>(&text) {
                    Ok(event) => state.handle_event(event),
                    Err(e) => {
                        web_sys::console::warn_1(&format!("invalid WebEvent: {e}").into());
                    }
                }
            }
            future::Either::Right((Some(Ok(Message::Bytes(_))), _)) => {}
            future::Either::Right((Some(Err(e)), _)) => {
                state.error.set(Some(format!("WebSocket error: {e}")));
                break;
            }
            // WebSocket closed.
            future::Either::Right((None, _)) => break,
        }
    }
}
