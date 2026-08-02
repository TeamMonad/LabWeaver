-- EvaluationRelease, EvaluationRun and StepRun authoritative control plane.

CREATE TABLE evaluation_releases (
    release_id uuid PRIMARY KEY,
    course_id uuid NOT NULL,
    candidate_id uuid NOT NULL,
    candidate_revision bigint NOT NULL CHECK (candidate_revision > 0),
    candidate_sha256 text NOT NULL CHECK (candidate_sha256 ~ '^[0-9a-f]{64}$'),
    approval_id uuid NOT NULL,
    approval_revision bigint NOT NULL CHECK (approval_revision > 0),
    approval_sha256 text NOT NULL CHECK (approval_sha256 ~ '^[0-9a-f]{64}$'),
    evaluation_spec_sha256 text NOT NULL CHECK (evaluation_spec_sha256 ~ '^[0-9a-f]{64}$'),
    runtime_identity_sha256 text NOT NULL CHECK (runtime_identity_sha256 ~ '^[0-9a-f]{64}$'),
    release_identity_sha256 text NOT NULL CHECK (release_identity_sha256 ~ '^[0-9a-f]{64}$'),
    state text NOT NULL CHECK (state IN ('active', 'withdrawn')),
    revision bigint NOT NULL CHECK (revision > 0),
    contract jsonb NOT NULL CHECK (jsonb_typeof(contract) = 'object'),
    published_by uuid NOT NULL,
    published_at timestamptz NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    withdrawn_at timestamptz,
    withdrawal_diagnostic_code text,
    UNIQUE (course_id, candidate_id, approval_id),
    UNIQUE (course_id, release_identity_sha256),
    CHECK (
        (state = 'active' AND withdrawn_at IS NULL AND withdrawal_diagnostic_code IS NULL)
        OR
        (state = 'withdrawn' AND withdrawn_at IS NOT NULL AND withdrawal_diagnostic_code IS NOT NULL)
    )
);

CREATE TABLE evaluation_runs (
    run_id uuid PRIMARY KEY,
    course_id uuid NOT NULL,
    release_id uuid NOT NULL REFERENCES evaluation_releases(release_id),
    release_revision bigint NOT NULL CHECK (release_revision > 0),
    frozen_submission_id uuid NOT NULL REFERENCES frozen_submissions(frozen_submission_id),
    actor_id uuid NOT NULL,
    idempotency_key text NOT NULL CHECK (length(idempotency_key) BETWEEN 1 AND 128),
    request_sha256 text NOT NULL CHECK (request_sha256 ~ '^[0-9a-f]{64}$'),
    run_identity_sha256 text NOT NULL CHECK (run_identity_sha256 ~ '^[0-9a-f]{64}$'),
    state text NOT NULL CHECK (state IN ('queued', 'running', 'cancelling', 'succeeded', 'failed', 'cancelled')),
    revision bigint NOT NULL CHECK (revision > 0),
    max_score integer NOT NULL CHECK (max_score >= 0),
    awarded_score integer NOT NULL CHECK (awarded_score >= 0 AND awarded_score <= max_score),
    diagnostic_code text,
    cancellation_requested boolean NOT NULL DEFAULT false,
    cleanup_verified boolean NOT NULL DEFAULT false,
    contract jsonb NOT NULL CHECK (jsonb_typeof(contract) = 'object'),
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    completed_at timestamptz,
    UNIQUE (course_id, idempotency_key),
    CHECK (
        (state IN ('queued', 'running', 'cancelling') AND completed_at IS NULL)
        OR
        (state = 'succeeded' AND completed_at IS NOT NULL AND diagnostic_code IS NULL AND cleanup_verified)
        OR
        (state IN ('failed', 'cancelled') AND diagnostic_code IS NOT NULL AND (
            (cleanup_verified AND completed_at IS NOT NULL)
            OR
            (NOT cleanup_verified AND completed_at IS NULL)
        ))
    )
);

CREATE INDEX evaluation_runs_due_idx
    ON evaluation_runs (state, created_at)
    WHERE state IN ('queued', 'running', 'cancelling');

CREATE TABLE evaluation_step_runs (
    step_run_id uuid PRIMARY KEY,
    run_id uuid NOT NULL REFERENCES evaluation_runs(run_id) ON DELETE CASCADE,
    position integer NOT NULL CHECK (position > 0),
    step_id text NOT NULL CHECK (length(step_id) BETWEEN 1 AND 96),
    role text NOT NULL CHECK (role IN ('gate', 'score', 'advisory')),
    depends_on text[] NOT NULL DEFAULT '{}',
    state text NOT NULL CHECK (state IN ('pending', 'running', 'retryable', 'succeeded', 'failed', 'cancelled', 'skipped')),
    revision bigint NOT NULL CHECK (revision > 0),
    current_attempt integer NOT NULL DEFAULT 0 CHECK (current_attempt >= 0),
    max_score integer NOT NULL CHECK (max_score >= 0),
    awarded_score integer CHECK (awarded_score >= 0 AND awarded_score <= max_score),
    diagnostic_code text,
    evidence_sha256 text CHECK (evidence_sha256 IS NULL OR evidence_sha256 ~ '^[0-9a-f]{64}$'),
    cleanup_verified boolean NOT NULL DEFAULT false,
    contract jsonb NOT NULL CHECK (jsonb_typeof(contract) = 'object'),
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    started_at timestamptz,
    completed_at timestamptz,
    UNIQUE (run_id, position),
    UNIQUE (run_id, step_id),
    CHECK (cardinality(depends_on) <= 64),
    CHECK (role = 'score' OR (max_score = 0 AND awarded_score IS NULL)),
    CHECK (
        (state IN ('pending', 'running', 'retryable') AND completed_at IS NULL)
        OR
        (state = 'succeeded' AND completed_at IS NOT NULL AND diagnostic_code IS NULL AND cleanup_verified)
        OR
        (state IN ('failed', 'cancelled', 'skipped') AND completed_at IS NOT NULL AND diagnostic_code IS NOT NULL)
    )
);

CREATE INDEX evaluation_step_runs_due_idx
    ON evaluation_step_runs (state, updated_at, position)
    WHERE state IN ('pending', 'retryable', 'running');

CREATE TABLE evaluation_step_attempts (
    step_run_id uuid NOT NULL REFERENCES evaluation_step_runs(step_run_id) ON DELETE CASCADE,
    attempt integer NOT NULL CHECK (attempt > 0),
    state text NOT NULL CHECK (state IN ('running', 'succeeded', 'failed', 'cancelled')),
    worker_id text,
    lease_token uuid,
    lease_expires_at timestamptz,
    diagnostic_code text,
    evidence_sha256 text CHECK (evidence_sha256 IS NULL OR evidence_sha256 ~ '^[0-9a-f]{64}$'),
    cleanup_verified boolean NOT NULL DEFAULT false,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    completed_at timestamptz,
    PRIMARY KEY (step_run_id, attempt),
    CHECK (
        (state = 'running' AND worker_id IS NOT NULL AND lease_token IS NOT NULL AND lease_expires_at IS NOT NULL AND completed_at IS NULL)
        OR
        (state = 'succeeded' AND worker_id IS NULL AND lease_token IS NULL AND lease_expires_at IS NULL AND diagnostic_code IS NULL AND evidence_sha256 IS NOT NULL AND cleanup_verified AND completed_at IS NOT NULL)
        OR
        (state IN ('failed', 'cancelled') AND worker_id IS NULL AND lease_token IS NULL AND lease_expires_at IS NULL AND diagnostic_code IS NOT NULL AND evidence_sha256 IS NOT NULL AND completed_at IS NOT NULL)
    )
);

CREATE UNIQUE INDEX evaluation_step_attempts_one_active_idx
    ON evaluation_step_attempts (step_run_id)
    WHERE state = 'running';

CREATE INDEX evaluation_step_attempts_lease_idx
    ON evaluation_step_attempts (state, lease_expires_at)
    WHERE state = 'running';
