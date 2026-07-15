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
