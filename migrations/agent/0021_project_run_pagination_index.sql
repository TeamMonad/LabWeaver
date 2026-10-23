-- Keyset pagination for project-scoped AgentRun history.
CREATE INDEX agent_runs_project_created_run_idx
    ON agent.agent_runs (project_id, created_at DESC, run_id DESC);
