//! Teacher authoring, LLM policy, Agent run, candidate, and approval contracts.

use std::collections::BTreeSet;

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};

use crate::diagnostic;
use crate::evaluation::{EvaluationRuntimeIdentity, EvaluationSpec};
use crate::supply_chain::{ImageArtifact, VirtualMachineBaseDisk};
use crate::{
    ActorId, AgentRunId, ApprovalId, ArtifactRef, CandidateId, CourseId, DiagnosticCode,
    EnvironmentId, EvaluationReleaseId, PolicyId, ProblemPackageId, ProjectId, ReleaseId,
    RetentionSnapshot, Revision, UtcTimestamp, WorkConfigurationPlanId,
    WorkConfigurationPreauthorizationId,
};

/// One immutable file in a teacher ProblemPackage.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PackageFile {
    /// Normalized package-relative file path.
    pub path: String,
    /// Immutable object reference.
    pub object: ArtifactRef,
}

/// Immutable, atomically completed teacher material package.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProblemPackage {
    pub id: ProblemPackageId,
    pub project_id: ProjectId,
    pub course_id: Option<CourseId>,
    pub revision: Revision,
    pub files: Vec<PackageFile>,
    pub retention: RetentionSnapshot,
    pub completed_at: UtcTimestamp,
}

impl ProblemPackage {
    /// Validates atomic package identity.
    pub fn validate(&self) -> Result<(), AuthoringError> {
        if self.files.is_empty() {
            return Err(AuthoringError::InvalidPackage(
                "files must not be empty".to_owned(),
            ));
        }
        let mut paths = BTreeSet::new();
        let mut previous: Option<&str> = None;
        for file in &self.files {
            crate::validate_relative_path(&file.path)
                .map_err(|error| AuthoringError::InvalidPackage(error.to_string()))?;
            if !paths.insert(file.path.as_str()) {
                return Err(AuthoringError::InvalidPackage(format!(
                    "duplicate package path: {}",
                    file.path
                )));
            }
            if previous.is_some_and(|path| path >= file.path.as_str()) {
                return Err(AuthoringError::InvalidPackage(
                    "package files must be lexicographically sorted".to_owned(),
                ));
            }
            previous = Some(&file.path);
            validate_artifact_ref(&file.object)?;
        }
        Ok(())
    }

    /// Verifies that this package belongs to the exact project context supplied by its caller.
    pub fn validate_ownership(
        &self,
        project_id: ProjectId,
        course_id: Option<CourseId>,
    ) -> Result<(), AuthoringError> {
        if self.project_id != project_id || self.course_id != course_id {
            return Err(AuthoringError::OwnershipMismatch);
        }
        Ok(())
    }
}

/// Immutable Claude Code worker binding.
///
/// Provider-specific transport and authentication remain deployment-owned Claude Code
/// configuration. The contract binds only a sanitized profile identity, exact model, CLI version,
/// worker image, effective non-secret runtime configuration hash, and per-worker admission limit.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClaudeCodeBindingV1 {
    /// Deployment-owned opaque runtime profile; never a credential or endpoint URL.
    pub runtime_binding: String,
    /// Exact model identifier passed to Claude Code; moving aliases are rejected.
    pub model: String,
    /// Exact Claude Code CLI version baked into the worker image.
    pub claude_code_version: String,
    /// Maximum concurrent Claude Code child processes admitted by one worker instance.
    pub max_in_flight_per_worker: u16,
}

/// Per-attempt bounded LLM budget.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LlmBudget {
    pub max_input_tokens: u64,
    pub max_output_tokens: u64,
    pub max_requests: u32,
    pub max_cost_microusd: u64,
    pub timeout_milliseconds: u64,
    pub max_transient_retries: u8,
    pub max_schema_repairs: u8,
}

/// Non-overridable content classifications at the LLM boundary.
#[derive(
    Clone, Copy, Debug, Deserialize, Eq, JsonSchema, Ord, PartialEq, PartialOrd, Serialize,
)]
#[serde(rename_all = "snake_case")]
pub enum DeniedDataClass {
    Secret,
    Token,
    PrivateKey,
    PersonallyIdentifiableInformation,
    UnallowlistedStudentSubmission,
}

/// Versioned project policy governing all LLM egress. A course is optional teaching context.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProjectLlmEgressPolicy {
    pub id: PolicyId,
    pub project_id: ProjectId,
    pub course_id: Option<CourseId>,
    pub revision: Revision,
    pub binding: ClaudeCodeBindingV1,
    pub budget: LlmBudget,
    pub denied_data_classes: Vec<DeniedDataClass>,
    pub student_content_mode: StudentContentMode,
    pub activated_at: UtcTimestamp,
}

impl ProjectLlmEgressPolicy {
    /// Validates explicit Claude Code identity, budgets, and hard-deny classifications.
    pub fn validate(&self) -> Result<(), AuthoringError> {
        if !valid_runtime_identity(&self.binding.runtime_binding, 256)
            || self.binding.runtime_binding.contains("://")
        {
            return Err(AuthoringError::RuntimeBindingRequired);
        }
        let normalized_model = self.binding.model.to_ascii_lowercase();
        if !valid_runtime_identity(&self.binding.model, 256)
            || matches!(
                normalized_model.as_str(),
                "default" | "sonnet" | "opus" | "haiku" | "opusplan"
            )
        {
            return Err(AuthoringError::ModelRequired);
        }
        if !valid_claude_code_version(&self.binding.claude_code_version) {
            return Err(AuthoringError::RuntimeIdentityInvalid);
        }
        if !(1..=64).contains(&self.binding.max_in_flight_per_worker) {
            return Err(AuthoringError::RuntimeIdentityInvalid);
        }
        if self.budget.max_input_tokens == 0
            || self.budget.max_output_tokens == 0
            || self.budget.max_requests == 0
            || self.budget.max_cost_microusd == 0
            || self.budget.timeout_milliseconds == 0
            || self.budget.max_transient_retries > 2
            || self.budget.max_schema_repairs > 2
        {
            return Err(AuthoringError::InvalidBudget);
        }
        let actual = self
            .denied_data_classes
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();
        let required = [
            DeniedDataClass::Secret,
            DeniedDataClass::Token,
            DeniedDataClass::PrivateKey,
            DeniedDataClass::PersonallyIdentifiableInformation,
            DeniedDataClass::UnallowlistedStudentSubmission,
        ]
        .into_iter()
        .collect::<BTreeSet<_>>();
        if actual != required {
            return Err(AuthoringError::HardDenyClassesModified);
        }
        Ok(())
    }

    /// Verifies that this policy belongs to the exact project context supplied by its caller.
    pub fn validate_ownership(
        &self,
        project_id: ProjectId,
        course_id: Option<CourseId>,
    ) -> Result<(), AuthoringError> {
        if self.project_id != project_id || self.course_id != course_id {
            return Err(AuthoringError::OwnershipMismatch);
        }
        Ok(())
    }
}

fn valid_claude_code_version(value: &str) -> bool {
    let mut parts = value.split('.');
    let valid_part = |part: &str| {
        !part.is_empty()
            && part.bytes().all(|byte| byte.is_ascii_digit())
            && (part == "0" || !part.starts_with('0'))
    };
    matches!(
        (parts.next(), parts.next(), parts.next(), parts.next()),
        (Some(major), Some(minor), Some(patch), None)
            if valid_part(major) && valid_part(minor) && valid_part(patch)
    )
}

fn valid_runtime_identity(value: &str, max_len: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_len
        && value.trim() == value
        && !value.bytes().any(|byte| byte.is_ascii_control())
        && !value.bytes().any(|byte| byte.is_ascii_whitespace())
}

/// Only explicit SubmissionManifest paths may disclose student content.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StudentContentMode {
    ManifestAllowlistOnly,
}

/// Environment business class retained from the v2.1 architecture.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentClass {
    Experiment,
    Work,
}

/// Runtime kind shared by candidates, releases, and instances.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeKind {
    Container,
    VirtualMachine,
}

/// Immutable purpose selected by Control for one Agent run.
///
/// The purpose is authoritative: callers cannot substitute an environment class, target
/// environment, actor, or runtime through an untyped request field. Authoring creates a new
/// Environment/Evaluation package, while WorkConfiguration targets one existing Work environment
/// and produces one configuration plan.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AgentRunPurpose {
    Authoring {
        #[serde(rename = "environmentClass")]
        environment_class: EnvironmentClass,
    },
    WorkConfiguration {
        #[serde(rename = "environmentId")]
        environment_id: EnvironmentId,
        #[serde(rename = "environmentRevision")]
        environment_revision: Revision,
        #[serde(rename = "actorId")]
        actor_id: ActorId,
        /// Runtime selected by the authoritative Work environment.
        ///
        /// Control resolves this value from the environment instance before dispatching the
        /// run. Agent uses it as the immutable execution routing key, so a missing or altered
        /// runtime cannot silently select a different executor.
        #[serde(rename = "runtimeKind")]
        runtime_kind: RuntimeKind,
    },
}

impl AgentRunPurpose {
    /// Returns the target existing environment for a Work configuration run.
    #[must_use]
    pub const fn work_environment(&self) -> Option<(EnvironmentId, Revision, ActorId)> {
        match self {
            Self::Authoring { .. } => None,
            Self::WorkConfiguration {
                environment_id,
                environment_revision,
                actor_id,
                ..
            } => Some((*environment_id, *environment_revision, *actor_id)),
        }
    }

    /// Returns the immutable runtime selected for a Work configuration run.
    #[must_use]
    pub const fn work_runtime_kind(&self) -> Option<RuntimeKind> {
        match self {
            Self::Authoring { .. } => None,
            Self::WorkConfiguration { runtime_kind, .. } => Some(*runtime_kind),
        }
    }
}

/// Immutable generated configuration plan for one existing Work environment.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkConfigurationPlan {
    pub id: WorkConfigurationPlanId,
    pub revision: Revision,
    pub script_artifact: ArtifactRef,
    pub verification_script_artifact: Option<ArtifactRef>,
    pub summary: String,
    pub requires_restart: bool,
    pub environment_id: EnvironmentId,
    pub environment_revision: Revision,
}

impl WorkConfigurationPlan {
    /// Validates bounded plan metadata and immutable script artifact identities.
    pub fn validate(&self) -> Result<(), AuthoringError> {
        validate_artifact_ref(&self.script_artifact)?;
        if let Some(artifact) = &self.verification_script_artifact {
            validate_artifact_ref(artifact)?;
        }
        if self.summary.trim().is_empty() || self.summary.len() > 8_192 {
            return Err(AuthoringError::InvalidWorkConfiguration(
                "configuration plan summary must be non-empty and bounded".to_owned(),
            ));
        }
        Ok(())
    }
}

/// Concrete per-Work grant for executing one exact immutable configuration plan.
///
/// The grant binds both script artifacts and the plan revision. It therefore cannot authorize a
/// newly generated script, a changed verification script, or a plan whose restart requirement was
/// altered after approval.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkConfigurationPreauthorization {
    pub id: WorkConfigurationPreauthorizationId,
    pub project_id: ProjectId,
    pub environment_id: EnvironmentId,
    pub environment_revision: Revision,
    pub actor_id: ActorId,
    pub plan_id: WorkConfigurationPlanId,
    pub plan_revision: Revision,
    pub script_artifact: ArtifactRef,
    pub verification_script_artifact: Option<ArtifactRef>,
    pub expires_at: UtcTimestamp,
    pub revision: Revision,
}

impl WorkConfigurationPreauthorization {
    /// Validates grant identity and exact artifact bindings against a generated plan.
    pub fn validate_against_plan(
        &self,
        plan: &WorkConfigurationPlan,
    ) -> Result<(), AuthoringError> {
        plan.validate()?;
        validate_artifact_ref(&self.script_artifact)?;
        if let Some(artifact) = &self.verification_script_artifact {
            validate_artifact_ref(artifact)?;
        }
        if self.environment_id != plan.environment_id
            || self.environment_revision != plan.environment_revision
            || self.plan_id != plan.id
            || self.plan_revision != plan.revision
            || self.script_artifact != plan.script_artifact
            || self.verification_script_artifact != plan.verification_script_artifact
        {
            return Err(AuthoringError::InvalidWorkConfiguration(
                "preauthorization does not bind the exact configuration plan".to_owned(),
            ));
        }
        Ok(())
    }
}

/// Bounded runtime resources expressed without Kubernetes-dependent parsing.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourceRequirements {
    pub cpu_millicores: u32,
    pub memory_bytes: u64,
    pub storage_bytes: u64,
}

/// Network egress posture for a published environment.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum NetworkPolicySpec {
    AllowAll,
    DenyAll,
    Restricted { policy_binding: String },
}

/// One named service entry exposed only through the controlled access plane.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentEntrySpec {
    pub name: String,
    pub protocol: crate::environment::EndpointProtocol,
    pub service_port: u16,
}

/// Runtime-independent minimum security posture.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentSecuritySpec {
    pub user_policy: RuntimeUserPolicy,
    pub root_filesystem_policy: RootFilesystemPolicy,
    pub privilege_escalation_policy: PrivilegeEscalationPolicy,
    pub public_exposure_policy: PublicExposurePolicy,
    pub security_profile_binding: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeUserPolicy {
    NonRootRequired,
}
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RootFilesystemPolicy {
    ReadOnlyRequired,
    MutableRequired,
}
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PrivilegeEscalationPolicy {
    Deny,
}
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PublicExposurePolicy {
    Deny,
}

/// Bounded direct-exec terminal owned by an immutable Container release.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TerminalSpec {
    pub executable: String,
    #[serde(default)]
    pub args: Vec<String>,
    pub working_directory: String,
}

impl TerminalSpec {
    /// Validates a shell-free invocation rooted in the platform workspace.
    pub fn validate(&self) -> Result<(), AuthoringError> {
        if !normalized_absolute_posix_path(&self.executable) || self.executable.len() > 256 {
            return Err(AuthoringError::InvalidEnvironmentSpec(
                "terminal executable must be a normalized absolute POSIX path".to_owned(),
            ));
        }
        if self.args.len() > 32
            || self.args.iter().any(|argument| {
                argument.len() > 1024 || argument.bytes().any(|byte| byte.is_ascii_control())
            })
        {
            return Err(AuthoringError::InvalidEnvironmentSpec(
                "terminal arguments exceed the bounded direct-exec contract".to_owned(),
            ));
        }
        if self.working_directory != "/workspace" {
            return Err(AuthoringError::InvalidEnvironmentSpec(
                "terminal workingDirectory must be /workspace".to_owned(),
            ));
        }
        Ok(())
    }
}

fn normalized_absolute_posix_path(path: &str) -> bool {
    path.starts_with('/')
        && path.len() > 1
        && !path.ends_with('/')
        && !path.contains("//")
        && !path.split('/').any(|segment| matches!(segment, "." | ".."))
        && !path.bytes().any(|byte| byte.is_ascii_control())
}

/// Strict runtime-specific environment shape.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum EnvironmentRuntimeSpec {
    Container {
        provider_binding: String,
        build_context: ArtifactRef,
        service_port: u16,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        terminal: Option<Box<TerminalSpec>>,
    },
    VirtualMachine {
        provider_binding: String,
        base_disk: VirtualMachineBaseDisk,
        storage_class_binding: String,
        ssh_port: u16,
    },
}

impl EnvironmentRuntimeSpec {
    /// Returns the runtime discriminator.
    #[must_use]
    pub const fn kind(&self) -> RuntimeKind {
        match self {
            Self::Container { .. } => RuntimeKind::Container,
            Self::VirtualMachine { .. } => RuntimeKind::VirtualMachine,
        }
    }
}

/// Stable EnvironmentSpec v1.
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentSpec {
    #[serde(rename = "apiVersion")]
    api_version: EnvironmentApiVersion,
    kind: EnvironmentDocumentKind,
    pub name: String,
    pub class: EnvironmentClass,
    pub resources: ResourceRequirements,
    pub network: NetworkPolicySpec,
    pub entries: Vec<EnvironmentEntrySpec>,
    pub security: EnvironmentSecuritySpec,
    pub runtime: EnvironmentRuntimeSpec,
    pub retention: RetentionSnapshot,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EnvironmentSpecWire {
    #[serde(rename = "apiVersion")]
    api_version: EnvironmentApiVersion,
    kind: EnvironmentDocumentKind,
    name: String,
    class: EnvironmentClass,
    resources: ResourceRequirements,
    network: NetworkPolicySpec,
    entries: Vec<EnvironmentEntrySpec>,
    security: EnvironmentSecuritySpec,
    runtime: EnvironmentRuntimeSpec,
    retention: RetentionSnapshot,
}

impl<'de> Deserialize<'de> for EnvironmentSpec {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = EnvironmentSpecWire::deserialize(deserializer)?;
        let value = Self {
            api_version: wire.api_version,
            kind: wire.kind,
            name: wire.name,
            class: wire.class,
            resources: wire.resources,
            network: wire.network,
            entries: wire.entries,
            security: wire.security,
            runtime: wire.runtime,
            retention: wire.retention,
        };
        value.validate().map_err(serde::de::Error::custom)?;
        Ok(value)
    }
}

impl EnvironmentSpec {
    /// Validates required bindings, resources, and runtime-specific guards.
    pub fn validate(&self) -> Result<(), AuthoringError> {
        if self.name.trim().is_empty() {
            return Err(AuthoringError::InvalidEnvironmentSpec(
                "name is required".to_owned(),
            ));
        }
        if self.resources.cpu_millicores == 0
            || self.resources.memory_bytes == 0
            || self.resources.storage_bytes == 0
        {
            return Err(AuthoringError::InvalidEnvironmentSpec(
                "resource requirements must be non-zero".to_owned(),
            ));
        }
        if self.entries.is_empty() || self.security.security_profile_binding.trim().is_empty() {
            return Err(AuthoringError::InvalidEnvironmentSpec(
                "entries and fail-closed security posture are required".to_owned(),
            ));
        }
        let mut entry_names = BTreeSet::new();
        for entry in &self.entries {
            if entry.name.trim().is_empty()
                || entry.service_port == 0
                || !entry_names.insert(entry.name.as_str())
            {
                return Err(AuthoringError::InvalidEnvironmentSpec(
                    "entries require unique names and non-zero service ports".to_owned(),
                ));
            }
        }
        if let NetworkPolicySpec::Restricted { policy_binding } = &self.network
            && policy_binding.trim().is_empty()
        {
            return Err(AuthoringError::InvalidEnvironmentSpec(
                "restricted network policy requires an explicit binding".to_owned(),
            ));
        }
        match &self.runtime {
            EnvironmentRuntimeSpec::Container {
                provider_binding,
                build_context,
                service_port,
                terminal,
            } => {
                if provider_binding.trim().is_empty()
                    || *service_port == 0
                    || self.security.root_filesystem_policy
                        != RootFilesystemPolicy::ReadOnlyRequired
                    || !self
                        .entries
                        .iter()
                        .any(|entry| entry.service_port == *service_port)
                {
                    return Err(AuthoringError::InvalidEnvironmentSpec(
                        "container binding and port are required".to_owned(),
                    ));
                }
                validate_artifact_ref(build_context)?;
                if let Some(terminal) = terminal {
                    terminal.validate()?;
                }
            }
            EnvironmentRuntimeSpec::VirtualMachine {
                provider_binding,
                base_disk,
                storage_class_binding,
                ssh_port,
            } => {
                if matches!(self.network, NetworkPolicySpec::AllowAll) {
                    return Err(AuthoringError::InvalidEnvironmentSpec(
                        "allow_all network policy is supported only by container runtimes"
                            .to_owned(),
                    ));
                }
                if provider_binding.trim().is_empty()
                    || storage_class_binding.trim().is_empty()
                    || *ssh_port != 22
                    || !self.entries.iter().any(|entry| {
                        entry.protocol == crate::environment::EndpointProtocol::Ssh
                            && entry.service_port == *ssh_port
                    })
                {
                    return Err(AuthoringError::InvalidEnvironmentSpec(
                        "VM requires provider/storage binding and SSH port 22".to_owned(),
                    ));
                }
                base_disk.validate().map_err(|_| {
                    AuthoringError::InvalidEnvironmentSpec(
                        "VM base disk binding must be immutable and complete".to_owned(),
                    )
                })?;
            }
        }
        Ok(())
    }
}

/// Returns the generated JSON Schema for `EnvironmentSpec` v1.
///
/// # Errors
///
/// Returns an error only if the generated Schema cannot be represented as JSON.
pub fn environment_spec_schema() -> Result<serde_json::Value, serde_json::Error> {
    serde_json::to_value(schemars::schema_for!(EnvironmentSpec))
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
enum EnvironmentApiVersion {
    #[serde(rename = "environment.labweaver.io/v1")]
    V1,
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
enum EnvironmentDocumentKind {
    EnvironmentSpec,
}

/// Independent Agent track.
#[derive(
    Clone, Copy, Debug, Deserialize, Eq, JsonSchema, Ord, PartialEq, PartialOrd, Serialize,
)]
#[serde(rename_all = "snake_case")]
pub enum AgentTrackKind {
    Environment,
    Evaluation,
    WorkConfiguration,
}

/// State of one immutable Agent attempt.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentAttemptState {
    Pending,
    Running,
    Repairing,
    AwaitingApproval,
    Succeeded,
    Failed,
    Cancelled,
}

/// Frozen LLM usage for one attempt.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LlmUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub requests: u32,
    pub cost_microusd: u64,
}

/// One append-only attempt in an Agent track.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentAttempt {
    pub number: u32,
    pub state: AgentAttemptState,
    pub checkpoint: Option<ArtifactRef>,
    pub usage: LlmUsage,
    /// Whether a terminal provider envelope made this usage observable.
    pub usage_observed: bool,
    pub diagnostic_code: Option<String>,
}

/// Independent track and its retained attempts.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentTrack {
    pub kind: AgentTrackKind,
    pub attempts: Vec<AgentAttempt>,
    pub candidate_id: Option<CandidateId>,
}

/// Aggregate AgentRun state derived from both tracks.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRunState {
    Requested,
    Running,
    PartiallySucceeded,
    Succeeded,
    AwaitingApproval,
    Failed,
    Cancelling,
    Cancelled,
}

/// One idempotent, auditable dual-candidate Agent run.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentRun {
    pub id: AgentRunId,
    pub project_id: ProjectId,
    pub course_id: Option<CourseId>,
    pub package_id: ProblemPackageId,
    pub policy_id: PolicyId,
    pub policy_revision: Revision,
    pub purpose: AgentRunPurpose,
    pub state: AgentRunState,
    pub revision: Revision,
    pub tracks: Vec<AgentTrack>,
    pub plan: Option<WorkConfigurationPlan>,
}

impl AgentRun {
    /// Validates the exact two-track shape and monotonically numbered attempts.
    pub fn validate(&self) -> Result<(), AuthoringError> {
        let kinds = self
            .tracks
            .iter()
            .map(|track| track.kind)
            .collect::<BTreeSet<_>>();
        let expected_kinds = match self.purpose {
            AgentRunPurpose::Authoring { .. } => {
                [AgentTrackKind::Environment, AgentTrackKind::Evaluation]
                    .into_iter()
                    .collect::<BTreeSet<_>>()
            }
            AgentRunPurpose::WorkConfiguration { .. } => [AgentTrackKind::WorkConfiguration]
                .into_iter()
                .collect::<BTreeSet<_>>(),
        };
        if kinds != expected_kinds || self.tracks.len() != expected_kinds.len() {
            return Err(AuthoringError::InvalidAgentRun(
                "AgentRun tracks do not match its immutable purpose".to_owned(),
            ));
        }
        if matches!(self.purpose, AgentRunPurpose::Authoring { .. }) && self.plan.is_some() {
            return Err(AuthoringError::InvalidAgentRun(
                "authoring runs cannot carry a Work configuration plan".to_owned(),
            ));
        }
        if let AgentRunPurpose::WorkConfiguration {
            environment_id,
            environment_revision,
            ..
        } = self.purpose
        {
            if let Some(plan) = &self.plan
                && (plan.environment_id != environment_id
                    || plan.environment_revision != environment_revision)
            {
                return Err(AuthoringError::InvalidAgentRun(
                    "configuration plan target does not match run purpose".to_owned(),
                ));
            }
            if let Some(plan) = &self.plan {
                plan.validate()?;
            }
        }
        for track in &self.tracks {
            for (index, attempt) in track.attempts.iter().enumerate() {
                let expected = u32::try_from(index + 1).map_err(|_| {
                    AuthoringError::InvalidAgentRun("attempt count exceeds u32".to_owned())
                })?;
                if attempt.number != expected {
                    return Err(AuthoringError::InvalidAgentRun(
                        "attempt numbers must be contiguous and one-based".to_owned(),
                    ));
                }
                let approved_work_execution = track.kind == AgentTrackKind::WorkConfiguration
                    && self.plan.is_some()
                    && attempt.state == AgentAttemptState::Running;
                match attempt.state {
                    AgentAttemptState::Succeeded if !attempt.usage_observed => {
                        return Err(AuthoringError::InvalidAgentRun(
                            "successful attempt requires observed usage".to_owned(),
                        ));
                    }
                    AgentAttemptState::Pending
                    | AgentAttemptState::Running
                    | AgentAttemptState::Repairing
                        if attempt.usage_observed && !approved_work_execution =>
                    {
                        return Err(AuthoringError::InvalidAgentRun(
                            "non-terminal attempt cannot claim observed usage".to_owned(),
                        ));
                    }
                    AgentAttemptState::Failed | AgentAttemptState::Cancelled
                        if attempt.diagnostic_code.as_deref().is_none_or(str::is_empty) =>
                    {
                        return Err(AuthoringError::InvalidAgentRun(
                            "failed or cancelled attempt requires a diagnostic".to_owned(),
                        ));
                    }
                    _ => {}
                }
            }
            if track.candidate_id.is_some()
                && !matches!(
                    track.attempts.last().map(|attempt| attempt.state),
                    Some(AgentAttemptState::Succeeded)
                )
            {
                return Err(AuthoringError::InvalidAgentRun(
                    "candidate identity requires a successful latest attempt".to_owned(),
                ));
            }
            if track.kind == AgentTrackKind::WorkConfiguration && track.candidate_id.is_some() {
                return Err(AuthoringError::InvalidAgentRun(
                    "Work configuration tracks do not carry candidate identities".to_owned(),
                ));
            }
        }
        let derived = self.derived_state()?;
        let work_configuration = matches!(self.purpose, AgentRunPurpose::WorkConfiguration { .. });
        let state_is_coherent = if work_configuration {
            match self.state {
                // A generated Work plan is a proposal until Control binds a concrete grant.
                AgentRunState::AwaitingApproval => {
                    self.plan.is_some() && derived == AgentRunState::AwaitingApproval
                }
                // Running with a plan is the approved execution phase. Running without a plan
                // remains valid while the Agent is still generating the proposal.
                AgentRunState::Running => derived == AgentRunState::Running,
                // Cancellation may race either proposal generation or approved execution.
                AgentRunState::Cancelling => {
                    derived == AgentRunState::Running
                        || (self.plan.is_some() && derived == AgentRunState::AwaitingApproval)
                }
                // A Work run cannot be successful until it has an immutable plan. The execution
                // and verification result is recorded by the owner of the Work environment.
                AgentRunState::Succeeded => {
                    self.plan.is_some() && derived == AgentRunState::Succeeded
                }
                _ => self.state == derived,
            }
        } else if self.state == AgentRunState::Cancelling {
            derived == AgentRunState::Running
        } else {
            self.state == derived
        };
        if !state_is_coherent {
            return Err(AuthoringError::InvalidAgentRun(
                "declared run state does not match retained track attempts".to_owned(),
            ));
        }
        Ok(())
    }

    /// Verifies the immutable package and policy bindings used to create this run.
    pub fn validate_against(
        &self,
        package: &ProblemPackage,
        policy: &ProjectLlmEgressPolicy,
    ) -> Result<(), AuthoringError> {
        if self.project_id != package.project_id
            || self.course_id != package.course_id
            || self.project_id != policy.project_id
            || self.course_id != policy.course_id
            || self.package_id != package.id
            || self.policy_id != policy.id
        {
            return Err(AuthoringError::OwnershipMismatch);
        }
        if self.policy_revision != policy.revision {
            return Err(AuthoringError::InvalidAgentRun(
                "run policy revision does not match the bound policy".to_owned(),
            ));
        }
        Ok(())
    }

    /// Derives aggregate state without discarding either track's outcome.
    pub fn derived_state(&self) -> Result<AgentRunState, AuthoringError> {
        let expected_track_count = match self.purpose {
            AgentRunPurpose::Authoring { .. } => 2,
            AgentRunPurpose::WorkConfiguration { .. } => 1,
        };
        let latest = self
            .tracks
            .iter()
            .map(|track| track.attempts.last().map(|attempt| attempt.state))
            .collect::<Vec<_>>();
        if latest.iter().all(Option::is_none) {
            return Ok(AgentRunState::Requested);
        }
        let succeeded = latest
            .iter()
            .filter(|state| matches!(state, Some(AgentAttemptState::Succeeded)))
            .count();
        if matches!(self.purpose, AgentRunPurpose::WorkConfiguration { .. })
            && latest
                .iter()
                .all(|state| matches!(state, Some(AgentAttemptState::AwaitingApproval)))
        {
            return Ok(AgentRunState::AwaitingApproval);
        }
        let terminal = latest
            .iter()
            .filter(|state| {
                matches!(
                    state,
                    Some(
                        AgentAttemptState::Succeeded
                            | AgentAttemptState::Failed
                            | AgentAttemptState::Cancelled
                    )
                )
            })
            .count();
        if succeeded == expected_track_count {
            Ok(AgentRunState::Succeeded)
        } else if expected_track_count > 1
            && terminal == expected_track_count
            && succeeded == expected_track_count - 1
        {
            Ok(AgentRunState::PartiallySucceeded)
        } else if terminal == expected_track_count
            && latest
                .iter()
                .all(|state| matches!(state, Some(AgentAttemptState::Cancelled)))
        {
            Ok(AgentRunState::Cancelled)
        } else if terminal == expected_track_count {
            Ok(AgentRunState::Failed)
        } else {
            Ok(AgentRunState::Running)
        }
    }
}

/// Immutable validated Environment candidate.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentCandidate {
    pub id: CandidateId,
    pub run_id: AgentRunId,
    pub project_id: ProjectId,
    pub course_id: Option<CourseId>,
    pub revision: Revision,
    pub spec: EnvironmentSpec,
    pub policy_revision: Revision,
    pub model: String,
    pub created_at: UtcTimestamp,
}

impl EnvironmentCandidate {
    pub fn validate(&self) -> Result<(), AuthoringError> {
        self.spec.validate()?;
        if self.model.trim().is_empty() {
            return Err(AuthoringError::InvalidEnvironmentSpec(
                "candidate model is invalid".to_owned(),
            ));
        }
        Ok(())
    }

    /// Verifies that this candidate remains within its run's project context.
    pub fn validate_against_run(&self, run: &AgentRun) -> Result<(), AuthoringError> {
        if self.run_id != run.id
            || self.project_id != run.project_id
            || self.course_id != run.course_id
        {
            return Err(AuthoringError::OwnershipMismatch);
        }
        Ok(())
    }
}

/// Immutable validated Evaluation candidate.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EvaluationCandidate {
    pub id: CandidateId,
    pub run_id: AgentRunId,
    pub project_id: ProjectId,
    pub course_id: Option<CourseId>,
    pub revision: Revision,
    pub spec: EvaluationSpec,
    pub policy_revision: Revision,
    pub model: String,
    pub created_at: UtcTimestamp,
}

impl EvaluationCandidate {
    pub fn validate(&self) -> Result<(), AuthoringError> {
        if self.model.trim().is_empty() {
            return Err(AuthoringError::InvalidAgentRun(
                "candidate model is invalid".to_owned(),
            ));
        }
        Ok(())
    }

    /// Verifies that this candidate remains within its run's project context.
    pub fn validate_against_run(&self, run: &AgentRun) -> Result<(), AuthoringError> {
        if self.run_id != run.id
            || self.project_id != run.project_id
            || self.course_id != run.course_id
        {
            return Err(AuthoringError::OwnershipMismatch);
        }
        Ok(())
    }
}

/// Append-only candidate decision.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateDecision {
    Approved,
    Rejected,
    Withdrawn,
}

/// Human decision bound to an exact candidate and dependency identity.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CandidateApproval {
    pub id: ApprovalId,
    pub candidate_id: CandidateId,
    pub candidate_revision: Revision,
    pub policy_revision: Revision,
    pub trust_revision: Revision,
    pub actor_id: ActorId,
    pub decision: CandidateDecision,
    pub reason: String,
    pub decided_at: UtcTimestamp,
}

impl CandidateApproval {
    /// Checks whether this exact decision remains eligible for release.
    #[must_use]
    pub fn is_release_eligible(
        &self,
        candidate_revision: Revision,
        active_policy_revision: Revision,
        active_trust_revision: Revision,
    ) -> bool {
        self.decision == CandidateDecision::Approved
            && self.candidate_revision == candidate_revision
            && self.policy_revision == active_policy_revision
            && self.trust_revision == active_trust_revision
    }
}

/// One teacher decision that binds a complete immutable authoring package.
///
/// The decision carries only stable identities and the resolved runtime artifact. Control writes
/// it once after validating the package, both candidates, and the authoritative build projection;
/// Environment and Evaluation consume this same binding instead of composing independent
/// candidate approvals.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuthoringApproval {
    pub id: ApprovalId,
    pub project_id: ProjectId,
    pub course_id: Option<CourseId>,
    pub revision: Revision,
    pub package_id: ProblemPackageId,
    pub package_revision: Revision,
    pub environment_candidate_id: CandidateId,
    pub environment_candidate_revision: Revision,
    pub evaluation_candidate_id: CandidateId,
    pub evaluation_candidate_revision: Revision,
    /// Exact Evaluation execution identity frozen when this approval is completed.
    pub evaluation_runtime_identity: EvaluationRuntimeIdentity,
    pub image_artifact: ImageArtifact,
    pub actor_id: ActorId,
    pub reason: String,
    pub approved_at: UtcTimestamp,
}

impl AuthoringApproval {
    /// Validates the bounded immutable approval identity.
    pub fn validate(&self) -> Result<(), AuthoringError> {
        if self.environment_candidate_id == self.evaluation_candidate_id
            || self.reason.trim().is_empty()
            || self.reason.chars().count() > 500
        {
            return Err(AuthoringError::InvalidApproval);
        }
        self.evaluation_runtime_identity
            .validate()
            .map_err(|_| AuthoringError::InvalidApproval)?;
        self.image_artifact
            .validate()
            .map_err(|_| AuthoringError::InvalidArtifactReference)
    }

    /// Verifies that the approval belongs to the requested project context.
    pub fn validate_ownership(
        &self,
        project_id: ProjectId,
        course_id: Option<CourseId>,
    ) -> Result<(), AuthoringError> {
        if self.project_id != project_id || self.course_id != course_id {
            return Err(AuthoringError::OwnershipMismatch);
        }
        Ok(())
    }
}

/// Durable publication state for one immutable authoring approval.
///
/// Control records the state while Environment and Evaluation publish their respective
/// releases. The state is a projection of those downstream acknowledgements and never changes
/// the immutable approval itself.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthoringPublicationState {
    Pending,
    Publishing,
    Ready,
    Failed,
}

/// Project-scoped publication status for a complete authoring approval.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuthoringApprovalPublicationStatus {
    pub approval: AuthoringApproval,
    pub status: AuthoringPublicationState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment_release_id: Option<ReleaseId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evaluation_release_id: Option<EvaluationReleaseId>,
    /// Exact Evaluation release revision acknowledged by the downstream publisher.
    ///
    /// The release identifier alone is insufficient for admission because a release can be
    /// revised independently of its identity. Control persists this value with the publication
    /// projection once Evaluation has returned its durable publish result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evaluation_release_revision: Option<Revision>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostic_code: Option<DiagnosticCode>,
    pub updated_at: UtcTimestamp,
    pub revision: Revision,
}

impl AuthoringApprovalPublicationStatus {
    /// Validates the immutable approval embedded in the publication projection.
    pub fn validate(&self) -> Result<(), AuthoringError> {
        self.approval.validate()
    }

    /// Verifies that the projection belongs to the requested project context.
    pub fn validate_ownership(
        &self,
        project_id: ProjectId,
        course_id: Option<CourseId>,
    ) -> Result<(), AuthoringError> {
        self.approval.validate_ownership(project_id, course_id)
    }
}

fn validate_artifact_ref(reference: &ArtifactRef) -> Result<(), AuthoringError> {
    if reference.store_binding.trim().is_empty()
        || reference.object_version.trim().is_empty()
        || reference.media_type.trim().is_empty()
        || reference.size_bytes == 0
    {
        return Err(AuthoringError::InvalidArtifactReference);
    }
    Ok(())
}

/// Fail-fast authoring contract errors.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum AuthoringError {
    #[error("invalid ProblemPackage: {0}")]
    InvalidPackage(String),
    #[error("ProblemPackage manifest SHA-256 does not match canonical files")]
    PackageHashMismatch,
    #[error("Claude Code runtime binding is required")]
    RuntimeBindingRequired,
    #[error("an immutable Claude Code model identifier is required")]
    ModelRequired,
    #[error("Claude Code runtime identity is invalid")]
    RuntimeIdentityInvalid,
    #[error("LLM token, request, cost, timeout, and repair budgets are invalid")]
    InvalidBudget,
    #[error("hard-denied LLM data classes are not configurable")]
    HardDenyClassesModified,
    #[error("invalid immutable artifact reference")]
    InvalidArtifactReference,
    #[error("invalid EnvironmentSpec: {0}")]
    InvalidEnvironmentSpec(String),
    #[error("invalid AgentRun: {0}")]
    InvalidAgentRun(String),
    #[error("invalid Work configuration: {0}")]
    InvalidWorkConfiguration(String),
    #[error("invalid complete authoring approval")]
    InvalidApproval,
    #[error("project ownership context does not match")]
    OwnershipMismatch,
}

impl AuthoringError {
    /// Returns the stable blocking diagnostic (coarse-grained with detail in `detail`).
    #[must_use]
    pub const fn diagnostic_code(&self) -> &'static str {
        match self {
            Self::RuntimeBindingRequired
            | Self::ModelRequired
            | Self::RuntimeIdentityInvalid
            | Self::InvalidEnvironmentSpec(_) => diagnostic::INVALID_REQUEST,
            Self::InvalidBudget | Self::HardDenyClassesModified => diagnostic::ACCESS_DENIED,
            Self::PackageHashMismatch => diagnostic::HASH_MISMATCH,
            Self::InvalidPackage(_)
            | Self::InvalidArtifactReference
            | Self::InvalidAgentRun(_)
            | Self::InvalidWorkConfiguration(_)
            | Self::InvalidApproval
            | Self::OwnershipMismatch => diagnostic::CONTRACT_DOCUMENT_INVALID,
        }
    }
}

#[cfg(test)]
mod terminal_tests {
    use super::TerminalSpec;

    #[test]
    fn terminal_spec_is_shell_free_normalized_and_bounded() {
        let valid = TerminalSpec {
            executable: "/bin/bash".to_owned(),
            args: vec!["--noprofile".to_owned(), "--norc".to_owned()],
            working_directory: "/workspace".to_owned(),
        };
        assert!(valid.validate().is_ok());
        for executable in ["bash", "/bin/../bash", "/bin//bash"] {
            let mut invalid = valid.clone();
            invalid.executable = executable.to_owned();
            assert!(invalid.validate().is_err());
        }
        let mut wrong_directory = valid.clone();
        wrong_directory.working_directory = "/tmp".to_owned();
        assert!(wrong_directory.validate().is_err());
    }
}

#[cfg(test)]
mod agent_run_state_tests {
    use std::error::Error;

    use super::{
        AgentAttempt, AgentAttemptState, AgentRun, AgentRunPurpose, AgentRunState, AgentTrack,
        AgentTrackKind, EnvironmentClass, LlmUsage, WorkConfigurationPlan,
    };
    use crate::{ArtifactId, ArtifactRef, PolicyId, ProblemPackageId, ProjectId, Revision};

    fn attempt(state: AgentAttemptState) -> AgentAttempt {
        AgentAttempt {
            number: 1,
            state,
            checkpoint: None,
            usage: LlmUsage {
                input_tokens: 0,
                output_tokens: 0,
                requests: 0,
                cost_microusd: 0,
            },
            usage_observed: state == AgentAttemptState::Succeeded,
            diagnostic_code: match state {
                AgentAttemptState::Failed | AgentAttemptState::Cancelled => {
                    Some("LW_AGENT_ATTEMPT_FAILED".to_owned())
                }
                AgentAttemptState::Pending
                | AgentAttemptState::Running
                | AgentAttemptState::Repairing
                | AgentAttemptState::AwaitingApproval
                | AgentAttemptState::Succeeded => None,
            },
        }
    }

    fn track(kind: AgentTrackKind, state: AgentAttemptState) -> AgentTrack {
        AgentTrack {
            kind,
            attempts: vec![attempt(state)],
            candidate_id: None,
        }
    }

    fn plan(
        environment_id: crate::EnvironmentId,
        environment_revision: Revision,
    ) -> Result<WorkConfigurationPlan, Box<dyn Error>> {
        Ok(WorkConfigurationPlan {
            id: crate::WorkConfigurationPlanId::new(),
            revision: Revision::new(1)?,
            script_artifact: ArtifactRef {
                artifact_id: ArtifactId::new(),
                store_binding: "object-store".to_owned(),
                object_version: "v1".to_owned(),
                size_bytes: 1,
                media_type: "text/plain".to_owned(),
            },
            verification_script_artifact: None,
            summary: "apply configuration".to_owned(),
            requires_restart: false,
            environment_id,
            environment_revision,
        })
    }

    fn run(
        purpose: AgentRunPurpose,
        state: AgentRunState,
        tracks: Vec<AgentTrack>,
    ) -> Result<AgentRun, Box<dyn Error>> {
        Ok(AgentRun {
            id: crate::AgentRunId::new(),
            project_id: ProjectId::new(),
            course_id: None,
            package_id: ProblemPackageId::new(),
            policy_id: PolicyId::new(),
            policy_revision: Revision::new(1)?,
            purpose,
            state,
            revision: Revision::new(1)?,
            tracks,
            plan: None,
        })
    }

    #[test]
    fn authoring_uses_two_track_aggregate_state() -> Result<(), Box<dyn Error>> {
        let purpose = AgentRunPurpose::Authoring {
            environment_class: EnvironmentClass::Experiment,
        };
        let succeeded = run(
            purpose,
            AgentRunState::Succeeded,
            vec![
                track(AgentTrackKind::Environment, AgentAttemptState::Succeeded),
                track(AgentTrackKind::Evaluation, AgentAttemptState::Succeeded),
            ],
        )?;
        assert_eq!(succeeded.derived_state()?, AgentRunState::Succeeded);
        assert!(succeeded.validate().is_ok());

        let partial = run(
            purpose,
            AgentRunState::PartiallySucceeded,
            vec![
                track(AgentTrackKind::Environment, AgentAttemptState::Succeeded),
                track(AgentTrackKind::Evaluation, AgentAttemptState::Failed),
            ],
        )?;
        assert_eq!(partial.derived_state()?, AgentRunState::PartiallySucceeded);
        assert!(partial.validate().is_ok());
        Ok(())
    }

    #[test]
    fn work_configuration_uses_one_track_and_allows_awaiting_approval() -> Result<(), Box<dyn Error>>
    {
        let environment_id = crate::EnvironmentId::new();
        let environment_revision = Revision::new(2)?;
        let purpose = AgentRunPurpose::WorkConfiguration {
            environment_id,
            environment_revision,
            actor_id: crate::ActorId::new(),
            runtime_kind: super::RuntimeKind::Container,
        };
        let mut awaiting = run(
            purpose,
            AgentRunState::AwaitingApproval,
            vec![track(
                AgentTrackKind::WorkConfiguration,
                AgentAttemptState::AwaitingApproval,
            )],
        )?;
        awaiting.plan = Some(plan(environment_id, environment_revision)?);
        awaiting.tracks[0].attempts[0].usage_observed = true;
        assert_eq!(awaiting.derived_state()?, AgentRunState::AwaitingApproval);
        assert!(awaiting.validate().is_ok());

        let mut approved = awaiting.clone();
        approved.state = AgentRunState::Running;
        approved.tracks[0].attempts[0].state = AgentAttemptState::Running;
        assert_eq!(approved.derived_state()?, AgentRunState::Running);
        assert!(approved.validate().is_ok());

        let mut unplanned_execution = approved.clone();
        unplanned_execution.plan = None;
        assert!(unplanned_execution.validate().is_err());

        let mut authoring_with_observed_running = run(
            AgentRunPurpose::Authoring {
                environment_class: EnvironmentClass::Experiment,
            },
            AgentRunState::Running,
            vec![
                track(AgentTrackKind::Environment, AgentAttemptState::Running),
                track(AgentTrackKind::Evaluation, AgentAttemptState::Pending),
            ],
        )?;
        authoring_with_observed_running.tracks[0].attempts[0].usage_observed = true;
        assert!(authoring_with_observed_running.validate().is_err());

        let missing_plan = run(
            purpose,
            AgentRunState::AwaitingApproval,
            vec![track(
                AgentTrackKind::WorkConfiguration,
                AgentAttemptState::AwaitingApproval,
            )],
        )?;
        assert!(missing_plan.validate().is_err());

        let failed = run(
            purpose,
            AgentRunState::Failed,
            vec![track(
                AgentTrackKind::WorkConfiguration,
                AgentAttemptState::Failed,
            )],
        )?;
        assert_eq!(failed.derived_state()?, AgentRunState::Failed);
        assert!(failed.validate().is_ok());

        let mut approved = run(
            purpose,
            AgentRunState::Running,
            vec![track(
                AgentTrackKind::WorkConfiguration,
                AgentAttemptState::Running,
            )],
        )?;
        approved.plan = Some(plan(environment_id, environment_revision)?);
        assert_eq!(approved.derived_state()?, AgentRunState::Running);
        assert!(approved.validate().is_ok());

        let mut stale_success = run(
            purpose,
            AgentRunState::Running,
            vec![track(
                AgentTrackKind::WorkConfiguration,
                AgentAttemptState::Succeeded,
            )],
        )?;
        stale_success.plan = Some(plan(environment_id, environment_revision)?);
        assert!(stale_success.validate().is_err());
        Ok(())
    }
}
