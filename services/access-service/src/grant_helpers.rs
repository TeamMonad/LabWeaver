//! Pure grant-domain utility helpers extracted from grants.rs.
//!
//! Validation, hashing, and naming/parsing helpers that have no SQL or state dependency.

use std::str::FromStr;

use contracts::{
    EndpointGrantId,
    access::{AccessGrantState, GatewaySessionState},
    environment::{EndpointHealth, EndpointProtocol},
};
use rand::RngCore;
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::ApiError;

/// Validates that the alias looks like a system-generated format (not user-supplied).
pub(crate) fn validate_alias(alias: &str) -> Result<(), ApiError> {
    if alias.len() == 23
        && alias.starts_with("lw-")
        && alias[3..]
            .bytes()
            .all(|b| b.is_ascii_lowercase() || (b'2'..=b'7').contains(&b))
    {
        Ok(())
    } else {
        Err(ApiError::forbidden("LW_ACCESS_ALIAS_INVALID"))
    }
}

/// Checks that the fingerprint starts with the right prefix and has valid base64 content.
#[must_use]
pub(crate) fn valid_fingerprint(value: &str) -> bool {
    value.starts_with("SHA256:")
        && value.len() <= 96
        && value[7..]
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'='))
}

/// Checks for exactly 32 lowercase hex bytes.
#[must_use]
pub(crate) fn valid_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// Generate a random hex token for use as an authorization secret.
#[must_use]
pub(crate) fn random_token() -> String {
    let mut bytes = [0_u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    hex(&bytes)
}

/// SHA-256 digest rendered as lowercase hexadecimal.
#[must_use]
pub(crate) fn sha256_hex(value: &[u8]) -> String {
    hex(&Sha256::digest(value))
}

/// Generic hex encoding helper.
#[must_use]
pub(crate) fn hex(value: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(value.len() * 2);
    for byte in value {
        output.push(char::from(DIGITS[usize::from(byte >> 4)]));
        output.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    output
}

/// Generate a stable lowercase SSH alias from an endpoint grant ID.
#[must_use]
pub(crate) fn ssh_alias(id: EndpointGrantId) -> String {
    let alphabet = b"abcdefghijklmnopqrstuvwxyz234567";
    let uuid = id.as_uuid();
    let bytes = uuid.as_bytes();
    let mut out = String::from("lw-");
    let mut acc = 0_u32;
    let mut bits = 0_u8;
    for byte in bytes {
        acc = (acc << 8) | u32::from(*byte);
        bits += 8;
        while bits >= 5 && out.len() < 23 {
            bits -= 5;
            out.push(alphabet[((acc >> bits) & 31) as usize] as char);
        }
    }
    while out.len() < 23 {
        out.push('a');
    }
    out
}

/// Serialize endpoint protocol to its stable string name.
#[must_use]
pub(crate) fn protocol_str(p: EndpointProtocol) -> &'static str {
    match p {
        EndpointProtocol::Http => "http",
        EndpointProtocol::Https => "https",
        EndpointProtocol::Ssh => "ssh",
    }
}

/// Parse endpoint protocol from its string name.
pub(crate) fn parse_protocol(v: &str) -> Result<EndpointProtocol, ApiError> {
    match v {
        "http" => Ok(EndpointProtocol::Http),
        "https" => Ok(EndpointProtocol::Https),
        "ssh" => Ok(EndpointProtocol::Ssh),
        _ => Err(ApiError::internal("LW_ACCESS_STORE_CORRUPT")),
    }
}

/// Parse endpoint health from its string name.
pub(crate) fn parse_health(v: &str) -> Result<EndpointHealth, ApiError> {
    match v {
        "healthy" => Ok(EndpointHealth::Healthy),
        "unhealthy" => Ok(EndpointHealth::Unhealthy),
        "removed" => Ok(EndpointHealth::Removed),
        _ => Err(ApiError::internal("LW_ACCESS_STORE_CORRUPT")),
    }
}

/// Extract an optional string field from a JSON contract object.
pub(crate) fn optional_contract_string(
    contract: &Value,
    field: &str,
) -> Result<Option<String>, ApiError> {
    match contract.get(field) {
        Some(Value::String(value)) if !value.is_empty() => Ok(Some(value.clone())),
        Some(Value::Null) | None => Ok(None),
        _ => Err(ApiError::internal("LW_ACCESS_STORE_CORRUPT")),
    }
}

/// Extract an optional u16 field from a JSON contract object.
pub(crate) fn optional_contract_u16(
    contract: &Value,
    field: &str,
) -> Result<Option<u16>, ApiError> {
    match contract.get(field) {
        Some(Value::Number(value)) => value
            .as_u64()
            .and_then(|value| u16::try_from(value).ok())
            .map(Some)
            .ok_or_else(|| ApiError::internal("LW_ACCESS_STORE_CORRUPT")),
        Some(Value::Null) | None => Ok(None),
        _ => Err(ApiError::internal("LW_ACCESS_STORE_CORRUPT")),
    }
}

/// Serialize access grant state to its stable string name.
#[must_use]
pub(crate) fn grant_state_str(s: AccessGrantState) -> &'static str {
    match s {
        AccessGrantState::Requested => "requested",
        AccessGrantState::Active => "active",
        AccessGrantState::Denied => "denied",
        AccessGrantState::Expired => "expired",
        AccessGrantState::Revoked => "revoked",
    }
}

/// Parse access grant state from its string name.
pub(crate) fn parse_grant_state(v: &str) -> Result<AccessGrantState, ApiError> {
    match v {
        "requested" => Ok(AccessGrantState::Requested),
        "active" => Ok(AccessGrantState::Active),
        "denied" => Ok(AccessGrantState::Denied),
        "expired" => Ok(AccessGrantState::Expired),
        "revoked" => Ok(AccessGrantState::Revoked),
        _ => Err(ApiError::internal("LW_ACCESS_STORE_CORRUPT")),
    }
}

/// Serialize gateway session state to its stable string name.
#[must_use]
pub(crate) fn session_state_str(s: GatewaySessionState) -> &'static str {
    match s {
        GatewaySessionState::Active => "active",
        GatewaySessionState::Terminating => "terminating",
        GatewaySessionState::TerminationOverdue => "termination_overdue",
        GatewaySessionState::Closed => "closed",
    }
}

/// Parse gateway session state from its string name.
pub(crate) fn parse_session_state(v: &str) -> Result<GatewaySessionState, ApiError> {
    match v {
        "active" => Ok(GatewaySessionState::Active),
        "terminating" => Ok(GatewaySessionState::Terminating),
        "termination_overdue" => Ok(GatewaySessionState::TerminationOverdue),
        "closed" => Ok(GatewaySessionState::Closed),
        _ => Err(ApiError::internal("LW_ACCESS_STORE_CORRUPT")),
    }
}

/// Convert a UUID to any type that implements `FromStr`, with store-corrupt error.
pub(crate) fn typed_id<T: FromStr>(v: uuid::Uuid) -> Result<T, ApiError> {
    v.to_string()
        .parse()
        .map_err(|_| ApiError::internal("LW_ACCESS_STORE_CORRUPT"))
}
