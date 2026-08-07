CREATE TABLE IF NOT EXISTS image_metadata (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL,
    project_id TEXT NOT NULL,
    status TEXT NOT NULL,
    visibility TEXT NOT NULL,
    container_format TEXT NOT NULL,
    disk_format TEXT NOT NULL,
    size INTEGER,
    checksum TEXT
);

CREATE INDEX IF NOT EXISTS idx_image_metadata_project ON image_metadata(project_id);
