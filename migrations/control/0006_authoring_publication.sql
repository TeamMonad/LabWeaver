-- Downstream publication is a mutable projection of one immutable authoring approval.
-- Control is the sole writer of this coordinator state. Environment and Evaluation return
-- their own publication results; the approval itself remains append-only.
CREATE TABLE control.authoring_approval_publications (
    approval_id uuid PRIMARY KEY REFERENCES control.authoring_approvals(approval_id),
    project_id uuid NOT NULL,
    course_id uuid,
    state text NOT NULL CHECK (state IN ('pending', 'publishing', 'ready', 'failed')),
    environment_release_id uuid,
    evaluation_release_id uuid,
    evaluation_release_revision bigint CHECK (evaluation_release_revision IS NULL OR evaluation_release_revision > 0),
    diagnostic_code text,
    updated_at timestamptz NOT NULL,
    revision bigint NOT NULL CHECK (revision > 0),
    contract jsonb NOT NULL CHECK (jsonb_typeof(contract) = 'object'),
    CHECK (
        state <> 'ready'
        OR (
            environment_release_id IS NOT NULL
            AND evaluation_release_id IS NOT NULL
            AND evaluation_release_revision IS NOT NULL
        )
    ),
    CHECK (
        state <> 'failed'
        OR COALESCE(btrim(diagnostic_code), '') <> ''
    )
);

CREATE INDEX authoring_approval_publications_project_idx
    ON control.authoring_approval_publications (project_id, updated_at DESC, approval_id);

CREATE INDEX authoring_approval_publications_environment_release_idx
    ON control.authoring_approval_publications (environment_release_id)
    WHERE environment_release_id IS NOT NULL;

CREATE INDEX authoring_approval_publications_evaluation_release_idx
    ON control.authoring_approval_publications (evaluation_release_id)
    WHERE evaluation_release_id IS NOT NULL;
