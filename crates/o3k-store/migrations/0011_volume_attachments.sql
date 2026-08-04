CREATE TABLE IF NOT EXISTS volume_attachments (
    id TEXT PRIMARY KEY,
    server_id TEXT NOT NULL,
    volume_id TEXT NOT NULL,
    device TEXT NOT NULL,
    tag TEXT,
    delete_on_termination INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    FOREIGN KEY(server_id) REFERENCES resources(id) ON DELETE CASCADE
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_volume_attachments_volume ON volume_attachments(volume_id);
CREATE INDEX IF NOT EXISTS idx_volume_attachments_server ON volume_attachments(server_id);
