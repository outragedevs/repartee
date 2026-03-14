use futures::{SinkExt, StreamExt, channel::mpsc};
use gloo_net::websocket::{Message, futures::WebSocket};
use leptos::prelude::*;

use crate::protocol::{WebCommand, WebEvent};
use crate::state::AppState;

/// Channel sender for components to send commands to the WebSocket loop.
/// Stored in Leptos context — `Send + Sync` because `futures::mpsc` sender is.
#[derive(Clone)]
pub struct CommandSender(pub mpsc::UnboundedSender<String>);

/// Connect to the WebSocket server and spawn the message loop.
pub fn connect(state: &AppState, cmd_rx: mpsc::UnboundedReceiver<String>) {
    let token = state.token.get_untracked();
    let Some(token) = token else { return };

    let location = web_sys::window().unwrap().location();
    let host = location.host().unwrap();
    let url = format!("wss://{host}/ws?token={token}");

    let state = state.clone();

    leptos::task::spawn_local(async move {
        match WebSocket::open(&url) {
            Ok(ws) => {
                state.error.set(None);
                state.connected.set(true);
                run_ws_loop(ws, &state, cmd_rx).await;
                state.connected.set(false);
            }
            Err(e) => {
                state.error.set(Some(format!("WebSocket error: {e}")));
                state.connected.set(false);
            }
        }
    });
}

/// Send a WebCommand to the server via the command channel.
pub fn send_command(cmd: &WebCommand) {
    let Some(sender) = use_context::<CommandSender>() else {
        web_sys::console::warn_1(&"no CommandSender context".into());
        return;
    };

    match serde_json::to_string(cmd) {
        Ok(json) => {
            if let Err(e) = sender.0.unbounded_send(json) {
                web_sys::console::warn_1(&format!("cmd send failed: {e}").into());
            }
        }
        Err(e) => {
            web_sys::console::warn_1(&format!("cmd serialize failed: {e}").into());
        }
    }
}

async fn run_ws_loop(
    ws: WebSocket,
    state: &AppState,
    mut cmd_rx: mpsc::UnboundedReceiver<String>,
) {
    let (mut ws_tx, mut ws_rx) = ws.split();

    // WASM is single-threaded — poll both streams in a manual loop.
    loop {
        // Drain all pending commands first (non-blocking).
        loop {
            match cmd_rx.try_recv() {
                Ok(Some(json)) => {
                    if let Err(e) = ws_tx.send(Message::Text(json)).await {
                        web_sys::console::warn_1(&format!("ws send failed: {e}").into());
                        return;
                    }
                }
                _ => break,
            }
        }

        // Wait for next server message.
        match ws_rx.next().await {
            Some(Ok(Message::Text(text))) => {
                match serde_json::from_str::<WebEvent>(&text) {
                    Ok(event) => state.handle_event(event),
                    Err(e) => {
                        web_sys::console::warn_1(&format!("invalid WebEvent: {e}").into());
                    }
                }
            }
            Some(Ok(Message::Bytes(_))) => {}
            Some(Err(e)) => {
                state.error.set(Some(format!("WebSocket error: {e}")));
                break;
            }
            None => break,
        }
    }
}
