use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::TryRngCore;
use subtle::ConstantTimeEq;

/// Opaque synchronizer token stored only in the server-side BFF session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CsrfToken(String);

impl CsrfToken {
    /// Creates a cryptographically random token suitable for a single browser session.
    pub fn generate() -> Result<Self, CsrfError> {
        let mut bytes = [0_u8; 32];
        rand::rngs::OsRng
            .try_fill_bytes(&mut bytes)
            .map_err(|_| CsrfError::RandomnessUnavailable)?;
        Ok(Self(URL_SAFE_NO_PAD.encode(bytes)))
    }

    /// Returns the value for an HTTPS response body or header.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Reconstitutes a token only after AEAD-authenticated server-side storage.
    #[must_use]
    pub fn from_secret(value: String) -> Self {
        Self(value)
    }
}

/// Validates an incoming token without early-exit string comparison.
pub fn verify_csrf_token(expected: &CsrfToken, supplied: Option<&str>) -> Result<(), CsrfError> {
    let supplied = supplied.ok_or(CsrfError::Missing)?;
    if supplied.len() != expected.0.len()
        || expected.0.as_bytes().ct_eq(supplied.as_bytes()).unwrap_u8() != 1
    {
        return Err(CsrfError::Mismatch);
    }
    Ok(())
}

/// CSRF failures exposed as stable authorization diagnostics by adapters.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum CsrfError {
    /// Entropy source could not provide a token.
    #[error("LW_AUTH_CSRF_RANDOMNESS_UNAVAILABLE")]
    RandomnessUnavailable,
    /// State-changing request omitted the synchronizer token.
    #[error("LW_AUTH_CSRF_REQUIRED")]
    Missing,
    /// State-changing request supplied a mismatched token.
    #[error("LW_AUTH_CSRF_REJECTED")]
    Mismatch,
}

#[cfg(test)]
mod tests {
    use super::{CsrfToken, verify_csrf_token};

    #[test]
    fn rejects_missing_and_mismatched_tokens() -> Result<(), Box<dyn std::error::Error>> {
        let token = CsrfToken::generate()?;
        assert!(verify_csrf_token(&token, None).is_err());
        assert!(verify_csrf_token(&token, Some("not-the-token")).is_err());
        verify_csrf_token(&token, Some(token.expose()))?;
        Ok(())
    }
}
