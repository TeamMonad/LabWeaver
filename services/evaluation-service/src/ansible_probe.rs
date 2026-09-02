//! Deterministic, payload-free Linux Ansible probe semantics used by the runner.
//!
//! This module is the pure data-semantics core of the read-only `ansible_probe`
//! Runner frozen in `contracts::evaluation::DeterministicRunnerSpec::AnsibleProbe`.
//! It performs no IO: the Kubernetes/SSH execution binding is a later stage.
#![allow(
    missing_docs,
    reason = "the internal runner wire is bounded by validation and stable diagnostics"
)]

use std::collections::{BTreeMap, BTreeSet};
use std::net::Ipv4Addr;

use contracts::evaluation::FactAssertion;
use persistence_sqlx::Sha256Digest; // internal persistence hash, not contract hash
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::oj::is_sha256_image;

pub const ANSIBLE_PROBE_EXECUTION_SCHEMA_VERSION: &str =
    "evaluation.labweaver.io/ansible-probe-execution/v1";
pub const ANSIBLE_PROBE_EVIDENCE_SCHEMA_VERSION: &str =
    "evaluation.labweaver.io/ansible-probe-evidence/v1";
pub const ANSIBLE_PROBE_EVIDENCE_RECEIPT_SCHEMA_VERSION: &str =
    "evaluation.labweaver.io/ansible-probe-evidence-receipt/v1";
/// Frozen v1 module allowlist; identical to the contract-level set.
pub const ALLOWED_PROBE_MODULES: [&str; 3] = [
    "ansible.builtin.package_facts",
    "ansible.builtin.service_facts",
    "ansible.builtin.stat",
];
pub const SSH_PORT: u16 = 22;
/// Probe wall time never outlives the short-lived SSH user certificate (<= 300 s).
pub const MAX_WALL_TIME_SECONDS: u64 = 300;
pub const MAX_FACTS_BYTES: u64 = 4 * 1024 * 1024;
pub const MAX_OUTPUT_BYTES: u64 = 1024 * 1024;
pub const MAX_ASSERTIONS: usize = 32;
pub const MAX_FACTS: usize = 256;
pub const MAX_FACT_STRING_BYTES: usize = 256;
pub const MAX_FACT_PATH_BYTES: usize = 1_024;
/// `file.` prefix plus the longest observed suffix around a bounded absolute path.
const MAX_FACT_NAME_BYTES: usize = MAX_FACT_PATH_BYTES + 16;
pub const MAX_EVIDENCE_BYTES: u64 = 1024 * 1024;
const MAX_PROFILE_BYTES: usize = 128;
const MAX_SECRET_NAME_BYTES: usize = 253;
const MAX_USERNAME_BYTES: usize = 64;

/// Typed, bounded fact set collected by the read-only probe.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct AnsibleProbeFacts {
    values: BTreeMap<String, ProbeFactValue>,
}

impl AnsibleProbeFacts {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Inserts one fact after structural and type validation.
    ///
    /// # Errors
    ///
    /// Returns [`AnsibleProbeError::FactsMalformed`] for an unknown fact family, a
    /// type or content mismatch, a duplicate name, or a bounds violation.
    pub fn insert(&mut self, name: &str, value: ProbeFactValue) -> Result<(), AnsibleProbeError> {
        if self.values.len() >= MAX_FACTS {
            return Err(AnsibleProbeError::FactsMalformed);
        }
        validate_fact(name, &value)?;
        if self.values.insert(name.to_owned(), value).is_some() {
            return Err(AnsibleProbeError::FactsMalformed);
        }
        Ok(())
    }

    /// Validates a deserialized fact set (counts, names, types, and content).
    ///
    /// # Errors
    ///
    /// Returns [`AnsibleProbeError::FactsMalformed`] for any malformed entry.
    pub fn validate(&self) -> Result<(), AnsibleProbeError> {
        if self.values.len() > MAX_FACTS {
            return Err(AnsibleProbeError::FactsMalformed);
        }
        for (name, value) in &self.values {
            validate_fact(name, value)?;
        }
        Ok(())
    }

    #[must_use]
    pub fn get(&self, name: &str) -> Option<&ProbeFactValue> {
        self.values.get(name)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.values.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &ProbeFactValue)> {
        self.values
            .iter()
            .map(|(name, value)| (name.as_str(), value))
    }
}

/// One typed fact value; numbers and objects are not representable.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(untagged)]
pub enum ProbeFactValue {
    Boolean(bool),
    Text(String),
}

impl ProbeFactValue {
    #[must_use]
    pub fn as_json(&self) -> serde_json::Value {
        match self {
            Self::Boolean(value) => serde_json::Value::Bool(*value),
            Self::Text(value) => serde_json::Value::String(value.clone()),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FactValueKind {
    Boolean,
    Text,
}

/// Parses a fact name into its family and required value kind.
///
/// Grammar (v1, read-only): `host.reachable`, `service.<name>.active|.state`,
/// `package.<name>.installed|.version`, `file.<absolute-path>.exists|.sha256|.mode`.
fn fact_value_kind(name: &str) -> Option<FactValueKind> {
    if name.is_empty() || name.len() > MAX_FACT_NAME_BYTES || name.chars().any(char::is_control) {
        return None;
    }
    if name == "host.reachable" {
        return Some(FactValueKind::Boolean);
    }
    for (prefix, suffix, kind) in [
        ("service.", ".active", FactValueKind::Boolean),
        ("service.", ".state", FactValueKind::Text),
        ("package.", ".installed", FactValueKind::Boolean),
        ("package.", ".version", FactValueKind::Text),
    ] {
        if let Some(rest) = name
            .strip_prefix(prefix)
            .and_then(|r| r.strip_suffix(suffix))
        {
            return is_safe_identifier(rest).then_some(kind);
        }
    }
    for (suffix, kind) in [
        (".exists", FactValueKind::Boolean),
        (".sha256", FactValueKind::Text),
        (".mode", FactValueKind::Text),
    ] {
        if let Some(path) = name
            .strip_prefix("file.")
            .and_then(|rest| rest.strip_suffix(suffix))
        {
            return is_safe_remote_path(path).then_some(kind);
        }
    }
    None
}

#[allow(
    clippy::case_sensitive_file_extension_comparisons,
    reason = "the suffix is the probe fact-name grammar, not a filesystem extension"
)]
fn validate_fact(name: &str, value: &ProbeFactValue) -> Result<(), AnsibleProbeError> {
    let kind = fact_value_kind(name).ok_or(AnsibleProbeError::FactsMalformed)?;
    let type_mismatched = matches!(
        (kind, value),
        (FactValueKind::Boolean, ProbeFactValue::Text(_))
            | (FactValueKind::Text, ProbeFactValue::Boolean(_))
    );
    if type_mismatched {
        return Err(AnsibleProbeError::FactsMalformed);
    }
    if let ProbeFactValue::Text(text) = value {
        if text.is_empty() || text.len() > MAX_FACT_STRING_BYTES {
            return Err(AnsibleProbeError::FactsMalformed);
        }
        if name.ends_with(".sha256") && !is_lower_hex_sha256(text) {
            return Err(AnsibleProbeError::FactsMalformed);
        }
        if name.ends_with(".mode")
            && (!(3..=4).contains(&text.len())
                || text.bytes().any(|byte| !(b'0'..=b'7').contains(&byte)))
        {
            return Err(AnsibleProbeError::FactsMalformed);
        }
    }
    Ok(())
}

fn is_lower_hex_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn is_safe_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 63
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        && value
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && value
            .as_bytes()
            .last()
            .is_some_and(u8::is_ascii_alphanumeric)
}

/// Observed remote paths are absolute, bounded, and free of traversal or control bytes.
fn is_safe_remote_path(value: &str) -> bool {
    value.len() > 1
        && value.len() <= MAX_FACT_PATH_BYTES
        && value.starts_with('/')
        && !value.ends_with('/')
        && !value.contains("//")
        && !value.chars().any(char::is_control)
        && value
            .split('/')
            .skip(1)
            .all(|component| !component.is_empty() && component != "." && component != "..")
}

fn is_secret_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_SECRET_NAME_BYTES
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'.')
        })
        && value
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && value
            .as_bytes()
            .last()
            .is_some_and(u8::is_ascii_alphanumeric)
}

/// SSH probe target; only private IPv4 on port 22 with a locked non-root account.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AnsibleProbeTarget {
    pub host: Ipv4Addr,
    pub port: u16,
    pub username: String,
}

/// Short-lived SSH identity references mounted from Kubernetes Secrets.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AnsibleProbeSshIdentity {
    pub private_key_secret: String,
    pub certificate_secret: String,
    pub expected_host_key_sha256: Sha256Digest,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AnsibleProbeExecutionLimits {
    pub wall_time_seconds: u64,
    pub facts_max_bytes: u64,
    pub output_max_bytes: u64,
    pub max_assertions: u32,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AnsibleProbeExecutionRequest {
    pub schema_version: String,
    pub run_id: Uuid,
    pub step_run_id: Uuid,
    pub attempt_id: Uuid,
    pub trace_id: String,
    pub runner_image_digest: String,
    pub playbook_profile: String,
    pub module_allowlist: Vec<String>,
    pub read_only: bool,
    pub assertions: Vec<FactAssertion>,
    pub target: AnsibleProbeTarget,
    pub ssh_identity: AnsibleProbeSshIdentity,
    pub limits: AnsibleProbeExecutionLimits,
    pub evaluation_spec_sha256: Sha256Digest,
}

impl AnsibleProbeExecutionRequest {
    /// Validates the complete immutable execution request.
    ///
    /// # Errors
    ///
    /// Returns a stable [`AnsibleProbeError`] when an identity, target, profile,
    /// allowlist, assertion set, limit, or SSH identity reference is invalid.
    pub fn validate(&self) -> Result<(), AnsibleProbeError> {
        if self.schema_version != ANSIBLE_PROBE_EXECUTION_SCHEMA_VERSION {
            return Err(AnsibleProbeError::SchemaVersionInvalid);
        }
        if [self.run_id, self.step_run_id, self.attempt_id]
            .iter()
            .any(|identity| identity.get_version_num() != 7)
        {
            return Err(AnsibleProbeError::IdentityInvalid);
        }
        if self.trace_id.is_empty()
            || self.trace_id.len() > 128
            || self.trace_id.chars().any(char::is_control)
        {
            return Err(AnsibleProbeError::IdentityInvalid);
        }
        if !is_sha256_image(&self.runner_image_digest) {
            return Err(AnsibleProbeError::ImageIdentityInvalid);
        }
        if self.playbook_profile.trim().is_empty()
            || self.playbook_profile.len() > MAX_PROFILE_BYTES
            || self.playbook_profile.chars().any(char::is_control)
        {
            return Err(AnsibleProbeError::ProfileInvalid);
        }
        if !self.read_only {
            return Err(AnsibleProbeError::ReadOnlyRequired);
        }
        if self.module_allowlist.is_empty()
            || self.module_allowlist.len() > ALLOWED_PROBE_MODULES.len()
        {
            return Err(AnsibleProbeError::ModuleNotAllowed);
        }
        let mut modules = BTreeSet::new();
        for module in &self.module_allowlist {
            if !ALLOWED_PROBE_MODULES.contains(&module.as_str()) || !modules.insert(module.as_str())
            {
                return Err(AnsibleProbeError::ModuleNotAllowed);
            }
        }
        if self.assertions.is_empty() || self.assertions.len() > MAX_ASSERTIONS {
            return Err(AnsibleProbeError::AssertionsInvalid);
        }
        let mut facts = BTreeSet::new();
        for assertion in &self.assertions {
            if !is_valid_assertion(assertion) || !facts.insert(assertion.fact()) {
                return Err(AnsibleProbeError::AssertionsInvalid);
            }
        }
        validate_limits(&self.limits, self.assertions.len())?;
        validate_target(&self.target)?;
        if !is_secret_name(&self.ssh_identity.private_key_secret)
            || !is_secret_name(&self.ssh_identity.certificate_secret)
        {
            return Err(AnsibleProbeError::SshIdentityInvalid);
        }
        Ok(())
    }

    /// Computes the canonical request identity after validation.
    ///
    /// # Errors
    ///
    /// Returns a stable [`AnsibleProbeError`] when validation or canonical
    /// serialization fails.
    pub fn request_sha256(&self) -> Result<Sha256Digest, AnsibleProbeError> {
        self.validate()?;
        Sha256Digest::of_canonical(self).map_err(|_| AnsibleProbeError::EvidenceInvalid)
    }
}

/// An assertion is well-formed when its fact name parses and the expected JSON
/// type matches the fact family's value kind.
fn is_valid_assertion(assertion: &FactAssertion) -> bool {
    let Some(kind) = fact_value_kind(assertion.fact()) else {
        return false;
    };
    matches!(
        (kind, assertion.expected()),
        (FactValueKind::Boolean, serde_json::Value::Bool(_))
            | (FactValueKind::Text, serde_json::Value::String(_))
    )
}

fn validate_limits(
    limits: &AnsibleProbeExecutionLimits,
    assertions: usize,
) -> Result<(), AnsibleProbeError> {
    let max_assertions =
        usize::try_from(limits.max_assertions).map_err(|_| AnsibleProbeError::LimitInvalid)?;
    if limits.wall_time_seconds == 0
        || limits.wall_time_seconds > MAX_WALL_TIME_SECONDS
        || limits.facts_max_bytes == 0
        || limits.facts_max_bytes > MAX_FACTS_BYTES
        || limits.output_max_bytes == 0
        || limits.output_max_bytes > MAX_OUTPUT_BYTES
        || limits.max_assertions == 0
        || max_assertions > MAX_ASSERTIONS
        || assertions > max_assertions
    {
        return Err(AnsibleProbeError::LimitInvalid);
    }
    Ok(())
}

fn validate_target(target: &AnsibleProbeTarget) -> Result<(), AnsibleProbeError> {
    // RFC 1918 only: 10/8, 172.16/12, 192.168/16. Loopback and link-local are
    // not valid probe targets for an environment VM.
    if !target.host.is_private() {
        return Err(AnsibleProbeError::TargetNotPrivate);
    }
    if target.port != SSH_PORT {
        return Err(AnsibleProbeError::PortInvalid);
    }
    let username = target.username.as_str();
    if username.is_empty()
        || username.len() > MAX_USERNAME_BYTES
        || username == "root"
        || !username
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(AnsibleProbeError::UsernameInvalid);
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AnsibleProbeAssertionStatus {
    Passed,
    Failed,
    FactUnknown,
    FactTypeMismatch,
}

impl AnsibleProbeAssertionStatus {
    #[must_use]
    pub const fn diagnostic_code(self) -> &'static str {
        match self {
            Self::Passed => "LW_AP_ASSERTION_PASSED",
            Self::Failed => "LW_AP_ASSERTION_FAILED",
            Self::FactUnknown => "LW_AP_FACT_UNKNOWN",
            Self::FactTypeMismatch => "LW_AP_FACT_TYPE_MISMATCH",
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AnsibleProbeAssertionResult {
    pub fact: String,
    pub expected: serde_json::Value,
    pub observed: Option<serde_json::Value>,
    pub status: AnsibleProbeAssertionStatus,
    pub passed: bool,
    pub diagnostic_code: String,
}

/// Deterministically evaluates every requested assertion against the typed facts.
///
/// Fail-closed: an unknown fact name or a value whose JSON type differs from the
/// expectation never passes. Assertion order is preserved.
#[must_use]
pub fn evaluate_assertions(
    facts: &AnsibleProbeFacts,
    assertions: &[FactAssertion],
) -> Vec<AnsibleProbeAssertionResult> {
    assertions
        .iter()
        .map(|assertion| {
            let expected = assertion.expected().clone();
            let (status, observed) = match facts.get(assertion.fact()) {
                None => (AnsibleProbeAssertionStatus::FactUnknown, None),
                Some(value) => {
                    let observed = value.as_json();
                    if same_json_type(&observed, &expected) {
                        if observed == expected {
                            (AnsibleProbeAssertionStatus::Passed, Some(observed))
                        } else {
                            (AnsibleProbeAssertionStatus::Failed, Some(observed))
                        }
                    } else {
                        (
                            AnsibleProbeAssertionStatus::FactTypeMismatch,
                            Some(observed),
                        )
                    }
                }
            };
            AnsibleProbeAssertionResult {
                fact: assertion.fact().to_owned(),
                expected,
                observed,
                status,
                passed: status == AnsibleProbeAssertionStatus::Passed,
                diagnostic_code: status.diagnostic_code().to_owned(),
            }
        })
        .collect()
}

fn same_json_type(left: &serde_json::Value, right: &serde_json::Value) -> bool {
    matches!(
        (left, right),
        (serde_json::Value::Bool(_), serde_json::Value::Bool(_))
            | (serde_json::Value::String(_), serde_json::Value::String(_))
    )
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AnsibleProbeTerminalStatus {
    Succeeded,
    AssertionsFailed,
    GrantMissing,
    ModuleNotAllowed,
    HostUnreachable,
    Timeout,
    IdentityExpired,
    OutputExceeded,
    FactsMalformed,
    HostKeyMismatch,
    TargetNotPrivate,
    Cancelled,
    InfrastructureError,
}

impl AnsibleProbeTerminalStatus {
    #[must_use]
    pub const fn diagnostic_code(self) -> &'static str {
        match self {
            Self::Succeeded => "LW_AP_SUCCEEDED",
            Self::AssertionsFailed => "LW_AP_ASSERTION_FAILED",
            Self::GrantMissing => "LW_AP_GRANT_MISSING",
            Self::ModuleNotAllowed => "LW_AP_MODULE_NOT_ALLOWED",
            Self::HostUnreachable => "LW_AP_HOST_UNREACHABLE",
            Self::Timeout => "LW_AP_TIMEOUT",
            Self::IdentityExpired => "LW_AP_IDENTITY_EXPIRED",
            Self::OutputExceeded => "LW_AP_OUTPUT_LIMIT",
            Self::FactsMalformed => "LW_AP_FACTS_MALFORMED",
            Self::HostKeyMismatch => "LW_AP_HOST_KEY_MISMATCH",
            Self::TargetNotPrivate => "LW_AP_TARGET_NOT_PRIVATE",
            Self::Cancelled => "LW_AP_CANCELLED",
            Self::InfrastructureError => "LW_AP_INFRASTRUCTURE_ERROR",
        }
    }

    /// Returns the terminal status for a fully evaluated assertion set.
    #[must_use]
    pub fn for_assertions(results: &[AnsibleProbeAssertionResult]) -> Self {
        if results.iter().all(|result| result.passed) {
            Self::Succeeded
        } else {
            Self::AssertionsFailed
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AnsibleProbeEvidence {
    pub schema_version: String,
    pub run_id: Uuid,
    pub step_run_id: Uuid,
    pub attempt_id: Uuid,
    pub trace_id: String,
    pub request_sha256: Sha256Digest,
    pub evaluation_spec_sha256: Sha256Digest,
    pub playbook_profile: String,
    pub runner_image_digest: String,
    pub terminal_status: AnsibleProbeTerminalStatus,
    pub diagnostic_code: String,
    pub facts: AnsibleProbeFacts,
    pub assertion_results: Vec<AnsibleProbeAssertionResult>,
    pub duration_milliseconds: u64,
    pub facts_bytes: u64,
    pub output_bytes: u64,
}

impl AnsibleProbeEvidence {
    /// Verifies that full evidence is complete and bound to `request`.
    ///
    /// # Errors
    ///
    /// Returns [`AnsibleProbeError::EvidenceInvalid`] for missing, duplicated,
    /// forged, malformed, or mismatched data.
    pub fn validate_for(
        &self,
        request: &AnsibleProbeExecutionRequest,
    ) -> Result<(), AnsibleProbeError> {
        request.validate()?;
        self.facts.validate()?;
        if self.schema_version != ANSIBLE_PROBE_EVIDENCE_SCHEMA_VERSION
            || self.run_id != request.run_id
            || self.step_run_id != request.step_run_id
            || self.attempt_id != request.attempt_id
            || self.trace_id != request.trace_id
            || self.request_sha256 != request.request_sha256()?
            || self.evaluation_spec_sha256 != request.evaluation_spec_sha256
            || self.playbook_profile != request.playbook_profile
            || self.runner_image_digest != request.runner_image_digest
            || self.diagnostic_code != self.terminal_status.diagnostic_code()
            || self.facts_bytes > request.limits.facts_max_bytes
            || self.output_bytes > request.limits.output_max_bytes
            || self.duration_milliseconds > request.limits.wall_time_seconds * 1_000
        {
            return Err(AnsibleProbeError::EvidenceInvalid);
        }
        if evaluate_assertions(&self.facts, &request.assertions) != self.assertion_results {
            return Err(AnsibleProbeError::EvidenceInvalid);
        }
        match self.terminal_status {
            AnsibleProbeTerminalStatus::Succeeded
            | AnsibleProbeTerminalStatus::AssertionsFailed => {
                if self.terminal_status
                    != AnsibleProbeTerminalStatus::for_assertions(&self.assertion_results)
                {
                    return Err(AnsibleProbeError::EvidenceInvalid);
                }
            }
            // Execution never produced trustworthy facts; every assertion must
            // fail closed as unknown instead of being reported as observed.
            AnsibleProbeTerminalStatus::GrantMissing
            | AnsibleProbeTerminalStatus::ModuleNotAllowed
            | AnsibleProbeTerminalStatus::HostUnreachable
            | AnsibleProbeTerminalStatus::Timeout
            | AnsibleProbeTerminalStatus::IdentityExpired
            | AnsibleProbeTerminalStatus::OutputExceeded
            | AnsibleProbeTerminalStatus::FactsMalformed
            | AnsibleProbeTerminalStatus::HostKeyMismatch
            | AnsibleProbeTerminalStatus::TargetNotPrivate
            | AnsibleProbeTerminalStatus::Cancelled
            | AnsibleProbeTerminalStatus::InfrastructureError => {
                if !self.facts.is_empty()
                    || self
                        .assertion_results
                        .iter()
                        .any(|result| result.status != AnsibleProbeAssertionStatus::FactUnknown)
                {
                    return Err(AnsibleProbeError::EvidenceInvalid);
                }
            }
        }
        Ok(())
    }
}

/// Payload-free termination-message receipt bound to the full evidence identity.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AnsibleProbeEvidenceReceipt {
    pub schema_version: String,
    pub run_id: Uuid,
    pub step_run_id: Uuid,
    pub attempt_id: Uuid,
    pub trace_id: String,
    pub request_sha256: Sha256Digest,
    pub evidence_sha256: Sha256Digest,
    pub evidence_size_bytes: u64,
    pub terminal_status: AnsibleProbeTerminalStatus,
    pub diagnostic_code: String,
    pub passed_assertions: u32,
    pub total_assertions: u32,
}

impl AnsibleProbeEvidenceReceipt {
    /// Verifies that the payload-free termination receipt is bound to `request`.
    ///
    /// # Errors
    ///
    /// Returns [`AnsibleProbeError::EvidenceInvalid`] for an invalid schema,
    /// identity, digest, size, or assertion count.
    pub fn validate_for(
        &self,
        request: &AnsibleProbeExecutionRequest,
    ) -> Result<(), AnsibleProbeError> {
        request.validate()?;
        let total_assertions = u32::try_from(request.assertions.len())
            .map_err(|_| AnsibleProbeError::EvidenceInvalid)?;
        if self.schema_version != ANSIBLE_PROBE_EVIDENCE_RECEIPT_SCHEMA_VERSION
            || self.run_id != request.run_id
            || self.step_run_id != request.step_run_id
            || self.attempt_id != request.attempt_id
            || self.trace_id != request.trace_id
            || self.request_sha256 != request.request_sha256()?
            || self.evidence_size_bytes == 0
            || self.evidence_size_bytes > MAX_EVIDENCE_BYTES
            || self.diagnostic_code != self.terminal_status.diagnostic_code()
            || self.passed_assertions > self.total_assertions
            || self.total_assertions != total_assertions
        {
            return Err(AnsibleProbeError::EvidenceInvalid);
        }
        Ok(())
    }
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum AnsibleProbeError {
    #[error("ansible probe execution schema version is unsupported")]
    SchemaVersionInvalid,
    #[error("ansible probe execution identity must use UUIDv7")]
    IdentityInvalid,
    #[error("ansible probe runner image is not digest-pinned")]
    ImageIdentityInvalid,
    #[error("ansible probe playbook profile is invalid")]
    ProfileInvalid,
    #[error("ansible probe v1 must be read-only")]
    ReadOnlyRequired,
    #[error("ansible probe module is outside the frozen v1 allowlist")]
    ModuleNotAllowed,
    #[error("ansible probe assertion set is invalid")]
    AssertionsInvalid,
    #[error("ansible probe execution limit is invalid")]
    LimitInvalid,
    #[error("ansible probe target must be a private IPv4 address")]
    TargetNotPrivate,
    #[error("ansible probe SSH port must be 22")]
    PortInvalid,
    #[error("ansible probe username must be a locked non-root account")]
    UsernameInvalid,
    #[error("ansible probe SSH identity reference is invalid")]
    SshIdentityInvalid,
    #[error("ansible probe facts are malformed")]
    FactsMalformed,
    #[error("ansible probe evidence is incomplete or inconsistent")]
    EvidenceInvalid,
}

impl AnsibleProbeError {
    #[must_use]
    pub const fn diagnostic_code(&self) -> &'static str {
        match self {
            Self::SchemaVersionInvalid => "LW_AP_SCHEMA_VERSION_INVALID",
            Self::IdentityInvalid => "LW_AP_IDENTITY_INVALID",
            Self::ImageIdentityInvalid => "LW_AP_IMAGE_IDENTITY_INVALID",
            Self::ProfileInvalid => "LW_AP_PROFILE_INVALID",
            Self::ReadOnlyRequired => "LW_AP_READ_ONLY_REQUIRED",
            Self::ModuleNotAllowed => "LW_AP_MODULE_NOT_ALLOWED",
            Self::AssertionsInvalid => "LW_AP_ASSERTIONS_INVALID",
            Self::LimitInvalid => "LW_AP_LIMIT_INVALID",
            Self::TargetNotPrivate => "LW_AP_TARGET_NOT_PRIVATE",
            Self::PortInvalid => "LW_AP_PORT_INVALID",
            Self::UsernameInvalid => "LW_AP_USERNAME_INVALID",
            Self::SshIdentityInvalid => "LW_AP_SSH_IDENTITY_INVALID",
            Self::FactsMalformed => "LW_AP_FACTS_MALFORMED",
            Self::EvidenceInvalid => "LW_AP_EVIDENCE_INVALID",
        }
    }
}
