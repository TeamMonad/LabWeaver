-- Projects are the ownership boundary. A course is optional teaching context.

ALTER TABLE access.project_memberships
    ALTER COLUMN course_id DROP NOT NULL,
    DROP CONSTRAINT project_memberships_pkey,
    ADD CONSTRAINT project_memberships_pkey PRIMARY KEY (project_id, actor_id);

-- Control creates projects and applies the owner-governed membership state
-- machine in one constrained database transaction. The role can only inspect
-- and mutate this Access-owned table; it cannot write any other Access data.
GRANT USAGE ON SCHEMA access TO lw_control_runtime;
GRANT SELECT, INSERT, UPDATE ON access.project_memberships TO lw_control_runtime;
