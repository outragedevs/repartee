use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use futures::{SinkExt, StreamExt};
use tokio::time::{Duration, interval};

use super::protocol::{WebCommand, WebEvent};
use super::server::AppHandle;

/// Query parameter for WebSocket auth.
#[derive(serde::Deserialize)]
pub struct WsQuery {
    token: String,
}

/// GET /ws?token=xxx — WebSocket upgrade with token authentication.
pub async fn ws_handler(
    Query(query): Query<WsQuery>,
    State(state): State<Arc<AppHandle>>,
    ws: axum::extract::WebSocketUpgrade,
) -> impl IntoResponse {
    // Validate session token.
    let valid = state
        .session_store
        .lock()
        .await
        .validate(&query.token)
        .is_some();
    if !valid {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let session_id = uuid::Uuid::new_v4().to_string();
    tracing::info!(session_id = %session_id, "web client connecting");

    ws.on_upgrade(move |socket| async move {
        handle_socket(socket, state, session_id).await;
    })
    .into_response()
}

/// Per-session WebSocket loop.
///
/// - Sends `SyncInit` on connect
/// - Forwards broadcast events to client as JSON
/// - Parses incoming JSON as `WebCommand`, sends through `web_cmd_tx`
/// - Ping/pong heartbeat (30s interval)
async fn handle_socket(
    socket: axum::extract::ws::WebSocket,
    state: Arc<AppHandle>,
    session_id: String,
) {
    let (mut ws_tx, mut ws_rx) = socket.split();
    let mut broadcast_rx = state.broadcaster.subscribe();

    // Send SyncInit.
    let sync_init = build_sync_init_from_snapshot(&state);
    tracing::info!(session_id = %session_id, "sending SyncInit");
    if send_json(&mut ws_tx, &sync_init).await.is_err() {
        tracing::warn!(session_id = %session_id, "failed to send SyncInit");
        return;
    }
    tracing::info!(session_id = %session_id, "SyncInit sent, entering event loop");

    let mut ping_interval = interval(Duration::from_secs(30));
    ping_interval.tick().await; // skip immediate first tick

    loop {
        tokio::select! {
            // Broadcast events from the server.
            event = broadcast_rx.recv() => {
                match event {
                    Ok(web_event) => {
                        // Filter session-targeted events.
                        if is_targeted_to_other(&web_event, &session_id) {
                            continue;
                        }
                        if send_json(&mut ws_tx, &web_event).await.is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(session_id = %session_id, lagged = n, "web client lagged — sending resync");
                        // Re-send SyncInit so the client can recover missed state.
                        let resync = build_sync_init_from_snapshot(&state);
                        if send_json(&mut ws_tx, &resync).await.is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }

            // Client messages.
            msg = ws_rx.next() => {
                match msg {
                    Some(Ok(axum::extract::ws::Message::Text(text))) => {
                        match serde_json::from_str::<WebCommand>(&text) {
                            Ok(cmd) => {
                                if state.web_cmd_tx.send((cmd, session_id.clone())).await.is_err() {
                                    tracing::warn!(session_id = %session_id, "web_cmd channel closed");
                                    break;
                                }
                            }
                            Err(e) => {
                                tracing::debug!(session_id = %session_id, error = %e, "invalid web command");
                                let err = WebEvent::Error { message: format!("invalid command: {e}") };
                                let _ = send_json(&mut ws_tx, &err).await;
                            }
                        }
                    }
                    Some(Ok(axum::extract::ws::Message::Close(_))) | None => break,
                    Some(Err(e)) => {
                        tracing::debug!(session_id = %session_id, error = %e, "ws recv error");
                        break;
                    }
                    _ => {} // Pong, Binary, Ping — no action needed
                }
            }

            // Heartbeat ping.
            _ = ping_interval.tick() => {
                if ws_tx.send(axum::extract::ws::Message::Ping(vec![].into())).await.is_err() {
                    break;
                }
            }
        }
    }

    // Clean up web-specific shell PTY.
    let _ = state
        .web_cmd_tx
        .send((
            crate::web::protocol::WebCommand::WebDisconnect,
            session_id.clone(),
        ))
        .await;

    tracing::info!(session_id = %session_id, "web client disconnected");
}

/// Build a `SyncInit` from the shared state snapshot.
fn build_sync_init_from_snapshot(state: &AppHandle) -> WebEvent {
    if let Some(ref snapshot) = state.web_state_snapshot
        && let Ok(snap) = snapshot.read()
    {
        return WebEvent::SyncInit {
            buffers: snap.buffers.clone(),
            connections: snap.connections.clone(),
            mention_count: snap.mention_count,
            active_buffer_id: snap.active_buffer_id.clone(),
            timestamp_format: snap.timestamp_format.clone(),
        };
    }
    // Fallback: empty init.
    WebEvent::SyncInit {
        buffers: Vec::new(),
        connections: Vec::new(),
        mention_count: 0,
        active_buffer_id: None,
        timestamp_format: crate::config::WebConfig::default().timestamp_format,
    }
}

/// Check if a `WebEvent` is targeted to a different session.
///
/// Returns `true` if the event has a `session_id` field that doesn't match
/// the current session — meaning this client should NOT receive it.
fn is_targeted_to_other(event: &WebEvent, session_id: &str) -> bool {
    let target = match event {
        WebEvent::Messages { session_id, .. }
        | WebEvent::NickList { session_id, .. }
        | WebEvent::MentionsList { session_id, .. } => session_id.as_deref(),
        _ => None,
    };
    // If target is Some and doesn't match, skip this event.
    target.is_some_and(|t| t != session_id)
}

/// Send a `WebEvent` as JSON text through the WebSocket.
async fn send_json(
    tx: &mut futures::stream::SplitSink<axum::extract::ws::WebSocket, axum::extract::ws::Message>,
    event: &WebEvent,
) -> Result<(), ()> {
    match serde_json::to_string(event) {
        Ok(json) => tx
            .send(axum::extract::ws::Message::Text(json.into()))
            .await
            .map_err(|_| ()),
        Err(e) => {
            tracing::warn!("failed to serialize WebEvent: {e}");
            Err(())
        }
    }
}
