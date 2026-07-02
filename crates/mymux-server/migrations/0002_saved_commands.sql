-- Per-user saved command snippets (migration 0002).
-- Applied at startup via sqlx::raw_sql (idempotent; safe to re-run).
-- Each row is owned by one user; a user only ever sees/edits their own.

CREATE TABLE IF NOT EXISTS saved_commands (
    id TEXT PRIMARY KEY,
    owner_user_id TEXT NOT NULL,
    name TEXT NOT NULL,
    command TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    favorite INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    FOREIGN KEY(owner_user_id) REFERENCES users(id)
);

CREATE INDEX IF NOT EXISTS idx_saved_commands_owner ON saved_commands(owner_user_id);
