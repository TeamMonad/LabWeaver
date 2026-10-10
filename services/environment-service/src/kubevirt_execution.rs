//! Concrete executor incarnation and per-write authorization for `KubeVirt` operations.

use contracts::EnvironmentId;
use persistence_sqlx::Sha256Digest;
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::{KubeVirtBackendFence, KubeVirtExecutorFenceError};

/// Kubernetes-observed identity of the process that actually admitted an operation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KubeVirtExecutionInstance {
    pub namespace: String,
    pub pod_name: String,
    pub pod_uid: Uuid,
    pub container_name: String,
    pub boot_token: Uuid,
}

/// This permit is server-created after durable admission, never supplied over NATS.
#[derive(Clone)]
pub struct KubeVirtExecutionPermit {
    pub(crate) pool: PgPool,
    pub(crate) fence: KubeVirtBackendFence,
    pub(crate) instance: KubeVirtExecutionInstance,
}

impl KubeVirtExecutionPermit {
    /// Rechecks exact admission and the current authoritative operation before a tenant write.
    pub async fn check(&self) -> Result<std::time::Duration, KubeVirtExecutorFenceError> {
        let row = sqlx::query(
            "SELECT f.highest_generation,f.operation_id,f.provider_step,f.attempt, \
             f.last_request_id,f.last_response,f.execution_owner,i.generation, \
             i.contract->'operation'->>'id' AS current_operation, \
             i.contract->'operation'->>'deadlineAt' AS current_deadline, \
             i.contract->'operation'->>'state' AS current_state, \
             date_trunc('milliseconds',clock_timestamp()) AS authority_now \
             FROM environment.kubevirt_executor_fences f \
             JOIN environment.environment_instances i USING(environment_id) \
             WHERE f.environment_id=$1",
        )
        .bind(self.fence.environment_id.as_uuid())
        .fetch_optional(&self.pool)
        .await?
        .ok_or(KubeVirtExecutorFenceError::Cancelled)?;
        let owner: KubeVirtExecutionInstance =
            serde_json::from_value(row.try_get("execution_owner")?)
                .map_err(|_| KubeVirtExecutorFenceError::IdentityMismatch)?;
        let request_id: String = row.try_get("last_request_id")?;
        let operation_id: Uuid = row.try_get("operation_id")?;
        let current_operation: Option<String> = row.try_get("current_operation")?;
        let current_deadline = row
            .try_get::<Option<String>, _>("current_deadline")?
            .and_then(|value| value.parse::<contracts::UtcTimestamp>().ok())
            .ok_or(KubeVirtExecutorFenceError::Cancelled)?;
        let current_state: Option<String> = row.try_get("current_state")?;
        if !matches!(
            current_state.as_deref(),
            Some("accepted" | "running" | "cancelling")
        ) || owner != self.instance
            || request_id != self.fence.request_id.to_string()
            || operation_id != self.fence.operation_id.as_uuid()
            || current_operation.as_deref() != Some(self.fence.operation_id.to_string().as_str())
            || row.try_get::<i64, _>("highest_generation")?
                != i64::try_from(self.fence.environment_generation)
                    .map_err(|_| KubeVirtExecutorFenceError::IdentityMismatch)?
            || row.try_get::<i64, _>("generation")?
                != i64::try_from(self.fence.environment_generation)
                    .map_err(|_| KubeVirtExecutorFenceError::IdentityMismatch)?
            || row.try_get::<i32, _>("provider_step")?
                != i32::try_from(self.fence.provider_step)
                    .map_err(|_| KubeVirtExecutorFenceError::IdentityMismatch)?
            || row.try_get::<i32, _>("attempt")?
                != i32::try_from(self.fence.attempt)
                    .map_err(|_| KubeVirtExecutorFenceError::IdentityMismatch)?
            || row
                .try_get::<Option<serde_json::Value>, _>("last_response")?
                .is_some()
        {
            return Err(KubeVirtExecutorFenceError::Cancelled);
        }
        let now: time::OffsetDateTime = row.try_get("authority_now")?;
        std::time::Duration::try_from(
            self.fence.deadline_at.get().min(current_deadline.get()) - now,
        )
        .ok()
        .filter(|remaining| !remaining.is_zero())
        .ok_or(KubeVirtExecutorFenceError::DeadlineExceeded)
    }

    #[must_use]
    pub fn environment_id(&self) -> EnvironmentId {
        self.fence.environment_id
    }

    #[must_use]
    pub fn request_id(&self) -> Sha256Digest {
        self.fence.request_id
    }
}

#[cfg(test)]
pub(crate) async fn test_permit(
    fence: &mut KubeVirtBackendFence,
) -> Result<
    (
        testcontainers::ContainerAsync<testcontainers_modules::postgres::Postgres>,
        KubeVirtExecutionPermit,
    ),
    Box<dyn std::error::Error>,
> {
    use testcontainers::{ImageExt, runners::AsyncRunner};
    use testcontainers_modules::postgres::Postgres;
    let container = Postgres::default().with_tag("17.5-alpine").start().await?;
    let pool = PgPool::connect(&format!(
        "postgres://postgres:postgres@127.0.0.1:{}/postgres",
        container.get_host_port_ipv4(5432).await?
    ))
    .await?;
    crate::test_support::apply_environment_migrations(&pool).await?;
    let now: time::OffsetDateTime =
        sqlx::query_scalar("SELECT date_trunc('milliseconds',clock_timestamp())")
            .fetch_one(&pool)
            .await?;
    fence.deadline_at = contracts::UtcTimestamp::from_utc(now + time::Duration::seconds(5))?;
    let mut authority = crate::test_support::requested_instance();
    authority.id = fence.environment_id;
    authority.operation.id = fence.operation_id;
    authority.operation.accepted_at = contracts::UtcTimestamp::from_utc(now)?;
    authority.operation.next_attempt_at = authority.operation.accepted_at;
    authority.operation.deadline_at = fence.deadline_at;
    authority.eligibility_expires_at = Some(fence.deadline_at);
    crate::PgEnvironmentStore::new(pool.clone())
        .create("runtime-authority", &authority)
        .await?;
    authority.generation = fence.environment_generation;
    sqlx::query("UPDATE environment.environment_instances SET generation=$2,contract=$3 WHERE environment_id=$1")
        .bind(authority.id.as_uuid()).bind(i64::try_from(authority.generation)?).bind(serde_json::to_value(&authority)?).execute(&pool).await?;
    let instance = KubeVirtExecutionInstance {
        namespace: "labweaver-system".to_owned(),
        pod_name: "executor-fixture".to_owned(),
        pod_uid: Uuid::new_v4(),
        container_name: "kubevirt-executor".to_owned(),
        boot_token: Uuid::new_v4(),
    };
    sqlx::query("INSERT INTO environment.kubevirt_executor_fences (environment_id,highest_generation,operation_id,provider_step,attempt,tombstoned,last_action,last_request_id,deadline_at,execution_owner) VALUES ($1,$2,$3,$4,$5,FALSE,'stop',$6,$7,$8)")
        .bind(fence.environment_id.as_uuid()).bind(i64::try_from(fence.environment_generation)?).bind(fence.operation_id.as_uuid())
        .bind(i32::try_from(fence.provider_step)?).bind(i32::try_from(fence.attempt)?).bind(fence.request_id.to_string())
        .bind(fence.deadline_at.get()).bind(serde_json::to_value(&instance)?).execute(&pool).await?;
    Ok((
        container,
        KubeVirtExecutionPermit {
            pool,
            fence: fence.clone(),
            instance,
        },
    ))
}
