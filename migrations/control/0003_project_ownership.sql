-- Project is the required ownership boundary for authoring and execution inputs.
-- Course remains optional teaching context. Existing pre-v3 rows are not migrated;
-- the v3 baseline is expected to be rebuilt before applying this hard cut.

ALTER TABLE candidates
    ADD COLUMN project_id uuid NOT NULL,
    ALTER COLUMN course_id DROP NOT NULL;

CREATE INDEX candidates_project_idx
    ON candidates (project_id, candidate_kind, created_at DESC, candidate_id);

ALTER TABLE environment_template_releases
    ADD COLUMN project_id uuid NOT NULL,
    ALTER COLUMN course_id DROP NOT NULL,
    DROP CONSTRAINT environment_template_releases_course_id_version_key,
    ADD CONSTRAINT environment_template_releases_project_id_version_key
        UNIQUE (project_id, version);

CREATE INDEX environment_template_releases_project_idx
    ON environment_template_releases (project_id, version DESC, release_id);

ALTER TABLE problem_package_upload_sessions
    ADD COLUMN project_id uuid NOT NULL,
    ALTER COLUMN course_id DROP NOT NULL;

ALTER TABLE problem_packages
    ADD COLUMN project_id uuid NOT NULL,
    ALTER COLUMN course_id DROP NOT NULL;

ALTER TABLE course_llm_policies
    RENAME TO project_llm_policies;

ALTER TABLE project_llm_policies
    ADD COLUMN project_id uuid NOT NULL,
    ALTER COLUMN course_id DROP NOT NULL,
    DROP CONSTRAINT course_llm_policies_course_id_revision_key,
    ADD CONSTRAINT project_llm_policies_project_id_revision_key
        UNIQUE (project_id, revision);

DROP INDEX course_llm_policies_one_active_idx;
CREATE UNIQUE INDEX project_llm_policies_one_active_idx
    ON project_llm_policies (project_id) WHERE superseded_at IS NULL;

ALTER TABLE agent_run_projections
    ADD COLUMN project_id uuid NOT NULL,
    ALTER COLUMN course_id DROP NOT NULL;

ALTER TABLE release_withdrawals
    ADD COLUMN project_id uuid NOT NULL,
    ADD COLUMN course_id uuid;

ALTER TABLE container_build_projections
    ADD COLUMN project_id uuid NOT NULL,
    ALTER COLUMN course_id DROP NOT NULL;

DROP INDEX container_build_projections_course_idx;
CREATE INDEX container_build_projections_project_idx
    ON container_build_projections (project_id, candidate_id, candidate_revision);
