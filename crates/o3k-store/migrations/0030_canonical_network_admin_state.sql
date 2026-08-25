ALTER TABLE canonical_networks
    ADD COLUMN admin_state_up INTEGER NOT NULL DEFAULT 1;
