-- Marker of which users have had the built-in default commands seeded (once).
-- Applied at startup via sqlx::raw_sql (idempotent; safe to re-run). A seeded
-- user is never re-seeded, so deleting a default command makes it stay gone.

CREATE TABLE IF NOT EXISTS command_seed_marker (
    user_id TEXT PRIMARY KEY,
    seeded_at TEXT NOT NULL
);
