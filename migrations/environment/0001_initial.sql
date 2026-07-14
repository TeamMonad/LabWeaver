CREATE TABLE idempotency_ledger (operation text NOT NULL, idempotency_key text NOT NULL, request_sha256 text NOT NULL CHECK (request_sha256 ~ '^[0-9a-f]{64}$'), state text NOT NULL CHECK (state IN ('in_progress', 'completed')), result jsonb, created_at timestamptz NOT NULL DEFAULT now(), completed_at timestamptz, PRIMARY KEY (operation, idempotency_key), CHECK ((state = 'completed') = (result IS NOT NULL AND completed_at IS NOT NULL)));
CREATE TABLE outbox_events (event_id uuid PRIMARY KEY, subject text NOT NULL, event_type text NOT NULL, aggregate_id uuid NOT NULL, aggregate_sequence bigint NOT NULL CHECK (aggregate_sequence > 0), payload jsonb NOT NULL CHECK (jsonb_typeof(payload) = 'object'), payload_sha256 text NOT NULL CHECK (payload_sha256 ~ '^[0-9a-f]{64}$'), created_at timestamptz NOT NULL DEFAULT now(), published_at timestamptz, UNIQUE (aggregate_id, aggregate_sequence));
CREATE TABLE inbox_events (consumer text NOT NULL, event_id uuid NOT NULL, aggregate_id uuid NOT NULL, aggregate_sequence bigint NOT NULL CHECK (aggregate_sequence > 0), payload_sha256 text NOT NULL CHECK (payload_sha256 ~ '^[0-9a-f]{64}$'), processed_at timestamptz NOT NULL DEFAULT now(), PRIMARY KEY (consumer, event_id));
CREATE TABLE inbox_watermarks (consumer text NOT NULL, aggregate_id uuid NOT NULL, last_sequence bigint NOT NULL CHECK (last_sequence > 0), updated_at timestamptz NOT NULL DEFAULT now(), PRIMARY KEY (consumer, aggregate_id));
CREATE TABLE environment_instances (
    environment_id uuid PRIMARY KEY, release_id uuid NOT NULL, generation bigint NOT NULL CHECK (generation > 0),
    observed_generation bigint NOT NULL CHECK (observed_generation >= 0 AND observed_generation <= generation),
    desired_state text NOT NULL, observed_state text NOT NULL, provider_binding text NOT NULL,
    lease_id uuid, revision bigint NOT NULL CHECK (revision > 0), terminal_diagnostic text,
    contract jsonb NOT NULL, created_at timestamptz NOT NULL DEFAULT now(), updated_at timestamptz NOT NULL DEFAULT now(),
    CHECK (jsonb_typeof(contract) = 'object')
);
CREATE TABLE environment_operations (
    operation_id uuid PRIMARY KEY, environment_id uuid NOT NULL, operation_kind text NOT NULL,
    expected_revision bigint NOT NULL CHECK (expected_revision > 0), target_generation bigint NOT NULL CHECK (target_generation > 0),
    state text NOT NULL, retry_count integer NOT NULL DEFAULT 0 CHECK (retry_count >= 0),
    lease_owner text, heartbeat_at timestamptz, diagnostic text, contract jsonb NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(), finished_at timestamptz, CHECK (jsonb_typeof(contract) = 'object')
);
