CREATE TABLE IF NOT EXISTS public_address_bindings (
    allocation_id TEXT PRIMARY KEY NOT NULL,
    operation_id TEXT NOT NULL UNIQUE,
    project_id TEXT NOT NULL,
    public_address TEXT NOT NULL UNIQUE,
    endpoint_id TEXT,
    generation INTEGER NOT NULL CHECK (generation > 0)
);
CREATE INDEX IF NOT EXISTS public_address_bindings_project_idx
    ON public_address_bindings(project_id, allocation_id);
CREATE UNIQUE INDEX IF NOT EXISTS public_address_bindings_endpoint_idx
    ON public_address_bindings(project_id, endpoint_id) WHERE endpoint_id IS NOT NULL;
