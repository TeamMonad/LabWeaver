-- Agent-owned runs and build requests belong to a Project. Course is optional
-- teaching context and is retained only when the Project is course-associated.

ALTER TABLE agent_runs
    ADD COLUMN project_id uuid NOT NULL,
    ALTER COLUMN course_id DROP NOT NULL;

ALTER TABLE build_commands
    ADD COLUMN project_id uuid NOT NULL,
    ALTER COLUMN course_id DROP NOT NULL;

CREATE INDEX agent_runs_project_idx
    ON agent_runs (project_id, created_at DESC, run_id);

CREATE INDEX build_commands_project_idx
    ON build_commands (project_id, created_at DESC, build_request_id);
