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
