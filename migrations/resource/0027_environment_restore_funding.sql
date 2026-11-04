-- Each restored disk generation has one immutable admission and one fresh hold.
CREATE TABLE resource.environment_restore_funding (
    lease_id uuid NOT NULL REFERENCES resource.resource_leases(lease_id),
    restored_generation bigint NOT NULL CHECK (restored_generation > 0),
    execution_generation bigint NOT NULL CHECK (execution_generation = restored_generation + 1),
    lease_revision bigint NOT NULL CHECK (lease_revision > 0),
    request jsonb NOT NULL,
    result jsonb NOT NULL,
    hold_id uuid NOT NULL UNIQUE REFERENCES resource.credit_holds(hold_id),
    PRIMARY KEY (lease_id, restored_generation),
    UNIQUE (lease_id, lease_revision)
);
