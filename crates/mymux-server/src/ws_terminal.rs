//! WebSocket terminal endpoint. Cookie-authenticated (via the AuthUser
//! extractor), owner/admin-checked, Origin- and CIDR-validated. Sends the
//! replay buffer first, then live output; accepts input/resize from the client.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, Path, State};
use axum::http::HeaderMap;
use axum::http::header::ORIGIN;
use axum::response::Response;
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use tokio::sync::broadcast::error::RecvError;

use crate::audit;
use crate::auth::AuthUser;
use crate::error::AppError;
use crate::state::AppState;
use crate::terminal::{PtyEvent, TerminalSession};
use crate::util;

pub async fn ws_terminal(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Path(id): Path<String>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    user: AuthUser,
) -> Result<Response, AppError> {
    let ip = util::resolve_client_ip(&headers, peer.ip(), &state.config.network);
    if !util::cidr_allowed(ip, &state.config.network.allowed_client_cidrs) {
        return Err(AppError::Forbidden);
    }

    // Same-origin check (only when an Origin header and a public_url are present).
    if let Some(origin) = headers.get(ORIGIN).and_then(|v| v.to_str().ok()) {
        let public_url = &state.config.server.public_url;
        if !public_url.is_empty() && url_origin(origin) != url_origin(public_url) {
            return Err(AppError::Forbidden);
        }
    }

    let session = state.terminals.get(&id).ok_or(AppError::NotFound)?;
    if session.owner_user_id != user.id && !user.role.is_admin() {
        audit::record(
            &state,
            Some(&user),
            Some(&ip.to_string()),
            "terminal.denied",
            Some(&id),
            json!({"reason":"attach_not_owner"}),
            "denied",
        )
        .await;
        return Err(AppError::Forbidden);
    }

    audit::record(
        &state,
        Some(&user),
        Some(&ip.to_string()),
        "terminal.attach",
        Some(&id),
        json!({}),
        "ok",
    )
    .await;

    let state2 = state.clone();
    let detach_meta = json!({ "user": user.username, "user_id": user.id });
    Ok(ws.on_upgrade(move |socket| handle_socket(socket, session, state2, id, detach_meta)))
}

async fn handle_socket(
    socket: WebSocket,
    session: Arc<TerminalSession>,
    state: AppState,
    term_id: String,
    detach_meta: serde_json::Value,
) {
    let (mut sender, mut receiver) = socket.split();

    // Replay buffer first so a reconnecting client sees recent scrollback.
    let replay = session.replay_snapshot();
    if !replay.is_empty() {
        let frame = json!({ "type": "output", "data": util::b64_std(&replay) }).to_string();
        let _ = sender.send(Message::Text(frame)).await;
    }

    let mut rx = session.output_tx.subscribe();
    let mut send_task = tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(PtyEvent::Output(bytes)) => {
                    let frame =
                        json!({ "type": "output", "data": util::b64_std(&bytes) }).to_string();
                    if sender.send(Message::Text(frame)).await.is_err() {
                        break;
                    }
                }
                Ok(PtyEvent::Exit) => {
                    let _ = sender
                        .send(Message::Text(
                            json!({ "type": "exit", "code": null }).to_string(),
                        ))
                        .await;
                    break;
                }
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => break,
            }
        }
    });

    let in_session = Arc::clone(&session);
    let mut recv_task = tokio::spawn(async move {
        while let Some(Ok(msg)) = receiver.next().await {
            match msg {
                Message::Text(t) => {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&t) {
                        match v.get("type").and_then(|x| x.as_str()) {
                            Some("input") => {
                                if let Some(d) = v.get("data").and_then(|x| x.as_str()) {
                                    let _ = in_session.write_input(d.as_bytes());
                                }
                            }
                            Some("resize") => {
                                let cols =
                                    v.get("cols").and_then(|x| x.as_u64()).unwrap_or(80) as u16;
                                let rows =
                                    v.get("rows").and_then(|x| x.as_u64()).unwrap_or(24) as u16;
                                let _ = in_session.resize(cols, rows);
                            }
                            _ => {}
                        }
                    }
                }
                Message::Binary(b) => {
                    let _ = in_session.write_input(&b);
                }
                Message::Close(_) => break,
                _ => {}
            }
        }
    });

    // When either direction ends, tear down the other.
    tokio::select! {
        _ = &mut send_task => { recv_task.abort(); }
        _ = &mut recv_task => { send_task.abort(); }
    }

    audit::record(
        &state,
        None,
        None,
        "terminal.detach",
        Some(&term_id),
        detach_meta,
        "ok",
    )
    .await;
}

/// Reduce a URL to its origin (scheme://authority), dropping any path.
fn url_origin(u: &str) -> String {
    if let Some(pos) = u.find("://") {
        let after = &u[pos + 3..];
        let end = after.find('/').unwrap_or(after.len());
        format!("{}://{}", &u[..pos], &after[..end])
    } else {
        u.trim_end_matches('/').to_string()
    }
}
