-- Persist the immutable AgentRun purpose and the optional generated Work plan.
-- Existing rows are authoring runs; their prior Control-dispatch class is the explicit source for
-- the purpose backfill before the old duplicated class column is removed.

ALTER TABLE agent.agent_runs
    ADD COLUMN purpose jsonb,
    ADD COLUMN plan jsonb;

UPDATE agent.agent_runs run
SET purpose = jsonb_build_object(
    'kind', 'authoring',
    'environmentClass', COALESCE(dispatch.expected_environment_class, 'experiment')
)
FROM agent.agent_run_dispatches dispatch
WHERE dispatch.run_id = run.run_id
  AND run.purpose IS NULL;

UPDATE agent.agent_runs
SET purpose = jsonb_build_object('kind', 'authoring', 'environmentClass', 'experiment')
WHERE purpose IS NULL;

UPDATE agent.agent_runs
SET contract = (contract - 'requestedRuntime')
    || jsonb_build_object('purpose', purpose, 'plan', plan);

ALTER TABLE agent.agent_runs
    ALTER COLUMN purpose SET NOT NULL,
    ADD CONSTRAINT agent_runs_purpose_object
        CHECK (jsonb_typeof(purpose) = 'object'),
    ADD CONSTRAINT agent_runs_plan_object
        CHECK (plan IS NULL OR jsonb_typeof(plan) = 'object');

ALTER TABLE agent.agent_run_dispatches
    ADD COLUMN purpose jsonb,
    ADD COLUMN preauthorization jsonb;

UPDATE agent.agent_run_dispatches
SET purpose = jsonb_build_object(
    'kind', 'authoring',
    'environmentClass', expected_environment_class
)
WHERE purpose IS NULL;

UPDATE agent.agent_run_dispatches
SET request = (request - 'requestedRuntime')
    || jsonb_build_object(
        'environmentClass', expected_environment_class
    )
WHERE request ? 'requestedRuntime';

ALTER TABLE agent.agent_run_dispatches
    ALTER COLUMN purpose SET NOT NULL,
    ADD CONSTRAINT agent_run_dispatches_purpose_object
        CHECK (jsonb_typeof(purpose) = 'object'),
    ADD CONSTRAINT agent_run_dispatches_preauthorization_object
        CHECK (preauthorization IS NULL OR jsonb_typeof(preauthorization) = 'object'),
    DROP CONSTRAINT IF EXISTS agent_run_dispatches_expected_environment_class_check,
    DROP COLUMN expected_environment_class;

ALTER TABLE agent.agent_track_work_items
    DROP CONSTRAINT IF EXISTS agent_track_work_items_track_check,
    ADD CONSTRAINT agent_track_work_items_track_check
        CHECK (track IN ('environment', 'evaluation', 'work_configuration'));
