-- Every Container experiment ships its own Evaluation runner image built through the same
-- single-image BuildKit pipeline as its environment image. Build projections therefore carry an
-- explicit target discriminator so the two builds for one candidate pair cannot collide.

ALTER TABLE control.container_build_projections
    ADD COLUMN target text;

UPDATE control.container_build_projections SET target = 'environment';

ALTER TABLE control.container_build_projections
    ALTER COLUMN target SET NOT NULL,
    ADD CONSTRAINT container_build_projections_target_check
        CHECK (target IN ('environment', 'evaluation_runner')),
    DROP CONSTRAINT container_build_projections_candidate_id_candidate_revision_key,
    ADD CONSTRAINT container_build_projections_candidate_revision_target_key
        UNIQUE (candidate_id, candidate_revision, target);

ALTER TABLE control.authoring_approvals
    ADD COLUMN evaluation_runner_image_artifact_id uuid;

CREATE INDEX authoring_approvals_evaluation_runner_artifact_idx
    ON control.authoring_approvals (evaluation_runner_image_artifact_id)
    WHERE evaluation_runner_image_artifact_id IS NOT NULL;
