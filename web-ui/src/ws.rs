use std::cell::RefCell;
use std::rc::Rc;

use gloo_net::websocket::{Message, futures::WebSocket};
use futures::{SinkExt, StreamExt};
use leptos::prelude::*;

use crate::protocol::{WebCommand, WebEvent};
use crate::state::AppState;

/// Shared WebSocket sender, accessible from any component via Leptos context.
/// Uses `Rc<RefCell>` because WASM is single-threaded.
#[derive(Clone)]
pub struct WsSender(pub Rc<RefCell<Option<futures::stream::SplitSink<WebSocket, Message>>>>);

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
                state.error.set(None);
                let (tx, mut rx) = ws.split();

                // Store the sender so components can use it.
                if let Some(ws_sender) = use_context::<WsSender>() {
                    *ws_sender.0.borrow_mut() = Some(tx);
                }

                state.connected.set(true);

                // Read loop — dispatch incoming events to state.
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
                        Ok(Message::Bytes(_)) => {}
                        Err(e) => {
                            state.error.set(Some(format!("WebSocket error: {e}")));
                            break;
                        }
                    }
                }

                // Connection closed — clear sender.
                if let Some(ws_sender) = use_context::<WsSender>() {
                    *ws_sender.0.borrow_mut() = None;
                }
                state.connected.set(false);
            }
            Err(e) => {
                state.error.set(Some(format!("WebSocket error: {e}")));
                state.connected.set(false);
            }
        }
    });
}

/// Send a WebCommand to the server via the shared WebSocket sender.
pub fn send_command(cmd: &WebCommand) {
    let Some(ws_sender) = use_context::<WsSender>() else {
        web_sys::console::warn_1(&"no WsSender context".into());
        return;
    };

    let json = match serde_json::to_string(cmd) {
        Ok(j) => j,
        Err(e) => {
            web_sys::console::warn_1(&format!("failed to serialize command: {e}").into());
            return;
        }
    };

    // Take the sender, send, put it back.
    let mut borrow = ws_sender.0.borrow_mut();
    if let Some(ref mut tx) = *borrow {
        // We need to spawn because SinkExt::send is async.
        // Clone the Rc so the spawn can access it.
        let sender_rc = ws_sender.0.clone();
        // Take the sender out temporarily.
        let mut taken_tx = borrow.take().unwrap();
        drop(borrow); // release the RefCell borrow before spawning

        leptos::task::spawn_local(async move {
            if let Err(e) = taken_tx.send(Message::Text(json)).await {
                web_sys::console::warn_1(&format!("ws send failed: {e}").into());
            }
            // Put the sender back.
            *sender_rc.borrow_mut() = Some(taken_tx);
        });
    }
}
