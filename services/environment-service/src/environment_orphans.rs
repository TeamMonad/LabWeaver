//! Reconcile platform-owned Kubernetes inventory with durable Environment deletion intent.
//!
//! The scan never deletes Kubernetes objects. Failed deletions re-enter the existing revisioned
//! Cleanup pipeline; unknown owners, live references and completed cleanup inconsistencies remain
//! diagnostic-only. In particular, labels alone never authorize a destructive provider call.

use contracts::{
    ProjectId,
    authoring::RuntimeKind,
    environment::{
        DesiredEnvironmentState, EnvironmentInstance, EnvironmentOperationKind,
        ObservedEnvironmentState, OperationState,
    },
};
use sqlx::PgPool;
use tokio::sync::watch;

use super::*;

/// Result of one bounded Kubernetes inventory page.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct EnvironmentOrphanReport {
    pub scanned: u32,
    pub referenced: u32,
    pub unverified: u32,
    pub inconsistent: u32,
    pub recovering: u32,
}

impl KubernetesContainerExecutor {
    /// Scans at most 100 objects. The Kubernetes continuation is opaque and never a URL.
    pub async fn reconcile_orphans(
        &self,
        pool: &PgPool,
        kind: RuntimeKind,
        continuation: Option<&str>,
    ) -> Result<(EnvironmentOrphanReport, Option<String>), ProviderFailure> {
        let (path, selector, namespace_object) = match (&self.namespace_profile, kind) {
            (
                crate::ContainerNamespaceProfile::Existing {
                    namespace,
                    namespace_uid,
                    ..
                },
                RuntimeKind::Container,
            ) => {
                let observed = self
                    .get_api_json(&format!("/api/v1/namespaces/{namespace}"))
                    .await?
                    .ok_or_else(rejected)?;
                verify_existing_namespace(&observed, namespace, *namespace_uid)?;
                (
                    format!("/apis/apps/v1/namespaces/{namespace}/deployments"),
                    "app.kubernetes.io/name=labweaver-environment",
                    false,
                )
            }
            _ => (
                "/api/v1/namespaces".to_owned(),
                "labweaver.io/managed=true",
                true,
            ),
        };
        let mut request = self
            .authorized(self.client.get(self.namespaced_url(&path)?))
            .query(&[("limit", "100"), ("labelSelector", selector)]);
        if let Some(continuation) = continuation {
            request = request.query(&[("continue", continuation)]);
        }
        let response = request.send().await.map_err(|_| unavailable())?;
        if !response.status().is_success() {
            return Err(status_failure(response.status()));
        }
        let list: Value = response.json().await.map_err(|_| invalid_observation())?;
        let items = list
            .get("items")
            .and_then(Value::as_array)
            .ok_or_else(invalid_observation)?;
        let continuation = match list.pointer("/metadata/continue") {
            Some(value) => Some(value.as_str().ok_or_else(invalid_observation)?)
                .filter(|value| !value.is_empty())
                .map(str::to_owned),
            None => None,
        };
        let mut report = EnvironmentOrphanReport::default();
        for object in items {
            report.scanned += 1;
            match self
                .recover_orphan_object(pool, kind, object, namespace_object)
                .await?
            {
                OrphanDecision::Referenced => report.referenced += 1,
                OrphanDecision::Unverified => report.unverified += 1,
                OrphanDecision::Inconsistent => report.inconsistent += 1,
                OrphanDecision::Recovering => report.recovering += 1,
            }
        }
        Ok((report, continuation))
    }

    async fn recover_orphan_object(
        &self,
        pool: &PgPool,
        kind: RuntimeKind,
        object: &Value,
        namespace_object: bool,
    ) -> Result<OrphanDecision, ProviderFailure> {
        let Some((environment_id, project_id)) = object_owner(object, namespace_object) else {
            return Ok(OrphanDecision::Unverified);
        };
        let value: Option<Value> = sqlx::query_scalar("SELECT contract FROM environment.environment_instances WHERE environment_id=$1 AND project_id=$2")
            .bind(environment_id.as_uuid()).bind(project_id.as_uuid()).fetch_optional(pool).await
            .map_err(|_| unavailable())?;
        let Some(value) = value else {
            tracing::warn!(event = "environment.orphan.owner_unknown", %environment_id, %project_id);
            return Ok(OrphanDecision::Unverified);
        };
        let instance: EnvironmentInstance =
            serde_json::from_value(value).map_err(|_| invalid_observation())?;
        instance.validate().map_err(|_| invalid_observation())?;
        if instance.id != environment_id
            || instance.project_id != project_id
            || instance.runtime_kind != kind
        {
            return Ok(OrphanDecision::Unverified);
        }
        if !namespace_object {
            let placement = self
                .namespace_profile
                .for_instance(&instance)
                .map_err(|_| rejected())?;
            if object
                .pointer("/metadata/namespace")
                .and_then(Value::as_str)
                != Some(placement.namespace.as_str())
                || object.pointer("/metadata/name").and_then(Value::as_str)
                    != Some(placement.resource_name("runtime").as_str())
            {
                return Ok(OrphanDecision::Unverified);
            }
        }
        let decision = classify_instance(&instance);
        if decision != OrphanDecision::Recovering {
            if decision == OrphanDecision::Inconsistent {
                tracing::warn!(event = "environment.orphan.cleanup_inconsistent", %environment_id,
                    generation = instance.generation);
            }
            return Ok(decision);
        }
        // Re-enter the owner implementation, preserving its revision CAS, Access revocation,
        // lease fencing, task claim and UID-protected provider cleanup. No network call holds a
        // database transaction. A concurrent user/worker mutation makes this admission fail closed.
        let now = timestamp()?;
        let command = crate::LifecycleCommand {
            environment_id,
            kind: EnvironmentOperationKind::Cleanup,
            expected_revision: instance.revision,
            actor_id: instance.owner_id,
            trace_id: format!("orphan-{}", instance.operation.id),
            accepted_at: now,
            deadline_at: UtcTimestamp::from_utc(now.get() + time::Duration::minutes(10))
                .map_err(|_| rejected())?,
            access_revocation_revision: None,
            preserve_mutable_disk: false,
            max_attempts: instance.operation.max_attempts,
            reset_target: None,
        };
        let key = format!(
            "orphan-{}-{}",
            instance.operation.id,
            instance.revision.get()
        );
        match crate::PgEnvironmentStore::new(pool.clone())
            .accept_command(&key, &command)
            .await
        {
            Ok(_) => {
                tracing::info!(event = "environment.orphan.cleanup_resumed", %environment_id,
                    generation = instance.generation);
                Ok(OrphanDecision::Recovering)
            }
            Err(
                crate::EnvironmentStoreError::Lifecycle(crate::LifecycleError::RevisionConflict)
                | crate::EnvironmentStoreError::IdempotencyConflict
                | crate::EnvironmentStoreError::IdempotencyInProgress,
            ) => Ok(OrphanDecision::Referenced),
            Err(_) => Err(unavailable()),
        }
    }

    /// Periodically visits every inventory page without blocking executor request admission.
    pub async fn run_orphan_reconciler(
        self,
        pool: PgPool,
        kind: RuntimeKind,
        mut shutdown: watch::Receiver<bool>,
    ) -> Result<(), ProviderFailure> {
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut continuation = None;
        loop {
            if *shutdown.borrow() {
                return Ok(());
            }
            tokio::select! {
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() { return Ok(()); }
                    continue;
                }
                _ = interval.tick() => {}
            }
            // A failed API page is retried from its same token; restart starts a fresh snapshot.
            match self
                .reconcile_orphans(&pool, kind, continuation.as_deref())
                .await
            {
                Ok((report, next)) => {
                    continuation = next;
                    tracing::info!(
                        event = "environment.orphan.scan",
                        scanned = report.scanned,
                        referenced = report.referenced,
                        unverified = report.unverified,
                        inconsistent = report.inconsistent,
                        recovering = report.recovering
                    );
                }
                Err(failure) => {
                    // Expired Kubernetes list snapshots need a fresh list, not a permanent
                    // stale token loop. Rechecking ownership makes revisiting objects safe.
                    continuation = None;
                    tracing::warn!(
                        event = "environment.orphan.scan_failed",
                        diagnostic_code = failure.diagnostic_code()
                    );
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OrphanDecision {
    Referenced,
    Unverified,
    Inconsistent,
    Recovering,
}

fn object_owner(object: &Value, namespace_object: bool) -> Option<(EnvironmentId, ProjectId)> {
    let label = |key: &str| object.pointer("/metadata/labels")?.get(key)?.as_str();
    if namespace_object && label("labweaver.io/managed") != Some("true") {
        return None;
    }
    if !namespace_object && label("app.kubernetes.io/name") != Some("labweaver-environment") {
        return None;
    }
    let environment_id = label("labweaver.io/environment-id")?
        .parse::<EnvironmentId>()
        .ok()?;
    let project_id = label("labweaver.io/project-id")?
        .parse::<ProjectId>()
        .ok()?;
    pointer_uuid(object, "/metadata/uid").ok()?;
    if namespace_object
        && object.pointer("/metadata/name")?.as_str()? != format!("lw-env-{environment_id}")
    {
        return None;
    }
    Some((environment_id, project_id))
}

fn classify_instance(instance: &EnvironmentInstance) -> OrphanDecision {
    if instance.desired_state != DesiredEnvironmentState::Deleted {
        return OrphanDecision::Referenced;
    }
    if instance.observed_state == ObservedEnvironmentState::Deleted {
        return OrphanDecision::Inconsistent;
    }
    if matches!(
        instance.operation.state,
        OperationState::Accepted | OperationState::Running | OperationState::Cancelling
    ) {
        return OrphanDecision::Referenced;
    }
    // Never invent an Access revocation receipt or destroy retained/current workloads.
    if instance.observed_state == ObservedEnvironmentState::Failed
        && instance.operation.state == OperationState::Failed
        && instance.endpoints.is_empty()
        && !instance.operation.preserve_mutable_disk
    {
        OrphanDecision::Recovering
    } else {
        OrphanDecision::Unverified
    }
}
