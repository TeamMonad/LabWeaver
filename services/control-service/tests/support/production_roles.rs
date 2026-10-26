//! Shared real-PostgreSQL runtime roles for Control and Access membership regressions.
//! Grants come from the checked-in migration catalog, not a test-only GRANT approximation.

use persistence_sqlx::{Domain, MigrationCatalog, MigrationCoordinator, MigrationIdentity};
use sqlx::{
    PgPool,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use std::{collections::BTreeMap, path::Path, str::FromStr};
use testcontainers::{ContainerAsync, ImageExt, runners::AsyncRunner};
use testcontainers_modules::postgres::Postgres;
use uuid::Uuid;

type TestResult<T> = Result<T, Box<dyn std::error::Error>>;
const TEST_PASSWORD: &str = "labweaver-test-only-password";

pub struct ProductionRoles {
    pub provisioner: PgPool,
    pub control: PgPool,
    pub access: PgPool,
    _container: ContainerAsync<Postgres>,
}

impl ProductionRoles {
    pub async fn database_errors(&self) -> TestResult<String> {
        Ok(String::from_utf8(self._container.stderr_to_vec().await?)?)
    }
    /// Applies the production bootstrap and every catalog migration, then logs in as runtime roles.
    pub async fn start() -> TestResult<Self> {
        let container = Postgres::default().with_tag("17.6-alpine").start().await?;
        let url = format!(
            "postgres://postgres:postgres@127.0.0.1:{}/postgres",
            container.get_host_port_ipv4(5432).await?
        );
        let provisioner = PgPoolOptions::new()
            .max_connections(4)
            .connect(&url)
            .await?;
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let migrations = root.join("migrations");
        let catalog = MigrationCatalog::load(&migrations.join("catalog.yaml"))?;
        let deployment: serde_yaml::Value = serde_yaml::from_str(&std::fs::read_to_string(
            root.join("deploy/ansible/roles/platform_foundation/defaults/main.yml"),
        )?)?;
        let roles = deployment
            .get("platform_foundation_postgres_admin_runtime_memberships")
            .and_then(serde_yaml::Value::as_sequence)
            .ok_or_else(|| std::io::Error::other("deployment runtime role list is missing"))?;
        for domain in Domain::ALL {
            assert!(
                roles
                    .iter()
                    .any(|role| role.as_str() == Some(domain.runtime_role()))
            );
        }
        MigrationCoordinator::bootstrap(&provisioner, &catalog, &migrations).await?;
        for role in std::iter::once("lw_release_coordinator").chain(
            Domain::ALL
                .into_iter()
                .flat_map(|domain| [domain.migration_role(), domain.runtime_role()]),
        ) {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "ALTER ROLE {role} PASSWORD '{TEST_PASSWORD}'"
            )))
            .execute(&provisioner)
            .await?;
        }
        let mut pools = BTreeMap::new();
        for domain in Domain::ALL {
            pools.insert(domain, role_pool(&url, domain.migration_role()).await?);
        }
        MigrationCoordinator::new(
            role_pool(&url, "lw_release_coordinator").await?,
            pools,
            MigrationIdentity {
                cluster_uuid: "membership-runtime-regression".to_owned(),
                release_id: "membership-runtime-regression".to_owned(),
                git_commit: "membership-runtime-regression".to_owned(),
                build_digest: "sha256:membership-runtime-regression".to_owned(),
                job_id: "membership-runtime-regression".to_owned(),
                attempt_id: Uuid::now_v7().to_string(),
            },
        )?
        .apply(&catalog, &migrations)
        .await?;
        let control = role_pool(&url, Domain::Control.runtime_role()).await?;
        let access = role_pool(&url, Domain::Access.runtime_role()).await?;
        assert_read_only_cross_schema(&control, Domain::Control, Domain::Access).await?;
        assert_read_only_cross_schema(&access, Domain::Access, Domain::Control).await?;
        Ok(Self {
            provisioner,
            control,
            access,
            _container: container,
        })
    }
}

async fn role_pool(url: &str, role: &str) -> Result<PgPool, sqlx::Error> {
    PgPoolOptions::new()
        .max_connections(4)
        .connect_with(
            PgConnectOptions::from_str(url)?
                .username(role)
                .password(TEST_PASSWORD),
        )
        .await
}

async fn assert_read_only_cross_schema(
    pool: &PgPool,
    own: Domain,
    foreign: Domain,
) -> TestResult<()> {
    let (role, superuser): (String, bool) =
        sqlx::query_as("SELECT current_user,rolsuper FROM pg_roles WHERE rolname=current_user")
            .fetch_one(pool)
            .await?;
    assert_eq!(role, own.runtime_role());
    assert!(!superuser, "runtime role must not be a superuser");
    let writes: i64 = sqlx::query_scalar(
        "SELECT count(*)::bigint FROM pg_class relation \
         JOIN pg_namespace namespace ON namespace.oid=relation.relnamespace \
         WHERE namespace.nspname=$1 AND relation.relkind IN ('r','p','v','m','f') \
           AND (has_table_privilege(current_user,relation.oid,'INSERT') \
             OR has_table_privilege(current_user,relation.oid,'UPDATE') \
             OR has_table_privilege(current_user,relation.oid,'DELETE') \
             OR has_table_privilege(current_user,relation.oid,'TRUNCATE'))",
    )
    .bind(foreign.schema())
    .fetch_one(pool)
    .await?;
    assert_eq!(writes, 0, "{role} must remain read-only in {foreign:?}");
    Ok(())
}
