-- Pricing consent and funded upper bound retain the immutable exact rate lines.
CREATE TABLE resource.credit_cost_snapshots (
    hold_id uuid PRIMARY KEY REFERENCES resource.credit_holds(hold_id),
    request_id uuid NOT NULL UNIQUE REFERENCES resource.resource_requests(request_id),
    snapshot jsonb NOT NULL CHECK (jsonb_typeof(snapshot)='object')
);
CREATE TRIGGER credit_cost_snapshots_immutable BEFORE UPDATE OR DELETE
ON resource.credit_cost_snapshots FOR EACH ROW EXECUTE FUNCTION resource.credit_ledger_immutable();
CREATE TRIGGER credit_cost_snapshots_no_truncate BEFORE TRUNCATE
ON resource.credit_cost_snapshots FOR EACH STATEMENT EXECUTE FUNCTION resource.credit_ledger_immutable();

-- Numeric columns retain the same rational representation as their contract.
ALTER TABLE resource.resource_usage_records ADD COLUMN quantity_divisor integer
    NOT NULL DEFAULT 1 CHECK (quantity_divisor IN (1,1000));

-- Explicitly configured zero prices still produce an authorization and immutable quote.
ALTER TABLE resource.credit_holds DROP CONSTRAINT credit_holds_amount_check;
ALTER TABLE resource.credit_holds ADD CONSTRAINT credit_holds_amount_check CHECK (amount >= 0);
