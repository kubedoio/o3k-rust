CREATE TABLE IF NOT EXISTS network_security_groups (
    id TEXT PRIMARY KEY NOT NULL,
    project_id TEXT NOT NULL,
    name TEXT NOT NULL,
    description TEXT NOT NULL,
    UNIQUE (project_id, name)
);
CREATE TABLE IF NOT EXISTS network_security_group_rules (
    id TEXT PRIMARY KEY NOT NULL,
    security_group_id TEXT NOT NULL REFERENCES network_security_groups(id) ON DELETE CASCADE,
    project_id TEXT NOT NULL,
    direction TEXT NOT NULL,
    protocol TEXT NOT NULL,
    port_min INTEGER,
    port_max INTEGER,
    remote_ip_prefix TEXT
);
CREATE TABLE IF NOT EXISTS network_security_group_bindings (
    project_id TEXT NOT NULL,
    endpoint_id TEXT NOT NULL,
    security_group_id TEXT NOT NULL REFERENCES network_security_groups(id) ON DELETE CASCADE,
    PRIMARY KEY (project_id, endpoint_id, security_group_id)
);
CREATE INDEX IF NOT EXISTS idx_network_security_group_rules_group ON network_security_group_rules(security_group_id);
CREATE INDEX IF NOT EXISTS idx_network_security_group_bindings_endpoint ON network_security_group_bindings(project_id, endpoint_id);
