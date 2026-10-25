-- One-time browser-bound handoffs and per-grant-host runtime credentials.
CREATE TABLE runtime_capabilities (
    handoff_sha256 text PRIMARY KEY CHECK (handoff_sha256 ~ '^[0-9a-f]{64}$'),
    binding_sha256 text NOT NULL CHECK (binding_sha256 ~ '^[0-9a-f]{64}$'),
    capability_sha256 text UNIQUE CHECK (capability_sha256 ~ '^[0-9a-f]{64}$'),
    endpoint_grant_id uuid NOT NULL REFERENCES endpoint_grants(endpoint_grant_id) ON DELETE CASCADE,
    actor_id uuid NOT NULL REFERENCES actors(actor_id) ON DELETE CASCADE,
    bff_session_id uuid NOT NULL REFERENCES bff_sessions(session_id) ON DELETE CASCADE,
    issued_at timestamptz NOT NULL,
    handoff_expires_at timestamptz NOT NULL,
    expires_at timestamptz NOT NULL,
    consumed_at timestamptz,
    CHECK ((consumed_at IS NULL) = (capability_sha256 IS NULL)),
    CHECK (handoff_expires_at > issued_at AND handoff_expires_at <= issued_at + interval '30 seconds'),
    CHECK (expires_at > issued_at AND expires_at <= issued_at + interval '300 seconds')
);
CREATE INDEX runtime_capabilities_expiry_idx ON runtime_capabilities(expires_at);
