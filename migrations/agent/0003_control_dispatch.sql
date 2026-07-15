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
