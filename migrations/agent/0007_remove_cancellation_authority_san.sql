-- Service JWT authentication makes a caller supplied authority SAN redundant.
-- Keep the actor and timestamp audit fields, but remove the obsolete certificate identity.
ALTER TABLE build_commands
    DROP CONSTRAINT build_commands_cancellation_audit_complete,
    DROP CONSTRAINT build_commands_cancellation_authority_exact,
    DROP COLUMN cancellation_authority_san_uri;

ALTER TABLE build_commands
    ADD CONSTRAINT build_commands_cancellation_audit_complete CHECK (
        (
            NOT cancellation_requested
            AND cancellation_actor_id IS NULL
            AND cancellation_requested_at IS NULL
        ) OR (
            cancellation_requested
            AND (
                (
                    cancellation_audit_version = 0
                    AND cancellation_actor_id IS NULL
                    AND cancellation_requested_at IS NULL
                ) OR (
                    cancellation_audit_version = 1
                    AND cancellation_actor_id IS NOT NULL
                    AND cancellation_requested_at IS NOT NULL
                )
            )
        )
    );
