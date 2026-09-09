-- Image metadata is a canonical operation resource.  Reinstall the shared
-- operation-reference trigger after the image repository became durable so
-- existing PostgreSQL databases and fresh installs have identical authority.
CREATE OR REPLACE FUNCTION o3k_validate_operation_resource_reference()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM resources WHERE id = NEW.resource_id FOR KEY SHARE)
       AND NOT EXISTS (SELECT 1 FROM canonical_networks WHERE id = NEW.resource_id FOR KEY SHARE)
       AND NOT EXISTS (SELECT 1 FROM canonical_address_realms WHERE id = NEW.resource_id FOR KEY SHARE)
       AND NOT EXISTS (SELECT 1 FROM keystone_role_assignments WHERE id = NEW.resource_id FOR KEY SHARE)
       AND NOT EXISTS (SELECT 1 FROM image_metadata WHERE id = NEW.resource_id FOR KEY SHARE)
    THEN
        RAISE EXCEPTION 'operation resource not found';
    END IF;
    RETURN NEW;
END;
$$;

DROP TRIGGER IF EXISTS operations_validate_resource_reference ON operations;
CREATE TRIGGER operations_validate_resource_reference
BEFORE INSERT ON operations
FOR EACH ROW
EXECUTE FUNCTION o3k_validate_operation_resource_reference();

CREATE OR REPLACE FUNCTION o3k_delete_generic_resource_operations()
RETURNS trigger
LANGUAGE plpgsql
AS $$
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
    RETURN OLD;
END;
$$;

DROP TRIGGER IF EXISTS resources_delete_generic_operations ON resources;
CREATE TRIGGER resources_delete_generic_operations
AFTER DELETE ON resources
FOR EACH ROW
EXECUTE FUNCTION o3k_delete_generic_resource_operations();
