-- The final authoring decision is a Control-owned immutable binding. Candidate
-- decisions authorize candidate review; they are not the authorization carried
-- by an isolated Agent build request.

ALTER TABLE control.container_build_projections
    DROP COLUMN approval_id;

CREATE TABLE control.authoring_approvals (
    approval_id uuid PRIMARY KEY,
    project_id uuid NOT NULL,
    course_id uuid,
    revision bigint NOT NULL CHECK (revision > 0),
    package_id uuid NOT NULL,
    package_revision bigint NOT NULL CHECK (package_revision > 0),
    environment_candidate_id uuid NOT NULL,
    environment_candidate_revision bigint NOT NULL CHECK (environment_candidate_revision > 0),
    evaluation_candidate_id uuid NOT NULL,
    evaluation_candidate_revision bigint NOT NULL CHECK (evaluation_candidate_revision > 0),
    image_artifact_id uuid NOT NULL,
    actor_id uuid NOT NULL,
    reason text NOT NULL CHECK (length(trim(reason)) BETWEEN 1 AND 500),
    contract jsonb NOT NULL CHECK (jsonb_typeof(contract) = 'object'),
    approved_at timestamptz NOT NULL,
    UNIQUE (project_id, revision),
    UNIQUE (
        project_id,
        package_id,
        package_revision,
        environment_candidate_id,
        environment_candidate_revision,
        evaluation_candidate_id,
        evaluation_candidate_revision
    )
);

CREATE INDEX authoring_approvals_project_idx
    ON control.authoring_approvals (project_id, approved_at DESC, approval_id);

CREATE INDEX authoring_approvals_artifact_idx
    ON control.authoring_approvals (image_artifact_id);
