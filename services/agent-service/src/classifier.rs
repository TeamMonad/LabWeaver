//! Deterministic fail-closed content classifier for LLM egress.

use std::collections::BTreeSet;

use async_trait::async_trait;
use contracts::Revision;
use contracts::authoring::DeniedDataClass;
use regex::RegexSet;

use crate::claude_code::{EgressClassificationError, EgressClassifier};

/// Reviewed deterministic classifier profile. Pattern classes are fixed in code and versioned.
///
/// The profile intentionally filters only credentials and secret material
/// (private keys, tokens, secret literals). Public teaching material may cite
/// personal contact details, so personally identifiable information is not a
/// blocking content class here; the policy contract still reserves the class
/// for deployment-controlled classifiers.
pub struct DeterministicEgressClassifier {
    binding: String,
    revision: Revision,
    secrets: RegexSet,
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
        Ok(Self {
            binding,
            revision,
            secrets,
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
        _path: &str,
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
    async fn fixed_profile_detects_sensitive_content_without_path_guessing()
    -> Result<(), Box<dyn std::error::Error>> {
        let classifier =
            DeterministicEgressClassifier::new("dlp-v1".to_owned(), Revision::new(1)?)?;
        let denied = classifier
            .classify(
                "student/auth.c",
                b"email: student@example.org\n-----BEGIN PRIVATE KEY-----\n",
            )
            .await?;
        assert!(denied.contains(&DeniedDataClass::PrivateKey));
        assert!(!denied.contains(&DeniedDataClass::PersonallyIdentifiableInformation));
        assert!(!denied.contains(&DeniedDataClass::UnallowlistedStudentSubmission));
        let contact_only = classifier
            .classify("xv6/README", b"contact rtm@mit.edu for details\n")
            .await?;
        assert!(contact_only.is_empty());
        let allowed = classifier
            .classify("student/auth.c", b"int authenticate(void) { return 0; }\n")
            .await?;
        assert!(allowed.is_empty());
        Ok(())
    }
}
