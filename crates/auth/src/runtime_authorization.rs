//! Current policy reads from Control's one read-only authority projection.
use contracts::platform_configuration::PlatformConfigurationStatus;
use sqlx::PgPool;

/// Reads current status, including unverified transitions. It never applies configuration.
pub async fn load_runtime_policy(
    pool: &PgPool,
) -> Result<PlatformConfigurationStatus, sqlx::Error> {
    let value: serde_json::Value =
        sqlx::query_scalar("SELECT contract FROM control.runtime_authorization_policy")
            .fetch_one(pool)
            .await?;
    serde_json::from_value(value).map_err(|error| sqlx::Error::Decode(Box::new(error)))
}
