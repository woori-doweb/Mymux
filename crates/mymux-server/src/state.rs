//! Shared application state handed to every axum handler via `State<AppState>`.

use std::sync::Arc;

use sqlx::SqlitePool;

use crate::config::Config;
use crate::terminal::TerminalManager;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub db: SqlitePool,
    pub terminals: Arc<TerminalManager>,
}
