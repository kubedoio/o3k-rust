CREATE TABLE agent_commands (
    command_id TEXT PRIMARY KEY NOT NULL,
    idempotency_key TEXT NOT NULL UNIQUE,
    operation_id TEXT NOT NULL REFERENCES operations(id) ON DELETE CASCADE,
    resource_id TEXT NOT NULL REFERENCES resources(id) ON DELETE CASCADE,
    agent_id TEXT NOT NULL,
    agent_epoch TEXT NOT NULL,
    payload_fingerprint_sha256 TEXT NOT NULL,
    payload BLOB NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('pending', 'accepted', 'running', 'succeeded', 'retryable', 'unknown_outcome', 'failed')),
    accepted_sequence INTEGER NOT NULL DEFAULT 0 CHECK (accepted_sequence >= 0),
    last_sequence INTEGER NOT NULL DEFAULT 0 CHECK (last_sequence >= 0),
    provider_operation_id TEXT,
    provider_resource_id TEXT,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX agent_commands_operation_idx ON agent_commands(operation_id);
CREATE INDEX agent_commands_resource_idx ON agent_commands(resource_id);
