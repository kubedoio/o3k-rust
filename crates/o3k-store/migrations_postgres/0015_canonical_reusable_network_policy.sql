CREATE UNIQUE INDEX canonical_endpoints_id_project_idx
    ON canonical_endpoints(id, project_id);

CREATE TABLE canonical_reusable_network_policies (
    id TEXT PRIMARY KEY NOT NULL,
    project_id TEXT NOT NULL,
    name TEXT NOT NULL,
    description TEXT NOT NULL,
    stateful_mode VARCHAR(16) NOT NULL CHECK (stateful_mode IN ('Stateful', 'Stateless')),
    unmatched_action VARCHAR(16) NOT NULL CHECK (unmatched_action IN ('Allow', 'Deny')),
    generation BIGINT NOT NULL CHECK (generation > 0),
    state VARCHAR(16) NOT NULL CHECK (state IN ('requested', 'active', 'deleting', 'deleted', 'error')),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE (id, project_id)
);
CREATE INDEX canonical_reusable_network_policies_project_idx
    ON canonical_reusable_network_policies(project_id);

CREATE TABLE canonical_network_policy_rules (
    id TEXT PRIMARY KEY NOT NULL,
    policy_id TEXT NOT NULL REFERENCES canonical_reusable_network_policies(id),
    project_id TEXT NOT NULL,
    direction VARCHAR(16) NOT NULL CHECK (direction IN ('Ingress', 'Egress')),
    address_family VARCHAR(16) NOT NULL CHECK (address_family IN ('Ipv4', 'Ipv6')),
    protocol VARCHAR(16) NOT NULL CHECK (protocol IN ('Any', 'Tcp', 'Udp', 'Icmp')),
    port_min INTEGER,
    port_max INTEGER,
    remote_selector CIDR,
    action VARCHAR(16) NOT NULL CHECK (action IN ('Allow', 'Deny')),
    state VARCHAR(16) NOT NULL CHECK (state IN ('requested', 'active', 'deleting', 'deleted', 'error')),
    generation BIGINT NOT NULL CHECK (generation > 0),
    enforcement_key TEXT NOT NULL,
    FOREIGN KEY (policy_id, project_id) REFERENCES canonical_reusable_network_policies(id, project_id),
    CHECK ((port_min IS NULL AND port_max IS NULL) OR
           (port_min BETWEEN 0 AND 65535 AND port_max BETWEEN 0 AND 65535 AND port_min <= port_max))
);
CREATE INDEX canonical_network_policy_rules_policy_idx
    ON canonical_network_policy_rules(policy_id);
CREATE UNIQUE INDEX canonical_network_policy_rules_active_key
    ON canonical_network_policy_rules(policy_id, enforcement_key)
    WHERE state = 'active';

CREATE TABLE canonical_policy_attachments (
    id TEXT PRIMARY KEY NOT NULL,
    policy_id TEXT NOT NULL REFERENCES canonical_reusable_network_policies(id),
    endpoint_id TEXT NOT NULL REFERENCES canonical_endpoints(id),
    project_id TEXT NOT NULL,
    state VARCHAR(16) NOT NULL CHECK (state IN ('requested', 'active', 'deleting', 'deleted', 'error')),
    generation BIGINT NOT NULL CHECK (generation > 0),
    FOREIGN KEY (policy_id, project_id) REFERENCES canonical_reusable_network_policies(id, project_id),
    FOREIGN KEY (endpoint_id, project_id) REFERENCES canonical_endpoints(id, project_id)
);
CREATE INDEX canonical_policy_attachments_policy_idx
    ON canonical_policy_attachments(policy_id);
CREATE INDEX canonical_policy_attachments_endpoint_idx
    ON canonical_policy_attachments(endpoint_id);
CREATE UNIQUE INDEX canonical_policy_attachments_active_pair
    ON canonical_policy_attachments(policy_id, endpoint_id)
    WHERE state = 'active';
