-- Sprint 2 destructive baseline for the environment domain.
-- Pre-baseline development data is intentionally not upgrade-compatible.

-- Folded from 0001_initial.sql.
CREATE TABLE idempotency_ledger (operation text NOT NULL, idempotency_key text NOT NULL, request_sha256 text NOT NULL CHECK (request_sha256 ~ '^[0-9a-f]{64}$'), state text NOT NULL CHECK (state IN ('in_progress', 'completed')), result jsonb, created_at timestamptz NOT NULL DEFAULT now(), completed_at timestamptz, PRIMARY KEY (operation, idempotency_key), CHECK ((state = 'completed') = (result IS NOT NULL AND completed_at IS NOT NULL)));
CREATE TABLE outbox_events (event_id uuid PRIMARY KEY, public_sequence bigserial UNIQUE NOT NULL, subject text NOT NULL, event_type text NOT NULL, aggregate_id uuid NOT NULL, aggregate_sequence bigint NOT NULL CHECK (aggregate_sequence > 0), payload jsonb NOT NULL CHECK (jsonb_typeof(payload) = 'object'), payload_sha256 text NOT NULL CHECK (payload_sha256 ~ '^[0-9a-f]{64}$'), created_at timestamptz NOT NULL DEFAULT now(), published_at timestamptz, UNIQUE (aggregate_id, aggregate_sequence));
CREATE TABLE inbox_events (consumer text NOT NULL, event_id uuid NOT NULL, aggregate_id uuid NOT NULL, aggregate_sequence bigint NOT NULL CHECK (aggregate_sequence > 0), payload_sha256 text NOT NULL CHECK (payload_sha256 ~ '^[0-9a-f]{64}$'), processed_at timestamptz NOT NULL DEFAULT now(), PRIMARY KEY (consumer, event_id), UNIQUE (consumer, aggregate_id, aggregate_sequence));
CREATE TABLE inbox_watermarks (consumer text NOT NULL, aggregate_id uuid NOT NULL, last_sequence bigint NOT NULL CHECK (last_sequence >= 0), updated_at timestamptz NOT NULL DEFAULT now(), PRIMARY KEY (consumer, aggregate_id));
CREATE TABLE environment_instances (
    environment_id uuid PRIMARY KEY, course_id uuid NOT NULL, owner_actor_id uuid NOT NULL, release_id uuid NOT NULL, generation bigint NOT NULL CHECK (generation > 0),
    observed_generation bigint NOT NULL CHECK (observed_generation >= 0 AND observed_generation <= generation),
    desired_state text NOT NULL, observed_state text NOT NULL, provider_binding text NOT NULL,
    lease_id uuid, revision bigint NOT NULL CHECK (revision > 0), terminal_diagnostic text,
    contract jsonb NOT NULL, created_at timestamptz NOT NULL DEFAULT now(), updated_at timestamptz NOT NULL DEFAULT now(),
    CHECK (jsonb_typeof(contract) = 'object')
);
CREATE INDEX environment_instances_owner_course_idx
    ON environment_instances (course_id, owner_actor_id, created_at DESC);
CREATE TABLE environment_operations (
    operation_id uuid PRIMARY KEY, environment_id uuid NOT NULL, operation_kind text NOT NULL,
    expected_revision bigint NOT NULL CHECK (expected_revision > 0), target_generation bigint NOT NULL CHECK (target_generation > 0),
    state text NOT NULL, retry_count integer NOT NULL DEFAULT 0 CHECK (retry_count >= 0),
    lease_owner text, heartbeat_at timestamptz, diagnostic text, contract jsonb NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(), finished_at timestamptz, CHECK (jsonb_typeof(contract) = 'object')
);

-- Folded from 0002_reconcile_leases.sql.
ALTER TABLE environment_operations
    ADD COLUMN max_attempts bigint NOT NULL DEFAULT 3,
    ADD COLUMN next_attempt_at timestamptz NOT NULL DEFAULT now(),
    ADD COLUMN deadline_at timestamptz NOT NULL DEFAULT (now() + interval '15 minutes'),
    ADD COLUMN provider_step bigint NOT NULL DEFAULT 1,
    ADD COLUMN lease_token uuid,
    ADD COLUMN lease_expires_at timestamptz;

-- Existing operations may already have more than the new default retry budget.
UPDATE environment_operations
    SET max_attempts = GREATEST(3::bigint, retry_count::bigint + 1),
        next_attempt_at = COALESCE(
            (contract ->> 'acceptedAt')::timestamptz,
            created_at
        ),
        deadline_at = COALESCE(
            (contract ->> 'deadlineAt')::timestamptz,
            created_at + interval '15 minutes'
        ),
        lease_owner = NULL,
        lease_token = NULL,
        lease_expires_at = NULL,
        heartbeat_at = NULL,
        contract = jsonb_set(
            jsonb_set(
                jsonb_set(
                    contract,
                    '{maxAttempts}',
                    to_jsonb(GREATEST(3::bigint, retry_count::bigint + 1)),
                    true
                ),
                '{nextAttemptAt}',
                to_jsonb(to_char(
                    COALESCE(
                        (contract ->> 'acceptedAt')::timestamptz,
                        created_at
                    ) AT TIME ZONE 'UTC',
                    'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"'
                )),
                true
            ),
            '{traceId}',
            to_jsonb('migration-v1-' || operation_id::text),
            true
        );

UPDATE environment_operations
    SET contract = contract || jsonb_build_object(
        'providerStep', 1,
        'retryFromPhase', NULL,
        'resetTarget', NULL,
        'leaseAuthorization', NULL
    );

ALTER TABLE environment_operations
    ADD CONSTRAINT environment_operation_max_attempts_positive
        CHECK (max_attempts > 0),
    ADD CONSTRAINT environment_operation_retry_bound
        CHECK (retry_count < max_attempts),
    ADD CONSTRAINT environment_operation_time_bound
        CHECK (next_attempt_at <= deadline_at),
    ADD CONSTRAINT environment_operation_provider_step_positive
        CHECK (provider_step > 0),
    ADD CONSTRAINT environment_operation_lease_bound
        CHECK (
            (lease_owner IS NULL) = (lease_token IS NULL)
            AND (lease_owner IS NULL) = (lease_expires_at IS NULL)
            AND (lease_owner IS NULL) = (heartbeat_at IS NULL)
        );

ALTER TABLE environment_instances
    ADD COLUMN eligibility_expires_at timestamptz,
    ADD COLUMN capacity_binding text,
    ADD COLUMN failed_phase text;

-- V1 rows did not retain the authoritative eligibility horizon. Expire them at
-- migration time so the owner resolver fails closed until a revisioned command
-- establishes a new horizon, while keeping every JSON contract readable.
UPDATE environment_instances
    SET eligibility_expires_at = date_trunc('milliseconds', updated_at),
        contract = jsonb_set(
            jsonb_set(
                jsonb_set(
                    jsonb_set(
                        contract,
                        '{operation,maxAttempts}',
                        to_jsonb(GREATEST(
                            3,
                            COALESCE((contract #>> '{operation,attempt}')::integer, 1)
                        )),
                        true
                    ),
                    '{operation,nextAttemptAt}',
                    to_jsonb(COALESCE(
                        contract #>> '{operation,acceptedAt}',
                        to_char(
                            updated_at AT TIME ZONE 'UTC',
                            'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"'
                        )
                    )),
                    true
                ),
                '{operation,traceId}',
                to_jsonb('migration-v1-' || (contract #>> '{operation,id}')),
                true
            ),
            '{eligibilityExpiresAt}',
            to_jsonb(to_char(
                updated_at AT TIME ZONE 'UTC',
                'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"'
            )),
            true
        );

UPDATE environment_instances
    SET capacity_binding = CASE
            WHEN contract ->> 'class' = 'work' THEN 'migration-reconcile-required'
            ELSE NULL
        END,
        -- V1 did not retain the phase that failed. Keep it unknown so decoding
        -- and retry/recover fail closed instead of guessing a Provider action.
        failed_phase = NULL,
        contract = jsonb_set(
            contract || jsonb_build_object(
                'capacityBinding', CASE
                    WHEN contract ->> 'class' = 'work' THEN 'migration-reconcile-required'
                    ELSE NULL
                END,
                'failedPhase', NULL
            ),
            '{operation}',
            (contract -> 'operation') || jsonb_build_object(
                'providerStep', 1,
                'retryFromPhase', NULL,
                'resetTarget', NULL,
                'leaseAuthorization', NULL
            ),
            true
        );

ALTER TABLE environment_instances
    ALTER COLUMN eligibility_expires_at SET NOT NULL;

ALTER TABLE environment_operations
    ADD CONSTRAINT environment_operation_instance_fk
        FOREIGN KEY (environment_id) REFERENCES environment_instances(environment_id)
        NOT VALID;

ALTER TABLE environment_operations
    VALIDATE CONSTRAINT environment_operation_instance_fk;

CREATE INDEX environment_operations_reconcile_due_idx
    ON environment_operations (next_attempt_at, created_at)
    WHERE state IN ('accepted', 'running', 'cancelling');

CREATE INDEX environment_instances_expiry_idx
    ON environment_instances (eligibility_expires_at)
    WHERE desired_state <> 'deleted';

-- Folded from 0003_release_projections.sql.
CREATE TABLE release_projections (
    release_id uuid PRIMARY KEY,
    course_id uuid NOT NULL,
    release_version bigint NOT NULL CHECK (release_version > 0),
    provider_binding text NOT NULL,
    projection_sha256 text NOT NULL CHECK (projection_sha256 ~ '^[0-9a-f]{64}$'),
    contract jsonb NOT NULL CHECK (jsonb_typeof(contract) = 'object'),
    projected_event_id uuid NOT NULL UNIQUE,
    aggregate_sequence bigint NOT NULL DEFAULT 1 CHECK (aggregate_sequence IN (1, 2)),
    withdrawn_at timestamptz,
    withdrawal_reason_code text,
    withdrawal_event_id uuid UNIQUE,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    CHECK (
        (aggregate_sequence = 1 AND withdrawn_at IS NULL AND withdrawal_reason_code IS NULL AND withdrawal_event_id IS NULL)
        OR
        (aggregate_sequence = 2 AND withdrawn_at IS NOT NULL AND withdrawal_reason_code IS NOT NULL AND withdrawal_event_id IS NOT NULL)
    ),
    UNIQUE (release_id, release_version)
);

CREATE INDEX release_projections_course_idx
    ON release_projections (course_id, release_version);

-- Folded from 0004_container_executor_fence.sql.
CREATE TABLE container_executor_fences (
    environment_id uuid PRIMARY KEY,
    highest_generation bigint NOT NULL CHECK (highest_generation > 0),
    operation_id uuid NOT NULL,
    provider_step integer NOT NULL CHECK (provider_step >= 0),
    attempt integer NOT NULL CHECK (attempt > 0),
    tombstoned boolean NOT NULL DEFAULT false,
    last_action text NOT NULL CHECK (
        last_action IN ('validate','build','provision','observe','start','stop','restart','reset','configure','cleanup')
    ),
    last_request_id text NOT NULL CHECK (last_request_id ~ '^[0-9a-f]{64}$'),
    last_response jsonb CHECK (last_response IS NULL OR jsonb_typeof(last_response) = 'object'),
    deadline_at timestamptz NOT NULL,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    CHECK (NOT tombstoned OR last_action = 'cleanup')
);

-- Folded from 0005_kubevirt_runtime_observations.sql.
CREATE TABLE kubevirt_runtime_observations (
    environment_id uuid PRIMARY KEY,
    state text NOT NULL CHECK (state IN ('running', 'stopped', 'deleted')),
    operation_id uuid NOT NULL,
    provider_step integer NOT NULL CHECK (provider_step > 0),
    environment_generation bigint NOT NULL CHECK (environment_generation > 0),
    attempt integer NOT NULL CHECK (attempt > 0),
    request_id text NOT NULL CHECK (request_id ~ '^[0-9a-f]{64}$'),
    namespace text NOT NULL,
    virtual_machine_name text NOT NULL,
    vm_resource_generation bigint CHECK (vm_resource_generation > 0),
    observed_vm_resource_generation bigint CHECK (observed_vm_resource_generation > 0),
    vm_uid uuid,
    vmi_uid uuid,
    root_disk_uid uuid,
    guest_ip text CHECK (guest_ip IS NULL OR guest_ip::inet IS NOT NULL),
    service_cluster_ip text CHECK (service_cluster_ip IS NULL OR service_cluster_ip::inet IS NOT NULL),
    ssh_host_key_sha256 text CHECK (
        ssh_host_key_sha256 IS NULL OR ssh_host_key_sha256 ~ '^[0-9a-f]{64}$'
    ),
    observation_sha256 text NOT NULL CHECK (observation_sha256 ~ '^[0-9a-f]{64}$'),
    cleanup_evidence jsonb,
    observed_at timestamptz NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    CHECK (
        (state = 'running'
            AND vm_resource_generation IS NOT NULL
            AND observed_vm_resource_generation >= vm_resource_generation
            AND vm_uid IS NOT NULL
            AND vmi_uid IS NOT NULL
            AND root_disk_uid IS NOT NULL
            AND guest_ip IS NOT NULL
            AND service_cluster_ip IS NOT NULL
            AND ssh_host_key_sha256 IS NOT NULL
            AND cleanup_evidence IS NULL)
        OR
        (state = 'stopped'
            AND vm_uid IS NOT NULL
            AND vmi_uid IS NULL
            AND root_disk_uid IS NOT NULL
            AND guest_ip IS NULL
            AND service_cluster_ip IS NULL
            AND ssh_host_key_sha256 IS NOT NULL
            AND cleanup_evidence IS NULL)
        OR
        (state = 'deleted'
            AND vmi_uid IS NULL
            AND guest_ip IS NULL
            AND service_cluster_ip IS NULL
            AND cleanup_evidence IS NOT NULL)
    )
);

CREATE INDEX kubevirt_runtime_observations_operation_idx
    ON kubevirt_runtime_observations (operation_id, provider_step);

-- Folded from 0006_kubevirt_executor_fence.sql.
CREATE TABLE kubevirt_executor_fences (
    environment_id uuid PRIMARY KEY,
    highest_generation bigint NOT NULL CHECK (highest_generation > 0),
    operation_id uuid NOT NULL,
    provider_step integer NOT NULL CHECK (provider_step >= 0),
    attempt integer NOT NULL CHECK (attempt > 0),
    tombstoned boolean NOT NULL DEFAULT false,
    last_action text NOT NULL CHECK (
        last_action IN ('validate','build','provision','observe','start','stop','restart','reset','configure','cleanup')
    ),
    last_request_id text NOT NULL CHECK (last_request_id ~ '^[0-9a-f]{64}$'),
    last_response jsonb CHECK (last_response IS NULL OR jsonb_typeof(last_response) = 'object'),
    deadline_at timestamptz NOT NULL,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    CHECK (NOT tombstoned OR last_action = 'cleanup')
);
