-- Each live Resource replica owns its observations, while reservations remain shared.
-- Historical rows receive an unreachable generation and require a fresh observation.
ALTER TABLE resource.gpu_capacity_observations
    ADD COLUMN observer_generation uuid NOT NULL DEFAULT gen_random_uuid();
ALTER TABLE resource.gpu_capacity_observations
    ALTER COLUMN observer_generation DROP DEFAULT,
    DROP CONSTRAINT gpu_capacity_observations_entry_id_observed_at_key,
    ADD CONSTRAINT gpu_capacity_observations_generation_time_key
        UNIQUE (entry_id, observer_generation, observed_at);
