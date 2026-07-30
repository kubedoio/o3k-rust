CREATE TABLE provider_refs (
    resource_id TEXT NOT NULL REFERENCES resources(id) ON DELETE CASCADE,
    provider_name TEXT NOT NULL,
    provider_resource_id TEXT NOT NULL,
    PRIMARY KEY (resource_id, provider_name),
    UNIQUE (provider_name, provider_resource_id)
);
