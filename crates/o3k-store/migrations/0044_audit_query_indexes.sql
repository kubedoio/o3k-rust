CREATE INDEX IF NOT EXISTS audit_events_scope_timestamp_event_idx
    ON audit_events(effective_scope, timestamp, event_id);
CREATE INDEX IF NOT EXISTS audit_events_scope_service_action_event_idx
    ON audit_events(effective_scope, service_namespace, action, event_id);
CREATE INDEX IF NOT EXISTS audit_events_scope_outcome_event_idx
    ON audit_events(effective_scope, outcome, event_id);
CREATE INDEX IF NOT EXISTS audit_events_scope_operation_event_idx
    ON audit_events(effective_scope, operation_id, event_id);
