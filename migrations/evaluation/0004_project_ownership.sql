-- Evaluation releases, runs, and frozen submissions are Project-owned. Course
-- is an optional teaching association for every evaluation aggregate.

ALTER TABLE evaluation_releases
    ADD COLUMN project_id uuid NOT NULL,
    ALTER COLUMN course_id DROP NOT NULL,
    DROP CONSTRAINT evaluation_releases_course_id_candidate_id_approval_id_key,
    DROP CONSTRAINT evaluation_releases_course_id_release_identity_sha256_key,
    ADD CONSTRAINT evaluation_releases_project_id_candidate_id_approval_id_key
        UNIQUE (project_id, candidate_id, approval_id),
    ADD CONSTRAINT evaluation_releases_project_id_release_identity_sha256_key
        UNIQUE (project_id, release_identity_sha256);

ALTER TABLE evaluation_runs
    ADD COLUMN project_id uuid NOT NULL,
    ALTER COLUMN course_id DROP NOT NULL,
    DROP CONSTRAINT evaluation_runs_course_id_idempotency_key_key,
    ADD CONSTRAINT evaluation_runs_project_id_idempotency_key_key
        UNIQUE (project_id, idempotency_key);

ALTER TABLE evaluation_release_withdrawals
    ADD COLUMN project_id uuid NOT NULL,
    ALTER COLUMN course_id DROP NOT NULL;

DROP INDEX evaluation_release_withdrawals_course_time_idx;
CREATE INDEX evaluation_release_withdrawals_project_time_idx
    ON evaluation_release_withdrawals (project_id, withdrawn_at DESC, release_id DESC);

DROP INDEX evaluation_releases_course_published_idx;
CREATE INDEX evaluation_releases_project_published_idx
    ON evaluation_releases (project_id, published_at DESC, release_id DESC);

DROP INDEX evaluation_runs_student_terminal_idx;
CREATE INDEX evaluation_runs_student_project_terminal_idx
    ON evaluation_runs (project_id, actor_id, updated_at DESC, run_id DESC)
    WHERE state IN ('succeeded', 'failed', 'cancelled');
