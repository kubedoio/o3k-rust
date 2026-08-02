CREATE TABLE image_overlay_ownership (
    overlay_id TEXT PRIMARY KEY NOT NULL CHECK (length(overlay_id) BETWEEN 1 AND 128),
    resource_id TEXT NOT NULL REFERENCES resources(id) ON DELETE CASCADE,
    operation_id TEXT NOT NULL REFERENCES operations(id) ON DELETE CASCADE,
    command_id TEXT NOT NULL CHECK (length(command_id) BETWEEN 1 AND 128),
    agent_id TEXT NOT NULL CHECK (length(agent_id) BETWEEN 1 AND 128),
    agent_epoch TEXT NOT NULL CHECK (length(agent_epoch) BETWEEN 1 AND 256),
    base_sha256 TEXT NOT NULL CHECK (length(base_sha256) = 64),
    base_format TEXT NOT NULL CHECK (base_format IN ('raw', 'qcow2')),
    overlay_format TEXT NOT NULL CHECK (overlay_format = 'qcow2'),
    state TEXT NOT NULL CHECK (state IN ('pending', 'materializing', 'ready', 'deleting', 'deleted', 'failed')),
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(resource_id, operation_id, command_id)
);

CREATE INDEX image_overlay_ownership_resource_idx
    ON image_overlay_ownership(resource_id, state);
CREATE INDEX image_overlay_ownership_base_ref_idx
    ON image_overlay_ownership(base_sha256, base_format, state);
CREATE INDEX image_overlay_ownership_recovery_idx
    ON image_overlay_ownership(state, updated_at);
