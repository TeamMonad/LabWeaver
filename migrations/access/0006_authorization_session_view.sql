-- Control reads current, non-secret session validity. Access remains the session writer.
-- The join makes this view read only. No encrypted token, CSRF token, sid or issuer subject is exposed.
CREATE VIEW access.current_authorization_sessions AS
    SELECT encode(sha256(convert_to(b.session_id::text,'UTF8')),'hex') AS session_sha256,b.actor_id,b.expires_at,b.idle_expires_at
    FROM access.bff_sessions b JOIN access.actors a ON a.actor_id=b.actor_id
    WHERE b.revoked_at IS NULL AND a.disabled_at IS NULL;
GRANT SELECT ON access.current_authorization_sessions TO lw_control_runtime;
