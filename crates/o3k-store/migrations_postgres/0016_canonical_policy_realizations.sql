CREATE TABLE canonical_policy_realizations (
    endpoint_id TEXT PRIMARY KEY NOT NULL,
    project_id TEXT NOT NULL,
    desired_fingerprint TEXT NOT NULL,
    desired_generation BIGINT NOT NULL CHECK (desired_generation > 0),
    observed_fingerprint TEXT,
    observed_generation BIGINT CHECK (observed_generation IS NULL OR observed_generation > 0),
    state VARCHAR(16) NOT NULL CHECK (state IN ('pending', 'applying', 'realized', 'failed', 'unknown')),
    provider_resource_id TEXT,
    last_outcome TEXT,
    FOREIGN KEY (endpoint_id, project_id) REFERENCES canonical_endpoints(id, project_id) ON DELETE CASCADE
);
CREATE INDEX canonical_policy_realizations_project_idx
    ON canonical_policy_realizations(project_id);
