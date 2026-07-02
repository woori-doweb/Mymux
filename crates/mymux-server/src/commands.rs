//! Per-user saved commands — personal command snippets a user can insert into
//! their active terminal. Adapted from the desktop app's `~/.mycli/commands.json`
//! store (mycli-core), but multi-user and server-side: SQLite, owner-scoped, and
//! auth-gated. Every handler takes `AuthUser`, so an unauthenticated request is
//! rejected before it runs, and every query is scoped to `owner_user_id`.

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
    command: String,
    description: String,
    favorite: i64,
    created_at: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandDto {
    id: String,
    name: String,
    command: String,
    description: String,
    favorite: bool,
    created_at: String,
}

impl From<Row> for CommandDto {
    fn from(r: Row) -> Self {
        Self {
            id: r.id,
            name: r.name,
            command: r.command,
            description: r.description,
            favorite: r.favorite != 0,
            created_at: r.created_at,
        }
    }
}

#[derive(Deserialize)]
pub struct CommandInput {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub favorite: bool,
}

const MAX_NAME: usize = 100;
const MAX_COMMAND: usize = 4000;
const MAX_DESCRIPTION: usize = 500;
const MAX_COMMANDS_PER_USER: i64 = 500;

/// Trim + bound the input. Returns the trimmed (name, command, description).
fn clean(input: &CommandInput) -> AppResult<(String, String, String)> {
    let name = input.name.trim().to_string();
    let command = input.command.trim().to_string();
    let description = input.description.trim().to_string();
    if name.is_empty() {
        return Err(AppError::BadRequest("name is required".into()));
    }
    if command.is_empty() {
        return Err(AppError::BadRequest("command is required".into()));
    }
    if name.chars().count() > MAX_NAME {
        return Err(AppError::BadRequest("name too long".into()));
    }
    if command.chars().count() > MAX_COMMAND {
        return Err(AppError::BadRequest("command too long".into()));
    }
    if description.chars().count() > MAX_DESCRIPTION {
        return Err(AppError::BadRequest("description too long".into()));
    }
    Ok((name, command, description))
}

/// GET /api/commands — the caller's own commands, favorites first then name.
pub async fn list_commands(
    State(state): State<AppState>,
    user: AuthUser,
) -> AppResult<Json<Vec<CommandDto>>> {
    let rows = sqlx::query_as::<_, Row>(
        "SELECT id, name, command, description, favorite, created_at \
         FROM saved_commands WHERE owner_user_id = ? \
         ORDER BY favorite DESC, name COLLATE NOCASE ASC",
    )
    .bind(&user.id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(rows.into_iter().map(CommandDto::from).collect()))
}

/// POST /api/commands — create a command owned by the caller.
pub async fn create_command(
    State(state): State<AppState>,
    user: AuthUser,
    Json(input): Json<CommandInput>,
) -> AppResult<Json<CommandDto>> {
    let (name, command, description) = clean(&input)?;
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM saved_commands WHERE owner_user_id = ?")
            .bind(&user.id)
            .fetch_one(&state.db)
            .await?;
    if count >= MAX_COMMANDS_PER_USER {
        return Err(AppError::BadRequest("command limit reached".into()));
    }
    let id = Uuid::new_v4().to_string();
    let now = util::now_rfc3339();
    sqlx::query(
        "INSERT INTO saved_commands \
         (id, owner_user_id, name, command, description, favorite, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&user.id)
    .bind(&name)
    .bind(&command)
    .bind(&description)
    .bind(input.favorite as i64)
    .bind(&now)
    .bind(&now)
    .execute(&state.db)
    .await?;
    Ok(Json(CommandDto {
        id,
        name,
        command,
        description,
        favorite: input.favorite,
        created_at: now,
    }))
}

/// PUT /api/commands/:id — update one of the caller's commands (incl. favorite).
pub async fn update_command(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
    Json(input): Json<CommandInput>,
) -> AppResult<Json<serde_json::Value>> {
    let (name, command, description) = clean(&input)?;
    let now = util::now_rfc3339();
    let res = sqlx::query(
        "UPDATE saved_commands \
         SET name = ?, command = ?, description = ?, favorite = ?, updated_at = ? \
         WHERE id = ? AND owner_user_id = ?",
    )
    .bind(&name)
    .bind(&command)
    .bind(&description)
    .bind(input.favorite as i64)
    .bind(&now)
    .bind(&id)
    .bind(&user.id)
    .execute(&state.db)
    .await?;
    if res.rows_affected() == 0 {
        return Err(AppError::NotFound);
    }
    Ok(Json(json!({ "ok": true })))
}

/// DELETE /api/commands/:id — remove one of the caller's commands.
pub async fn delete_command(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
) -> AppResult<Json<serde_json::Value>> {
    let res = sqlx::query("DELETE FROM saved_commands WHERE id = ? AND owner_user_id = ?")
        .bind(&id)
        .bind(&user.id)
        .execute(&state.db)
        .await?;
    if res.rows_affected() == 0 {
        return Err(AppError::NotFound);
    }
    Ok(Json(json!({ "ok": true })))
}
