-- Durable retry metadata keeps one unconfigured or malformed usage row from
-- blocking settlement of later projects. Pending rows remain retryable after
-- a rate publication or operator correction.

ALTER TABLE resource.resource_usage_records
    ADD COLUMN settlement_attempts integer NOT NULL DEFAULT 0
        CHECK (settlement_attempts >= 0),
    ADD COLUMN settlement_next_attempt_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    ADD COLUMN settlement_diagnostic_code text;

CREATE INDEX resource_usage_settlement_due_idx
    ON resource.resource_usage_records (settlement_next_attempt_at, observed_at, usage_record_id)
    WHERE settlement = 'pending';
