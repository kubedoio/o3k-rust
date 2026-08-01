CREATE TABLE observation_watermarks (
    resource_id TEXT PRIMARY KEY NOT NULL REFERENCES resources(id) ON DELETE CASCADE,
    agent_epoch TEXT NOT NULL,
    observation_sequence INTEGER NOT NULL CHECK (observation_sequence >= 0)
);
