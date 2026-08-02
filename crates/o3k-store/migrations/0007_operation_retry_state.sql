CREATE TABLE operation_retry_state (
    operation_id TEXT PRIMARY KEY NOT NULL REFERENCES operations(id) ON DELETE CASCADE,
    attempts INTEGER NOT NULL CHECK (attempts >= 0),
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);
