//! Deterministic, fail-closed orchestration for approved Container image builds.
#![allow(
    missing_docs,
    reason = "the contracts crate owns public wire documentation; this module exposes provider integration seams"
)]

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use contracts::events::AgentBuildRequested;
use contracts::supply_chain::ImageArtifact;
use contracts::{BuildRequestId, ImageArtifactId, UtcTimestamp};
use persistence_sqlx::Sha256Digest; // internal persistence hash, not contract hash
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;
use uuid::Uuid;

pub const BUILD_EXECUTOR_PROTOCOL_VERSION: u8 = 2;

/// Immutable identity shared by every provider stage and cleanup attempt.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct BuildIdentity(pub Sha256Digest);

/// Monotonic database lease identity that fences every remote build side effect.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BuildExecutionFence {
    pub generation: u32,
    pub lease_token: Uuid,
    pub deadline_at: UtcTimestamp,
}

impl BuildExecutionFence {
    pub fn new(
        generation: u32,
        lease_token: Uuid,
        deadline_at: UtcTimestamp,
    ) -> Result<Self, BuildPipelineError> {
        if generation == 0 {
            return Err(BuildPipelineError::new(
                BuildFailureCode::ConfigurationInvalid,
                false,
                true,
            ));
        }
        Ok(Self {
            generation,
            lease_token,
            deadline_at,
        })
    }

    fn request_context(
        self,
        build_request_id: BuildRequestId,
        stage: BuildProviderStage,
    ) -> BuildProviderRequestContext {
        let request_identity = format!(
            "{}\0{}\0{}\0{}\0{}",
            BUILD_EXECUTOR_PROTOCOL_VERSION,
            build_request_id,
            self.generation,
            self.lease_token,
            stage.as_str()
        );
        BuildProviderRequestContext {
            protocol_version: BUILD_EXECUTOR_PROTOCOL_VERSION,
            build_request_id,
            fence_generation: self.generation,
            lease_token: self.lease_token,
            stage,
            stage_request_id: Sha256Digest::of_bytes(request_identity.as_bytes()),
            deadline_at: self.deadline_at,
        }
    }
}

/// Stable build-executor stage vocabulary used for fencing and tombstones.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BuildProviderStage {
    EnsurePrivateProject,
    Build,
    Publish,
    Cleanup,
}

impl BuildProviderStage {
    const fn as_str(self) -> &'static str {
        match self {
            Self::EnsurePrivateProject => "ensure_private_project",
            Self::Build => "build",
            Self::Publish => "publish",
            Self::Cleanup => "cleanup",
        }
    }
}

/// Exact operation identity the executor must persist before producing a side effect.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BuildProviderRequestContext {
    pub protocol_version: u8,
    pub build_request_id: BuildRequestId,
    pub fence_generation: u32,
    pub lease_token: Uuid,
    pub stage: BuildProviderStage,
    pub stage_request_id: Sha256Digest,
    pub deadline_at: UtcTimestamp,
}

/// Non-secret policy bindings frozen into one build worker deployment.
#[derive(Clone, Debug)]
pub struct BuildPipelinePolicy {
    pub builder_binding: String,
    pub registry_binding: String,
    pub registry_robot_name: String,
    pub stage_timeout: Duration,
}

impl BuildPipelinePolicy {
    fn validate(&self) -> Result<(), BuildPipelineError> {
        let bindings = [
            self.builder_binding.as_str(),
            self.registry_binding.as_str(),
        ];
        if bindings.iter().any(|binding| {
            binding.trim().is_empty()
                || binding.contains("://")
                || binding.bytes().any(|byte| byte.is_ascii_whitespace())
        }) || self.registry_robot_name.trim().is_empty()
            || !self
                .registry_robot_name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
            || self.stage_timeout.is_zero()
            || self.stage_timeout > Duration::from_hours(1)
        {
            return Err(BuildPipelineError::new(
                BuildFailureCode::ConfigurationInvalid,
                false,
                true,
            ));
        }
        Ok(())
    }
}

/// `BuildKit` output still carrying candidate-only registry identity.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BuiltCandidate {
    pub build_request_id: BuildRequestId,
    pub build_identity: BuildIdentity,
    pub repository: String,
    pub digest: String,
}

/// Non-secret proof that the exact per-course Harbor project is private and usable.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PrivateRegistryProject {
    pub build_request_id: BuildRequestId,
    pub build_identity: BuildIdentity,
    pub repository_prefix: String,
    pub private: bool,
    pub storage_quota_bytes: u64,
    pub robot_subject: String,
}

/// Immutable Harbor publication receipt.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PublishedImage {
    pub build_identity: BuildIdentity,
    pub digest: String,
}

/// Complete result persisted by the Agent authority only after every gate passes.
#[derive(Clone, Debug, PartialEq)]
pub struct BuildPipelineOutput {
    pub build_identity: BuildIdentity,
    pub registry_project: PrivateRegistryProject,
    pub artifact: ImageArtifact,
}

/// Exact provider contract; no stage may select a fallback implementation.
#[async_trait]
pub trait BuildSupplyChainProvider: Send + Sync {
    fn builder_binding(&self) -> &str;
    fn registry_binding(&self) -> &str;

    async fn ensure_private_project(
        &self,
        context: &BuildProviderRequestContext,
        command: &AgentBuildRequested,
        identity: BuildIdentity,
    ) -> Result<PrivateRegistryProject, BuildProviderFailure>;

    async fn build_candidate(
        &self,
        context: &BuildProviderRequestContext,
        command: &AgentBuildRequested,
        identity: BuildIdentity,
    ) -> Result<BuiltCandidate, BuildProviderFailure>;

    async fn publish_immutable(
        &self,
        context: &BuildProviderRequestContext,
        candidate: &BuiltCandidate,
    ) -> Result<PublishedImage, BuildProviderFailure>;

    async fn cleanup_candidate(
        &self,
        context: &BuildProviderRequestContext,
        build_request_id: BuildRequestId,
        identity: BuildIdentity,
    ) -> Result<(), BuildProviderFailure>;
}

/// Cooperative cancellation shared with Provider stage deadlines.
#[derive(Clone, Default)]
pub struct BuildCancellation {
    inner: Arc<BuildCancellationInner>,
}

#[derive(Default)]
struct BuildCancellationInner {
    cancelled: AtomicBool,
    notify: Notify,
}

impl BuildCancellation {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.inner.cancelled.store(true, Ordering::Release);
        self.inner.notify.notify_waiters();
    }

    async fn cancelled(&self) {
        loop {
            if self.inner.cancelled.load(Ordering::Acquire) {
                return;
            }
            let notified = self.inner.notify.notified();
            if self.inner.cancelled.load(Ordering::Acquire) {
                return;
            }
            notified.await;
        }
    }
}

/// One deterministic pipeline with bounded provider calls and mandatory failure cleanup.
pub struct BuildPipeline<P> {
    provider: P,
    policy: BuildPipelinePolicy,
}

impl<P: BuildSupplyChainProvider> BuildPipeline<P> {
    pub fn new(provider: P, policy: BuildPipelinePolicy) -> Result<Self, BuildPipelineError> {
        policy.validate()?;
        if provider.builder_binding() != policy.builder_binding
            || provider.registry_binding() != policy.registry_binding
        {
            return Err(BuildPipelineError::new(
                BuildFailureCode::ProviderBindingMismatch,
                false,
                true,
            ));
        }
        Ok(Self { provider, policy })
    }

    /// Executes a complete candidate build. Any failure after admission invokes cleanup.
    pub async fn execute(
        &self,
        command: &AgentBuildRequested,
        started_at: UtcTimestamp,
        fence: BuildExecutionFence,
        cancellation: &BuildCancellation,
    ) -> Result<BuildPipelineOutput, BuildPipelineError> {
        command
            .validate()
            .map_err(|_| BuildPipelineError::new(BuildFailureCode::CommandInvalid, false, true))?;
        if command.request.builder_binding != self.policy.builder_binding {
            return Err(BuildPipelineError::new(
                BuildFailureCode::ProviderBindingMismatch,
                false,
                true,
            ));
        }
        let identity = BuildIdentity(Sha256Digest::of_bytes(command.request.id.as_uuid().as_bytes()));
        let execution_timeout = Duration::from_millis(command.request.max_duration_milliseconds);
        if fence.deadline_at
            != add_milliseconds(started_at, command.request.max_duration_milliseconds)?
        {
            return Err(BuildPipelineError::new(
                BuildFailureCode::ConfigurationInvalid,
                false,
                true,
            ));
        }
        match tokio::time::timeout(
            execution_timeout,
            self.execute_inner(command, started_at, fence, cancellation, identity),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => Err(self
                .cleanup(
                    command.request.id,
                    identity,
                    fence,
                    BuildPipelineError::new(BuildFailureCode::TimedOut, true, true),
                )
                .await),
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the ordered supply-chain gates and their cleanup edges form one auditable transaction"
    )]
    async fn execute_inner(
        &self,
        command: &AgentBuildRequested,
        _started_at: UtcTimestamp,
        fence: BuildExecutionFence,
        cancellation: &BuildCancellation,
        identity: BuildIdentity,
    ) -> Result<BuildPipelineOutput, BuildPipelineError> {
        let project_context =
            fence.request_context(command.request.id, BuildProviderStage::EnsurePrivateProject);
        let project = self
            .stage(
                cancellation,
                self.provider
                    .ensure_private_project(&project_context, command, identity),
            )
            .await;
        let project = match project {
            Ok(project) => project,
            Err(error) => {
                return Err(self
                    .cleanup(command.request.id, identity, fence, error)
                    .await);
            }
        };
        let expected_repository_prefix = match expected_course_repository_prefix(command) {
            Ok(prefix) => prefix,
            Err(error) => {
                return Err(self
                    .cleanup(command.request.id, identity, fence, error)
                    .await);
            }
        };
        if project.build_request_id != command.request.id
            || project.build_identity != identity
            || project.repository_prefix != expected_repository_prefix
            || !project.private
            || project.storage_quota_bytes == 0
            || project.robot_subject.trim().is_empty()
        {
            return Err(self
                .cleanup(
                    command.request.id,
                    identity,
                    fence,
                    BuildPipelineError::new(BuildFailureCode::RegistryProjectInvalid, false, true),
                )
                .await);
        }
        let build_context = fence.request_context(command.request.id, BuildProviderStage::Build);
        let candidate = match self
            .stage(
                cancellation,
                self.provider
                    .build_candidate(&build_context, command, identity),
            )
            .await
        {
            Ok(candidate) => candidate,
            Err(error) => {
                return Err(self
                    .cleanup(command.request.id, identity, fence, error)
                    .await);
            }
        };
        if candidate.build_request_id != command.request.id
            || candidate.build_identity != identity
            || candidate.repository != command.request.output_repository
            || validate_digest(&candidate.digest).is_err()
        {
            return Err(self
                .cleanup(
                    command.request.id,
                    identity,
                    fence,
                    BuildPipelineError::new(BuildFailureCode::BuildIdentityMismatch, false, true),
                )
                .await);
        }
        let publish_context =
            fence.request_context(command.request.id, BuildProviderStage::Publish);
        let published = match self
            .stage(
                cancellation,
                self.provider
                    .publish_immutable(&publish_context, &candidate),
            )
            .await
        {
            Ok(published) => published,
            Err(error) => {
                return Err(self
                    .cleanup(command.request.id, identity, fence, error)
                    .await);
            }
        };
        if published.build_identity != identity || published.digest != candidate.digest {
            return Err(self
                .cleanup(
                    command.request.id,
                    identity,
                    fence,
                    BuildPipelineError::new(
                        BuildFailureCode::PublicationIdentityMismatch,
                        false,
                        true,
                    ),
                )
                .await);
        }
        let output = (|| {
            let artifact_id = ImageArtifactId::new();
            let artifact = ImageArtifact::Container {
                id: artifact_id,
                build_request_id: command.request.id,
                repository: candidate.repository,
                digest: candidate.digest,
            };
            artifact.validate().map_err(|_| {
                BuildPipelineError::new(BuildFailureCode::ArtifactInvalid, false, true)
            })?;
            Ok(BuildPipelineOutput {
                build_identity: identity,
                registry_project: project,
                artifact,
            })
        })();
        match output {
            Ok(output) => {
                self.cleanup_success(command.request.id, identity, fence)
                    .await?;
                Ok(output)
            }
            Err(error) => Err(self
                .cleanup(command.request.id, identity, fence, error)
                .await),
        }
    }

    async fn stage<T, F>(
        &self,
        cancellation: &BuildCancellation,
        future: F,
    ) -> Result<T, BuildPipelineError>
    where
        F: Future<Output = Result<T, BuildProviderFailure>>,
    {
        tokio::select! {
            biased;
            () = cancellation.cancelled() => Err(BuildPipelineError::new(
                BuildFailureCode::Cancelled,
                false,
                true,
            )),
            result = tokio::time::timeout(self.policy.stage_timeout, future) => match result {
                Ok(Ok(value)) => Ok(value),
                Ok(Err(failure)) => Err(BuildPipelineError::new(
                    BuildFailureCode::Provider(failure.code),
                    failure.retryable
                        && matches!(failure.code, BuildProviderFailureCode::Unavailable),
                    true,
                )),
                Err(_) => Err(BuildPipelineError::new(
                    BuildFailureCode::TimedOut,
                    true,
                    true,
                )),
            }
        }
    }

    async fn cleanup(
        &self,
        build_request_id: BuildRequestId,
        identity: BuildIdentity,
        fence: BuildExecutionFence,
        mut original: BuildPipelineError,
    ) -> BuildPipelineError {
        let context = fence.request_context(build_request_id, BuildProviderStage::Cleanup);
        match tokio::time::timeout(
            self.policy.stage_timeout,
            self.provider
                .cleanup_candidate(&context, build_request_id, identity),
        )
        .await
        {
            Ok(Ok(())) => {
                original.cleanup_verified = true;
                original
            }
            Ok(Err(_)) | Err(_) => {
                BuildPipelineError::new(BuildFailureCode::CleanupFailed, false, false)
            }
        }
    }

    async fn cleanup_success(
        &self,
        build_request_id: BuildRequestId,
        identity: BuildIdentity,
        fence: BuildExecutionFence,
    ) -> Result<(), BuildPipelineError> {
        let context = fence.request_context(build_request_id, BuildProviderStage::Cleanup);
        match tokio::time::timeout(
            self.policy.stage_timeout,
            self.provider
                .cleanup_candidate(&context, build_request_id, identity),
        )
        .await
        {
            Ok(Ok(())) => Ok(()),
            Ok(Err(_)) | Err(_) => Err(BuildPipelineError::new(
                BuildFailureCode::CleanupFailed,
                false,
                false,
            )),
        }
    }
}

fn validate_digest(value: &str) -> Result<Sha256Digest, BuildPipelineError> {
    value
        .strip_prefix("sha256:")
        .ok_or_else(|| {
            BuildPipelineError::new(BuildFailureCode::BuildIdentityMismatch, false, true)
        })?
        .parse()
        .map_err(|_| BuildPipelineError::new(BuildFailureCode::BuildIdentityMismatch, false, true))
}

fn expected_course_repository_prefix(
    command: &AgentBuildRequested,
) -> Result<String, BuildPipelineError> {
    let mut parts = command.request.output_repository.split('/');
    let registry = parts.next().unwrap_or_default();
    let project = parts.next().unwrap_or_default();
    let repository = parts.next().unwrap_or_default();
    if registry.is_empty()
        || project.is_empty()
        || repository
            != format!(
                "course-{}-{}",
                command.request.course_id, command.request.candidate_id
            )
        || parts.next().is_some()
    {
        return Err(BuildPipelineError::new(
            BuildFailureCode::RegistryProjectInvalid,
            false,
            true,
        ));
    }
    Ok(format!("{registry}/{project}"))
}

fn add_milliseconds(
    timestamp: UtcTimestamp,
    milliseconds: u64,
) -> Result<UtcTimestamp, BuildPipelineError> {
    let milliseconds = i64::try_from(milliseconds)
        .map_err(|_| BuildPipelineError::new(BuildFailureCode::ClockInvalid, false, false))?;
    let value = timestamp
        .get()
        .checked_add(time::Duration::milliseconds(milliseconds))
        .ok_or_else(|| BuildPipelineError::new(BuildFailureCode::ClockInvalid, false, false))?;
    UtcTimestamp::from_utc(value)
        .map_err(|_| BuildPipelineError::new(BuildFailureCode::ClockInvalid, false, false))
}

/// Provider transport failure without raw backend text.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BuildProviderFailure {
    pub code: BuildProviderFailureCode,
    pub retryable: bool,
}

/// Closed provider failure family safe for metrics and persisted diagnostics.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BuildProviderFailureCode {
    Unavailable,
    Rejected,
    IdentityMismatch,
    OutputInvalid,
}

impl BuildProviderFailure {
    /// Returns the closed diagnostic exposed at the provider boundary.
    #[must_use]
    pub const fn diagnostic_code(self) -> &'static str {
        match self.code {
            BuildProviderFailureCode::Unavailable => "LW_AGENT_BUILD_PROVIDER_UNAVAILABLE",
            BuildProviderFailureCode::Rejected => "LW_AGENT_BUILD_REJECTED",
            BuildProviderFailureCode::IdentityMismatch => {
                "LW_AGENT_BUILD_PROVIDER_IDENTITY_MISMATCH"
            }
            BuildProviderFailureCode::OutputInvalid => "LW_AGENT_BUILD_PROVIDER_OUTPUT_INVALID",
        }
    }
}

/// Stable build failure used by Inbox retry and terminal event publication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BuildPipelineError {
    pub code: BuildFailureCode,
    pub retryable: bool,
    pub cleanup_verified: bool,
}

impl BuildPipelineError {
    const fn new(code: BuildFailureCode, retryable: bool, cleanup_verified: bool) -> Self {
        Self {
            code,
            retryable,
            cleanup_verified,
        }
    }

    #[must_use]
    pub const fn diagnostic_code(self) -> &'static str {
        self.code.diagnostic_code()
    }
}

impl std::fmt::Display for BuildPipelineError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.diagnostic_code())
    }
}

impl std::error::Error for BuildPipelineError {}

/// Stable failure code family for Issue #52.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BuildFailureCode {
    ConfigurationInvalid,
    CommandInvalid,
    ProviderBindingMismatch,
    RegistryProjectInvalid,
    TimedOut,
    Cancelled,
    Provider(BuildProviderFailureCode),
    BuildIdentityMismatch,
    PublicationIdentityMismatch,
    ArtifactInvalid,
    CleanupFailed,
    ClockInvalid,
}

impl BuildFailureCode {
    #[must_use]
    pub const fn diagnostic_code(self) -> &'static str {
        match self {
            Self::ConfigurationInvalid => "LW_AGENT_BUILD_CONFIGURATION_INVALID",
            Self::CommandInvalid => "LW_AGENT_BUILD_COMMAND_INVALID",
            Self::ProviderBindingMismatch => "LW_AGENT_BUILD_PROVIDER_BINDING_MISMATCH",
            Self::RegistryProjectInvalid => "LW_AGENT_BUILD_REGISTRY_PROJECT_INVALID",
            Self::TimedOut => "LW_AGENT_BUILD_TIMEOUT",
            Self::Cancelled => "LW_AGENT_BUILD_CANCELLED",
            Self::Provider(BuildProviderFailureCode::Unavailable) => {
                "LW_AGENT_BUILD_PROVIDER_UNAVAILABLE"
            }
            Self::Provider(BuildProviderFailureCode::Rejected) => "LW_AGENT_BUILD_REJECTED",
            Self::Provider(BuildProviderFailureCode::IdentityMismatch) => {
                "LW_AGENT_BUILD_PROVIDER_IDENTITY_MISMATCH"
            }
            Self::Provider(BuildProviderFailureCode::OutputInvalid) => {
                "LW_AGENT_BUILD_PROVIDER_OUTPUT_INVALID"
            }
            Self::BuildIdentityMismatch => "LW_AGENT_BUILD_IDENTITY_MISMATCH",
            Self::PublicationIdentityMismatch => "LW_AGENT_BUILD_PUBLICATION_IDENTITY_MISMATCH",
            Self::ArtifactInvalid => "LW_AGENT_BUILD_ARTIFACT_INVALID",
            Self::CleanupFailed => "LW_AGENT_BUILD_CLEANUP_FAILED",
            Self::ClockInvalid => "LW_AGENT_BUILD_CLOCK_INVALID",
        }
    }
}