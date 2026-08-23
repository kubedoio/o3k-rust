CREATE TABLE resource_relationships (
    parent_resource_id TEXT NOT NULL REFERENCES resources(id) ON DELETE CASCADE,
    parent_resource_type TEXT NOT NULL,
    slot TEXT NOT NULL,
    expected_child_resource_type TEXT NOT NULL,
    child_resource_id TEXT,
    ownership TEXT NOT NULL CHECK (ownership IN ('exclusive', 'referenced')),
    parent_operation_id TEXT NOT NULL,
    child_operation_id TEXT,
    owner_scope TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('reserved', 'bound', 'deleting', 'deleted', 'unknown')),
    fingerprint TEXT NOT NULL,
    PRIMARY KEY (parent_resource_id, slot)
);
CREATE INDEX resource_relationships_child_idx ON resource_relationships(child_resource_id);
