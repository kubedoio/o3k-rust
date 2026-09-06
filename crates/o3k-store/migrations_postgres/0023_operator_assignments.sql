CREATE TABLE IF NOT EXISTS operator_assignments (
    id TEXT PRIMARY KEY NOT NULL,
    user_id TEXT NOT NULL REFERENCES keystone_users(id) ON DELETE CASCADE,
    profile TEXT NOT NULL,
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE(user_id, profile)
);

CREATE INDEX IF NOT EXISTS idx_operator_assignments_user_enabled
    ON operator_assignments(user_id, enabled);
