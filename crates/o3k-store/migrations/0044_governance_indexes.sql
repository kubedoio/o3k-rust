-- Bounded native IAM governance collections. These support keyset pagination
-- and filtered assignment listing pushed into SQL.
CREATE INDEX IF NOT EXISTS idx_keystone_projects_domain_id
    ON keystone_projects(domain_id, id);
CREATE INDEX IF NOT EXISTS idx_keystone_role_assignments_principal_id
    ON keystone_role_assignments(user_id, id);
CREATE INDEX IF NOT EXISTS idx_keystone_role_assignments_project_id
    ON keystone_role_assignments(project_id, id);
CREATE INDEX IF NOT EXISTS idx_keystone_role_assignments_role_id
    ON keystone_role_assignments(role_id, id);
