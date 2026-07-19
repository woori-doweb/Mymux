//! Router wiring + the terminal HTTP handlers (create / list / close) and health.

use std::net::SocketAddr;

use axum::extract::{ConnectInfo, Path, State};
use axum::http::{HeaderMap, HeaderValue, header};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;
use tower_http::services::ServeDir;
use tower_http::set_header::SetResponseHeaderLayer;
use tower_http::trace::TraceLayer;

use crate::agent_status;
use crate::audit;
use crate::auth::{self, AuthUser};
use crate::commands;
use crate::layout;
use crate::projects;
use crate::error::{AppError, AppResult};
use crate::state::AppState;
use crate::util;
use crate::ws_terminal;

pub fn router(state: AppState) -> Router {
    let static_dir = state.config.server.static_dir.clone();
    Router::new()
        .route("/health", get(health))
        .route("/api/auth/login", post(auth::login))
        .route("/api/auth/me", get(auth::me))
        .route("/api/auth/logout", post(auth::logout))
        .route("/api/terminals", get(list_terminals).post(create_terminal))
        .route("/api/terminals/:id", delete(close_terminal))
        .route(
            "/api/commands",
            get(commands::list_commands).post(commands::create_command),
        )
        .route(
            "/api/commands/:id",
            put(commands::update_command).delete(commands::delete_command),
        )
        .route(
            "/api/projects",
            get(projects::list_projects).post(projects::create_project),
        )
        .route("/api/projects/:id", delete(projects::delete_project))
        .route(
            "/api/layout",
            get(layout::get_layout).put(layout::put_layout),
        )
        .route("/api/agent-status", post(agent_status::post_status))
        .route("/api/agent-status/ack", post(agent_status::ack_status))
        .route("/api/audit", get(audit::list_audit))
        .route("/ws/terminals/:id", get(ws_terminal::ws_terminal))
        .fallback_service(ServeDir::new(static_dir).append_index_html_on_directories(true))
        .layer(TraceLayer::new_for_http())
        // Defense-in-depth response headers (also settable at nginx).
        .layer(SetResponseHeaderLayer::overriding(
            header::X_FRAME_OPTIONS,
            HeaderValue::from_static("DENY"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            header::REFERRER_POLICY,
            HeaderValue::from_static("no-referrer"),
        ))
        .with_state(state)
}

async fn health() -> Json<serde_json::Value> {
    Json(json!({ "status": "ok" }))
}

#[derive(Deserialize)]
pub struct CreateTermReq {
    pub shell: Option<String>,
    pub cwd: Option<String>,
    #[serde(default = "d_cols")]
    pub cols: u16,
    #[serde(default = "d_rows")]
    pub rows: u16,
}
fn d_cols() -> u16 {
    120
}
fn d_rows() -> u16 {
    36
}

async fn create_terminal(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    user: AuthUser,
    Json(req): Json<CreateTermReq>,
) -> AppResult<Json<serde_json::Value>> {
    if !user.role.can_create_terminal() {
        audit::record(
            &state,
            Some(&user),
            None,
            "terminal.denied",
            None,
            json!({"reason":"role"}),
            "denied",
        )
        .await;
        return Err(AppError::Forbidden);
    }
    let ip = util::resolve_client_ip(&headers, peer.ip(), &state.config.network);
    let session = state.terminals.spawn(
        &user.id,
        &user.username,
        req.shell.clone(),
        req.cwd.clone(),
        req.cols,
        req.rows,
    )?;
    audit::record(
        &state,
        Some(&user),
        Some(&ip.to_string()),
        "terminal.spawn",
        Some(&session.id),
        json!({ "shell": session.shell, "cwd": session.cwd, "cols": req.cols, "rows": req.rows }),
        "ok",
    )
    .await;
    Ok(Json(json!({
        "id": session.id,
        "shell": session.shell,
        "cwd": session.cwd,
        "createdAt": session.created_at,
    })))
}

async fn list_terminals(
    State(state): State<AppState>,
    user: AuthUser,
) -> AppResult<Json<serde_json::Value>> {
    let arr: Vec<serde_json::Value> = state
        .terminals
        .list()
        .into_iter()
        .filter(|s| user.role.is_admin() || s.owner_user_id == user.id)
        .map(|s| {
            let agent = state
                .agent_status
                .lookup(&s.cwd, user.role.is_admin())
                .map(|a| json!({ "state": a.state, "updatedAt": a.updated_at }));
            json!({
                "id": s.id,
                "ownerUsername": s.owner_username,
                "shell": s.shell,
                "cwd": s.cwd,
                "createdAt": s.created_at,
                "lastActiveAt": s.last_active_rfc3339(),
                "exited": s.is_exited(),
                "agentStatus": agent,
            })
        })
        .collect();
    Ok(Json(json!(arr)))
}

async fn close_terminal(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    user: AuthUser,
    Path(id): Path<String>,
) -> AppResult<Json<serde_json::Value>> {
    let session = state.terminals.get(&id).ok_or(AppError::NotFound)?;
    if session.owner_user_id != user.id && !user.role.is_admin() {
        audit::record(
            &state,
            Some(&user),
            None,
            "terminal.denied",
            Some(&id),
            json!({"reason":"close_not_owner"}),
            "denied",
        )
        .await;
        return Err(AppError::Forbidden);
    }
    let ip = util::resolve_client_ip(&headers, peer.ip(), &state.config.network);
    state.terminals.remove(&id);
    audit::record(
        &state,
        Some(&user),
        Some(&ip.to_string()),
        "terminal.close",
        Some(&id),
        json!({}),
        "ok",
    )
    .await;
    Ok(Json(json!({ "ok": true })))
}
