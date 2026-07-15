CREATE TABLE agent_track_work_items (
    run_id uuid NOT NULL,
    track text NOT NULL CHECK (track IN ('environment', 'evaluation')),
    state text NOT NULL CHECK (state IN ('requested', 'running', 'succeeded', 'failed', 'cancelled')),
    input_sha256 text NOT NULL CHECK (input_sha256 ~ '^[0-9a-f]{64}$'),
    attempt_number bigint NOT NULL DEFAULT 0 CHECK (attempt_number >= 0),
    worker_id text,
    lease_token uuid,
    lease_expires_at timestamptz,
    heartbeat_at timestamptz,
    next_retry_at timestamptz NOT NULL DEFAULT now(),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (run_id, track),
    FOREIGN KEY (run_id) REFERENCES agent_runs(run_id) ON DELETE RESTRICT,
    CHECK (
        (worker_id IS NULL) = (lease_token IS NULL)
        AND (worker_id IS NULL) = (lease_expires_at IS NULL)
        AND (worker_id IS NULL) = (heartbeat_at IS NULL)
    ),
    CHECK (
        (state = 'running' AND worker_id IS NOT NULL)
        OR (state <> 'running' AND worker_id IS NULL)
    )
);

CREATE INDEX agent_track_work_items_due_idx
    ON agent_track_work_items (next_retry_at, created_at)
    WHERE state IN ('requested', 'running');

ALTER TABLE agent_runs
    ADD COLUMN cancellation_requested_at timestamptz;
