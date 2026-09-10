-- Every Evaluation attempt owns one durable Resource task reservation identity.
-- Existing rows cannot be reconstructed safely, so this migration intentionally has no default.

ALTER TABLE evaluation_step_attempts
    ADD COLUMN task_run_id uuid NOT NULL,
    ADD CONSTRAINT evaluation_step_attempts_task_run_id_key UNIQUE (task_run_id);
