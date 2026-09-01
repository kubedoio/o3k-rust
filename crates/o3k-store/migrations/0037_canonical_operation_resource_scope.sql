-- no transaction
-- Canonical Network/AddressRealm operations use service-owned rows rather
-- than the generic resources index. Keep the shared operations table, but do
-- not require its resource_id to be a generic-resource foreign key.
PRAGMA foreign_keys = OFF;

CREATE TABLE operations_without_resource_fk (
    id TEXT PRIMARY KEY NOT NULL,
    resource_id TEXT NOT NULL,
    kind TEXT NOT NULL DEFAULT 'create',
    state TEXT NOT NULL,
    provider_operation_id TEXT,
    error_category TEXT,
    error_message TEXT,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

INSERT INTO operations_without_resource_fk
    (id, resource_id, kind, state, provider_operation_id, error_category, error_message, created_at)
SELECT id, resource_id, kind, state, provider_operation_id, error_category, error_message, created_at
FROM operations;

DROP TABLE operations;
ALTER TABLE operations_without_resource_fk RENAME TO operations;
CREATE INDEX operations_resource_idx ON operations(resource_id);

-- Preserve the former generic-resource cascade for ordinary operations. A
-- canonical scoped operation is retained as durable lifecycle evidence.
CREATE TRIGGER resources_delete_generic_operations
AFTER DELETE ON resources
BEGIN
    DELETE FROM operations
    WHERE resource_id = OLD.id
      AND NOT EXISTS (
          SELECT 1 FROM canonical_operation_metadata metadata
          WHERE metadata.operation_id = operations.id
            AND metadata.resource_type IN ('network:network', 'network:address_realm')
      );
END;

PRAGMA foreign_keys = ON;
