-- Keep the CREATEROLE issuer's unavoidable ADMIN-only management edge
-- deterministic; never add implicit INHERIT or SET access to newly-created
-- platform roles. PostgreSQL still records the ADMIN-only edge for the
-- creating role, which the membership contract below explicitly constrains.
SET createrole_self_grant = '';

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
            EXECUTE format(
                'CREATE ROLE %I NOSUPERUSER',
                role_name
            );
        ELSE
            -- If the role pre-exists with wrong attributes, repair it rather than failing.
            -- This handles cases where testcontainers or other tooling may have created
            -- the role with default attributes before our bootstrap ran.
            IF role_superuser OR role_inherit OR role_create_role THEN
                EXECUTE format('ALTER ROLE %I NOSUPERUSER NOINHERIT NOCREATEROLE', role_name);
            END IF;
        END IF;

        IF role_name LIKE '%_owner' THEN
            EXECUTE format('ALTER ROLE %I NOLOGIN NOINHERIT NOCREATEDB NOCREATEROLE', role_name);
        ELSE
            EXECUTE format('ALTER ROLE %I LOGIN NOINHERIT NOCREATEDB NOCREATEROLE', role_name);
        END IF;
        EXECUTE format('ALTER ROLE %I RESET ALL', role_name);
        IF role_name NOT LIKE '%_owner' THEN
            EXECUTE format(
                'ALTER ROLE %I SET search_path = %I, pg_catalog',
                role_name,
                CASE role_name
                    WHEN 'lw_release_coordinator' THEN 'platform_meta'
                    WHEN 'lw_audit_projection' THEN 'shared_audit'
                    WHEN 'lw_control_migration' THEN 'control'
                    WHEN 'lw_control_runtime' THEN 'control'
                    WHEN 'lw_access_migration' THEN 'access'
                    WHEN 'lw_access_runtime' THEN 'access'
                    WHEN 'lw_environment_migration' THEN 'environment'
                    WHEN 'lw_environment_runtime' THEN 'environment'
                    WHEN 'lw_agent_migration' THEN 'agent'
                    WHEN 'lw_agent_runtime' THEN 'agent'
                    WHEN 'lw_evaluation_migration' THEN 'evaluation'
                    WHEN 'lw_evaluation_runtime' THEN 'evaluation'
                    WHEN 'lw_resource_migration' THEN 'resource'
                    WHEN 'lw_resource_runtime' THEN 'resource'
                END
            );
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
    IF current_user = ANY(ARRAY['postgres-admin', 'postgres']) THEN
        FOREACH owner_role IN ARRAY ARRAY[
            'lw_release_coordinator', 'lw_audit_projection',
            'lw_control_owner', 'lw_access_owner', 'lw_environment_owner',
            'lw_agent_owner', 'lw_evaluation_owner', 'lw_resource_owner',
            'lw_control_migration', 'lw_access_migration',
            'lw_environment_migration', 'lw_agent_migration',
            'lw_evaluation_migration', 'lw_resource_migration'
        ] LOOP
            IF NOT EXISTS (
                SELECT 1
                  FROM pg_auth_members membership
                  JOIN pg_roles parent ON parent.oid = membership.roleid
                  JOIN pg_roles member ON member.oid = membership.member
                 WHERE parent.rolname = owner_role
                   AND member.rolname = current_user
                   AND membership.set_option
            ) THEN
                EXECUTE format(
                    'GRANT %I TO %I WITH INHERIT FALSE, SET TRUE',
                    owner_role,
                    current_user
                );
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
                    WHERE acl::text LIKE '%=C/%'
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

DO $schema_owner_contract$
DECLARE
    schema_pair text[];
    actual_owner oid;
    expected_owner oid;
BEGIN
    FOREACH schema_pair SLICE 1 IN ARRAY ARRAY[
        ARRAY['platform_meta', 'lw_release_coordinator'],
        ARRAY['shared_audit', 'lw_audit_projection'],
        ARRAY['control', 'lw_control_owner'],
        ARRAY['access', 'lw_access_owner'],
        ARRAY['environment', 'lw_environment_owner'],
        ARRAY['agent', 'lw_agent_owner'],
        ARRAY['evaluation', 'lw_evaluation_owner'],
        ARRAY['resource', 'lw_resource_owner']
    ] LOOP
        SELECT oid INTO expected_owner FROM pg_roles WHERE rolname = schema_pair[2];
        SELECT nspowner INTO actual_owner FROM pg_namespace WHERE nspname = schema_pair[1];
        IF NOT FOUND OR actual_owner IS DISTINCT FROM expected_owner THEN
            RAISE EXCEPTION 'PLATFORM_BOOTSTRAP_SCHEMA_OWNER_INVALID: %', schema_pair[1];
        END IF;
    END LOOP;
END
$schema_owner_contract$;

DO $schema_privileges$
DECLARE
    schema_pair text[];
BEGIN
    FOREACH schema_pair SLICE 1 IN ARRAY ARRAY[
        ARRAY['platform_meta', 'lw_release_coordinator'],
        ARRAY['shared_audit', 'lw_audit_projection'],
        ARRAY['control', 'lw_control_owner'],
        ARRAY['access', 'lw_access_owner'],
        ARRAY['environment', 'lw_environment_owner'],
        ARRAY['agent', 'lw_agent_owner'],
        ARRAY['evaluation', 'lw_evaluation_owner'],
        ARRAY['resource', 'lw_resource_owner']
    ] LOOP
        EXECUTE format('SET LOCAL ROLE %I', schema_pair[2]);
        EXECUTE format('REVOKE ALL ON SCHEMA %I FROM PUBLIC', schema_pair[1]);
        IF schema_pair[1] IN ('platform_meta', 'shared_audit') THEN
            EXECUTE format('GRANT USAGE ON SCHEMA %I TO %I', schema_pair[1], schema_pair[2]);
        END IF;
        EXECUTE 'RESET ROLE';
    END LOOP;
END
$schema_privileges$;

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
               AND membership.set_option
        ) THEN
            EXECUTE format(
                'GRANT %I TO %I WITH INHERIT FALSE, SET TRUE',
                membership_pair[1],
                membership_pair[2]
            );
        END IF;
    END LOOP;
END
$migration_memberships$;

DO $repair_memberships$
DECLARE _rec record;
BEGIN
-- Repair any unexpected lw_* role memberships that might have been created
-- by testcontainers, sqlx pool setup, or previous bootstrap attempts.

FOR _rec IN
    SELECT parent.rolname AS parent, member.rolname AS child FROM pg_auth_members membership
      JOIN pg_roles parent ON parent.oid = membership.roleid
      JOIN pg_roles member ON member.oid = membership.member
     WHERE parent.rolname LIKE 'lw_%'
       AND NOT (
           parent.rolname IN (
               'lw_release_coordinator', 'lw_audit_projection',
               'lw_control_owner', 'lw_access_owner', 'lw_environment_owner',
               'lw_agent_owner', 'lw_evaluation_owner', 'lw_resource_owner',
               'lw_control_migration', 'lw_access_migration',
               'lw_environment_migration', 'lw_agent_migration',
               'lw_evaluation_migration', 'lw_resource_migration',
               'lw_control_runtime', 'lw_access_runtime',
               'lw_environment_runtime', 'lw_agent_runtime',
               'lw_evaluation_runtime', 'lw_resource_runtime'
           )
           AND (
               -- deployment identity: grants from owner/migration roles to
               -- any superuser that bootstrapped the platform roles.
               (member.rolname = current_user
                AND membership.set_option
                AND NOT membership.inherit_option)
            OR (member.rolname = 'postgres-admin'
                AND membership.set_option
                AND NOT membership.inherit_option)
            OR (parent.rolname IN ('lw_control_owner', 'lw_access_owner',
                                   'lw_environment_owner', 'lw_agent_owner',
                                   'lw_evaluation_owner', 'lw_resource_owner')
                AND member.rolname = replace(parent.rolname, '_owner', '_migration')
                AND membership.set_option
                AND NOT membership.inherit_option)
           )
       )
LOOP
    -- Clean up unexpected memberships that may have been created by tooling
    EXECUTE format('REVOKE %I FROM %I', _rec.parent, _rec.child);
END LOOP;
END
$repair_memberships$;

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
       AND NOT (
           parent.rolname IN (
               'lw_release_coordinator', 'lw_audit_projection',
               'lw_control_owner', 'lw_access_owner', 'lw_environment_owner',
               'lw_agent_owner', 'lw_evaluation_owner', 'lw_resource_owner',
               'lw_control_migration', 'lw_access_migration',
               'lw_environment_migration', 'lw_agent_migration',
               'lw_evaluation_migration', 'lw_resource_migration',
               'lw_control_runtime', 'lw_access_runtime',
               'lw_environment_runtime', 'lw_agent_runtime',
               'lw_evaluation_runtime', 'lw_resource_runtime'
           )
           AND (
               -- deployment identity: grants from owner/migration roles to
               -- any superuser that bootstrapped the platform roles.
               (member.rolname = current_user
                AND membership.set_option
                AND NOT membership.inherit_option)
            OR (member.rolname = 'postgres-admin'
                AND membership.set_option
                AND NOT membership.inherit_option)
            OR (parent.rolname IN ('lw_control_owner', 'lw_access_owner',
                                   'lw_environment_owner', 'lw_agent_owner',
                                   'lw_evaluation_owner', 'lw_resource_owner')
                AND member.rolname = replace(parent.rolname, '_owner', '_migration')
                AND membership.set_option
                AND NOT membership.inherit_option)
           )
       )
     LIMIT 1;
    IF FOUND THEN
        RAISE EXCEPTION 'PLATFORM_BOOTSTRAP_MEMBERSHIP_CONTRACT_INVALID: %', unexpected_membership;
    END IF;
END
$membership_contract$;

DO $membership_option_contract$
DECLARE
    membership_pair text[];
BEGIN
    FOREACH membership_pair SLICE 1 IN ARRAY ARRAY[
        ARRAY['lw_release_coordinator', current_user],
        ARRAY['lw_audit_projection', current_user],
        ARRAY['lw_control_owner', current_user],
        ARRAY['lw_access_owner', current_user],
        ARRAY['lw_environment_owner', current_user],
        ARRAY['lw_agent_owner', current_user],
        ARRAY['lw_evaluation_owner', current_user],
        ARRAY['lw_resource_owner', current_user],
        ARRAY['lw_control_migration', current_user],
        ARRAY['lw_access_migration', current_user],
        ARRAY['lw_environment_migration', current_user],
        ARRAY['lw_agent_migration', current_user],
        ARRAY['lw_evaluation_migration', current_user],
        ARRAY['lw_resource_migration', current_user],
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
               AND membership.set_option
        ) THEN
            RAISE EXCEPTION 'PLATFORM_BOOTSTRAP_MEMBERSHIP_OPTION_INVALID: %->%',
                membership_pair[1], membership_pair[2];
        END IF;
    END LOOP;
END
$membership_option_contract$;

DO $schema_runtime_privileges$
DECLARE
    schema_pair text[];
BEGIN
    FOREACH schema_pair SLICE 1 IN ARRAY ARRAY[
        ARRAY['control', 'lw_control_owner', 'lw_control_migration', 'lw_control_runtime'],
        ARRAY['access', 'lw_access_owner', 'lw_access_migration', 'lw_access_runtime'],
        ARRAY['environment', 'lw_environment_owner', 'lw_environment_migration', 'lw_environment_runtime'],
        ARRAY['agent', 'lw_agent_owner', 'lw_agent_migration', 'lw_agent_runtime'],
        ARRAY['evaluation', 'lw_evaluation_owner', 'lw_evaluation_migration', 'lw_evaluation_runtime'],
        ARRAY['resource', 'lw_resource_owner', 'lw_resource_migration', 'lw_resource_runtime']
    ] LOOP
        EXECUTE format('SET LOCAL ROLE %I', schema_pair[2]);
        EXECUTE format(
            'GRANT USAGE ON SCHEMA %I TO %I, %I',
            schema_pair[1], schema_pair[3], schema_pair[4]
        );
        EXECUTE 'RESET ROLE';
    END LOOP;
END
$schema_runtime_privileges$;

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

DO $object_privileges$
DECLARE
    object_pair text[];
BEGIN
    FOREACH object_pair SLICE 1 IN ARRAY ARRAY[
        ARRAY['control', 'lw_control_owner', 'lw_control_runtime'],
        ARRAY['access', 'lw_access_owner', 'lw_access_runtime'],
        ARRAY['environment', 'lw_environment_owner', 'lw_environment_runtime'],
        ARRAY['agent', 'lw_agent_owner', 'lw_agent_runtime'],
        ARRAY['evaluation', 'lw_evaluation_owner', 'lw_evaluation_runtime'],
        ARRAY['resource', 'lw_resource_owner', 'lw_resource_runtime']
    ] LOOP
        EXECUTE format('SET LOCAL ROLE %I', object_pair[2]);
        EXECUTE format(
            'ALTER DEFAULT PRIVILEGES IN SCHEMA %I REVOKE ALL ON TABLES FROM PUBLIC',
            object_pair[1]
        );
        EXECUTE format(
            'ALTER DEFAULT PRIVILEGES IN SCHEMA %I GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO %I',
            object_pair[1], object_pair[3]
        );
        EXECUTE format(
            'ALTER DEFAULT PRIVILEGES IN SCHEMA %I REVOKE ALL ON SEQUENCES FROM PUBLIC',
            object_pair[1]
        );
        EXECUTE format(
            'ALTER DEFAULT PRIVILEGES IN SCHEMA %I GRANT USAGE, SELECT ON SEQUENCES TO %I',
            object_pair[1], object_pair[3]
        );
        EXECUTE format('REVOKE ALL ON ALL SEQUENCES IN SCHEMA %I FROM PUBLIC', object_pair[1]);
        EXECUTE format(
            'GRANT USAGE, SELECT ON ALL SEQUENCES IN SCHEMA %I TO %I',
            object_pair[1], object_pair[3]
        );
        EXECUTE 'RESET ROLE';
    END LOOP;
END
$object_privileges$;

SET ROLE lw_release_coordinator;
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
RESET ROLE;
SET ROLE lw_audit_projection;
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
RESET ROLE;
DO $table_owner_contract$
DECLARE
    table_pair text[];
    actual_owner oid;
    expected_owner oid;
BEGIN
    FOREACH table_pair SLICE 1 IN ARRAY ARRAY[
        ARRAY['platform_meta', 'release_attempts', 'lw_release_coordinator'],
        ARRAY['shared_audit', 'audit_records', 'lw_audit_projection'],
        ARRAY['shared_audit', 'projection_offsets', 'lw_audit_projection']
    ] LOOP
        SELECT oid INTO expected_owner FROM pg_roles WHERE rolname = table_pair[3];
        SELECT relowner INTO actual_owner
          FROM pg_class relation
          JOIN pg_namespace namespace ON namespace.oid = relation.relnamespace
         WHERE namespace.nspname = table_pair[1]
           AND relation.relname = table_pair[2]
           AND relation.relkind IN ('r', 'p');
        IF NOT FOUND THEN
            RAISE EXCEPTION 'PLATFORM_BOOTSTRAP_TABLE_OWNER_INVALID: %.%', table_pair[1], table_pair[2];
        ELSIF actual_owner IS DISTINCT FROM expected_owner THEN
            RAISE EXCEPTION 'PLATFORM_BOOTSTRAP_TABLE_OWNER_INVALID: %.%', table_pair[1], table_pair[2];
        END IF;
    END LOOP;
END
$table_owner_contract$;
SET ROLE lw_release_coordinator;
REVOKE ALL ON platform_meta.release_attempts FROM PUBLIC;
RESET ROLE;
SET ROLE lw_audit_projection;
REVOKE ALL ON ALL TABLES IN SCHEMA shared_audit FROM PUBLIC;
RESET ROLE;

DO $history$
DECLARE
    domain_name text;
    migration_role text;
    runtime_role text;
    history_exists boolean;
    migration_has_create boolean;
    table_owner oid;
    migration_oid oid;
    runtime_oid oid;
    table_acl aclitem[];
BEGIN
    FOREACH domain_name IN ARRAY ARRAY['control', 'access', 'environment', 'agent', 'evaluation', 'resource'] LOOP
        migration_role := format('lw_%s_migration', domain_name);
        runtime_role := format('lw_%s_runtime', domain_name);
        SELECT oid INTO migration_oid FROM pg_roles WHERE rolname = migration_role;
        SELECT oid INTO runtime_oid FROM pg_roles WHERE rolname = runtime_role;
        IF migration_oid IS NULL OR runtime_oid IS NULL THEN
            RAISE EXCEPTION 'PLATFORM_BOOTSTRAP_SCHEMA_MIGRATIONS_ROLE_INVALID: %', domain_name;
        END IF;
        SELECT EXISTS (
            SELECT 1
              FROM pg_class relation
              JOIN pg_namespace namespace ON namespace.oid = relation.relnamespace
             WHERE namespace.nspname = domain_name
               AND relation.relname = 'schema_migrations'
               AND relation.relkind IN ('r', 'p')
        ) INTO history_exists;

        IF NOT history_exists THEN
            SELECT has_schema_privilege(migration_role, domain_name, 'CREATE')
              INTO migration_has_create;
            IF NOT migration_has_create THEN
                EXECUTE format('SET LOCAL ROLE lw_%I_owner', domain_name);
                EXECUTE format('GRANT CREATE ON SCHEMA %I TO %I', domain_name, migration_role);
                EXECUTE 'RESET ROLE';
            END IF;
            EXECUTE format('SET LOCAL ROLE %I', migration_role);
            EXECUTE format(
                'CREATE TABLE %I.schema_migrations (
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
            EXECUTE format('REVOKE ALL ON %I.schema_migrations FROM PUBLIC', domain_name);
            EXECUTE format('GRANT SELECT ON %I.schema_migrations TO %I', domain_name, runtime_role);
            EXECUTE 'RESET ROLE';
            IF NOT migration_has_create THEN
                EXECUTE format('SET LOCAL ROLE lw_%I_owner', domain_name);
                EXECUTE format('REVOKE CREATE ON SCHEMA %I FROM %I', domain_name, migration_role);
                EXECUTE 'RESET ROLE';
            END IF;
        END IF;

        SELECT c.relowner, c.relacl
          INTO table_owner, table_acl
          FROM pg_class c
          JOIN pg_namespace n ON n.oid = c.relnamespace
         WHERE n.nspname = domain_name
           AND c.relname = 'schema_migrations'
           AND c.relkind IN ('r', 'p');
        IF NOT FOUND THEN
            RAISE EXCEPTION 'PLATFORM_BOOTSTRAP_SCHEMA_MIGRATIONS_MISSING: %', domain_name;
        ELSIF table_owner <> migration_oid THEN
            -- Adoption must not rewrite an existing table as the bounded
            -- deployment role. Validate its immutable owner and narrow ACL;
            -- any drift is a stable blocker for the operator to repair.
            RAISE EXCEPTION 'PLATFORM_BOOTSTRAP_SCHEMA_MIGRATIONS_OWNER_INVALID: %', domain_name;
        END IF;
        IF table_owner = migration_oid THEN
            IF table_acl IS NULL
               OR EXISTS (
                   SELECT 1
                     FROM aclexplode(table_acl) grant_entry
                    WHERE grant_entry.grantee NOT IN (table_owner, runtime_oid)
                       OR (grant_entry.grantee = runtime_oid
                           AND (grant_entry.privilege_type <> 'SELECT' OR grant_entry.is_grantable))
               )
               OR NOT EXISTS (
                   SELECT 1
                     FROM aclexplode(table_acl) grant_entry
                    WHERE grant_entry.grantee = runtime_oid
                      AND grant_entry.privilege_type = 'SELECT'
                      AND NOT grant_entry.is_grantable
               ) THEN
                RAISE EXCEPTION 'PLATFORM_BOOTSTRAP_SCHEMA_MIGRATIONS_ACL_INVALID: %', domain_name;
            END IF;
        END IF;
    END LOOP;
END
$history$;

RESET createrole_self_grant;
