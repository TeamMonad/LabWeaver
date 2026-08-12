DO $bootstrap$
DECLARE
    role_name text;
    role_superuser boolean;
    role_inherit boolean;
    role_create_role boolean;
    role_create_db boolean;
    role_can_login boolean;
    role_replication boolean;
    role_bypass_rls boolean;
    expected_login boolean;
BEGIN
    FOREACH role_name IN ARRAY ARRAY[
        'lw_release_coordinator', 'lw_audit_projection',
        'lw_control_owner', 'lw_control_migration', 'lw_control_runtime',
        'lw_access_owner', 'lw_access_migration', 'lw_access_runtime',
        'lw_environment_owner', 'lw_environment_migration', 'lw_environment_runtime',
        'lw_agent_owner', 'lw_agent_migration', 'lw_agent_runtime',
        'lw_evaluation_owner', 'lw_evaluation_migration', 'lw_evaluation_runtime',
        'lw_resource_owner', 'lw_resource_migration', 'lw_resource_runtime'
    ] LOOP
        expected_login := role_name NOT LIKE '%_owner';
        SELECT
            rolsuper, rolinherit, rolcreaterole, rolcreatedb,
            rolcanlogin, rolreplication, rolbypassrls
          INTO
            role_superuser, role_inherit, role_create_role,
            role_create_db, role_can_login, role_replication, role_bypass_rls
          FROM pg_roles
         WHERE rolname = role_name;

        IF NOT FOUND THEN
            EXECUTE format('CREATE ROLE %I', role_name);

            -- A role created by the bounded deployment identity is already
            -- NOSUPERUSER by default. Do not spell NOSUPERUSER here: on
            -- PostgreSQL, a non-superuser cannot change that attribute on an
            -- existing role, even when it has CREATEROLE. The later
            -- validation still rejects any pre-existing superuser role.
            IF role_name LIKE '%_owner' THEN
                EXECUTE format('ALTER ROLE %I NOLOGIN NOINHERIT NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS', role_name);
            ELSE
                EXECUTE format('ALTER ROLE %I LOGIN NOINHERIT NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS', role_name);
            END IF;
            EXECUTE format('ALTER ROLE %I RESET ALL', role_name);
        ELSE
            IF role_superuser
                OR role_inherit
                OR role_create_role
                OR role_create_db
                OR role_can_login IS DISTINCT FROM expected_login
                OR role_replication
                OR role_bypass_rls THEN
                RAISE EXCEPTION 'PLATFORM_BOOTSTRAP_ROLE_ATTRIBUTE_MISMATCH: %', role_name;
            END IF;
        END IF;
    END LOOP;
END
$bootstrap$;

-- The deployment identity is deliberately bounded: it is not a superuser,
-- but it must be able to SET ROLE to each domain owner while applying the
-- reviewed baseline.  Foundation creates this LOGIN role before this script
-- runs; granting only the owner roles keeps runtime identities unchanged.
DO $deployment_admin$
DECLARE
    owner_role text;
BEGIN
    IF current_user = 'postgres-admin' THEN
        FOREACH owner_role IN ARRAY ARRAY[
            'lw_release_coordinator', 'lw_audit_projection',
            'lw_control_owner', 'lw_access_owner', 'lw_environment_owner',
            'lw_agent_owner', 'lw_evaluation_owner', 'lw_resource_owner'
        ] LOOP
            IF NOT EXISTS (
                SELECT 1
                  FROM pg_auth_members membership
                  JOIN pg_roles parent ON parent.oid = membership.roleid
                  JOIN pg_roles member ON member.oid = membership.member
                 WHERE parent.rolname = owner_role
                   AND member.rolname = current_user
            ) THEN
                EXECUTE format('GRANT %I TO %I', owner_role, current_user);
            END IF;
        END LOOP;
    END IF;
END
$deployment_admin$;

DO $public_schema_contract$
BEGIN
    IF EXISTS (
        SELECT 1
          FROM pg_namespace namespace
         WHERE namespace.nspname = 'public'
           AND (
               namespace.nspacl IS NULL
               OR EXISTS (
                   SELECT 1
                     FROM unnest(namespace.nspacl) acl
                    WHERE acl::text LIKE '=%'
               )
           )
    ) THEN
        RAISE EXCEPTION 'PLATFORM_BOOTSTRAP_PUBLIC_SCHEMA_ACL_INVALID';
    END IF;
END
$public_schema_contract$;

CREATE SCHEMA IF NOT EXISTS platform_meta AUTHORIZATION lw_release_coordinator;
CREATE SCHEMA IF NOT EXISTS shared_audit AUTHORIZATION lw_audit_projection;
CREATE SCHEMA IF NOT EXISTS control AUTHORIZATION lw_control_owner;
CREATE SCHEMA IF NOT EXISTS access AUTHORIZATION lw_access_owner;
CREATE SCHEMA IF NOT EXISTS environment AUTHORIZATION lw_environment_owner;
CREATE SCHEMA IF NOT EXISTS agent AUTHORIZATION lw_agent_owner;
CREATE SCHEMA IF NOT EXISTS evaluation AUTHORIZATION lw_evaluation_owner;
CREATE SCHEMA IF NOT EXISTS resource AUTHORIZATION lw_resource_owner;

ALTER SCHEMA platform_meta OWNER TO lw_release_coordinator;
ALTER SCHEMA shared_audit OWNER TO lw_audit_projection;
ALTER SCHEMA control OWNER TO lw_control_owner;
ALTER SCHEMA access OWNER TO lw_access_owner;
ALTER SCHEMA environment OWNER TO lw_environment_owner;
ALTER SCHEMA agent OWNER TO lw_agent_owner;
ALTER SCHEMA evaluation OWNER TO lw_evaluation_owner;
ALTER SCHEMA resource OWNER TO lw_resource_owner;

REVOKE ALL ON SCHEMA platform_meta, shared_audit, control, access, environment, agent, evaluation, resource FROM PUBLIC;
GRANT USAGE ON SCHEMA platform_meta TO lw_release_coordinator;
GRANT USAGE ON SCHEMA shared_audit TO lw_audit_projection;

DO $migration_memberships$
DECLARE
    membership_pair text[];
BEGIN
    FOREACH membership_pair SLICE 1 IN ARRAY ARRAY[
        ARRAY['lw_control_owner', 'lw_control_migration'],
        ARRAY['lw_access_owner', 'lw_access_migration'],
        ARRAY['lw_environment_owner', 'lw_environment_migration'],
        ARRAY['lw_agent_owner', 'lw_agent_migration'],
        ARRAY['lw_evaluation_owner', 'lw_evaluation_migration'],
        ARRAY['lw_resource_owner', 'lw_resource_migration']
    ] LOOP
        IF NOT EXISTS (
            SELECT 1
              FROM pg_auth_members membership
              JOIN pg_roles parent ON parent.oid = membership.roleid
              JOIN pg_roles member ON member.oid = membership.member
             WHERE parent.rolname = membership_pair[1]
               AND member.rolname = membership_pair[2]
        ) THEN
            EXECUTE format('GRANT %I TO %I', membership_pair[1], membership_pair[2]);
        END IF;
    END LOOP;
END
$migration_memberships$;

DO $membership_contract$
DECLARE
    unexpected_membership text;
BEGIN
    SELECT parent.rolname || '->' || member.rolname
      INTO unexpected_membership
      FROM pg_auth_members membership
      JOIN pg_roles parent ON parent.oid = membership.roleid
      JOIN pg_roles member ON member.oid = membership.member
     WHERE parent.rolname LIKE 'lw_%'
       AND (
           (parent.rolname = 'lw_control_owner'
            AND member.rolname NOT IN ('lw_control_migration', 'postgres-admin'))
        OR (parent.rolname = 'lw_access_owner'
            AND member.rolname NOT IN ('lw_access_migration', 'postgres-admin'))
        OR (parent.rolname = 'lw_environment_owner'
            AND member.rolname NOT IN ('lw_environment_migration', 'postgres-admin'))
        OR (parent.rolname = 'lw_agent_owner'
            AND member.rolname NOT IN ('lw_agent_migration', 'postgres-admin'))
        OR (parent.rolname = 'lw_evaluation_owner'
            AND member.rolname NOT IN ('lw_evaluation_migration', 'postgres-admin'))
        OR (parent.rolname = 'lw_resource_owner'
            AND member.rolname NOT IN ('lw_resource_migration', 'postgres-admin'))
        OR (parent.rolname IN ('lw_release_coordinator', 'lw_audit_projection')
            AND member.rolname <> 'postgres-admin')
        OR parent.rolname NOT IN (
            'lw_control_owner', 'lw_access_owner', 'lw_environment_owner',
            'lw_agent_owner', 'lw_evaluation_owner', 'lw_resource_owner',
            'lw_release_coordinator', 'lw_audit_projection'
        )
       )
     LIMIT 1;
    IF FOUND THEN
        RAISE EXCEPTION 'PLATFORM_BOOTSTRAP_MEMBERSHIP_CONTRACT_INVALID: %', unexpected_membership;
    END IF;
END
$membership_contract$;

GRANT USAGE ON SCHEMA control TO lw_control_migration, lw_control_runtime;
GRANT USAGE ON SCHEMA access TO lw_access_migration, lw_access_runtime;
GRANT USAGE ON SCHEMA environment TO lw_environment_migration, lw_environment_runtime;
GRANT USAGE ON SCHEMA agent TO lw_agent_migration, lw_agent_runtime;
GRANT USAGE ON SCHEMA evaluation TO lw_evaluation_migration, lw_evaluation_runtime;
GRANT USAGE ON SCHEMA resource TO lw_resource_migration, lw_resource_runtime;

DO $role_configuration$
DECLARE
    role_configuration text[];
    role_name text;
    expected_configuration text[];
BEGIN
    FOREACH role_name IN ARRAY ARRAY[
        'lw_release_coordinator', 'lw_audit_projection',
        'lw_control_owner', 'lw_access_owner', 'lw_environment_owner',
        'lw_agent_owner', 'lw_evaluation_owner', 'lw_resource_owner',
        'lw_control_migration', 'lw_control_runtime',
        'lw_access_migration', 'lw_access_runtime',
        'lw_environment_migration', 'lw_environment_runtime',
        'lw_agent_migration', 'lw_agent_runtime',
        'lw_evaluation_migration', 'lw_evaluation_runtime',
        'lw_resource_migration', 'lw_resource_runtime'
    ] LOOP
        IF role_name LIKE '%_owner' THEN
            expected_configuration := ARRAY[]::text[];
        ELSE
            expected_configuration := ARRAY[
                CASE role_name
                    WHEN 'lw_release_coordinator' THEN 'search_path=platform_meta, pg_catalog'
                    WHEN 'lw_audit_projection' THEN 'search_path=shared_audit, pg_catalog'
                    WHEN 'lw_control_migration' THEN 'search_path=control, pg_catalog'
                    WHEN 'lw_control_runtime' THEN 'search_path=control, pg_catalog'
                    WHEN 'lw_access_migration' THEN 'search_path=access, pg_catalog'
                    WHEN 'lw_access_runtime' THEN 'search_path=access, pg_catalog'
                    WHEN 'lw_environment_migration' THEN 'search_path=environment, pg_catalog'
                    WHEN 'lw_environment_runtime' THEN 'search_path=environment, pg_catalog'
                    WHEN 'lw_agent_migration' THEN 'search_path=agent, pg_catalog'
                    WHEN 'lw_agent_runtime' THEN 'search_path=agent, pg_catalog'
                    WHEN 'lw_evaluation_migration' THEN 'search_path=evaluation, pg_catalog'
                    WHEN 'lw_evaluation_runtime' THEN 'search_path=evaluation, pg_catalog'
                    WHEN 'lw_resource_migration' THEN 'search_path=resource, pg_catalog'
                    WHEN 'lw_resource_runtime' THEN 'search_path=resource, pg_catalog'
                END
            ];
        END IF;
        SELECT COALESCE(rolconfig, ARRAY[]::text[])
          INTO role_configuration
          FROM pg_roles
         WHERE rolname = role_name;
        IF role_configuration IS DISTINCT FROM expected_configuration THEN
            RAISE EXCEPTION 'PLATFORM_BOOTSTRAP_ROLE_CONFIGURATION_MISMATCH: %', role_name;
        END IF;
    END LOOP;
END
$role_configuration$;

ALTER DEFAULT PRIVILEGES FOR ROLE lw_control_owner IN SCHEMA control REVOKE ALL ON TABLES FROM PUBLIC;
ALTER DEFAULT PRIVILEGES FOR ROLE lw_control_owner IN SCHEMA control GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO lw_control_runtime;
ALTER DEFAULT PRIVILEGES FOR ROLE lw_control_owner IN SCHEMA control REVOKE ALL ON SEQUENCES FROM PUBLIC;
ALTER DEFAULT PRIVILEGES FOR ROLE lw_control_owner IN SCHEMA control GRANT USAGE, SELECT ON SEQUENCES TO lw_control_runtime;
ALTER DEFAULT PRIVILEGES FOR ROLE lw_access_owner IN SCHEMA access REVOKE ALL ON TABLES FROM PUBLIC;
ALTER DEFAULT PRIVILEGES FOR ROLE lw_access_owner IN SCHEMA access GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO lw_access_runtime;
ALTER DEFAULT PRIVILEGES FOR ROLE lw_access_owner IN SCHEMA access REVOKE ALL ON SEQUENCES FROM PUBLIC;
ALTER DEFAULT PRIVILEGES FOR ROLE lw_access_owner IN SCHEMA access GRANT USAGE, SELECT ON SEQUENCES TO lw_access_runtime;
ALTER DEFAULT PRIVILEGES FOR ROLE lw_environment_owner IN SCHEMA environment REVOKE ALL ON TABLES FROM PUBLIC;
ALTER DEFAULT PRIVILEGES FOR ROLE lw_environment_owner IN SCHEMA environment GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO lw_environment_runtime;
ALTER DEFAULT PRIVILEGES FOR ROLE lw_environment_owner IN SCHEMA environment REVOKE ALL ON SEQUENCES FROM PUBLIC;
ALTER DEFAULT PRIVILEGES FOR ROLE lw_environment_owner IN SCHEMA environment GRANT USAGE, SELECT ON SEQUENCES TO lw_environment_runtime;
ALTER DEFAULT PRIVILEGES FOR ROLE lw_agent_owner IN SCHEMA agent REVOKE ALL ON TABLES FROM PUBLIC;
ALTER DEFAULT PRIVILEGES FOR ROLE lw_agent_owner IN SCHEMA agent GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO lw_agent_runtime;
ALTER DEFAULT PRIVILEGES FOR ROLE lw_agent_owner IN SCHEMA agent REVOKE ALL ON SEQUENCES FROM PUBLIC;
ALTER DEFAULT PRIVILEGES FOR ROLE lw_agent_owner IN SCHEMA agent GRANT USAGE, SELECT ON SEQUENCES TO lw_agent_runtime;
ALTER DEFAULT PRIVILEGES FOR ROLE lw_evaluation_owner IN SCHEMA evaluation REVOKE ALL ON TABLES FROM PUBLIC;
ALTER DEFAULT PRIVILEGES FOR ROLE lw_evaluation_owner IN SCHEMA evaluation GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO lw_evaluation_runtime;
ALTER DEFAULT PRIVILEGES FOR ROLE lw_evaluation_owner IN SCHEMA evaluation REVOKE ALL ON SEQUENCES FROM PUBLIC;
ALTER DEFAULT PRIVILEGES FOR ROLE lw_evaluation_owner IN SCHEMA evaluation GRANT USAGE, SELECT ON SEQUENCES TO lw_evaluation_runtime;
ALTER DEFAULT PRIVILEGES FOR ROLE lw_resource_owner IN SCHEMA resource REVOKE ALL ON TABLES FROM PUBLIC;
ALTER DEFAULT PRIVILEGES FOR ROLE lw_resource_owner IN SCHEMA resource GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO lw_resource_runtime;
ALTER DEFAULT PRIVILEGES FOR ROLE lw_resource_owner IN SCHEMA resource REVOKE ALL ON SEQUENCES FROM PUBLIC;
ALTER DEFAULT PRIVILEGES FOR ROLE lw_resource_owner IN SCHEMA resource GRANT USAGE, SELECT ON SEQUENCES TO lw_resource_runtime;

REVOKE ALL ON ALL SEQUENCES IN SCHEMA control FROM PUBLIC;
GRANT USAGE, SELECT ON ALL SEQUENCES IN SCHEMA control TO lw_control_runtime;
REVOKE ALL ON ALL SEQUENCES IN SCHEMA access FROM PUBLIC;
GRANT USAGE, SELECT ON ALL SEQUENCES IN SCHEMA access TO lw_access_runtime;
REVOKE ALL ON ALL SEQUENCES IN SCHEMA environment FROM PUBLIC;
GRANT USAGE, SELECT ON ALL SEQUENCES IN SCHEMA environment TO lw_environment_runtime;
REVOKE ALL ON ALL SEQUENCES IN SCHEMA agent FROM PUBLIC;
GRANT USAGE, SELECT ON ALL SEQUENCES IN SCHEMA agent TO lw_agent_runtime;
REVOKE ALL ON ALL SEQUENCES IN SCHEMA evaluation FROM PUBLIC;
GRANT USAGE, SELECT ON ALL SEQUENCES IN SCHEMA evaluation TO lw_evaluation_runtime;
REVOKE ALL ON ALL SEQUENCES IN SCHEMA resource FROM PUBLIC;
GRANT USAGE, SELECT ON ALL SEQUENCES IN SCHEMA resource TO lw_resource_runtime;

CREATE TABLE IF NOT EXISTS platform_meta.release_attempts (
    attempt_id text PRIMARY KEY,
    release_id text NOT NULL,
    catalog_sha256 text NOT NULL CHECK (catalog_sha256 ~ '^[0-9a-f]{64}$'),
    git_commit text NOT NULL,
    build_digest text NOT NULL,
    job_id text NOT NULL,
    state text NOT NULL CHECK (state IN ('running', 'succeeded', 'failed')),
    current_domain text,
    diagnostic text,
    report_sha256 text CHECK (report_sha256 IS NULL OR report_sha256 ~ '^[0-9a-f]{64}$'),
    started_at timestamptz NOT NULL DEFAULT now(),
    finished_at timestamptz,
    resolved_at timestamptz,
    resolution_id text,
    CHECK ((state = 'running' AND finished_at IS NULL) OR state <> 'running')
);
ALTER TABLE platform_meta.release_attempts OWNER TO lw_release_coordinator;
REVOKE ALL ON platform_meta.release_attempts FROM PUBLIC;

CREATE TABLE IF NOT EXISTS shared_audit.audit_records (
    event_id uuid PRIMARY KEY,
    source_domain text NOT NULL,
    event_type text NOT NULL,
    aggregate_id uuid NOT NULL,
    aggregate_sequence bigint NOT NULL CHECK (aggregate_sequence > 0),
    sanitized_record jsonb NOT NULL CHECK (jsonb_typeof(sanitized_record) = 'object'),
    projected_at timestamptz NOT NULL DEFAULT now()
);
CREATE TABLE IF NOT EXISTS shared_audit.projection_offsets (
    consumer text PRIMARY KEY,
    last_event_id uuid NOT NULL,
    watermark bigint NOT NULL CHECK (watermark >= 0),
    healthy boolean NOT NULL,
    diagnostic text,
    updated_at timestamptz NOT NULL DEFAULT now()
);
ALTER TABLE shared_audit.audit_records OWNER TO lw_audit_projection;
ALTER TABLE shared_audit.projection_offsets OWNER TO lw_audit_projection;
REVOKE ALL ON ALL TABLES IN SCHEMA shared_audit FROM PUBLIC;

DO $history$
DECLARE
    domain_name text;
    migration_role text;
BEGIN
    FOREACH domain_name IN ARRAY ARRAY['control', 'access', 'environment', 'agent', 'evaluation', 'resource'] LOOP
        migration_role := format('lw_%s_migration', domain_name);
        EXECUTE format(
            'CREATE TABLE IF NOT EXISTS %I.schema_migrations (
                migration_id bigint PRIMARY KEY CHECK (migration_id > 0),
                filename text NOT NULL UNIQUE,
                sha256 text NOT NULL CHECK (sha256 ~ ''^[0-9a-f]{64}$''),
                applied_at timestamptz NOT NULL DEFAULT now(),
                outcome text NOT NULL CHECK (outcome = ''applied''),
                executor_identity text NOT NULL,
                release_id text NOT NULL,
                catalog_sha256 text NOT NULL CHECK (catalog_sha256 ~ ''^[0-9a-f]{64}$'')
            )', domain_name
        );
        EXECUTE format('ALTER TABLE %I.schema_migrations OWNER TO %I', domain_name, migration_role);
        EXECUTE format('REVOKE ALL ON %I.schema_migrations FROM PUBLIC', domain_name);
        EXECUTE format('GRANT SELECT ON %I.schema_migrations TO lw_%s_runtime', domain_name, domain_name);
    END LOOP;
END
$history$;
