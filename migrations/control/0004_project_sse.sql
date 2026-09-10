-- Project-scoped Control events have an independent cursor so a project can
-- stream events even when it has no course association.

CREATE TABLE control.sse_project_cursors (
    project_id uuid PRIMARY KEY,
    last_sequence bigint NOT NULL CHECK (last_sequence >= 0)
);

CREATE TABLE control.sse_project_events (
    project_id uuid NOT NULL,
    sequence bigint NOT NULL CHECK (sequence > 0),
    event_type text NOT NULL,
    aggregate_id uuid NOT NULL,
    aggregate_revision bigint NOT NULL CHECK (aggregate_revision > 0),
    payload jsonb NOT NULL CHECK (jsonb_typeof(payload) = 'object'),
    payload_sha256 text NOT NULL CHECK (payload_sha256 ~ '^[0-9a-f]{64}$'),
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (project_id, sequence)
);

CREATE INDEX sse_project_events_retention_idx ON control.sse_project_events(created_at);
