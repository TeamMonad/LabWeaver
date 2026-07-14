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
    processed_at timestamptz NOT NULL DEFAULT now(), PRIMARY KEY (consumer, event_id)
);
CREATE TABLE inbox_watermarks (
    consumer text NOT NULL, aggregate_id uuid NOT NULL, last_sequence bigint NOT NULL CHECK (last_sequence > 0),
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
