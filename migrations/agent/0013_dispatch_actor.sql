-- Control-authenticated actor that authorized one Agent dispatch.
--
-- The actor authorizes and is attributed for the one-shot Resource task reservations created by
-- sandboxed authoring attempts. The column is nullable only for pre-existing pre-release rows;
-- the worker fails closed when a claimed dispatch has no actor.
ALTER TABLE agent.agent_run_dispatches
    ADD COLUMN actor_id uuid;
