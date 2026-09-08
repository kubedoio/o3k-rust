-- Supports the bounded native resource query contract.
CREATE INDEX IF NOT EXISTS resources_project_kind_id_idx
    ON resources(project_id, kind, id);
