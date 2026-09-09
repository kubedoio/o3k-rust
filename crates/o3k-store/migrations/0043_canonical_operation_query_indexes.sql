-- Bounded native Operation collection predicates.  The owner-scope prefix
-- keeps tenant reads selective; operation_id preserves the opaque keyset
-- ordering used by the native cursor.
CREATE INDEX IF NOT EXISTS canonical_operation_metadata_owner_operation_idx
    ON canonical_operation_metadata(owner_scope, operation_id);
CREATE INDEX IF NOT EXISTS canonical_operation_metadata_owner_service_operation_idx
    ON canonical_operation_metadata(owner_scope, service, operation_id);
CREATE INDEX IF NOT EXISTS operations_state_id_idx
    ON operations(state, id);
