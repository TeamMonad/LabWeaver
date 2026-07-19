//! Submission collection allowlists and immutable freeze identity.

use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};

use crate::authoring::RuntimeKind;
use crate::{
    ActorId, AgentRunId, ArtifactRef, BuildRequestId, CourseId, EnvironmentId, FrozenSubmissionId,
    PathRule, ReleaseId, RetentionSnapshot, Revision, Sha256Digest, UtcTimestamp,
};

/// Source available to a bounded Collector.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SubmissionSource {
    Workspace,
    SystemFacts,
}

/// Stable SubmissionManifest v1.
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SubmissionManifest {
    #[serde(rename = "apiVersion")]
    api_version: SubmissionApiVersion,
    kind: SubmissionDocumentKind,
    pub name: String,
    pub source: SubmissionSource,
    pub include: Vec<PathRule>,
    pub exclude: Vec<PathRule>,
    pub required: Vec<PathRule>,
    #[serde(rename = "llmReadable")]
    pub llm_readable: Vec<PathRule>,
    pub max_total_bytes: u64,
    pub max_files: u32,
    pub follow_symlinks: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SubmissionManifestWire {
    #[serde(rename = "apiVersion")]
    api_version: SubmissionApiVersion,
    kind: SubmissionDocumentKind,
    name: String,
    source: SubmissionSource,
    include: Vec<PathRule>,
    #[serde(default)]
    exclude: Vec<PathRule>,
    #[serde(default)]
    required: Vec<PathRule>,
    #[serde(rename = "llmReadable", default)]
    llm_readable: Vec<PathRule>,
    max_total_bytes: u64,
    max_files: u32,
    follow_symlinks: bool,
}

impl<'de> Deserialize<'de> for SubmissionManifest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = SubmissionManifestWire::deserialize(deserializer)?;
        let value = Self {
            api_version: wire.api_version,
            kind: wire.kind,
            name: wire.name,
            source: wire.source,
            include: wire.include,
            exclude: wire.exclude,
            required: wire.required,
            llm_readable: wire.llm_readable,
            max_total_bytes: wire.max_total_bytes,
            max_files: wire.max_files,
            follow_symlinks: wire.follow_symlinks,
        };
        value.validate().map_err(serde::de::Error::custom)?;
        Ok(value)
    }
}

impl SubmissionManifest {
    /// Validates portable path rules, limits, and LLM subset constraints.
    pub fn validate(&self) -> Result<(), SubmissionError> {
        if self.name.trim().is_empty()
            || self.include.is_empty()
            || self.max_total_bytes == 0
            || self.max_files == 0
            || self.follow_symlinks
            || self.max_total_bytes > 10 * 1024 * 1024 * 1024
            || self.max_files > 100_000
        {
            return Err(SubmissionError::InvalidManifest(
                "name, include, non-zero limits, and followSymlinks=false are required".to_owned(),
            ));
        }
        for rule in self
            .include
            .iter()
            .chain(&self.exclude)
            .chain(&self.required)
            .chain(&self.llm_readable)
        {
            rule.validate()
                .map_err(|error| SubmissionError::UnsafePath(error.to_string()))?;
        }
        reject_duplicates("include", &self.include)?;
        reject_duplicates("exclude", &self.exclude)?;
        reject_duplicates("required", &self.required)?;
        reject_duplicates("llmReadable", &self.llm_readable)?;
        reject_overlaps("include", &self.include)?;
        reject_overlaps("exclude", &self.exclude)?;
        reject_overlaps("required", &self.required)?;
        reject_overlaps("llmReadable", &self.llm_readable)?;

        for required in &self.required {
            if !self.include.iter().any(|include| covers(include, required))
                || self.exclude.iter().any(|exclude| covers(exclude, required))
            {
                return Err(SubmissionError::PathConflict(format!(
                    "required path is not collected: {}",
                    required.path()
                )));
            }
        }
        for allowed in &self.llm_readable {
            if !self.include.iter().any(|include| covers(include, allowed))
                || self.exclude.iter().any(|exclude| covers(exclude, allowed))
            {
                return Err(SubmissionError::LlmPathNotCollected(
                    allowed.path().to_owned(),
                ));
            }
        }
        if self.source == SubmissionSource::SystemFacts && !self.llm_readable.is_empty() {
            return Err(SubmissionError::LlmPathNotCollected(
                "system facts cannot be disclosed to the v1 LLM channel".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
enum SubmissionApiVersion {
    #[serde(rename = "evaluation.labweaver.io/v1")]
    V1,
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
enum SubmissionDocumentKind {
    SubmissionManifest,
}

/// One immutable frozen file.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FrozenFile {
    pub path: String,
    pub sha256: Sha256Digest,
    pub size_bytes: u64,
    pub media_type: String,
}

/// Frozen build and runtime identity used to reproduce collection.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FrozenEnvironmentIdentity {
    pub environment_id: EnvironmentId,
    pub environment_revision: Revision,
    pub release_id: ReleaseId,
    pub release_version: u64,
    pub runtime_kind: RuntimeKind,
    pub runtime_artifact_sha256: Sha256Digest,
    pub build_request_id: Option<BuildRequestId>,
}

/// Evaluation-authenticated request for one current Environment freeze binding.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentFreezeBindingRequest {
    pub course_id: CourseId,
    pub actor_id: ActorId,
    pub expected_revision: Revision,
    pub collector_public_key_openssh: Option<String>,
}

/// Environment-owned source locator safe for one bounded freeze Job.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum EnvironmentFreezeSourceBinding {
    Container {
        namespace: String,
        persistent_volume_claim: String,
        storage_class_name: String,
    },
    VirtualMachine {
        host: String,
        port: u16,
        username: String,
        workspace_root: String,
        expected_host_key_sha256: Sha256Digest,
        source_identity: Sha256Digest,
        collector_certificate_openssh: String,
        expires_at: UtcTimestamp,
    },
}

/// Exact immutable and runtime source identity returned only to Evaluation over mTLS.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentFreezeBinding {
    pub environment: FrozenEnvironmentIdentity,
    pub agent_run_id: AgentRunId,
    pub source: EnvironmentFreezeSourceBinding,
}

/// Manifest-authoritative immutable collection result.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FrozenSubmission {
    pub id: FrozenSubmissionId,
    pub course_id: CourseId,
    pub actor_id: ActorId,
    pub agent_run_id: AgentRunId,
    pub attempt: u32,
    pub manifest_revision: Revision,
    pub submission_manifest_sha256: Sha256Digest,
    pub files: Vec<FrozenFile>,
    pub manifest_sha256: Sha256Digest,
    pub object: ArtifactRef,
    pub environment: FrozenEnvironmentIdentity,
    pub retention: RetentionSnapshot,
    pub system_facts: BTreeMap<String, String>,
    pub frozen_at: UtcTimestamp,
    pub derived_archive: Option<ArtifactRef>,
}

impl FrozenSubmission {
    /// Validates file ordering, individual identity, canonical manifest hash, and immutable object.
    pub fn validate(&self) -> Result<(), SubmissionError> {
        if self.attempt == 0
            || self.files.is_empty()
            || self.object.size_bytes == 0
            || self.object.store_binding.trim().is_empty()
            || self.object.object_version.trim().is_empty()
            || self.object.media_type.trim().is_empty()
            || self.environment.release_version == 0
            || self.retention.class != crate::RetentionClass::StudentSubmission
            || self.derived_archive.as_ref().is_some_and(|archive| {
                archive.store_binding.trim().is_empty()
                    || archive.object_version.trim().is_empty()
                    || archive.media_type.trim().is_empty()
                    || archive.size_bytes == 0
            })
        {
            return Err(SubmissionError::IncompleteFreeze);
        }
        let mut previous: Option<&str> = None;
        let mut unique = BTreeSet::new();
        for file in &self.files {
            crate::validate_relative_path(&file.path)
                .map_err(|error| SubmissionError::UnsafePath(error.to_string()))?;
            if file.media_type.trim().is_empty() {
                return Err(SubmissionError::IncompleteFreeze);
            }
            if previous.is_some_and(|path| path >= file.path.as_str())
                || !unique.insert(file.path.as_str())
            {
                return Err(SubmissionError::PathConflict(
                    "frozen files must be unique and lexicographically sorted".to_owned(),
                ));
            }
            previous = Some(&file.path);
        }
        let computed = Sha256Digest::of_canonical(&self.files)
            .map_err(|error| SubmissionError::InvalidManifest(error.to_string()))?;
        if computed != self.manifest_sha256 {
            return Err(SubmissionError::HashMismatch);
        }
        Ok(())
    }
}

fn reject_duplicates(location: &'static str, rules: &[PathRule]) -> Result<(), SubmissionError> {
    let unique = rules.iter().collect::<BTreeSet<_>>();
    if unique.len() != rules.len() {
        return Err(SubmissionError::PathConflict(format!(
            "{location} contains duplicate path rules"
        )));
    }
    Ok(())
}

fn reject_overlaps(location: &'static str, rules: &[PathRule]) -> Result<(), SubmissionError> {
    for (index, left) in rules.iter().enumerate() {
        for right in &rules[index + 1..] {
            if covers(left, right) || covers(right, left) {
                return Err(SubmissionError::PathConflict(format!(
                    "{location} contains overlapping path rules: {} and {}",
                    left.path(),
                    right.path()
                )));
            }
        }
    }
    Ok(())
}

fn covers(parent: &PathRule, child: &PathRule) -> bool {
    if parent == child {
        return true;
    }
    match parent {
        PathRule::ExactFile { .. } => false,
        PathRule::DirectoryTree { path } => {
            child.path().starts_with(path)
                && child.path().as_bytes().get(path.len()).copied() == Some(b'/')
        }
    }
}

/// Submission contract failure.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum SubmissionError {
    #[error("invalid SubmissionManifest: {0}")]
    InvalidManifest(String),
    #[error("unsafe submission path: {0}")]
    UnsafePath(String),
    #[error("submission path rules conflict: {0}")]
    PathConflict(String),
    #[error("LLM path is not part of the frozen submission: {0}")]
    LlmPathNotCollected(String),
    #[error("frozen manifest hash does not match its file list")]
    HashMismatch,
    #[error("FrozenSubmission is incomplete")]
    IncompleteFreeze,
}

#[cfg(test)]
mod tests {
    use super::SubmissionManifest;
    use crate::parse_strict_json;

    fn manifest(include: &str, llm_readable: &str, follow_symlinks: bool) -> String {
        format!(
            r#"{{
                "apiVersion":"evaluation.labweaver.io/v1",
                "kind":"SubmissionManifest",
                "name":"workspace",
                "source":"workspace",
                "include":{include},
                "exclude":[],
                "required":[],
                "llmReadable":{llm_readable},
                "maxTotalBytes":1048576,
                "maxFiles":100,
                "followSymlinks":{follow_symlinks}
            }}"#
        )
    }

    #[test]
    fn manifest_rejects_escape_symlink_overlap_and_uncollected_llm_paths() {
        let escape = manifest(r#"[{"kind":"exactFile","path":"../secret"}]"#, "[]", false);
        assert!(parse_strict_json::<SubmissionManifest>(escape.as_bytes()).is_err());

        let symlink = manifest(r#"[{"kind":"directoryTree","path":"src"}]"#, "[]", true);
        assert!(parse_strict_json::<SubmissionManifest>(symlink.as_bytes()).is_err());

        let overlap = manifest(
            r#"[{"kind":"directoryTree","path":"src"},{"kind":"exactFile","path":"src/main.rs"}]"#,
            "[]",
            false,
        );
        assert!(parse_strict_json::<SubmissionManifest>(overlap.as_bytes()).is_err());

        let not_collected = manifest(
            r#"[{"kind":"directoryTree","path":"src"}]"#,
            r#"[{"kind":"exactFile","path":"private/key.txt"}]"#,
            false,
        );
        assert!(parse_strict_json::<SubmissionManifest>(not_collected.as_bytes()).is_err());
    }

    #[test]
    fn manifest_accepts_exact_and_directory_rules_with_explicit_llm_subset() {
        let input = manifest(
            r#"[{"kind":"directoryTree","path":"src"},{"kind":"exactFile","path":"README.md"}]"#,
            r#"[{"kind":"exactFile","path":"src/main.rs"}]"#,
            false,
        );
        assert!(parse_strict_json::<SubmissionManifest>(input.as_bytes()).is_ok());
    }
}
