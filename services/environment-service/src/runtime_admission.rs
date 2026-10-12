//! Fresh ResourceApproval consumption for one immutable Environment release and compute generation.

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
}

#[derive(Clone)]
pub struct EnvironmentRuntimeAdmission {
    releases: Arc<dyn ContainerReleaseResolver>,
    authority: Arc<dyn EnvironmentRuntimeAuthority>,
}

impl EnvironmentRuntimeAdmission {
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
        let resolution = self
            .authority
            .resolve_runtime_authorization(request)
            .await?;
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
        Ok(resolution)
    }

    pub async fn for_instance(
        &self,
        instance: &EnvironmentInstance,
    ) -> Result<EnvironmentRuntimeAuthorizationResolution, RuntimeAdmissionError> {
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
    #[error("LW_RUNTIME_CONFIGURATION_BINDING_MISMATCH")]
    Binding,
    #[error("LW_RUNTIME_RELEASE_UNAVAILABLE")]
    Release,
    #[error(transparent)]
    Resource(#[from] crate::ResourceUsageClientError),
}

impl RuntimeAdmissionError {
    pub fn diagnostic_code(&self) -> &str {
        match self {
            Self::Binding => "LW_RUNTIME_CONFIGURATION_BINDING_MISMATCH",
            Self::Release => "LW_RUNTIME_RELEASE_UNAVAILABLE",
            Self::Resource(error) => error.diagnostic_code(),
        }
    }
    pub const fn retryable(&self) -> bool {
        match self {
            Self::Resource(error) => error.retryable(),
            Self::Binding | Self::Release => false,
        }
    }
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
    if receipt.authorization.configuration.authoring_project_id != projection.release.project_id {
        return Err(crate::ReleaseProjectionError::IdentityMismatch);
    }
    Ok(projection.environment_spec.security.run_as_user.unwrap_or(
        if instance.runtime_kind == contracts::authoring::RuntimeKind::VirtualMachine {
            0
        } else {
            receipt.default_container_uid
        },
    ))
}
