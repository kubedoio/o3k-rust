-- Supports the bounded native resource query contract.  The primary key is
-- the deterministic continuation key; keep it in the scoped index so a page
-- never requires an application-side full collection scan.
CREATE INDEX IF NOT EXISTS resources_project_kind_id_idx
    ON resources(project_id, kind, id);
