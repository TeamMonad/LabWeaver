-- Sprint 2 destructive baseline for the control domain.
-- Pre-baseline development data is intentionally not upgrade-compatible.

-- Folded from 0001_initial.sql.
CREATE TABLE idempotency_ledger (
    operation text NOT NULL,
    idempotency_key text NOT NULL,
    request_sha256 text NOT NULL CHECK (request_sha256 ~ '^[0-9a-f]{64}$'),
    state text NOT NULL CHECK (state IN ('in_progress', 'completed')),
    result jsonb,
    created_at timestamptz NOT NULL DEFAULT now(),
    completed_at timestamptz,
    PRIMARY KEY (operation, idempotency_key),
    CHECK ((state = 'completed') = (result IS NOT NULL AND completed_at IS NOT NULL))
);
CREATE TABLE outbox_events (
    event_id uuid PRIMARY KEY, subject text NOT NULL, event_type text NOT NULL,
    aggregate_id uuid NOT NULL, aggregate_sequence bigint NOT NULL CHECK (aggregate_sequence > 0),
    payload jsonb NOT NULL CHECK (jsonb_typeof(payload) = 'object'),
    payload_sha256 text NOT NULL CHECK (payload_sha256 ~ '^[0-9a-f]{64}$'),
    created_at timestamptz NOT NULL DEFAULT now(), published_at timestamptz,
    UNIQUE (aggregate_id, aggregate_sequence)
);
CREATE TABLE inbox_events (
    consumer text NOT NULL, event_id uuid NOT NULL, aggregate_id uuid NOT NULL,
    aggregate_sequence bigint NOT NULL CHECK (aggregate_sequence > 0),
    payload_sha256 text NOT NULL CHECK (payload_sha256 ~ '^[0-9a-f]{64}$'),
    processed_at timestamptz NOT NULL DEFAULT now(), PRIMARY KEY (consumer, event_id),
    UNIQUE (consumer, aggregate_id, aggregate_sequence)
);
CREATE TABLE inbox_watermarks (
    consumer text NOT NULL, aggregate_id uuid NOT NULL, last_sequence bigint NOT NULL CHECK (last_sequence >= 0),
    updated_at timestamptz NOT NULL DEFAULT now(), PRIMARY KEY (consumer, aggregate_id)
);
CREATE TABLE candidates (
    candidate_id uuid PRIMARY KEY, candidate_kind text NOT NULL CHECK (candidate_kind IN ('environment', 'evaluation')),
    course_id uuid NOT NULL, revision bigint NOT NULL CHECK (revision > 0), state text NOT NULL,
    content_sha256 text NOT NULL CHECK (content_sha256 ~ '^[0-9a-f]{64}$'), contract jsonb NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(), UNIQUE (candidate_id, revision),
    CHECK (jsonb_typeof(contract) = 'object')
);
CREATE TABLE candidate_approvals (
    approval_id uuid PRIMARY KEY, candidate_id uuid NOT NULL, candidate_revision bigint NOT NULL CHECK (candidate_revision > 0),
    decision text NOT NULL CHECK (decision IN ('approved', 'rejected')), actor_id uuid NOT NULL,
    decision_sha256 text NOT NULL CHECK (decision_sha256 ~ '^[0-9a-f]{64}$'), contract jsonb NOT NULL,
    decided_at timestamptz NOT NULL, UNIQUE (candidate_id, candidate_revision), CHECK (jsonb_typeof(contract) = 'object')
);
CREATE TABLE environment_template_releases (
    release_id uuid PRIMARY KEY, course_id uuid NOT NULL, version bigint NOT NULL CHECK (version > 0),
    environment_candidate_id uuid NOT NULL, candidate_revision bigint NOT NULL CHECK (candidate_revision > 0),
    spec_sha256 text NOT NULL CHECK (spec_sha256 ~ '^[0-9a-f]{64}$'), image_artifact_id uuid,
    contract jsonb NOT NULL, published_at timestamptz NOT NULL, UNIQUE (course_id, version),
    CHECK (jsonb_typeof(contract) = 'object')
);

-- Folded from 0002_control_plane.sql.
CREATE TABLE problem_package_upload_sessions (
    upload_id uuid PRIMARY KEY,
    course_id uuid NOT NULL,
    revision bigint NOT NULL CHECK (revision > 0),
    state text NOT NULL CHECK (state IN ('pending', 'completing', 'completed', 'failed', 'expired')),
    retention_policy_revision bigint NOT NULL CHECK (retention_policy_revision > 0),
    expires_at timestamptz NOT NULL,
    completed_package_id uuid,
    terminal_diagnostic text,
    completion_idempotency_key text,
    completion_request_sha256 text CHECK (completion_request_sha256 ~ '^[0-9a-f]{64}$'),
    completion_lease_token uuid,
    completion_lease_expires_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    CHECK ((state = 'completed') = (completed_package_id IS NOT NULL)),
    CHECK ((completion_idempotency_key IS NULL) = (completion_request_sha256 IS NULL)),
    CHECK ((completion_lease_token IS NULL) = (completion_lease_expires_at IS NULL)),
    CHECK ((state = 'completing') = (completion_lease_token IS NOT NULL))
);

CREATE TABLE problem_package_upload_files (
    upload_id uuid NOT NULL REFERENCES problem_package_upload_sessions(upload_id) ON DELETE RESTRICT,
    ordinal integer NOT NULL CHECK (ordinal >= 0),
    path text NOT NULL,
    object_key text NOT NULL UNIQUE,
    artifact_id uuid UNIQUE,
    object_version text,
    size_bytes bigint NOT NULL CHECK (size_bytes > 0),
    sha256 text NOT NULL CHECK (sha256 ~ '^[0-9a-f]{64}$'),
    media_type text NOT NULL,
    verified_at timestamptz,
    PRIMARY KEY (upload_id, ordinal),
    UNIQUE (upload_id, path),
    CHECK (path <> '' AND object_key <> '' AND media_type <> '')
    ,CHECK ((object_version IS NULL) = (artifact_id IS NULL))
);

CREATE TABLE problem_packages (
    package_id uuid PRIMARY KEY,
    course_id uuid NOT NULL,
    revision bigint NOT NULL CHECK (revision > 0),
    manifest_sha256 text NOT NULL CHECK (manifest_sha256 ~ '^[0-9a-f]{64}$'),
    contract jsonb NOT NULL CHECK (jsonb_typeof(contract) = 'object'),
    completed_at timestamptz NOT NULL,
    UNIQUE (package_id, revision)
);

CREATE TABLE course_llm_policies (
    policy_id uuid PRIMARY KEY,
    course_id uuid NOT NULL,
    revision bigint NOT NULL CHECK (revision > 0),
    contract_sha256 text NOT NULL CHECK (contract_sha256 ~ '^[0-9a-f]{64}$'),
    contract jsonb NOT NULL CHECK (jsonb_typeof(contract) = 'object'),
    activated_at timestamptz NOT NULL,
    superseded_at timestamptz,
    UNIQUE (course_id, revision)
);
CREATE UNIQUE INDEX course_llm_policies_one_active_idx
    ON course_llm_policies(course_id) WHERE superseded_at IS NULL;

ALTER TABLE candidates
    ADD COLUMN run_id uuid,
    ADD COLUMN policy_revision bigint CHECK (policy_revision > 0),
    ADD COLUMN schema_sha256 text CHECK (schema_sha256 ~ '^[0-9a-f]{64}$'),
    ADD COLUMN projected_event_id uuid;
CREATE INDEX candidates_projected_event_idx ON candidates(projected_event_id);

ALTER TABLE candidate_approvals
    DROP CONSTRAINT candidate_approvals_decision_check,
    ADD CONSTRAINT candidate_approvals_decision_check
        CHECK (decision IN ('approved', 'rejected', 'withdrawn'));

CREATE TABLE agent_run_projections (
    run_id uuid PRIMARY KEY,
    course_id uuid NOT NULL,
    revision bigint NOT NULL CHECK (revision > 0),
    state text NOT NULL,
    contract_sha256 text NOT NULL CHECK (contract_sha256 ~ '^[0-9a-f]{64}$'),
    contract jsonb NOT NULL CHECK (jsonb_typeof(contract) = 'object'),
    projected_event_id uuid NOT NULL UNIQUE,
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE image_artifact_projections (
    image_artifact_id uuid PRIMARY KEY,
    runtime_kind text NOT NULL CHECK (runtime_kind IN ('container', 'virtual_machine')),
    artifact_sha256 text NOT NULL CHECK (artifact_sha256 ~ '^[0-9a-f]{64}$'),
    artifact jsonb NOT NULL CHECK (jsonb_typeof(artifact) = 'object'),
    policy_evaluation jsonb NOT NULL CHECK (jsonb_typeof(policy_evaluation) = 'object'),
    projected_event_id uuid NOT NULL UNIQUE,
    projected_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE release_withdrawals (
    release_id uuid PRIMARY KEY REFERENCES environment_template_releases(release_id) ON DELETE RESTRICT,
    release_version bigint NOT NULL CHECK (release_version > 0),
    actor_id uuid NOT NULL,
    reason_code text NOT NULL CHECK (reason_code <> ''),
    withdrawn_at timestamptz NOT NULL,
    contract jsonb NOT NULL CHECK (jsonb_typeof(contract) = 'object')
);

CREATE TABLE sse_course_cursors (
    course_id uuid PRIMARY KEY,
    last_sequence bigint NOT NULL CHECK (last_sequence >= 0)
);

CREATE TABLE sse_events (
    course_id uuid NOT NULL,
    sequence bigint NOT NULL CHECK (sequence > 0),
    event_type text NOT NULL,
    aggregate_id uuid NOT NULL,
    aggregate_revision bigint NOT NULL CHECK (aggregate_revision > 0),
    payload jsonb NOT NULL CHECK (jsonb_typeof(payload) = 'object'),
    payload_sha256 text NOT NULL CHECK (payload_sha256 ~ '^[0-9a-f]{64}$'),
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (course_id, sequence)
);
CREATE INDEX sse_events_retention_idx ON sse_events(created_at);

CREATE TABLE object_cleanup_ledger (
    object_key text NOT NULL,
    object_version text NOT NULL,
    upload_id uuid NOT NULL,
    attempts integer NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    next_attempt_at timestamptz NOT NULL DEFAULT now(),
    completed_at timestamptz,
    last_diagnostic text,
    PRIMARY KEY (object_key, object_version)
);

CREATE INDEX object_cleanup_due_idx ON object_cleanup_ledger(next_attempt_at)
    WHERE completed_at IS NULL;

-- Folded from 0003_container_build_projections.sql.
CREATE TABLE container_build_projections (
    build_request_id uuid PRIMARY KEY,
    course_id uuid NOT NULL,
    candidate_id uuid NOT NULL,
    candidate_revision bigint NOT NULL CHECK (candidate_revision > 0),
    candidate_sha256 text NOT NULL CHECK (candidate_sha256 ~ '^[0-9a-f]{64}$'),
    approval_id uuid NOT NULL UNIQUE,
    command_sha256 text NOT NULL UNIQUE CHECK (command_sha256 ~ '^[0-9a-f]{64}$'),
    state text NOT NULL CHECK (state IN ('requested', 'succeeded', 'failed', 'cancelled')),
    image_artifact_id uuid UNIQUE,
    terminal_diagnostic text,
    cleanup_verified boolean,
    contract jsonb NOT NULL CHECK (jsonb_typeof(contract) = 'object'),
    terminal_event_id uuid UNIQUE,
    created_at timestamptz NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now(),
    completed_at timestamptz,
    UNIQUE (candidate_id, candidate_revision),
    CHECK ((state = 'succeeded') = (image_artifact_id IS NOT NULL)),
    CHECK ((state IN ('failed', 'cancelled')) = (terminal_diagnostic IS NOT NULL AND cleanup_verified IS NOT NULL)),
    CHECK ((state = 'requested') = (completed_at IS NULL AND terminal_event_id IS NULL))
);

CREATE INDEX container_build_projections_course_idx
    ON container_build_projections (course_id, candidate_id, candidate_revision);
