-- Sprint 2 destructive baseline for the evaluation domain.
-- Pre-baseline development data is intentionally not upgrade-compatible.

-- Folded from 0001_initial.sql.
CREATE TABLE idempotency_ledger (operation text NOT NULL, idempotency_key text NOT NULL, request_sha256 text NOT NULL CHECK (request_sha256 ~ '^[0-9a-f]{64}$'), state text NOT NULL CHECK (state IN ('in_progress', 'completed')), result jsonb, created_at timestamptz NOT NULL DEFAULT now(), completed_at timestamptz, PRIMARY KEY (operation, idempotency_key), CHECK ((state = 'completed') = (result IS NOT NULL AND completed_at IS NOT NULL)));
CREATE TABLE outbox_events (event_id uuid PRIMARY KEY, subject text NOT NULL, event_type text NOT NULL, aggregate_id uuid NOT NULL, aggregate_sequence bigint NOT NULL CHECK (aggregate_sequence > 0), payload jsonb NOT NULL CHECK (jsonb_typeof(payload) = 'object'), payload_sha256 text NOT NULL CHECK (payload_sha256 ~ '^[0-9a-f]{64}$'), created_at timestamptz NOT NULL DEFAULT now(), published_at timestamptz, UNIQUE (aggregate_id, aggregate_sequence));
CREATE TABLE inbox_events (consumer text NOT NULL, event_id uuid NOT NULL, aggregate_id uuid NOT NULL, aggregate_sequence bigint NOT NULL CHECK (aggregate_sequence > 0), payload_sha256 text NOT NULL CHECK (payload_sha256 ~ '^[0-9a-f]{64}$'), processed_at timestamptz NOT NULL DEFAULT now(), PRIMARY KEY (consumer, event_id), UNIQUE (consumer, aggregate_id, aggregate_sequence));
CREATE TABLE inbox_watermarks (consumer text NOT NULL, aggregate_id uuid NOT NULL, last_sequence bigint NOT NULL CHECK (last_sequence >= 0), updated_at timestamptz NOT NULL DEFAULT now(), PRIMARY KEY (consumer, aggregate_id));
CREATE TABLE frozen_submissions (
    frozen_submission_id uuid PRIMARY KEY, course_id uuid NOT NULL, environment_id uuid NOT NULL,
    manifest_sha256 text NOT NULL CHECK (manifest_sha256 ~ '^[0-9a-f]{64}$'), content_sha256 text NOT NULL CHECK (content_sha256 ~ '^[0-9a-f]{64}$'),
    schema_version text NOT NULL, tool_version text NOT NULL, contract jsonb NOT NULL,
    frozen_at timestamptz NOT NULL, UNIQUE (course_id, content_sha256), CHECK (jsonb_typeof(contract) = 'object')
);

-- Folded from 0002_submission_freezes.sql.
CREATE TABLE submission_freeze_requests (
    frozen_submission_id uuid PRIMARY KEY,
    course_id uuid NOT NULL,
    environment_id uuid NOT NULL,
    idempotency_key text NOT NULL CHECK (length(idempotency_key) BETWEEN 1 AND 512),
    request_sha256 text NOT NULL CHECK (request_sha256 ~ '^[0-9a-f]{64}$'),
    source_identity_sha256 text NOT NULL CHECK (source_identity_sha256 ~ '^[0-9a-f]{64}$'),
    state text NOT NULL CHECK (state IN ('active', 'retryable', 'completed')),
    current_attempt integer NOT NULL CHECK (current_attempt > 0),
    contract jsonb CHECK (contract IS NULL OR jsonb_typeof(contract) = 'object'),
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    completed_at timestamptz,
    UNIQUE (course_id, idempotency_key),
    CHECK ((state = 'completed') = (contract IS NOT NULL AND completed_at IS NOT NULL))
);

CREATE TABLE submission_freeze_attempts (
    frozen_submission_id uuid NOT NULL REFERENCES submission_freeze_requests(frozen_submission_id),
    attempt integer NOT NULL CHECK (attempt > 0),
    state text NOT NULL CHECK (state IN ('reserved', 'preflighting', 'uploading', 'completed', 'failed')),
    worker_id text,
    lease_token uuid,
    lease_expires_at timestamptz,
    object_key text,
    object_version text,
    object_sha256 text CHECK (object_sha256 IS NULL OR object_sha256 ~ '^[0-9a-f]{64}$'),
    diagnostic_code text,
    cleanup_verified boolean NOT NULL DEFAULT false,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    completed_at timestamptz,
    PRIMARY KEY (frozen_submission_id, attempt),
    CHECK ((state IN ('reserved', 'preflighting', 'uploading')) = (worker_id IS NOT NULL AND lease_token IS NOT NULL AND lease_expires_at IS NOT NULL)),
    CHECK ((state = 'completed') = (object_key IS NOT NULL AND object_version IS NOT NULL AND object_sha256 IS NOT NULL AND completed_at IS NOT NULL)),
    CHECK ((state = 'failed') = (diagnostic_code IS NOT NULL AND completed_at IS NOT NULL))
);

CREATE INDEX submission_freeze_attempts_lease_idx
    ON submission_freeze_attempts (state, lease_expires_at)
    WHERE state IN ('reserved', 'preflighting', 'uploading');

CREATE UNIQUE INDEX submission_freeze_attempts_one_active_idx
    ON submission_freeze_attempts (frozen_submission_id)
    WHERE state IN ('reserved', 'preflighting', 'uploading');

ALTER TABLE frozen_submissions
    DROP CONSTRAINT IF EXISTS frozen_submissions_course_id_content_sha256_key;

ALTER TABLE frozen_submissions
    ADD COLUMN idempotency_key text,
    ADD COLUMN source_identity_sha256 text CHECK (source_identity_sha256 IS NULL OR source_identity_sha256 ~ '^[0-9a-f]{64}$'),
    ADD COLUMN object_key text,
    ADD COLUMN object_version text;

CREATE UNIQUE INDEX frozen_submissions_course_id_idempotency_key_idx
    ON frozen_submissions (course_id, idempotency_key)
    WHERE idempotency_key IS NOT NULL;

-- Public freeze acceptance and durable coordinator queue.
CREATE TABLE submission_freeze_commands (
    frozen_submission_id uuid PRIMARY KEY,
    operation_id uuid NOT NULL UNIQUE,
    course_id uuid NOT NULL,
    environment_id uuid NOT NULL,
    actor_id uuid NOT NULL,
    idempotency_key text NOT NULL CHECK (length(idempotency_key) BETWEEN 1 AND 512),
    request_sha256 text NOT NULL CHECK (request_sha256 ~ '^[0-9a-f]{64}$'),
    manifest_sha256 text NOT NULL CHECK (manifest_sha256 ~ '^[0-9a-f]{64}$'),
    state text NOT NULL CHECK (state IN ('queued', 'running', 'completed', 'failed')),
    contract jsonb NOT NULL CHECK (jsonb_typeof(contract) = 'object'),
    job_name text,
    diagnostic_code text,
    cleanup_verified boolean NOT NULL DEFAULT false,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    completed_at timestamptz,
    UNIQUE (course_id, idempotency_key),
    CHECK ((state IN ('completed', 'failed')) = (completed_at IS NOT NULL)),
    CHECK (state <> 'completed' OR cleanup_verified),
    CHECK (state <> 'failed' OR diagnostic_code IS NOT NULL)
);

CREATE INDEX submission_freeze_commands_due_idx
    ON submission_freeze_commands (state, created_at)
    WHERE state IN ('queued', 'running');
