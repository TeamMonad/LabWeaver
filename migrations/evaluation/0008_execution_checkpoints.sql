-- Durable execution checkpoints used to recover an attempt after a worker restart.
-- The execution checkpoint keeps opaque object references plus the immutable,
-- payload-free request owned by the concrete executor. Materializer URLs and
-- SSH credentials remain in Kubernetes Secrets and never cross this boundary.

ALTER TABLE evaluation_step_attempts
    ADD COLUMN execution_started_at timestamptz,
    ADD COLUMN execution_terminated_at timestamptz,
    ADD COLUMN terminal_completion jsonb,
    ADD COLUMN execution_resources jsonb;

ALTER TABLE evaluation_step_attempts
    ADD CONSTRAINT evaluation_step_attempts_execution_checkpoint_shape
    CHECK (
        (terminal_completion IS NULL OR jsonb_typeof(terminal_completion) = 'object')
        AND
        (execution_resources IS NULL OR jsonb_typeof(execution_resources) = 'object')
        AND
        (execution_started_at IS NULL OR execution_resources IS NOT NULL)
        AND
        (execution_terminated_at IS NULL OR execution_resources IS NOT NULL)
        AND
        (terminal_completion IS NULL OR execution_resources IS NOT NULL)
    );
