-- Durable metering store (ADR-0183, SPEC-0046): O3K metering authority anchor,
-- open/closed usage intervals, and bounded ingest-bucket aggregates. Bucket
-- arithmetic is owned by the kernel port; storage only persists.
CREATE TABLE IF NOT EXISTS metering_authority (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    authority_started_at_ms BIGINT NOT NULL,
    last_observed_at_ms BIGINT NOT NULL
);

CREATE TABLE IF NOT EXISTS metering_intervals (
    interval_id TEXT PRIMARY KEY NOT NULL,
    meter_key TEXT NOT NULL,
    scope TEXT NOT NULL,
    resource_id TEXT NOT NULL,
    quantity BIGINT NOT NULL CHECK (quantity >= 0),
    started_at_ms BIGINT NOT NULL,
    ended_at_ms BIGINT,
    authority TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_metering_intervals_series
    ON metering_intervals(meter_key, scope, resource_id, started_at_ms);
CREATE UNIQUE INDEX IF NOT EXISTS idx_metering_intervals_open
    ON metering_intervals(meter_key, scope, resource_id) WHERE ended_at_ms IS NULL;

CREATE TABLE IF NOT EXISTS metering_aggregates (
    scope TEXT NOT NULL,
    meter_key TEXT NOT NULL,
    resource_id TEXT NOT NULL,
    bucket_start_ms BIGINT NOT NULL,
    bucket_width_ms BIGINT NOT NULL,
    quantity_millis BIGINT NOT NULL CHECK (quantity_millis >= 0),
    PRIMARY KEY (scope, meter_key, resource_id, bucket_start_ms)
);
CREATE INDEX IF NOT EXISTS idx_metering_aggregates_scope_bucket
    ON metering_aggregates(scope, meter_key, bucket_start_ms, resource_id);

-- The bounded usage read filters open intervals by series and start instant
-- (`meter_key = ? AND scope = ? AND ended_at_ms IS NULL AND started_at_ms < ?`),
-- which neither index above serves. Identical DDL on both engines.
CREATE INDEX IF NOT EXISTS idx_metering_intervals_open_series
    ON metering_intervals(meter_key, scope, started_at_ms) WHERE ended_at_ms IS NULL;
