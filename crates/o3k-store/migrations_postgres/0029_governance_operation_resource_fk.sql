-- Canonical IAM role assignments are authoritative resources in their own
-- table. Operations must be able to reference them without fabricating a
-- generic resources row; ownership is checked by the scoped-operation query.
ALTER TABLE operations DROP CONSTRAINT IF EXISTS operations_resource_id_fkey;
