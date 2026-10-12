//! Runtime bindings read Control projections and remain part of the existing Resource approval.
use contracts::authoring::{AuthoringApproval, EnvironmentCandidate, EnvironmentClass};
use contracts::platform_configuration::{
    PlatformConfigurationStatus, RuntimePreauthorizationScope,
};
use contracts::resource::{ResourceRequest, ResourceTarget};
use contracts::runtime_authorization::{
    RuntimeAdmissionBasis, RuntimeAuthorizationBinding, RuntimeAuthorizationError,
    RuntimeConfigurationIdentity, RuntimePolicyUse, check_platform_range, check_policy_use,
    declared_baseline_permissions, select_preauthorization,
};
use contracts::supply_chain::EnvironmentTemplateRelease;
use persistence_sqlx::Sha256Digest;
use sqlx::{PgPool, Postgres, Row, Transaction};

mod environment;
pub use environment::{CurrentEnvironmentAuthorization, current_environment_authorization};

#[derive(Debug, thiserror::Error)]
pub enum RuntimeBindingError {
    #[error(transparent)]
    Authorization(#[from] RuntimeAuthorizationError),
    #[error("LW_RUNTIME_SCOPE_DENIED")]
    ScopeDenied,
    #[error("LW_RUNTIME_LEASE_NOT_ACTIVE")]
    LeaseNotActive,
    #[error("LW_RUNTIME_AUTHORIZATION_INPUT_UNAVAILABLE")]
    Unavailable,
}

pub(crate) async fn current_inputs(
    tx: &mut Transaction<'_, Postgres>,
    request: &ResourceRequest,
) -> Result<
    (
        RuntimeConfigurationIdentity,
        EnvironmentCandidate,
        PlatformConfigurationStatus,
    ),
    RuntimeBindingError,
> {
    let ResourceTarget::Environment {
        release_id,
        release_version,
        ..
    } = &request.target
    else {
        return Err(RuntimeAuthorizationError::TargetNotEnvironment.into());
    };
    let row=sqlx::query("SELECT project_id,course_id,spec_sha256,release_contract,candidate_contract,authoring_approval_contract FROM control.runtime_authorization_configurations WHERE release_id=$1 AND release_version=$2")
        .bind(release_id.as_uuid()).bind(i64::try_from(*release_version).map_err(|_| RuntimeBindingError::ScopeDenied)?)
        .fetch_optional(&mut **tx).await.map_err(|_| RuntimeBindingError::Unavailable)?.ok_or(RuntimeBindingError::ScopeDenied)?;
    let candidate: EnvironmentCandidate = serde_json::from_value(
        row.try_get("candidate_contract")
            .map_err(|_| RuntimeBindingError::Unavailable)?,
    )
    .map_err(|_| RuntimeBindingError::Unavailable)?;
    let release: EnvironmentTemplateRelease = serde_json::from_value(
        row.try_get("release_contract")
            .map_err(|_| RuntimeBindingError::Unavailable)?,
    )
    .map_err(|_| RuntimeBindingError::Unavailable)?;
    release
        .validate()
        .map_err(|_| RuntimeBindingError::ScopeDenied)?;
    candidate
        .validate()
        .map_err(|_| RuntimeBindingError::ScopeDenied)?;
    release
        .validate_against_candidate(&candidate)
        .map_err(|_| RuntimeBindingError::ScopeDenied)?;
    let configuration_sha256 = Sha256Digest::of_canonical(&candidate.spec)
        .map_err(|_| RuntimeBindingError::Unavailable)?
        .to_string();
    if release.id != *release_id
        || release.version != *release_version
        || release.course_id != request.course_id
        || configuration_sha256
            != row
                .try_get::<String, _>("spec_sha256")
                .map_err(|_| RuntimeBindingError::Unavailable)?
    {
        return Err(RuntimeAuthorizationError::ConfigurationMismatch.into());
    }
    let approval: Option<AuthoringApproval> = row
        .try_get::<Option<serde_json::Value>, _>("authoring_approval_contract")
        .map_err(|_| RuntimeBindingError::Unavailable)?
        .map(serde_json::from_value)
        .transpose()
        .map_err(|_| RuntimeBindingError::Unavailable)?;
    if candidate.spec.class == EnvironmentClass::Work && release.project_id != request.project_id {
        return Err(RuntimeBindingError::ScopeDenied);
    }
    if candidate.spec.class == EnvironmentClass::Experiment {
        let approval = approval
            .as_ref()
            .ok_or(RuntimeAuthorizationError::TeacherApprovalRequired)?;
        approval
            .validate()
            .map_err(|_| RuntimeAuthorizationError::TeacherApprovalRequired)?;
        if approval.project_id != candidate.project_id
            || approval.course_id != candidate.course_id
            || approval.environment_candidate_id != candidate.id
            || approval.environment_candidate_revision != candidate.revision
            || approval.image_artifact != release.artifact
        {
            return Err(RuntimeAuthorizationError::TeacherApprovalRequired.into());
        }
        if let Some(course_id) = request.course_id {
            let teacher = approval
                .teacher_scope
                .as_ref()
                .ok_or(RuntimeAuthorizationError::TeacherApprovalRequired)?;
            if teacher.course_id != course_id || teacher.actor_id != approval.actor_id {
                return Err(RuntimeAuthorizationError::TeacherApprovalRequired.into());
            }
        } else if release.project_id != request.project_id {
            return Err(RuntimeBindingError::ScopeDenied);
        }
    }
    check_actor_scope(tx, request).await?;
    let value: serde_json::Value =
        sqlx::query_scalar("SELECT contract FROM control.runtime_authorization_policy")
            .fetch_one(&mut **tx)
            .await
            .map_err(|_| RuntimeBindingError::Unavailable)?;
    let status = serde_json::from_value(value).map_err(|_| RuntimeBindingError::Unavailable)?;
    let identity = RuntimeConfigurationIdentity {
        authoring_project_id: candidate.project_id,
        course_id: candidate.course_id,
        candidate_id: candidate.id,
        candidate_revision: candidate.revision,
        configuration_sha256,
        authoring_approval_id: approval.as_ref().map(|approval| approval.id),
        teacher_scope: approval.and_then(|approval| approval.teacher_scope),
    };
    Ok((identity, candidate, status))
}

async fn check_actor_scope(
    tx: &mut Transaction<'_, Postgres>,
    request: &ResourceRequest,
) -> Result<(), RuntimeBindingError> {
    let actor_active: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM access.actors WHERE actor_id=$1 AND disabled_at IS NULL)",
    )
    .bind(request.requester_id.as_uuid())
    .fetch_one(&mut **tx)
    .await
    .map_err(|_| RuntimeBindingError::Unavailable)?;
    let project_active: bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM control.project_memberships WHERE project_id=$1 AND actor_id=$2 AND state='active' AND object_role IN ('editor','manager','owner') AND (expires_at IS NULL OR expires_at>clock_timestamp()))")
        .bind(request.project_id.as_uuid()).bind(request.requester_id.as_uuid()).fetch_one(&mut **tx).await.map_err(|_| RuntimeBindingError::Unavailable)?;
    let global_admin: bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM access.current_authorization_sessions WHERE actor_id=$1 AND 'platform_admin'=ANY(platform_roles) AND expires_at>clock_timestamp() AND idle_expires_at>clock_timestamp())")
        .bind(request.requester_id.as_uuid()).fetch_one(&mut **tx).await.map_err(|_|RuntimeBindingError::Unavailable)?;
    if !actor_active || (!project_active && !global_admin) {
        return Err(RuntimeBindingError::ScopeDenied);
    }
    if let Some(course_id) = request.course_id
        && !global_admin
    {
        let course_active: bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM control.course_memberships WHERE course_id=$1 AND actor_id=$2 AND state='active' AND (expires_at IS NULL OR expires_at>clock_timestamp()))")
            .bind(course_id.as_uuid()).bind(request.requester_id.as_uuid()).fetch_one(&mut **tx).await.map_err(|_| RuntimeBindingError::Unavailable)?;
        if !course_active {
            return Err(RuntimeBindingError::ScopeDenied);
        }
    }
    Ok(())
}

fn select_permissions(
    policy: &contracts::platform_configuration::PlatformRuntimePolicy,
    request: &ResourceRequest,
    identity: &RuntimeConfigurationIdentity,
    candidate: &EnvironmentCandidate,
    declared: contracts::platform_configuration::RuntimePreauthorization,
) -> Result<
    (
        contracts::platform_configuration::RuntimePreauthorization,
        RuntimeAdmissionBasis,
    ),
    RuntimeAuthorizationError,
> {
    let approved_course = if candidate.spec.class == EnvironmentClass::Experiment
        && identity.teacher_scope.is_some()
    {
        request.course_id
    } else {
        None
    };
    let selected = select_preauthorization(policy, request.project_id, approved_course, &declared);
    let result = if let Some(selected) = selected {
        let basis = match selected.scope {
            RuntimePreauthorizationScope::Project { .. } => {
                RuntimeAdmissionBasis::ProjectPreauthorization
            }
            RuntimePreauthorizationScope::Course { .. } => {
                RuntimeAdmissionBasis::CoursePreauthorization
            }
        };
        (selected, basis)
    } else if declared.capabilities.is_empty() && !declared.privileged {
        (declared, RuntimeAdmissionBasis::RestrictedBaseline)
    } else {
        return Err(RuntimeAuthorizationError::ApprovalRequired);
    };
    Ok(result)
}

/// Browser approval resolves the actual immutable target. No runtime parameters or hashes are accepted.
pub async fn resolve_binding(
    pool: &PgPool,
    request: &ResourceRequest,
) -> Result<RuntimeAuthorizationBinding, RuntimeBindingError> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|_| RuntimeBindingError::Unavailable)?;
    let (identity, candidate, status) = current_inputs(&mut tx, request).await?;
    check_policy_use(&status, RuntimePolicyUse::Admission)?;
    let applied = status
        .applied
        .as_ref()
        .ok_or(RuntimeAuthorizationError::PolicyNotVerified)?;
    let declared = declared_baseline_permissions(&candidate.spec, request.project_id)?;
    check_platform_range(&applied.configuration.runtime_policy, &declared)?;
    let (permissions, basis) = select_permissions(
        &applied.configuration.runtime_policy,
        request,
        &identity,
        &candidate,
        declared,
    )?;
    let binding = RuntimeAuthorizationBinding {
        request_id: request.id,
        project_id: request.project_id,
        course_id: request.course_id,
        request_generation: request.generation,
        target: request.target.clone(),
        configuration: identity,
        platform_configuration_revision: applied.revision,
        platform_configuration_sha256: applied.configuration_sha256.clone(),
        permissions,
        basis,
    };
    tx.commit()
        .await
        .map_err(|_| RuntimeBindingError::Unavailable)?;
    Ok(binding)
}

/// Called under the existing request approval fence before its claim, lease and Outbox writes.
pub(crate) async fn validate_binding(
    tx: &mut Transaction<'_, Postgres>,
    request: &ResourceRequest,
    binding: &RuntimeAuthorizationBinding,
) -> Result<(), RuntimeBindingError> {
    validated_policy(tx, request, binding).await.map(|_| ())
}

async fn validated_policy(
    tx: &mut Transaction<'_, Postgres>,
    request: &ResourceRequest,
    binding: &RuntimeAuthorizationBinding,
) -> Result<contracts::platform_configuration::PlatformConfigurationVersion, RuntimeBindingError> {
    let (identity, candidate, status) = current_inputs(tx, request).await?;
    binding.validate_current(request, &identity, &status)?;
    let declared = declared_baseline_permissions(&candidate.spec, request.project_id)?;
    if declared.capabilities != binding.permissions.capabilities
        || declared.privileged != binding.permissions.privileged
    {
        return Err(RuntimeAuthorizationError::ConfigurationMismatch.into());
    }
    let applied = status
        .applied
        .as_ref()
        .ok_or(RuntimeAuthorizationError::PolicyNotVerified)?;
    let (permissions, basis) = select_permissions(
        &applied.configuration.runtime_policy,
        request,
        &identity,
        &candidate,
        declared,
    )?;
    if binding.permissions != permissions || binding.basis != basis {
        return Err(RuntimeAuthorizationError::ConfigurationMismatch.into());
    }
    Ok(applied.clone())
}

/// Reads the existing approval and checks its current request, expiry, Control identity and policy.
pub async fn current_authorization(
    pool: &PgPool,
    request: &ResourceRequest,
) -> Result<RuntimeAuthorizationBinding, RuntimeBindingError> {
    if !matches!(
        request.state,
        contracts::resource::ResourceRequestState::Allocating
            | contracts::resource::ResourceRequestState::Active
    ) {
        return Err(RuntimeBindingError::ScopeDenied);
    }
    let mut tx = pool
        .begin()
        .await
        .map_err(|_| RuntimeBindingError::Unavailable)?;
    let current: serde_json::Value = sqlx::query_scalar(
        "SELECT contract FROM resource.resource_requests WHERE request_id=$1 FOR SHARE",
    )
    .bind(request.id.as_uuid())
    .fetch_optional(&mut *tx)
    .await
    .map_err(|_| RuntimeBindingError::Unavailable)?
    .ok_or(RuntimeBindingError::ScopeDenied)?;
    let current: ResourceRequest =
        serde_json::from_value(current).map_err(|_| RuntimeBindingError::Unavailable)?;
    if current != *request {
        return Err(RuntimeAuthorizationError::ConfigurationMismatch.into());
    }
    let row=sqlx::query("SELECT contract FROM resource.resource_approvals WHERE request_id=$1 AND revoked_at IS NULL AND valid_until>clock_timestamp() ORDER BY created_at DESC,approval_id DESC LIMIT 1")
        .bind(request.id.as_uuid()).fetch_optional(&mut *tx).await.map_err(|_|RuntimeBindingError::Unavailable)?.ok_or(RuntimeAuthorizationError::ApprovalRequired)?;
    let approval: contracts::resource::ResourceApproval = serde_json::from_value(
        row.try_get("contract")
            .map_err(|_| RuntimeBindingError::Unavailable)?,
    )
    .map_err(|_| RuntimeBindingError::Unavailable)?;
    let binding = approval
        .runtime_authorization
        .ok_or(RuntimeAuthorizationError::ApprovalRequired)?;
    validate_binding(&mut tx, request, &binding).await?;
    tx.commit()
        .await
        .map_err(|_| RuntimeBindingError::Unavailable)?;
    Ok(binding)
}
