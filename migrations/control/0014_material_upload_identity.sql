-- Unverified uploads have no content identity until Control reads actual bytes.
ALTER TABLE control.problem_package_upload_files ALTER COLUMN sha256 DROP NOT NULL;
UPDATE control.problem_package_upload_files SET sha256=NULL WHERE verified_at IS NULL;
ALTER TABLE control.problem_package_upload_sessions
    DROP CONSTRAINT problem_package_upload_sessions_state_check;
ALTER TABLE control.problem_package_upload_sessions
    ADD CONSTRAINT problem_package_upload_sessions_state_check
    CHECK (state IN ('pending','completing','completed','failed','expired','cancelled')),
    ADD COLUMN cleanup_versions_next_attempt_at timestamptz NOT NULL DEFAULT now(),
    ADD COLUMN completion_expected_revision bigint CHECK (completion_expected_revision>0);
CREATE INDEX material_upload_cleanup_due_idx
    ON control.problem_package_upload_sessions(cleanup_versions_next_attempt_at,expires_at)
    WHERE state IN ('failed','expired','cancelled');
