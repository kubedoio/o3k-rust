-- =====================================================================
-- O3K PostgreSQL Schema: Exact Parity with SQLite 0001 - 0018
-- =====================================================================

-- Resources and Operations (0001, 0003, 0007)
CREATE TABLE IF NOT EXISTS resources (
    id TEXT PRIMARY KEY NOT NULL,
    kind TEXT NOT NULL,
    project_id TEXT NOT NULL,
    generation BIGINT NOT NULL CHECK (generation > 0),
    observed_generation BIGINT NOT NULL CHECK (observed_generation >= 0),
    desired_state TEXT NOT NULL,
    observed_state TEXT NOT NULL,
    provider_id TEXT,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX IF NOT EXISTS resources_project_kind_idx ON resources(project_id, kind);

CREATE TABLE IF NOT EXISTS operations (
    id TEXT PRIMARY KEY NOT NULL,
    resource_id TEXT NOT NULL REFERENCES resources(id) ON DELETE CASCADE,
    kind TEXT,
    state TEXT NOT NULL,
    provider_operation_id TEXT,
    error_category TEXT,
    error_message TEXT,
    retry_count INT NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX IF NOT EXISTS operations_resource_idx ON operations(resource_id);

CREATE TABLE IF NOT EXISTS operation_retry_state (
    operation_id TEXT PRIMARY KEY NOT NULL REFERENCES operations(id) ON DELETE CASCADE,
    attempts BIGINT NOT NULL,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- Provider references (0002)
CREATE TABLE IF NOT EXISTS provider_refs (
    resource_id TEXT NOT NULL REFERENCES resources(id) ON DELETE CASCADE,
    provider_name TEXT NOT NULL,
    provider_resource_id TEXT NOT NULL,
    PRIMARY KEY (resource_id, provider_name),
    UNIQUE (provider_name, provider_resource_id)
);

-- Observation Watermarks (0004)
CREATE TABLE IF NOT EXISTS observation_watermarks (
    resource_id TEXT PRIMARY KEY NOT NULL REFERENCES resources(id) ON DELETE CASCADE,
    agent_epoch TEXT NOT NULL,
    observation_sequence BIGINT NOT NULL CHECK (observation_sequence >= 0)
);

-- Keypairs and Server Keypairs (0005)
CREATE TABLE IF NOT EXISTS keypairs (
    id TEXT PRIMARY KEY NOT NULL,
    user_id TEXT NOT NULL,
    project_id TEXT NOT NULL,
    name TEXT NOT NULL,
    key_type TEXT NOT NULL,
    public_key TEXT NOT NULL,
    fingerprint TEXT NOT NULL,
    created_at TEXT NOT NULL,
    UNIQUE (user_id, project_id, name)
);

CREATE TABLE IF NOT EXISTS server_keypairs (
    server_id TEXT PRIMARY KEY NOT NULL REFERENCES resources(id) ON DELETE CASCADE,
    keypair_id TEXT NOT NULL REFERENCES keypairs(id) ON DELETE CASCADE,
    created_at TEXT NOT NULL
);

-- Agent Commands (0006)
CREATE TABLE IF NOT EXISTS agent_commands (
    command_id TEXT PRIMARY KEY NOT NULL CHECK (length(command_id) BETWEEN 1 AND 128),
    idempotency_key TEXT NOT NULL UNIQUE CHECK (length(idempotency_key) BETWEEN 1 AND 128),
    operation_id TEXT NOT NULL REFERENCES operations(id) ON DELETE CASCADE,
    resource_id TEXT NOT NULL REFERENCES resources(id) ON DELETE CASCADE,
    agent_id TEXT NOT NULL CHECK (length(agent_id) BETWEEN 1 AND 128),
    agent_epoch TEXT NOT NULL CHECK (length(agent_epoch) BETWEEN 1 AND 256),
    payload_fingerprint_sha256 TEXT NOT NULL CHECK (length(payload_fingerprint_sha256) = 64),
    payload BYTEA NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('pending', 'accepted', 'running', 'succeeded', 'retryable', 'unknown_outcome', 'failed')),
    accepted_sequence BIGINT NOT NULL DEFAULT 0,
    last_sequence BIGINT NOT NULL DEFAULT 0,
    provider_operation_id TEXT,
    provider_resource_id TEXT,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX IF NOT EXISTS agent_commands_operation_idx ON agent_commands(operation_id);
CREATE INDEX IF NOT EXISTS agent_commands_resource_idx ON agent_commands(resource_id);

-- Artifact Transfers (0008, 0010)
CREATE TABLE IF NOT EXISTS artifact_transfers (
    transfer_id TEXT PRIMARY KEY NOT NULL CHECK (length(transfer_id) BETWEEN 1 AND 128),
    command_id TEXT NOT NULL CHECK (length(command_id) BETWEEN 1 AND 128),
    operation_id TEXT NOT NULL REFERENCES operations(id) ON DELETE CASCADE,
    resource_id TEXT NOT NULL REFERENCES resources(id) ON DELETE CASCADE,
    agent_id TEXT NOT NULL CHECK (length(agent_id) BETWEEN 1 AND 128),
    agent_epoch TEXT NOT NULL CHECK (length(agent_epoch) BETWEEN 1 AND 256),
    artifact_id TEXT NOT NULL CHECK (length(artifact_id) BETWEEN 1 AND 256),
    artifact_kind TEXT NOT NULL CHECK (length(artifact_kind) BETWEEN 1 AND 64),
    sha256 TEXT NOT NULL CHECK (length(sha256) = 64),
    size_bytes BIGINT NOT NULL CHECK (size_bytes BETWEEN 1 AND 67108864),
    expires_at_unix_ms BIGINT CHECK (expires_at_unix_ms > 0),
    format TEXT NOT NULL CHECK (length(format) BETWEEN 1 AND 32),
    chunk_size_bytes BIGINT NOT NULL CHECK (chunk_size_bytes BETWEEN 1 AND 262144),
    chunk_count BIGINT NOT NULL CHECK (chunk_count BETWEEN 1 AND 67108864),
    state TEXT NOT NULL CHECK (state IN ('offered', 'receiving', 'committed', 'rejected', 'expired')),
    contiguous_bytes BIGINT NOT NULL DEFAULT 0 CHECK (contiguous_bytes BETWEEN 0 AND 67108864),
    next_chunk_index BIGINT NOT NULL DEFAULT 0 CHECK (next_chunk_index BETWEEN 0 AND 67108864),
    retry_count INT NOT NULL DEFAULT 0 CHECK (retry_count BETWEEN 0 AND 16),
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CHECK (contiguous_bytes <= size_bytes),
    CHECK (next_chunk_index <= chunk_count),
    CHECK (chunk_count = ((size_bytes + chunk_size_bytes - 1) / chunk_size_bytes))
);

CREATE INDEX IF NOT EXISTS artifact_transfers_operation_idx ON artifact_transfers(operation_id);
CREATE INDEX IF NOT EXISTS artifact_transfers_agent_epoch_idx ON artifact_transfers(agent_id, agent_epoch);
CREATE INDEX IF NOT EXISTS artifact_transfers_recovery_idx ON artifact_transfers(state, updated_at);

-- Image Overlay Ownership (0009)
CREATE TABLE IF NOT EXISTS image_overlay_ownership (
    overlay_id TEXT PRIMARY KEY NOT NULL CHECK (length(overlay_id) BETWEEN 1 AND 128),
    resource_id TEXT NOT NULL REFERENCES resources(id) ON DELETE CASCADE,
    operation_id TEXT NOT NULL REFERENCES operations(id) ON DELETE CASCADE,
    command_id TEXT NOT NULL CHECK (length(command_id) BETWEEN 1 AND 128),
    agent_id TEXT NOT NULL CHECK (length(agent_id) BETWEEN 1 AND 128),
    agent_epoch TEXT NOT NULL CHECK (length(agent_epoch) BETWEEN 1 AND 256),
    base_sha256 TEXT NOT NULL CHECK (length(base_sha256) = 64),
    base_format TEXT NOT NULL CHECK (base_format IN ('raw', 'qcow2')),
    overlay_format TEXT NOT NULL CHECK (overlay_format = 'qcow2'),
    state TEXT NOT NULL CHECK (state IN ('pending', 'materializing', 'ready', 'deleting', 'deleted', 'failed')),
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(resource_id, operation_id, command_id)
);

CREATE INDEX IF NOT EXISTS image_overlay_ownership_resource_idx ON image_overlay_ownership(resource_id, state);
CREATE INDEX IF NOT EXISTS image_overlay_ownership_base_ref_idx ON image_overlay_ownership(base_sha256, base_format, state);
CREATE INDEX IF NOT EXISTS image_overlay_ownership_recovery_idx ON image_overlay_ownership(state, updated_at);

-- Volume Attachments (0011, 0014)
CREATE TABLE IF NOT EXISTS volume_attachments (
    id TEXT PRIMARY KEY,
    server_id TEXT NOT NULL REFERENCES resources(id) ON DELETE CASCADE,
    volume_id TEXT NOT NULL,
    device TEXT NOT NULL,
    tag TEXT,
    delete_on_termination INT NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'validated',
    operation_id TEXT,
    idempotency_key TEXT,
    cinder_attachment_id TEXT,
    connector_host TEXT,
    connector_ip TEXT,
    connector_initiator TEXT,
    driver_volume_type TEXT,
    target_iqn TEXT,
    target_portal TEXT,
    target_lun INT,
    connection_info_digest TEXT,
    error TEXT
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_volume_attachments_volume ON volume_attachments(volume_id);
CREATE INDEX IF NOT EXISTS idx_volume_attachments_server ON volume_attachments(server_id);
CREATE INDEX IF NOT EXISTS idx_volume_attachments_status ON volume_attachments(status);
CREATE INDEX IF NOT EXISTS idx_volume_attachments_idempotency ON volume_attachments(idempotency_key);

-- Keystone Identity (0012, 0013)
CREATE TABLE IF NOT EXISTS keystone_domains (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    description TEXT,
    enabled INT NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS keystone_projects (
    id TEXT PRIMARY KEY,
    domain_id TEXT NOT NULL REFERENCES keystone_domains(id),
    name TEXT NOT NULL,
    description TEXT,
    enabled INT NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL,
    UNIQUE(domain_id, name)
);

CREATE TABLE IF NOT EXISTS keystone_users (
    id TEXT PRIMARY KEY,
    domain_id TEXT NOT NULL REFERENCES keystone_domains(id),
    name TEXT NOT NULL UNIQUE,
    password_hash TEXT NOT NULL,
    email TEXT,
    enabled INT NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS keystone_roles (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    description TEXT,
    created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS keystone_role_assignments (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES keystone_users(id),
    project_id TEXT NOT NULL REFERENCES keystone_projects(id),
    role_id TEXT NOT NULL REFERENCES keystone_roles(id),
    created_at TEXT NOT NULL,
    UNIQUE(user_id, project_id, role_id)
);

CREATE TABLE IF NOT EXISTS keystone_services (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    type TEXT NOT NULL,
    description TEXT,
    enabled INT NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS keystone_endpoints (
    id TEXT PRIMARY KEY,
    service_id TEXT NOT NULL REFERENCES keystone_services(id),
    interface TEXT NOT NULL,
    url TEXT NOT NULL,
    region TEXT NOT NULL DEFAULT 'RegionOne',
    enabled INT NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS keystone_regions (
    id TEXT PRIMARY KEY,
    description TEXT,
    parent_region_id TEXT,
    enabled INT NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL
);

-- Image Metadata (0015)
CREATE TABLE IF NOT EXISTS image_metadata (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL,
    project_id TEXT NOT NULL,
    status TEXT NOT NULL,
    visibility TEXT NOT NULL,
    container_format TEXT NOT NULL,
    disk_format TEXT NOT NULL,
    size BIGINT,
    checksum TEXT
);

CREATE INDEX IF NOT EXISTS idx_image_metadata_project ON image_metadata(project_id);

-- Network Metadata (0016)
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

-- Placement (0017)
CREATE TABLE IF NOT EXISTS placement_providers (
    id TEXT PRIMARY KEY NOT NULL,
    node_id TEXT NOT NULL UNIQUE,
    state TEXT NOT NULL,
    generation BIGINT NOT NULL
);

CREATE TABLE IF NOT EXISTS placement_inventories (
    provider_id TEXT NOT NULL REFERENCES placement_providers(id) ON DELETE CASCADE,
    resource_class TEXT NOT NULL,
    total BIGINT NOT NULL,
    reserved BIGINT NOT NULL,
    allocation_ratio DOUBLE PRECISION NOT NULL,
    used BIGINT NOT NULL,
    PRIMARY KEY (provider_id, resource_class)
);

CREATE TABLE IF NOT EXISTS placement_allocations (
    id TEXT PRIMARY KEY NOT NULL,
    provider_id TEXT NOT NULL REFERENCES placement_providers(id) ON DELETE CASCADE,
    consumer_id TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_placement_allocations_provider ON placement_allocations(provider_id);
CREATE INDEX IF NOT EXISTS idx_placement_allocations_consumer ON placement_allocations(consumer_id);

CREATE TABLE IF NOT EXISTS placement_allocation_resources (
    allocation_id TEXT NOT NULL REFERENCES placement_allocations(id) ON DELETE CASCADE,
    resource_class TEXT NOT NULL,
    amount BIGINT NOT NULL,
    PRIMARY KEY (allocation_id, resource_class)
);

CREATE TABLE IF NOT EXISTS placement_allocation_intents (
    id TEXT PRIMARY KEY NOT NULL,
    provider_id TEXT NOT NULL REFERENCES placement_providers(id) ON DELETE CASCADE,
    consumer_id TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_placement_intents_provider ON placement_allocation_intents(provider_id);

CREATE TABLE IF NOT EXISTS placement_allocation_intent_resources (
    intent_id TEXT NOT NULL REFERENCES placement_allocation_intents(id) ON DELETE CASCADE,
    resource_class TEXT NOT NULL,
    amount BIGINT NOT NULL,
    PRIMARY KEY (intent_id, resource_class)
);

-- Quotas (0018)
CREATE TABLE IF NOT EXISTS quota_limits (
    scope_id TEXT NOT NULL,
    scope_kind TEXT NOT NULL,
    namespace TEXT NOT NULL,
    resource TEXT NOT NULL,
    limit_value BIGINT,
    PRIMARY KEY (scope_id, scope_kind, namespace, resource)
);

CREATE TABLE IF NOT EXISTS quota_reservations (
    id TEXT PRIMARY KEY NOT NULL,
    scope_id TEXT NOT NULL,
    scope_kind TEXT NOT NULL,
    operation_id TEXT NOT NULL UNIQUE,
    state TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_quota_res_scope ON quota_reservations(scope_id, state);
CREATE INDEX IF NOT EXISTS idx_quota_res_op ON quota_reservations(operation_id);

CREATE TABLE IF NOT EXISTS quota_reservation_amounts (
    reservation_id TEXT NOT NULL REFERENCES quota_reservations(id) ON DELETE CASCADE,
    namespace TEXT NOT NULL,
    resource TEXT NOT NULL,
    amount BIGINT NOT NULL,
    PRIMARY KEY (reservation_id, namespace, resource)
);
