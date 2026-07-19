//! SQLite connection, migration, and the row structs shared across modules.
//! Queries themselves live next to their domain logic (auth.rs, audit.rs).

use std::str::FromStr;

use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use crate::error::AppError;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct UserRow {
    pub id: String,
    pub username: String,
    pub password_hash: String,
    pub role: String,
    pub disabled: i64,
}

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize)]
pub struct AuditRow {
    pub id: i64,
    pub time: String,
    pub user_id: Option<String>,
    pub username: Option<String>,
    pub ip: Option<String>,
    pub action: String,
    pub target: Option<String>,
    pub metadata_json: Option<String>,
    pub result: String,
}

/// Open (creating if absent) the SQLite database and return a pool.
pub async fn connect(database_url: &str) -> Result<SqlitePool, AppError> {
    // SQLite won't create the parent directory — do it ourselves for file URLs.
    if let Some(path) = sqlite_file_path(database_url)
        && let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .map_err(|e| AppError::Internal(format!("create db dir {}: {e}", parent.display())))?;
    }

    let opts = SqliteConnectOptions::from_str(database_url)
        .map_err(|e| AppError::Internal(format!("bad database_url: {e}")))?
        .create_if_missing(true)
        .busy_timeout(std::time::Duration::from_secs(5));

    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(opts)
        .await?;
    Ok(pool)
}

/// Apply all schema migrations (idempotent — every statement uses
/// CREATE TABLE/INDEX IF NOT EXISTS, so re-running is a no-op on existing DBs).
pub async fn migrate(pool: &SqlitePool) -> Result<(), AppError> {
    sqlx::raw_sql(include_str!("../migrations/0001_init.sql"))
        .execute(pool)
        .await?;
    sqlx::raw_sql(include_str!("../migrations/0002_saved_commands.sql"))
        .execute(pool)
        .await?;
    sqlx::raw_sql(include_str!("../migrations/0003_command_defaults.sql"))
        .execute(pool)
        .await?;
    sqlx::raw_sql(include_str!("../migrations/0004_projects_layouts.sql"))
        .execute(pool)
        .await?;
    Ok(())
}

/// Extract the filesystem path from a `sqlite:` / `sqlite://` URL, if any.
fn sqlite_file_path(url: &str) -> Option<std::path::PathBuf> {
    let rest = url
        .strip_prefix("sqlite://")
        .or_else(|| url.strip_prefix("sqlite:"))?;
    let rest = rest.split('?').next().unwrap_or(rest);
    if rest.is_empty() || rest == ":memory:" {
        return None;
    }
    Some(std::path::PathBuf::from(rest))
}
