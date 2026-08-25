ALTER TABLE canonical_networks
    ADD COLUMN admin_state_up BOOLEAN NOT NULL DEFAULT TRUE;
