-- Forward migration for the Work AgentRun contract.
--
-- The retained pre-Work baseline predates the dispatch class column.  Those
-- rows were created through the original AgentRun endpoint, whose only
-- runtime class was experiment, so the backfill is an explicit compatibility
-- decision rather than an implicit default in the service.  A fresh baseline
-- already contains the column and this migration is intentionally idempotent.
ALTER TABLE agent_run_dispatches
    ADD COLUMN IF NOT EXISTS expected_environment_class text;

UPDATE agent_run_dispatches
SET expected_environment_class = 'experiment'
WHERE expected_environment_class IS NULL;

ALTER TABLE agent_run_dispatches
    ALTER COLUMN expected_environment_class SET NOT NULL;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1
        FROM pg_constraint
        WHERE conrelid = 'agent.agent_run_dispatches'::regclass
          AND conname = 'agent_run_dispatches_expected_environment_class_check'
    ) THEN
        ALTER TABLE agent_run_dispatches
            ADD CONSTRAINT agent_run_dispatches_expected_environment_class_check
            CHECK (expected_environment_class IN ('experiment', 'work'));
    END IF;
END
$$;
