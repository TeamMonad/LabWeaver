//! Deterministic fail-closed content classifier for LLM egress.

use std::collections::BTreeSet;

use async_trait::async_trait;
use contracts::Revision;
use contracts::authoring::DeniedDataClass;
use regex::RegexSet;

use crate::claude_code::{EgressClassificationError, EgressClassifier};

/// Reviewed deterministic classifier profile. Pattern classes are fixed in code and versioned.
pub struct DeterministicEgressClassifier {
    binding: String,
    revision: Revision,
    secrets: RegexSet,
    pii: RegexSet,
    student_paths: RegexSet,
}

impl DeterministicEgressClassifier {
    /// Builds the fixed profile from an explicit deployment identity.
    ///
    /// # Errors
    ///
    /// Returns an error when the deployment identity or compiled profile is invalid.
    pub fn new(binding: String, revision: Revision) -> Result<Self, EgressClassificationError> {
        if binding.trim().is_empty()
            || binding.trim() != binding
            || binding
                .bytes()
                .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace())
        {
            return Err(EgressClassificationError);
        }
        let secrets = RegexSet::new([
            r"(?i)-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----",
            r"(?i)\b(?:password|passwd|client_secret|api[_-]?key)\s*[:=]\s*[^\s]{8,}",
            r"\bAKIA[0-9A-Z]{16}\b",
            r"\bgh[pousr]_[A-Za-z0-9_]{30,}\b",
            r"\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\b",
            r"(?i)\bBearer\s+[A-Za-z0-9._~+/-]{16,}=*",
        ])
        .map_err(|_| EgressClassificationError)?;
        let pii = RegexSet::new([
            r"(?i)\b[A-Z0-9._%+-]+@[A-Z0-9.-]+\.[A-Z]{2,}\b",
            r"\b1[3-9][0-9]{9}\b",
            r"\b[1-9][0-9]{5}(?:18|19|20)[0-9]{2}(?:0[1-9]|1[0-2])(?:0[1-9]|[12][0-9]|3[01])[0-9]{3}[0-9Xx]\b",
        ])
        .map_err(|_| EgressClassificationError)?;
        let student_paths = RegexSet::new([
            r"(?i)(?:^|/)(?:student|students|submission|submissions)(?:/|$)",
            r"(?i)(?:^|/)(?:roster|gradebook)(?:\.|/|$)",
        ])
        .map_err(|_| EgressClassificationError)?;
        Ok(Self {
            binding,
            revision,
            secrets,
            pii,
            student_paths,
        })
    }
}

#[async_trait]
impl EgressClassifier for DeterministicEgressClassifier {
    fn binding(&self) -> &str {
        &self.binding
    }

    fn revision(&self) -> Revision {
        self.revision
    }

    async fn classify(
        &self,
        path: &str,
        bytes: &[u8],
    ) -> Result<BTreeSet<DeniedDataClass>, EgressClassificationError> {
        let text = std::str::from_utf8(bytes).map_err(|_| EgressClassificationError)?;
        let mut denied = BTreeSet::new();
        if self.secrets.is_match(text) {
            denied.insert(DeniedDataClass::Secret);
            denied.insert(DeniedDataClass::Token);
            if text.contains("PRIVATE KEY") {
                denied.insert(DeniedDataClass::PrivateKey);
            }
        }
        if self.pii.is_match(text) {
            denied.insert(DeniedDataClass::PersonallyIdentifiableInformation);
        }
        if self.student_paths.is_match(path) {
            denied.insert(DeniedDataClass::UnallowlistedStudentSubmission);
        }
        Ok(denied)
    }
}

#[cfg(test)]
mod tests {
    use contracts::Revision;
    use contracts::authoring::DeniedDataClass;

    use crate::claude_code::EgressClassifier;

    use super::DeterministicEgressClassifier;

    #[tokio::test]
    async fn fixed_profile_detects_private_keys_tokens_pii_and_student_paths()
    -> Result<(), Box<dyn std::error::Error>> {
        let classifier =
            DeterministicEgressClassifier::new("dlp-v1".to_owned(), Revision::new(1)?)?;
        let denied = classifier
            .classify(
                "students/a/submission.md",
                b"email: student@example.org\n-----BEGIN PRIVATE KEY-----\n",
            )
            .await?;
        assert!(denied.contains(&DeniedDataClass::PrivateKey));
        assert!(denied.contains(&DeniedDataClass::PersonallyIdentifiableInformation));
        assert!(denied.contains(&DeniedDataClass::UnallowlistedStudentSubmission));
        Ok(())
    }
}
