CREATE TABLE idempotency_ledger (operation text NOT NULL, idempotency_key text NOT NULL, request_sha256 text NOT NULL CHECK (request_sha256 ~ '^[0-9a-f]{64}$'), state text NOT NULL CHECK (state IN ('in_progress', 'completed')), result jsonb, created_at timestamptz NOT NULL DEFAULT now(), completed_at timestamptz, PRIMARY KEY (operation, idempotency_key), CHECK ((state = 'completed') = (result IS NOT NULL AND completed_at IS NOT NULL)));
CREATE TABLE outbox_events (event_id uuid PRIMARY KEY, subject text NOT NULL, event_type text NOT NULL, aggregate_id uuid NOT NULL, aggregate_sequence bigint NOT NULL CHECK (aggregate_sequence > 0), payload jsonb NOT NULL CHECK (jsonb_typeof(payload) = 'object'), payload_sha256 text NOT NULL CHECK (payload_sha256 ~ '^[0-9a-f]{64}$'), created_at timestamptz NOT NULL DEFAULT now(), published_at timestamptz, UNIQUE (aggregate_id, aggregate_sequence));
CREATE TABLE inbox_events (consumer text NOT NULL, event_id uuid NOT NULL, aggregate_id uuid NOT NULL, aggregate_sequence bigint NOT NULL CHECK (aggregate_sequence > 0), payload_sha256 text NOT NULL CHECK (payload_sha256 ~ '^[0-9a-f]{64}$'), processed_at timestamptz NOT NULL DEFAULT now(), PRIMARY KEY (consumer, event_id), UNIQUE (consumer, aggregate_id, aggregate_sequence));
CREATE TABLE inbox_watermarks (consumer text NOT NULL, aggregate_id uuid NOT NULL, last_sequence bigint NOT NULL CHECK (last_sequence >= 0), updated_at timestamptz NOT NULL DEFAULT now(), PRIMARY KEY (consumer, aggregate_id));
CREATE TABLE access_grants (
    grant_id uuid PRIMARY KEY, actor_id uuid NOT NULL, course_id uuid NOT NULL, environment_id uuid NOT NULL,
    revision bigint NOT NULL CHECK (revision > 0), state text NOT NULL,
    not_before timestamptz NOT NULL, expires_at timestamptz NOT NULL, contract jsonb NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(), revoked_at timestamptz,
    CHECK (expires_at > not_before), CHECK (jsonb_typeof(contract) = 'object')
);
CREATE TABLE gateway_sessions (
    session_id uuid PRIMARY KEY, grant_id uuid NOT NULL, grant_revision bigint NOT NULL CHECK (grant_revision > 0),
    actor_id uuid NOT NULL, endpoint_id uuid NOT NULL, state text NOT NULL,
    started_at timestamptz NOT NULL, expires_at timestamptz NOT NULL, terminated_at timestamptz,
    contract jsonb NOT NULL, CHECK (expires_at > started_at), CHECK (jsonb_typeof(contract) = 'object')
);
