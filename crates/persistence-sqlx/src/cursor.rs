//! Canonical, scope-bound HMAC cursors using the existing service cursor envelope.
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest as _, Sha256};
use subtle::ConstantTimeEq as _;

/// Invalid, noncanonical, tampered or out-of-scope continuation.
#[derive(Debug, thiserror::Error)]
#[error("invalid cursor")]
pub struct CursorError;

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Cursor<T> {
    version: u8,
    key_id: String,
    scope: serde_json::Value,
    position: T,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SignedCursor<T> {
    cursor: Cursor<T>,
    signature: String,
}

/// Signs an immutable boundary and the full endpoint/actor/object scope.
///
/// # Errors
/// Returns an error if serialization fails or the cursor exceeds the contract bound.
pub fn encode_cursor<T: Serialize>(
    domain: &str,
    current: &[u8],
    scope: &impl Serialize,
    position: T,
) -> Result<String, CursorError> {
    let cursor = Cursor {
        version: 1,
        key_id: crate::cursor_key_id(domain, current),
        scope: serde_json::to_value(scope).map_err(|_| CursorError)?,
        position,
    };
    let payload = serde_json::to_vec(&cursor).map_err(|_| CursorError)?;
    let signed = SignedCursor {
        cursor,
        signature: URL_SAFE_NO_PAD.encode(hmac_sha256(current, &payload)),
    };
    let value = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&signed).map_err(|_| CursorError)?);
    if value.len() > contracts::http::MAX_CURSOR_LENGTH {
        return Err(CursorError);
    }
    Ok(value)
}

/// Verifies canonical encoding, current/previous key, signature and exact scope.
///
/// # Errors
/// Returns an error for any invalid or out-of-scope cursor.
pub fn decode_cursor<T: DeserializeOwned + Serialize>(
    domain: &str,
    current: &[u8],
    previous: Option<&[u8]>,
    scope: &impl Serialize,
    value: &str,
) -> Result<T, CursorError> {
    if value.is_empty() || value.len() > contracts::http::MAX_CURSOR_LENGTH {
        return Err(CursorError);
    }
    let bytes = URL_SAFE_NO_PAD.decode(value).map_err(|_| CursorError)?;
    let signed: SignedCursor<T> = serde_json::from_slice(&bytes).map_err(|_| CursorError)?;
    if URL_SAFE_NO_PAD.encode(&bytes) != value
        || serde_json::to_vec(&signed).map_err(|_| CursorError)? != bytes
    {
        return Err(CursorError);
    }
    let key = if signed.cursor.key_id == crate::cursor_key_id(domain, current) {
        current
    } else {
        previous
            .filter(|key| signed.cursor.key_id == crate::cursor_key_id(domain, key))
            .ok_or(CursorError)?
    };
    let payload = serde_json::to_vec(&signed.cursor).map_err(|_| CursorError)?;
    let signature = URL_SAFE_NO_PAD
        .decode(&signed.signature)
        .map_err(|_| CursorError)?;
    if signature.len() != 32
        || !bool::from(
            signature
                .as_slice()
                .ct_eq(hmac_sha256(key, &payload).as_slice()),
        )
        || signed.cursor.version != 1
        || signed.cursor.scope != serde_json::to_value(scope).map_err(|_| CursorError)?
    {
        return Err(CursorError);
    }
    Ok(signed.cursor.position)
}

fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    let mut block = [0_u8; 64];
    if key.len() > block.len() {
        block[..32].copy_from_slice(Sha256::digest(key).as_slice());
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let mut inner = [0x36_u8; 64];
    let mut outer = [0x5c_u8; 64];
    for index in 0..64 {
        inner[index] ^= block[index];
        outer[index] ^= block[index];
    }
    let mut inner_hash = Sha256::new();
    inner_hash.update(inner);
    inner_hash.update(message);
    let mut result = Sha256::new();
    result.update(outer);
    result.update(inner_hash.finalize());
    result.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_matches_rfc4231() {
        assert_eq!(
            hmac_sha256(&[0x0b; 20], b"Hi There"),
            [
                0xb0, 0x34, 0x4c, 0x61, 0xd8, 0xdb, 0x38, 0x53, 0x5c, 0xa8, 0xaf, 0xce, 0xaf, 0x0b,
                0xf1, 0x2b, 0x88, 0x1d, 0xc2, 0x00, 0xc9, 0x83, 0x3d, 0xa7, 0x26, 0xe9, 0x37, 0x6c,
                0x2e, 0x32, 0xcf, 0xf7,
            ]
        );
    }

    #[test]
    fn signed_scope_and_boundary_cannot_be_reencoded_or_reused() -> Result<(), CursorError> {
        let current = [7; 32];
        let scope = ("attempts", "actor", "project", "run");
        let value = encode_cursor("agent", &current, &scope, (3_u32, 2_u64))?;
        assert_eq!(
            decode_cursor::<(u32, u64)>("agent", &current, None, &scope, &value)?,
            (3, 2)
        );
        for foreign in [
            ("other", "actor", "project", "run"),
            ("attempts", "other", "project", "run"),
            ("attempts", "actor", "other", "run"),
            ("attempts", "actor", "project", "other"),
        ] {
            assert!(
                decode_cursor::<(u32, u64)>("agent", &current, None, &foreign, &value).is_err()
            );
        }
        let bytes = URL_SAFE_NO_PAD.decode(&value).map_err(|_| CursorError)?;
        let mut signed: SignedCursor<(u32, u64)> =
            serde_json::from_slice(&bytes).map_err(|_| CursorError)?;
        signed.cursor.position = (1, 1);
        let forged = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&signed).map_err(|_| CursorError)?);
        assert!(decode_cursor::<(u32, u64)>("agent", &current, None, &scope, &forged).is_err());
        let rotated = [8; 32];
        assert!(
            decode_cursor::<(u32, u64)>("agent", &rotated, Some(&current), &scope, &value).is_ok()
        );
        assert!(decode_cursor::<(u32, u64)>("agent", &rotated, None, &scope, &value).is_err());
        assert!(decode_cursor::<(u32, u64)>("evaluation", &current, None, &scope, &value).is_err());
        Ok(())
    }
}
