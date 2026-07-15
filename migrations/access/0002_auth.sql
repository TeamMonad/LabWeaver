CREATE TABLE actors (
    actor_id uuid PRIMARY KEY,
    issuer text NOT NULL CHECK (issuer ~ '^https://'),
    subject_sha256 text NOT NULL CHECK (subject_sha256 ~ '^[0-9a-f]{64}$'),
    created_at timestamptz NOT NULL DEFAULT now(),
    disabled_at timestamptz,
    UNIQUE (issuer, subject_sha256)
);

CREATE TABLE course_memberships (
    course_id uuid NOT NULL,
    actor_id uuid NOT NULL REFERENCES actors(actor_id),
    role text NOT NULL CHECK (role IN ('teacher', 'student', 'platform_admin')),
    state text NOT NULL CHECK (state IN ('active', 'suspended', 'revoked')),
    revision bigint NOT NULL CHECK (revision > 0),
    expires_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (course_id, actor_id, role),
    UNIQUE (course_id, actor_id, revision)
);

CREATE TABLE project_memberships (
    course_id uuid NOT NULL,
    project_id uuid NOT NULL,
    actor_id uuid NOT NULL REFERENCES actors(actor_id),
    role text NOT NULL CHECK (role IN ('teacher', 'student', 'platform_admin')),
    state text NOT NULL CHECK (state IN ('active', 'suspended', 'revoked')),
    revision bigint NOT NULL CHECK (revision > 0),
    expires_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (project_id, actor_id, role),
    UNIQUE (project_id, actor_id, revision)
);

CREATE TABLE service_identities (
    service_identity_id uuid PRIMARY KEY,
    san_uri text NOT NULL UNIQUE CHECK (san_uri LIKE 'spiffe://%'),
    service_name text NOT NULL,
    state text NOT NULL CHECK (state IN ('active', 'revoked')),
    revision bigint NOT NULL CHECK (revision > 0),
    expires_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now(),
    revoked_at timestamptz
);

CREATE TABLE oidc_transactions (
    transaction_id uuid PRIMARY KEY,
    state_sha256 text NOT NULL UNIQUE CHECK (state_sha256 ~ '^[0-9a-f]{64}$'),
    encrypted_payload bytea NOT NULL CHECK (octet_length(encrypted_payload) > 28),
    encryption_key_id text NOT NULL,
    expires_at timestamptz NOT NULL,
    consumed_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now(),
    CHECK (expires_at > created_at)
);

CREATE INDEX oidc_transactions_expiry_idx ON oidc_transactions (expires_at) WHERE consumed_at IS NULL;

CREATE TABLE bff_sessions (
    session_id uuid PRIMARY KEY,
    actor_id uuid NOT NULL REFERENCES actors(actor_id),
    platform_roles text[] NOT NULL CHECK (cardinality(platform_roles) > 0),
    oidc_sid_sha256 text CHECK (oidc_sid_sha256 ~ '^[0-9a-f]{64}$'),
    authorization_revision bigint NOT NULL CHECK (authorization_revision > 0),
    issued_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL,
    idle_expires_at timestamptz NOT NULL,
    encrypted_csrf_token bytea NOT NULL CHECK (octet_length(encrypted_csrf_token) > 28),
    csrf_encryption_key_id text NOT NULL,
    encrypted_logout_hint bytea,
    encryption_key_id text,
    revoked_at timestamptz,
    revoke_diagnostic text CHECK (revoke_diagnostic IS NULL OR revoke_diagnostic ~ '^LW_[A-Z0-9_]+$'),
    CHECK (expires_at > issued_at),
    CHECK (idle_expires_at <= expires_at),
    CHECK ((encrypted_logout_hint IS NULL) = (encryption_key_id IS NULL))
);

CREATE INDEX bff_sessions_actor_idx ON bff_sessions (actor_id) WHERE revoked_at IS NULL;
CREATE INDEX bff_sessions_sid_idx ON bff_sessions (oidc_sid_sha256) WHERE revoked_at IS NULL;
CREATE INDEX bff_sessions_expiry_idx ON bff_sessions (expires_at, idle_expires_at) WHERE revoked_at IS NULL;

CREATE TABLE backchannel_logout_events (
    issuer text NOT NULL CHECK (issuer ~ '^https://'),
    jti_sha256 text NOT NULL CHECK (jti_sha256 ~ '^[0-9a-f]{64}$'),
    received_at timestamptz NOT NULL,
    expires_at timestamptz NOT NULL,
    PRIMARY KEY (issuer, jti_sha256),
    CHECK (expires_at > received_at)
);

CREATE INDEX backchannel_logout_events_expiry_idx ON backchannel_logout_events (expires_at);
