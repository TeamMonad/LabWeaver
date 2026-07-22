//! Real `PostgreSQL` integration coverage using a disposable Docker container.

use std::collections::BTreeMap;
use std::path::Path;
use std::str::FromStr;

use auth::{
    CreateBffSession, KeyRing, cleanup_expired_auth_state, consume_backchannel_logout,
    create_bff_session, load_bff_session, load_logout_hint, load_membership_snapshot,
    require_service_identity, revoke_bff_session, upsert_actor,
};
use contracts::{PlatformRole, Sha256Digest};
use persistence_sqlx::{
    Domain, IdempotencyDecision, IdempotencyStore, InboxDecision, InboxStore, MigrationCatalog,
    MigrationCoordinator, MigrationIdentity, OutboxStore, SchemaStatus, SchemaVerifier,
};
use serde_json::json;
use sqlx::{
    ConnectOptions, PgPool, Row,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use testcontainers::{ImageExt, runners::AsyncRunner};
use testcontainers_modules::postgres::Postgres;
use uuid::Uuid;

const TEST_PASSWORD: &str = "labweaver-test-only-password";

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn bootstrap_migrate_and_enforce_domain_boundaries() -> Result<(), Box<dyn std::error::Error>>
{
    let container = Postgres::default().with_tag("17.5-alpine").start().await?;
    let provisioner_url = format!(
        "postgres://postgres:postgres@127.0.0.1:{}/postgres",
        container.get_host_port_ipv4(5432).await?
    );
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../migrations");
    let catalog = MigrationCatalog::load(&root.join("catalog.yaml"))?;
    let provisioner = PgPoolOptions::new()
        .max_connections(2)
        .connect(&provisioner_url)
        .await?;
    MigrationCoordinator::bootstrap(&provisioner, &catalog, &root).await?;
    sqlx::query("ALTER ROLE lw_control_runtime SUPERUSER CREATEROLE INHERIT")
        .execute(&provisioner)
        .await?;
    sqlx::query("GRANT lw_access_owner TO lw_control_runtime")
        .execute(&provisioner)
        .await?;
    MigrationCoordinator::bootstrap(&provisioner, &catalog, &root).await?;
    let role = sqlx::query(
        "SELECT rolsuper, rolcreaterole, rolinherit FROM pg_roles WHERE rolname = 'lw_control_runtime'",
    )
    .fetch_one(&provisioner)
    .await?;
    assert!(!role.try_get::<bool, _>("rolsuper")?);
    assert!(!role.try_get::<bool, _>("rolcreaterole")?);
    assert!(!role.try_get::<bool, _>("rolinherit")?);
    let memberships: i64 = sqlx::query_scalar(
        "SELECT count(*)::bigint FROM pg_auth_members membership \
         JOIN pg_roles parent ON parent.oid = membership.roleid \
         JOIN pg_roles member ON member.oid = membership.member \
         WHERE parent.rolname = 'lw_access_owner' AND member.rolname = 'lw_control_runtime'",
    )
    .fetch_one(&provisioner)
    .await?;
    assert_eq!(memberships, 0);
    set_role_passwords(&provisioner).await?;

    let coordinator = role_pool(&provisioner_url, "lw_release_coordinator").await?;
    let migration_pools = pools_for(&provisioner_url, "migration").await?;
    let identity = MigrationIdentity {
        cluster_uuid: "integration-cluster".to_owned(),
        release_id: "integration-release".to_owned(),
        git_commit: "integration-commit".to_owned(),
        build_digest: "sha256:integration-build".to_owned(),
        job_id: "integration-job".to_owned(),
        attempt_id: Uuid::now_v7().to_string(),
    };
    sqlx::query(
        "INSERT INTO platform_meta.release_attempts \
         (attempt_id, release_id, catalog_sha256, git_commit, build_digest, job_id, state) \
         VALUES ('abandoned-attempt', 'integration-release', $1, 'integration-commit', \
                 'sha256:integration-build', 'integration-job', 'running')",
    )
    .bind(catalog.sha256()?.to_string())
    .execute(&provisioner)
    .await?;
    let coordinator = MigrationCoordinator::new(coordinator, migration_pools, identity)?;
    let report = coordinator.apply(&catalog, &root).await?;
    assert_eq!(report.report.outcome, "succeeded");
    let abandoned: String = sqlx::query_scalar(
        "SELECT diagnostic FROM platform_meta.release_attempts WHERE attempt_id = 'abandoned-attempt'",
    )
    .fetch_one(&provisioner)
    .await?;
    assert_eq!(abandoned, "DB_MIGRATION_ABANDONED_ATTEMPT");
    let Err(reused) = coordinator.apply(&catalog, &root).await else {
        return Err("reused attempt ID was accepted".into());
    };
    assert_eq!(reused.diagnostic_code(), "DB_MIGRATION_ATTEMPT_REUSED");

    let runtime_pools = pools_for(&provisioner_url, "runtime").await?;
    for catalog_domain in &catalog.domains {
        assert_eq!(
            SchemaVerifier::classify(&runtime_pools[&catalog_domain.name], catalog_domain).await,
            SchemaStatus::Ready
        );
        let inaccessible_sequences: i64 = sqlx::query_scalar(
            "WITH domain_sequences AS MATERIALIZED ( \
                 SELECT sequence.oid FROM pg_class sequence \
                 JOIN pg_namespace namespace ON namespace.oid = sequence.relnamespace \
                 WHERE namespace.nspname = $1 AND sequence.relkind = 'S' \
             ) \
             SELECT count(*)::bigint FROM domain_sequences \
             WHERE NOT has_sequence_privilege(oid, 'USAGE')",
        )
        .bind(catalog_domain.name.schema())
        .fetch_one(&runtime_pools[&catalog_domain.name])
        .await?;
        assert_eq!(
            inaccessible_sequences, 0,
            "runtime role must be able to allocate every sequence in its domain"
        );
    }

    let access = &runtime_pools[&Domain::Access];
    let auth_tables: i64 = sqlx::query_scalar(
        "SELECT count(*)::bigint FROM information_schema.tables \
         WHERE table_schema = 'access' \
         AND table_name IN ('actors', 'course_memberships', 'project_memberships', \
                            'service_identities', 'oidc_transactions', 'bff_sessions')",
    )
    .fetch_one(access)
    .await?;
    assert_eq!(
        auth_tables, 6,
        "auth migration must be applied by the controlled catalog"
    );
    assert!(
        sqlx::query("CREATE TABLE access.unauthorized_auth_ddl (id integer)")
            .execute(access)
            .await
            .is_err(),
        "runtime role must not be able to mutate the auth schema"
    );
    let key_ring = KeyRing::parse(
        "active".to_owned(),
        "active:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
    )?;
    let local_actor = upsert_actor(
        access,
        "https://issuer.example.test/realms/labweaver",
        "opaque-subject-not-persisted",
    )
    .await?;
    let now = time::OffsetDateTime::now_utc();
    let course_id = Uuid::now_v7();
    let project_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO access.course_memberships (course_id, actor_id, role, state, revision) \
         VALUES ($1, $2, 'student', 'active', 4)",
    )
    .bind(course_id)
    .bind(local_actor.actor_id)
    .execute(access)
    .await?;
    sqlx::query(
        "INSERT INTO access.project_memberships (course_id, project_id, actor_id, role, state, revision) \
         VALUES ($1, $2, $3, 'student', 'active', 5)",
    )
    .bind(course_id)
    .bind(project_id)
    .bind(local_actor.actor_id)
    .execute(access)
    .await?;
    let memberships = load_membership_snapshot(access, local_actor.actor_id).await?;
    assert_eq!(memberships.course_memberships.len(), 1);
    assert_eq!(memberships.project_memberships.len(), 1);
    assert_eq!(memberships.course_memberships[0].revision.get(), 4);
    assert_eq!(memberships.project_memberships[0].revision.get(), 5);
    let san_uri = "spiffe://labweaver/gateway";
    sqlx::query(
        "INSERT INTO access.service_identities \
         (service_identity_id, san_uri, service_name, state, revision) \
         VALUES ($1, $2, 'gateway', 'active', 1)",
    )
    .bind(Uuid::now_v7())
    .bind(san_uri)
    .execute(access)
    .await?;
    require_service_identity(access, san_uri, now).await?;
    assert!(
        require_service_identity(access, "spiffe://labweaver/unregistered", now)
            .await
            .is_err()
    );
    let session = create_bff_session(
        access,
        &key_ring,
        CreateBffSession {
            actor_id: local_actor.actor_id,
            roles: vec![PlatformRole::Student],
            authorization_revision: 1,
            expires_at: now + time::Duration::minutes(15),
            idle_ttl: time::Duration::minutes(5),
            oidc_sid: Some("provider-session".to_owned()),
            logout_hint: "signed-id-token-not-logged".to_owned(),
        },
        now,
    )
    .await?;
    let resumed = load_bff_session(
        access,
        &key_ring,
        session.session_id,
        time::Duration::minutes(5),
        now,
    )
    .await?;
    assert_eq!(resumed.actor_id, local_actor.actor_id);
    assert_eq!(resumed.roles, vec![PlatformRole::Student]);
    assert_eq!(
        load_logout_hint(access, &key_ring, session.session_id).await?,
        "signed-id-token-not-logged"
    );
    auth::verify_csrf_token(&resumed.csrf_token, Some(resumed.csrf_token.expose()))?;
    assert_eq!(
        consume_backchannel_logout(
            access,
            "https://keycloak.example.test/realms/labweaver",
            "logout-token-jti-1",
            "provider-session",
            now + time::Duration::minutes(5),
            now
        )
        .await?,
        1
    );
    assert!(matches!(
        consume_backchannel_logout(
            access,
            "https://keycloak.example.test/realms/labweaver",
            "logout-token-jti-1",
            "provider-session",
            now + time::Duration::minutes(5),
            now
        )
        .await,
        Err(auth::RepositoryError::LogoutReplay)
    ));
    assert!(
        load_bff_session(
            access,
            &key_ring,
            session.session_id,
            time::Duration::minutes(5),
            now
        )
        .await
        .is_err()
    );

    let direct_session = create_bff_session(
        access,
        &key_ring,
        CreateBffSession {
            actor_id: local_actor.actor_id,
            roles: vec![PlatformRole::Student],
            authorization_revision: 1,
            expires_at: now + time::Duration::minutes(15),
            idle_ttl: time::Duration::minutes(5),
            oidc_sid: None,
            logout_hint: "signed-id-token-not-logged".to_owned(),
        },
        now,
    )
    .await?;
    revoke_bff_session(
        access,
        direct_session.session_id,
        "LW_AUTH_SESSION_REVOKED",
        now,
    )
    .await?;
    assert!(
        load_bff_session(
            access,
            &key_ring,
            direct_session.session_id,
            time::Duration::minutes(5),
            now
        )
        .await
        .is_err()
    );

    let cleanup = cleanup_expired_auth_state(
        access,
        now + time::Duration::days(2),
        time::Duration::days(1),
    )
    .await?;
    assert!(cleanup.sessions_deleted >= 2);
    assert_eq!(cleanup.logout_events_deleted, 1);

    let control = &runtime_pools[&Domain::Control];
    let request_hash = Sha256Digest::of_bytes(b"request");
    let mut transaction = control.begin().await?;
    assert_eq!(
        IdempotencyStore::reserve(
            &mut transaction,
            Domain::Control,
            "create",
            "key-1",
            request_hash
        )
        .await?,
        IdempotencyDecision::Reserved
    );
    OutboxStore::enqueue(
        &mut transaction,
        Domain::Control,
        Uuid::now_v7(),
        "labweaver.control.environment_template_release.published.v1",
        "labweaver.control.environment_template_release.published.v1",
        Uuid::now_v7(),
        1,
        &json!({"event": "safe"}),
        Sha256Digest::of_bytes(b"payload"),
    )
    .await?;
    IdempotencyStore::complete(
        &mut transaction,
        Domain::Control,
        "create",
        "key-1",
        &json!({"id": "one"}),
    )
    .await?;
    transaction.commit().await?;
    let mut replay = control.begin().await?;
    assert_eq!(
        IdempotencyStore::reserve(
            &mut replay,
            Domain::Control,
            "create",
            "key-1",
            request_hash
        )
        .await?,
        IdempotencyDecision::Replay(json!({"id": "one"}))
    );
    replay.rollback().await?;

    let aggregate_id = Uuid::now_v7();
    let first_event = Uuid::now_v7();
    let mut first = control.begin().await?;
    assert_eq!(
        InboxStore::accept(
            &mut first,
            Domain::Control,
            "concurrent-consumer",
            first_event,
            aggregate_id,
            1,
            Sha256Digest::of_bytes(b"first"),
        )
        .await?,
        InboxDecision::Accepted
    );
    let second_pool = control.clone();
    let second = tokio::spawn(async move {
        let mut transaction = second_pool.begin().await?;
        let decision = InboxStore::accept(
            &mut transaction,
            Domain::Control,
            "concurrent-consumer",
            Uuid::now_v7(),
            aggregate_id,
            1,
            Sha256Digest::of_bytes(b"second"),
        )
        .await?;
        transaction.commit().await?;
        Ok::<_, persistence_sqlx::PersistenceError>(decision)
    });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(
        !second.is_finished(),
        "second first-sequence reservation must wait on the watermark row"
    );
    first.commit().await?;
    assert_eq!(second.await??, InboxDecision::Stale);

    assert!(
        sqlx::query("CREATE TABLE forbidden_runtime_ddl (id integer)")
            .execute(control)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("SELECT * FROM access.access_grants")
            .fetch_all(control)
            .await
            .is_err()
    );
    assert!(sqlx::query("INSERT INTO shared_audit.audit_records (event_id, source_domain, event_type, aggregate_id, aggregate_sequence, sanitized_record) VALUES ($1, 'control', 'test', $2, 1, '{}'::jsonb)").bind(Uuid::now_v7()).bind(Uuid::now_v7()).execute(control).await.is_err());
    Ok(())
}

async fn set_role_passwords(pool: &PgPool) -> Result<(), sqlx::Error> {
    for role in [
        "lw_release_coordinator",
        "lw_control_migration",
        "lw_access_migration",
        "lw_environment_migration",
        "lw_agent_migration",
        "lw_evaluation_migration",
        "lw_resource_migration",
        "lw_control_runtime",
        "lw_access_runtime",
        "lw_environment_runtime",
        "lw_agent_runtime",
        "lw_evaluation_runtime",
        "lw_resource_runtime",
    ] {
        sqlx::query(&format!("ALTER ROLE {role} PASSWORD '{TEST_PASSWORD}'"))
            .execute(pool)
            .await?;
    }
    Ok(())
}

async fn pools_for(url: &str, kind: &str) -> Result<BTreeMap<Domain, PgPool>, sqlx::Error> {
    let mut pools = BTreeMap::new();
    for domain in Domain::ALL {
        let role = if kind == "migration" {
            domain.migration_role()
        } else {
            domain.runtime_role()
        };
        pools.insert(domain, role_pool(url, role).await?);
    }
    Ok(pools)
}

async fn role_pool(url: &str, role: &str) -> Result<PgPool, sqlx::Error> {
    let options = PgConnectOptions::from_str(url)?
        .username(role)
        .password(TEST_PASSWORD)
        .log_statements(log::LevelFilter::Off);
    PgPoolOptions::new()
        .max_connections(2)
        .connect_with(options)
        .await
}
