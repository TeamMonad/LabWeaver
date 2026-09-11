-- Evaluation-owned final resource usage deliveries.
--
-- A delivery belongs to one exact StepRun attempt and one durable Resource task
-- identity.  The tuple foreign key prevents a delivery from being attached to
-- a different attempt that happens to reuse either identifier.

ALTER TABLE evaluation_step_attempts
    ADD CONSTRAINT evaluation_step_attempts_step_attempt_task_run_key
        UNIQUE (step_run_id, attempt, task_run_id);

CREATE TABLE resource_meter_deliveries (
    delivery_id uuid PRIMARY KEY,
    step_run_id uuid NOT NULL,
    attempt integer NOT NULL CHECK (attempt > 0),
    task_run_id uuid NOT NULL,
    source_event_id uuid NOT NULL UNIQUE,
    kind text NOT NULL CHECK (kind IN ('compute', 'storage')),
    measured_from timestamptz NOT NULL,
    measured_until timestamptz NOT NULL,
    request jsonb NOT NULL CHECK (jsonb_typeof(request) = 'object'),
    state text NOT NULL CHECK (state IN ('pending', 'delivered')),
    attempts integer NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    next_attempt_at timestamptz NOT NULL,
    delivered_at timestamptz,
    last_diagnostic_code text,
    UNIQUE (task_run_id, kind),
    FOREIGN KEY (step_run_id, attempt, task_run_id)
        REFERENCES evaluation_step_attempts (step_run_id, attempt, task_run_id),
    CHECK (measured_until > measured_from),
    CHECK ((state = 'delivered') = (delivered_at IS NOT NULL)),
    CHECK (last_diagnostic_code IS NULL OR length(last_diagnostic_code) BETWEEN 1 AND 128)
);

CREATE INDEX resource_meter_deliveries_due_idx
    ON resource_meter_deliveries (next_attempt_at, delivery_id)
    WHERE state = 'pending';
