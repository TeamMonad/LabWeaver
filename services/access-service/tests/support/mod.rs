use std::error::Error;
use std::path::Path;

use persistence_sqlx::{Domain, MigrationCatalog};
use sqlx::PgPool;

/// Creates the Access schema and applies every migration currently listed in the checked-in
/// catalog so integration tests cannot silently exercise a historical schema subset.
pub async fn apply_access_migrations(pool: &PgPool) -> Result<(), Box<dyn Error>> {
    sqlx::query(
        "DO $$ BEGIN
             IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'lw_control_runtime') THEN
                 CREATE ROLE lw_control_runtime NOLOGIN;
             END IF;
         END $$",
    )
    .execute(pool)
    .await?;
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
    for migration in &migrations.migrations {
        let sql = MigrationCatalog::read_verified_sql(&migration_root, migration)?;
        sqlx::raw_sql(&sql).execute(&mut *connection).await?;
    }
    Ok(())
}
