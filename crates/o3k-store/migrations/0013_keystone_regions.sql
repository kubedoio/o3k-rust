CREATE TABLE IF NOT EXISTS keystone_regions (
    id TEXT PRIMARY KEY,
    description TEXT,
    parent_region_id TEXT,
    enabled INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL
);
