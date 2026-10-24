-- Detailed model diagnostics are separate from durable usage, grades and checkpoints.
CREATE TABLE agent.model_diagnostics (
    run_id uuid NOT NULL REFERENCES agent.agent_runs(run_id),
    track text NOT NULL CHECK (track IN ('environment','evaluation','work_configuration')),
    attempt bigint NOT NULL CHECK (attempt > 0),
    detail jsonb NOT NULL CHECK (jsonb_typeof(detail) = 'object'),
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    expires_at timestamptz NOT NULL DEFAULT (clock_timestamp() + interval '30 days'),
    PRIMARY KEY (run_id, track, attempt),
    CHECK (expires_at > created_at)
);
CREATE INDEX model_diagnostics_expiry ON agent.model_diagnostics(expires_at);
