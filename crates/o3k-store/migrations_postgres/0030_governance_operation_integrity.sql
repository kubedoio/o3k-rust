-- Reinstall the operation reference trigger after IAM role assignments became
-- canonical operation resources.
CREATE OR REPLACE FUNCTION o3k_validate_operation_resource_reference()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM resources WHERE id = NEW.resource_id FOR KEY SHARE)
       AND NOT EXISTS (SELECT 1 FROM canonical_networks WHERE id = NEW.resource_id FOR KEY SHARE)
       AND NOT EXISTS (SELECT 1 FROM canonical_address_realms WHERE id = NEW.resource_id FOR KEY SHARE)
       AND NOT EXISTS (SELECT 1 FROM keystone_role_assignments WHERE id = NEW.resource_id FOR KEY SHARE)
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
