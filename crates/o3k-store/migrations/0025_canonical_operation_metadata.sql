CREATE TABLE canonical_operation_metadata (
    operation_id TEXT PRIMARY KEY NOT NULL REFERENCES operations(id) ON DELETE CASCADE,
    service TEXT NOT NULL, action TEXT NOT NULL, actor TEXT NOT NULL,
    owner_scope TEXT NOT NULL, resource_type TEXT NOT NULL, resource_id TEXT,
    attempt INTEGER NOT NULL CHECK (attempt >= 0), created_at TEXT NOT NULL,
    started_at TEXT, finished_at TEXT, error TEXT, request_id TEXT
);
