-- Existing claims have no producer receipt proving partial retirement; keep all dimensions held.
ALTER TABLE resource.capacity_claims
    ADD COLUMN IF NOT EXISTS compute_released boolean NOT NULL DEFAULT false,
    ADD COLUMN IF NOT EXISTS storage_released boolean NOT NULL DEFAULT false;

-- No historical interval or compute-release fact can safely be inferred from old usage rows.
CREATE TABLE IF NOT EXISTS resource.retained_storage_intervals (
    environment_id uuid NOT NULL,
    started_at timestamptz NOT NULL,
    project_id uuid NOT NULL,
    course_id uuid,
    request_id uuid NOT NULL REFERENCES resource.resource_requests(request_id),
    lease_id uuid NOT NULL REFERENCES resource.resource_leases(lease_id),
    generation bigint NOT NULL CHECK (generation > 0),
    measured_until timestamptz NOT NULL,
    priced_until timestamptz NOT NULL,
    storage_bytes bigint NOT NULL CHECK (storage_bytes > 0),
    closed boolean NOT NULL DEFAULT false,
    closed_at timestamptz,
    PRIMARY KEY (environment_id, started_at),
    CHECK (measured_until >= started_at),
    CHECK (closed = (closed_at IS NOT NULL))
);

-- A monotonic producer revision/generation fence. Stale receipts cannot reopen terminal facts.
CREATE TABLE IF NOT EXISTS resource.environment_cleanup_fences (
    lease_id uuid PRIMARY KEY REFERENCES resource.resource_leases(lease_id),
    environment_id uuid NOT NULL,
    revision bigint NOT NULL CHECK (revision > 0),
    generation bigint NOT NULL CHECK (generation > 0),
    compute_released boolean NOT NULL DEFAULT false,
    compute_retired_at timestamptz,
    storage_purged boolean NOT NULL DEFAULT false,
    reactivated_generation bigint,
    reactivated_at timestamptz,
    updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    CHECK ((reactivated_generation IS NULL) = (reactivated_at IS NULL))
);

-- Immutable platform costs reference the ordinary exact Resource charge calculation without
-- creating a project charge, Credits ledger entry, or budget delta.
CREATE TABLE IF NOT EXISTS resource.retained_storage_platform_costs (
    cost_id uuid PRIMARY KEY,
    environment_id uuid NOT NULL,
    started_at timestamptz NOT NULL,
    measured_from timestamptz NOT NULL,
    measured_until timestamptz NOT NULL,
    storage_bytes bigint NOT NULL CHECK (storage_bytes > 0),
    rate_snapshots jsonb NOT NULL,
    amount numeric(38,0) NOT NULL CHECK (amount >= 0),
    currency text NOT NULL CHECK (currency = 'CREDITS'),
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    FOREIGN KEY (environment_id, started_at)
        REFERENCES resource.retained_storage_intervals(environment_id, started_at)
        DEFERRABLE INITIALLY DEFERRED,
    CHECK (measured_until > measured_from),
    UNIQUE (environment_id, started_at, measured_until)
);
COMMENT ON TABLE resource.retained_storage_intervals IS
    'Cumulative storage watermark and terminal closure keyed by Environment and disk start time.';
COMMENT ON TABLE resource.environment_cleanup_fences IS
    'Highest accepted Environment cleanup receipt revision/generation and partial-release facts.';
COMMENT ON TABLE resource.retained_storage_platform_costs IS
    'Immutable retained-storage rate costs borne by the platform, never project Credits or budget.';

DROP TRIGGER IF EXISTS retained_storage_costs_immutable ON resource.retained_storage_platform_costs;
CREATE TRIGGER retained_storage_costs_immutable BEFORE UPDATE OR DELETE ON resource.retained_storage_platform_costs
    FOR EACH STATEMENT EXECUTE FUNCTION resource.credit_ledger_immutable();
DROP TRIGGER IF EXISTS retained_storage_costs_no_truncate ON resource.retained_storage_platform_costs;
CREATE TRIGGER retained_storage_costs_no_truncate BEFORE TRUNCATE ON resource.retained_storage_platform_costs
    FOR EACH STATEMENT EXECUTE FUNCTION resource.credit_ledger_immutable();
