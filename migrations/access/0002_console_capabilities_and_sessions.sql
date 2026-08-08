-- Issue #131: one-time browser console admission and metadata-only sessions.
-- Secret material is AEAD-encrypted; terminal payloads are never persisted.

CREATE TABLE console_capabilities (
    capability_id uuid PRIMARY KEY,
    kind text NOT NULL CHECK (kind IN ('xterm', 'novnc')),
    access_grant_id uuid NOT NULL REFERENCES access_grants(grant_id) ON DELETE RESTRICT,
    access_grant_revision bigint NOT NULL CHECK (access_grant_revision > 0),
    actor_id uuid NOT NULL REFERENCES actors(actor_id) ON DELETE RESTRICT,
    bff_session_id uuid NOT NULL REFERENCES bff_sessions(session_id) ON DELETE RESTRICT,
    course_id uuid NOT NULL,
    environment_id uuid NOT NULL,
    environment_class text NOT NULL CHECK (environment_class IN ('experiment', 'work')),
    environment_revision bigint NOT NULL CHECK (environment_revision > 0),
    lease_id uuid,
    lease_revision bigint CHECK (lease_revision > 0),
    lease_expires_at timestamptz,
    issued_at timestamptz NOT NULL,
    expires_at timestamptz NOT NULL,
    authorization_expires_at timestamptz NOT NULL,
    locator_sha256 text NOT NULL UNIQUE CHECK (locator_sha256 ~ '^[0-9a-f]{64}$'),
    handoff_secret_sha256 text NOT NULL CHECK (handoff_secret_sha256 ~ '^[0-9a-f]{64}$'),
    encrypted_handoff_secret bytea NOT NULL,
    encryption_key_id text NOT NULL CHECK (length(encryption_key_id) BETWEEN 1 AND 128),
    idempotency_scope text NOT NULL CHECK (length(idempotency_scope) BETWEEN 1 AND 256),
    idempotency_key_sha256 text NOT NULL CHECK (idempotency_key_sha256 ~ '^[0-9a-f]{64}$'),
    consumed_at timestamptz,
    session_id uuid,
    secret_scrubbed_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now(),
    CHECK (expires_at = issued_at + interval '30 seconds'),
    CHECK (authorization_expires_at >= expires_at),
    CHECK (
        (environment_class = 'experiment' AND lease_id IS NULL AND lease_revision IS NULL AND lease_expires_at IS NULL)
        OR
        (environment_class = 'work' AND lease_id IS NOT NULL AND lease_revision IS NOT NULL
            AND lease_expires_at IS NOT NULL AND expires_at <= lease_expires_at)
    ),
    CHECK ((consumed_at IS NULL) = (session_id IS NULL)),
    CHECK (
        (secret_scrubbed_at IS NULL AND octet_length(encrypted_handoff_secret) > 28)
        OR (secret_scrubbed_at IS NOT NULL AND octet_length(encrypted_handoff_secret) = 0)
    ),
    CHECK (consumed_at IS NULL OR consumed_at <= expires_at),
    CHECK ((secret_scrubbed_at IS NULL) OR secret_scrubbed_at >= issued_at)
);

CREATE UNIQUE INDEX console_capabilities_idempotency_idx
    ON console_capabilities (idempotency_scope, idempotency_key_sha256);

CREATE INDEX console_capabilities_grant_live_idx
    ON console_capabilities (access_grant_id, expires_at)
    WHERE consumed_at IS NULL;
CREATE INDEX console_capabilities_expiry_idx
    ON console_capabilities (expires_at, capability_id)
    WHERE secret_scrubbed_at IS NULL;

CREATE TABLE console_sessions (
    session_id uuid PRIMARY KEY,
    capability_id uuid NOT NULL UNIQUE REFERENCES console_capabilities(capability_id) ON DELETE RESTRICT,
    kind text NOT NULL CHECK (kind IN ('xterm', 'novnc')),
    bff_session_id uuid NOT NULL REFERENCES bff_sessions(session_id) ON DELETE RESTRICT,
    access_grant_id uuid NOT NULL REFERENCES access_grants(grant_id) ON DELETE RESTRICT,
    access_grant_revision bigint NOT NULL CHECK (access_grant_revision > 0),
    actor_id uuid NOT NULL REFERENCES actors(actor_id) ON DELETE RESTRICT,
    course_id uuid NOT NULL,
    environment_id uuid NOT NULL,
    environment_revision bigint NOT NULL CHECK (environment_revision > 0),
    lease_id uuid,
    lease_revision bigint CHECK (lease_revision > 0),
    lease_expires_at timestamptz,
    proxy_owner text NOT NULL CHECK (length(proxy_owner) BETWEEN 1 AND 128),
    revision bigint NOT NULL DEFAULT 1 CHECK (revision > 0),
    state text NOT NULL CHECK (state IN ('opening', 'active', 'terminating', 'termination_overdue', 'closed')),
    opened_at timestamptz NOT NULL,
    authorization_expires_at timestamptz NOT NULL,
    activated_at timestamptz,
    termination_requested_at timestamptz,
    terminate_by timestamptz,
    closed_at timestamptz,
    diagnostic_code text CHECK (diagnostic_code IS NULL OR diagnostic_code ~ '^LW_[A-Z0-9_]+$'),
    CHECK (authorization_expires_at > opened_at),
    CHECK ((lease_id IS NULL) = (lease_revision IS NULL) AND (lease_id IS NULL) = (lease_expires_at IS NULL)),
    CHECK (lease_expires_at IS NULL OR authorization_expires_at <= lease_expires_at),
    CHECK (
        (state IN ('opening', 'active') AND termination_requested_at IS NULL AND terminate_by IS NULL AND closed_at IS NULL)
        OR (state = 'terminating' AND termination_requested_at IS NOT NULL AND terminate_by IS NOT NULL AND closed_at IS NULL)
        OR (state = 'termination_overdue' AND termination_requested_at IS NOT NULL AND terminate_by IS NOT NULL
            AND closed_at IS NULL AND diagnostic_code = 'LW_CONSOLE_TERMINATION_OVERDUE')
        OR (state = 'closed' AND closed_at IS NOT NULL AND diagnostic_code IS NOT NULL)
    ),
    CHECK (terminate_by IS NULL OR terminate_by <= termination_requested_at + interval '60 seconds')
);

ALTER TABLE console_capabilities
    ADD CONSTRAINT console_capabilities_session_fk
    FOREIGN KEY (session_id) REFERENCES console_sessions(session_id) ON DELETE RESTRICT;

CREATE INDEX console_sessions_grant_live_idx
    ON console_sessions (access_grant_id, state)
    WHERE state IN ('opening', 'active', 'terminating', 'termination_overdue');
CREATE INDEX console_sessions_bff_live_idx
    ON console_sessions (bff_session_id, state)
    WHERE state IN ('opening', 'active', 'terminating', 'termination_overdue');
CREATE INDEX console_sessions_environment_live_idx
    ON console_sessions (environment_id, state)
    WHERE state IN ('opening', 'active', 'terminating', 'termination_overdue');
CREATE INDEX console_sessions_lease_live_idx
    ON console_sessions (lease_id, state)
    WHERE lease_id IS NOT NULL AND state IN ('opening', 'active', 'terminating', 'termination_overdue');
CREATE INDEX console_sessions_termination_idx
    ON console_sessions (terminate_by, session_id)
    WHERE state = 'terminating';
