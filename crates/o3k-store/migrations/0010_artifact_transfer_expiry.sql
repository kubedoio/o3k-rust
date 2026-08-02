ALTER TABLE artifact_transfers
    ADD COLUMN expires_at_unix_ms INTEGER CHECK (expires_at_unix_ms > 0);

-- Older rows have no authenticated expiry. Mark them expired rather than
-- inventing a new authorization window during recovery.
UPDATE artifact_transfers
SET expires_at_unix_ms = 1,
    state = 'expired'
WHERE expires_at_unix_ms IS NULL;
