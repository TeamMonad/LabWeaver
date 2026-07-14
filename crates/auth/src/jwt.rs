//! Bearer JWT verification with configured JWKS refresh and algorithm allowlist.

use std::{collections::BTreeSet, time::Duration};

use jsonwebtoken::Algorithm;
use jwt_authorizer::{Authorizer, JwtAuthorizer, Refresh, RefreshStrategy, Validation};
use serde::Deserialize;

use crate::{AuthConfig, config::OidcFileConfig};

/// Verified JWT claims retained after signature and registered-claim validation.
#[derive(Clone, Debug, Deserialize)]
pub struct BearerClaims {
    /// Issuer-local opaque subject.
    pub sub: String,
    /// Token expiry required by validation.
    pub exp: i64,
    /// Authorized party required to bind a Keycloak client token.
    pub azp: Option<String>,
    /// Provider-specific signed claims used only for configured role extraction.
    #[serde(flatten)]
    pub claims: serde_json::Map<String, serde_json::Value>,
}

/// Verified Keycloak back-channel logout claims.
#[derive(Clone, Debug, Deserialize)]
pub struct BackchannelLogoutClaims {
    /// Provider session identifier which selects sessions for revocation.
    pub sid: Option<String>,
    /// Required logout event declaration.
    pub events: serde_json::Map<String, serde_json::Value>,
    /// Token expiry required by validation.
    pub exp: i64,
    /// Issued-at time is required by the back-channel logout specification.
    pub iat: i64,
    /// Unique token identity used for durable replay rejection.
    pub jti: String,
    /// Back-channel logout must never carry a browser nonce.
    pub nonce: Option<String>,
    /// Authorized party binds the logout token to this relying party.
    pub azp: Option<String>,
}

/// Builds a bearer verifier that fails closed on startup discovery failure and
/// performs a single serialized refresh when a token carries an unknown `kid`.
pub async fn build_bearer_authorizer(
    config: &AuthConfig,
    oidc: &OidcFileConfig,
    http: reqwest::Client,
) -> Result<Authorizer<BearerClaims>, JwtVerifierError> {
    let algorithms = parse_algorithms(&oidc.jwt_algorithms)?;
    let client_id = config.client_id.clone();
    JwtAuthorizer::from_oidc(config.issuer.as_str())
        .http_client(http)
        .validation(
            Validation::new()
                .iss(&[config.issuer.as_str()])
                .aud(&[config.audience.as_str()])
                .nbf(true)
                .leeway(0)
                .algs(algorithms),
        )
        .check(move |claims: &BearerClaims| {
            !claims.sub.is_empty() && claims.azp.as_deref() == Some(client_id.as_str())
        })
        .refresh(Refresh {
            strategy: RefreshStrategy::KeyNotFound,
            refresh_interval: Duration::from_secs(oidc.jwks_refresh_seconds),
            retry_interval: Duration::from_secs(oidc.jwks_retry_seconds),
        })
        .build()
        .await
        .map_err(|_| JwtVerifierError::JwksUnavailable)
}

/// Builds the dedicated verifier for Keycloak back-channel logout tokens.
pub async fn build_backchannel_logout_authorizer(
    config: &AuthConfig,
    oidc: &OidcFileConfig,
    http: reqwest::Client,
) -> Result<Authorizer<BackchannelLogoutClaims>, JwtVerifierError> {
    let algorithms = parse_algorithms(&oidc.jwt_algorithms)?;
    let client_id = config.client_id.clone();
    JwtAuthorizer::from_oidc(config.issuer.as_str())
        .http_client(http)
        .validation(
            Validation::new()
                .iss(&[config.issuer.as_str()])
                .aud(&[config.client_id.as_str()])
                .nbf(true)
                .leeway(0)
                .algs(algorithms),
        )
        .check(move |claims: &BackchannelLogoutClaims| {
            claims.sid.as_deref().is_some_and(|sid| !sid.is_empty())
                && !claims.jti.is_empty()
                && claims.iat > 0
                && claims.exp > claims.iat
                && claims.nonce.is_none()
                && claims.azp.as_deref() == Some(client_id.as_str())
                && claims
                    .events
                    .contains_key("http://schemas.openid.net/event/backchannel-logout")
        })
        .refresh(Refresh {
            strategy: RefreshStrategy::KeyNotFound,
            refresh_interval: Duration::from_secs(oidc.jwks_refresh_seconds),
            retry_interval: Duration::from_secs(oidc.jwks_retry_seconds),
        })
        .build()
        .await
        .map_err(|_| JwtVerifierError::JwksUnavailable)
}

fn parse_algorithms(values: &BTreeSet<String>) -> Result<Vec<Algorithm>, JwtVerifierError> {
    values
        .iter()
        .map(|value| match value.as_str() {
            "RS256" => Ok(Algorithm::RS256),
            "RS384" => Ok(Algorithm::RS384),
            "RS512" => Ok(Algorithm::RS512),
            "PS256" => Ok(Algorithm::PS256),
            "PS384" => Ok(Algorithm::PS384),
            "PS512" => Ok(Algorithm::PS512),
            "ES256" => Ok(Algorithm::ES256),
            "ES384" => Ok(Algorithm::ES384),
            "EdDSA" => Ok(Algorithm::EdDSA),
            _ => Err(JwtVerifierError::AlgorithmRejected),
        })
        .collect()
}

/// JWT verifier failures never permit a bearer-authenticated request.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum JwtVerifierError {
    /// Configured algorithm was not in the explicit asymmetric allowlist.
    #[error("LW_AUTH_CONFIG_BINDING_MISSING")]
    AlgorithmRejected,
    /// OIDC Discovery/JWKS initialisation could not complete.
    #[error("LW_AUTH_JWKS_UNAVAILABLE")]
    JwksUnavailable,
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::parse_algorithms;

    #[test]
    fn rejects_symmetric_and_unknown_algorithms() {
        assert!(parse_algorithms(&BTreeSet::from(["HS256".to_owned()])).is_err());
        assert!(parse_algorithms(&BTreeSet::from(["none".to_owned()])).is_err());
    }
}
