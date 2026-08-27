CREATE TABLE canonical_l3_gateways (
    id UUID PRIMARY KEY,
    project_id TEXT NOT NULL,
    name TEXT NOT NULL,
    external_realm_id UUID,
    enable_snat BOOLEAN NOT NULL,
    generation BIGINT NOT NULL CHECK (generation > 0),
    state TEXT NOT NULL CHECK (state IN ('requested', 'active', 'deleting', 'deleted', 'error')),
    UNIQUE (id, project_id),
    FOREIGN KEY (external_realm_id, project_id) REFERENCES canonical_address_realms(id, project_id)
);

CREATE UNIQUE INDEX canonical_address_realms_id_project_idx
    ON canonical_address_realms (id, project_id);

CREATE TABLE canonical_l3_gateway_attachments (
    id UUID PRIMARY KEY,
    gateway_id UUID NOT NULL,
    realm_id UUID NOT NULL,
    project_id TEXT NOT NULL,
    generation BIGINT NOT NULL CHECK (generation > 0),
    state TEXT NOT NULL CHECK (state IN ('requested', 'active', 'deleting', 'deleted', 'error')),
    UNIQUE (gateway_id, realm_id, project_id),
    FOREIGN KEY (gateway_id, project_id) REFERENCES canonical_l3_gateways(id, project_id),
    FOREIGN KEY (realm_id, project_id) REFERENCES canonical_address_realms(id, project_id)
);
CREATE UNIQUE INDEX canonical_l3_gateway_attachments_active_pair
    ON canonical_l3_gateway_attachments (gateway_id, realm_id, project_id)
    WHERE state = 'active';
CREATE INDEX canonical_l3_gateway_attachments_by_realm
    ON canonical_l3_gateway_attachments (project_id, realm_id, state);
