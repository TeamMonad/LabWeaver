use std::error::Error;
use std::path::Path;

use persistence_sqlx::{Domain, MigrationCatalog};
use sqlx::PgPool;

/// Creates the Access schema and applies every migration currently listed in the checked-in
/// catalog so integration tests cannot silently exercise a historical schema subset.
pub async fn apply_access_migrations(pool: &PgPool) -> Result<(), Box<dyn Error>> {
    apply_access_migrations_through(pool, u64::MAX).await
}

pub async fn apply_access_migrations_through(
    pool: &PgPool,
    last_access_migration: u64,
) -> Result<(), Box<dyn Error>> {
    sqlx::query(
        "DO $$ BEGIN
             IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'lw_control_runtime') THEN
                 CREATE ROLE lw_control_runtime NOLOGIN;
             END IF;
         END $$",
    )
    .execute(pool)
    .await?;
    sqlx::raw_sql("CREATE SCHEMA IF NOT EXISTS control")
        .execute(pool)
        .await?;
    let migration_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../migrations");
    let catalog = MigrationCatalog::load(&migration_root.join("catalog.yaml"))?;
    let control = catalog
        .domains
        .iter()
        .find(|entry| entry.name == Domain::Control)
        .ok_or_else(|| std::io::Error::other("no Control migrations"))?;
    let mut control_connection = pool.acquire().await?;
    sqlx::query("SET search_path = control, pg_catalog")
        .execute(&mut *control_connection)
        .await?;
    for migration in &control.migrations {
        sqlx::raw_sql(&MigrationCatalog::read_verified_sql(
            &migration_root,
            migration,
        )?)
        .execute(&mut *control_connection)
        .await?;
    }
    drop(control_connection);
    let domain = Domain::Access;
    sqlx::query(&format!("CREATE SCHEMA {}", domain.schema()))
        .execute(pool)
        .await?;
    let mut connection = pool.acquire().await?;
    sqlx::query(&format!(
        "SET search_path = {}, pg_catalog",
        domain.schema()
    ))
    .execute(&mut *connection)
    .await?;
    let migration_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../migrations");
    let catalog = MigrationCatalog::load(&migration_root.join("catalog.yaml"))?;
    let migrations = catalog
        .domains
        .iter()
        .find(|entry| entry.name == domain)
        .ok_or_else(|| std::io::Error::other("migration catalog has no access domain"))?;
    for migration in migrations
        .migrations
        .iter()
        .filter(|migration| migration.id <= last_access_migration)
    {
        let sql = MigrationCatalog::read_verified_sql(&migration_root, migration)?;
        sqlx::raw_sql(&sql).execute(&mut *connection).await?;
    }
    Ok(())
}

#[allow(
    dead_code,
    reason = "only the schema regression needs a single-step catalog upgrade"
)]
pub async fn apply_access_migration(
    pool: &PgPool,
    migration_id: u64,
) -> Result<(), Box<dyn Error>> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../migrations");
    let catalog = MigrationCatalog::load(&root.join("catalog.yaml"))?;
    let migration = catalog
        .domains
        .iter()
        .find(|domain| domain.name == Domain::Access)
        .and_then(|domain| {
            domain
                .migrations
                .iter()
                .find(|migration| migration.id == migration_id)
        })
        .ok_or_else(|| std::io::Error::other("Access migration not found"))?;
    let mut connection = pool.acquire().await?;
    sqlx::query("SET search_path = access, pg_catalog")
        .execute(&mut *connection)
        .await?;
    sqlx::raw_sql(&MigrationCatalog::read_verified_sql(&root, migration)?)
        .execute(&mut *connection)
        .await?;
    Ok(())
}
