-- Fence Environment stop/start operations while preserving the original Resource allocation.
ALTER TABLE resource.environment_resource_reservations
    ADD COLUMN reservation_generation bigint NOT NULL DEFAULT 1
        CHECK (reservation_generation > 0),
    ADD COLUMN environment_generation bigint NOT NULL DEFAULT 1
        CHECK (environment_generation > 0),
    ADD COLUMN operation_id uuid,
    ADD COLUMN suspended_at timestamptz;

ALTER TABLE resource.environment_resource_reservations
    DROP CONSTRAINT IF EXISTS environment_resource_reservations_state_check,
    DROP CONSTRAINT IF EXISTS environment_gpu_reservations_state_check,
    ADD CONSTRAINT environment_resource_reservations_state_check
        CHECK (state IN ('reserved', 'suspended', 'released')),
    ADD CONSTRAINT environment_resource_reservations_suspended_at_check
        CHECK ((state = 'suspended') = (suspended_at IS NOT NULL));

UPDATE resource.environment_resource_reservations
SET contract = contract || jsonb_build_object(
    'reservationGeneration', reservation_generation,
    'environmentGeneration', environment_generation,
    'operationId', operation_id
)
WHERE NOT (contract ? 'reservationGeneration')
   OR NOT (contract ? 'environmentGeneration')
   OR NOT (contract ? 'operationId');
