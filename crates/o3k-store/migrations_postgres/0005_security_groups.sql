CREATE TABLE IF NOT EXISTS network_security_groups (
    id VARCHAR(36) PRIMARY KEY NOT NULL,
    project_id VARCHAR(255) NOT NULL,
    name VARCHAR(255) NOT NULL,
    description TEXT NOT NULL,
    UNIQUE (project_id, name)
);
CREATE TABLE IF NOT EXISTS network_security_group_rules (
    id VARCHAR(36) PRIMARY KEY NOT NULL,
    security_group_id VARCHAR(36) NOT NULL REFERENCES network_security_groups(id) ON DELETE CASCADE,
    project_id VARCHAR(255) NOT NULL,
    direction VARCHAR(16) NOT NULL,
    protocol VARCHAR(16) NOT NULL,
    port_min INTEGER,
    port_max INTEGER,
    remote_ip_prefix INET
);
CREATE TABLE IF NOT EXISTS network_security_group_bindings (
    project_id VARCHAR(255) NOT NULL,
    endpoint_id VARCHAR(36) NOT NULL,
    security_group_id VARCHAR(36) NOT NULL REFERENCES network_security_groups(id) ON DELETE CASCADE,
    PRIMARY KEY (project_id, endpoint_id, security_group_id)
);
CREATE INDEX IF NOT EXISTS idx_network_security_group_rules_group ON network_security_group_rules(security_group_id);
CREATE INDEX IF NOT EXISTS idx_network_security_group_bindings_endpoint ON network_security_group_bindings(project_id, endpoint_id);
