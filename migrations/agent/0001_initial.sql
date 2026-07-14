CREATE TABLE idempotency_ledger (operation text NOT NULL, idempotency_key text NOT NULL, request_sha256 text NOT NULL CHECK (request_sha256 ~ '^[0-9a-f]{64}$'), state text NOT NULL CHECK (state IN ('in_progress', 'completed')), result jsonb, created_at timestamptz NOT NULL DEFAULT now(), completed_at timestamptz, PRIMARY KEY (operation, idempotency_key), CHECK ((state = 'completed') = (result IS NOT NULL AND completed_at IS NOT NULL)));
CREATE TABLE outbox_events (event_id uuid PRIMARY KEY, subject text NOT NULL, event_type text NOT NULL, aggregate_id uuid NOT NULL, aggregate_sequence bigint NOT NULL CHECK (aggregate_sequence > 0), payload jsonb NOT NULL CHECK (jsonb_typeof(payload) = 'object'), payload_sha256 text NOT NULL CHECK (payload_sha256 ~ '^[0-9a-f]{64}$'), created_at timestamptz NOT NULL DEFAULT now(), published_at timestamptz, UNIQUE (aggregate_id, aggregate_sequence));
CREATE TABLE inbox_events (consumer text NOT NULL, event_id uuid NOT NULL, aggregate_id uuid NOT NULL, aggregate_sequence bigint NOT NULL CHECK (aggregate_sequence > 0), payload_sha256 text NOT NULL CHECK (payload_sha256 ~ '^[0-9a-f]{64}$'), processed_at timestamptz NOT NULL DEFAULT now(), PRIMARY KEY (consumer, event_id));
CREATE TABLE inbox_watermarks (consumer text NOT NULL, aggregate_id uuid NOT NULL, last_sequence bigint NOT NULL CHECK (last_sequence > 0), updated_at timestamptz NOT NULL DEFAULT now(), PRIMARY KEY (consumer, aggregate_id));
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
