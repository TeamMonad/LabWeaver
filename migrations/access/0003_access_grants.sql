CREATE TABLE ssh_public_keys (
    key_id uuid PRIMARY KEY,
    actor_id uuid NOT NULL REFERENCES actors(actor_id),
    fingerprint_sha256 text NOT NULL UNIQUE CHECK (fingerprint_sha256 ~ '^SHA256:[A-Za-z0-9+/]+$'),
    algorithm text NOT NULL CHECK (algorithm IN ('ed25519', 'security_key_ed25519', 'rsa_sha2')),
    rsa_bits integer CHECK ((algorithm = 'rsa_sha2' AND rsa_bits >= 3072) OR (algorithm <> 'rsa_sha2' AND rsa_bits IS NULL)),
    normalized_openssh text NOT NULL CHECK (length(normalized_openssh) BETWEEN 32 AND 16384 AND normalized_openssh !~ E'[\\r\\n]'),
    revision bigint NOT NULL DEFAULT 1 CHECK (revision > 0),
    created_at timestamptz NOT NULL DEFAULT now(),
    revoked_at timestamptz,
    revoke_reason_code text CHECK (revoke_reason_code IS NULL OR revoke_reason_code ~ '^LW_[A-Z0-9_]+$'),
    CHECK ((revoked_at IS NULL) = (revoke_reason_code IS NULL))
);

ALTER TABLE idempotency_ledger
    ADD COLUMN scope_id text NOT NULL DEFAULT 'legacy:unscoped'
        CHECK (length(scope_id) BETWEEN 1 AND 256);
ALTER TABLE idempotency_ledger DROP CONSTRAINT idempotency_ledger_pkey;
ALTER TABLE idempotency_ledger
    ADD PRIMARY KEY (operation, scope_id, idempotency_key);
ALTER TABLE idempotency_ledger ALTER COLUMN scope_id DROP DEFAULT;

CREATE INDEX ssh_public_keys_actor_active_idx ON ssh_public_keys (actor_id, created_at, key_id)
    WHERE revoked_at IS NULL;

ALTER TABLE access_grants
    ADD COLUMN environment_revision bigint NOT NULL DEFAULT 1 CHECK (environment_revision > 0),
    ADD COLUMN updated_at timestamptz NOT NULL DEFAULT now(),
    ADD COLUMN last_stream_sequence bigint NOT NULL DEFAULT 1 CHECK (last_stream_sequence > 0),
    ADD COLUMN reason_code text,
    ADD COLUMN activation_attempts integer NOT NULL DEFAULT 0 CHECK (activation_attempts >= 0),
    ADD COLUMN last_activation_diagnostic text CHECK (last_activation_diagnostic IS NULL OR last_activation_diagnostic ~ '^LW_[A-Z0-9_]+$');

ALTER TABLE access_grants ADD CONSTRAINT access_grants_actor_fk
    FOREIGN KEY (actor_id) REFERENCES actors(actor_id) ON DELETE RESTRICT;

CREATE SEQUENCE access_stream_sequence AS bigint MINVALUE 1 START WITH 1;

ALTER TABLE access_grants ADD CONSTRAINT access_grants_state_v1
    CHECK (state IN ('requested', 'active', 'denied', 'expired', 'revoked'));
ALTER TABLE access_grants ADD CONSTRAINT access_grants_terminal_facts_v1
    CHECK (
        (state = 'revoked' AND revoked_at IS NOT NULL AND reason_code IS NOT NULL)
        OR (state = 'denied' AND revoked_at IS NULL AND reason_code IS NOT NULL)
        OR (state = 'expired' AND revoked_at IS NULL AND reason_code = 'expired')
        OR (state IN ('requested', 'active') AND revoked_at IS NULL)
    );

CREATE UNIQUE INDEX access_grants_actor_environment_live_idx
    ON access_grants (actor_id, environment_id)
    WHERE state IN ('requested', 'active');
CREATE INDEX access_grants_expiry_idx ON access_grants (expires_at, grant_id)
    WHERE state = 'active';

CREATE TABLE endpoint_grants (
    endpoint_grant_id uuid PRIMARY KEY,
    grant_id uuid NOT NULL REFERENCES access_grants(grant_id) ON DELETE RESTRICT,
    endpoint_id uuid NOT NULL,
    endpoint_revision bigint NOT NULL CHECK (endpoint_revision > 0),
    protocol text NOT NULL CHECK (protocol IN ('http', 'https', 'ssh')),
    action text NOT NULL DEFAULT 'connect' CHECK (action = 'connect'),
    health text NOT NULL CHECK (health IN ('healthy', 'unhealthy', 'removed')),
    alias text,
    expires_at timestamptz NOT NULL,
    contract jsonb NOT NULL CHECK (jsonb_typeof(contract) = 'object'),
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (grant_id, endpoint_id),
    UNIQUE (alias),
    CHECK (
        (protocol = 'ssh' AND alias ~ '^lw-[a-z2-7]{20}$')
        OR (protocol <> 'ssh' AND alias IS NULL)
    )
);

CREATE INDEX endpoint_grants_endpoint_idx ON endpoint_grants (endpoint_id, endpoint_revision);

CREATE TABLE access_grant_activation_jobs (
    grant_id uuid PRIMARY KEY REFERENCES access_grants(grant_id) ON DELETE CASCADE,
    state text NOT NULL CHECK (state IN ('pending', 'leased', 'retry', 'completed', 'failed')),
    attempts integer NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    next_attempt_at timestamptz NOT NULL DEFAULT now(),
    lease_owner text,
    lease_token uuid,
    lease_expires_at timestamptz,
    last_diagnostic text CHECK (last_diagnostic IS NULL OR last_diagnostic ~ '^LW_[A-Z0-9_]+$'),
    updated_at timestamptz NOT NULL DEFAULT now(),
    CHECK ((state = 'leased') = (lease_owner IS NOT NULL AND lease_token IS NOT NULL AND lease_expires_at IS NOT NULL))
);

CREATE INDEX access_grant_activation_due_idx
    ON access_grant_activation_jobs (next_attempt_at, grant_id)
    WHERE state IN ('pending', 'retry');

CREATE INDEX access_grant_activation_lease_expiry_idx
    ON access_grant_activation_jobs (lease_expires_at, grant_id)
    WHERE state = 'leased';

CREATE TABLE ssh_authorizations (
    authorization_id uuid PRIMARY KEY,
    token_sha256 text NOT NULL UNIQUE CHECK (token_sha256 ~ '^[0-9a-f]{64}$'),
    grant_id uuid NOT NULL REFERENCES access_grants(grant_id) ON DELETE RESTRICT,
    grant_revision bigint NOT NULL CHECK (grant_revision > 0),
    endpoint_grant_id uuid NOT NULL REFERENCES endpoint_grants(endpoint_grant_id) ON DELETE RESTRICT,
    key_id uuid NOT NULL REFERENCES ssh_public_keys(key_id) ON DELETE RESTRICT,
    gateway_identity text NOT NULL CHECK (gateway_identity LIKE 'spiffe://%'),
    connection_id text NOT NULL CHECK (length(connection_id) BETWEEN 1 AND 128),
    source_address_sha256 text NOT NULL CHECK (source_address_sha256 ~ '^[0-9a-f]{64}$'),
    issued_at timestamptz NOT NULL,
    expires_at timestamptz NOT NULL,
    consumed_at timestamptz,
    session_id uuid,
    CHECK (expires_at > issued_at),
    CHECK ((consumed_at IS NULL) = (session_id IS NULL))
);

CREATE INDEX ssh_authorizations_expiry_idx ON ssh_authorizations (expires_at)
    WHERE consumed_at IS NULL;

ALTER TABLE gateway_sessions
    ADD COLUMN endpoint_grant_id uuid REFERENCES endpoint_grants(endpoint_grant_id) ON DELETE RESTRICT,
    ADD COLUMN key_id uuid REFERENCES ssh_public_keys(key_id) ON DELETE RESTRICT,
    ADD COLUMN gateway_identity text,
    ADD COLUMN connection_id text,
    ADD COLUMN revision bigint NOT NULL DEFAULT 1 CHECK (revision > 0),
    ADD COLUMN last_heartbeat_at timestamptz,
    ADD COLUMN termination_requested_at timestamptz,
    ADD COLUMN terminate_by timestamptz,
    ADD COLUMN close_reason_code text;

UPDATE gateway_sessions SET last_heartbeat_at = started_at WHERE last_heartbeat_at IS NULL;
ALTER TABLE gateway_sessions ALTER COLUMN last_heartbeat_at SET NOT NULL;
ALTER TABLE gateway_sessions ALTER COLUMN endpoint_grant_id SET NOT NULL;
ALTER TABLE gateway_sessions ALTER COLUMN key_id SET NOT NULL;
ALTER TABLE gateway_sessions ALTER COLUMN gateway_identity SET NOT NULL;
ALTER TABLE gateway_sessions ALTER COLUMN connection_id SET NOT NULL;
ALTER TABLE gateway_sessions ADD CONSTRAINT gateway_sessions_grant_fk
    FOREIGN KEY (grant_id) REFERENCES access_grants(grant_id) ON DELETE RESTRICT;
ALTER TABLE gateway_sessions ADD CONSTRAINT gateway_sessions_state_v1
    CHECK (state IN ('active', 'terminating', 'termination_overdue', 'closed'));
ALTER TABLE gateway_sessions ADD CONSTRAINT gateway_sessions_termination_v1
    CHECK (
        (state = 'active' AND termination_requested_at IS NULL AND terminate_by IS NULL AND terminated_at IS NULL)
        OR (state = 'terminating' AND termination_requested_at IS NOT NULL AND terminate_by IS NOT NULL AND terminated_at IS NULL)
        OR (state = 'termination_overdue' AND termination_requested_at IS NOT NULL AND terminate_by IS NOT NULL AND terminated_at IS NULL AND close_reason_code = 'termination_overdue')
        OR (state = 'closed' AND terminated_at IS NOT NULL AND close_reason_code IS NOT NULL)
    );
ALTER TABLE gateway_sessions ADD CONSTRAINT gateway_sessions_termination_deadline_v1
    CHECK (
        terminate_by IS NULL
        OR terminate_by <= termination_requested_at + interval '60 seconds'
    );

CREATE UNIQUE INDEX gateway_sessions_connection_idx
    ON gateway_sessions (gateway_identity, connection_id)
    WHERE gateway_identity IS NOT NULL AND connection_id IS NOT NULL;
CREATE INDEX gateway_sessions_grant_live_idx ON gateway_sessions (grant_id, state)
    WHERE state IN ('active', 'terminating', 'termination_overdue');
CREATE INDEX gateway_sessions_key_live_idx ON gateway_sessions (key_id, state)
    WHERE state IN ('active', 'terminating', 'termination_overdue');
CREATE INDEX gateway_sessions_termination_idx ON gateway_sessions (terminate_by, session_id)
    WHERE state = 'terminating';
