//! Audit logging. Every significant auth/terminal event is persisted. Terminal
//! input/output bodies are NEVER logged here (config can opt in elsewhere; the
//! default is off) — secrets would otherwise leak into the audit trail.

use axum::Json;
use axum::extract::{Query, State};
use serde::Deserialize;
use serde_json::Value;

use crate::auth::AuthUser;
use crate::db::AuditRow;
use crate::error::{AppError, AppResult};
use crate::state::AppState;
use crate::util;

/// Record an audit event. Best-effort: a logging failure never breaks a request.
pub async fn record(
    state: &AppState,
    user: Option<&AuthUser>,
    ip: Option<&str>,
    action: &str,
    target: Option<&str>,
    metadata: Value,
    result: &str,
) {
    if !state.config.audit.enabled {
        return;
    }
    let now = util::now_rfc3339();
    let meta = if metadata.is_null() {
        None
    } else {
        Some(metadata.to_string())
    };
    let (uid, uname) = match user {
        Some(u) => (Some(u.id.clone()), Some(u.username.clone())),
        None => (None, None),
    };
    let _ = sqlx::query(
        "INSERT INTO audit_logs (time, user_id, username, ip, action, target, metadata_json, result) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&now)
    .bind(uid)
    .bind(uname)
    .bind(ip)
    .bind(action)
    .bind(target)
    .bind(meta)
    .bind(result)
    .execute(&state.db)
    .await;
}

#[derive(Deserialize)]
pub struct AuditQuery {
    pub limit: Option<i64>,
}

/// Admin-only: most recent audit rows.
pub async fn list_audit(
    State(state): State<AppState>,
    user: AuthUser,
    Query(q): Query<AuditQuery>,
) -> AppResult<Json<Vec<AuditRow>>> {
    if !user.role.is_admin() {
        return Err(AppError::Forbidden);
    }
    let limit = q.limit.unwrap_or(100).clamp(1, 500);
    let rows = sqlx::query_as::<_, AuditRow>(
        "SELECT id, time, user_id, username, ip, action, target, metadata_json, result \
         FROM audit_logs ORDER BY id DESC LIMIT ?",
    )
    .bind(limit)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(rows))
}
