use schemars::{JsonSchema, schema_for};
use serde::{Deserialize, Deserializer, Serialize};

use super::validation::{EvaluationSpecError, validate_spec};

/// Versioned evaluation definition shared by OJ and Linux system experiments.
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EvaluationSpec {
    #[serde(rename = "apiVersion")]
    api_version: EvaluationApiVersion,
    kind: EvaluationKind,
    metadata: EvaluationMetadata,
    spec: EvaluationBody,
}

impl EvaluationSpec {
    /// Parses an `EvaluationSpec` YAML document and runs semantic validation.
    ///
    /// # Errors
    ///
    /// Returns a stable diagnostic for malformed YAML or invalid evaluation semantics.
    pub fn from_yaml(input: &str) -> Result<Self, EvaluationSpecError> {
        let wire: EvaluationSpecWire = serde_yaml::from_str(input)
            .map_err(|error| EvaluationSpecError::InvalidDocument(error.to_string()))?;
        let spec = Self::from_wire(wire);
        spec.validate()?;
        Ok(spec)
    }

    /// Validates cross-field, dependency, and deterministic aggregation constraints.
    ///
    /// # Errors
    ///
    /// Returns the first stable blocking diagnostic.
    pub fn validate(&self) -> Result<(), EvaluationSpecError> {
        validate_spec(self)
    }

    /// Returns immutable contract metadata.
    #[must_use]
    pub const fn metadata(&self) -> &EvaluationMetadata {
        &self.metadata
    }

    /// Returns the immutable evaluation body.
    #[must_use]
    pub const fn body(&self) -> &EvaluationBody {
        &self.spec
    }

    fn from_wire(wire: EvaluationSpecWire) -> Self {
        Self {
            api_version: wire.api_version,
            kind: wire.kind,
            metadata: wire.metadata,
            spec: wire.spec,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EvaluationSpecWire {
    #[serde(rename = "apiVersion")]
    api_version: EvaluationApiVersion,
    kind: EvaluationKind,
    metadata: EvaluationMetadata,
    spec: EvaluationBody,
}

impl<'de> Deserialize<'de> for EvaluationSpec {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let spec = Self::from_wire(EvaluationSpecWire::deserialize(deserializer)?);
        spec.validate().map_err(serde::de::Error::custom)?;
        Ok(spec)
    }
}

/// Returns the generated JSON Schema for `EvaluationSpec` v1.
///
/// # Errors
///
/// Returns an error only if the generated schema cannot be represented as JSON.
pub fn evaluation_spec_schema() -> Result<serde_json::Value, serde_json::Error> {
    serde_json::to_value(schema_for!(EvaluationSpec))
}

/// Returns the generated JSON Schema for advisory `GoalReview` v1.
///
/// # Errors
///
/// Returns an error only if the generated schema cannot be represented as JSON.
pub fn goal_review_schema() -> Result<serde_json::Value, serde_json::Error> {
    serde_json::to_value(schema_for!(super::GoalReview))
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
enum EvaluationApiVersion {
    #[serde(rename = "evaluation.labweaver.io/v1")]
    V1,
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
enum EvaluationKind {
    EvaluationSpec,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
/// Stable name and version of an evaluation definition.
pub struct EvaluationMetadata {
    pub(crate) name: String,
    pub(crate) version: String,
}

impl EvaluationMetadata {
    /// Returns the stable evaluation name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the author-controlled evaluation version.
    #[must_use]
    pub fn version(&self) -> &str {
        &self.version
    }
}

/// Submission, step, aggregation, and review decomposition of an evaluation.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EvaluationBody {
    pub(crate) submission: SubmissionSpec,
    pub(crate) steps: Vec<EvaluationStep>,
    pub(crate) aggregation: AggregationSpec,
    pub(crate) review: ReviewPolicy,
}

impl EvaluationBody {
    /// Returns the immutable submission collection contract.
    #[must_use]
    pub const fn submission(&self) -> &SubmissionSpec {
        &self.submission
    }

    /// Returns evaluation steps in declaration order.
    #[must_use]
    pub fn steps(&self) -> &[EvaluationStep] {
        &self.steps
    }

    /// Returns deterministic aggregation configuration.
    #[must_use]
    pub const fn aggregation(&self) -> &AggregationSpec {
        &self.aggregation
    }

    /// Returns release and manual-review policy.
    #[must_use]
    pub const fn review(&self) -> &ReviewPolicy {
        &self.review
    }
}

/// Submission collection boundary used before evaluation starts.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SubmissionSpec {
    pub(crate) collector: CollectorSpec,
    #[serde(rename = "llmReadable")]
    pub(crate) llm_readable: Vec<String>,
}

impl SubmissionSpec {
    /// Returns the explicitly bound collector.
    #[must_use]
    pub const fn collector(&self) -> &CollectorSpec {
        &self.collector
    }

    /// Returns frozen submission paths that may be disclosed to an LLM Runner.
    #[must_use]
    pub fn llm_readable(&self) -> &[String] {
        &self.llm_readable
    }
}

/// Supported P0 submission collectors.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CollectorSpec {
    /// Collects an allowlisted workspace snapshot.
    WorkspaceSnapshot {
        /// Included submission-relative paths.
        include: Vec<String>,
        #[serde(default)]
        /// Excluded submission-relative paths.
        exclude: Vec<String>,
        #[serde(rename = "maxBytes")]
        /// Maximum collected bytes.
        max_bytes: u64,
    },
    /// Collects bounded system facts for a Linux experiment.
    SystemFacts {
        /// Named facts to collect.
        facts: Vec<String>,
        #[serde(rename = "maxBytes")]
        /// Maximum collected bytes.
        max_bytes: u64,
    },
}

impl CollectorSpec {
    /// Returns included workspace paths when this is a workspace collector.
    #[must_use]
    pub fn included_paths(&self) -> Option<&[String]> {
        match self {
            Self::WorkspaceSnapshot { include, .. } => Some(include),
            Self::SystemFacts { .. } => None,
        }
    }

    /// Returns excluded workspace paths when this is a workspace collector.
    #[must_use]
    pub fn excluded_paths(&self) -> Option<&[String]> {
        match self {
            Self::WorkspaceSnapshot { exclude, .. } => Some(exclude),
            Self::SystemFacts { .. } => None,
        }
    }

    pub(crate) fn has_inputs(&self) -> bool {
        match self {
            Self::WorkspaceSnapshot { include, .. } => !include.is_empty(),
            Self::SystemFacts { facts, .. } => !facts.is_empty(),
        }
    }

    /// Returns the collection byte limit.
    #[must_use]
    pub const fn max_bytes(&self) -> u64 {
        match self {
            Self::WorkspaceSnapshot { max_bytes, .. } | Self::SystemFacts { max_bytes, .. } => {
                *max_bytes
            }
        }
    }
}

/// A role-specific evaluation step.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "role", rename_all = "snake_case", deny_unknown_fields)]
pub enum EvaluationStep {
    /// A deterministic prerequisite that can stop downstream execution.
    Gate(GateStep),
    /// A deterministic step that contributes to aggregation.
    Score(ScoreStep),
    /// A non-scoring advisory review step.
    Advisory(AdvisoryStep),
}

impl EvaluationStep {
    /// Returns the unique step identifier.
    #[must_use]
    pub fn id(&self) -> &str {
        match self {
            Self::Gate(step) => &step.id,
            Self::Score(step) => &step.id,
            Self::Advisory(step) => &step.id,
        }
    }

    /// Returns declared predecessor step identifiers.
    #[must_use]
    pub fn dependencies(&self) -> &[String] {
        match self {
            Self::Gate(step) => &step.depends_on,
            Self::Score(step) => &step.depends_on,
            Self::Advisory(step) => &step.depends_on,
        }
    }

    /// Reports whether this is a Gate step.
    #[must_use]
    pub const fn is_gate(&self) -> bool {
        matches!(self, Self::Gate(_))
    }

    /// Returns the maximum deterministic score, or none for non-scoring steps.
    #[must_use]
    pub const fn score(&self) -> Option<u32> {
        match self {
            Self::Score(step) => Some(step.score.max),
            Self::Gate(_) | Self::Advisory(_) => None,
        }
    }

    /// Returns the role-specific failure behavior declared in the immutable spec.
    #[must_use]
    pub const fn failure_policy(&self) -> super::control::EvaluationStepFailurePolicy {
        match self {
            Self::Gate(_) => super::control::EvaluationStepFailurePolicy::Stop,
            Self::Score(step) => match step.failure_policy {
                ScoreFailurePolicy::Stop => super::control::EvaluationStepFailurePolicy::Stop,
                ScoreFailurePolicy::Continue => {
                    super::control::EvaluationStepFailurePolicy::Continue
                }
            },
            Self::Advisory(_) => super::control::EvaluationStepFailurePolicy::ContinueAdvisory,
        }
    }

    /// Returns the deterministic runner for Gate and Score steps.
    #[must_use]
    pub fn deterministic_runner(&self) -> Option<&DeterministicRunnerSpec> {
        match self {
            Self::Gate(step) => Some(&step.runner),
            Self::Score(step) => Some(&step.runner),
            Self::Advisory(_) => None,
        }
    }

    /// Returns the advisory runner for Advisory steps.
    #[must_use]
    pub fn advisory_runner(&self) -> Option<&AdvisoryRunnerSpec> {
        match self {
            Self::Advisory(step) => Some(&step.runner),
            Self::Gate(_) | Self::Score(_) => None,
        }
    }

    pub(crate) fn deterministic_checker(&self) -> Option<&CheckerSpec> {
        match self {
            Self::Gate(step) => Some(&step.checker),
            Self::Score(step) => Some(&step.checker),
            Self::Advisory(_) => None,
        }
    }
}

/// Deterministic prerequisite step.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GateStep {
    id: String,
    #[serde(default)]
    depends_on: Vec<String>,
    runner: DeterministicRunnerSpec,
    checker: CheckerSpec,
    failure_policy: GateFailurePolicy,
}

impl GateStep {
    /// Returns the step identifier.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Returns predecessor step identifiers.
    #[must_use]
    pub fn dependencies(&self) -> &[String] {
        &self.depends_on
    }

    /// Returns the deterministic runner configuration.
    #[must_use]
    pub const fn runner(&self) -> &DeterministicRunnerSpec {
        &self.runner
    }

    /// Returns the deterministic checker configuration.
    #[must_use]
    pub const fn checker(&self) -> &CheckerSpec {
        &self.checker
    }

    /// Returns the Gate failure policy.
    #[must_use]
    pub const fn failure_policy(&self) -> GateFailurePolicy {
        self.failure_policy
    }
}

/// Deterministic scoring step.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScoreStep {
    id: String,
    #[serde(default)]
    depends_on: Vec<String>,
    runner: DeterministicRunnerSpec,
    checker: CheckerSpec,
    score: ScoreSpec,
    failure_policy: ScoreFailurePolicy,
}

impl ScoreStep {
    /// Returns the step identifier.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Returns predecessor step identifiers.
    #[must_use]
    pub fn dependencies(&self) -> &[String] {
        &self.depends_on
    }

    /// Returns the deterministic runner configuration.
    #[must_use]
    pub const fn runner(&self) -> &DeterministicRunnerSpec {
        &self.runner
    }

    /// Returns the deterministic checker configuration.
    #[must_use]
    pub const fn checker(&self) -> &CheckerSpec {
        &self.checker
    }

    /// Returns the score contract.
    #[must_use]
    pub const fn score_spec(&self) -> &ScoreSpec {
        &self.score
    }

    /// Returns the scoring failure policy.
    #[must_use]
    pub const fn failure_policy(&self) -> ScoreFailurePolicy {
        self.failure_policy
    }
}

/// Non-scoring advisory review step.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AdvisoryStep {
    id: String,
    #[serde(default)]
    depends_on: Vec<String>,
    runner: AdvisoryRunnerSpec,
    failure_policy: AdvisoryFailurePolicy,
}

impl AdvisoryStep {
    /// Returns the step identifier.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Returns predecessor step identifiers.
    #[must_use]
    pub fn dependencies(&self) -> &[String] {
        &self.depends_on
    }

    /// Returns advisory runner configuration.
    #[must_use]
    pub const fn runner(&self) -> &AdvisoryRunnerSpec {
        &self.runner
    }

    /// Returns the advisory failure policy.
    #[must_use]
    pub const fn failure_policy(&self) -> AdvisoryFailurePolicy {
        self.failure_policy
    }
}

/// Failure behavior for a Gate step.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GateFailurePolicy {
    /// Stop downstream deterministic execution.
    Stop,
}

/// Failure behavior for a Score step.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScoreFailurePolicy {
    /// Stop downstream deterministic execution.
    Stop,
    /// Continue with independent steps while preserving the failure.
    Continue,
}

/// Failure behavior for an Advisory step.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AdvisoryFailurePolicy {
    /// Preserve the advisory failure without changing deterministic results.
    ContinueAdvisory,
}

/// Deterministic Runner configurations frozen for P0 OJ and Linux evaluation.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DeterministicRunnerSpec {
    /// Verifies that allowlisted submission files exist.
    FileAssertion {
        #[serde(rename = "requiredFiles")]
        /// Required submission-relative files.
        required_files: Vec<String>,
    },
    /// Compiles or tests a program through an approved toolchain profile.
    Program {
        #[serde(rename = "toolchainProfile")]
        /// Explicit approved toolchain binding.
        toolchain_profile: String,
        /// Compile or test phase.
        phase: ProgramPhase,
        /// Submission-relative program input.
        input: String,
        #[serde(rename = "testGroups", default)]
        /// Deterministic test groups used by the test phase.
        test_groups: Vec<TestGroup>,
        /// Mandatory execution limits.
        limits: ExecutionLimits,
    },
    /// Executes a read-only approved Ansible probe profile.
    AnsibleProbe {
        #[serde(rename = "playbookProfile")]
        /// Explicit approved playbook binding.
        playbook_profile: String,
        #[serde(rename = "moduleAllowlist")]
        /// Modules requested from the frozen P0 allowlist.
        module_allowlist: Vec<String>,
        #[serde(rename = "readOnly")]
        /// Must remain true in v1.
        read_only: bool,
        /// Facts expected from the probe.
        assertions: Vec<FactAssertion>,
    },
}

impl DeterministicRunnerSpec {
    /// Returns all submission-relative paths consumed by this Runner.
    #[must_use]
    pub fn submission_paths(&self) -> Vec<&str> {
        match self {
            Self::FileAssertion { required_files } => {
                required_files.iter().map(String::as_str).collect()
            }
            Self::Program { input, .. } => vec![input],
            Self::AnsibleProbe { .. } => Vec::new(),
        }
    }

    pub(crate) fn validate(&self, step_id: &str) -> Result<(), EvaluationSpecError> {
        match self {
            Self::FileAssertion { required_files } if required_files.is_empty() => {
                Err(EvaluationSpecError::InvalidStepConfiguration {
                    step_id: step_id.to_owned(),
                    detail: "file_assertion requires at least one file".to_owned(),
                })
            }
            Self::Program {
                toolchain_profile,
                phase,
                input,
                test_groups,
                limits,
            } => {
                if toolchain_profile.trim().is_empty() || input.trim().is_empty() {
                    return Err(EvaluationSpecError::InvalidStepConfiguration {
                        step_id: step_id.to_owned(),
                        detail: "program runner requires a toolchain profile and input".to_owned(),
                    });
                }
                if *phase == ProgramPhase::Test && test_groups.is_empty() {
                    return Err(EvaluationSpecError::InvalidStepConfiguration {
                        step_id: step_id.to_owned(),
                        detail: "program test phase requires test groups".to_owned(),
                    });
                }
                if *phase == ProgramPhase::Compile && !test_groups.is_empty() {
                    return Err(EvaluationSpecError::InvalidStepConfiguration {
                        step_id: step_id.to_owned(),
                        detail: "program compile phase cannot contain test groups".to_owned(),
                    });
                }
                if test_groups.iter().any(|group| {
                    group.name.trim().is_empty()
                        || group.source.trim().is_empty()
                        || group.max_points == 0
                }) {
                    return Err(EvaluationSpecError::InvalidStepConfiguration {
                        step_id: step_id.to_owned(),
                        detail: "test groups require a name, source, and non-zero maxPoints"
                            .to_owned(),
                    });
                }
                if limits.wall_time_seconds == 0
                    || limits.memory_bytes == 0
                    || limits.output_bytes == 0
                {
                    return Err(EvaluationSpecError::InvalidStepConfiguration {
                        step_id: step_id.to_owned(),
                        detail: "execution limits must be non-zero".to_owned(),
                    });
                }
                Ok(())
            }
            Self::AnsibleProbe {
                playbook_profile,
                module_allowlist,
                read_only,
                assertions,
            } => {
                if playbook_profile.trim().is_empty()
                    || module_allowlist.is_empty()
                    || assertions.is_empty()
                {
                    return Err(EvaluationSpecError::InvalidStepConfiguration {
                        step_id: step_id.to_owned(),
                        detail: "ansible_probe requires a profile, allowlist, and assertions"
                            .to_owned(),
                    });
                }
                if !read_only {
                    return Err(EvaluationSpecError::InvalidStepConfiguration {
                        step_id: step_id.to_owned(),
                        detail: "v1 ansible_probe must be read-only".to_owned(),
                    });
                }
                if module_allowlist
                    .iter()
                    .any(|module| !is_allowed_probe_module(module))
                {
                    return Err(EvaluationSpecError::InvalidStepConfiguration {
                        step_id: step_id.to_owned(),
                        detail: "ansible_probe contains a module outside the v1 allowlist"
                            .to_owned(),
                    });
                }
                Ok(())
            }
            Self::FileAssertion { .. } => Ok(()),
        }
    }
}

fn is_allowed_probe_module(module: &str) -> bool {
    matches!(
        module,
        "ansible.builtin.service_facts" | "ansible.builtin.stat" | "ansible.builtin.package_facts"
    )
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
/// Approved program Runner phase.
pub enum ProgramPhase {
    /// Compile the submitted program.
    Compile,
    /// Execute deterministic test groups.
    Test,
}

/// One deterministic program test group.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TestGroup {
    name: String,
    source: String,
    max_points: u32,
}

impl TestGroup {
    /// Returns the test group name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the immutable evaluator source locator.
    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Returns the deterministic group maximum points.
    #[must_use]
    pub const fn max_points(&self) -> u32 {
        self.max_points
    }
}

/// Resource and output limits for a program Runner.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExecutionLimits {
    wall_time_seconds: u64,
    memory_bytes: u64,
    output_bytes: u64,
}

impl ExecutionLimits {
    /// Returns the wall-clock timeout in seconds.
    #[must_use]
    pub const fn wall_time_seconds(&self) -> u64 {
        self.wall_time_seconds
    }

    /// Returns the memory limit in bytes.
    #[must_use]
    pub const fn memory_bytes(&self) -> u64 {
        self.memory_bytes
    }

    /// Returns the output limit in bytes.
    #[must_use]
    pub const fn output_bytes(&self) -> u64 {
        self.output_bytes
    }
}

/// Expected fact emitted by a Linux probe.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FactAssertion {
    fact: String,
    expected: serde_json::Value,
}

impl FactAssertion {
    /// Returns the stable fact name.
    #[must_use]
    pub fn fact(&self) -> &str {
        &self.fact
    }

    /// Returns the expected JSON value.
    #[must_use]
    pub const fn expected(&self) -> &serde_json::Value {
        &self.expected
    }
}

/// Advisory Runner configurations, structurally separated from scoring.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AdvisoryRunnerSpec {
    /// Reviews allowlisted submission paths without producing a score.
    LlmReview {
        /// Submission-relative paths visible to the LLM.
        include: Vec<String>,
        /// Immutable advisory rubric locator.
        rubric: String,
        #[serde(rename = "outputMode")]
        /// Versioned advisory output shape.
        output_mode: AdvisoryOutputMode,
    },
}

impl AdvisoryRunnerSpec {
    /// Returns submission-relative paths visible to the advisory Runner.
    #[must_use]
    pub fn included_paths(&self) -> &[String] {
        match self {
            Self::LlmReview { include, .. } => include,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
/// Supported non-scoring advisory output mode.
pub enum AdvisoryOutputMode {
    /// Emit a `goal-review/v1` assessment.
    GoalAssessment,
}

/// Deterministic Checker configurations frozen for v1.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CheckerSpec {
    /// Exact byte-oriented comparison.
    Exact,
    /// Whitespace-insensitive token comparison.
    Token,
    /// Process exit-code comparison.
    ExitCode {
        /// Required exit code.
        expected: i32,
    },
    /// JSON Schema document validation.
    JsonSchema {
        #[serde(rename = "schemaRef")]
        /// Immutable schema locator.
        schema_ref: String,
    },
    /// System service state comparison.
    ServiceState {
        /// Stable service name.
        service: String,
        /// Required service state.
        expected: ExpectedServiceState,
    },
}

impl CheckerSpec {
    pub(crate) fn validate_for(
        &self,
        runner: &DeterministicRunnerSpec,
        step_id: &str,
    ) -> Result<(), EvaluationSpecError> {
        if let Self::JsonSchema { schema_ref } = self
            && schema_ref.trim().is_empty()
        {
            return Err(invalid_checker(step_id, "json_schema requires schemaRef"));
        }
        if let Self::ServiceState { service, .. } = self
            && service.trim().is_empty()
        {
            return Err(invalid_checker(
                step_id,
                "service_state requires a service name",
            ));
        }

        let compatible = matches!(
            (runner, self),
            (
                DeterministicRunnerSpec::FileAssertion { .. }
                    | DeterministicRunnerSpec::Program {
                        phase: ProgramPhase::Compile,
                        ..
                    },
                Self::ExitCode { .. }
            ) | (
                DeterministicRunnerSpec::Program {
                    phase: ProgramPhase::Test,
                    ..
                },
                Self::Exact | Self::Token | Self::JsonSchema { .. }
            ) | (
                DeterministicRunnerSpec::AnsibleProbe { .. },
                Self::ExitCode { .. } | Self::JsonSchema { .. } | Self::ServiceState { .. }
            )
        );
        if compatible {
            Ok(())
        } else {
            Err(invalid_checker(
                step_id,
                "checker is incompatible with the selected runner and phase",
            ))
        }
    }
}

fn invalid_checker(step_id: &str, detail: &str) -> EvaluationSpecError {
    EvaluationSpecError::InvalidStepConfiguration {
        step_id: step_id.to_owned(),
        detail: detail.to_owned(),
    }
}

/// Expected state of a system service.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExpectedServiceState {
    /// Service is active.
    Active,
    /// Service is inactive.
    Inactive,
}

/// Maximum contribution of a deterministic Score step.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScoreSpec {
    max: u32,
}

impl ScoreSpec {
    /// Returns the maximum deterministic score contribution.
    #[must_use]
    pub const fn max(&self) -> u32 {
        self.max
    }
}

/// Pure deterministic score aggregation contract.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AggregationSpec {
    pub(crate) kind: AggregationKind,
    pub(crate) max_score: u32,
    pub(crate) gates: Vec<AggregationGate>,
}

impl AggregationSpec {
    /// Returns the deterministic aggregation algorithm.
    #[must_use]
    pub const fn kind(&self) -> AggregationKind {
        self.kind
    }

    /// Returns the declared maximum score.
    #[must_use]
    pub const fn max_score(&self) -> u32 {
        self.max_score
    }

    /// Returns release-blocking deterministic gates.
    #[must_use]
    pub fn gates(&self) -> &[AggregationGate] {
        &self.gates
    }
}

/// Supported deterministic aggregation algorithm.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AggregationKind {
    /// Checked sum of deterministic Score steps.
    DeterministicSum,
}

/// Required Gate status used by deterministic aggregation.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AggregationGate {
    pub(crate) step: String,
    pub(crate) required_status: RequiredStatus,
}

impl AggregationGate {
    /// Returns the referenced Gate step identifier.
    #[must_use]
    pub fn step(&self) -> &str {
        &self.step
    }

    /// Returns the required terminal status.
    #[must_use]
    pub const fn required_status(&self) -> RequiredStatus {
        self.required_status
    }
}

/// Required deterministic Gate terminal status.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RequiredStatus {
    /// The Gate must pass.
    Passed,
}

/// Mandatory approval and manual-review policy.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReviewPolicy {
    #[schemars(schema_with = "required_true_schema")]
    teacher_approval_required_for_release: bool,
    force_manual_when: Vec<ManualReviewReason>,
}

impl ReviewPolicy {
    /// Reports the mandatory teacher approval requirement.
    ///
    /// This always returns true for a validated v1 document.
    #[must_use]
    pub const fn teacher_approval_required_for_release(&self) -> bool {
        self.teacher_approval_required_for_release
    }

    /// Returns conditions that force explicit manual review.
    #[must_use]
    pub fn force_manual_when(&self) -> &[ManualReviewReason] {
        &self.force_manual_when
    }
}

fn required_true_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "type": "boolean",
        "const": true
    })
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
/// Conditions that force manual teacher review.
pub enum ManualReviewReason {
    /// Runner or infrastructure execution failed independently of student work.
    InfrastructureError,
    /// Evidence is missing, malformed, or fails identity validation.
    InvalidEvidence,
}
