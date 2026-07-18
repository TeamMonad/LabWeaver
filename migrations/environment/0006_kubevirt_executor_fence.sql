CREATE TABLE kubevirt_executor_fences (
    environment_id uuid PRIMARY KEY,
    highest_generation bigint NOT NULL CHECK (highest_generation > 0),
    operation_id uuid NOT NULL,
    provider_step integer NOT NULL CHECK (provider_step >= 0),
    attempt integer NOT NULL CHECK (attempt > 0),
    tombstoned boolean NOT NULL DEFAULT false,
    last_action text NOT NULL CHECK (
        last_action IN ('validate','build','provision','observe','start','stop','restart','reset','configure','cleanup')
    ),
    last_request_id text NOT NULL CHECK (last_request_id ~ '^[0-9a-f]{64}$'),
    last_response jsonb CHECK (last_response IS NULL OR jsonb_typeof(last_response) = 'object'),
    deadline_at timestamptz NOT NULL,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    CHECK (NOT tombstoned OR last_action = 'cleanup')
);
