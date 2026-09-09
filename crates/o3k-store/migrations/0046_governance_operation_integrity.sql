-- IAM role assignments are canonical operation resources and must satisfy the
-- same existence invariant as generic and network-owned resources.
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
