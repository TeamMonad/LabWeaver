-- Durable Agent-owned authoring sandbox attempt checkpoints.
--
-- One row binds one authoring attempt generation to its admitted Resource task lease and the
-- exact Kubernetes objects Agent owns.  The admitted execution binding and the observed object
-- identities are written before any terminal result is accepted, so a resumed worker can prove
-- the same reservation, clean up only its own objects and never execute one attempt twice.
CREATE TABLE agent.authoring_sandbox_attempts (
    run_id uuid NOT NULL,
    track text NOT NULL CHECK (track IN ('environment', 'evaluation', 'work_configuration')),
    attempt_number bigint NOT NULL CHECK (attempt_number > 0),
    task_run_id uuid NOT NULL UNIQUE,
    execution_generation bigint NOT NULL CHECK (execution_generation > 0),
    namespace text NOT NULL CHECK (length(namespace) BETWEEN 1 AND 63),
    workload_name text NOT NULL CHECK (length(workload_name) BETWEEN 1 AND 63),
    binding jsonb NOT NULL CHECK (jsonb_typeof(binding) = 'object'),
    objects jsonb NOT NULL DEFAULT '[]'::jsonb CHECK (jsonb_typeof(objects) = 'array'),
    state text NOT NULL CHECK (
        state IN ('creating', 'submitted', 'terminal', 'cleanup_confirmed', 'released', 'failed')
    ),
    result_object_key text,
    result_sha256 text CHECK (result_sha256 IS NULL OR result_sha256 ~ '^[0-9a-f]{64}$'),
    result_size_bytes bigint CHECK (result_size_bytes IS NULL OR result_size_bytes >= 0),
    exit_code integer,
    diagnostic_code text
        CHECK (diagnostic_code IS NULL OR length(diagnostic_code) BETWEEN 1 AND 128),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (run_id, track, attempt_number)
);

CREATE INDEX authoring_sandbox_attempts_task_run_idx
    ON agent.authoring_sandbox_attempts (task_run_id, run_id, track, attempt_number);
