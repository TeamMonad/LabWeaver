use std::{error::Error, path::Path};

use persistence_sqlx::{Domain, MigrationCatalog};
use sqlx::PgPool;

/// Creates the Evaluation schema from every migration declared in the repository catalog.
///
/// Integration tests must use the same ordered and hash-checked SQL as the service migration
/// coordinator so recovery and control-plane tests cannot silently run against an older schema.
pub async fn apply_evaluation_migrations(pool: &PgPool) -> Result<(), Box<dyn Error>> {
    sqlx::query("CREATE SCHEMA evaluation")
        .execute(pool)
        .await?;
    let mut connection = pool.acquire().await?;
    sqlx::query("SET search_path = evaluation, pg_catalog")
        .execute(&mut *connection)
        .await?;
    let migration_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../migrations");
    let catalog = MigrationCatalog::load(&migration_root.join("catalog.yaml"))?;
    let evaluation_migrations = catalog
        .domains
        .iter()
        .find(|domain| domain.name == Domain::Evaluation)
        .ok_or_else(|| std::io::Error::other("migration catalog has no evaluation domain"))?;
    for migration in &evaluation_migrations.migrations {
        let sql = MigrationCatalog::read_verified_sql(&migration_root, migration)?;
        sqlx::raw_sql(&sql).execute(&mut *connection).await?;
    }
    Ok(())
}
