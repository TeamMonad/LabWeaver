-- Each row is an immutable funded interval on the original Environment lease/hold.
-- No operational price or balance is seeded.
CREATE TABLE resource.environment_credit_windows (
 lease_id uuid NOT NULL REFERENCES resource.resource_leases(lease_id),
 lease_revision bigint NOT NULL CHECK(lease_revision>0),
 request_id uuid NOT NULL REFERENCES resource.resource_requests(request_id),
 claim_id uuid NOT NULL REFERENCES resource.capacity_claims(claim_id),
 approval_id uuid NOT NULL REFERENCES resource.resource_approvals(approval_id),
 hold_id uuid NOT NULL REFERENCES resource.credit_holds(hold_id),
 covered_from timestamptz NOT NULL,
 covered_until timestamptz NOT NULL CHECK(covered_until>covered_from),
 estimate jsonb NOT NULL CHECK(jsonb_typeof(estimate)='object'),
 created_at timestamptz NOT NULL DEFAULT now(),
 PRIMARY KEY(lease_id,lease_revision),
 UNIQUE(lease_id,covered_from,covered_until)
);
CREATE INDEX environment_credit_windows_request_idx ON resource.environment_credit_windows(request_id,lease_revision DESC);
CREATE TRIGGER environment_credit_windows_immutable BEFORE UPDATE OR DELETE ON resource.environment_credit_windows
 FOR EACH STATEMENT EXECUTE FUNCTION resource.credit_ledger_immutable();
CREATE TRIGGER environment_credit_windows_no_truncate BEFORE TRUNCATE ON resource.environment_credit_windows
 FOR EACH STATEMENT EXECUTE FUNCTION resource.credit_ledger_immutable();
