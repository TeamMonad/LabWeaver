ALTER TABLE build_commands
    ADD COLUMN revision bigint NOT NULL DEFAULT 1 CHECK (revision > 0),
    ADD COLUMN cancellation_audit_version smallint NOT NULL DEFAULT 0
        CHECK (cancellation_audit_version IN (0, 1)),
    ADD COLUMN cancellation_actor_id uuid,
    ADD COLUMN cancellation_authority_san_uri text,
    ADD COLUMN cancellation_requested_at timestamptz,
    ADD CONSTRAINT build_commands_cancellation_audit_complete CHECK (
        (
            NOT cancellation_requested
            AND cancellation_actor_id IS NULL
            AND cancellation_authority_san_uri IS NULL
            AND cancellation_requested_at IS NULL
        ) OR (
            cancellation_requested
            AND (
                (
                    cancellation_audit_version = 0
                    AND cancellation_actor_id IS NULL
                    AND cancellation_authority_san_uri IS NULL
                    AND cancellation_requested_at IS NULL
                ) OR (
                    cancellation_audit_version = 1
                    AND cancellation_actor_id IS NOT NULL
                    AND cancellation_authority_san_uri IS NOT NULL
                    AND cancellation_requested_at IS NOT NULL
                )
            )
        )
    ),
    ADD CONSTRAINT build_commands_cancellation_authority_exact CHECK (
        cancellation_authority_san_uri IS NULL
        OR cancellation_authority_san_uri = 'spiffe://labweaver/control-service'
    );

-- Existing rows are explicitly version 0. Every row inserted after this forward
-- migration defaults to the actor-attributed v1 cancellation audit contract.
ALTER TABLE build_commands
    ALTER COLUMN cancellation_audit_version SET DEFAULT 1;

CREATE TABLE build_executor_fences (
    build_request_id uuid PRIMARY KEY,
    highest_generation integer NOT NULL CHECK (highest_generation > 0),
    lease_token uuid NOT NULL,
    tombstone_generation integer CHECK (
        tombstone_generation > 0 AND tombstone_generation <= highest_generation
    ),
    last_stage text NOT NULL CHECK (
        last_stage IN ('ensure_private_project','build','scan','sign','publish','cleanup')
    ),
    last_stage_rank smallint NOT NULL CHECK (last_stage_rank BETWEEN 1 AND 6),
    last_request_id text NOT NULL CHECK (last_request_id ~ '^[0-9a-f]{64}$'),
    last_response jsonb CHECK (last_response IS NULL OR jsonb_typeof(last_response) = 'object'),
    deadline_at timestamptz NOT NULL,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    CHECK (
        (last_stage = 'cleanup' AND tombstone_generation = highest_generation)
        OR (last_stage <> 'cleanup' AND tombstone_generation IS NULL)
    )
);
