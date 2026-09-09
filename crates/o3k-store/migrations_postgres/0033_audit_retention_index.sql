-- Retention deletes select by timestamp across all security scopes.  Keep the
-- bounded maintenance operation bounded at the database boundary rather than
-- forcing a full audit table scan as the history grows.
CREATE INDEX IF NOT EXISTS audit_events_timestamp_event_idx
    ON audit_events(timestamp, event_id);
