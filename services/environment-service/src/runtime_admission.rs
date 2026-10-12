//! Fresh `ResourceApproval` consumption for one immutable Environment release and compute generation.

use crate::ContainerReleaseResolver;
use async_trait::async_trait;
use contracts::environment::{
    EnvironmentInstance, EnvironmentRuntimeAuthorizationResolution,
    ResolveEnvironmentRuntimeAuthorizationRequest,
};
use contracts::platform_configuration::LinuxCapability;
use persistence_sqlx::Sha256Digest;
use std::sync::Arc;

#[async_trait]
pub trait EnvironmentRuntimeAuthority: Send + Sync {
    async fn resolve_runtime_authorization(
        &self,
        request: &ResolveEnvironmentRuntimeAuthorizationRequest,
    ) -> Result<EnvironmentRuntimeAuthorizationResolution, crate::ResourceUsageClientError>;

    async fn resolve_credit_grant(
        &self,
        authorization: &contracts::environment::EnvironmentLeaseAuthorization,
    ) -> Result<
        contracts::resource::environment_funding::EnvironmentCreditGrant,
        crate::ResourceUsageClientError,
    >;
}

#[async_trait]
impl EnvironmentRuntimeAuthority for crate::metering::ResourceUsageClient {
    async fn resolve_runtime_authorization(
        &self,
        request: &ResolveEnvironmentRuntimeAuthorizationRequest,
    ) -> Result<EnvironmentRuntimeAuthorizationResolution, crate::ResourceUsageClientError> {
        self.post_json("internal/v1/environment-runtime-authorization", request)
            .await?
            .json()
            .await
            .map_err(|_| crate::ResourceUsageClientError::InvalidResponse)
    }

    async fn resolve_credit_grant(
        &self,
        authorization: &contracts::environment::EnvironmentLeaseAuthorization,
    ) -> Result<
        contracts::resource::environment_funding::EnvironmentCreditGrant,
        crate::ResourceUsageClientError,
    > {
        self.get_json(&format!(
            "internal/v1/environment-credit-grants/{}",
            authorization.lease_id
        ))
        .await?
        .json()
        .await
        .map_err(|_| crate::ResourceUsageClientError::InvalidResponse)
    }
}

#[derive(Clone)]
pub struct EnvironmentRuntimeAdmission {
    releases: Arc<dyn ContainerReleaseResolver>,
    authority: Arc<dyn EnvironmentRuntimeAuthority>,
}

impl EnvironmentRuntimeAdmission {
    pub async fn from_env<R: ContainerReleaseResolver + 'static>(
        releases: R,
    ) -> Result<Self, crate::ResourceUsageClientError> {
        Ok(Self::new(
            releases,
            Arc::new(crate::metering::ResourceUsageClient::from_env().await?),
        ))
    }
    pub fn new<R: ContainerReleaseResolver + 'static>(
        releases: R,
        authority: Arc<dyn EnvironmentRuntimeAuthority>,
    ) -> Self {
        Self {
            releases: Arc::new(releases),
            authority,
        }
    }

    pub async fn resolve(
        &self,
        request: &ResolveEnvironmentRuntimeAuthorizationRequest,
    ) -> Result<EnvironmentRuntimeAuthorizationResolution, RuntimeAdmissionError> {
        let release = self
            .releases
            .resolve(request.release_id, request.release_version)
            .await
            .map_err(|_| RuntimeAdmissionError::Release)?;
        if release.withdrawn_at.is_some()
            || release.projection.release.course_id != request.course_id
        {
            return Err(RuntimeAdmissionError::Release);
        }
        let mut resolution = self
            .authority
            .resolve_runtime_authorization(request)
            .await?;
        let provider_binding = match &release.projection.environment_spec.runtime {
            contracts::authoring::EnvironmentRuntimeSpec::Container {
                provider_binding, ..
            }
            | contracts::authoring::EnvironmentRuntimeSpec::VirtualMachine {
                provider_binding,
                ..
            } => provider_binding,
        };
        if resolution.valid_until <= release.authority_now
            || resolution.provider_binding != *provider_binding
        {
            return Err(RuntimeAdmissionError::Binding);
        }
        let digest = Sha256Digest::of_canonical(&release.projection.environment_spec)
            .map_err(|_| RuntimeAdmissionError::Binding)?;
        resolution
            .validate_for(
                request,
                &release.projection.environment_spec,
                &digest.to_string(),
            )
            .map_err(|_| RuntimeAdmissionError::Binding)?;
        if resolution.authorization.configuration.authoring_project_id
            != release.projection.release.project_id
            || resolution.authorization.configuration.course_id
                != release.projection.release.course_id
        {
            return Err(RuntimeAdmissionError::Binding);
        }
        let grant = self
            .authority
            .resolve_credit_grant(&resolution.lease_authorization)
            .await?;
        validate_credit_grant(
            &grant,
            &resolution.lease_authorization,
            release.authority_now,
        )?;
        // Funds and runtime permission remain separate Resource facts. The persisted receipt's
        // execution deadline is bounded by both; no second hold or consumption marker is minted.
        resolution.valid_until = resolution.valid_until.min(grant.covered_until);
        Ok(resolution)
    }

    pub async fn for_instance(
        &self,
        instance: &EnvironmentInstance,
    ) -> Result<EnvironmentRuntimeAuthorizationResolution, RuntimeAdmissionError> {
        if !instance.has_resource_lease() {
            return Err(RuntimeAdmissionError::AllocationPathUnsupported);
        }
        let request_id = instance
            .runtime_authorization
            .as_ref()
            .map(|receipt| receipt.authorization.request_id)
            .or_else(|| {
                instance
                    .operation
                    .lease_authorization
                    .as_ref()
                    .map(|lease| lease.resource_request_id)
            })
            .ok_or(RuntimeAdmissionError::Binding)?;
        self.resolve(&ResolveEnvironmentRuntimeAuthorizationRequest {
            expected_lease_id: instance.lease_id,
            resource_request_id: request_id,
            environment_id: Some(instance.id),
            project_id: instance.project_id,
            course_id: instance.course_id,
            owner_actor_id: instance.owner_id,
            release_id: instance.release_id,
            release_version: instance.release_version,
        })
        .await
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RuntimeAdmissionError {
    #[error("LW_RUNTIME_AUTHORITY_UNAVAILABLE")]
    Unavailable,
    #[error("LW_RUNTIME_EXECUTION_DEADLINE_EXCEEDED")]
    Deadline,
    #[error(transparent)]
    Store(#[from] crate::EnvironmentStoreError),
    #[error("LW_RUNTIME_ALLOCATION_PATH_UNSUPPORTED")]
    AllocationPathUnsupported,
    #[error("LW_RUNTIME_FUNDING_INACTIVE")]
    FundingInactive,
    #[error("LW_RUNTIME_FUNDING_CHANGED")]
    FundingChanged,
    #[error("LW_RUNTIME_CONFIGURATION_BINDING_MISMATCH")]
    Binding,
    #[error("LW_RUNTIME_RELEASE_UNAVAILABLE")]
    Release,
    #[error(transparent)]
    Resource(#[from] crate::ResourceUsageClientError),
}

impl RuntimeAdmissionError {
    #[must_use]
    pub fn diagnostic_code(&self) -> &str {
        match self {
            Self::Unavailable => "LW_RUNTIME_AUTHORITY_UNAVAILABLE",
            Self::Deadline => "LW_RUNTIME_EXECUTION_DEADLINE_EXCEEDED",
            Self::Store(_) => "LW_RUNTIME_AUTHORITY_READ_FAILED",
            Self::AllocationPathUnsupported => "LW_RUNTIME_ALLOCATION_PATH_UNSUPPORTED",
            Self::FundingInactive => "LW_RUNTIME_FUNDING_INACTIVE",
            Self::FundingChanged => "LW_RUNTIME_FUNDING_CHANGED",
            Self::Binding => "LW_RUNTIME_CONFIGURATION_BINDING_MISMATCH",
            Self::Release => "LW_RUNTIME_RELEASE_UNAVAILABLE",
            Self::Resource(error) => error.diagnostic_code(),
        }
    }
    #[must_use]
    pub const fn retryable(&self) -> bool {
        match self {
            Self::Store(error) => error.retryable(),
            Self::Resource(error) => error.retryable(),
            Self::FundingChanged => true,
            Self::Unavailable
            | Self::Deadline
            | Self::FundingInactive
            | Self::Binding
            | Self::Release
            | Self::AllocationPathUnsupported => false,
        }
    }
}

/// Fresh execution permission is read before opening the executor's fencing transaction.
/// A completed response can still be replayed if this read fails; a new effect cannot start.
pub(crate) async fn executor_authorization(
    admission: Option<&EnvironmentRuntimeAdmission>,
    pool: sqlx::PgPool,
    environment_id: contracts::EnvironmentId,
    deadline: contracts::UtcTimestamp,
) -> Result<(), RuntimeAdmissionError> {
    let admission = admission.ok_or(RuntimeAdmissionError::Unavailable)?;
    let store = crate::PgEnvironmentStore::new(pool);
    let (instance, now) = store.load_for_owner_resolution(environment_id).await?;
    let remaining = std::time::Duration::try_from(deadline.get() - now.get())
        .map_err(|_| RuntimeAdmissionError::Deadline)?;
    if remaining.is_zero() {
        return Err(RuntimeAdmissionError::Deadline);
    }
    let receipt = tokio::time::timeout(remaining, admission.for_instance(&instance))
        .await
        .map_err(|_| RuntimeAdmissionError::Deadline)??;
    if deadline > receipt.valid_until {
        return Err(RuntimeAdmissionError::Deadline);
    }
    Ok(())
}

fn validate_credit_grant(
    grant: &contracts::resource::environment_funding::EnvironmentCreditGrant,
    lease: &contracts::environment::EnvironmentLeaseAuthorization,
    now: contracts::UtcTimestamp,
) -> Result<(), RuntimeAdmissionError> {
    if grant.request_id != lease.resource_request_id
        || grant.lease_id != lease.lease_id
        || grant.claim_id.to_string() != lease.capacity_binding
        || grant.hold.request_id != Some(lease.resource_request_id)
        || grant.hold.project_id != lease.project_id
        || grant.estimate.project_id != lease.project_id
    {
        return Err(RuntimeAdmissionError::Binding);
    }
    if grant.lease_revision > lease.lease_revision {
        // A concurrent funded renewal may occur between the two read-only Resource queries.
        // Re-read the original authorization within the existing operation retry budget.
        return Err(RuntimeAdmissionError::FundingChanged);
    }
    if grant.lease_revision != lease.lease_revision
        || grant.covered_from != lease.active_from
        || grant.covered_until != lease.expires_at
    {
        return Err(RuntimeAdmissionError::Binding);
    }
    if !grant.can_authorize_execution
        || grant.hold.state != contracts::resource::credits::CreditHoldState::Pending
        || grant.hold.measurement_unknown
        || grant.hold.expired_unknown_at.is_some()
        || grant.covered_from > now
        || grant.covered_until <= now
    {
        return Err(RuntimeAdmissionError::FundingInactive);
    }
    Ok(())
}

pub(crate) fn authorized_uid(
    instance: &EnvironmentInstance,
    projection: &contracts::events::ReleasePublished,
) -> Result<u32, crate::ReleaseProjectionError> {
    let receipt = instance
        .runtime_authorization
        .as_ref()
        .ok_or(crate::ReleaseProjectionError::IdentityMismatch)?;
    let hash = Sha256Digest::of_canonical(&projection.environment_spec)
        .map_err(|_| crate::ReleaseProjectionError::ContractInvalid)?;
    let request = ResolveEnvironmentRuntimeAuthorizationRequest {
        expected_lease_id: instance.lease_id,
        resource_request_id: receipt.authorization.request_id,
        environment_id: Some(instance.id),
        project_id: instance.project_id,
        course_id: instance.course_id,
        owner_actor_id: instance.owner_id,
        release_id: instance.release_id,
        release_version: instance.release_version,
    };
    receipt
        .validate_for(&request, &projection.environment_spec, &hash.to_string())
        .map_err(|_| crate::ReleaseProjectionError::IdentityMismatch)?;
    if receipt.provider_binding != instance.provider_binding
        || receipt.authorization.configuration.authoring_project_id != projection.release.project_id
    {
        return Err(crate::ReleaseProjectionError::IdentityMismatch);
    }
    let uid = projection.environment_spec.security.run_as_user.unwrap_or(
        if instance.runtime_kind == contracts::authoring::RuntimeKind::VirtualMachine {
            0
        } else {
            receipt.default_container_uid
        },
    );
    if uid == u32::MAX {
        return Err(crate::ReleaseProjectionError::SecurityPostureInvalid);
    }
    Ok(uid)
}

/// One projection used by the provider and durable executor admission.
pub(crate) fn container_security_context(
    instance: &EnvironmentInstance,
    projection: &contracts::events::ReleasePublished,
) -> Result<serde_json::Value, crate::ReleaseProjectionError> {
    let uid = authorized_uid(instance, projection)?;
    let security = &projection.environment_spec.security;
    // Current execution supports the baseline capability set plus isolated pod networking.
    // Broader kernel/host capabilities need the same proven boundary as privileged execution.
    if security.privileged
        || security.capabilities.iter().any(|cap| {
            !matches!(
                cap,
                LinuxCapability::AuditWrite
                    | LinuxCapability::Chown
                    | LinuxCapability::DacOverride
                    | LinuxCapability::Fowner
                    | LinuxCapability::Fsetid
                    | LinuxCapability::Kill
                    | LinuxCapability::Mknod
                    | LinuxCapability::NetBindService
                    | LinuxCapability::Setfcap
                    | LinuxCapability::Setgid
                    | LinuxCapability::Setpcap
                    | LinuxCapability::Setuid
                    | LinuxCapability::SysChroot
                    | LinuxCapability::NetAdmin
                    | LinuxCapability::NetRaw
            )
        })
    {
        return Err(crate::ReleaseProjectionError::SecurityPostureInvalid);
    }
    Ok(
        serde_json::json!({"runAsNonRoot":uid!=0,"runAsUser":uid,"privileged":false,
        "allowPrivilegeEscalation":security.privilege_escalation_policy==contracts::authoring::PrivilegeEscalationPolicy::Allow,
        "readOnlyRootFilesystem":security.root_filesystem_policy==contracts::authoring::RootFilesystemPolicy::ReadOnlyRequired,
        "capabilities":{"drop":["ALL"],"add":security.capabilities},"seccompProfile":{"type":"RuntimeDefault"}}),
    )
}

pub(crate) fn validate_container_runtime_plan(
    instance: &EnvironmentInstance,
    projection: &contracts::events::ReleasePublished,
    plan: &crate::ContainerResourcePlan,
    generation: u64,
) -> Result<(), crate::ReleaseProjectionError> {
    if instance.id != plan.environment_id
        || instance.project_id != plan.project_id
        || instance.generation != generation
        || instance.runtime_kind != contracts::authoring::RuntimeKind::Container
    {
        return Err(crate::ReleaseProjectionError::IdentityMismatch);
    }
    let expected = container_security_context(instance, projection)?;
    let uid = authorized_uid(instance, projection)?;
    let deployment = plan
        .resources
        .iter()
        .find(|r| r.kind == "Deployment")
        .ok_or(crate::ReleaseProjectionError::ContractInvalid)?;
    let pod = deployment
        .document
        .pointer("/spec/template/spec")
        .ok_or(crate::ReleaseProjectionError::ContractInvalid)?;
    if pod["securityContext"]
        != serde_json::json!({"runAsNonRoot":uid!=0,"runAsUser":uid,"runAsGroup":uid,"fsGroup":uid,"seccompProfile":{"type":"RuntimeDefault"}})
        || ["containers", "initContainers"].iter().any(|name| {
            pod.get(name)
                .and_then(serde_json::Value::as_array)
                .is_none_or(|values| {
                    values.is_empty()
                        || values
                            .iter()
                            .any(|value| value["securityContext"] != expected)
                })
        })
        || pod
            .get("ephemeralContainers")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|values| !values.is_empty())
    {
        return Err(crate::ReleaseProjectionError::SecurityPostureInvalid);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn executor_rejects_uid_capability_project_and_generation_changes()
    -> Result<(), Box<dyn std::error::Error>> {
        let instance = crate::test_support::requested_instance();
        let projection = crate::test_support::runtime_projection(&instance);
        let mut plan = crate::ContainerResourcePlan {
            environment_id: instance.id,
            project_id: instance.project_id,
            namespace: format!("lw-env-{}", instance.id),
            namespace_uid: None,
            image: "synthetic/runtime".to_owned(),
            plan_sha256: Sha256Digest::of_bytes(b"synthetic"),
            resources: vec![crate::ContainerResource {
                kind: "Deployment".to_owned(),
                namespace: Some(format!("lw-env-{}", instance.id)),
                name: "runtime".to_owned(),
                document: json!({"spec":{"template":{"spec":{"securityContext":{"runAsNonRoot":false,"runAsUser":0,"runAsGroup":0,"fsGroup":0,"seccompProfile":{"type":"RuntimeDefault"}},"containers":[{"securityContext":{"runAsNonRoot":false,"runAsUser":0,"privileged":false,"allowPrivilegeEscalation":false,"readOnlyRootFilesystem":false,"capabilities":{"drop":["ALL"],"add":[]},"seccompProfile":{"type":"RuntimeDefault"}}}],"initContainers":[{"securityContext":{"runAsNonRoot":false,"runAsUser":0,"privileged":false,"allowPrivilegeEscalation":false,"readOnlyRootFilesystem":false,"capabilities":{"drop":["ALL"],"add":[]},"seccompProfile":{"type":"RuntimeDefault"}}}]}}}}),
            }],
        };
        validate_container_runtime_plan(&instance, &projection, &plan, instance.generation)?;
        assert!(
            validate_container_runtime_plan(&instance, &projection, &plan, instance.generation + 1)
                .is_err()
        );
        let original = plan.clone();
        plan.project_id = contracts::ProjectId::new();
        assert!(
            validate_container_runtime_plan(&instance, &projection, &plan, instance.generation)
                .is_err()
        );
        for (field, value) in [
            ("runAsUser", json!(1000)),
            ("privileged", json!(true)),
            ("capabilities", json!({"drop":["ALL"],"add":["NET_ADMIN"]})),
            ("readOnlyRootFilesystem", json!(true)),
        ] {
            plan = original.clone();
            plan.resources[0].document["spec"]["template"]["spec"]["containers"][0]["securityContext"]
                [field] = value;
            assert!(
                validate_container_runtime_plan(&instance, &projection, &plan, instance.generation)
                    .is_err(),
                "{field}"
            );
        }
        Ok(())
    }
}
