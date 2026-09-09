CREATE TABLE IF NOT EXISTS metering_events (
    event_id TEXT PRIMARY KEY NOT NULL,
    project_id TEXT NOT NULL,
    meter_id TEXT NOT NULL,
    resource_id TEXT,
    quantity INTEGER NOT NULL CHECK (quantity >= 0),
    unit TEXT NOT NULL,
    effective_at TEXT NOT NULL,
    recorded_at TEXT NOT NULL,
    source TEXT NOT NULL,
    payload_fingerprint TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS metering_events_project_meter_time_idx
    ON metering_events(project_id, meter_id, effective_at, event_id);
