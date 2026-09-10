use std::error::Error;
use std::path::Path;

use persistence_sqlx::{Domain, MigrationCatalog};
use sqlx::PgPool;

/// Creates one service schema and applies every migration listed for that domain in the checked-in
/// catalog. `PostgreSQL` integration tests must track the live schema contract automatically.
pub async fn apply_domain_migrations(pool: &PgPool, domain: Domain) -> Result<(), Box<dyn Error>> {
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
    let domain_migrations = catalog
        .domains
        .iter()
        .find(|entry| entry.name == domain)
        .ok_or_else(|| std::io::Error::other("migration catalog has no requested domain"))?;
    for migration in &domain_migrations.migrations {
        let sql = MigrationCatalog::read_verified_sql(&migration_root, migration)?;
        sqlx::raw_sql(&sql).execute(&mut *connection).await?;
    }
    Ok(())
}
