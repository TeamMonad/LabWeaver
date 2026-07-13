use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::sync::watch;
use tokio::time::timeout;

/// Security risk declared by an Agent Tool implementation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolRisk {
    /// Read-only or pure deterministic operation.
    Low,
    /// Operation requires explicit approval before dispatch.
    Elevated,
    /// High-impact operation requires explicit approval before dispatch.
    High,
}

impl ToolRisk {
    /// Reports whether an explicit per-tool approval is required.
    #[must_use]
    pub const fn requires_approval(self) -> bool {
        matches!(self, Self::Elevated | Self::High)
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Elevated => "elevated",
            Self::High => "high",
        }
    }
}

/// Registry retry policy fixed by the Tool binding.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToolRetryPolicy {
    /// Registry performs exactly one dispatch attempt and never retries side effects implicitly.
    Never,
}

/// Immutable identity and structured input/output contract of an Agent Tool.
#[derive(Clone, Debug)]
pub struct ToolDescriptor {
    name: String,
    version: String,
    risk: ToolRisk,
    input_schema: Value,
    output_schema_version: String,
    output_schema: Value,
    capability_sha256: String,
}

impl ToolDescriptor {
    /// Creates a Tool descriptor.
    ///
    /// # Errors
    ///
    /// Returns a stable diagnostic when name, version, or output schema version is empty.
    pub fn new(
        name: impl Into<String>,
        version: impl Into<String>,
        risk: ToolRisk,
        input_schema: Value,
        output_schema_version: impl Into<String>,
        output_schema: Value,
    ) -> Result<Self, AgentToolError> {
        let name = name.into();
        let version = version.into();
        let output_schema_version = output_schema_version.into();
        if name.trim().is_empty()
            || version.trim().is_empty()
            || output_schema_version.trim().is_empty()
        {
            return Err(AgentToolError::InvalidDescriptor {
                detail: "tool name, version, and output schema version are required".to_owned(),
            });
        }
        let capability_sha256 = capability_hash(
            &name,
            &version,
            risk,
            &input_schema,
            &output_schema_version,
            &output_schema,
        );
        Ok(Self {
            name,
            version,
            risk,
            input_schema,
            output_schema_version,
            output_schema,
            capability_sha256,
        })
    }

    /// Returns the stable Tool name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the exact bound Tool version, or `None` when no binding exists.
    #[must_use]
    pub fn version(&self) -> &str {
        &self.version
    }

    /// Returns the declared risk.
    #[must_use]
    pub const fn risk(&self) -> ToolRisk {
        self.risk
    }

    /// Returns the versioned JSON Schema for Tool input.
    #[must_use]
    pub const fn input_schema(&self) -> &Value {
        &self.input_schema
    }

    /// Returns the exact Tool output schema version.
    #[must_use]
    pub fn output_schema_version(&self) -> &str {
        &self.output_schema_version
    }

    /// Returns the versioned JSON Schema for Tool output payloads.
    #[must_use]
    pub const fn output_schema(&self) -> &Value {
        &self.output_schema
    }

    /// Returns the SHA-256 identity of the complete capability contract.
    #[must_use]
    pub fn capability_sha256(&self) -> &str {
        &self.capability_sha256
    }
}

/// Explicit manifest binding to one exact Tool capability and execution-control contract.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolBinding {
    name: String,
    version: String,
    risk: ToolRisk,
    capability_sha256: String,
    timeout_millis: u64,
    retry_policy: ToolRetryPolicy,
}

impl ToolBinding {
    /// Creates an exact Tool binding.
    ///
    /// # Errors
    ///
    /// Returns a stable diagnostic when identity fields or the capability SHA-256 are invalid.
    pub fn new(
        name: impl Into<String>,
        version: impl Into<String>,
        risk: ToolRisk,
        capability_sha256: impl Into<String>,
        timeout_millis: u64,
    ) -> Result<Self, AgentToolError> {
        let name = name.into();
        let version = version.into();
        let capability_sha256 = capability_sha256.into();
        if name.trim().is_empty()
            || version.trim().is_empty()
            || !is_sha256(&capability_sha256)
            || timeout_millis == 0
        {
            return Err(AgentToolError::InvalidBinding {
                detail: "binding name, version, risk, capability SHA-256, and non-zero timeout are required"
                    .to_owned(),
            });
        }
        Ok(Self {
            name,
            version,
            risk,
            capability_sha256,
            timeout_millis,
            retry_policy: ToolRetryPolicy::Never,
        })
    }

    /// Returns the bound Tool name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the exact bound version.
    #[must_use]
    pub fn version(&self) -> &str {
        &self.version
    }

    /// Returns the risk fixed by the binding owner.
    #[must_use]
    pub const fn risk(&self) -> ToolRisk {
        self.risk
    }

    /// Returns the expected complete capability identity.
    #[must_use]
    pub fn capability_sha256(&self) -> &str {
        &self.capability_sha256
    }

    /// Returns the maximum wall time for one dispatch attempt.
    #[must_use]
    pub const fn timeout_millis(&self) -> u64 {
        self.timeout_millis
    }

    /// Returns the fixed no-implicit-retry policy.
    #[must_use]
    pub const fn retry_policy(&self) -> ToolRetryPolicy {
        self.retry_policy
    }
}

/// Cooperative cancellation handle owned by the authoritative dispatch boundary.
#[derive(Clone, Debug)]
pub struct ToolCancellation {
    sender: watch::Sender<bool>,
}

impl ToolCancellation {
    /// Creates an active cancellation handle.
    #[must_use]
    pub fn new() -> Self {
        let (sender, _) = watch::channel(false);
        Self { sender }
    }

    /// Requests cancellation. Repeated cancellation is idempotent.
    pub fn cancel(&self) {
        self.sender.send_replace(true);
    }

    /// Reports whether cancellation was already requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        *self.sender.borrow()
    }

    async fn cancelled(&self) {
        let mut receiver = self.sender.subscribe();
        while !*receiver.borrow() {
            if receiver.changed().await.is_err() {
                return;
            }
        }
    }
}

impl Default for ToolCancellation {
    fn default() -> Self {
        Self::new()
    }
}

/// Audited call context supplied by the Agent owner rather than model text.
#[derive(Clone, Debug)]
pub struct AgentContext {
    run_id: String,
    actor_id: String,
    candidate_revision: String,
    idempotency_key: String,
    cancellation: ToolCancellation,
}

impl AgentContext {
    /// Creates a context with no elevated Tool approvals.
    ///
    /// # Errors
    ///
    /// Returns a stable diagnostic when any call identity field is empty.
    pub fn new(
        run_id: impl Into<String>,
        actor_id: impl Into<String>,
        candidate_revision: impl Into<String>,
        idempotency_key: impl Into<String>,
        cancellation: ToolCancellation,
    ) -> Result<Self, AgentToolError> {
        let run_id = run_id.into();
        let actor_id = actor_id.into();
        let candidate_revision = candidate_revision.into();
        let idempotency_key = idempotency_key.into();
        if run_id.trim().is_empty()
            || actor_id.trim().is_empty()
            || candidate_revision.trim().is_empty()
            || idempotency_key.trim().is_empty()
        {
            return Err(AgentToolError::InvalidContext);
        }
        Ok(Self {
            run_id,
            actor_id,
            candidate_revision,
            idempotency_key,
            cancellation,
        })
    }

    /// Returns the Agent run identity.
    #[must_use]
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    /// Returns the authenticated actor identity.
    #[must_use]
    pub fn actor_id(&self) -> &str {
        &self.actor_id
    }

    /// Returns the immutable candidate revision being validated.
    #[must_use]
    pub fn candidate_revision(&self) -> &str {
        &self.candidate_revision
    }

    /// Returns the caller-reserved idempotency identity for this dispatch.
    #[must_use]
    pub fn idempotency_key(&self) -> &str {
        &self.idempotency_key
    }

    /// Returns the cooperative cancellation handle.
    #[must_use]
    pub const fn cancellation(&self) -> &ToolCancellation {
        &self.cancellation
    }
}

/// Structured Tool output. It is never interpreted as a shell command.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ToolOutput {
    schema_version: String,
    payload: Value,
}

impl ToolOutput {
    /// Creates versioned structured output.
    ///
    /// # Errors
    ///
    /// Returns an implementation error when the schema version is empty. Registry owns the stable
    /// dispatch diagnostic.
    pub fn new(
        schema_version: impl Into<String>,
        payload: Value,
    ) -> Result<Self, ToolExecutionError> {
        let schema_version = schema_version.into();
        if schema_version.trim().is_empty() {
            return Err(ToolExecutionError::new(
                ToolExecutionFailureCode::InvalidOutput,
            ));
        }
        Ok(Self {
            schema_version,
            payload,
        })
    }

    /// Returns the Tool output schema version.
    #[must_use]
    pub fn schema_version(&self) -> &str {
        &self.schema_version
    }

    /// Returns the structured output payload.
    #[must_use]
    pub const fn payload(&self) -> &Value {
        &self.payload
    }
}

/// Payload-free result of one Tool dispatch attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToolAuditOutcome {
    /// Bound Tool completed and its output contract was validated.
    Succeeded,
    /// Registry policy rejected the attempt before implementation dispatch.
    Rejected,
    /// The implementation or its returned output contract failed.
    Failed,
    /// Dispatch was cancelled before a successful result.
    Cancelled,
}

/// Hash-only audit evidence for one Tool dispatch attempt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolAudit {
    run_id: String,
    actor_id: String,
    candidate_revision: String,
    idempotency_key: String,
    tool_name: String,
    tool_version: Option<String>,
    risk: Option<ToolRisk>,
    capability_sha256: Option<String>,
    input_sha256: String,
    output_sha256: Option<String>,
    outcome: ToolAuditOutcome,
    diagnostic_code: Option<String>,
    attempt: u8,
    timeout_millis: Option<u64>,
    retry_policy: Option<ToolRetryPolicy>,
}

impl ToolAudit {
    /// Returns the run identity.
    #[must_use]
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    /// Returns the actor identity.
    #[must_use]
    pub fn actor_id(&self) -> &str {
        &self.actor_id
    }

    /// Returns the immutable candidate revision attached to the attempt.
    #[must_use]
    pub fn candidate_revision(&self) -> &str {
        &self.candidate_revision
    }

    /// Returns the idempotency key reserved for the attempt.
    #[must_use]
    pub fn idempotency_key(&self) -> &str {
        &self.idempotency_key
    }

    /// Returns the exact Tool name.
    #[must_use]
    pub fn tool_name(&self) -> &str {
        &self.tool_name
    }

    /// Returns the exact Tool version.
    #[must_use]
    pub fn tool_version(&self) -> Option<&str> {
        self.tool_version.as_deref()
    }

    /// Returns the bound Tool risk, or `None` when no binding exists.
    #[must_use]
    pub const fn risk(&self) -> Option<ToolRisk> {
        self.risk
    }

    /// Returns the bound complete capability identity, or `None` for unbound attempts.
    #[must_use]
    pub fn capability_sha256(&self) -> Option<&str> {
        self.capability_sha256.as_deref()
    }

    /// Returns the SHA-256 of structured Tool input.
    #[must_use]
    pub fn input_sha256(&self) -> &str {
        &self.input_sha256
    }

    /// Returns the SHA-256 of structured Tool output when the implementation returned one.
    #[must_use]
    pub fn output_sha256(&self) -> Option<&str> {
        self.output_sha256.as_deref()
    }

    /// Returns the dispatch result without exposing input or output payloads.
    #[must_use]
    pub const fn outcome(&self) -> ToolAuditOutcome {
        self.outcome
    }

    /// Returns the stable rejection/failure diagnostic, if any.
    #[must_use]
    pub fn diagnostic_code(&self) -> Option<&str> {
        self.diagnostic_code.as_deref()
    }

    /// Returns the one-based attempt number. Registry never creates attempt two.
    #[must_use]
    pub const fn attempt(&self) -> u8 {
        self.attempt
    }

    /// Returns the bound timeout, or `None` when no binding exists.
    #[must_use]
    pub const fn timeout_millis(&self) -> Option<u64> {
        self.timeout_millis
    }

    /// Returns the bound retry policy, or `None` when no binding exists.
    #[must_use]
    pub const fn retry_policy(&self) -> Option<ToolRetryPolicy> {
        self.retry_policy
    }
}

/// Tool result and payload-free audit evidence.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolExecution {
    output: ToolOutput,
    audit: ToolAudit,
}

/// Failed or rejected Tool dispatch with payload-free audit evidence.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
#[error("{error}")]
pub struct ToolDispatchFailure {
    #[source]
    error: Box<AgentToolError>,
    audit: Box<ToolAudit>,
}

impl ToolDispatchFailure {
    /// Returns the typed root cause.
    #[must_use]
    pub fn error(&self) -> &AgentToolError {
        self.error.as_ref()
    }

    /// Returns payload-free evidence for the rejected or failed attempt.
    #[must_use]
    pub fn audit(&self) -> &ToolAudit {
        self.audit.as_ref()
    }

    /// Returns the stable machine-readable root-cause diagnostic.
    #[must_use]
    pub fn diagnostic_code(&self) -> &'static str {
        self.error.diagnostic_code()
    }
}

impl ToolExecution {
    /// Returns structured Tool output.
    #[must_use]
    pub const fn output(&self) -> &ToolOutput {
        &self.output
    }

    /// Returns hash-only audit evidence.
    #[must_use]
    pub const fn audit(&self) -> &ToolAudit {
        &self.audit
    }
}

/// Bounded, payload-free failure code available to Tool implementations.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Error, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolExecutionFailureCode {
    /// A bound provider is temporarily unavailable.
    #[error("provider_unavailable")]
    ProviderUnavailable,
    /// The controlled operation rejected the request.
    #[error("operation_rejected")]
    OperationRejected,
    /// The implementation could not construct its declared output.
    #[error("invalid_output")]
    InvalidOutput,
    /// An internal implementation failure occurred.
    #[error("internal")]
    Internal,
}

/// Failure reported by a Tool implementation using a bounded, payload-free code.
///
/// This type deliberately carries no Registry diagnostic. Registry owns approval, validation,
/// timeout, cancellation and audit semantics and maps every implementation failure to
/// `LW_AGENT_TOOL_EXECUTION_FAILED`.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
#[error("Tool implementation failed with code {code}")]
pub struct ToolExecutionError {
    code: ToolExecutionFailureCode,
}

impl ToolExecutionError {
    /// Creates a payload-free implementation failure.
    #[must_use]
    pub const fn new(code: ToolExecutionFailureCode) -> Self {
        Self { code }
    }

    /// Returns the bounded implementation failure code.
    #[must_use]
    pub const fn code(&self) -> ToolExecutionFailureCode {
        self.code
    }
}

/// Controlled implementation boundary for Agent Tools.
#[async_trait]
pub trait AgentTool: Send + Sync {
    /// Returns an immutable Tool descriptor.
    fn descriptor(&self) -> ToolDescriptor;

    /// Executes structured input without interpreting model text as shell.
    ///
    /// Implementations receive one immutable caller-supplied idempotency identity. Registry never
    /// retries implicitly, but durable reservation and result replay belong to Agent Service.
    /// Implementations must be cancellation-safe and must not detach untracked work that can outlive
    /// the returned future.
    ///
    /// # Errors
    ///
    /// Returns implementation-only failure detail without logging the input payload. Registry maps
    /// it to the single stable `LW_AGENT_TOOL_EXECUTION_FAILED` diagnostic.
    async fn execute(
        &self,
        context: &AgentContext,
        input: Value,
    ) -> Result<ToolOutput, ToolExecutionError>;
}

struct RegisteredTool {
    descriptor: ToolDescriptor,
    input_validator: jsonschema::Validator,
    output_validator: jsonschema::Validator,
    timeout_millis: u64,
    retry_policy: ToolRetryPolicy,
    implementation: Arc<dyn AgentTool>,
}

/// Immutable registry containing only explicit, exact Tool bindings.
pub struct ToolRegistry {
    tools: BTreeMap<String, RegisteredTool>,
}

impl ToolRegistry {
    /// Builds an immutable registry from implementations and exact manifest bindings.
    ///
    /// # Errors
    ///
    /// Rejects duplicate tools or bindings, missing implementations, capability conflicts, and
    /// invalid input/output schemas.
    pub fn new(
        implementations: Vec<Arc<dyn AgentTool>>,
        bindings: Vec<ToolBinding>,
    ) -> Result<Self, AgentToolError> {
        if bindings.is_empty() {
            return Err(AgentToolError::MissingBindings);
        }
        let mut available = BTreeMap::new();
        for implementation in implementations {
            let descriptor = implementation.descriptor();
            let name = descriptor.name().to_owned();
            if available
                .insert(name.clone(), (descriptor, implementation))
                .is_some()
            {
                return Err(AgentToolError::DuplicateTool { name });
            }
        }

        let mut tools = BTreeMap::new();
        let mut seen_bindings = BTreeSet::new();
        for binding in bindings {
            if !seen_bindings.insert(binding.name().to_owned()) {
                return Err(AgentToolError::DuplicateBinding {
                    name: binding.name().to_owned(),
                });
            }
            let (descriptor, implementation) =
                available.remove(binding.name()).ok_or_else(|| {
                    AgentToolError::MissingImplementation {
                        name: binding.name().to_owned(),
                        version: binding.version().to_owned(),
                    }
                })?;
            if descriptor.version() != binding.version() {
                return Err(AgentToolError::VersionMismatch {
                    name: binding.name().to_owned(),
                    expected: binding.version().to_owned(),
                    actual: descriptor.version().to_owned(),
                });
            }
            if descriptor.risk() != binding.risk() {
                return Err(AgentToolError::RiskMismatch {
                    name: binding.name().to_owned(),
                    expected: binding.risk(),
                    actual: descriptor.risk(),
                });
            }
            if descriptor.capability_sha256() != binding.capability_sha256() {
                return Err(AgentToolError::CapabilityMismatch {
                    name: binding.name().to_owned(),
                    expected: binding.capability_sha256().to_owned(),
                    actual: descriptor.capability_sha256().to_owned(),
                });
            }
            let input_validator =
                jsonschema::validator_for(descriptor.input_schema()).map_err(|error| {
                    AgentToolError::InvalidInputSchema {
                        name: descriptor.name().to_owned(),
                        detail: error.to_string(),
                    }
                })?;
            let output_validator =
                jsonschema::validator_for(descriptor.output_schema()).map_err(|error| {
                    AgentToolError::InvalidOutputSchema {
                        name: descriptor.name().to_owned(),
                        detail: error.to_string(),
                    }
                })?;
            tools.insert(
                binding.name().to_owned(),
                RegisteredTool {
                    descriptor,
                    input_validator,
                    output_validator,
                    timeout_millis: binding.timeout_millis(),
                    retry_policy: binding.retry_policy(),
                    implementation,
                },
            );
        }
        Ok(Self { tools })
    }

    /// Returns sorted explicitly bound Tool names.
    #[must_use]
    pub fn bound_tool_names(&self) -> Vec<&str> {
        self.tools.keys().map(String::as_str).collect()
    }

    /// Validates and dispatches one explicitly named Tool.
    ///
    /// # Errors
    ///
    /// Every rejection or failure returns payload-free audit evidence with the root-cause
    /// diagnostic. Unbound attempts cannot name a version, risk, or capability because no binding
    /// exists; all bound attempts include that exact identity.
    pub async fn execute(
        &self,
        name: &str,
        context: &AgentContext,
        input: Value,
    ) -> Result<ToolExecution, ToolDispatchFailure> {
        let input_sha256 = hash_json(&input);
        let Some(registered) = self.tools.get(name) else {
            return Err(dispatch_failure(
                AgentToolError::UnboundTool {
                    name: name.to_owned(),
                },
                context,
                name,
                None,
                &input_sha256,
                None,
                ToolAuditOutcome::Rejected,
            ));
        };
        Self::validate_dispatch(name, context, &input, &input_sha256, registered)?;
        let output = execute_once(name, context, input, &input_sha256, registered).await?;
        validate_output(name, context, output, input_sha256, registered)
    }

    fn validate_dispatch(
        name: &str,
        context: &AgentContext,
        input: &Value,
        input_sha256: &str,
        registered: &RegisteredTool,
    ) -> Result<(), ToolDispatchFailure> {
        if context.cancellation().is_cancelled() {
            return Err(bound_dispatch_failure(
                AgentToolError::DispatchCancelled {
                    name: name.to_owned(),
                },
                context,
                registered,
                input_sha256,
                None,
                ToolAuditOutcome::Cancelled,
            ));
        }
        if registered.descriptor.risk().requires_approval() {
            return Err(bound_dispatch_failure(
                AgentToolError::ApprovalRequired {
                    name: name.to_owned(),
                },
                context,
                registered,
                input_sha256,
                None,
                ToolAuditOutcome::Rejected,
            ));
        }
        if let Err(error) = registered.input_validator.validate(input) {
            return Err(bound_dispatch_failure(
                AgentToolError::InputRejected {
                    name: name.to_owned(),
                    detail: error.masked().to_string(),
                },
                context,
                registered,
                input_sha256,
                None,
                ToolAuditOutcome::Rejected,
            ));
        }
        Ok(())
    }
}

async fn execute_once(
    name: &str,
    context: &AgentContext,
    input: Value,
    input_sha256: &str,
    registered: &RegisteredTool,
) -> Result<ToolOutput, ToolDispatchFailure> {
    let execution = timeout(
        Duration::from_millis(registered.timeout_millis),
        registered.implementation.execute(context, input),
    );
    tokio::select! {
        biased;
        () = context.cancellation().cancelled() => Err(bound_dispatch_failure(
            AgentToolError::DispatchCancelled { name: name.to_owned() },
            context,
            registered,
            input_sha256,
            None,
            ToolAuditOutcome::Cancelled,
        )),
        result = execution => match result {
            Err(_) => Err(bound_dispatch_failure(
                AgentToolError::DispatchTimedOut {
                    name: name.to_owned(),
                    timeout_millis: registered.timeout_millis,
                },
                context,
                registered,
                input_sha256,
                None,
                ToolAuditOutcome::Failed,
            )),
            Ok(Err(error)) => Err(bound_dispatch_failure(
                AgentToolError::ExecutionFailed {
                    name: name.to_owned(),
                    code: error.code(),
                },
                context,
                registered,
                input_sha256,
                None,
                ToolAuditOutcome::Failed,
            )),
            Ok(Ok(output)) => Ok(output),
        }
    }
}

fn validate_output(
    name: &str,
    context: &AgentContext,
    output: ToolOutput,
    input_sha256: String,
    registered: &RegisteredTool,
) -> Result<ToolExecution, ToolDispatchFailure> {
    let output_sha256 = hash_tool_output(&output);
    if output.schema_version() != registered.descriptor.output_schema_version() {
        return Err(bound_dispatch_failure(
            AgentToolError::OutputVersionMismatch {
                name: name.to_owned(),
                expected: registered.descriptor.output_schema_version().to_owned(),
                actual: output.schema_version().to_owned(),
            },
            context,
            registered,
            &input_sha256,
            Some(output_sha256),
            ToolAuditOutcome::Failed,
        ));
    }
    if let Err(error) = registered.output_validator.validate(output.payload()) {
        return Err(bound_dispatch_failure(
            AgentToolError::OutputRejected {
                name: name.to_owned(),
                detail: error.masked().to_string(),
            },
            context,
            registered,
            &input_sha256,
            Some(output_sha256),
            ToolAuditOutcome::Failed,
        ));
    }
    Ok(ToolExecution {
        audit: successful_audit(context, registered, input_sha256, output_sha256),
        output,
    })
}

fn successful_audit(
    context: &AgentContext,
    registered: &RegisteredTool,
    input_sha256: String,
    output_sha256: String,
) -> ToolAudit {
    ToolAudit {
        run_id: context.run_id().to_owned(),
        actor_id: context.actor_id().to_owned(),
        candidate_revision: context.candidate_revision().to_owned(),
        idempotency_key: context.idempotency_key().to_owned(),
        tool_name: registered.descriptor.name().to_owned(),
        tool_version: Some(registered.descriptor.version().to_owned()),
        risk: Some(registered.descriptor.risk()),
        capability_sha256: Some(registered.descriptor.capability_sha256().to_owned()),
        input_sha256,
        output_sha256: Some(output_sha256),
        outcome: ToolAuditOutcome::Succeeded,
        diagnostic_code: None,
        attempt: 1,
        timeout_millis: Some(registered.timeout_millis),
        retry_policy: Some(registered.retry_policy),
    }
}

fn bound_dispatch_failure(
    error: AgentToolError,
    context: &AgentContext,
    registered: &RegisteredTool,
    input_sha256: &str,
    output_sha256: Option<String>,
    outcome: ToolAuditOutcome,
) -> ToolDispatchFailure {
    dispatch_failure(
        error,
        context,
        registered.descriptor.name(),
        Some(registered),
        input_sha256,
        output_sha256,
        outcome,
    )
}

fn dispatch_failure(
    error: AgentToolError,
    context: &AgentContext,
    tool_name: &str,
    registered: Option<&RegisteredTool>,
    input_sha256: &str,
    output_sha256: Option<String>,
    outcome: ToolAuditOutcome,
) -> ToolDispatchFailure {
    let diagnostic_code = error.diagnostic_code().to_owned();
    ToolDispatchFailure {
        error: Box::new(error),
        audit: Box::new(ToolAudit {
            run_id: context.run_id().to_owned(),
            actor_id: context.actor_id().to_owned(),
            candidate_revision: context.candidate_revision().to_owned(),
            idempotency_key: context.idempotency_key().to_owned(),
            tool_name: tool_name.to_owned(),
            tool_version: registered.map(|value| value.descriptor.version().to_owned()),
            risk: registered.map(|value| value.descriptor.risk()),
            capability_sha256: registered
                .map(|value| value.descriptor.capability_sha256().to_owned()),
            input_sha256: input_sha256.to_owned(),
            output_sha256,
            outcome,
            diagnostic_code: Some(diagnostic_code),
            attempt: 1,
            timeout_millis: registered.map(|value| value.timeout_millis),
            retry_policy: registered.map(|value| value.retry_policy),
        }),
    }
}

fn capability_hash(
    name: &str,
    version: &str,
    risk: ToolRisk,
    input_schema: &Value,
    output_schema_version: &str,
    output_schema: &Value,
) -> String {
    hash_json(&Value::Array(vec![
        Value::String(name.to_owned()),
        Value::String(version.to_owned()),
        Value::String(risk.as_str().to_owned()),
        input_schema.clone(),
        Value::String(output_schema_version.to_owned()),
        output_schema.clone(),
    ]))
}

fn hash_tool_output(output: &ToolOutput) -> String {
    hash_json(&Value::Array(vec![
        Value::String(output.schema_version().to_owned()),
        output.payload().clone(),
    ]))
}

fn hash_json(value: &Value) -> String {
    let mut hasher = Sha256::new();
    hash_json_value(&mut hasher, value);
    format!("{:x}", hasher.finalize())
}

fn hash_json_value(hasher: &mut Sha256, value: &Value) {
    match value {
        Value::Null => hasher.update(b"null"),
        Value::Bool(value) => {
            hasher.update(if *value {
                b"true".as_slice()
            } else {
                b"false".as_slice()
            });
        }
        Value::Number(value) => {
            hasher.update(b"number");
            hash_bytes(hasher, value.to_string().as_bytes());
        }
        Value::String(value) => {
            hasher.update(b"string");
            hash_bytes(hasher, value.as_bytes());
        }
        Value::Array(values) => {
            hasher.update(b"array");
            hash_length(hasher, values.len());
            for value in values {
                hash_json_value(hasher, value);
            }
        }
        Value::Object(values) => {
            hasher.update(b"object");
            hash_length(hasher, values.len());
            let mut keys = values.keys().collect::<Vec<_>>();
            keys.sort_unstable();
            for key in keys {
                hash_bytes(hasher, key.as_bytes());
                hash_json_value(hasher, &values[key]);
            }
        }
    }
}

fn hash_bytes(hasher: &mut Sha256, bytes: &[u8]) {
    hash_length(hasher, bytes.len());
    hasher.update(bytes);
}

fn hash_length(hasher: &mut Sha256, length: usize) {
    hasher.update(length.to_be_bytes());
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Stable fail-fast diagnostics for Tool registration and dispatch.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum AgentToolError {
    /// Tool descriptor identity is incomplete.
    #[error("invalid Tool descriptor: {detail}")]
    InvalidDescriptor {
        /// Safe validation detail.
        detail: String,
    },
    /// Manifest binding identity is incomplete.
    #[error("invalid Tool binding: {detail}")]
    InvalidBinding {
        /// Safe validation detail.
        detail: String,
    },
    /// Run or actor identity is incomplete.
    #[error("Agent Tool context requires run and actor identity")]
    InvalidContext,
    /// Registry construction omitted explicit Tool bindings.
    #[error("Agent Tool registry requires at least one explicit binding")]
    MissingBindings,
    /// More than one implementation claimed a Tool name.
    #[error("duplicate Tool implementation: {name}")]
    DuplicateTool {
        /// Conflicting Tool name.
        name: String,
    },
    /// The manifest bound a Tool more than once.
    #[error("duplicate Tool binding: {name}")]
    DuplicateBinding {
        /// Conflicting Tool name.
        name: String,
    },
    /// A binding has no installed implementation.
    #[error("missing Tool implementation: {name}@{version}")]
    MissingImplementation {
        /// Bound Tool name.
        name: String,
        /// Bound Tool version.
        version: String,
    },
    /// Installed implementation version differs from the exact binding.
    #[error("Tool version mismatch for {name}: expected {expected}, got {actual}")]
    VersionMismatch {
        /// Tool name.
        name: String,
        /// Bound version.
        expected: String,
        /// Installed version.
        actual: String,
    },
    /// Installed implementation risk differs from the permission fixed by the binding.
    #[error("Tool risk mismatch for {name}: expected {expected:?}, got {actual:?}")]
    RiskMismatch {
        /// Tool name.
        name: String,
        /// Risk fixed by the binding.
        expected: ToolRisk,
        /// Risk declared by the implementation.
        actual: ToolRisk,
    },
    /// Installed implementation changed a bound input/output capability contract.
    #[error("Tool capability mismatch for {name}: expected {expected}, got {actual}")]
    CapabilityMismatch {
        /// Tool name.
        name: String,
        /// SHA-256 fixed by the binding.
        expected: String,
        /// SHA-256 calculated from the implementation descriptor.
        actual: String,
    },
    /// Tool input schema cannot be compiled.
    #[error("invalid input schema for Tool {name}: {detail}")]
    InvalidInputSchema {
        /// Tool name.
        name: String,
        /// Safe schema failure.
        detail: String,
    },
    /// Tool output schema cannot be compiled.
    #[error("invalid output schema for Tool {name}: {detail}")]
    InvalidOutputSchema {
        /// Tool name.
        name: String,
        /// Safe schema failure.
        detail: String,
    },
    /// A call named a Tool absent from explicit bindings.
    #[error("unbound Agent Tool: {name}")]
    UnboundTool {
        /// Rejected Tool name.
        name: String,
    },
    /// Elevated Tool dispatch lacks approval.
    #[error("Agent Tool requires approval: {name}")]
    ApprovalRequired {
        /// Rejected Tool name.
        name: String,
    },
    /// Structured input failed the Tool schema.
    #[error("input rejected for Tool {name}: {detail}")]
    InputRejected {
        /// Tool name.
        name: String,
        /// Safe schema diagnostic.
        detail: String,
    },
    /// Dispatch exceeded the timeout fixed by its binding.
    #[error("Agent Tool dispatch timed out for {name} after {timeout_millis} ms")]
    DispatchTimedOut {
        /// Tool name.
        name: String,
        /// Bound timeout.
        timeout_millis: u64,
    },
    /// Dispatch was cancelled by the authoritative caller.
    #[error("Agent Tool dispatch cancelled for {name}")]
    DispatchCancelled {
        /// Tool name.
        name: String,
    },
    /// A Tool implementation returned a bounded failure code.
    #[error("LW_AGENT_TOOL_EXECUTION_FAILED: Agent Tool execution failed for {name}")]
    ExecutionFailed {
        /// Tool name.
        name: String,
        /// Payload-free implementation failure classification.
        code: ToolExecutionFailureCode,
    },
    /// Tool output version differs from the exact versioned output binding.
    #[error("Tool output version mismatch for {name}: expected {expected}, got {actual}")]
    OutputVersionMismatch {
        /// Tool name.
        name: String,
        /// Bound output schema version.
        expected: String,
        /// Returned output schema version.
        actual: String,
    },
    /// Tool output payload failed its bound JSON Schema.
    #[error("output rejected for Tool {name}: {detail}")]
    OutputRejected {
        /// Tool name.
        name: String,
        /// Safe schema diagnostic.
        detail: String,
    },
}

impl AgentToolError {
    /// Returns the stable machine-readable diagnostic code.
    #[must_use]
    pub const fn diagnostic_code(&self) -> &'static str {
        match self {
            Self::InvalidDescriptor { .. } => "LW_AGENT_TOOL_DESCRIPTOR_INVALID",
            Self::InvalidBinding { .. } => "LW_AGENT_TOOL_BINDING_INVALID",
            Self::InvalidContext => "LW_AGENT_TOOL_CONTEXT_INVALID",
            Self::MissingBindings => "LW_AGENT_TOOL_BINDING_MISSING",
            Self::DuplicateTool { .. } => "LW_AGENT_TOOL_DUPLICATE",
            Self::DuplicateBinding { .. } => "LW_AGENT_TOOL_BINDING_DUPLICATE",
            Self::MissingImplementation { .. } => "LW_AGENT_TOOL_IMPLEMENTATION_MISSING",
            Self::VersionMismatch { .. } => "LW_AGENT_TOOL_VERSION_MISMATCH",
            Self::RiskMismatch { .. } => "LW_AGENT_TOOL_RISK_MISMATCH",
            Self::CapabilityMismatch { .. } => "LW_AGENT_TOOL_CAPABILITY_MISMATCH",
            Self::InvalidInputSchema { .. } => "LW_AGENT_TOOL_INPUT_SCHEMA_INVALID",
            Self::InvalidOutputSchema { .. } => "LW_AGENT_TOOL_OUTPUT_SCHEMA_INVALID",
            Self::UnboundTool { .. } => "LW_AGENT_TOOL_UNBOUND",
            Self::ApprovalRequired { .. } => "LW_AGENT_TOOL_APPROVAL_REQUIRED",
            Self::InputRejected { .. } => "LW_AGENT_TOOL_INPUT_REJECTED",
            Self::DispatchTimedOut { .. } => "LW_AGENT_TOOL_TIMEOUT",
            Self::DispatchCancelled { .. } => "LW_AGENT_TOOL_CANCELLED",
            Self::ExecutionFailed { .. } => "LW_AGENT_TOOL_EXECUTION_FAILED",
            Self::OutputVersionMismatch { .. } => "LW_AGENT_TOOL_OUTPUT_VERSION_MISMATCH",
            Self::OutputRejected { .. } => "LW_AGENT_TOOL_OUTPUT_REJECTED",
        }
    }
}
