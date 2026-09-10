-- Environment-owned metering state and durable Resource delivery attempts.
-- The state projection records when each meter became eligible; deliveries are
-- idempotent interval facts and remain auditable until Resource acknowledges them.

CREATE TABLE resource_metering_state (
    environment_id uuid PRIMARY KEY REFERENCES environment_instances(environment_id),
    contract jsonb NOT NULL CHECK (jsonb_typeof(contract) = 'object')
);

CREATE TABLE resource_meter_deliveries (
    delivery_id uuid PRIMARY KEY,
    environment_id uuid NOT NULL REFERENCES environment_instances(environment_id),
    source_event_id uuid NOT NULL UNIQUE,
    kind text NOT NULL CHECK (kind IN ('compute', 'storage')),
    measured_from timestamptz NOT NULL,
    measured_until timestamptz NOT NULL,
    request jsonb NOT NULL CHECK (jsonb_typeof(request) = 'object'),
    state text NOT NULL CHECK (state IN ('pending', 'delivered')),
    attempts integer NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    next_attempt_at timestamptz NOT NULL,
    delivered_at timestamptz,
    last_diagnostic_code text,
    UNIQUE (environment_id, kind, measured_from, measured_until),
    CHECK (measured_until > measured_from),
    CHECK ((state = 'delivered') = (delivered_at IS NOT NULL)),
    CHECK (last_diagnostic_code IS NULL OR length(last_diagnostic_code) BETWEEN 1 AND 128)
);

CREATE INDEX resource_meter_deliveries_due_idx
    ON resource_meter_deliveries (next_attempt_at, delivery_id)
    WHERE state = 'pending';
