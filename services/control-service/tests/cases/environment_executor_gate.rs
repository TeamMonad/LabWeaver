//! Executor effects use the existing real Control/Resource funded journey, not a second authority.
use super::{EnvironmentRuntimeAdmission, PgEnvironmentStore, Result};
use async_trait::async_trait;
use contracts::{UtcTimestamp, authoring::RuntimeKind, environment::EnvironmentInstance};
use environment_service::{
    ContainerApplyObservation, ContainerBackendFence, ContainerExecutorBackend,
    ContainerExecutorRequest, ContainerExecutorResponse, FencedContainerExecutor,
    FencedKubeVirtExecutor, KUBEVIRT_BACKEND_PROTOCOL_VERSION, KubeVirtBackendFence,
    KubeVirtBaseDiskIdentity, KubeVirtExecutionInstance, KubeVirtExecutionPermit,
    KubeVirtExecutorBackend, KubeVirtExecutorRequest, KubeVirtExecutorRequestEnvelope,
    KubeVirtExecutorResponse, KubeVirtResourcePlan, KubeVirtRunningObservation,
    PgKubeVirtExecutorFenceStore, ProviderFailure, ProviderFailureCode, ReconcileAction,
};
use persistence_sqlx::Sha256Digest;
use sqlx::PgPool;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[path = "environment_executor_transport.rs"]
mod transport;
pub(super) use transport::exercise as exercise_transport;

pub(super) async fn exercise(
    pool: &PgPool,
    resource: &resource_service::store::PgResourceStore,
    admission: &EnvironmentRuntimeAdmission,
    instance: &EnvironmentInstance,
) -> Result {
    let lease = resource
        .load_lease(instance.lease_id.ok_or("funded lease")?)
        .await?;
    let store = PgEnvironmentStore::new(pool.clone());
    let deadline =
        UtcTimestamp::from_utc(store.current_time().await?.get() + time::Duration::seconds(30))?;
    let calls = Arc::new(AtomicUsize::new(0));
    let mut unexpected = Vec::new();
    // Corrupt only current lease eligibility, preserving the previously accepted Environment intent.
    // Both columns and contract change, as in the original lease-chain regression fixture.
    for expired in [false, true] {
        let mut denied = lease.clone();
        if expired {
            denied.expires_at = Some(UtcTimestamp::from_utc(
                lease.active_from.ok_or("active lease")?.get() + time::Duration::milliseconds(1),
            )?);
        } else {
            denied.state = contracts::resource::ResourceLeaseState::Revoked;
        }
        sqlx::query("UPDATE resource.resource_leases SET state=$2,expires_at=$3,contract=$4 WHERE lease_id=$1")
            .bind(lease.id.as_uuid())
            .bind(if expired { "active" } else { "revoked" })
            .bind(denied.expires_at.ok_or("lease expiry")?.get())
            .bind(serde_json::to_value(&denied)?)
            .execute(pool).await?;
        for &action in actions(instance.runtime_kind) {
            let result = execute(
                pool,
                admission,
                instance,
                action,
                deadline,
                calls.clone(),
                true,
            )
            .await;
            if !matches!(&result, Err(error) if error.to_string().contains("LW_RUNTIME_LEASE_NOT_ACTIVE"))
            {
                unexpected.push(format!(
                    "{:?}/{action:?}, expired={expired}: {result:?}",
                    instance.runtime_kind
                ));
            }
        }
    }
    sqlx::query("UPDATE resource.resource_leases SET state='active',expires_at=$2,contract=$3 WHERE lease_id=$1")
        .bind(lease.id.as_uuid()).bind(lease.expires_at.ok_or("lease expiry")?.get())
        .bind(serde_json::to_value(&lease)?).execute(pool).await?;
    assert!(
        unexpected.is_empty(),
        "original lease gate bypasses: {unexpected:#?}"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "denied leases must cause zero Kubernetes effects"
    );
    for (index, &action) in actions(instance.runtime_kind).iter().enumerate() {
        execute(
            pool,
            admission,
            instance,
            action,
            deadline,
            calls.clone(),
            false,
        )
        .await?;
        assert_eq!(
            calls.load(Ordering::SeqCst),
            index + 1,
            "valid original lease executes {action:?}"
        );
    }
    Ok(())
}

const fn actions(kind: RuntimeKind) -> &'static [ReconcileAction] {
    match kind {
        RuntimeKind::Container => &[
            ReconcileAction::Provision,
            ReconcileAction::Reset,
            ReconcileAction::Start,
            ReconcileAction::Restart,
        ],
        RuntimeKind::VirtualMachine => &[ReconcileAction::Start, ReconcileAction::Restart],
    }
}

async fn execute(
    pool: &PgPool,
    admission: &EnvironmentRuntimeAdmission,
    instance: &EnvironmentInstance,
    action: ReconcileAction,
    deadline: UtcTimestamp,
    calls: Arc<AtomicUsize>,
    sparse_attack: bool,
) -> Result {
    let backend = Cluster(
        calls,
        PgEnvironmentStore::new(pool.clone()).current_time().await?,
    );
    if instance.runtime_kind == RuntimeKind::Container {
        // These grant probes precede the outer step-1 replay/tombstone probe. Full plans must
        // not rely on different database clock ticks to distinguish the two effect identities.
        let mut envelope = super::physical_envelope(pool, instance, 0, false, deadline).await?;
        let ContainerExecutorRequest::Apply { mut plan } = envelope.request else {
            return Err("apply fixture".into());
        };
        if sparse_attack {
            // Malformed sparse requests must not bypass the fresh grant gate. Legitimate writes
            // below always carry the complete trusted plan; there is no sparse-write exception.
            plan.resources.retain(|resource| {
                matches!(action, ReconcileAction::Provision | ReconcileAction::Reset)
                    && resource.kind == "Service"
            });
        }
        envelope.request = match action {
            ReconcileAction::Provision | ReconcileAction::Reset => {
                ContainerExecutorRequest::Apply { plan }
            }
            ReconcileAction::Start => ContainerExecutorRequest::Scale { plan, replicas: 1 },
            ReconcileAction::Restart => ContainerExecutorRequest::Restart {
                plan,
                operation_revision: instance.revision,
            },
            _ => return Err("compute action required".into()),
        };
        envelope.fence.action = action;
        envelope.fence.request_id = Sha256Digest::of_canonical(&serde_json::json!({
            "protocolVersion": envelope.fence.protocol_version, "environmentId": instance.id,
            "operationId": instance.operation.id, "providerStep": envelope.fence.provider_step, "operationGeneration": instance.generation,
            "attempt": 1, "action": action, "deadlineAt": deadline, "request": envelope.request,
        }))?;
        let response = FencedContainerExecutor::new(
            super::container_fence_store(pool.clone(), instance).await?,
            backend,
        )
        .with_runtime_admission(admission.clone())
        .execute(envelope)
        .await?;
        assert!(matches!(
            response.response,
            ContainerExecutorResponse::Observed { .. }
        ));
    } else {
        let plan = vm_plan(instance);
        let request = match action {
            ReconcileAction::Start => KubeVirtExecutorRequest::Start { plan },
            ReconcileAction::Restart => KubeVirtExecutorRequest::Restart { plan },
            _ => return Err("VM subresource action required".into()),
        };
        let request_id = Sha256Digest::of_canonical(&serde_json::json!({
            "protocolVersion": KUBEVIRT_BACKEND_PROTOCOL_VERSION, "environmentId": instance.id,
            "operationId": instance.operation.id, "providerStep": 1, "environmentGeneration": instance.generation,
            "attempt": 1, "action": action, "deadlineAt": deadline, "request": request,
        }))?;
        let response = FencedKubeVirtExecutor::new(
            PgKubeVirtExecutorFenceStore::new(pool.clone()),
            backend,
            KubeVirtExecutionInstance {
                namespace: "synthetic-executors".into(),
                pod_name: "executor".into(),
                pod_uid: uuid::Uuid::new_v4(),
                container_name: "executor".into(),
                boot_token: uuid::Uuid::new_v4(),
            },
        )
        .with_runtime_admission(admission.clone())
        .execute(KubeVirtExecutorRequestEnvelope {
            fence: KubeVirtBackendFence {
                protocol_version: KUBEVIRT_BACKEND_PROTOCOL_VERSION,
                environment_id: instance.id,
                operation_id: instance.operation.id,
                provider_step: 1,
                environment_generation: instance.generation,
                attempt: 1,
                action,
                request_id,
                trace_id: "synthetic-executor-gate".into(),
                deadline_at: deadline,
            },
            request,
        })
        .await?;
        assert!(matches!(
            response.response,
            KubeVirtExecutorResponse::Running { .. }
        ));
    }
    Ok(())
}

fn vm_plan(instance: &EnvironmentInstance) -> KubeVirtResourcePlan {
    KubeVirtResourcePlan {
        environment_id: instance.id,
        namespace: format!("lw-env-{}", instance.id),
        virtual_machine_name: "runtime".into(),
        data_volume_name: "rootdisk".into(),
        base_disk: contracts::supply_chain::VirtualMachineBaseDisk {
            binding: "synthetic-base".into(),
            source_registry_digest: format!("docker://synthetic/base@sha256:{}", "a".repeat(64)),
            capacity_bytes: 10_737_418_240,
        },
        base_disk_format: contracts::supply_chain::VirtualMachineDiskFormat::Qcow2,
        base_disk_identity: KubeVirtBaseDiskIdentity::ReviewedDiskSha256,
        base_disk_data_source_namespace: "synthetic-bases".into(),
        base_disk_data_source_name: "base".into(),
        base_disk_disk_sha256: "b".repeat(64),
        storage_class_name: "synthetic-storage".into(),
        vm_vgpu_licensing: None,
        resources: vec![],
        plan_sha256: Sha256Digest::of_bytes(b"empty-vm-subresource-plan"),
    }
}

struct Cluster(Arc<AtomicUsize>, UtcTimestamp);
#[async_trait]
impl ContainerExecutorBackend for Cluster {
    async fn execute(
        &self,
        _: &ContainerBackendFence,
        request: &ContainerExecutorRequest,
    ) -> ContainerExecutorResponse {
        self.0.fetch_add(1, Ordering::SeqCst);
        let plan = match request {
            ContainerExecutorRequest::Apply { plan }
            | ContainerExecutorRequest::Observe { plan }
            | ContainerExecutorRequest::Scale { plan, .. }
            | ContainerExecutorRequest::Restart { plan, .. }
            | ContainerExecutorRequest::DeleteNamespace { plan, .. } => plan,
        };
        ContainerExecutorResponse::Observed {
            plan_sha256: plan.plan_sha256,
            observation: ContainerApplyObservation {
                ready: true,
                observed_at: self.1,
            },
        }
    }
}
#[async_trait]
impl KubeVirtExecutorBackend for Cluster {
    async fn execute(
        &self,
        fence: &KubeVirtBackendFence,
        request: &KubeVirtExecutorRequest,
        _: &KubeVirtExecutionPermit,
    ) -> KubeVirtExecutorResponse {
        self.0.fetch_add(1, Ordering::SeqCst);
        let (KubeVirtExecutorRequest::Start { plan } | KubeVirtExecutorRequest::Restart { plan }) =
            request
        else {
            return KubeVirtExecutorResponse::Failed {
                failure: ProviderFailure {
                    code: ProviderFailureCode::Rejected,
                    retryable: false,
                },
            };
        };
        KubeVirtExecutorResponse::Running {
            plan_sha256: plan.plan_sha256,
            observation: KubeVirtRunningObservation {
                observed_environment_generation: fence.environment_generation,
                vm_resource_generation: 1,
                observed_vm_resource_generation: 1,
                vm_uid: uuid::Uuid::new_v4(),
                vmi_uid: uuid::Uuid::new_v4(),
                root_disk_uid: uuid::Uuid::new_v4(),
                guest_ip: std::net::Ipv4Addr::new(10, 0, 0, 2).into(),
                service_cluster_ip: std::net::Ipv4Addr::new(10, 96, 0, 2).into(),
                ssh_host_key_sha256: Some(Sha256Digest::of_bytes(b"synthetic-host-key")),
                guest_agent_connected: true,
                observed_at: self.1,
            },
        }
    }
}

pub(super) async fn assert_security_rejections(
    pool: &PgPool,
    admission: &EnvironmentRuntimeAdmission,
    instance: &EnvironmentInstance,
    original: &environment_service::ContainerResourcePlan,
) -> Result {
    use environment_service::{ContainerExecutorFenceError, ContainerExecutorRequestEnvelope};
    let calls = Arc::new(AtomicUsize::new(0));
    let executor = FencedContainerExecutor::new(
        super::container_fence_store(pool.clone(), instance).await?,
        Cluster(
            calls.clone(),
            PgEnvironmentStore::new(pool.clone()).current_time().await?,
        ),
    )
    .with_runtime_admission(admission.clone());
    admission.for_instance(instance).await?;
    for (field, value) in [
        ("runAsUser", serde_json::json!(12345)),
        (
            "capabilities",
            serde_json::json!({"drop":["ALL"],"add":["SYS_ADMIN"]}),
        ),
        ("readOnlyRootFilesystem", serde_json::json!(true)),
        ("privileged", serde_json::json!(true)),
    ] {
        let mut plan = original.clone();
        let deployment = plan
            .resources
            .iter_mut()
            .find(|r| r.kind == "Deployment")
            .ok_or("deployment")?;
        deployment.document["spec"]["template"]["spec"]["containers"][0]["securityContext"]
            [field] = value;
        let request = ContainerExecutorRequest::Apply { plan };
        let deadline = instance.operation.deadline_at.min(
            instance
                .runtime_authorization
                .as_ref()
                .ok_or("receipt")?
                .valid_until,
        );
        let mut fence = super::physical_envelope(pool, instance, 1, false, deadline)
            .await?
            .fence;
        fence.request_id = Sha256Digest::of_canonical(&serde_json::json!({
            "protocolVersion": fence.protocol_version, "environmentId": fence.environment_id,
            "operationId": fence.operation_id, "providerStep": fence.provider_step,
            "operationGeneration": fence.operation_generation, "attempt": fence.attempt,
            "action": fence.action, "deadlineAt": fence.deadline_at, "request": request,
        }))?;
        assert!(
            matches!(
                executor
                    .execute(ContainerExecutorRequestEnvelope { fence, request })
                    .await,
                Err(ContainerExecutorFenceError::IdentityMismatch)
            ),
            "{field}"
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "inconsistent {field} must never reach Kubernetes"
        );
    }
    Ok(())
}
