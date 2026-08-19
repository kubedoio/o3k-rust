CREATE TABLE IF NOT EXISTS native_storage_backends (
    id TEXT PRIMARY KEY NOT NULL,
    scope_kind TEXT NOT NULL CHECK (scope_kind IN ('host', 'backend')),
    scope_id TEXT NOT NULL,
    generation INTEGER NOT NULL CHECK (generation > 0),
    available INTEGER NOT NULL CHECK (available IN (0, 1)),
    payload TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS native_volumes (
    id TEXT PRIMARY KEY NOT NULL,
    project_id TEXT NOT NULL,
    generation INTEGER NOT NULL CHECK (generation > 0),
    state TEXT NOT NULL,
    payload TEXT NOT NULL,
    provider_name TEXT,
    provider_resource_id TEXT,
    created_at TEXT NOT NULL,
    CHECK ((provider_name IS NULL) = (provider_resource_id IS NULL))
);
CREATE INDEX IF NOT EXISTS native_volumes_project_idx ON native_volumes(project_id);

CREATE TABLE IF NOT EXISTS native_volume_attachments (
    id TEXT PRIMARY KEY NOT NULL,
    project_id TEXT NOT NULL,
    volume_id TEXT NOT NULL UNIQUE,
    server_id TEXT NOT NULL,
    scope_kind TEXT NOT NULL CHECK (scope_kind IN ('host', 'backend')),
    scope_id TEXT NOT NULL,
    generation INTEGER NOT NULL CHECK (generation > 0),
    state TEXT NOT NULL,
    payload TEXT NOT NULL,
    created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS native_volume_attachments_project_idx ON native_volume_attachments(project_id);
CREATE INDEX IF NOT EXISTS native_volume_attachments_server_idx ON native_volume_attachments(server_id);

CREATE TABLE IF NOT EXISTS native_snapshots (
    id TEXT PRIMARY KEY NOT NULL,
    project_id TEXT NOT NULL,
    volume_id TEXT NOT NULL,
    scope_kind TEXT NOT NULL CHECK (scope_kind IN ('host', 'backend')),
    scope_id TEXT NOT NULL,
    source_generation INTEGER NOT NULL CHECK (source_generation > 0),
    generation INTEGER NOT NULL CHECK (generation > 0),
    state TEXT NOT NULL,
    payload TEXT NOT NULL,
    provider_name TEXT,
    provider_resource_id TEXT,
    created_at TEXT NOT NULL,
    CHECK ((provider_name IS NULL) = (provider_resource_id IS NULL))
);
CREATE INDEX IF NOT EXISTS native_snapshots_project_idx ON native_snapshots(project_id);
CREATE INDEX IF NOT EXISTS native_snapshots_volume_idx ON native_snapshots(volume_id);
