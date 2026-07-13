use std::collections::BTreeSet;

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

use crate::validation::is_normalized_safe_relative_path;

const MAX_FINDINGS: usize = 64;
const MAX_EVIDENCE_PER_FINDING: usize = 16;
const MAX_CRITERION_BYTES: usize = 1_024;
const MAX_SUGGESTION_BYTES: usize = 4_096;
const MAX_EVIDENCE_PATH_BYTES: usize = 1_024;

/// Advisory-only review produced by an LLM backend.
///
/// This contract deliberately has no score or verdict field. Unknown fields are rejected during
/// deserialization so an LLM cannot smuggle protected scoring data into the review channel.
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GoalReview {
    schema_version: GoalReviewSchemaVersion,
    assessment: GoalAssessment,
    #[schemars(range(min = 0.0, max = 1.0))]
    confidence: f64,
    #[schemars(length(max = 64))]
    findings: Vec<GoalFinding>,
    requires_teacher_attention: bool,
}

impl GoalReview {
    /// Parses and validates an advisory review from JSON.
    ///
    /// # Errors
    ///
    /// Returns a stable parse or validation error for malformed reviews.
    pub fn from_json(input: &str) -> Result<Self, GoalReviewError> {
        let wire: GoalReviewWire = serde_json::from_str(input)
            .map_err(|error| GoalReviewError::InvalidDocument(error.to_string()))?;
        let review = Self::from_wire(wire);
        review.validate()?;
        Ok(review)
    }

    /// Parses an advisory review and binds every evidence location to the current step include.
    ///
    /// # Errors
    ///
    /// Returns a stable parse, intrinsic validation, or path-allowlist error.
    pub fn from_json_against<T: AsRef<str>>(
        input: &str,
        allowed_paths: &[T],
    ) -> Result<Self, GoalReviewError> {
        let review = Self::from_json(input)?;
        review.validate_against(allowed_paths)?;
        Ok(review)
    }

    /// Validates constraints that JSON Schema cannot express portably.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid confidence, content, bounds, paths, or line ranges.
    pub fn validate(&self) -> Result<(), GoalReviewError> {
        if !(0.0..=1.0).contains(&self.confidence) {
            return Err(GoalReviewError::InvalidConfidence(self.confidence));
        }
        if self.findings.len() > MAX_FINDINGS {
            return Err(GoalReviewError::LimitExceeded {
                field: "findings",
                actual: self.findings.len(),
                max: MAX_FINDINGS,
            });
        }
        for (index, finding) in self.findings.iter().enumerate() {
            validate_non_empty(index, "criterion", &finding.criterion)?;
            validate_length("criterion", &finding.criterion, MAX_CRITERION_BYTES)?;
            validate_non_empty(index, "suggestion", &finding.suggestion)?;
            validate_length("suggestion", &finding.suggestion, MAX_SUGGESTION_BYTES)?;
            if finding.evidence.is_empty() {
                return Err(GoalReviewError::InvalidFinding {
                    index,
                    detail: "evidence must not be empty",
                });
            }
            if finding.evidence.len() > MAX_EVIDENCE_PER_FINDING {
                return Err(GoalReviewError::LimitExceeded {
                    field: "evidence",
                    actual: finding.evidence.len(),
                    max: MAX_EVIDENCE_PER_FINDING,
                });
            }
            for evidence in &finding.evidence {
                validate_length("evidence.path", &evidence.path, MAX_EVIDENCE_PATH_BYTES)?;
                if !is_normalized_safe_relative_path(&evidence.path) {
                    return Err(GoalReviewError::UnsafeEvidencePath {
                        path: evidence.path.clone(),
                    });
                }
                if evidence.start_line == 0 || evidence.end_line < evidence.start_line {
                    return Err(GoalReviewError::InvalidEvidenceRange {
                        path: evidence.path.clone(),
                        start_line: evidence.start_line,
                        end_line: evidence.end_line,
                    });
                }
            }
        }
        Ok(())
    }

    /// Validates intrinsic review constraints and restricts evidence to one Advisory Step include.
    ///
    /// # Errors
    ///
    /// Returns an error when an evidence path is unsafe or absent from `allowed_paths`.
    pub fn validate_against<T: AsRef<str>>(
        &self,
        allowed_paths: &[T],
    ) -> Result<(), GoalReviewError> {
        self.validate()?;
        let allowed = allowed_paths
            .iter()
            .map(AsRef::as_ref)
            .collect::<BTreeSet<_>>();
        for evidence in self
            .findings
            .iter()
            .flat_map(|finding| finding.evidence.iter())
        {
            if !allowed.contains(evidence.path.as_str()) {
                return Err(GoalReviewError::EvidencePathNotAllowed {
                    path: evidence.path.clone(),
                });
            }
        }
        Ok(())
    }

    /// Returns the advisory assessment.
    #[must_use]
    pub const fn assessment(&self) -> GoalAssessment {
        self.assessment
    }

    /// Returns the model-reported confidence.
    #[must_use]
    pub const fn confidence(&self) -> f64 {
        self.confidence
    }

    /// Returns the advisory findings.
    #[must_use]
    pub fn findings(&self) -> &[GoalFinding] {
        &self.findings
    }

    /// Reports whether the advisory channel requested teacher attention.
    #[must_use]
    pub const fn requires_teacher_attention(&self) -> bool {
        self.requires_teacher_attention
    }

    fn from_wire(wire: GoalReviewWire) -> Self {
        Self {
            schema_version: wire.schema_version,
            assessment: wire.assessment,
            confidence: wire.confidence,
            findings: wire.findings,
            requires_teacher_attention: wire.requires_teacher_attention,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GoalReviewWire {
    schema_version: GoalReviewSchemaVersion,
    assessment: GoalAssessment,
    confidence: f64,
    findings: Vec<GoalFinding>,
    requires_teacher_attention: bool,
}

impl<'de> Deserialize<'de> for GoalReview {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let review = Self::from_wire(GoalReviewWire::deserialize(deserializer)?);
        review.validate().map_err(serde::de::Error::custom)?;
        Ok(review)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum GoalReviewSchemaVersion {
    #[serde(rename = "goal-review/v1")]
    V1,
}

/// Overall advisory assessment of a submitted goal.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalAssessment {
    /// The goal is fully met according to the advisory rubric.
    Met,
    /// The goal is only partially met.
    PartiallyMet,
    /// The goal is not met.
    NotMet,
    /// Evidence is insufficient for an advisory assessment.
    InsufficientEvidence,
}

/// One advisory finding with bounded evidence locations.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GoalFinding {
    #[schemars(length(min = 1, max = 1024))]
    criterion: String,
    result: FindingResult,
    #[schemars(length(min = 1, max = 16))]
    evidence: Vec<EvidenceLocation>,
    #[schemars(length(min = 1, max = 4096))]
    suggestion: String,
}

impl GoalFinding {
    /// Returns the rubric criterion.
    #[must_use]
    pub fn criterion(&self) -> &str {
        &self.criterion
    }

    /// Returns the advisory finding result.
    #[must_use]
    pub const fn result(&self) -> FindingResult {
        self.result
    }

    /// Returns bounded evidence locations.
    #[must_use]
    pub fn evidence(&self) -> &[EvidenceLocation] {
        &self.evidence
    }

    /// Returns the non-scoring suggestion.
    #[must_use]
    pub fn suggestion(&self) -> &str {
        &self.suggestion
    }
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
/// Advisory result for one rubric criterion.
pub enum FindingResult {
    /// The criterion is met.
    Met,
    /// The criterion is partially met.
    Partial,
    /// Required evidence or work is missing.
    Missing,
    /// Available evidence is unclear.
    Unclear,
}

/// Bounded source location cited by an advisory finding.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceLocation {
    #[schemars(length(min = 1, max = 1024))]
    path: String,
    #[schemars(range(min = 1))]
    start_line: u32,
    #[schemars(range(min = 1))]
    end_line: u32,
}

impl EvidenceLocation {
    /// Returns the submission-relative evidence path.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Returns the inclusive first line.
    #[must_use]
    pub const fn start_line(&self) -> u32 {
        self.start_line
    }

    /// Returns the inclusive final line.
    #[must_use]
    pub const fn end_line(&self) -> u32 {
        self.end_line
    }
}

/// Fail-fast diagnostics for advisory review documents.
#[derive(Debug, Error, PartialEq)]
pub enum GoalReviewError {
    /// JSON syntax, type, or unknown-field validation failed.
    #[error("LLM review document is invalid: {0}")]
    InvalidDocument(String),
    /// Confidence was outside the supported range.
    #[error("LLM review confidence must be between 0 and 1, got {0}")]
    InvalidConfidence(f64),
    /// One finding omitted required advisory content.
    #[error("LLM review finding {index} is invalid: {detail}")]
    InvalidFinding {
        /// Zero-based finding index.
        index: usize,
        /// Stable validation detail.
        detail: &'static str,
    },
    /// A bounded review field exceeded its contract limit.
    #[error("LLM review {field} count or length {actual} exceeds limit {max}")]
    LimitExceeded {
        /// Bounded field name.
        field: &'static str,
        /// Observed item count or UTF-8 byte length.
        actual: usize,
        /// Maximum accepted count or length.
        max: usize,
    },
    /// An evidence path was not a normalized safe relative path.
    #[error("LLM review evidence path is unsafe: {path}")]
    UnsafeEvidencePath {
        /// Rejected evidence path.
        path: String,
    },
    /// An evidence path was outside the current Advisory Step include.
    #[error("LLM review evidence path is not allowed by the advisory step: {path}")]
    EvidencePathNotAllowed {
        /// Rejected evidence path.
        path: String,
    },
    /// An evidence location used zero or reversed line bounds.
    #[error("LLM review evidence range is invalid for {path}: {start_line}..={end_line}")]
    InvalidEvidenceRange {
        /// Evidence path containing the invalid range.
        path: String,
        /// Inclusive first line.
        start_line: u32,
        /// Inclusive final line.
        end_line: u32,
    },
}

impl GoalReviewError {
    /// Returns the stable diagnostic code for this failure.
    #[must_use]
    pub const fn diagnostic_code(&self) -> &'static str {
        match self {
            Self::InvalidDocument(_) => "LW_EVAL_LLM_REVIEW_INVALID",
            Self::InvalidConfidence(_) => "LW_EVAL_LLM_CONFIDENCE_INVALID",
            Self::InvalidFinding { .. } => "LW_EVAL_LLM_FINDING_INVALID",
            Self::LimitExceeded { .. } => "LW_EVAL_LLM_LIMIT_EXCEEDED",
            Self::UnsafeEvidencePath { .. } => "LW_EVAL_LLM_EVIDENCE_PATH_UNSAFE",
            Self::EvidencePathNotAllowed { .. } => "LW_EVAL_LLM_EVIDENCE_PATH_NOT_ALLOWED",
            Self::InvalidEvidenceRange { .. } => "LW_EVAL_LLM_EVIDENCE_RANGE_INVALID",
        }
    }
}

fn validate_non_empty(
    index: usize,
    field: &'static str,
    value: &str,
) -> Result<(), GoalReviewError> {
    if value.trim().is_empty() {
        Err(GoalReviewError::InvalidFinding {
            index,
            detail: match field {
                "criterion" => "criterion must not be empty",
                "suggestion" => "suggestion must not be empty",
                _ => "required field must not be empty",
            },
        })
    } else {
        Ok(())
    }
}

fn validate_length(field: &'static str, value: &str, max: usize) -> Result<(), GoalReviewError> {
    if value.len() > max {
        Err(GoalReviewError::LimitExceeded {
            field,
            actual: value.len(),
            max,
        })
    } else {
        Ok(())
    }
}
