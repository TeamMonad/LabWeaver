-- Durable Agent-owned import jobs. The upload_id is the stable idempotency key
-- supplied by Control. Catalog publication is fenced by this row's lease and
-- must be committed with the terminal job state.

CREATE TABLE agent.platform_image_import_jobs (
    upload_id uuid PRIMARY KEY,
    request_json jsonb NOT NULL,
    request_sha256 text NOT NULL CHECK (request_sha256 ~ '^[0-9a-f]{64}$'),
    state text NOT NULL CHECK (state IN ('queued', 'running', 'succeeded', 'failed', 'cancelled')),
    revision bigint NOT NULL DEFAULT 1 CHECK (revision > 0),
    worker_id text,
    lease_token uuid,
    lease_expires_at timestamptz,
    cancellation_requested boolean NOT NULL DEFAULT false,
    diagnostic text,
    catalog_id uuid,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    completed_at timestamptz,
    CHECK ((lease_token IS NULL) = (lease_expires_at IS NULL)),
    CHECK ((state = 'running') = (lease_token IS NOT NULL)),
    CHECK ((state = 'succeeded') = (catalog_id IS NOT NULL)),
    CHECK (state <> 'succeeded' OR diagnostic IS NULL),
    CHECK (state NOT IN ('succeeded', 'failed', 'cancelled') OR completed_at IS NOT NULL)
);

CREATE INDEX platform_image_import_jobs_due_idx
    ON agent.platform_image_import_jobs (state, lease_expires_at, updated_at);
