CREATE TABLE IF NOT EXISTS audit_events (
    event_id TEXT PRIMARY KEY NOT NULL,
    timestamp TEXT NOT NULL,
    request_id TEXT NOT NULL,
    audit_id TEXT NOT NULL,
    principal_id TEXT NOT NULL,
    principal_kind TEXT NOT NULL,
    effective_scope TEXT NOT NULL,
    service TEXT NOT NULL,
    action TEXT NOT NULL,
    resource_type TEXT,
    resource_id TEXT,
    owner_scope TEXT,
    operation_id TEXT,
    outcome TEXT NOT NULL,
    reason_category TEXT
);

CREATE INDEX IF NOT EXISTS idx_audit_scope_time
    ON audit_events(effective_scope, timestamp, event_id);
CREATE INDEX IF NOT EXISTS idx_audit_operation
    ON audit_events(effective_scope, operation_id, timestamp, event_id);
