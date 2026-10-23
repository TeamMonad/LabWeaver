-- User-confirmed rule: release unknown preauthorization after one hour, never erase usage.
ALTER TABLE resource.credit_holds
    ADD COLUMN unknown_since timestamptz,
    ADD COLUMN reconciliation_due_at timestamptz,
    ADD COLUMN expired_unknown_at timestamptz;
-- Existing immutable unknown entries supply the original observation; retries are not the origin.
UPDATE resource.credit_holds h
SET unknown_since=original.first_unknown,
    reconciliation_due_at=original.first_unknown+interval '1 hour'
FROM (
    SELECT hold_id,min(created_at) AS first_unknown FROM resource.credit_ledger_entries
    WHERE contract->>'kind'='unknown' AND hold_id IS NOT NULL GROUP BY hold_id
) original
WHERE h.hold_id=original.hold_id AND h.measurement_unknown;
CREATE INDEX credit_holds_unknown_deadline ON resource.credit_holds(reconciliation_due_at)
    WHERE measurement_unknown AND expired_unknown_at IS NULL;
