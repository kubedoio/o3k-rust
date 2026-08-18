CREATE TABLE IF NOT EXISTS network_intents (
    id VARCHAR(36) PRIMARY KEY NOT NULL,
    project_id VARCHAR(255) NOT NULL,
    generation BIGINT NOT NULL,
    payload TEXT NOT NULL,
    plan_fingerprint_sha256 VARCHAR(64),
    status VARCHAR(32) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_network_intents_project ON network_intents(project_id);
