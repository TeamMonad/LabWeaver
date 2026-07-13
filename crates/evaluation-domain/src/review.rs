use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

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

    /// Validates constraints that JSON Schema cannot express portably.
    ///
    /// # Errors
    ///
    /// Returns an error when confidence is outside the inclusive range from zero to one.
    pub fn validate(&self) -> Result<(), GoalReviewError> {
        if !(0.0..=1.0).contains(&self.confidence) {
            return Err(GoalReviewError::InvalidConfidence(self.confidence));
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
    criterion: String,
    result: FindingResult,
    evidence: Vec<EvidenceLocation>,
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
    path: String,
    start_line: u32,
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
}

impl GoalReviewError {
    /// Returns the stable diagnostic code for this failure.
    #[must_use]
    pub const fn diagnostic_code(&self) -> &'static str {
        match self {
            Self::InvalidDocument(_) => "LW_EVAL_LLM_REVIEW_INVALID",
            Self::InvalidConfidence(_) => "LW_EVAL_LLM_CONFIDENCE_INVALID",
        }
    }
}
