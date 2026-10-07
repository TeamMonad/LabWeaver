-- Bind every Environment instance and its durable meter to one immutable Resource target.
-- Work uses the saved lease authorization; Experiment uses the matching, project-owned
-- ReleaseProjection. A missing authoritative snapshot aborts the migration rather than
-- inventing a zero-valued meter.
WITH candidates AS (
    SELECT
        instance.environment_id,
        CASE
            WHEN instance.contract ->> 'class' = 'work' THEN COALESCE(
                CASE
                    WHEN jsonb_typeof(meter.contract -> 'approvedResources') = 'object'
                        THEN meter.contract -> 'approvedResources'
                    ELSE NULL
                END,
                instance.contract #> '{operation,leaseAuthorization,approvedResources}'
            )
            ELSE jsonb_build_object(
                'cpuMillicores', projection.contract #> '{environmentSpec,resources,cpuMillicores}',
                'memoryBytes', projection.contract #> '{environmentSpec,resources,memoryBytes}',
                'storageBytes', projection.contract #> '{environmentSpec,resources,storageBytes}',
                'gpu', projection.contract #> '{environmentSpec,resources,gpu}'
            )
        END AS approved_resources
    FROM environment.environment_instances AS instance
    LEFT JOIN environment.resource_metering_state AS meter
        ON meter.environment_id = instance.environment_id
    LEFT JOIN environment.release_projections AS projection
        ON projection.release_id = instance.release_id
        AND projection.release_version = (instance.contract ->> 'releaseVersion')::bigint
        AND projection.project_id = instance.project_id
    WHERE COALESCE(jsonb_typeof(instance.contract -> 'approvedResources'), 'null') <> 'object'
), valid_candidates AS (
    SELECT environment_id, approved_resources
    FROM candidates
    WHERE jsonb_typeof(approved_resources) = 'object'
        AND (approved_resources ->> 'cpuMillicores') ~ '^[1-9][0-9]*$'
        AND (approved_resources ->> 'memoryBytes') ~ '^[1-9][0-9]*$'
        AND (approved_resources ->> 'storageBytes') ~ '^[1-9][0-9]*$'
)
UPDATE environment.environment_instances AS instance
SET contract = jsonb_set(instance.contract, '{approvedResources}', candidate.approved_resources, true)
FROM valid_candidates AS candidate
WHERE instance.environment_id = candidate.environment_id;

DO $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM environment.environment_instances
        WHERE COALESCE(jsonb_typeof(contract -> 'approvedResources'), 'null') <> 'object'
            OR COALESCE(contract -> 'approvedResources' ->> 'cpuMillicores', '') !~ '^[1-9][0-9]*$'
            OR COALESCE(contract -> 'approvedResources' ->> 'memoryBytes', '') !~ '^[1-9][0-9]*$'
            OR COALESCE(contract -> 'approvedResources' ->> 'storageBytes', '') !~ '^[1-9][0-9]*$'
            OR COALESCE(
                jsonb_typeof(contract -> 'approvedResources' -> 'gpu'),
                'null'
            ) NOT IN ('null', 'object')
            OR (
                jsonb_typeof(contract -> 'approvedResources' -> 'gpu') = 'object'
                AND (
                    COALESCE(contract -> 'approvedResources' -> 'gpu' ->> 'class', '') !~
                        '^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$'
                    OR COALESCE(contract -> 'approvedResources' -> 'gpu' ->> 'count', '') !~
                        '^[1-9][0-9]*$'
                )
            )
    ) THEN
        RAISE EXCEPTION 'environment instance lacks an authoritative approved resource snapshot';
    END IF;
END
$$;

-- The previous Experiment-only meter accumulated a GPU quantity without a target. Its old
-- pending quantity is not a reconstructible interval. Do not discard a non-zero value: stop the
-- migration so an operator can preserve it before enabling the new state machine.
DO $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM environment.resource_metering_state
        WHERE (contract ->> 'pendingGpuUnitSeconds') ~ '^[1-9][0-9]*$'
    ) THEN
        RAISE EXCEPTION 'legacy Experiment GPU quantity requires interval reconciliation';
    END IF;
END
$$;

-- Convert existing durable meters. The new state has one target and one approved-resource
-- snapshot for both Work and Experiment; no GPU-only or settlement-pending state remains.
UPDATE environment.resource_metering_state AS meter
SET contract = CASE
    WHEN instance.contract ->> 'class' = 'work' THEN
        (meter.contract - 'pendingGpuUnitSeconds' - 'settlementPending') || jsonb_build_object(
            'target', jsonb_build_object(
                'kind', 'resource_request',
                'requestId', meter.contract -> 'requestId',
                'leaseId', meter.contract -> 'leaseId'
            ),
            'approvedResources', instance.contract -> 'approvedResources'
        )
    ELSE jsonb_build_object(
        'version', 1,
        'environmentId', instance.environment_id,
        'projectId', instance.project_id,
        'courseId', instance.contract -> 'courseId',
        'ownerActorId', instance.contract -> 'ownerId',
        'target', jsonb_build_object(
            'kind', 'experiment_environment',
            'environmentId', instance.environment_id
        ),
        'requestId', NULL,
        'leaseId', NULL,
        'leaseRevision', NULL,
        'capacityBinding', NULL,
        'approvedResources', instance.contract -> 'approvedResources',
        'gpuAllocation', meter.contract -> 'gpuAllocation',
        -- The old Experiment meter only proved a GPU compute boundary.  Do not
        -- reopen compute after a confirmed stop or deletion.  A stopping or
        -- failed row remains explicitly unknown until a real observation closes
        -- it; a stopped/deleted row has no active compute interval.
        'computeStartedAt', CASE
            WHEN instance.contract ->> 'observedState' = 'ready'
                AND jsonb_typeof(meter.contract -> 'computeStartedAt') = 'string'
                THEN meter.contract -> 'computeStartedAt'
            ELSE NULL
        END,
        'computeUnknownStartedAt', CASE
            WHEN instance.contract ->> 'observedState' IN ('stopping', 'failed')
                AND jsonb_typeof(meter.contract -> 'computeStartedAt') = 'string'
                THEN meter.contract -> 'computeStartedAt'
            ELSE NULL
        END,
        -- No storage boundary existed in the GPU-only contract.  Preserve an
        -- explicitly stored storage boundary when one is present; otherwise use
        -- the old compute boundary only as an unknown storage start.  Deletion
        -- closes the durable interval instead of leaving it open to the future.
        'storageStartedAt', CASE
            WHEN instance.contract ->> 'observedState' IN ('ready', 'stopping', 'stopped', 'failed')
                THEN COALESCE(
                    CASE
                        WHEN jsonb_typeof(meter.contract -> 'storageStartedAt') = 'string'
                            THEN meter.contract -> 'storageStartedAt'
                    END,
                    CASE
                        WHEN jsonb_typeof(meter.contract -> 'computeStartedAt') = 'string'
                            THEN meter.contract -> 'computeStartedAt'
                    END
                )
            ELSE NULL
        END,
        'storageKnown', CASE
            WHEN instance.contract ->> 'observedState' <> 'deleted'
                AND meter.contract ->> 'storageKnown' = 'true'
                AND jsonb_typeof(meter.contract -> 'storageStartedAt') = 'string'
                THEN true
            ELSE false
        END
    )
END
FROM environment.environment_instances AS instance
WHERE meter.environment_id = instance.environment_id
    AND NOT (meter.contract ? 'target');

-- A partially upgraded meter may already carry a target. Complete only the missing immutable
-- snapshot and remove the retired aggregate fields.
UPDATE environment.resource_metering_state AS meter
SET contract = (meter.contract - 'pendingGpuUnitSeconds' - 'settlementPending')
    || jsonb_build_object('approvedResources', instance.contract -> 'approvedResources')
FROM environment.environment_instances AS instance
WHERE meter.environment_id = instance.environment_id
    AND (meter.contract ? 'target')
    AND NOT (meter.contract ? 'approvedResources');

-- Older Experiment instances were not metered at all. Create the unified state for every
-- missing row. For an already observed Experiment, accepted_at is the earliest durable boundary
-- available to this migration; the resulting interval is explicitly unknown until a real
-- provider observation closes or recovers it. No known quantity is fabricated.
INSERT INTO environment.resource_metering_state (environment_id, contract)
SELECT
    instance.environment_id,
    jsonb_build_object(
        'version', 1,
        'environmentId', instance.environment_id,
        'projectId', instance.project_id,
        'courseId', instance.contract -> 'courseId',
        'ownerActorId', instance.contract -> 'ownerId',
        'target', CASE
            WHEN instance.contract ->> 'class' = 'work' THEN jsonb_build_object(
                'kind', 'resource_request',
                'requestId', instance.contract #> '{operation,leaseAuthorization,resourceRequestId}',
                'leaseId', instance.contract -> 'leaseId'
            )
            ELSE jsonb_build_object(
                'kind', 'experiment_environment',
                'environmentId', instance.environment_id
            )
        END,
        'requestId', CASE
            WHEN instance.contract ->> 'class' = 'work'
                THEN instance.contract #> '{operation,leaseAuthorization,resourceRequestId}'
            ELSE NULL
        END,
        'leaseId', CASE
            WHEN instance.contract ->> 'class' = 'work'
                THEN instance.contract -> 'leaseId'
            ELSE NULL
        END,
        'leaseRevision', CASE
            WHEN instance.contract ->> 'class' = 'work'
                THEN instance.contract #> '{operation,leaseAuthorization,leaseRevision}'
            ELSE NULL
        END,
        'capacityBinding', CASE
            WHEN instance.contract ->> 'class' = 'work'
                THEN instance.contract -> 'capacityBinding'
            ELSE NULL
        END,
        'approvedResources', instance.contract -> 'approvedResources',
        'gpuAllocation', CASE
            WHEN instance.contract ->> 'class' = 'work'
                THEN instance.contract #> '{operation,leaseAuthorization,gpuAllocation}'
            ELSE instance.contract -> 'gpuAllocation'
        END,
        'computeStartedAt', NULL,
        'computeUnknownStartedAt', CASE
            WHEN instance.contract ->> 'class' = 'experiment'
                AND instance.contract ->> 'observedState'
                    IN ('ready', 'stopping', 'failed')
                THEN instance.contract #> '{operation,acceptedAt}'
            ELSE NULL
        END,
        'storageStartedAt', CASE
            WHEN instance.contract ->> 'class' = 'experiment'
                AND instance.contract ->> 'observedState'
                    IN ('ready', 'stopping', 'stopped', 'failed')
                THEN instance.contract #> '{operation,acceptedAt}'
            ELSE NULL
        END,
        'storageKnown', false
    )
FROM environment.environment_instances AS instance
WHERE NOT EXISTS (
    SELECT 1
    FROM environment.resource_metering_state AS meter
    WHERE meter.environment_id = instance.environment_id
)
    AND instance.contract ->> 'observedState' <> 'requested';

-- Existing Environment deliveries use the same strict internal request as Resource. Preserve
-- event identity, interval, measurement, and delivery state while replacing caller-supplied
-- project/request fields with the durable target.
UPDATE environment.resource_meter_deliveries AS delivery
SET request = CASE
    WHEN instance.contract ->> 'class' = 'work' THEN
        (delivery.request - 'projectId' - 'courseId' - 'requestId' - 'leaseId')
        || jsonb_build_object(
            'target', jsonb_build_object(
                'kind', 'resource_request',
                'requestId', delivery.request -> 'requestId',
                'leaseId', delivery.request -> 'leaseId'
            )
        )
    ELSE
        (delivery.request - 'projectId' - 'courseId' - 'requestId' - 'leaseId')
        || jsonb_build_object(
            'target', jsonb_build_object(
                'kind', 'experiment_environment',
                'environmentId', instance.environment_id
            )
        )
END
FROM environment.environment_instances AS instance
WHERE delivery.environment_id = instance.environment_id
    AND NOT (delivery.request ? 'target');

DO $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM environment.resource_metering_state
        WHERE NOT (contract ? 'target') OR NOT (contract ? 'approvedResources')
    ) THEN
        RAISE EXCEPTION 'environment metering state lacks unified target or approved resources';
    END IF;
END
$$;

-- Resource 0013 runs as its domain owner and reads these two Environment-owned
-- snapshots while backfilling historical Experiment reservations. Keep that
-- migration-only read boundary explicit; runtime roles retain their domain ACLs.
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'lw_resource_owner') THEN
        EXECUTE 'GRANT USAGE ON SCHEMA environment TO lw_resource_owner';
        EXECUTE 'GRANT SELECT ON environment.environment_instances, environment.release_projections TO lw_resource_owner';
    END IF;
END
$$;
