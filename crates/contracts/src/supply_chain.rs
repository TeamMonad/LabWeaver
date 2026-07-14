//! Build, scan, signing, immutable runtime artifact, and release contracts.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::authoring::{CandidateApproval, CandidateDecision, RuntimeKind};
use crate::{
    ActorId, ArtifactRef, BuildRequestId, CandidateId, CourseId, ImageArtifactId, PolicyId,
    ReleaseId, Revision, Sha256Digest, UtcTimestamp,
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
        {
            return Err(SupplyChainError::IncompleteBuildRequest);
        }
        crate::validate_relative_path(&self.dockerfile_path)
            .map_err(|_| SupplyChainError::IncompleteBuildRequest)?;
        validate_oci_digest(&self.base_image_digest)?;
        if let BuildNetworkPolicy::Restricted { allowed_registries } = &self.network {
            if allowed_registries.is_empty()
                || allowed_registries
                    .iter()
                    .any(|registry| registry.trim().is_empty())
            {
                return Err(SupplyChainError::IncompleteBuildRequest);
            }
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

/// Private Sigstore identity and transparency evidence.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SigstoreEvidence {
    pub trust_bundle_sha256: Sha256Digest,
    pub fulcio_issuer: String,
    pub certificate_subject: String,
    pub certificate_sha256: Sha256Digest,
    pub signature_sha256: Sha256Digest,
    pub rekor_log_id: String,
    pub rekor_log_index: u64,
    pub rekor_inclusion_proof_sha256: Sha256Digest,
    pub ct_log_id: String,
    pub sct_sha256: Sha256Digest,
    pub verified_at: UtcTimestamp,
}

/// Complete immutable runtime artifact identity.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ImageArtifact {
    Container {
        id: ImageArtifactId,
        build_request_id: BuildRequestId,
        repository: String,
        immutable_tag: String,
        digest: String,
        sbom: ArtifactRef,
        provenance: ArtifactRef,
        signature: SigstoreEvidence,
    },
    VirtualMachine {
        id: ImageArtifactId,
        base_disk: ArtifactRef,
        format: VirtualMachineDiskFormat,
        sbom: ArtifactRef,
        provenance: ArtifactRef,
        signature: SigstoreEvidence,
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

    /// Returns the immutable content identity used by a Release.
    pub fn content_sha256(&self) -> Result<Sha256Digest, SupplyChainError> {
        match self {
            Self::Container { digest, .. } => digest
                .strip_prefix("sha256:")
                .ok_or(SupplyChainError::DigestMismatch)?
                .parse()
                .map_err(|_| SupplyChainError::DigestMismatch),
            Self::VirtualMachine { base_disk, .. } => Ok(base_disk.sha256),
        }
    }

    #[must_use]
    pub const fn signature_evidence(&self) -> &SigstoreEvidence {
        match self {
            Self::Container { signature, .. } | Self::VirtualMachine { signature, .. } => signature,
        }
    }

    /// Validates required SBOM, provenance, digest, and transparency proof identity.
    pub fn validate(&self) -> Result<(), SupplyChainError> {
        let (sbom, provenance, signature) = match self {
            Self::Container {
                repository,
                immutable_tag,
                digest,
                sbom,
                provenance,
                signature,
                ..
            } => {
                if repository.trim().is_empty() || immutable_tag.trim().is_empty() {
                    return Err(SupplyChainError::IncompleteArtifact);
                }
                validate_oci_digest(digest)?;
                (sbom, provenance, signature)
            }
            Self::VirtualMachine {
                base_disk,
                sbom,
                provenance,
                signature,
                ..
            } => {
                if base_disk.size_bytes == 0 || base_disk.object_version.trim().is_empty() {
                    return Err(SupplyChainError::IncompleteArtifact);
                }
                (sbom, provenance, signature)
            }
        };
        if sbom.size_bytes == 0
            || provenance.size_bytes == 0
            || signature.fulcio_issuer.trim().is_empty()
            || signature.certificate_subject.trim().is_empty()
            || signature.rekor_log_id.trim().is_empty()
            || signature.ct_log_id.trim().is_empty()
        {
            return Err(SupplyChainError::IncompleteArtifact);
        }
        Ok(())
    }
}

/// Supported VM base-disk encodings.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VirtualMachineDiskFormat {
    Qcow2,
    Raw,
}

/// Deterministic scan and trust-policy evaluation.
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
    pub trust_bundle_sha256: Sha256Digest,
    pub expected_fulcio_issuer: String,
    pub expected_certificate_subject: String,
    pub require_rekor_inclusion: bool,
    pub require_ct_sct: bool,
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
            || self.max_evidence_age_milliseconds == 0
            || self.valid_until <= self.evaluated_at
            || self.expected_fulcio_issuer.trim().is_empty()
            || self.expected_certificate_subject.trim().is_empty()
            || !self.require_rekor_inclusion
            || !self.require_ct_sct
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
    pub candidate_revision: Revision,
    pub environment_spec_sha256: Sha256Digest,
    pub runtime_kind: RuntimeKind,
    pub approval: CandidateApproval,
    pub artifact: ImageArtifact,
    pub image_policy_evaluation: ImagePolicyEvaluation,
    pub published_by: ActorId,
    pub published_at: UtcTimestamp,
}

impl EnvironmentTemplateRelease {
    /// Validates exact approval, runtime artifact, scan, and trust bindings.
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
        self.image_policy_evaluation.validate()?;
        if self.image_policy_evaluation.artifact_sha256 != self.artifact.content_sha256()? {
            return Err(SupplyChainError::DigestMismatch);
        }
        let signature = self.artifact.signature_evidence();
        if self.image_policy_evaluation.trust_bundle_sha256 != signature.trust_bundle_sha256
            || self.image_policy_evaluation.expected_fulcio_issuer != signature.fulcio_issuer
            || self.image_policy_evaluation.expected_certificate_subject
                != signature.certificate_subject
            || signature.rekor_inclusion_proof_sha256 == Sha256Digest::of_bytes(&[])
            || signature.sct_sha256 == Sha256Digest::of_bytes(&[])
        {
            return Err(SupplyChainError::SignatureInvalid);
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
    #[error("runtime artifact is missing digest, SBOM, provenance, or signature evidence")]
    IncompleteArtifact,
    #[error("runtime artifact digest does not match the evaluated identity")]
    DigestMismatch,
    #[error("Critical vulnerabilities block publication")]
    CriticalVulnerability,
    #[error("scan or trust evidence is missing or stale")]
    StaleEvidence,
    #[error("signature identity or transparency evidence does not match policy")]
    SignatureInvalid,
    #[error("candidate approval does not bind this exact release")]
    ApprovalMismatch,
}
