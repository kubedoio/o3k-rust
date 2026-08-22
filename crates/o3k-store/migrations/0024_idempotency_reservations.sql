CREATE TABLE idempotency_reservations (
    owner_scope TEXT NOT NULL,
    action TEXT NOT NULL,
    idempotency_key TEXT NOT NULL CHECK (length(idempotency_key) BETWEEN 1 AND 128),
    fingerprint TEXT NOT NULL,
    operation_id TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (owner_scope, action, idempotency_key)
);
CREATE UNIQUE INDEX idempotency_reservations_operation_idx ON idempotency_reservations(operation_id);
