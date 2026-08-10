//! Build, scan, immutable runtime artifact, and release contracts.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::authoring::{CandidateApproval, CandidateDecision, RuntimeKind};
use crate::{
    ActorId, AgentRunId, ArtifactRef, BuildRequestId, CandidateId, CourseId, ImageArtifactId,
    PolicyId, ReleaseId, Revision, Sha256Digest, UtcTimestamp,
};

/// Explicit BuildKit network posture.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum BuildNetworkPolicy {
    DenyAll,
    Restricted { allowed_registries: Vec<String> },
}

/// Approved, immutable build request.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BuildRequest {
    pub id: BuildRequestId,
    pub course_id: CourseId,
    pub candidate_id: CandidateId,
    pub candidate_revision: Revision,
    pub candidate_sha256: Sha256Digest,
    pub approval_id: crate::ApprovalId,
    pub builder_binding: String,
    pub context: ArtifactRef,
    /// Object-store key resolved by Control and bound to the immutable context reference.
    pub context_object_key: String,
    pub dockerfile_path: String,
    pub base_image_digest: String,
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
            || self.context.size_bytes == 0
            || crate::validate_relative_path(&self.context_object_key).is_err()
        {
            return Err(SupplyChainError::IncompleteBuildRequest);
        }
        crate::validate_relative_path(&self.dockerfile_path)
            .map_err(|_| SupplyChainError::IncompleteBuildRequest)?;
        validate_oci_digest(&self.base_image_digest)?;
        if let BuildNetworkPolicy::Restricted { allowed_registries } = &self.network
            && (allowed_registries.is_empty()
                || allowed_registries
                    .iter()
                    .any(|registry| registry.trim().is_empty()))
        {
            return Err(SupplyChainError::IncompleteBuildRequest);
        }
        Ok(())
    }
}

/// Vulnerability counts by severity.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VulnerabilitySummary {
    pub unknown: u32,
    pub low: u32,
    pub medium: u32,
    pub high: u32,
    pub critical: u32,
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
    pub disk_sha256: Sha256Digest,
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
}

impl ImageArtifact {
    /// Returns the bound runtime kind.
    #[must_use]
    pub const fn runtime_kind(&self) -> RuntimeKind {
        match self {
            Self::Container { .. } => RuntimeKind::Container,
            Self::VirtualMachine { .. } => RuntimeKind::VirtualMachine,
        }
    }

    /// Returns the artifact identifier.
    #[must_use]
    pub const fn id(&self) -> ImageArtifactId {
        match self {
            Self::Container { id, .. } | Self::VirtualMachine { id, .. } => *id,
        }
    }

    /// Returns the immutable content identity used by a Release.
    pub fn content_sha256(&self) -> Result<Sha256Digest, SupplyChainError> {
        match self {
            Self::Container { digest, .. } => digest
                .strip_prefix("sha256:")
                .ok_or(SupplyChainError::DigestMismatch)?
                .parse()
                .map_err(|_| SupplyChainError::DigestMismatch),
            Self::VirtualMachine { base_disk, .. } => Ok(base_disk.disk_sha256),
        }
    }

    /// Validates the private registry and immutable content identity.
    pub fn validate(&self) -> Result<(), SupplyChainError> {
        match self {
            Self::Container {
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

/// Deterministic digest-bound Trivy evaluation.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImagePolicyEvaluation {
    pub artifact_id: ImageArtifactId,
    pub artifact_sha256: Sha256Digest,
    pub policy_id: PolicyId,
    pub policy_revision: Revision,
    pub scanner_name: String,
    pub scanner_version: String,
    pub scanner_database_sha256: Sha256Digest,
    pub vulnerabilities: VulnerabilitySummary,
    pub evaluated_at: UtcTimestamp,
    pub max_evidence_age_milliseconds: u64,
    pub valid_until: UtcTimestamp,
    pub passed: bool,
}

impl ImagePolicyEvaluation {
    /// Validates fail-closed Critical policy and explicit evidence lifetime.
    pub fn validate(&self) -> Result<(), SupplyChainError> {
        let evidence_age = (self.valid_until.get() - self.evaluated_at.get()).whole_milliseconds();
        if self.scanner_name.trim().is_empty()
            || self.scanner_version.trim().is_empty()
            || self.scanner_database_sha256 == Sha256Digest::of_bytes(&[])
            || self.max_evidence_age_milliseconds == 0
            || self.valid_until <= self.evaluated_at
            || evidence_age <= 0
            || u128::try_from(evidence_age).ok()
                != Some(u128::from(self.max_evidence_age_milliseconds))
        {
            return Err(SupplyChainError::StaleEvidence);
        }
        if self.vulnerabilities.critical > 0 || !self.passed {
            return Err(SupplyChainError::CriticalVulnerability);
        }
        Ok(())
    }

    /// Returns the explicit non-blocking High-severity warning count.
    #[must_use]
    pub const fn high_severity_warning_count(&self) -> u32 {
        self.vulnerabilities.high
    }
}

/// Immutable environment-first release.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentTemplateRelease {
    pub id: ReleaseId,
    pub course_id: CourseId,
    pub version: u64,
    pub candidate_id: CandidateId,
    pub agent_run_id: AgentRunId,
    pub candidate_revision: Revision,
    pub environment_spec_sha256: Sha256Digest,
    pub runtime_kind: RuntimeKind,
    pub approval: CandidateApproval,
    pub artifact: ImageArtifact,
    /// Container-only Trivy evidence. VM releases bind a deployment-owned CDI base disk instead
    /// and must not fabricate vulnerability counts for an imported guest disk.
    pub image_policy_evaluation: Option<ImagePolicyEvaluation>,
    pub published_by: ActorId,
    pub published_at: UtcTimestamp,
}

impl EnvironmentTemplateRelease {
    /// Validates exact approval, runtime artifact, digest, and scan bindings.
    pub fn validate(&self) -> Result<(), SupplyChainError> {
        if self.version == 0
            || self.approval.decision != CandidateDecision::Approved
            || self.approval.candidate_id != self.candidate_id
            || self.approval.candidate_revision != self.candidate_revision
            || self.approval.candidate_sha256 != self.environment_spec_sha256
            || self.runtime_kind != self.artifact.runtime_kind()
        {
            return Err(SupplyChainError::ApprovalMismatch);
        }
        self.artifact.validate()?;
        match (&self.artifact, &self.image_policy_evaluation) {
            (ImageArtifact::Container { id, .. }, Some(evaluation)) => {
                evaluation.validate()?;
                if evaluation.artifact_id != *id
                    || evaluation.artifact_sha256 != self.artifact.content_sha256()?
                {
                    return Err(SupplyChainError::DigestMismatch);
                }
            }
            (ImageArtifact::VirtualMachine { .. }, None) => {}
            _ => return Err(SupplyChainError::ApprovalMismatch),
        }
        Ok(())
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
    pub withdrawal: Option<ReleaseWithdrawal>,
}

fn validate_oci_digest(value: &str) -> Result<(), SupplyChainError> {
    let Some(digest) = value.strip_prefix("sha256:") else {
        return Err(SupplyChainError::DigestMismatch);
    };
    digest
        .parse::<Sha256Digest>()
        .map_err(|_| SupplyChainError::DigestMismatch)?;
    Ok(())
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
    #[error("Critical vulnerabilities block publication")]
    CriticalVulnerability,
    #[error("scan evidence is missing or stale")]
    StaleEvidence,
    #[error("candidate approval does not bind this exact release")]
    ApprovalMismatch,
}
