-- Sprint 2 destructive baseline for the agent domain.
-- Pre-baseline development data is intentionally not upgrade-compatible.

-- Folded from 0001_initial.sql.
CREATE TABLE idempotency_ledger (operation text NOT NULL, idempotency_key text NOT NULL, request_sha256 text NOT NULL CHECK (request_sha256 ~ '^[0-9a-f]{64}$'), state text NOT NULL CHECK (state IN ('in_progress', 'completed')), result jsonb, created_at timestamptz NOT NULL DEFAULT now(), completed_at timestamptz, PRIMARY KEY (operation, idempotency_key), CHECK ((state = 'completed') = (result IS NOT NULL AND completed_at IS NOT NULL)));
CREATE TABLE outbox_events (event_id uuid PRIMARY KEY, subject text NOT NULL, event_type text NOT NULL, aggregate_id uuid NOT NULL, aggregate_sequence bigint NOT NULL CHECK (aggregate_sequence > 0), payload jsonb NOT NULL CHECK (jsonb_typeof(payload) = 'object'), payload_sha256 text NOT NULL CHECK (payload_sha256 ~ '^[0-9a-f]{64}$'), created_at timestamptz NOT NULL DEFAULT now(), published_at timestamptz, UNIQUE (aggregate_id, aggregate_sequence));
CREATE TABLE inbox_events (consumer text NOT NULL, event_id uuid NOT NULL, aggregate_id uuid NOT NULL, aggregate_sequence bigint NOT NULL CHECK (aggregate_sequence > 0), payload_sha256 text NOT NULL CHECK (payload_sha256 ~ '^[0-9a-f]{64}$'), processed_at timestamptz NOT NULL DEFAULT now(), PRIMARY KEY (consumer, event_id), UNIQUE (consumer, aggregate_id, aggregate_sequence));
CREATE TABLE inbox_watermarks (consumer text NOT NULL, aggregate_id uuid NOT NULL, last_sequence bigint NOT NULL CHECK (last_sequence >= 0), updated_at timestamptz NOT NULL DEFAULT now(), PRIMARY KEY (consumer, aggregate_id));
CREATE TABLE agent_runs (
    run_id uuid PRIMARY KEY, course_id uuid NOT NULL, problem_package_id uuid NOT NULL,
    revision bigint NOT NULL CHECK (revision > 0), state text NOT NULL, provider_binding text NOT NULL,
    input_sha256 text NOT NULL CHECK (input_sha256 ~ '^[0-9a-f]{64}$'), policy_revision bigint NOT NULL CHECK (policy_revision > 0),
    contract jsonb NOT NULL, created_at timestamptz NOT NULL DEFAULT now(), updated_at timestamptz NOT NULL DEFAULT now(),
    CHECK (jsonb_typeof(contract) = 'object')
);
CREATE TABLE agent_checkpoints (
    run_id uuid NOT NULL, checkpoint_sequence bigint NOT NULL CHECK (checkpoint_sequence > 0),
    checkpoint_sha256 text NOT NULL CHECK (checkpoint_sha256 ~ '^[0-9a-f]{64}$'), state text NOT NULL,
    contract jsonb NOT NULL, created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (run_id, checkpoint_sequence), CHECK (jsonb_typeof(contract) = 'object')
);
CREATE TABLE image_artifacts (
    image_artifact_id uuid PRIMARY KEY, build_request_id uuid NOT NULL, image_digest text NOT NULL UNIQUE,
    evidence_sha256 text NOT NULL CHECK (evidence_sha256 ~ '^[0-9a-f]{64}$'), state text NOT NULL,
    contract jsonb NOT NULL, created_at timestamptz NOT NULL DEFAULT now(), CHECK (jsonb_typeof(contract) = 'object')
);

-- Folded from 0002_track_leases.sql.
CREATE TABLE agent_track_work_items (
    run_id uuid NOT NULL,
    track text NOT NULL CHECK (track IN ('environment', 'evaluation')),
    state text NOT NULL CHECK (state IN ('requested', 'running', 'succeeded', 'failed', 'cancelled')),
    input_sha256 text NOT NULL CHECK (input_sha256 ~ '^[0-9a-f]{64}$'),
    attempt_number bigint NOT NULL DEFAULT 0 CHECK (attempt_number >= 0),
    worker_id text,
    lease_token uuid,
    lease_expires_at timestamptz,
    heartbeat_at timestamptz,
    next_retry_at timestamptz NOT NULL DEFAULT now(),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (run_id, track),
    FOREIGN KEY (run_id) REFERENCES agent_runs(run_id) ON DELETE RESTRICT,
    CHECK (
        (worker_id IS NULL) = (lease_token IS NULL)
        AND (worker_id IS NULL) = (lease_expires_at IS NULL)
        AND (worker_id IS NULL) = (heartbeat_at IS NULL)
    ),
    CHECK (
        (state = 'running' AND worker_id IS NOT NULL)
        OR (state <> 'running' AND worker_id IS NULL)
    )
);

CREATE INDEX agent_track_work_items_due_idx
    ON agent_track_work_items (next_retry_at, created_at)
    WHERE state IN ('requested', 'running');

ALTER TABLE agent_runs
    ADD COLUMN cancellation_requested_at timestamptz;

-- Folded from 0003_control_dispatch.sql.
CREATE TABLE agent_run_dispatches (
    run_id uuid PRIMARY KEY REFERENCES agent_runs(run_id) ON DELETE RESTRICT,
    dispatch_sha256 text NOT NULL CHECK (dispatch_sha256 ~ '^[0-9a-f]{64}$'),
    idempotency_key text NOT NULL,
    request jsonb NOT NULL CHECK (jsonb_typeof(request) = 'object'),
    package jsonb NOT NULL CHECK (jsonb_typeof(package) = 'object'),
    object_locators jsonb NOT NULL CHECK (jsonb_typeof(object_locators) = 'object'),
    policy jsonb NOT NULL CHECK (jsonb_typeof(policy) = 'object'),
    trace_id text NOT NULL CHECK (trace_id <> ''),
    state text NOT NULL CHECK (state IN ('pending', 'preparing', 'prepared', 'failed')),
    lease_token uuid,
    lease_expires_at timestamptz,
    prepared_input_sha256 text CHECK (prepared_input_sha256 ~ '^[0-9a-f]{64}$'),
    terminal_diagnostic text,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    CHECK ((state = 'preparing') = (lease_token IS NOT NULL AND lease_expires_at IS NOT NULL)),
    CHECK (state <> 'prepared' OR prepared_input_sha256 IS NOT NULL)
);

CREATE INDEX agent_run_dispatches_due_idx
    ON agent_run_dispatches(created_at)
    WHERE state IN ('pending', 'preparing', 'prepared');

ALTER TABLE image_artifacts
    ADD COLUMN policy_evaluation jsonb,
    ADD COLUMN artifact_sha256 text CHECK (artifact_sha256 ~ '^[0-9a-f]{64}$'),
    ADD CONSTRAINT image_artifacts_policy_evaluation_object
        CHECK (policy_evaluation IS NULL OR jsonb_typeof(policy_evaluation) = 'object');

-- Folded from 0004_build_pipeline.sql.
CREATE TABLE build_commands (
    build_request_id uuid PRIMARY KEY,
    course_id uuid NOT NULL,
    command_sha256 text NOT NULL UNIQUE CHECK (command_sha256 ~ '^[0-9a-f]{64}$'),
    idempotency_key text NOT NULL UNIQUE,
    state text NOT NULL CHECK (state IN ('requested', 'running', 'succeeded', 'failed', 'cancelled')),
    command jsonb NOT NULL CHECK (jsonb_typeof(command) = 'object'),
    attempt integer NOT NULL DEFAULT 0 CHECK (attempt >= 0),
    next_attempt_at timestamptz NOT NULL DEFAULT now(),
    worker_id text,
    lease_token uuid,
    lease_expires_at timestamptz,
    cancellation_requested boolean NOT NULL DEFAULT false,
    diagnostic_code text,
    retryable boolean,
    cleanup_verified boolean,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    completed_at timestamptz,
    CHECK ((state = 'running') = (worker_id IS NOT NULL AND lease_token IS NOT NULL AND lease_expires_at IS NOT NULL)),
    CHECK ((state IN ('succeeded', 'failed', 'cancelled')) = (completed_at IS NOT NULL)),
    CHECK ((state IN ('failed', 'cancelled')) = (diagnostic_code IS NOT NULL AND retryable IS NOT NULL AND cleanup_verified IS NOT NULL))
);

CREATE INDEX build_commands_due_idx
    ON build_commands (next_attempt_at, created_at)
    WHERE state IN ('requested', 'running');

ALTER TABLE image_artifacts
    ADD COLUMN registry_project_evidence jsonb
    CHECK (registry_project_evidence IS NULL OR jsonb_typeof(registry_project_evidence) = 'object');

CREATE UNIQUE INDEX image_artifacts_build_request_idx
    ON image_artifacts (build_request_id);

-- Folded from 0005_build_cancellation_fence.sql.
ALTER TABLE build_commands
    ADD COLUMN revision bigint NOT NULL DEFAULT 1 CHECK (revision > 0),
    ADD COLUMN cancellation_audit_version smallint NOT NULL DEFAULT 0
        CHECK (cancellation_audit_version IN (0, 1)),
    ADD COLUMN cancellation_actor_id uuid,
    ADD COLUMN cancellation_authority_san_uri text,
    ADD COLUMN cancellation_requested_at timestamptz,
    ADD CONSTRAINT build_commands_cancellation_audit_complete CHECK (
        (
            NOT cancellation_requested
            AND cancellation_actor_id IS NULL
            AND cancellation_authority_san_uri IS NULL
            AND cancellation_requested_at IS NULL
        ) OR (
            cancellation_requested
            AND (
                (
                    cancellation_audit_version = 0
                    AND cancellation_actor_id IS NULL
                    AND cancellation_authority_san_uri IS NULL
                    AND cancellation_requested_at IS NULL
                ) OR (
                    cancellation_audit_version = 1
                    AND cancellation_actor_id IS NOT NULL
                    AND cancellation_authority_san_uri IS NOT NULL
                    AND cancellation_requested_at IS NOT NULL
                )
            )
        )
    ),
    ADD CONSTRAINT build_commands_cancellation_authority_exact CHECK (
        cancellation_authority_san_uri IS NULL
        OR cancellation_authority_san_uri = 'spiffe://labweaver/control-service'
    );

-- Existing rows are explicitly version 0. Every row inserted after this forward
-- migration defaults to the actor-attributed v1 cancellation audit contract.
ALTER TABLE build_commands
    ALTER COLUMN cancellation_audit_version SET DEFAULT 1;

CREATE TABLE build_executor_fences (
    build_request_id uuid PRIMARY KEY,
    highest_generation integer NOT NULL CHECK (highest_generation > 0),
    lease_token uuid NOT NULL,
    tombstone_generation integer CHECK (
        tombstone_generation > 0 AND tombstone_generation <= highest_generation
    ),
    last_stage text NOT NULL CHECK (
        last_stage IN ('ensure_private_project','build','scan','sign','publish','cleanup')
    ),
    last_stage_rank smallint NOT NULL CHECK (last_stage_rank BETWEEN 1 AND 6),
    last_request_id text NOT NULL CHECK (last_request_id ~ '^[0-9a-f]{64}$'),
    last_response jsonb CHECK (last_response IS NULL OR jsonb_typeof(last_response) = 'object'),
    deadline_at timestamptz NOT NULL,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    CHECK (
        (last_stage = 'cleanup' AND tombstone_generation = highest_generation)
        OR (last_stage <> 'cleanup' AND tombstone_generation IS NULL)
    )
);

-- Folded from 0006_build_executor_artifacts.sql.
CREATE TABLE build_executor_artifacts (
    build_request_id uuid PRIMARY KEY,
    build_identity text NOT NULL CHECK (build_identity ~ '^[0-9a-f]{64}$'),
    repository text NOT NULL CHECK (repository <> '' AND repository NOT LIKE '%@%'),
    project_name text NOT NULL CHECK (project_name ~ '^[a-z0-9][a-z0-9._-]*$'),
    repository_name text NOT NULL CHECK (repository_name ~ '^[a-z0-9._-]+$'),
    candidate_tag text NOT NULL CHECK (candidate_tag ~ '^candidate-[0-9a-f]{24}$'),
    digest text NOT NULL CHECK (digest ~ '^sha256:[0-9a-f]{64}$'),
    cleaned_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    updated_at timestamptz NOT NULL DEFAULT clock_timestamp()
);

CREATE UNIQUE INDEX build_executor_artifacts_candidate_identity
    ON build_executor_artifacts(project_name, repository_name, candidate_tag);
