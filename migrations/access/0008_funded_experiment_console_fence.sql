-- Issue #207: the original Resource lease may fund either environment class.
-- Keep the complete fence and deadline; only legacy Experiment may omit it.
ALTER TABLE console_capabilities
    DROP CONSTRAINT console_capabilities_check2;

ALTER TABLE console_capabilities
    ADD CONSTRAINT console_capabilities_lease_fence_check CHECK (
        (environment_class = 'experiment'
            AND lease_id IS NULL AND lease_revision IS NULL AND lease_expires_at IS NULL)
        OR
        (lease_id IS NOT NULL AND lease_revision IS NOT NULL
            AND lease_expires_at IS NOT NULL AND expires_at <= lease_expires_at)
    );
