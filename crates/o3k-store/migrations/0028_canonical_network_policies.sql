CREATE TABLE canonical_network_policies (
    id TEXT PRIMARY KEY NOT NULL,
    project_id TEXT NOT NULL,
    endpoint_id TEXT NOT NULL REFERENCES canonical_endpoints(id),
    direction TEXT NOT NULL CHECK (direction IN ('Ingress', 'Egress')),
    protocol TEXT NOT NULL CHECK (protocol IN ('Any', 'Tcp', 'Udp', 'Icmp')),
    port_min INTEGER,
    port_max INTEGER,
    source TEXT,
    destination TEXT,
    action TEXT NOT NULL CHECK (action IN ('Allow', 'Deny')),
    generation INTEGER NOT NULL CHECK (generation > 0),
    state TEXT NOT NULL CHECK (state IN ('active', 'deleting', 'deleted', 'error')),
    CHECK ((port_min IS NULL AND port_max IS NULL) OR (port_min BETWEEN 0 AND 65535 AND port_max BETWEEN 0 AND 65535 AND port_min <= port_max))
);

CREATE INDEX canonical_network_policies_project_idx
    ON canonical_network_policies(project_id);
CREATE INDEX canonical_network_policies_endpoint_idx
    ON canonical_network_policies(endpoint_id);
