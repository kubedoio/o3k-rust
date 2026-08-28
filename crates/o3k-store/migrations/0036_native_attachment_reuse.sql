-- A volume may be detached and later attached again with a new canonical
-- attachment identity.  Historical terminal rows remain durable, while the
-- live attachment invariant stays database-enforced.
ALTER TABLE native_volume_attachments RENAME TO native_volume_attachments_old;

CREATE TABLE native_volume_attachments (
    id TEXT PRIMARY KEY NOT NULL,
    project_id TEXT NOT NULL,
    volume_id TEXT NOT NULL,
    server_id TEXT NOT NULL,
    scope_kind TEXT NOT NULL CHECK (scope_kind IN ('host', 'backend')),
    scope_id TEXT NOT NULL,
    generation INTEGER NOT NULL CHECK (generation > 0),
    state TEXT NOT NULL,
    payload TEXT NOT NULL,
    created_at TEXT NOT NULL
);

INSERT INTO native_volume_attachments
    (id, project_id, volume_id, server_id, scope_kind, scope_id, generation,
     state, payload, created_at)
SELECT id, project_id, volume_id, server_id, scope_kind, scope_id, generation,
       state, payload, created_at
FROM native_volume_attachments_old;

DROP TABLE native_volume_attachments_old;

CREATE UNIQUE INDEX native_volume_attachments_live_volume
    ON native_volume_attachments (volume_id)
    WHERE state <> 'deleted';
CREATE INDEX native_volume_attachments_project_idx
    ON native_volume_attachments(project_id);
CREATE INDEX native_volume_attachments_server_idx
    ON native_volume_attachments(server_id);
