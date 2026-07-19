//! Per-user workspace layout persistence (nmux-linux absorption phase 2).
//!
//! The client serializes its tab/split structure to one JSON blob and PUTs it
//! here; after a page reload — or a server restart that killed every PTY — the
//! client GETs it back and rebuilds the structure, reattaching to sessions
//! that still exist and respawning shells (at their saved cwd) for the rest.
//! Same semantics as nmux-linux: structure survives, scrollback doesn't.
//!
//! The blob is opaque to the server on purpose (the split-tree shape is a
//! frontend concern); we only enforce that it is valid JSON and bounded.

use axum::Json;
use axum::extract::State;
use serde::Deserialize;
use serde_json::json;

use crate::auth::AuthUser;
use crate::error::{AppError, AppResult};
use crate::state::AppState;
use crate::util;

/// Generous for a split-tree, tiny for a DB row.
const MAX_LAYOUT_BYTES: usize = 65536;

#[derive(Deserialize)]
pub struct LayoutInput {
    pub data: serde_json::Value,
}

/// GET /api/layout — the caller's saved layout, `{ "data": null }` if none.
pub async fn get_layout(
    State(state): State<AppState>,
    user: AuthUser,
) -> AppResult<Json<serde_json::Value>> {
    let row: Option<String> =
        sqlx::query_scalar("SELECT data FROM layouts WHERE owner_user_id = ?")
            .bind(&user.id)
            .fetch_optional(&state.db)
            .await?;
    let data = match row {
        Some(text) => serde_json::from_str(&text).unwrap_or(serde_json::Value::Null),
        None => serde_json::Value::Null,
    };
    Ok(Json(json!({ "data": data })))
}

/// PUT /api/layout — upsert the caller's layout.
pub async fn put_layout(
    State(state): State<AppState>,
    user: AuthUser,
    Json(input): Json<LayoutInput>,
) -> AppResult<Json<serde_json::Value>> {
    let text = serde_json::to_string(&input.data)
        .map_err(|e| AppError::BadRequest(format!("bad layout: {e}")))?;
    if text.len() > MAX_LAYOUT_BYTES {
        return Err(AppError::BadRequest("layout too large".into()));
    }
    let now = util::now_rfc3339();
    sqlx::query(
        "INSERT INTO layouts (owner_user_id, data, updated_at) VALUES (?, ?, ?) \
         ON CONFLICT(owner_user_id) DO UPDATE SET data = excluded.data, \
         updated_at = excluded.updated_at",
    )
    .bind(&user.id)
    .bind(&text)
    .bind(&now)
    .execute(&state.db)
    .await?;
    Ok(Json(json!({ "ok": true })))
}
