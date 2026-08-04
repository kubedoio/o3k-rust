ALTER TABLE volume_attachments ADD COLUMN status TEXT NOT NULL DEFAULT 'validated';
ALTER TABLE volume_attachments ADD COLUMN operation_id TEXT;
ALTER TABLE volume_attachments ADD COLUMN idempotency_key TEXT;
ALTER TABLE volume_attachments ADD COLUMN cinder_attachment_id TEXT;
ALTER TABLE volume_attachments ADD COLUMN connector_host TEXT;
ALTER TABLE volume_attachments ADD COLUMN connector_ip TEXT;
ALTER TABLE volume_attachments ADD COLUMN connector_initiator TEXT;
ALTER TABLE volume_attachments ADD COLUMN driver_volume_type TEXT;
ALTER TABLE volume_attachments ADD COLUMN target_iqn TEXT;
ALTER TABLE volume_attachments ADD COLUMN target_portal TEXT;
ALTER TABLE volume_attachments ADD COLUMN target_lun INTEGER;
ALTER TABLE volume_attachments ADD COLUMN connection_info_digest TEXT;
ALTER TABLE volume_attachments ADD COLUMN error TEXT;

CREATE INDEX IF NOT EXISTS idx_volume_attachments_status ON volume_attachments(status);
CREATE INDEX IF NOT EXISTS idx_volume_attachments_idempotency ON volume_attachments(idempotency_key);
