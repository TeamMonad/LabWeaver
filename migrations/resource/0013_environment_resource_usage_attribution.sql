-- Unify Environment resource authorization across CPU, memory, storage, and GPU.
-- Existing GPU reservations are retained and renamed. Rows without an exact,
-- recoverable ReleaseProjection are unrecoverable at this hard cut; abort the
-- migration rather than treating them as zero or dropping their historical contract.
ALTER TABLE resource.environment_gpu_reservations
    RENAME TO environment_resource_reservations;

ALTER INDEX resource.environment_gpu_reservations_active_idx
    RENAME TO environment_resource_reservations_active_idx;

ALTER TABLE resource.environment_resource_reservations
    ALTER COLUMN entry_id DROP NOT NULL,
    ALTER COLUMN allocation_binding DROP NOT NULL,
    ALTER COLUMN units DROP NOT NULL;

-- The old table carried only GPU columns. Enrich every historical reservation from the exact
-- Environment ReleaseProjection before the application starts decoding the unified contract.
-- The dynamic block keeps isolated Resource migration tests valid when the Environment schema is
-- not installed in that database; a live platform database always has the schema.
DO $$
BEGIN
    IF to_regclass('environment.environment_instances') IS NOT NULL
        AND to_regclass('environment.release_projections') IS NOT NULL
    THEN
        EXECUTE $sql$
            WITH sources AS (
                SELECT
                    instance.environment_id,
                    CASE
                        WHEN jsonb_typeof(instance.contract -> 'approvedResources') = 'object'
                            THEN instance.contract -> 'approvedResources'
                        ELSE jsonb_build_object(
                            'cpuMillicores', projection.contract #> '{environmentSpec,resources,cpuMillicores}',
                            'memoryBytes', projection.contract #> '{environmentSpec,resources,memoryBytes}',
                            'storageBytes', projection.contract #> '{environmentSpec,resources,storageBytes}',
                            'gpu', projection.contract #> '{environmentSpec,resources,gpu}'
                        )
                    END AS approved_resources
                FROM environment.environment_instances AS instance
                LEFT JOIN environment.release_projections AS projection
                    ON projection.release_id = instance.release_id
                    AND projection.release_version = (instance.contract ->> 'releaseVersion')::bigint
                    AND projection.project_id = instance.project_id
            )
            UPDATE resource.environment_resource_reservations AS reservation
            SET contract = jsonb_build_object(
                'environmentId', reservation.environment_id,
                'projectId', reservation.project_id,
                'courseId', reservation.course_id,
                'ownerActorId', reservation.owner_actor_id,
                'providerBinding', reservation.provider_binding,
                'approvedResources', sources.approved_resources,
                'allocation', reservation.contract -> 'allocation',
                'state', reservation.state
            )
            FROM sources
            WHERE reservation.environment_id = sources.environment_id
        $sql$;

        EXECUTE $sql$
            WITH candidates AS (
                SELECT
                    instance.environment_id,
                    instance.project_id,
                    instance.course_id,
                    instance.owner_actor_id,
                    instance.provider_binding,
                    CASE
                        WHEN jsonb_typeof(instance.contract -> 'approvedResources') = 'object'
                            THEN instance.contract -> 'approvedResources'
                        ELSE jsonb_build_object(
                            'cpuMillicores', projection.contract #> '{environmentSpec,resources,cpuMillicores}',
                            'memoryBytes', projection.contract #> '{environmentSpec,resources,memoryBytes}',
                            'storageBytes', projection.contract #> '{environmentSpec,resources,storageBytes}',
                            'gpu', projection.contract #> '{environmentSpec,resources,gpu}'
                        )
                    END AS approved_resources,
                    CASE
                        WHEN jsonb_typeof(instance.contract -> 'gpuAllocation') = 'object'
                            THEN instance.contract -> 'gpuAllocation'
                        ELSE NULL
                    END AS allocation,
                    instance.contract ->> 'observedState' AS observed_state
                FROM environment.environment_instances AS instance
                LEFT JOIN environment.release_projections AS projection
                    ON projection.release_id = instance.release_id
                    AND projection.release_version = (instance.contract ->> 'releaseVersion')::bigint
                    AND projection.project_id = instance.project_id
                WHERE instance.contract ->> 'class' = 'experiment'
            ),
            valid_candidates AS (
                SELECT
                    candidates.*,
                    CASE
                        WHEN allocation IS NULL
                            OR allocation ->> 'entryId' ~
                                '^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$'
                            THEN NULLIF(allocation ->> 'entryId', '')::uuid
                    END AS entry_id,
                    CASE
                        WHEN allocation IS NULL THEN NULL
                        ELSE allocation ->> 'allocationBinding'
                    END AS allocation_binding,
                    CASE
                        WHEN allocation IS NULL THEN NULL
                        ELSE NULLIF(allocation ->> 'count', '')::integer
                    END AS units
                FROM candidates
                WHERE jsonb_typeof(approved_resources) = 'object'
                    AND (approved_resources ->> 'cpuMillicores') ~ '^[1-9][0-9]*$'
                    AND (approved_resources ->> 'memoryBytes') ~ '^[1-9][0-9]*$'
                    AND (approved_resources ->> 'storageBytes') ~ '^[1-9][0-9]*$'
                    AND (
                        allocation IS NULL
                        OR (
                            allocation ->> 'entryId' ~
                                '^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$'
                            AND (allocation ->> 'count') ~ '^[1-9][0-9]*$'
                            AND length(trim(allocation ->> 'allocationBinding')) BETWEEN 1 AND 256
                        )
                    )
                    AND (
                        (
                            COALESCE(jsonb_typeof(approved_resources -> 'gpu'), 'null') = 'null'
                            AND allocation IS NULL
                        )
                        OR (
                            jsonb_typeof(approved_resources -> 'gpu') = 'object'
                            AND jsonb_typeof(allocation) = 'object'
                            AND approved_resources -> 'gpu' ->> 'class' = allocation ->> 'class'
                            AND approved_resources -> 'gpu' ->> 'count' = allocation ->> 'count'
                        )
                    )
            )
            INSERT INTO resource.environment_resource_reservations (
                reservation_id, environment_id, project_id, course_id, owner_actor_id,
                provider_binding, entry_id, allocation_binding, units, state, released_at, contract)
            SELECT
                uuid_in(md5(random()::text || clock_timestamp()::text)::cstring),
                candidate.environment_id,
                candidate.project_id,
                candidate.course_id,
                candidate.owner_actor_id,
                candidate.provider_binding,
                candidate.entry_id,
                candidate.allocation_binding,
                candidate.units,
                CASE WHEN candidate.observed_state = 'deleted' THEN 'released' ELSE 'reserved' END,
                CASE WHEN candidate.observed_state = 'deleted' THEN clock_timestamp() ELSE NULL END,
                jsonb_build_object(
                    'environmentId', candidate.environment_id,
                    'projectId', candidate.project_id,
                    'courseId', candidate.course_id,
                    'ownerActorId', candidate.owner_actor_id,
                    'providerBinding', candidate.provider_binding,
                    'approvedResources', candidate.approved_resources,
                    'allocation', candidate.allocation,
                    'state', CASE WHEN candidate.observed_state = 'deleted' THEN 'released' ELSE 'reserved' END
                )
            FROM valid_candidates AS candidate
            WHERE NOT EXISTS (
                SELECT 1
                FROM resource.environment_resource_reservations AS existing
                WHERE existing.environment_id = candidate.environment_id
            )
        $sql$;

        EXECUTE $sql$
            DO $check$
            BEGIN
                IF EXISTS (
                    SELECT 1
                    FROM environment.environment_instances AS instance
                    LEFT JOIN resource.environment_resource_reservations AS reservation
                        ON reservation.environment_id = instance.environment_id
                    WHERE instance.contract ->> 'class' = 'experiment'
                        AND reservation.environment_id IS NULL
                ) THEN
                    RAISE EXCEPTION 'Experiment instance lacks an authoritative resource reservation';
                END IF;
            END
            $check$
        $sql$;
    END IF;
END
$$;

DO $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM resource.environment_resource_reservations
        WHERE jsonb_typeof(contract -> 'approvedResources') <> 'object'
            OR COALESCE(contract -> 'approvedResources' ->> 'cpuMillicores', '') !~ '^[1-9][0-9]*$'
            OR COALESCE(contract -> 'approvedResources' ->> 'memoryBytes', '') !~ '^[1-9][0-9]*$'
            OR COALESCE(contract -> 'approvedResources' ->> 'storageBytes', '') !~ '^[1-9][0-9]*$'
    ) THEN
        RAISE EXCEPTION 'environment resource reservation lacks an authoritative approved resource snapshot';
    END IF;
END
$$;

DO $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM resource.environment_resource_reservations
        WHERE (
            jsonb_typeof(contract -> 'approvedResources' -> 'gpu') = 'object'
            AND (
                COALESCE(jsonb_typeof(contract -> 'allocation'), 'null') <> 'object'
                OR COALESCE(contract -> 'approvedResources' -> 'gpu' ->> 'class', '')
                    <> COALESCE(contract -> 'allocation' ->> 'class', '')
                OR COALESCE(contract -> 'approvedResources' -> 'gpu' ->> 'count', '')
                    <> COALESCE(contract -> 'allocation' ->> 'count', '')
            )
        )
        OR (
            COALESCE(jsonb_typeof(contract -> 'approvedResources' -> 'gpu'), 'null') = 'null'
            AND jsonb_typeof(contract -> 'allocation') = 'object'
        )
        OR COALESCE(jsonb_typeof(contract -> 'approvedResources' -> 'gpu'), 'null')
            NOT IN ('null', 'object')
        OR COALESCE(jsonb_typeof(contract -> 'allocation'), 'null') NOT IN ('null', 'object')
        OR (
            jsonb_typeof(contract -> 'approvedResources' -> 'gpu') = 'object'
            AND (
                COALESCE(contract -> 'approvedResources' -> 'gpu' ->> 'class', '') !~
                    '^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$'
                OR COALESCE(contract -> 'approvedResources' -> 'gpu' ->> 'count', '') !~
                    '^[1-9][0-9]*$'
            )
        )
        OR (
            jsonb_typeof(contract -> 'allocation') = 'object'
            AND (
                COALESCE(contract -> 'allocation' ->> 'entryId', '') !~
                    '^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$'
                OR COALESCE(contract -> 'allocation' ->> 'class', '') !~
                    '^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$'
                OR COALESCE(contract -> 'allocation' ->> 'count', '') !~
                    '^[1-9][0-9]*$'
                OR COALESCE(contract -> 'allocation' ->> 'mode', '') NOT IN
                    ('exclusive', 'container_time_slice', 'vm_vgpu')
                OR COALESCE(length(trim(contract -> 'allocation' ->> 'providerBinding')), 0)
                    NOT BETWEEN 1 AND 120
                OR COALESCE(length(trim(contract -> 'allocation' ->> 'allocationBinding')), 0)
                    NOT BETWEEN 1 AND 256
                OR COALESCE(contract -> 'allocation' ->> 'catalogRevision', '') !~ '^[1-9][0-9]*$'
            )
        )
    ) THEN
        RAISE EXCEPTION 'environment resource reservation has an unrecoverable GPU authorization';
    END IF;
END
$$;

ALTER TABLE resource.resource_usage_records
    ALTER COLUMN request_id DROP NOT NULL;

ALTER TABLE resource.resource_usage_records
    ADD COLUMN target_kind text NOT NULL DEFAULT 'resource_request'
        CHECK (target_kind IN ('resource_request', 'experiment_environment')),
    ADD COLUMN environment_id uuid;

UPDATE resource.resource_usage_records
SET contract = (contract - 'requestId' - 'leaseId') || jsonb_build_object(
    'target', jsonb_build_object(
        'kind', 'resource_request',
        'requestId', request_id,
        'leaseId', lease_id
    )
)
WHERE NOT (contract ? 'target');

ALTER TABLE resource.resource_usage_records
    ADD CONSTRAINT resource_usage_records_target_shape
    CHECK (
        (target_kind = 'resource_request' AND request_id IS NOT NULL AND environment_id IS NULL)
        OR
        (target_kind = 'experiment_environment' AND request_id IS NULL AND environment_id IS NOT NULL)
    );

CREATE INDEX resource_usage_records_environment_interval_idx
    ON resource.resource_usage_records (environment_id, kind, measured_from, measured_until)
    WHERE target_kind = 'experiment_environment';
