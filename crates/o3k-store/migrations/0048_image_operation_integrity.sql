-- Image metadata is a canonical operation resource.  Reinstall the SQLite
-- operation-reference trigger so existing databases converge with fresh
-- databases and reject no valid image operation at the generic trigger.
DROP TRIGGER IF EXISTS operations_validate_resource_reference;
CREATE TRIGGER operations_validate_resource_reference
BEFORE INSERT ON operations
BEGIN
    SELECT RAISE(ABORT, 'operation resource not found')
    WHERE NOT EXISTS (SELECT 1 FROM resources WHERE id = NEW.resource_id)
      AND NOT EXISTS (SELECT 1 FROM canonical_networks WHERE id = NEW.resource_id)
      AND NOT EXISTS (SELECT 1 FROM canonical_address_realms WHERE id = NEW.resource_id)
      AND NOT EXISTS (SELECT 1 FROM keystone_role_assignments WHERE id = NEW.resource_id)
      AND NOT EXISTS (SELECT 1 FROM image_metadata WHERE id = NEW.resource_id);
END;

DROP TRIGGER IF EXISTS resources_delete_generic_operations;
CREATE TRIGGER resources_delete_generic_operations
AFTER DELETE ON resources
BEGIN
    DELETE FROM operations
    WHERE resource_id = OLD.id
      AND NOT EXISTS (
          SELECT 1 FROM canonical_operation_metadata metadata
          WHERE metadata.operation_id = operations.id
            AND metadata.resource_type IN (
                'network:network', 'network:address_realm',
                'iam:role_assignment', 'image:image'
            )
      );
END;
