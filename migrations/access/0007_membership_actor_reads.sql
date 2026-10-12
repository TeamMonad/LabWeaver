-- Control reads only the non-secret actor fields used by member governance.
REVOKE SELECT ON access.actors FROM lw_control_runtime;
GRANT SELECT (actor_id,username,display_name,disabled_at) ON access.actors TO lw_control_runtime;
DO $$ BEGIN
    IF EXISTS (SELECT FROM pg_roles WHERE rolname='lw_resource_runtime') THEN
        GRANT USAGE ON SCHEMA access TO lw_resource_runtime;
        GRANT SELECT (actor_id,disabled_at) ON access.actors TO lw_resource_runtime;
        GRANT SELECT ON access.current_authorization_sessions TO lw_resource_runtime;
    END IF;
END $$;
