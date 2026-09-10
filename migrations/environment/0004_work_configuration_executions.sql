-- Persist the Environment-owned execution identity and JSON request/target/
-- receipt records for Work configuration runs.

CREATE TABLE work_configuration_executions (
    run_id uuid PRIMARY KEY,
    execution_id uuid NOT NULL UNIQUE,
    project_id uuid NOT NULL,
    environment_id uuid NOT NULL REFERENCES environment_instances(environment_id),
    plan_id uuid NOT NULL,
    plan_revision bigint NOT NULL CHECK (plan_revision > 0),
    request_json jsonb NOT NULL CHECK (jsonb_typeof(request_json) = 'object'),
    target_json jsonb NOT NULL CHECK (jsonb_typeof(target_json) = 'object'),
    receipt_json jsonb NOT NULL CHECK (jsonb_typeof(receipt_json) = 'object'),
    revision bigint NOT NULL DEFAULT 1 CHECK (revision > 0),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX work_configuration_executions_project_idx
    ON work_configuration_executions (project_id, created_at DESC);

CREATE INDEX work_configuration_executions_environment_idx
    ON work_configuration_executions (environment_id, created_at DESC);

CREATE INDEX work_configuration_executions_reconcile_state_idx
    ON work_configuration_executions ((receipt_json->>'state'), created_at)
    WHERE (receipt_json->>'state') IN ('running', 'cancelling', 'cleanup_pending');
