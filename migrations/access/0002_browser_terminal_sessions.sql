CREATE TABLE access.browser_terminal_sessions (
    session_id uuid PRIMARY KEY,
    endpoint_grant_id uuid NOT NULL REFERENCES access.endpoint_grants(endpoint_grant_id),
    access_grant_id uuid NOT NULL REFERENCES access.access_grants(grant_id),
    actor_id uuid NOT NULL,
    course_id uuid NOT NULL,
    environment_id uuid NOT NULL,
    environment_revision bigint NOT NULL CHECK (environment_revision > 0),
    endpoint_revision bigint NOT NULL CHECK (endpoint_revision > 0),
    state text NOT NULL CHECK (state IN ('opening', 'active', 'terminating', 'closed', 'failed')),
    opened_at timestamptz NOT NULL,
    activated_at timestamptz,
    last_heartbeat_at timestamptz NOT NULL,
    expires_at timestamptz NOT NULL,
    closed_at timestamptz,
    diagnostic_code text,
    CHECK (expires_at > opened_at),
    CHECK ((state IN ('closed', 'failed')) = (closed_at IS NOT NULL))
);

CREATE INDEX browser_terminal_sessions_active_idx
    ON access.browser_terminal_sessions (expires_at, last_heartbeat_at)
    WHERE state IN ('opening', 'active', 'terminating');

CREATE INDEX browser_terminal_sessions_grant_idx
    ON access.browser_terminal_sessions (access_grant_id, state);
