use gloo_net::websocket::{Message, futures::WebSocket};
use futures::StreamExt;
use leptos::prelude::*;

use crate::protocol::{WebCommand, WebEvent};
use crate::state::AppState;

/// Connect to the WebSocket server and spawn the message loop.
pub fn connect(state: &AppState) {
    let token = state.token.get_untracked();
    let Some(token) = token else { return };

    let location = web_sys::window().unwrap().location();
    let host = location.host().unwrap();
    let url = format!("wss://{host}/ws?token={token}");

    let state = state.clone();

    leptos::task::spawn_local(async move {
        match WebSocket::open(&url) {
            Ok(ws) => {
                state.connected.set(true);
                state.error.set(None);
                run_ws_loop(ws, &state).await;
                state.connected.set(false);
            }
            Err(e) => {
                state.error.set(Some(format!("WebSocket error: {e}")));
                state.connected.set(false);
            }
        }
    });
}

/// Send a WebCommand to the server.
pub fn send_command(cmd: &WebCommand) {
    // This will be called via a stored sender. For now, log.
    web_sys::console::log_1(&format!("send_command: {cmd:?}").into());
}

async fn run_ws_loop(ws: WebSocket, state: &AppState) {
    let (_tx, mut rx) = ws.split();

    while let Some(msg) = rx.next().await {
        match msg {
            Ok(Message::Text(text)) => {
                match serde_json::from_str::<WebEvent>(&text) {
                    Ok(event) => state.handle_event(event),
                    Err(e) => {
                        web_sys::console::warn_1(
                            &format!("invalid WebEvent: {e}").into(),
                        );
                    }
                }
            }
            Ok(Message::Bytes(_)) => {} // ignore binary
            Err(e) => {
                state.error.set(Some(format!("WebSocket error: {e}")));
                break;
            }
        }
    }
}
