CREATE TABLE IF NOT EXISTS audit_events (
    event_id TEXT PRIMARY KEY,
    timestamp TEXT NOT NULL,
    request_id TEXT NOT NULL,
    audit_id TEXT NOT NULL,
    principal_id TEXT NOT NULL,
    effective_scope TEXT NOT NULL,
    service_namespace TEXT NOT NULL,
    action TEXT NOT NULL,
    resource_type TEXT,
    resource_id TEXT,
    owner_scope TEXT,
    operation_id TEXT,
    outcome TEXT NOT NULL,
    reason_category TEXT,
    event_json TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS audit_events_scope_event_idx
    ON audit_events(effective_scope, event_id);
