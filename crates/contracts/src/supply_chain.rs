//! Build, scan, signing, immutable runtime artifact, and release contracts.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::authoring::{CandidateApproval, CandidateDecision, RuntimeKind};
use crate::{
    ActorId, ArtifactRef, BuildRequestId, CandidateId, CourseId, ImageArtifactId, PolicyId,
    ReleaseId, Revision, Sha256Digest, UtcTimestamp,
};

const PRIVATE_SIGSTORE_SCHEMA_VERSION: &str = "private-sigstore.v1";

/// Exact workload identity accepted by the private Fulcio deployment.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkloadIdentityPolicy {
    pub schema_version: String,
    pub issuer: String,
    pub audience: String,
    pub client_id: String,
    pub allowed_subjects: Vec<String>,
    pub required_claims: Vec<String>,
    pub certificate_identity_template: String,
    pub token_lifetime_milliseconds: u64,
    pub clock_skew_milliseconds: u64,
    pub replay_cache_ttl_milliseconds: u64,
}

impl WorkloadIdentityPolicy {
    /// Rejects wildcard, public, human-user, and incomplete workload bindings.
    pub fn validate(&self) -> Result<(), SupplyChainError> {
        let exact = |value: &str| {
            !value.trim().is_empty()
                && !value.contains('*')
                && !value.contains("{{")
                && !value.contains("REPLACE_")
        };
        if self.schema_version != PRIVATE_SIGSTORE_SCHEMA_VERSION
            || !exact(&self.issuer)
            || !self.issuer.starts_with("https://")
            || self.issuer.contains("sigstore.dev")
            || !exact(&self.audience)
            || !exact(&self.client_id)
            || self.audience != self.client_id
            || self.allowed_subjects.len() != 1
            || self.allowed_subjects.iter().any(|subject| {
                !exact(subject) || subject.starts_with("user:") || subject.contains("email")
            })
            || ["iss", "aud", "sub", "azp"].iter().any(|required| {
                self.required_claims
                    .iter()
                    .filter(|claim| claim.as_str() == *required)
                    .count()
                    != 1
            })
            || self.required_claims.iter().any(|claim| !exact(claim))
            || !exact(&self.certificate_identity_template)
            || self.certificate_identity_template != self.allowed_subjects[0]
            || self.token_lifetime_milliseconds == 0
            || self.token_lifetime_milliseconds > 600_000
            || self.clock_skew_milliseconds > 60_000
            || self.replay_cache_ttl_milliseconds < self.token_lifetime_milliseconds
            || self.clock_skew_milliseconds >= self.token_lifetime_milliseconds
        {
            return Err(SupplyChainError::WorkloadIdentityInvalid);
        }
        Ok(())
    }
}

/// Public TUF metadata identity; private root key material is never represented.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TufRootIdentity {
    pub version: u64,
    pub root_sha256: Sha256Digest,
    pub targets_version: u64,
    pub snapshot_version: u64,
    pub timestamp_version: u64,
    pub expires_at: UtcTimestamp,
    pub compatibility_window_ends_at: UtcTimestamp,
    pub rotation_state: String,
}

impl TufRootIdentity {
    pub fn validate(&self, observed_at: UtcTimestamp) -> Result<(), SupplyChainError> {
        if self.version == 0
            || self.targets_version == 0
            || self.snapshot_version == 0
            || self.timestamp_version == 0
            || self.expires_at <= observed_at
            || self.compatibility_window_ends_at > self.expires_at
            || !matches!(
                self.rotation_state.as_str(),
                "stable" | "rotating" | "recovery"
            )
        {
            return Err(SupplyChainError::TufMetadataInvalid);
        }
        Ok(())
    }

    pub fn validate_successor(&self, previous: &Self) -> Result<(), SupplyChainError> {
        if self.version <= previous.version
            || self.targets_version < previous.targets_version
            || self.snapshot_version < previous.snapshot_version
            || self.timestamp_version < previous.timestamp_version
        {
            return Err(SupplyChainError::TufRollbackDetected);
        }
        Ok(())
    }
}

/// Versioned identity shared by Cosign, Kyverno, and Release Gate consumers.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PrivateSigstoreTrustBundle {
    pub schema_version: String,
    pub bundle_version: u64,
    pub generated_at: UtcTimestamp,
    pub expires_at: UtcTimestamp,
    pub run_id: String,
    pub commit_sha: String,
    pub cluster_uid: String,
    pub inventory_sha256: Sha256Digest,
    pub deployment_manifest_sha256: Sha256Digest,
    pub component_lock_sha256: Sha256Digest,
    pub trust_bundle_sha256: Sha256Digest,
    pub fulcio_root_sha256: Sha256Digest,
    pub fulcio_intermediate_sha256: Sha256Digest,
    pub fulcio_issuer: String,
    pub audience: String,
    pub allowed_subjects: Vec<String>,
    pub rekor_public_key_sha256: Sha256Digest,
    pub rekor_log_id: String,
    pub ct_public_key_sha256: Sha256Digest,
    pub ct_log_id: String,
    pub tuf: TufRootIdentity,
}

impl PrivateSigstoreTrustBundle {
    pub fn validate(
        &self,
        observed_at: UtcTimestamp,
        expected_bundle_sha256: Sha256Digest,
    ) -> Result<(), SupplyChainError> {
        if self.schema_version != PRIVATE_SIGSTORE_SCHEMA_VERSION
            || self.bundle_version == 0
            || self.generated_at >= self.expires_at
            || self.expires_at <= observed_at
            || self.run_id.trim().is_empty()
            || !is_hex_sha(&self.commit_sha, 40, 64)
            || self.cluster_uid.trim().is_empty()
            || self.trust_bundle_sha256 != expected_bundle_sha256
            || self.fulcio_issuer.trim().is_empty()
            || !self.fulcio_issuer.starts_with("https://")
            || self.fulcio_issuer.contains("sigstore.dev")
            || self.audience.trim().is_empty()
            || self.allowed_subjects.is_empty()
            || self.allowed_subjects.iter().any(|subject| {
                subject.trim().is_empty() || subject.contains('*') || subject.starts_with("user:")
            })
            || self.rekor_log_id.trim().is_empty()
            || self.ct_log_id.trim().is_empty()
        {
            return Err(SupplyChainError::TrustBundleInvalid);
        }
        self.tuf.validate(observed_at)
    }

    pub fn verify_evidence(&self, evidence: &SigstoreEvidence) -> Result<(), SupplyChainError> {
        if evidence.trust_bundle_sha256 != self.trust_bundle_sha256
            || evidence.fulcio_issuer != self.fulcio_issuer
            || !self
                .allowed_subjects
                .contains(&evidence.certificate_subject)
            || evidence.rekor_log_id != self.rekor_log_id
            || evidence.ct_log_id != self.ct_log_id
            || evidence.rekor_inclusion_proof_sha256 == Sha256Digest::of_bytes(&[])
            || evidence.sct_sha256 == Sha256Digest::of_bytes(&[])
            || evidence.verified_at < self.generated_at
            || evidence.verified_at >= self.expires_at
        {
            return Err(SupplyChainError::SignatureInvalid);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TestFlightStatus {
    Passed,
    Failed,
    Blocked,
    NotRun,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TestFlightCheck {
    pub name: String,
    pub status: TestFlightStatus,
    pub diagnostic_code: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct PrivateSigstoreBackupIdentity {
    pub schema_version: String,
    pub run_id: String,
    pub commit_sha: String,
    pub cluster_uid: String,
    pub inventory_sha256: Sha256Digest,
    pub deployment_manifest_sha256: Sha256Digest,
    pub component_lock_sha256: Sha256Digest,
    pub tuf_root_sha256: Sha256Digest,
    pub trust_bundle_sha256: Sha256Digest,
    pub artifact_sha256: Sha256Digest,
    pub generated_at: UtcTimestamp,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PrivateSigstoreLifecycleAction {
    Backup,
    Restore,
    Rotate,
    Verify,
    Cleanup,
    DisasterRecovery,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct PrivateSigstoreLifecycleReport {
    pub schema_version: String,
    pub action: PrivateSigstoreLifecycleAction,
    pub status: TestFlightStatus,
    pub run_id: String,
    pub commit_sha: String,
    pub controller_id: String,
    pub cluster_uid: String,
    pub inventory_sha256: Sha256Digest,
    pub deployment_manifest_sha256: Sha256Digest,
    pub component_lock_sha256: Sha256Digest,
    pub chart_archive_sha256: Sha256Digest,
    pub image_digests: BTreeMap<String, Sha256Digest>,
    pub trust_bundle_sha256: Sha256Digest,
    pub tuf_root_version: u64,
    pub tuf_root_sha256: Sha256Digest,
    pub workload_identity_policy_sha256: Sha256Digest,
    pub backup: Option<PrivateSigstoreBackupIdentity>,
    pub checks: Vec<TestFlightCheck>,
    pub blocked_items: Vec<String>,
    pub unblock_owner: Option<String>,
    pub exit_condition: Option<String>,
    pub generated_at: UtcTimestamp,
}

impl PrivateSigstoreLifecycleReport {
    pub fn validate(&self) -> Result<(), SupplyChainError> {
        let all_passed = !self.checks.is_empty()
            && self
                .checks
                .iter()
                .all(|check| check.status == TestFlightStatus::Passed);
        let backup_required = matches!(
            self.action,
            PrivateSigstoreLifecycleAction::Restore
                | PrivateSigstoreLifecycleAction::Rotate
                | PrivateSigstoreLifecycleAction::DisasterRecovery
        );
        if self.schema_version != "private-sigstore-lifecycle-report.v1"
            || self.run_id.trim().is_empty()
            || !is_hex_sha(&self.commit_sha, 40, 64)
            || self.controller_id.trim().is_empty()
            || self.cluster_uid.trim().is_empty()
            || self.image_digests.is_empty()
            || self.tuf_root_version == 0
            || self.checks.iter().any(|check| {
                check.name.trim().is_empty()
                    || (check.status != TestFlightStatus::Passed
                        && check.diagnostic_code.as_deref().is_none_or(str::is_empty))
            })
            || (self.status == TestFlightStatus::Passed) != all_passed
            || (backup_required
                && self.backup.as_ref().is_none_or(|backup| {
                    backup.schema_version != "private-sigstore-backup.v1"
                        || backup.run_id != self.run_id
                        || backup.commit_sha != self.commit_sha
                        || backup.cluster_uid != self.cluster_uid
                        || backup.inventory_sha256 != self.inventory_sha256
                        || backup.deployment_manifest_sha256 != self.deployment_manifest_sha256
                        || backup.component_lock_sha256 != self.component_lock_sha256
                }))
            || (self.status == TestFlightStatus::Blocked
                && (self.blocked_items.is_empty()
                    || self.unblock_owner.as_deref().is_none_or(str::is_empty)
                    || self.exit_condition.as_deref().is_none_or(str::is_empty)))
        {
            return Err(SupplyChainError::TestFlightReportInvalid);
        }
        Ok(())
    }
}

impl PrivateSigstoreBackupIdentity {
    fn matches_report(&self, report: &PrivateSigstoreTestFlightReport) -> bool {
        self.schema_version == "private-sigstore-backup.v1"
            && self.run_id == report.run_id
            && self.commit_sha == report.commit_sha
            && self.cluster_uid == report.cluster_uid
            && self.inventory_sha256 == report.inventory_sha256
            && self.deployment_manifest_sha256 == report.deployment_manifest_sha256
            && self.component_lock_sha256 == report.component_lock_sha256
            && self.trust_bundle_sha256 == report.trust_bundle_sha256
            && self.artifact_sha256 != Sha256Digest::of_bytes(&[])
            && self.generated_at <= report.generated_at
    }
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PrivateSigstoreTestFlightReport {
    pub schema_version: String,
    pub scope: String,
    pub status: TestFlightStatus,
    pub run_id: String,
    pub commit_sha: String,
    pub cluster_uid: String,
    pub inventory_sha256: Sha256Digest,
    pub deployment_manifest_sha256: Sha256Digest,
    pub component_lock_sha256: Sha256Digest,
    pub trust_bundle_sha256: Sha256Digest,
    pub workload_identity_policy_sha256: Sha256Digest,
    pub backup: Option<PrivateSigstoreBackupIdentity>,
    pub checks: Vec<TestFlightCheck>,
    pub cleanup_status: TestFlightStatus,
    pub blocked_items: Vec<String>,
    pub unblock_owner: Option<String>,
    pub exit_condition: Option<String>,
    pub generated_at: UtcTimestamp,
}

impl PrivateSigstoreTestFlightReport {
    pub fn validate(&self) -> Result<(), SupplyChainError> {
        let required = [
            "identity",
            "schema",
            "component_lock",
            "chart_identity",
            "image_identity",
            "backup",
            "deploy",
            "second_deploy",
            "sign_verify",
            "restore",
            "rotation",
            "disaster_recovery",
            "cleanup",
            "outage_fail_closed",
            "tls",
            "network_policy",
            "oidc",
            "sct",
            "rekor_inclusion",
            "tuf_root",
            "trust_bundle",
        ];
        if self.schema_version != "private-sigstore-testflight.v1"
            || self.scope != "private-sigstore"
            || self.run_id.trim().is_empty()
            || !is_hex_sha(&self.commit_sha, 40, 64)
            || self.cluster_uid.trim().is_empty()
            || required.iter().any(|required_name| {
                self.checks
                    .iter()
                    .filter(|check| check.name == *required_name)
                    .count()
                    != 1
            })
            || self.checks.iter().any(|check| {
                check.name.trim().is_empty()
                    || (check.status != TestFlightStatus::Passed
                        && check.diagnostic_code.as_deref().is_none_or(str::is_empty))
            })
        {
            return Err(SupplyChainError::TestFlightReportInvalid);
        }
        let all_passed = self
            .checks
            .iter()
            .all(|check| check.status == TestFlightStatus::Passed)
            && self.cleanup_status == TestFlightStatus::Passed;
        if self
            .backup
            .as_ref()
            .is_some_and(|backup| !backup.matches_report(self))
            || (all_passed && self.backup.is_none())
        {
            return Err(SupplyChainError::TestFlightReportInvalid);
        }
        if (self.status == TestFlightStatus::Passed) != all_passed {
            return Err(SupplyChainError::TestFlightReportInvalid);
        }
        if self.status == TestFlightStatus::Blocked
            && (self.blocked_items.is_empty()
                || self.unblock_owner.as_deref().is_none_or(str::is_empty)
                || self.exit_condition.as_deref().is_none_or(str::is_empty))
        {
            return Err(SupplyChainError::TestFlightReportInvalid);
        }
        Ok(())
    }
}

fn is_hex_sha(value: &str, min: usize, max: usize) -> bool {
    (min..=max).contains(&value.len()) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

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
    #[error("private Sigstore workload identity is incomplete, wildcarded, or public")]
    WorkloadIdentityInvalid,
    #[error("private Sigstore trust bundle identity is stale, public, or mismatched")]
    TrustBundleInvalid,
    #[error("TUF metadata is expired, incomplete, or outside the compatibility window")]
    TufMetadataInvalid,
    #[error("TUF metadata version rollback was detected")]
    TufRollbackDetected,
    #[error("private Sigstore TestFlight report is incomplete or reports false success")]
    TestFlightReportInvalid,
}
