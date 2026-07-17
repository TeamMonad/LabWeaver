CREATE TABLE release_projections (
    release_id uuid PRIMARY KEY,
    course_id uuid NOT NULL,
    release_version bigint NOT NULL CHECK (release_version > 0),
    provider_binding text NOT NULL,
    projection_sha256 text NOT NULL CHECK (projection_sha256 ~ '^[0-9a-f]{64}$'),
    contract jsonb NOT NULL CHECK (jsonb_typeof(contract) = 'object'),
    projected_event_id uuid NOT NULL UNIQUE,
    aggregate_sequence bigint NOT NULL DEFAULT 1 CHECK (aggregate_sequence IN (1, 2)),
    withdrawn_at timestamptz,
    withdrawal_reason_code text,
    withdrawal_event_id uuid UNIQUE,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    CHECK (
        (aggregate_sequence = 1 AND withdrawn_at IS NULL AND withdrawal_reason_code IS NULL AND withdrawal_event_id IS NULL)
        OR
        (aggregate_sequence = 2 AND withdrawn_at IS NOT NULL AND withdrawal_reason_code IS NOT NULL AND withdrawal_event_id IS NOT NULL)
    ),
    UNIQUE (release_id, release_version)
);

CREATE INDEX release_projections_course_idx
    ON release_projections (course_id, release_version);
