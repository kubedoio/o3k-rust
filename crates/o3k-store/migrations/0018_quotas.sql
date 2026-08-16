CREATE TABLE IF NOT EXISTS quota_limits (
    scope_id TEXT NOT NULL,
    scope_kind TEXT NOT NULL,
    namespace TEXT NOT NULL,
    resource TEXT NOT NULL,
    limit_value INTEGER,
    PRIMARY KEY (scope_id, scope_kind, namespace, resource)
);

CREATE TABLE IF NOT EXISTS quota_reservations (
    id TEXT PRIMARY KEY NOT NULL,
    scope_id TEXT NOT NULL,
    scope_kind TEXT NOT NULL,
    operation_id TEXT NOT NULL UNIQUE,
    state TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_quota_res_scope ON quota_reservations(scope_id, state);
CREATE INDEX IF NOT EXISTS idx_quota_res_op ON quota_reservations(operation_id);

CREATE TABLE IF NOT EXISTS quota_reservation_amounts (
    reservation_id TEXT NOT NULL REFERENCES quota_reservations(id) ON DELETE CASCADE,
    namespace TEXT NOT NULL,
    resource TEXT NOT NULL,
    amount INTEGER NOT NULL,
    PRIMARY KEY (reservation_id, namespace, resource)
);
