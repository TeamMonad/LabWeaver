//! Shared lifecycle journey; only the Kubernetes execution observation is supplied by a fixture.
use super::*;
use contracts::environment::{EnvironmentCleanupTarget, EnvironmentDiskState};
use environment_service::{
    LifecycleCommand, ProviderObservation, apply_provider_observation, plan_command,
    plan_command_authorized, record_disk_observation,
};

pub fn delete_restore_and_reauthorize(
    mut instance: EnvironmentInstance,
    projection: &contracts::events::ReleasePublished,
    disk_uid: uuid::Uuid,
    now: UtcTimestamp,
) -> Result<EnvironmentInstance, Box<dyn Error>> {
    instance.approved_resources.cpu_millicores =
        projection.environment_spec.resources.cpu_millicores;
    instance.approved_resources.memory_bytes = projection.environment_spec.resources.memory_bytes;
    instance.approved_resources.storage_bytes = projection.environment_spec.resources.storage_bytes;
    let receipt = runtime_receipt(&instance, projection);
    instance.lease_id = Some(receipt.lease_authorization.lease_id);
    instance.capacity_binding = Some(receipt.lease_authorization.capacity_binding.clone());
    instance.operation.lease_authorization = Some(receipt.lease_authorization.clone());
    instance.runtime_authorization = Some(receipt);
    instance.observed_state = ObservedEnvironmentState::Ready;
    instance.observed_generation = instance.generation;
    instance.operation.state = OperationState::Succeeded;
    instance.endpoints = vec![EnvironmentEndpoint {
        id: EndpointId::new(),
        protocol: EndpointProtocol::Http,
        revision: instance.revision,
        health: EndpointHealth::Healthy,
        observed_at: now,
    }];
    instance.validate()?;
    let command =
        |current: &EnvironmentInstance, kind| -> Result<LifecycleCommand, Box<dyn Error>> {
            Ok(LifecycleCommand {
                environment_id: current.id,
                kind,
                expected_revision: current.revision,
                actor_id: current.owner_id,
                trace_id: "restore-start-journey".to_owned(),
                accepted_at: now,
                deadline_at: UtcTimestamp::from_utc(now.get() + time::Duration::minutes(10))?,
                access_revocation_revision: Some(current.revision),
                preserve_mutable_disk: true,
                max_attempts: 3,
                reset_target: None,
            })
        };
    let mut deleting = plan_command(
        &instance,
        &command(&instance, EnvironmentOperationKind::Delete)?,
        OperationId::new(),
    )?;
    deleting.operation.cleanup_target = Some(EnvironmentCleanupTarget {
        namespace: format!("lw-env-{}", instance.id),
        uid: uuid::Uuid::from_u128(100),
        retain_disk: true,
        purge_disk: false,
        objects: vec![contracts::execution::ExecutionObjectRef {
            api_version: "v1".to_owned(),
            resource: "persistentvolumeclaims".to_owned(),
            name: if instance.runtime_kind == RuntimeKind::Container {
                "workspace"
            } else {
                "rootdisk"
            }
            .to_owned(),
            uid: disk_uid.to_string(),
        }],
    });
    let mut retired = apply_provider_observation(
        &deleting,
        deleting.operation.id,
        ProviderObservation {
            next_state: ObservedEnvironmentState::Deleted,
            endpoints: Vec::new(),
            operation_complete: true,
            cleanup_evidence: Some(contracts::ArtifactRef {
                artifact_id: contracts::ArtifactId::new(),
                store_binding: "verified-retirement".to_owned(),
                object_version: "1".to_owned(),
                size_bytes: 1,
                media_type: "application/json".to_owned(),
            }),
        },
    )?;
    record_disk_observation(&mut retired, now)?;
    assert_eq!(
        retired.disk_retention.as_ref().ok_or("retention")?.state,
        EnvironmentDiskState::Retained
    );
    let restored = plan_command(
        &retired,
        &command(&retired, EnvironmentOperationKind::Restore)?,
        OperationId::new(),
    )?;
    assert_eq!(restored.observed_state, ObservedEnvironmentState::Stopped);
    let start = command(&restored, EnvironmentOperationKind::Start)?;
    assert!(
        plan_command_authorized(&restored, &start, OperationId::new(), None, now).is_err(),
        "restore does not authorize new compute"
    );
    let mut authorization = restored
        .operation
        .lease_authorization
        .clone()
        .ok_or("lease")?;
    authorization.lease_revision = revision(authorization.lease_revision.get() + 1);
    let mut started = plan_command_authorized(
        &restored,
        &start,
        OperationId::new(),
        Some(authorization.clone()),
        now,
    )?;
    assert_eq!(
        started.observed_state,
        ObservedEnvironmentState::Provisioning,
        "restored Start must enter the state accepted by both providers"
    );
    started
        .runtime_authorization
        .as_mut()
        .ok_or("receipt")?
        .lease_authorization = authorization;
    assert_eq!(
        started.operation.cleanup_target,
        retired.operation.cleanup_target
    );
    Ok(started)
}
