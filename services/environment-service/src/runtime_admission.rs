//! Fresh `ResourceApproval` consumption for one immutable Environment release and compute generation.

use crate::ContainerReleaseResolver;
use async_trait::async_trait;
use contracts::environment::{
    EnvironmentInstance, EnvironmentRuntimeAuthorizationResolution,
    ResolveEnvironmentRuntimeAuthorizationRequest,
};
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
    operation_id: contracts::OperationId,
    generation: u64,
    deadline: contracts::UtcTimestamp,
) -> Result<(), RuntimeAdmissionError> {
    let admission = admission.ok_or(RuntimeAdmissionError::Unavailable)?;
    let store = crate::PgEnvironmentStore::new(pool);
    let (instance, now) = store.load_for_owner_resolution(environment_id).await?;
    if instance.operation.id != operation_id || instance.generation != generation {
        return Err(RuntimeAdmissionError::Binding);
    }
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


/// Classroom authoring and payer projects may differ only under the original exact receipt.
pub(crate) fn release_project_matches(instance: &EnvironmentInstance, projection: &contracts::events::ReleasePublished) -> bool {
    if instance.project_id == projection.release.project_id {
        return true;
    }
    if instance.class != contracts::authoring::EnvironmentClass::Experiment || instance.course_id.is_none() || !instance.has_resource_lease() {
        return false;
    }
    let Some(receipt) = &instance.runtime_authorization else { return false; };
    let Ok(digest) = Sha256Digest::of_canonical(&projection.environment_spec) else { return false; };
    receipt.authorization.configuration.authoring_project_id == projection.release.project_id
        && receipt.validate_for(&ResolveEnvironmentRuntimeAuthorizationRequest {
            expected_lease_id: instance.lease_id, resource_request_id: receipt.authorization.request_id,
            environment_id: Some(instance.id), project_id: instance.project_id, course_id: instance.course_id,
            owner_actor_id: instance.owner_id, release_id: instance.release_id, release_version: instance.release_version,
        }, &projection.environment_spec, &digest.to_string()).is_ok()
}
