-- Resource may verify the immutable approved command and current build execution fence.
-- Agent receives no wallet or ledger write privilege.
DO $$
DECLARE role_name text;
BEGIN
 FOREACH role_name IN ARRAY ARRAY['lw_resource_owner','lw_resource_runtime'] LOOP
  IF EXISTS(SELECT 1 FROM pg_roles WHERE rolname=role_name) THEN
   EXECUTE format('GRANT USAGE ON SCHEMA agent TO %I',role_name);
   EXECUTE format('GRANT SELECT(build_request_id,command,state,attempt,lease_token,lease_expires_at,cancellation_requested) ON agent.build_commands TO %I',role_name);
  END IF;
 END LOOP;
END $$;
