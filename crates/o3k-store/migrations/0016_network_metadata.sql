CREATE TABLE IF NOT EXISTS network_networks (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL,
    project_id TEXT NOT NULL,
    status TEXT NOT NULL,
    UNIQUE (project_id, name)
);

CREATE INDEX IF NOT EXISTS idx_network_networks_project ON network_networks(project_id);

CREATE TABLE IF NOT EXISTS network_subnets (
    id TEXT PRIMARY KEY NOT NULL,
    network_id TEXT NOT NULL REFERENCES network_networks(id),
    name TEXT NOT NULL,
    project_id TEXT NOT NULL,
    cidr TEXT NOT NULL,
    gateway_ip TEXT NOT NULL,
    allocation_start TEXT NOT NULL,
    allocation_end TEXT NOT NULL,
    UNIQUE (network_id, cidr)
);

CREATE INDEX IF NOT EXISTS idx_network_subnets_project ON network_subnets(project_id);
CREATE INDEX IF NOT EXISTS idx_network_subnets_network ON network_subnets(network_id);

CREATE TABLE IF NOT EXISTS network_ports (
    id TEXT PRIMARY KEY NOT NULL,
    network_id TEXT NOT NULL REFERENCES network_networks(id),
    subnet_id TEXT REFERENCES network_subnets(id),
    project_id TEXT NOT NULL,
    name TEXT NOT NULL,
    mac_address TEXT NOT NULL,
    fixed_ip TEXT NOT NULL,
    status TEXT NOT NULL,
    binding_host TEXT,
    binding_state TEXT,
    UNIQUE (subnet_id, fixed_ip),
    UNIQUE (mac_address)
);

CREATE INDEX IF NOT EXISTS idx_network_ports_project ON network_ports(project_id);
CREATE INDEX IF NOT EXISTS idx_network_ports_network ON network_ports(network_id);
