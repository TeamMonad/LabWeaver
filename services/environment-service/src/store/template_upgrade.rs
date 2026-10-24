//! A confirmed template cutover is an ordinary durable Restart, not another lifecycle.
use contracts::authoring::{EnvironmentClass, EnvironmentRuntimeSpec, RuntimeKind};
use contracts::environment::{
    EnvironmentInstance, EnvironmentLeaseAuthorization, EnvironmentOperationKind,
    EnvironmentTemplateUpgrade, WorkTemplateUpgradeDataPolicy,
};
use contracts::events::{ReleasePublished, subjects};
use contracts::http::{
    EnvironmentOperationAccepted, IdempotencyKey, UpgradeEnvironmentTemplateRequest,
};
use contracts::{ActorId, CandidateId, EnvironmentId, OperationId, Revision};
use persistence_sqlx::{Domain, IdempotencyDecision, IdempotencyStore, Sha256Digest};
use sqlx::{Postgres, Row, Transaction};

use super::{
    EnvironmentStoreError, LifecycleCommand, PgEnvironmentStore, accepted_response, as_i64,
    canonical_hash, database_now, enqueue_environment_event, insert_operation, load_locked,
    plan_command_authorized, update_instance,
};

const UPGRADE_OPERATION: &str = "upgrade_template";

#[derive(Debug, thiserror::Error)]
pub enum TemplateUpgradeError {
    #[error("LW_WORK_TEMPLATE_UPGRADE_UNSUPPORTED_FOR_VM")]
    UnsupportedForVm,
    #[error("LW_WORK_TEMPLATE_UPGRADE_CONFIRMATION_REQUIRED")]
    ConfirmationRequired,
    #[error("LW_WORK_TEMPLATE_UPGRADE_WORK_REQUIRED")]
    WorkRequired,
    #[error("LW_WORK_TEMPLATE_UPGRADE_SCOPE_DENIED")]
    ScopeDenied,
    #[error("LW_WORK_TEMPLATE_UPGRADE_FAMILY_MISMATCH")]
    FamilyMismatch,
    #[error("LW_WORK_TEMPLATE_UPGRADE_TARGET_NOT_NEWER")]
    TargetNotNewer,
    #[error("LW_WORK_TEMPLATE_UPGRADE_REQUIRES_RESOURCE_APPROVAL")]
    RequiresResourceApproval,
    #[error("LW_WORK_TEMPLATE_UPGRADE_UNAVAILABLE")]
    UpgradeUnavailable,
    #[error("LW_WORK_TEMPLATE_UPGRADE_RELEASE_UNAVAILABLE")]
    ReleaseUnavailable,
}

impl TemplateUpgradeError {
    #[must_use]
    pub const fn detail(&self) -> &'static str {
        match self {
            Self::UnsupportedForVm => {
                "虚拟机 Work 不能原地升级模板：需要导出/导入流程，当前不可用。原 Work 和磁盘没有更改，也不会隐式重建。"
            }
            Self::ConfirmationRequired => {
                "升级会重启容器并中断连接；仅保留 /workspace，容器临时根文件系统修改不保留。请阅读影响说明后显式确认。"
            }
            Self::WorkRequired => "此操作只适用于个人 Work，课程实验使用自己的版本发布流程。",
            Self::ScopeDenied => "目标模板不属于当前 Work 的项目、课程或执行后端，不能升级。",
            Self::FamilyMismatch => "目标版本不属于当前模板族，请选择同一模板族的新版本。",
            Self::TargetNotNewer => "请选择比当前固定版本更新的已发布模板。",
            Self::RequiresResourceApproval => {
                "新模板改变了资源、网络、安全策略或连接入口；现有资源授权不覆盖这些变化，请重新申请资源。原 Work 与数据未更改。"
            }
            Self::UpgradeUnavailable => {
                "本版本暂不支持在原资金租约上升级 Work 模板。请新建 Work 并自行迁移数据，确认迁移完成后再释放旧 Work。原 Work 的模板、工作目录和运行实例未更改。"
            }
            Self::ReleaseUnavailable => {
                "模板不存在、已撤回或版本身份不匹配，不能升级。原 Work 与数据未更改。"
            }
        }
    }
}

fn upgrade_hash(
    environment_id: EnvironmentId,
    actor: ActorId,
    revision: Revision,
    request: &UpgradeEnvironmentTemplateRequest,
) -> Result<Sha256Digest, EnvironmentStoreError> {
    canonical_hash(
        &serde_json::json!({"environmentId":environment_id,"actorId":actor,"expectedRevision":revision,"request":request}),
    )
}

fn validate_intent(
    instance: &EnvironmentInstance,
    request: &UpgradeEnvironmentTemplateRequest,
) -> Result<(), TemplateUpgradeError> {
    if instance.runtime_kind == RuntimeKind::VirtualMachine {
        return Err(TemplateUpgradeError::UnsupportedForVm);
    }
    if instance.class != EnvironmentClass::Work {
        return Err(TemplateUpgradeError::WorkRequired);
    }
    if !request.confirmed {
        return Err(TemplateUpgradeError::ConfirmationRequired);
    }
    if request.target_release_id == instance.release_id
        || request.target_release_version <= instance.release_version
    {
        return Err(TemplateUpgradeError::TargetNotNewer);
    }
    Ok(())
}

fn same_family(source: &ReleasePublished, target: &ReleasePublished) -> bool {
    match (
        &source.release.manual_approval,
        &target.release.manual_approval,
    ) {
        (Some(source), Some(target)) => source.template_family_id == target.template_family_id,
        (None, None) => {
            source.release.candidate_id.is_some()
                && source.release.candidate_id == target.release.candidate_id
        }
        (Some(manual), None) | (None, Some(manual)) => {
            let candidate = if source.release.manual_approval.is_none() {
                source.release.candidate_id
            } else {
                target.release.candidate_id
            };
            manual
                .template_family_id
                .strip_prefix("candidate-")
                .is_some_and(|id| id.parse::<CandidateId>().ok() == candidate)
        }
    }
}

fn validate_compatibility(
    instance: &EnvironmentInstance,
    source: &ReleasePublished,
    target: &ReleasePublished,
) -> Result<(), TemplateUpgradeError> {
    for projection in [source, target] {
        projection
            .validate()
            .map_err(|_| TemplateUpgradeError::ReleaseUnavailable)?;
        if projection.release.project_id != instance.project_id
            || projection.release.course_id != instance.course_id
            || projection.release.runtime_kind != RuntimeKind::Container
            || projection.environment_spec.class != EnvironmentClass::Work
        {
            return Err(TemplateUpgradeError::ScopeDenied);
        }
    }
    if !same_family(source, target) {
        return Err(TemplateUpgradeError::FamilyMismatch);
    }
    let old = &source.environment_spec;
    let new = &target.environment_spec;
    let (
        EnvironmentRuntimeSpec::Container {
            provider_binding: old_provider,
            service_port: old_port,
            ..
        },
        EnvironmentRuntimeSpec::Container {
            provider_binding: new_provider,
            service_port: new_port,
            ..
        },
    ) = (&old.runtime, &new.runtime)
    else {
        return Err(TemplateUpgradeError::ScopeDenied);
    };
    // Resource's original approval remains immutable. An upgrade cannot mint extra resource or
    // network/security rights, change PVC bindings, or leave stale non-Deployment objects behind.
    if old.resources != new.resources
        || old.network != new.network
        || old.security != new.security
        || old.entries != new.entries
        || old_provider != new_provider
        || old_port != new_port
        || new_provider != &instance.provider_binding
    {
        return Err(TemplateUpgradeError::RequiresResourceApproval);
    }
    Ok(())
}

async fn validate_projections(
    tx: &mut Transaction<'_, Postgres>,
    instance: &EnvironmentInstance,
    request: &UpgradeEnvironmentTemplateRequest,
) -> Result<(), EnvironmentStoreError> {
    validate_intent(instance, request)?;
    let rows = sqlx::query("SELECT release_id,release_version,contract,withdrawn_at FROM environment.release_projections WHERE release_id IN ($1,$2) ORDER BY release_id FOR SHARE")
        .bind(instance.release_id.as_uuid()).bind(request.target_release_id.as_uuid()).fetch_all(&mut **tx).await?;
    let mut source = None;
    let mut target = None;
    for row in rows {
        let projection: ReleasePublished = serde_json::from_value(row.try_get("contract")?)?;
        let row_id: uuid::Uuid = row.try_get("release_id")?;
        let row_version: i64 = row.try_get("release_version")?;
        if projection.release.id.as_uuid() != row_id
            || as_i64(projection.release.version, "release version")? != row_version
        {
            return Err(TemplateUpgradeError::ReleaseUnavailable.into());
        }
        if projection.release.id == instance.release_id
            && projection.release.version == instance.release_version
        {
            source = Some(projection);
        } else if projection.release.id == request.target_release_id
            && projection.release.version == request.target_release_version
            && row
                .try_get::<Option<time::OffsetDateTime>, _>("withdrawn_at")?
                .is_none()
        {
            target = Some(projection);
        }
    }
    let source = source.ok_or(TemplateUpgradeError::ReleaseUnavailable)?;
    let target = target.ok_or(TemplateUpgradeError::ReleaseUnavailable)?;
    validate_compatibility(instance, &source, &target)?;
    Ok(())
}

impl PgEnvironmentStore {
    /// Checks compatibility before Access revocation. Acceptance rechecks it under row locks.
    pub async fn prepare_work_template_upgrade(
        &self,
        instance: &EnvironmentInstance,
        request: &UpgradeEnvironmentTemplateRequest,
    ) -> Result<(), EnvironmentStoreError> {
        let mut tx = self.pool.begin().await?;
        validate_projections(&mut tx, instance, request).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn lookup_work_template_upgrade_replay(
        &self,
        key: &str,
        environment_id: EnvironmentId,
        actor: ActorId,
        revision: Revision,
        request: &UpgradeEnvironmentTemplateRequest,
    ) -> Result<Option<EnvironmentOperationAccepted>, EnvironmentStoreError> {
        self.lookup_completed_api_command(
            key,
            UPGRADE_OPERATION,
            upgrade_hash(environment_id, actor, revision, request)?,
        )
        .await
    }

    /// Changes only the desired release and writes the normal Restart operation atomically.
    /// The existing workspace PVC, Resource request, lease, resources and retention are untouched.
    pub async fn accept_work_template_upgrade(
        &self,
        key: &str,
        command: &LifecycleCommand,
        request: &UpgradeEnvironmentTemplateRequest,
        authorization: EnvironmentLeaseAuthorization,
    ) -> Result<EnvironmentOperationAccepted, EnvironmentStoreError> {
        IdempotencyKey::parse(key).map_err(|_| EnvironmentStoreError::InvalidIdempotencyKey)?;
        let mut tx = self.pool.begin().await?;
        match IdempotencyStore::reserve(
            &mut tx,
            Domain::Environment,
            UPGRADE_OPERATION,
            key,
            upgrade_hash(
                command.environment_id,
                command.actor_id,
                command.expected_revision,
                request,
            )?,
        )
        .await?
        {
            IdempotencyDecision::Replay(value) => return Ok(serde_json::from_value(value)?),
            IdempotencyDecision::Conflict => {
                return Err(EnvironmentStoreError::IdempotencyConflict);
            }
            IdempotencyDecision::InProgress => {
                return Err(EnvironmentStoreError::IdempotencyInProgress);
            }
            IdempotencyDecision::Reserved => {}
        }
        let current = load_locked(&mut tx, command.environment_id).await?;
        if current.owner_id != command.actor_id {
            return Err(TemplateUpgradeError::ScopeDenied.into());
        }
        if command.kind != EnvironmentOperationKind::Restart
            || !command.preserve_mutable_disk
            || command.reset_target.is_some()
        {
            return Err(EnvironmentStoreError::InboundMetadataInvalid);
        }
        if current.revision != command.expected_revision {
            return Err(EnvironmentStoreError::RevisionConflict);
        }
        validate_projections(&mut tx, &current, request).await?;
        let now = database_now(&mut tx).await?;
        let mut planned = plan_command_authorized(
            &current,
            command,
            OperationId::new(),
            Some(authorization),
            now,
        )?;
        planned.release_id = request.target_release_id;
        planned.release_version = request.target_release_version;
        planned.operation.template_upgrade = Some(EnvironmentTemplateUpgrade {
            source_release_id: current.release_id,
            source_release_version: current.release_version,
            target_release_id: request.target_release_id,
            target_release_version: request.target_release_version,
            data_policy: WorkTemplateUpgradeDataPolicy::PreserveWorkspace,
        });
        update_instance(&mut tx, &current, &planned).await?;
        insert_operation(&mut tx, &planned).await?;
        enqueue_environment_event(&mut tx, &planned, subjects::ENVIRONMENT_OPERATION_ACCEPTED)
            .await?;
        let accepted = accepted_response(&planned);
        IdempotencyStore::complete(
            &mut tx,
            Domain::Environment,
            UPGRADE_OPERATION,
            key,
            &serde_json::to_value(&accepted)?,
        )
        .await?;
        tx.commit().await?;
        Ok(accepted)
    }
}
