//! SSH public key, AccessGrant, EndpointGrant, and Gateway session contracts.

use std::{collections::BTreeSet, fmt};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ssh_key::{Algorithm, HashAlg, PublicKey, public::KeyData};

use crate::authoring::EnvironmentClass;
use crate::environment::{EndpointHealth, EndpointProtocol};
use crate::{
    AccessGrantId, ActorId, ConsoleCapabilityId, CourseId, EndpointGrantId, EndpointId,
    EnvironmentId, GatewaySessionId, LeaseId, Revision, SshPublicKeyId, UtcTimestamp,
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
    Denied,
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

/// Browser interaction transport selected by an approved environment release.
#[derive(
    Clone, Copy, Debug, Deserialize, Eq, JsonSchema, Ord, PartialEq, PartialOrd, Serialize,
)]
#[serde(rename_all = "snake_case")]
pub enum ConsoleKind {
    Xterm,
    Novnc,
}

impl ConsoleKind {
    /// Versioned WebSocket subprotocol required for this console transport.
    #[must_use]
    pub const fn websocket_subprotocol(self) -> &'static str {
        match self {
            Self::Xterm => "labweaver.console.xterm.v1",
            Self::Novnc => "labweaver.console.novnc.v1",
        }
    }
}

/// Resource-owned Lease identity that fences a Work console capability.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConsoleLeaseFence {
    pub lease_id: LeaseId,
    pub lease_revision: Revision,
    pub expires_at: UtcTimestamp,
}

/// Public capability discovery result. It deliberately contains no locator or handoff secret.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConsoleCapabilityAvailability {
    pub access_grant_id: AccessGrantId,
    pub access_grant_revision: Revision,
    pub environment_id: EnvironmentId,
    pub environment_class: EnvironmentClass,
    pub environment_revision: Revision,
    pub expires_at: UtcTimestamp,
    pub lease_fence: Option<ConsoleLeaseFence>,
    pub kinds: Vec<ConsoleKind>,
}

impl ConsoleCapabilityAvailability {
    pub fn validate(&self) -> Result<(), AccessError> {
        let mut kinds = BTreeSet::new();
        let work = matches!(self.environment_class, EnvironmentClass::Work);
        if self.kinds.is_empty() || !self.kinds.iter().all(|kind| kinds.insert(*kind)) {
            return Err(AccessError::InvalidConsoleCapability);
        }
        match (&self.lease_fence, work) {
            (Some(fence), true) if self.expires_at <= fence.expires_at => Ok(()),
            (None, false) => Ok(()),
            _ => Err(AccessError::InvalidConsoleCapability),
        }
    }
}

/// One-time browser console handoff. The secret is an HttpOnly cookie, never a field here.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConsoleCapability {
    pub id: ConsoleCapabilityId,
    pub kind: ConsoleKind,
    pub access_grant_id: AccessGrantId,
    pub access_grant_revision: Revision,
    pub environment_id: EnvironmentId,
    pub environment_class: EnvironmentClass,
    pub environment_revision: Revision,
    pub lease_fence: Option<ConsoleLeaseFence>,
    pub issued_at: UtcTimestamp,
    pub expires_at: UtcTimestamp,
    pub connection_locator: String,
    pub websocket_subprotocol: String,
}

impl ConsoleCapability {
    pub fn validate(&self) -> Result<(), AccessError> {
        let work = matches!(self.environment_class, EnvironmentClass::Work);
        let lifetime_is_exact =
            self.expires_at.get() - self.issued_at.get() == time::Duration::seconds(30);
        if !lifetime_is_exact
            || !is_console_connection_locator(&self.connection_locator)
            || self.websocket_subprotocol != self.kind.websocket_subprotocol()
        {
            return Err(AccessError::InvalidConsoleCapability);
        }
        match (&self.lease_fence, work) {
            (Some(fence), true) if self.expires_at <= fence.expires_at => Ok(()),
            (None, false) => Ok(()),
            _ => Err(AccessError::InvalidConsoleCapability),
        }
    }

    /// Ensures an issued handoff remains within discovery and Lease authority.
    pub fn validate_against(
        &self,
        availability: &ConsoleCapabilityAvailability,
    ) -> Result<(), AccessError> {
        self.validate()?;
        availability.validate()?;
        if !availability.kinds.contains(&self.kind)
            || self.access_grant_id != availability.access_grant_id
            || self.access_grant_revision != availability.access_grant_revision
            || self.environment_id != availability.environment_id
            || self.environment_class != availability.environment_class
            || self.environment_revision != availability.environment_revision
            || self.lease_fence != availability.lease_fence
            || self.expires_at > availability.expires_at
        {
            return Err(AccessError::InvalidConsoleCapability);
        }
        Ok(())
    }
}

fn is_console_connection_locator(value: &str) -> bool {
    const PREFIX: &str = "/connect/console/";
    let Some(segment) = value.strip_prefix(PREFIX) else {
        return false;
    };
    (1..=128).contains(&segment.len())
        && segment
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
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
            || (matches!(
                self.state,
                AccessGrantState::Active | AccessGrantState::Expired
            ) && self.endpoint_grants.is_empty())
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
            AccessGrantState::Denied | AccessGrantState::Expired | AccessGrantState::Revoked
        );
        if terminal != matches!(self.decision.decision, AuthorizationDecision::Terminal)
            || (self.state == AccessGrantState::Revoked) != self.revoked_at.is_some()
            || (matches!(
                self.state,
                AccessGrantState::Denied | AccessGrantState::Revoked
            ) && self.reason_code.as_deref().is_none_or(str::is_empty))
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

fn valid_dns_name(value: &str) -> bool {
    value.len() <= 253
        && value.contains('.')
        && value.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        })
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
    /// Same-origin, Access Service-authorized browser entry point. Present only
    /// for HTTP(S) grants and derived from this immutable endpoint grant ID.
    pub connect_url: Option<String>,
    /// Public OpenSSH Gateway DNS name for SSH grants. Never a runtime target.
    pub ssh_gateway_hostname: Option<String>,
    /// Public OpenSSH Gateway listener port for SSH grants.
    pub ssh_gateway_port: Option<u16>,
    /// OpenSSH SHA-256 host-key fingerprint for the public Gateway.
    pub ssh_gateway_host_key_fingerprint: Option<String>,
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
                if self.connect_url.is_some() {
                    return Err(AccessError::InvalidConnectUrl);
                }
                let hostname = self
                    .ssh_gateway_hostname
                    .as_deref()
                    .ok_or(AccessError::InvalidGatewayEndpoint)?;
                if !valid_dns_name(hostname)
                    || self.ssh_gateway_port != Some(2222)
                    || self
                        .ssh_gateway_host_key_fingerprint
                        .as_deref()
                        .is_none_or(|value| {
                            value.len() != 50
                                || !value.starts_with("SHA256:")
                                || !value.bytes().skip(7).all(|byte| {
                                    byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/')
                                })
                        })
                {
                    return Err(AccessError::InvalidGatewayEndpoint);
                }
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
                if self.ssh_gateway_hostname.is_some()
                    || self.ssh_gateway_port.is_some()
                    || self.ssh_gateway_host_key_fingerprint.is_some()
                {
                    return Err(AccessError::InvalidGatewayEndpoint);
                }
                let expected = format!("/connect/{}/", self.id);
                if self.connect_url.as_deref() != Some(expected.as_str()) {
                    return Err(AccessError::InvalidConnectUrl);
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
        if self.expires_at <= self.issued_at
            || (matches!(
                self.state,
                AccessGrantState::Active | AccessGrantState::Expired
            ) && self.endpoint_grants.is_empty())
        {
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
            AccessGrantState::Denied | AccessGrantState::Revoked => {
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
                AccessGrantState::Active | AccessGrantState::Denied | AccessGrantState::Revoked
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

/// Request from `AuthorizedKeysCommand` over mTLS. Target selection happens only after
/// public-key authentication, through the fixed command grammar.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SshAuthorizationRequest {
    pub presented_key_fingerprint_sha256: String,
    pub gateway_identity: String,
    pub connection_id: String,
    pub source_address_hash: String,
    pub requested_at: UtcTimestamp,
}

/// Fail-closed SSH authorization result.
#[derive(Clone, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SshAuthorization {
    pub authorization_id: String,
    pub ssh_public_key_id: SshPublicKeyId,
    pub normalized_authorized_key: String,
    pub force_command_token: String,
    pub valid_until: UtcTimestamp,
}

impl fmt::Debug for SshAuthorization {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SshAuthorization")
            .field("authorization_id", &self.authorization_id)
            .field("ssh_public_key_id", &self.ssh_public_key_id)
            .field("normalized_authorized_key", &"[REDACTED]")
            .field("force_command_token", &"[REDACTED]")
            .field("valid_until", &self.valid_until)
            .finish()
    }
}

/// Redeems exactly one SSH authorization into a tracked Gateway session.
#[derive(Clone, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateGatewaySessionRequest {
    pub authorization_id: String,
    pub force_command_token: String,
    /// Exact server-generated SSH endpoint alias parsed from `connect <alias>`.
    pub alias: String,
    pub gateway_identity: String,
    pub connection_id: String,
    pub opened_at: UtcTimestamp,
}

impl fmt::Debug for CreateGatewaySessionRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CreateGatewaySessionRequest")
            .field("authorization_id", &self.authorization_id)
            .field("force_command_token", &"[REDACTED]")
            .field("alias", &self.alias)
            .field("gateway_identity", &self.gateway_identity)
            .field("connection_id", &self.connection_id)
            .field("opened_at", &self.opened_at)
            .finish()
    }
}

/// Revision-checked heartbeat emitted only by the owning Gateway connection.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HeartbeatGatewaySessionRequest {
    pub gateway_identity: String,
    pub connection_id: String,
    pub expected_revision: Revision,
    pub observed_at: UtcTimestamp,
}

/// Terminal receipt from the Gateway. Terminal content is never accepted.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CloseGatewaySessionRequest {
    pub gateway_identity: String,
    pub connection_id: String,
    pub expected_revision: Revision,
    pub closed_at: UtcTimestamp,
    pub reason_code: String,
}

/// Gateway session lifecycle.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GatewaySessionState {
    Active,
    Terminating,
    TerminationOverdue,
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
    pub ssh_public_key_id: SshPublicKeyId,
    /// Server-generated DNS alias selected by the fixed `connect` command.
    pub target_alias: String,
    /// Environment-owned in-cluster SSH Service DNS name. This is returned only
    /// to the mTLS-authenticated Gateway and never to the browser grant model.
    pub target_host: String,
    /// Environment-authoritative digest of the exact OpenSSH host-key
    /// fingerprint expected from the selected target.
    pub target_ssh_host_key_identity_sha256: crate::Sha256Digest,
    pub gateway_identity: String,
    pub connection_id: String,
    pub revision: Revision,
    pub state: GatewaySessionState,
    pub opened_at: UtcTimestamp,
    pub last_heartbeat_at: UtcTimestamp,
    pub termination_requested_at: Option<UtcTimestamp>,
    pub terminate_by: Option<UtcTimestamp>,
    pub closed_at: Option<UtcTimestamp>,
    pub close_reason_code: Option<String>,
}

impl GatewaySession {
    /// Validates terminal facts and the 60-second revoke/expiry termination bound.
    pub fn validate(&self) -> Result<(), AccessError> {
        if self.gateway_identity.trim().is_empty()
            || self.connection_id.trim().is_empty()
            || self.target_alias.len() != 23
            || !self.target_alias.starts_with("lw-")
            || !self
                .target_alias
                .bytes()
                .skip(3)
                .all(|byte| byte.is_ascii_lowercase() || matches!(byte, b'2'..=b'7'))
            || !valid_gateway_target_host(&self.target_host)
            || self.last_heartbeat_at < self.opened_at
        {
            return Err(AccessError::InvalidSession);
        }
        match self.state {
            GatewaySessionState::Active
                if self.termination_requested_at.is_some()
                    || self.terminate_by.is_some()
                    || self.closed_at.is_some() =>
            {
                Err(AccessError::InvalidSession)
            }
            GatewaySessionState::Terminating => {
                let requested = self
                    .termination_requested_at
                    .ok_or(AccessError::InvalidSession)?;
                let deadline = self.terminate_by.ok_or(AccessError::InvalidSession)?;
                let seconds = (deadline.get() - requested.get()).whole_seconds();
                if (0..=60).contains(&seconds) {
                    Ok(())
                } else {
                    Err(AccessError::TerminationDeadlineExceeded)
                }
            }
            GatewaySessionState::TerminationOverdue
                if self.termination_requested_at.is_none()
                    || self.terminate_by.is_none()
                    || self.closed_at.is_some()
                    || self.close_reason_code.as_deref() != Some("termination_overdue") =>
            {
                Err(AccessError::InvalidSession)
            }
            GatewaySessionState::Closed
                if self.closed_at.is_none()
                    || self.close_reason_code.as_deref().is_none_or(str::is_empty) =>
            {
                Err(AccessError::InvalidSession)
            }
            GatewaySessionState::TerminationOverdue
            | GatewaySessionState::Closed
            | GatewaySessionState::Active => Ok(()),
        }
    }
}

fn valid_gateway_target_host(value: &str) -> bool {
    value
        .strip_prefix("ssh.lw-env-")
        .and_then(|value| value.strip_suffix(".svc"))
        .is_some_and(|environment_id| uuid::Uuid::parse_str(environment_id).is_ok())
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
    #[error("HTTP endpoint connect URL is not the server-generated v1 path")]
    InvalidConnectUrl,
    #[error("SSH Gateway endpoint is not the reviewed Sprint 2 binding")]
    InvalidGatewayEndpoint,
    #[error("GatewaySession is internally inconsistent")]
    InvalidSession,
    #[error("GatewaySession termination deadline exceeds 60 seconds")]
    TerminationDeadlineExceeded,
    #[error("public AccessGrant snapshot is internally inconsistent")]
    InvalidGrantSnapshot,
    #[error("ConsoleCapability is internally inconsistent")]
    InvalidConsoleCapability,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CourseId, EnvironmentId};

    fn timestamp(value: &str) -> UtcTimestamp {
        serde_json::from_str(&format!("\"{value}\""))
            .unwrap_or_else(|error| unreachable!("static timestamp must parse: {error}"))
    }

    fn revision(value: u64) -> Revision {
        Revision::new(value).unwrap_or_else(|error| unreachable!("valid revision: {error}"))
    }

    #[test]
    fn ed25519_key_is_normalized_and_multiline_input_is_rejected() {
        let key = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA user@example";
        let validated = validate_ssh_public_key(key)
            .unwrap_or_else(|error| unreachable!("static Ed25519 key must parse: {error}"));
        assert_eq!(validated.algorithm, SshKeyAlgorithm::Ed25519);
        assert!(!validated.normalized_openssh.contains("user@example"));
        assert!(validate_ssh_public_key(&format!("{key}\n{key}")).is_err());
    }

    #[test]
    fn grant_state_machine_is_closed_and_requested_may_have_no_materialized_endpoints() {
        assert!(
            AccessGrant::ensure_transition(AccessGrantState::Requested, AccessGrantState::Active)
                .is_ok()
        );
        assert!(
            AccessGrant::ensure_transition(AccessGrantState::Requested, AccessGrantState::Denied)
                .is_ok()
        );
        assert!(
            AccessGrant::ensure_transition(AccessGrantState::Active, AccessGrantState::Expired)
                .is_ok()
        );
        assert!(
            AccessGrant::ensure_transition(AccessGrantState::Denied, AccessGrantState::Active)
                .is_err()
        );
        let requested = AccessGrant {
            id: AccessGrantId::new(),
            actor_id: ActorId::new(),
            course_id: CourseId::new(),
            environment_id: EnvironmentId::new(),
            environment_revision: revision(1),
            state: AccessGrantState::Requested,
            revision: revision(1),
            endpoint_grants: Vec::new(),
            issued_at: timestamp("2026-07-16T00:00:00.000Z"),
            expires_at: timestamp("2026-07-16T00:30:00.000Z"),
            revoked_at: None,
            reason_code: None,
        };
        assert!(requested.validate().is_ok());
        let mut active = requested;
        active.state = AccessGrantState::Active;
        assert!(active.validate().is_err());
    }

    #[test]
    fn http_endpoint_grant_requires_its_exact_same_origin_connect_path() {
        let id = EndpointGrantId::new();
        let mut endpoint = EndpointGrant {
            id,
            access_grant_id: AccessGrantId::new(),
            endpoint_id: EndpointId::new(),
            endpoint_revision: revision(1),
            protocol: EndpointProtocol::Https,
            action: EndpointAction::Connect,
            health: EndpointHealth::Healthy,
            alias: None,
            connect_url: Some(format!("/connect/{id}/")),
            ssh_gateway_hostname: None,
            ssh_gateway_port: None,
            ssh_gateway_host_key_fingerprint: None,
            expires_at: timestamp("2026-07-16T00:30:00.000Z"),
        };
        assert!(endpoint.validate().is_ok());
        endpoint.connect_url = Some("https://runtime.invalid/".to_owned());
        assert_eq!(endpoint.validate(), Err(AccessError::InvalidConnectUrl));

        endpoint.protocol = EndpointProtocol::Ssh;
        endpoint.alias = Some("lw-abcdefghijklmnopqrst".to_owned());
        endpoint.connect_url = None;
        endpoint.ssh_gateway_hostname = Some("demo.lab.lan".to_owned());
        endpoint.ssh_gateway_port = Some(2222);
        endpoint.ssh_gateway_host_key_fingerprint =
            Some("SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".to_owned());
        assert!(endpoint.validate().is_ok());
        endpoint.ssh_gateway_port = Some(22);
        assert_eq!(
            endpoint.validate(),
            Err(AccessError::InvalidGatewayEndpoint)
        );
    }

    #[test]
    fn termination_deadline_and_overdue_state_are_explicit() {
        let opened = timestamp("2026-07-16T00:00:00.000Z");
        let heartbeat = timestamp("2026-07-16T00:01:00.000Z");
        let mut session = GatewaySession {
            id: GatewaySessionId::new(),
            access_grant_id: AccessGrantId::new(),
            access_grant_revision: revision(2),
            endpoint_grant_id: EndpointGrantId::new(),
            ssh_public_key_id: SshPublicKeyId::new(),
            target_alias: "lw-abcdefghijklmnopqrst".to_owned(),
            target_host: format!("ssh.lw-env-{}.svc", uuid::Uuid::now_v7()),
            target_ssh_host_key_identity_sha256: crate::Sha256Digest::of_bytes(
                b"SHA256:target-host-key",
            ),
            gateway_identity: "spiffe://labweaver/gateway".to_owned(),
            connection_id: "connection-1".to_owned(),
            revision: revision(3),
            state: GatewaySessionState::Terminating,
            opened_at: opened,
            last_heartbeat_at: heartbeat,
            termination_requested_at: Some(heartbeat),
            terminate_by: Some(timestamp("2026-07-16T00:02:00.000Z")),
            closed_at: None,
            close_reason_code: None,
        };
        assert!(session.validate().is_ok());
        session.terminate_by = Some(timestamp("2026-07-16T00:02:01.000Z"));
        assert!(matches!(
            session.validate(),
            Err(AccessError::TerminationDeadlineExceeded)
        ));
        session.state = GatewaySessionState::TerminationOverdue;
        session.close_reason_code = Some("termination_overdue".to_owned());
        assert!(session.validate().is_ok());
    }

    #[test]
    fn authorization_debug_output_redacts_key_and_one_time_token() {
        let authorization = SshAuthorization {
            authorization_id: "authorization-1".to_owned(),
            ssh_public_key_id: SshPublicKeyId::new(),
            normalized_authorized_key: "ssh-ed25519 SECRET_KEY_BODY".to_owned(),
            force_command_token: "secret-token".to_owned(),
            valid_until: timestamp("2026-07-16T00:00:30.000Z"),
        };
        let rendered = format!("{authorization:?}");
        assert!(!rendered.contains("SECRET_KEY_BODY"));
        assert!(!rendered.contains("secret-token"));
        assert_eq!(rendered.matches("[REDACTED]").count(), 2);
    }

    #[test]
    fn console_capability_fences_exact_lifetime_lease_and_opaque_locator() {
        let issued_at = timestamp("2026-07-29T00:00:00.000Z");
        let expires_at = timestamp("2026-07-29T00:00:30.000Z");
        let capability = ConsoleCapability {
            id: ConsoleCapabilityId::new(),
            kind: ConsoleKind::Novnc,
            access_grant_id: AccessGrantId::new(),
            access_grant_revision: revision(2),
            environment_id: EnvironmentId::new(),
            environment_class: EnvironmentClass::Experiment,
            environment_revision: revision(3),
            lease_fence: None,
            issued_at,
            expires_at,
            connection_locator: "/connect/console/opaque-handoff".to_owned(),
            websocket_subprotocol: "labweaver.console.novnc.v1".to_owned(),
        };
        assert!(capability.validate().is_ok());

        for locator in [
            "https://runtime.invalid/connect",
            "/connect/console/",
            "/connect/console/one/two",
            "/connect/console/opaque?query",
            "/connect/console/opaque.handoff",
        ] {
            let mut invalid = capability.clone();
            invalid.connection_locator = locator.to_owned();
            assert_eq!(
                invalid.validate(),
                Err(AccessError::InvalidConsoleCapability),
                "{locator} must not be accepted as an opaque locator"
            );
        }

        let mut millisecond_drift = capability.clone();
        millisecond_drift.expires_at = timestamp("2026-07-29T00:00:30.001Z");
        assert_eq!(
            millisecond_drift.validate(),
            Err(AccessError::InvalidConsoleCapability)
        );

        let mut wrong_subprotocol = capability.clone();
        wrong_subprotocol.websocket_subprotocol =
            ConsoleKind::Xterm.websocket_subprotocol().to_owned();
        assert_eq!(
            wrong_subprotocol.validate(),
            Err(AccessError::InvalidConsoleCapability)
        );

        let mut availability = ConsoleCapabilityAvailability {
            access_grant_id: capability.access_grant_id,
            access_grant_revision: capability.access_grant_revision,
            environment_id: capability.environment_id,
            environment_class: EnvironmentClass::Experiment,
            environment_revision: capability.environment_revision,
            expires_at,
            lease_fence: None,
            kinds: vec![ConsoleKind::Novnc],
        };
        assert!(capability.validate_against(&availability).is_ok());

        let mut less_than_thirty_seconds = availability.clone();
        less_than_thirty_seconds.expires_at = timestamp("2026-07-29T00:00:29.999Z");
        assert_eq!(
            capability.validate_against(&less_than_thirty_seconds),
            Err(AccessError::InvalidConsoleCapability),
            "issuers must reject rather than shorten a handoff when less than 30 seconds remain"
        );

        availability.kinds.push(ConsoleKind::Novnc);
        assert_eq!(
            availability.validate(),
            Err(AccessError::InvalidConsoleCapability)
        );

        let mut work_capability = capability.clone();
        work_capability.environment_class = EnvironmentClass::Work;
        let fence = ConsoleLeaseFence {
            lease_id: LeaseId::new(),
            lease_revision: revision(4),
            expires_at,
        };
        work_capability.lease_fence = Some(fence.clone());
        assert!(work_capability.validate().is_ok());

        let mut expired_by_lease = work_capability.clone();
        expired_by_lease.expires_at = timestamp("2026-07-29T00:00:30.001Z");
        assert_eq!(
            expired_by_lease.validate(),
            Err(AccessError::InvalidConsoleCapability)
        );

        let mut invalid_experiment = capability.clone();
        invalid_experiment.lease_fence = Some(fence.clone());
        assert_eq!(
            invalid_experiment.validate(),
            Err(AccessError::InvalidConsoleCapability)
        );

        let lease_limited_availability = ConsoleCapabilityAvailability {
            environment_class: EnvironmentClass::Work,
            expires_at: fence.expires_at,
            lease_fence: Some(fence),
            kinds: vec![ConsoleKind::Novnc],
            ..availability
        };
        assert!(
            work_capability
                .validate_against(&lease_limited_availability)
                .is_ok()
        );

        let mut availability_past_lease = lease_limited_availability;
        availability_past_lease.expires_at = timestamp("2026-07-29T00:00:30.001Z");
        assert_eq!(
            availability_past_lease.validate(),
            Err(AccessError::InvalidConsoleCapability)
        );
    }
}
