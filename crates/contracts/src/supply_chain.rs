//! Build, scan, immutable runtime artifact, and release contracts.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::authoring::{CandidateApproval, CandidateDecision, EnvironmentCandidate, RuntimeKind};
use crate::submission::SubmissionManifest;
use crate::{
    ActorId, AgentRunId, ArtifactRef, BuildRequestId, CandidateId, CourseId, ImageArtifactId,
    ProjectId, ReleaseId, Revision, UtcTimestamp,
};

/// Explicit BuildKit network posture.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum BuildNetworkPolicy {
    DenyAll,
    Restricted { allowed_registries: Vec<String> },
}

/// Approved, immutable build input source.
///
/// `Dockerfile` is the deployment-owned `BuildKit` path. `ExportedOci` names an OCI
/// layout/tar archive produced by an admitted sandbox attempt: the build executor
/// verifies every entry and content digest before publishing the exact image by
/// digest, so the sandbox itself never holds registry push credentials.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum BuildSource {
    Dockerfile {
        context: ArtifactRef,
        /// Object-store key resolved by Control and bound to the immutable context reference.
        context_object_key: String,
        dockerfile_path: String,
    },
    ExportedOci {
        image: ExportedOciImage,
    },
}

/// Frozen sandbox-exported OCI layout archive.
///
/// The agent freezes and hashes the exact uploaded version before Control may bind it into a
/// build request, so the executor re-reads that immutable version instead of a current key.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExportedOciImage {
    /// Immutable object-store identity of the layout archive.
    pub layout: ArtifactRef,
    /// Object-store key of the layout archive.
    pub layout_object_key: String,
}

impl BuildSource {
    /// Validates the source-specific immutable inputs.
    fn validate(&self, network: &BuildNetworkPolicy) -> Result<(), SupplyChainError> {
        match self {
            Self::Dockerfile {
                context,
                context_object_key,
                dockerfile_path,
            } => {
                if context.size_bytes == 0
                    || crate::validate_relative_path(context_object_key).is_err()
                    || crate::validate_relative_path(dockerfile_path).is_err()
                {
                    return Err(SupplyChainError::IncompleteBuildRequest);
                }
                if let BuildNetworkPolicy::Restricted { allowed_registries } = network
                    && (allowed_registries.is_empty()
                        || allowed_registries
                            .iter()
                            .any(|registry| registry.trim().is_empty()))
                {
                    return Err(SupplyChainError::IncompleteBuildRequest);
                }
                Ok(())
            }
            Self::ExportedOci { image } => {
                image.validate()?;
                if !matches!(network, BuildNetworkPolicy::DenyAll) {
                    return Err(SupplyChainError::IncompleteBuildRequest);
                }
                Ok(())
            }
        }
    }
}

/// Approved, immutable build request.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BuildRequest {
    pub id: BuildRequestId,
    pub project_id: ProjectId,
    pub course_id: Option<CourseId>,
    pub candidate_id: CandidateId,
    pub candidate_revision: Revision,
    pub builder_binding: String,
    pub source: BuildSource,
    pub output_repository: String,
    pub network: BuildNetworkPolicy,
    pub max_duration_milliseconds: u64,
    pub max_cpu_millicores: u32,
    pub max_memory_bytes: u64,
    pub created_at: UtcTimestamp,
}

impl BuildRequest {
    /// Validates explicit immutable inputs and bounded execution.
    pub fn validate(&self) -> Result<(), SupplyChainError> {
        if self.builder_binding.trim().is_empty()
            || self.output_repository.trim().is_empty()
            || self.max_duration_milliseconds == 0
            || self.max_cpu_millicores == 0
            || self.max_memory_bytes == 0
        {
            return Err(SupplyChainError::IncompleteBuildRequest);
        }
        self.source.validate(&self.network)
    }
}

/// Deployment-owned immutable KubeVirt base-disk identity.
///
/// Unlike an object-store `ArtifactRef`, this identifies a CDI source image and its imported
/// disk content. `capacity_bytes` is the reviewed PVC capacity, not a fabricated object length.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VirtualMachineBaseDisk {
    pub binding: String,
    pub source_registry_digest: String,
    pub capacity_bytes: u64,
}

impl VirtualMachineBaseDisk {
    /// Validates a fixed CDI binding and immutable OCI source digest.
    pub fn validate(&self) -> Result<(), SupplyChainError> {
        let Some(source) = self.source_registry_digest.strip_prefix("docker://") else {
            return Err(SupplyChainError::IncompleteArtifact);
        };
        let Some((repository, digest)) = source.rsplit_once('@') else {
            return Err(SupplyChainError::DigestMismatch);
        };
        if self.binding.trim().is_empty()
            || self.binding.bytes().any(|byte| byte.is_ascii_whitespace())
            || repository.trim().is_empty()
            || repository.contains(char::is_whitespace)
            || self.capacity_bytes == 0
        {
            return Err(SupplyChainError::IncompleteArtifact);
        }
        validate_oci_digest(digest)
    }
}

/// Complete immutable runtime artifact identity.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ImageArtifact {
    Container {
        id: ImageArtifactId,
        build_request_id: BuildRequestId,
        repository: String,
        digest: String,
    },
    VirtualMachine {
        id: ImageArtifactId,
        base_disk: VirtualMachineBaseDisk,
        format: VirtualMachineDiskFormat,
    },
    /// Administrator-imported container, not an Agent build.
    CatalogContainer {
        id: ImageArtifactId,
        catalog_id: crate::PlatformImageId,
        repin_generation: u64,
        trust_revision: Revision,
        repository: String,
        digest: String,
    },
}

impl ImageArtifact {
    /// Returns the bound runtime kind.
    #[must_use]
    pub const fn runtime_kind(&self) -> RuntimeKind {
        match self {
            Self::Container { .. } | Self::CatalogContainer { .. } => RuntimeKind::Container,
            Self::VirtualMachine { .. } => RuntimeKind::VirtualMachine,
        }
    }

    /// Returns the artifact identifier.
    #[must_use]
    pub const fn id(&self) -> ImageArtifactId {
        match self {
            Self::Container { id, .. }
            | Self::VirtualMachine { id, .. }
            | Self::CatalogContainer { id, .. } => *id,
        }
    }

    /// Immutable registry reference for either built or imported containers.
    #[must_use]
    pub fn container_reference(&self) -> Option<(&str, &str)> {
        match self {
            Self::Container {
                repository, digest, ..
            }
            | Self::CatalogContainer {
                repository, digest, ..
            } => Some((repository, digest)),
            Self::VirtualMachine { .. } => None,
        }
    }

    /// Validates the private registry and immutable content identity.
    pub fn validate(&self) -> Result<(), SupplyChainError> {
        match self {
            Self::Container {
                repository, digest, ..
            }
            | Self::CatalogContainer {
                repository, digest, ..
            } => {
                if repository.trim().is_empty()
                    || repository.starts_with("http://")
                    || repository.contains('@')
                    || repository.contains(char::is_whitespace)
                {
                    return Err(SupplyChainError::IncompleteArtifact);
                }
                validate_oci_digest(digest)
            }
            Self::VirtualMachine { base_disk, .. } => base_disk.validate(),
        }
    }
}

/// Supported VM base-disk encodings.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VirtualMachineDiskFormat {
    Qcow2,
    Raw,
}

/// Human-reviewed Work configuration. No model, Agent run, or LLM policy is fabricated.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManualTemplateApproval {
    pub template_family_id: String,
    pub description: String,
    pub platform_image: crate::http::PlatformImageEntry,
    pub spec: crate::authoring::EnvironmentSpec,
    pub reason: String,
    pub approved_by: ActorId,
    pub approved_at: UtcTimestamp,
    pub trust_revision: Revision,
}

impl ManualTemplateApproval {
    /// Verifies the immutable catalog snapshot and configuration, without any model evidence.
    pub fn validate_artifact(&self, artifact: &ImageArtifact) -> Result<(), SupplyChainError> {
        use crate::authoring::EnvironmentRuntimeSpec;
        use crate::http::{PlatformImageKind, PlatformImageStatus};
        self.spec
            .validate()
            .map_err(|_| SupplyChainError::ApprovalMismatch)?;
        let image = &self.platform_image;
        if image.status != PlatformImageStatus::Active
            || image.trust_revision != self.trust_revision.get()
            || artifact.id().as_uuid() != image.catalog_id.as_uuid()
        {
            return Err(SupplyChainError::ApprovalMismatch);
        }
        match (artifact, &self.spec.runtime) {
            (
                ImageArtifact::CatalogContainer {
                    catalog_id,
                    repin_generation,
                    trust_revision,
                    repository,
                    digest,
                    ..
                },
                EnvironmentRuntimeSpec::Container {
                    build_context: None,
                    ..
                },
            ) => {
                let reference = image.source_reference.split('@').next().unwrap_or_default();
                let image_repository = reference
                    .rsplit_once(':')
                    .filter(|(_, tag)| !tag.contains('/'))
                    .map_or(reference, |(repository, _)| repository);
                if image.kind != PlatformImageKind::Container
                    || *catalog_id != image.catalog_id
                    || *repin_generation != image.repin_generation
                    || *trust_revision != self.trust_revision
                    || repository != image_repository
                    || digest != &image.resolved_digest
                {
                    return Err(SupplyChainError::ApprovalMismatch);
                }
            }
            (
                ImageArtifact::VirtualMachine {
                    base_disk, format, ..
                },
                EnvironmentRuntimeSpec::VirtualMachine {
                    base_disk: declared,
                    ..
                },
            ) => {
                let source = base_disk
                    .source_registry_digest
                    .strip_prefix("docker://")
                    .and_then(|source| source.rsplit_once('@'))
                    .ok_or(SupplyChainError::ApprovalMismatch)?;
                let reference = image.source_reference.split('@').next().unwrap_or_default();
                let repository = reference
                    .rsplit_once(':')
                    .filter(|(_, tag)| !tag.contains('/'))
                    .map_or(reference, |(repository, _)| repository);
                if image.kind != PlatformImageKind::VirtualMachine
                    || base_disk != declared
                    || base_disk.binding != image.binding
                    || image.format != Some(*format)
                    || image.capacity_bytes != Some(base_disk.capacity_bytes)
                    || image.disk_sha256.is_none()
                    || source != (repository, image.resolved_digest.as_str())
                {
                    return Err(SupplyChainError::ApprovalMismatch);
                }
            }
            _ => return Err(SupplyChainError::ApprovalMismatch),
        }
        Ok(())
    }
}

/// Immutable environment-first release.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentTemplateRelease {
    pub id: ReleaseId,
    pub project_id: ProjectId,
    pub course_id: Option<CourseId>,
    pub version: u64,
    pub candidate_id: Option<CandidateId>,
    pub agent_run_id: Option<AgentRunId>,
    pub candidate_revision: Option<Revision>,
    pub runtime_kind: RuntimeKind,
    pub approval: Option<CandidateApproval>,
    pub manual_approval: Option<Box<ManualTemplateApproval>>,
    pub artifact: ImageArtifact,
    pub published_by: ActorId,
    pub published_at: UtcTimestamp,
}

impl EnvironmentTemplateRelease {
    /// Validates exact approval and runtime artifact binding.
    pub fn validate(&self) -> Result<(), SupplyChainError> {
        if self.version == 0 || self.runtime_kind != self.artifact.runtime_kind() {
            return Err(SupplyChainError::ApprovalMismatch);
        }
        match (&self.approval, &self.manual_approval) {
            (Some(approval), None)
                if approval.decision == CandidateDecision::Approved
                    && Some(approval.candidate_id) == self.candidate_id
                    && Some(approval.candidate_revision) == self.candidate_revision
                    && self.agent_run_id.is_some()
                    && !matches!(self.artifact, ImageArtifact::CatalogContainer { .. }) => {}
            (None, Some(manual))
                if self.candidate_id.is_none()
                    && self.candidate_revision.is_none()
                    && self.agent_run_id.is_none()
                    && manual.approved_by == self.published_by
                    && !manual.reason.trim().is_empty()
                    && !manual.template_family_id.trim().is_empty()
                    && manual.spec.class == crate::authoring::EnvironmentClass::Work
                    && manual.spec.runtime.kind() == self.runtime_kind =>
            {
                manual.validate_artifact(&self.artifact)?;
            }
            _ => return Err(SupplyChainError::ApprovalMismatch),
        }
        self.artifact.validate()?;
        Ok(())
    }

    /// Verifies that a release is published into the exact project context supplied by Control.
    pub fn validate_ownership(
        &self,
        project_id: ProjectId,
        course_id: Option<CourseId>,
    ) -> Result<(), SupplyChainError> {
        if self.project_id != project_id || self.course_id != course_id {
            return Err(SupplyChainError::OwnershipMismatch);
        }
        Ok(())
    }

    /// Verifies the release and candidate share the same immutable identity and project context.
    pub fn validate_against_candidate(
        &self,
        candidate: &EnvironmentCandidate,
    ) -> Result<(), SupplyChainError> {
        if self.project_id != candidate.project_id
            || self.course_id != candidate.course_id
            || self.candidate_id != Some(candidate.id)
            || self.agent_run_id != Some(candidate.run_id)
            || self.candidate_revision != Some(candidate.revision)
        {
            return Err(SupplyChainError::OwnershipMismatch);
        }
        Ok(())
    }

    /// Trust version bound by the single human approval, regardless of its input source.
    pub fn trust_revision(&self) -> Result<Revision, SupplyChainError> {
        match (&self.approval, &self.manual_approval) {
            (Some(approval), None) => Ok(approval.trust_revision),
            (None, Some(manual)) => Ok(manual.trust_revision),
            _ => Err(SupplyChainError::ApprovalMismatch),
        }
    }

    /// Returns the single bound runtime.
    #[must_use]
    pub const fn runtime_kind(&self) -> RuntimeKind {
        self.runtime_kind
    }
}

/// Append-only release withdrawal fact.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReleaseWithdrawal {
    pub release_id: ReleaseId,
    pub release_version: u64,
    pub actor_id: ActorId,
    pub reason_code: String,
    pub withdrawn_at: UtcTimestamp,
}

/// Read model that keeps the immutable release separate from its append-only withdrawal fact.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentTemplateReleaseView {
    #[serde(flatten)]
    pub release: EnvironmentTemplateRelease,
    pub name: String,
    pub description: String,
    pub template_family_id: String,
    pub purpose: crate::authoring::EnvironmentClass,
    pub capabilities: Vec<TemplateCapability>,
    pub withdrawal: Option<ReleaseWithdrawal>,
    /// Workspace submission manifest from the exact ready authoring publication, when present.
    ///
    /// Work releases and system-facts evaluations intentionally omit this projection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub submission_manifest: Option<SubmissionManifest>,
}

/// Supported template features, not live connection or model availability promises.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TemplateCapability {
    Terminal,
    Desktop,
    Ssh,
    Applications,
    Files,
    Agent,
}

impl TemplateCapability {
    #[must_use]
    pub fn for_spec(spec: &crate::authoring::EnvironmentSpec) -> Vec<Self> {
        use crate::authoring::EnvironmentRuntimeSpec;
        let mut capabilities = Vec::with_capacity(6);
        match &spec.runtime {
            EnvironmentRuntimeSpec::Container { terminal, .. } => {
                if terminal.is_some() {
                    capabilities.push(Self::Terminal);
                }
                capabilities.push(Self::Files);
            }
            EnvironmentRuntimeSpec::VirtualMachine { .. } => capabilities.push(Self::Desktop),
        }
        if spec
            .entries
            .iter()
            .any(|entry| entry.protocol == crate::environment::EndpointProtocol::Ssh)
        {
            capabilities.push(Self::Ssh);
        }
        if spec.entries.iter().any(|entry| {
            matches!(
                entry.protocol,
                crate::environment::EndpointProtocol::Http
                    | crate::environment::EndpointProtocol::Https
            )
        }) {
            capabilities.push(Self::Applications);
        }
        capabilities.push(Self::Agent);
        capabilities
    }
}

fn validate_oci_digest(value: &str) -> Result<(), SupplyChainError> {
    let Some(digest) = value.strip_prefix("sha256:") else {
        return Err(SupplyChainError::DigestMismatch);
    };
    if digest.len() != 64
        || !digest.bytes().all(|byte| byte.is_ascii_hexdigit())
        || digest.bytes().any(|byte| byte.is_ascii_uppercase())
    {
        return Err(SupplyChainError::DigestMismatch);
    }
    Ok(())
}

impl ExportedOciImage {
    /// Validates the frozen layout identity and object key.
    pub fn validate(&self) -> Result<(), SupplyChainError> {
        if self.layout.size_bytes == 0
            || self.layout.object_version.trim().is_empty()
            || self.layout.store_binding.trim().is_empty()
            || crate::validate_relative_path(&self.layout_object_key).is_err()
        {
            return Err(SupplyChainError::IncompleteArtifact);
        }
        Ok(())
    }
}

/// Supply-chain contract failure.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum SupplyChainError {
    #[error("BuildRequest is missing an immutable input, binding, or execution limit")]
    IncompleteBuildRequest,
    #[error("runtime artifact is missing a private repository or immutable digest")]
    IncompleteArtifact,
    #[error("runtime artifact digest does not match the evaluated identity")]
    DigestMismatch,
    #[error("candidate approval does not bind this exact release")]
    ApprovalMismatch,
    #[error("project ownership context does not match")]
    OwnershipMismatch,
}
