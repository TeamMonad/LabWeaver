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
