//! Signed Access-to-Resource identity delegation.
//!
//! mTLS authenticates the calling service. This short-lived JWT binds the
//! authenticated Access service to the already verified BFF session identity,
//! so actor and role values never arrive at Resource as unauthenticated HTTP
//! assertions.

use contracts::{ActorId, BffSessionId, PlatformRole};
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, encode};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

const ISSUER: &str = "labweaver-access-service";
const AUDIENCE: &str = "labweaver-resource-service";

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Claims {
    iss: String,
    aud: String,
    sub: String,
    session_id: String,
    roles: Vec<String>,
    iat: usize,
    exp: usize,
    jti: String,
}

/// Verified user identity delegated by Access to Resource.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResourceDelegation {
    /// Durable actor identity authenticated by Access.
    pub actor_id: ActorId,
    /// BFF session identity and expiry binding.
    pub session_id: BffSessionId,
    /// Base platform roles authenticated by Access.
    pub roles: Vec<PlatformRole>,
}

/// Signs the current BFF session with the shared service binding.
pub fn encode_resource_delegation(
    secret: &[u8],
    session: &crate::BffSession,
    now: OffsetDateTime,
) -> Result<String, ResourceDelegationError> {
    validate_secret(secret)?;
    let now_seconds = unix_seconds(now)?;
    let expiry = session.expires_at.min(session.idle_expires_at);
    let expiry_seconds = unix_seconds(expiry)?;
    if expiry_seconds <= now_seconds {
        return Err(ResourceDelegationError::Expired);
    }
    let roles = session
        .roles
        .iter()
        .map(|role| {
            serde_json::to_value(role)
                .ok()
                .and_then(|value| value.as_str().map(ToOwned::to_owned))
                .ok_or(ResourceDelegationError::Claims)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let claims = Claims {
        iss: ISSUER.to_owned(),
        aud: AUDIENCE.to_owned(),
        sub: session.actor_id.to_string(),
        session_id: session.session_id.to_string(),
        roles,
        iat: usize::try_from(now_seconds).map_err(|_| ResourceDelegationError::Claims)?,
        exp: usize::try_from(expiry_seconds).map_err(|_| ResourceDelegationError::Claims)?,
        jti: Uuid::now_v7().to_string(),
    };
    encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(secret),
    )
    .map_err(|_| ResourceDelegationError::Encoding)
}

/// Verifies signature, issuer, audience, expiry, identity and role encoding.
pub fn decode_resource_delegation(
    secret: &[u8],
    token: &str,
) -> Result<ResourceDelegation, ResourceDelegationError> {
    validate_secret(secret)?;
    let mut validation = Validation::new(Algorithm::HS256);
    validation.leeway = 0;
    validation.set_issuer(&[ISSUER]);
    validation.set_audience(&[AUDIENCE]);
    let token = decode::<Claims>(token, &DecodingKey::from_secret(secret), &validation)
        .map_err(|_| ResourceDelegationError::Verification)?;
    if token.claims.iss != ISSUER
        || token.claims.aud != AUDIENCE
        || token.claims.sub.is_empty()
        || token.claims.session_id.is_empty()
        || token.claims.jti.is_empty()
        || token.claims.roles.is_empty()
    {
        return Err(ResourceDelegationError::Claims);
    }
    let actor_id = token
        .claims
        .sub
        .parse()
        .map_err(|_| ResourceDelegationError::Claims)?;
    let session_id = token
        .claims
        .session_id
        .parse()
        .map_err(|_| ResourceDelegationError::Claims)?;
    let roles = token
        .claims
        .roles
        .iter()
        .map(|role| {
            serde_json::from_value(serde_json::Value::String(role.clone()))
                .map_err(|_| ResourceDelegationError::Claims)
        })
        .collect::<Result<Vec<PlatformRole>, _>>()?;
    Ok(ResourceDelegation {
        actor_id,
        session_id,
        roles,
    })
}

fn validate_secret(secret: &[u8]) -> Result<(), ResourceDelegationError> {
    if secret.len() < 32 {
        Err(ResourceDelegationError::Secret)
    } else {
        Ok(())
    }
}

fn unix_seconds(value: OffsetDateTime) -> Result<i64, ResourceDelegationError> {
    value
        .unix_timestamp()
        .checked_sub(0)
        .ok_or(ResourceDelegationError::Claims)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
/// Failure while signing or verifying an Access-to-Resource delegation.
pub enum ResourceDelegationError {
    /// The signing secret is too short for the configured algorithm.
    #[error("LW_AUTH_RESOURCE_DELEGATION_SECRET_INVALID")]
    Secret,
    /// Claims are absent, malformed, or semantically invalid.
    #[error("LW_AUTH_RESOURCE_DELEGATION_CLAIMS_INVALID")]
    Claims,
    /// The BFF session has expired.
    #[error("LW_AUTH_RESOURCE_DELEGATION_EXPIRED")]
    Expired,
    /// Signing the delegation failed.
    #[error("LW_AUTH_RESOURCE_DELEGATION_ENCODING_FAILED")]
    Encoding,
    /// Verification of the delegation failed.
    #[error("LW_AUTH_RESOURCE_DELEGATION_VERIFICATION_FAILED")]
    Verification,
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "the round-trip test uses fixed valid test fixtures and keeps failure diagnostics local"
)]
mod tests {
    use super::*;
    use crate::{BffSession, CsrfToken};

    #[test]
    fn delegation_round_trip_preserves_verified_identity() {
        let now = OffsetDateTime::from_unix_timestamp(1_800_000_000).expect("timestamp");
        let session = BffSession {
            session_id: Uuid::now_v7(),
            actor_id: Uuid::now_v7(),
            roles: vec![PlatformRole::PlatformAdmin],
            authorization_revision: 1,
            expires_at: now + time::Duration::minutes(10),
            idle_expires_at: now + time::Duration::minutes(5),
            csrf_token: CsrfToken::from_secret("test-csrf".to_owned()),
        };
        let token = encode_resource_delegation(&[7; 32], &session, now).expect("token");
        let decoded = decode_resource_delegation(&[7; 32], &token).expect("delegation");
        assert_eq!(decoded.actor_id.as_uuid(), session.actor_id);
        assert_eq!(decoded.session_id.as_uuid(), session.session_id);
        assert_eq!(decoded.roles, session.roles);
    }

    #[test]
    fn tampered_or_short_secret_is_rejected() {
        assert_eq!(
            validate_secret(&[1; 31]),
            Err(ResourceDelegationError::Secret)
        );
    }
}
