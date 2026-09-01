-- Canonical Network/AddressRealm operations use service-owned rows rather
-- than the generic resources index. Keep the shared operations table, but do
-- not require its resource_id to be a generic-resource foreign key.
ALTER TABLE operations DROP CONSTRAINT IF EXISTS operations_resource_id_fkey;
