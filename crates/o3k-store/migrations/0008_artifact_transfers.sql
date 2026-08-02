CREATE TABLE artifact_transfers (
    transfer_id TEXT PRIMARY KEY NOT NULL CHECK (length(transfer_id) BETWEEN 1 AND 128),
    command_id TEXT NOT NULL CHECK (length(command_id) BETWEEN 1 AND 128),
    operation_id TEXT NOT NULL REFERENCES operations(id) ON DELETE CASCADE,
    resource_id TEXT NOT NULL REFERENCES resources(id) ON DELETE CASCADE,
    agent_id TEXT NOT NULL CHECK (length(agent_id) BETWEEN 1 AND 128),
    agent_epoch TEXT NOT NULL CHECK (length(agent_epoch) BETWEEN 1 AND 256),
    artifact_id TEXT NOT NULL CHECK (length(artifact_id) BETWEEN 1 AND 256),
    artifact_kind TEXT NOT NULL CHECK (length(artifact_kind) BETWEEN 1 AND 64),
    sha256 TEXT NOT NULL CHECK (length(sha256) = 64),
    size_bytes INTEGER NOT NULL CHECK (size_bytes BETWEEN 1 AND 67108864),
    format TEXT NOT NULL CHECK (length(format) BETWEEN 1 AND 32),
    chunk_size_bytes INTEGER NOT NULL CHECK (chunk_size_bytes BETWEEN 1 AND 262144),
    chunk_count INTEGER NOT NULL CHECK (chunk_count BETWEEN 1 AND 67108864),
    state TEXT NOT NULL CHECK (state IN ('offered', 'receiving', 'committed', 'rejected', 'expired')),
    contiguous_bytes INTEGER NOT NULL DEFAULT 0 CHECK (contiguous_bytes BETWEEN 0 AND 67108864),
    next_chunk_index INTEGER NOT NULL DEFAULT 0 CHECK (next_chunk_index BETWEEN 0 AND 67108864),
    retry_count INTEGER NOT NULL DEFAULT 0 CHECK (retry_count BETWEEN 0 AND 16),
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CHECK (contiguous_bytes <= size_bytes),
    CHECK (next_chunk_index <= chunk_count),
    CHECK (chunk_count = ((size_bytes + chunk_size_bytes - 1) / chunk_size_bytes))
);

CREATE INDEX artifact_transfers_operation_idx ON artifact_transfers(operation_id);
CREATE INDEX artifact_transfers_agent_epoch_idx ON artifact_transfers(agent_id, agent_epoch);
CREATE INDEX artifact_transfers_recovery_idx ON artifact_transfers(state, updated_at);
