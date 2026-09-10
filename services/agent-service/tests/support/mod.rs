use std::error::Error;
use std::path::Path;

use persistence_sqlx::{Domain, MigrationCatalog};
use sqlx::PgPool;

/// Creates the Agent schema and applies every migration currently listed in the checked-in
/// catalog. Integration tests must exercise the same schema as the service instead of selecting
/// a historical subset of migrations by hand.
pub async fn apply_agent_migrations(pool: &PgPool) -> Result<(), Box<dyn Error>> {
    sqlx::query("CREATE SCHEMA agent").execute(pool).await?;
    let mut connection = pool.acquire().await?;
    sqlx::query("SET search_path = agent, pg_catalog")
        .execute(&mut *connection)
        .await?;
    let migration_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../migrations");
    let catalog = MigrationCatalog::load(&migration_root.join("catalog.yaml"))?;
    let agent_migrations = catalog
        .domains
        .iter()
        .find(|domain| domain.name == Domain::Agent)
        .ok_or_else(|| std::io::Error::other("migration catalog has no agent domain"))?;
    for migration in &agent_migrations.migrations {
        let sql = MigrationCatalog::read_verified_sql(&migration_root, migration)?;
        sqlx::raw_sql(&sql).execute(&mut *connection).await?;
    }
    Ok(())
}
