//! SSH public key, AccessGrant, EndpointGrant, and Gateway session contracts.

use std::collections::BTreeSet;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ssh_key::{Algorithm, HashAlg, PublicKey, public::KeyData};

use crate::environment::{EndpointHealth, EndpointProtocol};
use crate::{
    AccessGrantId, ActorId, CourseId, EndpointGrantId, EndpointId, EnvironmentId, GatewaySessionId,
    Revision, SshPublicKeyId, UtcTimestamp,
};

/// Public-key algorithm frozen by the v1 Access contract.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SshKeyAlgorithm {
    Ed25519,
    SecurityKeyEd25519,
    RsaSha2,
}

/// Sanitized key metadata returned by public APIs.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SshPublicKey {
    pub id: SshPublicKeyId,
    pub actor_id: ActorId,
    pub fingerprint_sha256: String,
    pub algorithm: SshKeyAlgorithm,
    pub rsa_bits: Option<u32>,
    pub revision: Revision,
    pub created_at: UtcTimestamp,
}

/// Validated key material used only by Access Service and the mTLS Gateway contract.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidatedSshPublicKey {
    pub normalized_openssh: String,
    pub fingerprint_sha256: String,
    pub algorithm: SshKeyAlgorithm,
    pub rsa_bits: Option<u32>,
}

/// Parses and enforces the exact v1 SSH public-key allowlist.
pub fn validate_ssh_public_key(input: &str) -> Result<ValidatedSshPublicKey, AccessError> {
    if input.len() > 16_384 || input.contains('\n') || input.contains('\r') {
        return Err(AccessError::InvalidKey);
    }
    let key = PublicKey::from_openssh(input).map_err(|_| AccessError::InvalidKey)?;
    let (algorithm, rsa_bits) = match key.key_data() {
        KeyData::Ed25519(_) => (SshKeyAlgorithm::Ed25519, None),
        KeyData::SkEd25519(_) => (SshKeyAlgorithm::SecurityKeyEd25519, None),
        KeyData::Rsa(rsa) => {
            let modulus = rsa.n.as_bytes();
            let Some(first) = modulus.first() else {
                return Err(AccessError::InvalidKey);
            };
            let bit_length = modulus
                .len()
                .saturating_mul(8)
                .saturating_sub(first.leading_zeros() as usize);
            let bit_length = u32::try_from(bit_length).map_err(|_| AccessError::InvalidKey)?;
            if bit_length < 3_072 {
                return Err(AccessError::WeakRsaKey(bit_length));
            }
            (SshKeyAlgorithm::RsaSha2, Some(bit_length))
        }
        #[allow(unreachable_patterns)]
        _ => {
            return Err(AccessError::UnsupportedKeyAlgorithm(
                key.algorithm().to_string(),
            ));
        }
    };
    if matches!(key.algorithm(), Algorithm::Dsa | Algorithm::Ecdsa { .. }) {
        return Err(AccessError::UnsupportedKeyAlgorithm(
            key.algorithm().to_string(),
        ));
    }
    let normalized = PublicKey::new(key.key_data().clone(), "")
        .to_openssh()
        .map_err(|_| AccessError::InvalidKey)?;
    Ok(ValidatedSshPublicKey {
        normalized_openssh: normalized,
        fingerprint_sha256: key.fingerprint(HashAlg::Sha256).to_string(),
        algorithm,
        rsa_bits,
    })
}

/// Validates the service-owned global fingerprint uniqueness projection.
pub fn validate_ssh_key_registry(keys: &[SshPublicKey]) -> Result<(), AccessError> {
    let mut ids = BTreeSet::new();
    let mut fingerprints = BTreeSet::new();
    for key in keys {
        if !ids.insert(key.id) || !fingerprints.insert(key.fingerprint_sha256.as_str()) {
            return Err(AccessError::DuplicateKey);
        }
    }
    Ok(())
}

/// AccessGrant lifecycle.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessGrantState {
    Requested,
    Active,
    Expired,
    Revoked,
}

/// Effective endpoint grant state exposed to the current actor.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EndpointGrantSnapshotState {
    Active,
    Expired,
    Revoked,
    Unhealthy,
}

/// Safe authorization result suitable for a console timeline.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthorizationDecision {
    Allow,
    Deny,
    Terminal,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuthorizationDecisionSummary {
    pub decision: AuthorizationDecision,
    pub reason_code: String,
    pub evaluated_at: UtcTimestamp,
}

/// EndpointGrant projection without host, port, credential, or policy internals.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EndpointGrantSnapshot {
    pub id: EndpointGrantId,
    pub endpoint_id: EndpointId,
    pub endpoint_revision: Revision,
    pub protocol: EndpointProtocol,
    pub alias: Option<String>,
    pub state: EndpointGrantSnapshotState,
    pub expires_at: UtcTimestamp,
}

/// Actor-scoped AccessGrant snapshot used by Environment discovery APIs.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccessGrantSnapshot {
    pub id: AccessGrantId,
    pub environment_id: EnvironmentId,
    pub environment_revision: Revision,
    pub state: AccessGrantState,
    pub revision: Revision,
    pub endpoint_grants: Vec<EndpointGrantSnapshot>,
    pub issued_at: UtcTimestamp,
    pub expires_at: UtcTimestamp,
    pub revoked_at: Option<UtcTimestamp>,
    pub reason_code: Option<String>,
    pub decision: AuthorizationDecisionSummary,
    pub last_changed_stream_sequence: crate::StreamSequence,
}

impl AccessGrantSnapshot {
    pub fn validate(&self) -> Result<(), AccessError> {
        if self.expires_at <= self.issued_at
            || self.endpoint_grants.is_empty()
            || self.last_changed_stream_sequence.0 == 0
            || self.decision.reason_code.trim().is_empty()
            || self
                .endpoint_grants
                .iter()
                .any(|endpoint| endpoint.expires_at > self.expires_at)
        {
            return Err(AccessError::InvalidGrantSnapshot);
        }
        let terminal = matches!(
            self.state,
            AccessGrantState::Expired | AccessGrantState::Revoked
        );
        if terminal != matches!(self.decision.decision, AuthorizationDecision::Terminal)
            || (self.state == AccessGrantState::Revoked) != self.revoked_at.is_some()
            || (self.state == AccessGrantState::Expired
                && self.reason_code.as_deref() != Some("expired"))
        {
            return Err(AccessError::InvalidGrantSnapshot);
        }
        Ok(())
    }
}

/// Allowed endpoint action.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EndpointAction {
    Connect,
}

/// Child grant for one exact endpoint revision.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EndpointGrant {
    pub id: EndpointGrantId,
    pub access_grant_id: AccessGrantId,
    pub endpoint_id: EndpointId,
    pub endpoint_revision: Revision,
    pub protocol: EndpointProtocol,
    pub action: EndpointAction,
    pub health: EndpointHealth,
    pub alias: Option<String>,
    pub expires_at: UtcTimestamp,
}

impl EndpointGrant {
    /// Validates a server-generated, non-routing SSH alias when required.
    pub fn validate(&self) -> Result<(), AccessError> {
        if self.health != EndpointHealth::Healthy {
            return Err(AccessError::EndpointUnhealthy);
        }
        match self.protocol {
            EndpointProtocol::Ssh => {
                let alias = self.alias.as_deref().ok_or(AccessError::InvalidAlias)?;
                if alias.len() != 23
                    || !alias.starts_with("lw-")
                    || !alias[3..]
                        .bytes()
                        .all(|byte| byte.is_ascii_lowercase() || (b'2'..=b'7').contains(&byte))
                {
                    return Err(AccessError::InvalidAlias);
                }
            }
            EndpointProtocol::Http | EndpointProtocol::Https => {
                if self.alias.is_some() {
                    return Err(AccessError::InvalidAlias);
                }
            }
        }
        Ok(())
    }
}

/// Parent actor×course×environment grant.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccessGrant {
    pub id: AccessGrantId,
    pub actor_id: ActorId,
    pub course_id: CourseId,
    pub environment_id: EnvironmentId,
    pub environment_revision: Revision,
    pub state: AccessGrantState,
    pub revision: Revision,
    pub endpoint_grants: Vec<EndpointGrant>,
    pub issued_at: UtcTimestamp,
    pub expires_at: UtcTimestamp,
    pub revoked_at: Option<UtcTimestamp>,
    pub reason_code: Option<String>,
}

impl AccessGrant {
    /// Validates child scope, time bounds, and terminal-state facts.
    pub fn validate(&self) -> Result<(), AccessError> {
        if self.expires_at <= self.issued_at || self.endpoint_grants.is_empty() {
            return Err(AccessError::InvalidGrant);
        }
        let mut endpoint_ids = BTreeSet::new();
        let mut endpoint_grant_ids = BTreeSet::new();
        for endpoint in &self.endpoint_grants {
            if endpoint.access_grant_id != self.id || endpoint.expires_at > self.expires_at {
                return Err(AccessError::InvalidGrant);
            }
            if !endpoint_ids.insert(endpoint.endpoint_id) || !endpoint_grant_ids.insert(endpoint.id)
            {
                return Err(AccessError::InvalidGrant);
            }
            endpoint.validate()?;
        }
        match self.state {
            AccessGrantState::Revoked => {
                if self.revoked_at.is_none()
                    || self.reason_code.as_deref().is_none_or(str::is_empty)
                {
                    return Err(AccessError::InvalidGrant);
                }
            }
            AccessGrantState::Expired => {
                if self.reason_code.as_deref() != Some("expired") {
                    return Err(AccessError::InvalidGrant);
                }
            }
            AccessGrantState::Requested | AccessGrantState::Active => {
                if self.revoked_at.is_some() {
                    return Err(AccessError::InvalidGrant);
                }
            }
        }
        Ok(())
    }

    /// Checks the only legal state progressions.
    pub fn ensure_transition(
        from: AccessGrantState,
        to: AccessGrantState,
    ) -> Result<(), AccessError> {
        let valid = matches!(
            (from, to),
            (
                AccessGrantState::Requested,
                AccessGrantState::Active | AccessGrantState::Revoked
            ) | (
                AccessGrantState::Active,
                AccessGrantState::Expired | AccessGrantState::Revoked
            )
        );
        if valid {
            Ok(())
        } else {
            Err(AccessError::InvalidGrantTransition { from, to })
        }
    }
}

/// Request from AuthorizedKeysCommand over mTLS. It deliberately has no target field.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SshAuthorizationRequest {
    pub alias: String,
    pub gateway_identity: String,
    pub connection_id: String,
    pub source_address_hash: String,
    pub requested_at: UtcTimestamp,
}

/// Fail-closed SSH authorization result.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SshAuthorization {
    pub authorization_id: String,
    pub access_grant_id: AccessGrantId,
    pub access_grant_revision: Revision,
    pub endpoint_grant_id: EndpointGrantId,
    pub endpoint_id: EndpointId,
    pub normalized_authorized_key: String,
    pub force_command_token: String,
    pub valid_until: UtcTimestamp,
}

/// Gateway session lifecycle.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GatewaySessionState {
    Active,
    Terminating,
    Closed,
}

/// Auditable session metadata; terminal content is never recorded.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GatewaySession {
    pub id: GatewaySessionId,
    pub access_grant_id: AccessGrantId,
    pub access_grant_revision: Revision,
    pub endpoint_grant_id: EndpointGrantId,
    pub gateway_identity: String,
    pub connection_id: String,
    pub state: GatewaySessionState,
    pub opened_at: UtcTimestamp,
    pub last_heartbeat_at: UtcTimestamp,
    pub terminate_by: Option<UtcTimestamp>,
    pub closed_at: Option<UtcTimestamp>,
    pub close_reason_code: Option<String>,
}

impl GatewaySession {
    /// Validates terminal facts and the 60-second revoke/expiry termination bound.
    pub fn validate(&self) -> Result<(), AccessError> {
        if self.gateway_identity.trim().is_empty()
            || self.connection_id.trim().is_empty()
            || self.last_heartbeat_at < self.opened_at
        {
            return Err(AccessError::InvalidSession);
        }
        match self.state {
            GatewaySessionState::Active
                if self.terminate_by.is_some() || self.closed_at.is_some() =>
            {
                Err(AccessError::InvalidSession)
            }
            GatewaySessionState::Terminating => {
                let deadline = self.terminate_by.ok_or(AccessError::InvalidSession)?;
                let seconds = (deadline.get() - self.last_heartbeat_at.get()).whole_seconds();
                if (0..=60).contains(&seconds) {
                    Ok(())
                } else {
                    Err(AccessError::TerminationDeadlineExceeded)
                }
            }
            GatewaySessionState::Closed
                if self.closed_at.is_none()
                    || self.close_reason_code.as_deref().is_none_or(str::is_empty) =>
            {
                Err(AccessError::InvalidSession)
            }
            GatewaySessionState::Closed | GatewaySessionState::Active => Ok(()),
        }
    }
}

/// Access contract failure.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum AccessError {
    #[error("SSH public key is malformed")]
    InvalidKey,
    #[error("unsupported SSH public-key algorithm: {0}")]
    UnsupportedKeyAlgorithm(String),
    #[error("RSA key has {0} bits; at least 3072 are required")]
    WeakRsaKey(u32),
    #[error("SSH public-key ID or fingerprint already exists")]
    DuplicateKey,
    #[error("AccessGrant is internally inconsistent")]
    InvalidGrant,
    #[error("illegal AccessGrant transition: {from:?} -> {to:?}")]
    InvalidGrantTransition {
        from: AccessGrantState,
        to: AccessGrantState,
    },
    #[error("EndpointGrant requires a healthy endpoint")]
    EndpointUnhealthy,
    #[error("SSH alias is not a server-generated v1 alias")]
    InvalidAlias,
    #[error("GatewaySession is internally inconsistent")]
    InvalidSession,
    #[error("GatewaySession termination deadline exceeds 60 seconds")]
    TerminationDeadlineExceeded,
    #[error("public AccessGrant snapshot is internally inconsistent")]
    InvalidGrantSnapshot,
}
