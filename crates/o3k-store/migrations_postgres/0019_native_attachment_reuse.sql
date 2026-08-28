-- Preserve terminal attachment history while allowing a detached volume to
-- receive a new canonical attachment identity.
ALTER TABLE native_volume_attachments
    DROP CONSTRAINT IF EXISTS native_volume_attachments_volume_id_key;
DROP INDEX IF EXISTS native_volume_attachments_volume_id_key;

CREATE UNIQUE INDEX IF NOT EXISTS native_volume_attachments_live_volume
    ON native_volume_attachments (volume_id)
    WHERE state <> 'deleted';
