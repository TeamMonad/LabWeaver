-- Experiment GPU capacity reservations are durable ownership for an Environment
-- instance that has no Work claim. Resource records them here so the observed
-- device pool is not double-counted and two experiments cannot oversubscribe it.
-- A reservation is keyed by environment_id so re-resolving the same instance is
-- idempotent; release transitions the single row without deleting it.
CREATE TABLE resource.environment_gpu_reservations (
    reservation_id uuid PRIMARY KEY,
    environment_id uuid NOT NULL UNIQUE,
    project_id uuid NOT NULL,
    course_id uuid,
    owner_actor_id uuid NOT NULL,
    provider_binding text NOT NULL CHECK (length(trim(provider_binding)) BETWEEN 1 AND 120),
    entry_id uuid NOT NULL REFERENCES resource.gpu_catalog_entries(entry_id),
    allocation_binding text NOT NULL CHECK (length(trim(allocation_binding)) BETWEEN 1 AND 256),
    units integer NOT NULL CHECK (units > 0),
    namespace_name text CHECK (namespace_name IS NULL OR length(trim(namespace_name)) BETWEEN 1 AND 63),
    state text NOT NULL CHECK (state IN ('reserved', 'released')),
    created_at timestamptz NOT NULL DEFAULT now(),
    released_at timestamptz,
    contract jsonb NOT NULL CHECK (jsonb_typeof(contract) = 'object'),
    CHECK ((state = 'released') = (released_at IS NOT NULL))
);

CREATE INDEX environment_gpu_reservations_active_idx
    ON resource.environment_gpu_reservations (allocation_binding, state)
    WHERE state = 'reserved';
