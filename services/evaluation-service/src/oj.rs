//! Deterministic, payload-free OJ execution semantics used by the Kubernetes runner.
#![allow(
    missing_docs,
    reason = "the internal runner wire is bounded by validation and stable diagnostics"
)]

use std::collections::{BTreeMap, BTreeSet};

use contracts::;
use crate::hash_compat::Sha256Digest;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

pub const OJ_EXECUTION_SCHEMA_VERSION: &str = "evaluation.labweaver.io/oj-execution/v1";
pub const OJ_EVIDENCE_SCHEMA_VERSION: &str = "evaluation.labweaver.io/oj-evidence/v1";
pub const OJ_EVIDENCE_RECEIPT_SCHEMA_VERSION: &str =
    "evaluation.labweaver.io/oj-evidence-receipt/v1";
pub const APPROVED_CPP17_PROFILE: &str = "cpp17-approved-v1";
pub const MAX_OJ_CASES: usize = 64;
pub const MAX_COMPILE_WALL_MILLISECONDS: u64 = 120_000;
pub const MAX_RUN_WALL_MILLISECONDS: u64 = 30_000;
pub const MAX_CPU_MILLISECONDS: u64 = 30_000;
pub const MIN_MEMORY_BYTES: u64 = 16 * 1024 * 1024;
pub const MAX_MEMORY_BYTES: u64 = 2 * 1024 * 1024 * 1024;
pub const MAX_OUTPUT_BYTES: u64 = 16 * 1024 * 1024;
pub const MAX_BOUND_FILE_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OjCheckerKind {
    Exact,
    Token,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OjExecutionPhase {
    Compile,
    Test,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OjFileBinding {
    pub path: String,
    pub sha256: Sha256Digest,
    pub size_bytes: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OjCaseBinding {
    pub id: String,
    pub input: OjFileBinding,
    pub expected: OjFileBinding,
    pub max_points: u32,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OjExecutionLimits {
    pub compile_wall_milliseconds: u64,
    pub run_wall_milliseconds: u64,
    pub cpu_milliseconds: u64,
    pub memory_bytes: u64,
    pub output_bytes: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OjExecutionRequest {
    pub schema_version: String,
    pub run_id: Uuid,
    pub step_run_id: Uuid,
    pub attempt_id: Uuid,
    pub trace_id: String,
    pub toolchain_profile: String,
    pub toolchain_image_digest: String,
    pub submission_identity: Sha256Digest,
    pub evaluator_identity: Option<Sha256Digest>,
    pub source: OjFileBinding,
    pub phase: OjExecutionPhase,
    pub checker: Option<OjCheckerKind>,
    pub cases: Vec<OjCaseBinding>,
    pub score_max_points: u32,
    pub limits: OjExecutionLimits,
}

impl OjExecutionRequest {
    /// Validates the complete immutable execution request.
    ///
    /// # Errors
    ///
    /// Returns a stable [`OjError`] when an identity, binding, limit, or profile is invalid.
    pub fn validate(&self) -> Result<(), OjError> {
        if self.schema_version != OJ_EXECUTION_SCHEMA_VERSION {
            return Err(OjError::SchemaVersionInvalid);
        }
        if [self.run_id, self.step_run_id, self.attempt_id]
            .iter()
            .any(|identity| identity.get_version_num() != 7)
        {
            return Err(OjError::IdentityInvalid);
        }
        if self.trace_id.is_empty()
            || self.trace_id.len() > 128
            || self.trace_id.chars().any(char::is_control)
        {
            return Err(OjError::IdentityInvalid);
        }
        if self.toolchain_profile != APPROVED_CPP17_PROFILE {
            return Err(OjError::ToolchainUnapproved);
        }
        if !is_sha256_digest(&self.toolchain_image_digest) {
            return Err(OjError::ImageIdentityInvalid);
        }
        validate_file(&self.source, false)?;
        match self.phase {
            OjExecutionPhase::Compile
                if self.checker.is_some()
                    || !self.cases.is_empty()
                    || self.evaluator_identity.is_some()
                    || self.score_max_points != 0 =>
            {
                return Err(OjError::ExecutionPlanInvalid);
            }
            OjExecutionPhase::Test
                if self.checker.is_none()
                    || self.evaluator_identity.is_none()
                    || self.cases.is_empty()
                    || self.cases.len() > MAX_OJ_CASES
                    || self.score_max_points == 0 =>
            {
                return Err(OjError::ExecutionPlanInvalid);
            }
            OjExecutionPhase::Compile | OjExecutionPhase::Test => {}
        }
        validate_limits(self.limits)?;

        let mut case_ids = BTreeSet::new();
        let mut score = 0_u32;
        for case in &self.cases {
            if !is_safe_identifier(&case.id) || !case_ids.insert(case.id.as_str()) {
                return Err(OjError::CaseSetInvalid);
            }
            validate_file(&case.input, true)?;
            validate_file(&case.expected, true)?;
            if case.max_points == 0 {
                return Err(OjError::CaseSetInvalid);
            }
            score = score
                .checked_add(case.max_points)
                .ok_or(OjError::ScoreOverflow)?;
        }
        if self.phase == OjExecutionPhase::Test && score == 0 {
            return Err(OjError::CaseSetInvalid);
        }
        Ok(())
    }

    /// Computes the canonical request identity after validation.
    ///
    /// # Errors
    ///
    /// Returns a stable [`OjError`] when validation or canonical serialization fails.
    pub fn request_sha256(&self) -> Result<Sha256Digest, OjError> {
        self.validate()?;
        Sha256Digest::of_canonical(self).map_err(|_| OjError::EvidenceInvalid)
    }

    #[must_use]
    pub fn case_max_points(&self) -> Option<u32> {
        self.cases
            .iter()
            .try_fold(0_u32, |total, case| total.checked_add(case.max_points))
    }
}

fn validate_file(binding: &OjFileBinding, empty_allowed: bool) -> Result<(), OjError> {
    if !is_safe_relative_path(&binding.path)
        || binding.size_bytes > MAX_BOUND_FILE_BYTES
        || (!empty_allowed && binding.size_bytes == 0)
    {
        return Err(if is_safe_relative_path(&binding.path) {
            OjError::FileBindingInvalid
        } else {
            OjError::PathUnsafe
        });
    }
    Ok(())
}

fn validate_limits(limits: OjExecutionLimits) -> Result<(), OjError> {
    if limits.compile_wall_milliseconds == 0
        || limits.compile_wall_milliseconds > MAX_COMPILE_WALL_MILLISECONDS
        || limits.run_wall_milliseconds == 0
        || limits.run_wall_milliseconds > MAX_RUN_WALL_MILLISECONDS
        || limits.cpu_milliseconds == 0
        || limits.cpu_milliseconds > MAX_CPU_MILLISECONDS
        || limits.cpu_milliseconds > limits.run_wall_milliseconds
        || limits.memory_bytes < MIN_MEMORY_BYTES
        || limits.memory_bytes > MAX_MEMORY_BYTES
        || limits.output_bytes == 0
        || limits.output_bytes > MAX_OUTPUT_BYTES
    {
        return Err(OjError::LimitInvalid);
    }
    Ok(())
}

pub(crate) fn is_sha256_image(value: &str) -> bool {
    let Some((name, digest)) = value.rsplit_once("@sha256:") else {
        return false;
    };
    !name.trim().is_empty()
        && !name.contains(char::is_whitespace)
        && digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn is_sha256_digest(value: &str) -> bool {
    value.len() == 71
        && value.starts_with("sha256:")
        && value[7..]
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

pub(crate) fn is_safe_relative_path(value: &str) -> bool {
    !value.is_empty()
        && value == value.trim()
        && !value.starts_with('/')
        && !value.contains('\\')
        && !value
            .chars()
            .any(|character| matches!(character, '*' | '?' | '[' | ']' | '{' | '}'))
        && value
            .split('/')
            .all(|component| !component.is_empty() && component != "." && component != "..")
}

#[must_use]
pub fn check_output(checker: OjCheckerKind, actual: &[u8], expected: &[u8]) -> bool {
    match checker {
        OjCheckerKind::Exact => actual == expected,
        OjCheckerKind::Token => ascii_tokens(actual).eq(ascii_tokens(expected)),
    }
}

fn ascii_tokens(bytes: &[u8]) -> impl Iterator<Item = &[u8]> {
    bytes
        .split(u8::is_ascii_whitespace)
        .filter(|token| !token.is_empty())
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OjCaseStatus {
    Accepted,
    WrongAnswer,
    TimeLimitExceeded,
    MemoryLimitExceeded,
    OutputLimitExceeded,
    RuntimeError,
}

impl OjCaseStatus {
    #[must_use]
    pub const fn diagnostic_code(self) -> &'static str {
        match self {
            Self::Accepted => "LW_OJ_ACCEPTED",
            Self::WrongAnswer => "LW_OJ_WRONG_ANSWER",
            Self::TimeLimitExceeded => "LW_OJ_TIME_LIMIT",
            Self::MemoryLimitExceeded => "LW_OJ_MEMORY_LIMIT",
            Self::OutputLimitExceeded => "LW_OJ_OUTPUT_LIMIT",
            Self::RuntimeError => "LW_OJ_RUNTIME_ERROR",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OjTerminalStatus {
    Accepted,
    CompileError,
    WrongAnswer,
    TimeLimitExceeded,
    MemoryLimitExceeded,
    OutputLimitExceeded,
    RuntimeError,
    Cancelled,
    InfrastructureError,
}

impl OjTerminalStatus {
    #[must_use]
    pub const fn diagnostic_code(self) -> &'static str {
        match self {
            Self::Accepted => "LW_OJ_ACCEPTED",
            Self::CompileError => "LW_OJ_COMPILE_ERROR",
            Self::WrongAnswer => "LW_OJ_WRONG_ANSWER",
            Self::TimeLimitExceeded => "LW_OJ_TIME_LIMIT",
            Self::MemoryLimitExceeded => "LW_OJ_MEMORY_LIMIT",
            Self::OutputLimitExceeded => "LW_OJ_OUTPUT_LIMIT",
            Self::RuntimeError => "LW_OJ_RUNTIME_ERROR",
            Self::Cancelled => "LW_OJ_CANCELLED",
            Self::InfrastructureError => "LW_OJ_INFRASTRUCTURE_ERROR",
        }
    }
}

impl From<OjCaseStatus> for OjTerminalStatus {
    fn from(value: OjCaseStatus) -> Self {
        match value {
            OjCaseStatus::Accepted => Self::Accepted,
            OjCaseStatus::WrongAnswer => Self::WrongAnswer,
            OjCaseStatus::TimeLimitExceeded => Self::TimeLimitExceeded,
            OjCaseStatus::MemoryLimitExceeded => Self::MemoryLimitExceeded,
            OjCaseStatus::OutputLimitExceeded => Self::OutputLimitExceeded,
            OjCaseStatus::RuntimeError => Self::RuntimeError,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OjCaseEvidence {
    pub case_id: String,
    pub status: OjCaseStatus,
    pub actual_output_sha256: Sha256Digest,
    pub stdout_bytes: u64,
    pub stderr_sha256: Sha256Digest,
    pub stderr_bytes: u64,
    pub duration_milliseconds: u64,
    pub peak_memory_bytes: Option<u64>,
    pub awarded_points: u32,
    pub diagnostic_code: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OjAggregate {
    pub status: OjTerminalStatus,
    pub awarded_points: u32,
    pub max_points: u32,
    pub passed_cases: u32,
    pub total_cases: u32,
    pub diagnostic_code: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OjProcessEvidence {
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub stdout_sha256: Sha256Digest,
    pub stdout_bytes: u64,
    pub stderr_sha256: Sha256Digest,
    pub stderr_bytes: u64,
    pub duration_milliseconds: u64,
    pub peak_memory_bytes: Option<u64>,
    pub timed_out: bool,
    pub output_exceeded: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OjExecutionEvidence {
    pub schema_version: String,
    pub run_id: Uuid,
    pub step_run_id: Uuid,
    pub attempt_id: Uuid,
    pub trace_id: String,
    pub request_sha256: Sha256Digest,
    pub submission_identity: Sha256Digest,
    pub evaluator_identity: Option<Sha256Digest>,
    pub toolchain_profile: String,
    pub toolchain_image_digest: String,
    pub terminal_status: OjTerminalStatus,
    pub diagnostic_code: String,
    pub compile: OjProcessEvidence,
    pub cases: Vec<OjCaseEvidence>,
    pub aggregate: OjAggregate,
}

impl OjExecutionEvidence {
    /// Verifies that full evidence is complete and bound to `request`.
    ///
    /// # Errors
    ///
    /// Returns [`OjError::EvidenceInvalid`] for missing, duplicated, forged, or mismatched data.
    pub fn validate_for(&self, request: &OjExecutionRequest) -> Result<(), OjError> {
        request.validate()?;
        if self.schema_version != OJ_EVIDENCE_SCHEMA_VERSION
            || self.run_id != request.run_id
            || self.step_run_id != request.step_run_id
            || self.attempt_id != request.attempt_id
            || self.trace_id != request.trace_id
            || self.request_sha256 != request.request_sha256()?
            || self.submission_identity != request.submission_identity
            || self.evaluator_identity != request.evaluator_identity
            || self.toolchain_profile != request.toolchain_profile
            || self.toolchain_image_digest != request.toolchain_image_digest
            || self.diagnostic_code != self.terminal_status.diagnostic_code()
            || self.aggregate.status != self.terminal_status
            || self.aggregate.diagnostic_code != self.diagnostic_code
        {
            return Err(OjError::EvidenceInvalid);
        }
        let compile_succeeded = self.compile.exit_code == Some(0)
            && self.compile.signal.is_none()
            && !self.compile.timed_out
            && !self.compile.output_exceeded;
        if self.terminal_status == OjTerminalStatus::CompileError {
            if compile_succeeded
                || !self.cases.is_empty()
                || self.aggregate.awarded_points != 0
                || self.aggregate.max_points != request.score_max_points
                || self.aggregate.passed_cases != 0
                || self.aggregate.total_cases
                    != u32::try_from(request.cases.len()).map_err(|_| OjError::EvidenceInvalid)?
            {
                return Err(OjError::EvidenceInvalid);
            }
            return Ok(());
        }
        if !compile_succeeded {
            return Err(OjError::EvidenceInvalid);
        }
        if request.phase == OjExecutionPhase::Compile {
            if self.terminal_status != OjTerminalStatus::Accepted
                || !self.cases.is_empty()
                || self.aggregate.awarded_points != 0
                || self.aggregate.max_points != 0
                || self.aggregate.passed_cases != 0
                || self.aggregate.total_cases != 0
            {
                return Err(OjError::EvidenceInvalid);
            }
            return Ok(());
        }
        let aggregate = aggregate_case_evidence(request, &self.cases)?;
        if aggregate != self.aggregate {
            return Err(OjError::EvidenceInvalid);
        }
        Ok(())
    }

    #[must_use]
    pub fn student_result(&self) -> OjStudentResult {
        OjStudentResult {
            status: self.terminal_status,
            diagnostic_code: self.diagnostic_code.clone(),
            awarded_points: self.aggregate.awarded_points,
            max_points: self.aggregate.max_points,
            passed_cases: self.aggregate.passed_cases,
            total_cases: self.aggregate.total_cases,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OjEvidenceReceipt {
    pub schema_version: String,
    pub run_id: Uuid,
    pub step_run_id: Uuid,
    pub attempt_id: Uuid,
    pub trace_id: String,
    pub request_sha256: Sha256Digest,
    pub evidence_sha256: Sha256Digest,
    pub evidence_size_bytes: u64,
    pub terminal_status: OjTerminalStatus,
    pub diagnostic_code: String,
    pub awarded_points: u32,
    pub max_points: u32,
}

impl OjEvidenceReceipt {
    /// Verifies that the payload-free termination receipt is bound to `request`.
    ///
    /// # Errors
    ///
    /// Returns [`OjError::EvidenceInvalid`] for an invalid schema, identity, digest, or score.
    pub fn validate_for(&self, request: &OjExecutionRequest) -> Result<(), OjError> {
        request.validate()?;
        if self.schema_version != OJ_EVIDENCE_RECEIPT_SCHEMA_VERSION
            || self.run_id != request.run_id
            || self.step_run_id != request.step_run_id
            || self.attempt_id != request.attempt_id
            || self.trace_id != request.trace_id
            || self.request_sha256 != request.request_sha256()?
            || self.evidence_size_bytes == 0
            || self.evidence_size_bytes > 1024 * 1024
            || self.diagnostic_code != self.terminal_status.diagnostic_code()
            || self.awarded_points > self.max_points
            || self.max_points != request.score_max_points
        {
            return Err(OjError::EvidenceInvalid);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OjStudentResult {
    pub status: OjTerminalStatus,
    pub diagnostic_code: String,
    pub awarded_points: u32,
    pub max_points: u32,
    pub passed_cases: u32,
    pub total_cases: u32,
}

/// Deterministically aggregates one complete case-evidence set.
///
/// # Errors
///
/// Returns a stable [`OjError`] for incomplete, duplicated, unknown, or forged evidence.
pub fn aggregate_case_evidence(
    request: &OjExecutionRequest,
    evidence: &[OjCaseEvidence],
) -> Result<OjAggregate, OjError> {
    request.validate()?;
    if request.phase != OjExecutionPhase::Test {
        return Err(OjError::ExecutionPlanInvalid);
    }
    if evidence.len() != request.cases.len() {
        return Err(OjError::EvidenceInvalid);
    }
    let mut by_id = BTreeMap::new();
    for item in evidence {
        if item.diagnostic_code != item.status.diagnostic_code()
            || by_id.insert(item.case_id.as_str(), item).is_some()
        {
            return Err(OjError::EvidenceInvalid);
        }
    }

    let mut awarded_points = 0_u32;
    let mut passed_cases = 0_u32;
    let mut terminal = OjTerminalStatus::Accepted;
    for case in &request.cases {
        let item = by_id
            .get(case.id.as_str())
            .ok_or(OjError::EvidenceInvalid)?;
        let expected_points = if item.status == OjCaseStatus::Accepted {
            case.max_points
        } else {
            0
        };
        if item.awarded_points != expected_points {
            return Err(OjError::EvidenceInvalid);
        }
        awarded_points = awarded_points
            .checked_add(item.awarded_points)
            .ok_or(OjError::ScoreOverflow)?;
        if item.status == OjCaseStatus::Accepted {
            passed_cases = passed_cases
                .checked_add(1)
                .ok_or(OjError::EvidenceInvalid)?;
        } else if terminal == OjTerminalStatus::Accepted {
            terminal = item.status.into();
        }
    }
    let case_max_points = request.case_max_points().ok_or(OjError::ScoreOverflow)?;
    if awarded_points > case_max_points || case_max_points == 0 {
        return Err(OjError::EvidenceInvalid);
    }
    let scaled = u64::from(awarded_points)
        .checked_mul(u64::from(request.score_max_points))
        .ok_or(OjError::ScoreOverflow)?
        / u64::from(case_max_points);
    let awarded_points = u32::try_from(scaled).map_err(|_| OjError::ScoreOverflow)?;
    Ok(OjAggregate {
        status: terminal,
        awarded_points,
        max_points: request.score_max_points,
        passed_cases,
        total_cases: u32::try_from(request.cases.len()).map_err(|_| OjError::EvidenceInvalid)?,
        diagnostic_code: terminal.diagnostic_code().to_owned(),
    })
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum OjError {
    #[error("OJ execution schema version is unsupported")]
    SchemaVersionInvalid,
    #[error("OJ execution identity must use UUIDv7")]
    IdentityInvalid,
    #[error("OJ toolchain profile is not approved")]
    ToolchainUnapproved,
    #[error("OJ worker image is not digest-pinned")]
    ImageIdentityInvalid,
    #[error("OJ path is unsafe")]
    PathUnsafe,
    #[error("OJ file binding is invalid")]
    FileBindingInvalid,
    #[error("OJ case set is invalid")]
    CaseSetInvalid,
    #[error("OJ compile/test execution plan is invalid")]
    ExecutionPlanInvalid,
    #[error("OJ execution limit is invalid")]
    LimitInvalid,
    #[error("OJ score overflow")]
    ScoreOverflow,
    #[error("OJ evidence is incomplete or inconsistent")]
    EvidenceInvalid,
}

impl OjError {
    #[must_use]
    pub const fn diagnostic_code(&self) -> &'static str {
        match self {
            Self::SchemaVersionInvalid => "LW_OJ_SCHEMA_VERSION_INVALID",
            Self::IdentityInvalid => "LW_OJ_IDENTITY_INVALID",
            Self::ToolchainUnapproved => "LW_OJ_TOOLCHAIN_UNAPPROVED",
            Self::ImageIdentityInvalid => "LW_OJ_IMAGE_IDENTITY_INVALID",
            Self::PathUnsafe => "LW_OJ_PATH_UNSAFE",
            Self::FileBindingInvalid => "LW_OJ_FILE_BINDING_INVALID",
            Self::CaseSetInvalid => "LW_OJ_CASE_SET_INVALID",
            Self::ExecutionPlanInvalid => "LW_OJ_EXECUTION_PLAN_INVALID",
            Self::LimitInvalid => "LW_OJ_LIMIT_INVALID",
            Self::ScoreOverflow => "LW_OJ_SCORE_OVERFLOW",
            Self::EvidenceInvalid => "LW_OJ_EVIDENCE_INVALID",
        }
    }
}