-- Canonical scoped operations intentionally do not use a generic-resource FK.
-- Keep the same existence invariant with a trigger for both generic and
-- service-owned canonical resources.
CREATE TRIGGER operations_validate_resource_reference
BEFORE INSERT ON operations
BEGIN
    SELECT RAISE(ABORT, 'operation resource not found')
    WHERE NOT EXISTS (SELECT 1 FROM resources WHERE id = NEW.resource_id)
      AND NOT EXISTS (SELECT 1 FROM canonical_networks WHERE id = NEW.resource_id)
      AND NOT EXISTS (SELECT 1 FROM canonical_address_realms WHERE id = NEW.resource_id);
END;
