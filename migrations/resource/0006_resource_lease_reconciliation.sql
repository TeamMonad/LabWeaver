ALTER TABLE capacity_claims
    ADD COLUMN lease_synced_revision bigint NOT NULL DEFAULT 0
        CHECK (lease_synced_revision >= 0);

CREATE INDEX capacity_claims_lease_sync_idx
    ON capacity_claims (lease_synced_revision, updated_at)
    WHERE state = 'handed_off';

CREATE INDEX resource_leases_expiry_idx
    ON resource_leases (expires_at, updated_at)
    WHERE state IN ('active', 'expiring');
