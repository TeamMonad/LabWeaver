-- Existing upload sessions used the finite configured CourseMaterial policy. Preserve that
-- decision explicitly while allowing new sessions to select the permanent policy.
ALTER TABLE problem_package_upload_sessions
    ADD COLUMN retention_choice text NOT NULL DEFAULT 'finite',
    ADD CONSTRAINT problem_package_upload_sessions_retention_choice_check
        CHECK (retention_choice IN ('finite', 'permanent'));
