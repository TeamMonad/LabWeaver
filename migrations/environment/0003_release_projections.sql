CREATE TABLE release_projections (
    release_id uuid PRIMARY KEY,
    course_id uuid NOT NULL,
    release_version bigint NOT NULL CHECK (release_version > 0),
    provider_binding text NOT NULL,
    projection_sha256 text NOT NULL CHECK (projection_sha256 ~ '^[0-9a-f]{64}$'),
    contract jsonb NOT NULL CHECK (jsonb_typeof(contract) = 'object'),
    projected_event_id uuid NOT NULL UNIQUE,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (release_id, release_version)
);

CREATE INDEX release_projections_course_idx
    ON release_projections (course_id, release_version);
