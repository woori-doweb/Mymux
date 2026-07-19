//! Per-user projects — "one project = one workspace unit" (nmux-linux
//! absorption). A project is just a named cwd inside workspace_root; opening
//! one spawns a terminal there, and the sidebar rolls the agent-status badge
//! up to the project row. Owner-scoped and auth-gated like saved_commands.

use axum::Json;
use axum::extract::{Path, State};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::error::{AppError, AppResult};
use crate::state::AppState;
use crate::util;

#[derive(sqlx::FromRow)]
struct Row {
    id: String,
    name: String,
    cwd: String,
    created_at: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectDto {
    id: String,
    name: String,
    cwd: String,
    created_at: String,
}

impl From<Row> for ProjectDto {
    fn from(r: Row) -> Self {
        Self {
            id: r.id,
            name: r.name,
            cwd: r.cwd,
            created_at: r.created_at,
        }
    }
}

#[derive(Deserialize)]
pub struct ProjectInput {
    pub name: String,
    pub cwd: String,
}

const MAX_NAME: usize = 100;
const MAX_PROJECTS_PER_USER: i64 = 100;

/// Validate + canonicalize the project cwd: it must exist and live inside
/// workspace_root — the same containment rule terminal spawn enforces, checked
/// here too so a project can't be a standing pointer outside the workspace.
fn resolve_project_cwd(state: &AppState, requested: &str) -> AppResult<String> {
    let root = std::fs::canonicalize(&state.config.terminal.workspace_root)
        .map_err(|e| AppError::Internal(format!("workspace_root not accessible: {e}")))?;
    let target = std::fs::canonicalize(requested)
        .map_err(|_| AppError::BadRequest("directory not found".into()))?;
    if !target.starts_with(&root) {
        return Err(AppError::Forbidden);
    }
    Ok(target.to_string_lossy().to_string())
}

/// GET /api/projects — the caller's own projects, by name.
pub async fn list_projects(
    State(state): State<AppState>,
    user: AuthUser,
) -> AppResult<Json<Vec<ProjectDto>>> {
    let rows = sqlx::query_as::<_, Row>(
        "SELECT id, name, cwd, created_at FROM projects \
         WHERE owner_user_id = ? ORDER BY name COLLATE NOCASE ASC",
    )
    .bind(&user.id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(rows.into_iter().map(ProjectDto::from).collect()))
}

/// POST /api/projects — register a project (same role gate as spawning a
/// terminal: a viewer can't create what they could never open).
pub async fn create_project(
    State(state): State<AppState>,
    user: AuthUser,
    Json(input): Json<ProjectInput>,
) -> AppResult<Json<ProjectDto>> {
    if !user.role.can_create_terminal() {
        return Err(AppError::Forbidden);
    }
    let name = input.name.trim().to_string();
    if name.is_empty() {
        return Err(AppError::BadRequest("name is required".into()));
    }
    if name.chars().count() > MAX_NAME {
        return Err(AppError::BadRequest("name too long".into()));
    }
    let cwd = resolve_project_cwd(&state, input.cwd.trim())?;

    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM projects WHERE owner_user_id = ?")
        .bind(&user.id)
        .fetch_one(&state.db)
        .await?;
    if count >= MAX_PROJECTS_PER_USER {
        return Err(AppError::BadRequest("project limit reached".into()));
    }

    let id = Uuid::new_v4().to_string();
    let now = util::now_rfc3339();
    sqlx::query(
        "INSERT INTO projects (id, owner_user_id, name, cwd, created_at) VALUES (?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&user.id)
    .bind(&name)
    .bind(&cwd)
    .bind(&now)
    .execute(&state.db)
    .await
    .map_err(|e| match e {
        sqlx::Error::Database(ref d) if d.is_unique_violation() => {
            AppError::BadRequest("a project with that name already exists".into())
        }
        other => AppError::from(other),
    })?;
    Ok(Json(ProjectDto {
        id,
        name,
        cwd,
        created_at: now,
    }))
}

/// DELETE /api/projects/:id — remove one of the caller's projects. Running
/// terminals are untouched — a project is only a shortcut.
pub async fn delete_project(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
) -> AppResult<Json<serde_json::Value>> {
    let res = sqlx::query("DELETE FROM projects WHERE id = ? AND owner_user_id = ?")
        .bind(&id)
        .bind(&user.id)
        .execute(&state.db)
        .await?;
    if res.rows_affected() == 0 {
        return Err(AppError::NotFound);
    }
    Ok(Json(json!({ "ok": true })))
}
