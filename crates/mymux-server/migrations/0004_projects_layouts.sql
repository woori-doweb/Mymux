-- Projects (per-user workspace shortcuts) + saved workspace layouts (0004).
-- Applied at startup via sqlx::raw_sql (idempotent; safe to re-run).
-- nmux-linux absorption phase 2: "project = one workspace unit" and
-- "structure survives a server restart" (shells respawn, scrollback doesn't).

CREATE TABLE IF NOT EXISTS projects (
    id TEXT PRIMARY KEY,
    owner_user_id TEXT NOT NULL,
    name TEXT NOT NULL,
    cwd TEXT NOT NULL,
    created_at TEXT NOT NULL,
    FOREIGN KEY(owner_user_id) REFERENCES users(id),
    UNIQUE(owner_user_id, name)
);
CREATE INDEX IF NOT EXISTS idx_projects_owner ON projects(owner_user_id);

-- One JSON blob per user: { tabs: [{ name, root }] }, where root is a
-- split-tree of {t:'s',dir,ratio,a,b} / {t:'p',termId,cwd} nodes.
CREATE TABLE IF NOT EXISTS layouts (
    owner_user_id TEXT PRIMARY KEY,
    data TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    FOREIGN KEY(owner_user_id) REFERENCES users(id)
);
