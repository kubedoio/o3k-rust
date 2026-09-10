-- Query-shape indexes for the bounded native Audit collection.  Scope is
-- always the leading key so tenant history never requires a global scan.
CREATE INDEX IF NOT EXISTS idx_audit_scope_service_event
    ON audit_events(effective_scope, service, event_id);
CREATE INDEX IF NOT EXISTS idx_audit_scope_action_event
    ON audit_events(effective_scope, action, event_id);
CREATE INDEX IF NOT EXISTS idx_audit_scope_outcome_event
    ON audit_events(effective_scope, outcome, event_id);
CREATE INDEX IF NOT EXISTS idx_audit_scope_principal_event
    ON audit_events(effective_scope, principal_id, event_id);
CREATE INDEX IF NOT EXISTS idx_audit_scope_request_event
    ON audit_events(effective_scope, request_id, event_id);
CREATE INDEX IF NOT EXISTS idx_audit_scope_audit_event
    ON audit_events(effective_scope, audit_id, event_id);
CREATE INDEX IF NOT EXISTS idx_audit_scope_resource_event
    ON audit_events(effective_scope, resource_type, resource_id, event_id);
