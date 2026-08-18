CREATE TABLE IF NOT EXISTS network_intents (
    id TEXT PRIMARY KEY NOT NULL,
    project_id TEXT NOT NULL,
    generation INTEGER NOT NULL,
    payload TEXT NOT NULL,
    plan_fingerprint_sha256 TEXT,
    status TEXT NOT NULL,
    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX IF NOT EXISTS idx_network_intents_project ON network_intents(project_id);
