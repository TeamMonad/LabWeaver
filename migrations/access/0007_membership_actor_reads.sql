-- Control reads only the non-secret actor fields used by member governance.
REVOKE SELECT ON access.actors FROM lw_control_runtime;
GRANT SELECT (actor_id,username,display_name,disabled_at) ON access.actors TO lw_control_runtime;
