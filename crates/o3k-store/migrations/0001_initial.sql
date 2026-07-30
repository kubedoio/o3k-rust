CREATE TABLE resources (
    id TEXT PRIMARY KEY NOT NULL,
    kind TEXT NOT NULL,
    project_id TEXT NOT NULL,
    generation INTEGER NOT NULL CHECK (generation > 0),
    observed_generation INTEGER NOT NULL CHECK (observed_generation >= 0),
    desired_state TEXT NOT NULL,
    observed_state TEXT NOT NULL,
    provider_id TEXT,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX resources_project_kind_idx ON resources(project_id, kind);

CREATE TABLE operations (
    id TEXT PRIMARY KEY NOT NULL,
    resource_id TEXT NOT NULL REFERENCES resources(id) ON DELETE CASCADE,
    state TEXT NOT NULL,
    provider_operation_id TEXT,
    error_category TEXT,
    error_message TEXT,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX operations_resource_idx ON operations(resource_id);
