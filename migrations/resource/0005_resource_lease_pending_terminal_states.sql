-- A rejected approval or failed allocation has no active time window. The Lease remains
-- authoritative and auditable, but it must not be forced to fabricate an activation interval.

ALTER TABLE resource_leases
    DROP CONSTRAINT resource_leases_check,
    ADD CONSTRAINT resource_leases_active_window
        CHECK (
            state <> 'active'
            OR (active_from IS NOT NULL AND expires_at IS NOT NULL AND expires_at > active_from)
        ),
    ADD CONSTRAINT resource_leases_window_pair
        CHECK ((active_from IS NULL) = (expires_at IS NULL)),
    ADD CONSTRAINT resource_leases_non_active_window_order
        CHECK (
            active_from IS NULL
            OR expires_at > active_from
        );
