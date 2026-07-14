use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::TryRngCore;
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;

/// One-time browser authorization-code transaction persisted in encrypted server-side session state.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct OidcTransaction {
    /// CSRF state sent to Keycloak.
    pub state: String,
    /// OIDC nonce bound to the ID token.
    pub nonce: String,
    /// PKCE S256 verifier; this value must never be logged or sent to the browser.
    pub pkce_verifier: String,
}

impl OidcTransaction {
    /// Generates independent state, nonce, and PKCE verifier values.
    pub fn generate() -> Result<Self, OidcTransactionError> {
        Ok(Self {
            state: random_value()?,
            nonce: random_value()?,
            pkce_verifier: random_value()?,
        })
    }

    /// Consumes the transaction only when callback state is exact.
    pub fn verify_state(&self, returned_state: Option<&str>) -> Result<(), OidcTransactionError> {
        let returned_state = returned_state.ok_or(OidcTransactionError::StateMissing)?;
        if returned_state.len() != self.state.len()
            || self
                .state
                .as_bytes()
                .ct_eq(returned_state.as_bytes())
                .unwrap_u8()
                != 1
        {
            return Err(OidcTransactionError::StateMismatch);
        }
        Ok(())
    }
}

fn random_value() -> Result<String, OidcTransactionError> {
    let mut bytes = [0_u8; 48];
    rand::rngs::OsRng
        .try_fill_bytes(&mut bytes)
        .map_err(|_| OidcTransactionError::RandomnessUnavailable)?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

/// One-time OIDC transaction failures.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum OidcTransactionError {
    /// Entropy source failed.
    #[error("LW_AUTH_OIDC_RANDOMNESS_UNAVAILABLE")]
    RandomnessUnavailable,
    /// Keycloak callback omitted state.
    #[error("LW_AUTH_OIDC_STATE_REQUIRED")]
    StateMissing,
    /// Keycloak callback state did not match the stored one-time transaction.
    #[error("LW_AUTH_OIDC_STATE_REJECTED")]
    StateMismatch,
}

#[cfg(test)]
mod tests {
    use super::OidcTransaction;

    #[test]
    fn state_is_required_and_exact() -> Result<(), Box<dyn std::error::Error>> {
        let transaction = OidcTransaction::generate()?;
        assert!(transaction.verify_state(None).is_err());
        assert!(transaction.verify_state(Some("forged")).is_err());
        transaction.verify_state(Some(&transaction.state))?;
        Ok(())
    }
}
