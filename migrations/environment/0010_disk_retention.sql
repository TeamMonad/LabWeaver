-- Disk retention is an Environment fact, never a reuse of Resource's compute hold.
CREATE TABLE retained_disk_usage (
    environment_id uuid NOT NULL,
    started_at timestamptz NOT NULL,
    measured_until timestamptz NOT NULL,
    storage_bytes numeric(20,0) NOT NULL CHECK (storage_bytes > 0),
    closed_at timestamptz,
    PRIMARY KEY (environment_id, started_at),
    CHECK (measured_until >= started_at),
    CHECK (closed_at IS NULL OR closed_at = measured_until)
);
CREATE INDEX environment_retained_disk_expiry_idx
    ON environment_instances ((contract->'diskRetention'->>'expiresAt'))
    WHERE (observed_state='deleted' AND contract->'diskRetention'->>'state'='retained')
       OR (observed_state='stopped' AND contract->'diskRetention'->>'state'='restored'
           AND (contract->'diskRetention'->>'reactivatedAt') IS NULL);
