CREATE TABLE keypairs (
    id TEXT PRIMARY KEY NOT NULL,
    user_id TEXT NOT NULL,
    project_id TEXT NOT NULL,
    name TEXT NOT NULL,
    key_type TEXT NOT NULL,
    public_key TEXT NOT NULL,
    fingerprint TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(user_id, project_id, name)
);

CREATE INDEX keypairs_scope_idx ON keypairs(user_id, project_id);

CREATE TABLE server_keypairs (
    server_id TEXT PRIMARY KEY NOT NULL REFERENCES resources(id) ON DELETE CASCADE,
    keypair_id TEXT NOT NULL REFERENCES keypairs(id)
);
