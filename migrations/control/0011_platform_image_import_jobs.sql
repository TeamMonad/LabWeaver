-- Durable Control-owned state for asynchronous platform image imports.
-- The object identity remains in this row so a retry can never select a newer
-- upload. completion_lease_token fences workers that lost their lease.

ALTER TABLE control.platform_image_upload_sessions
    DROP CONSTRAINT IF EXISTS platform_image_upload_sessions_state_check,
    DROP CONSTRAINT IF EXISTS platform_image_upload_sessions_check3;

ALTER TABLE control.platform_image_upload_sessions
    ADD COLUMN revision bigint NOT NULL DEFAULT 1 CHECK (revision > 0),
    ADD COLUMN cancel_requested boolean NOT NULL DEFAULT false,
    ADD COLUMN cleanup_versions_next_attempt_at timestamptz NOT NULL DEFAULT now();

ALTER TABLE control.platform_image_upload_sessions
    ADD CONSTRAINT platform_image_upload_sessions_state_check
        CHECK (state IN (
            'pending', 'queued', 'freezing', 'importing', 'cancelling',
            'imported', 'failed', 'cancelled'
        )),
    ADD CONSTRAINT platform_image_upload_sessions_completion_lease_token_check
        CHECK (
            (state IN ('freezing', 'importing', 'cancelling')) =
            (completion_lease_token IS NOT NULL)
        );

CREATE INDEX platform_image_upload_sessions_worker_idx
    ON control.platform_image_upload_sessions
        (state, completion_lease_expires_at, updated_at);

-- After the signed PUT expires, reconcile versions of this exact staged key. An upload that
-- started before expiry may still commit later, so reconciliation remains periodic.
CREATE INDEX platform_image_upload_sessions_cleanup_versions_idx
    ON control.platform_image_upload_sessions (cleanup_versions_next_attempt_at, expires_at, upload_id)
    WHERE state IN ('imported', 'failed', 'cancelled');
