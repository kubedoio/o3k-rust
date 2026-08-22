CREATE TABLE IF NOT EXISTS idempotency_reservations (
    owner_scope TEXT NOT NULL,
    action TEXT NOT NULL,
    idempotency_key TEXT NOT NULL CHECK (length(idempotency_key) BETWEEN 1 AND 128),
    fingerprint TEXT NOT NULL,
    operation_id TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (owner_scope, action, idempotency_key),
    UNIQUE (operation_id)
);
