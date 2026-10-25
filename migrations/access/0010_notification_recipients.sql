-- PII stays outside public actor metadata and is sourced only from verified OIDC login.
CREATE TABLE access.actor_notification_emails (
    actor_id uuid PRIMARY KEY REFERENCES access.actors(actor_id),
    verified_email text,
    updated_at timestamptz NOT NULL DEFAULT date_trunc('milliseconds',clock_timestamp()),
    CHECK (verified_email IS NULL OR (length(verified_email) BETWEEN 3 AND 254 AND verified_email !~ '[[:cntrl:]]'))
);
CREATE VIEW access.notification_recipients AS
    SELECT e.actor_id,e.verified_email
    FROM access.actor_notification_emails e JOIN access.actors a USING(actor_id)
    WHERE a.disabled_at IS NULL;
REVOKE ALL ON access.actor_notification_emails,access.notification_recipients FROM PUBLIC;
DO $$ BEGIN
    IF EXISTS (SELECT FROM pg_roles WHERE rolname='lw_access_runtime') THEN
        GRANT SELECT,INSERT,UPDATE,DELETE ON access.actor_notification_emails TO lw_access_runtime;
    END IF;
    IF EXISTS (SELECT FROM pg_roles WHERE rolname='lw_control_runtime') THEN
        GRANT USAGE ON SCHEMA access TO lw_control_runtime;
        GRANT SELECT ON access.notification_recipients TO lw_control_runtime;
    END IF;
END $$;
