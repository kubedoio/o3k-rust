CREATE TABLE IF NOT EXISTS federated_bindings (
    id TEXT PRIMARY KEY,
    trusted_issuer_id TEXT NOT NULL,
    issuer TEXT NOT NULL,
    subject TEXT NOT NULL,
    principal_id TEXT NOT NULL REFERENCES keystone_users(id),
    principal_type TEXT NOT NULL CHECK (principal_type = 'user'),
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE(trusted_issuer_id, subject),
    CHECK(length(trusted_issuer_id) > 0),
    CHECK(length(issuer) > 0),
    CHECK(length(subject) > 0)
);

CREATE INDEX IF NOT EXISTS idx_federated_bindings_principal
    ON federated_bindings(principal_id);
