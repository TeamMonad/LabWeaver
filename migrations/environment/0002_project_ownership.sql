-- Every Environment instance and release projection is owned by a Project.
-- Course is optional teaching context and must not define lifecycle ownership.

ALTER TABLE environment_instances
    ADD COLUMN project_id uuid NOT NULL,
    ALTER COLUMN course_id DROP NOT NULL;

DROP INDEX environment_instances_owner_course_idx;
CREATE INDEX environment_instances_owner_project_idx
    ON environment_instances (project_id, owner_actor_id, created_at DESC);

ALTER TABLE release_projections
    ADD COLUMN project_id uuid NOT NULL,
    ALTER COLUMN course_id DROP NOT NULL;

DROP INDEX release_projections_course_idx;
CREATE INDEX release_projections_project_idx
    ON release_projections (project_id, release_version);
