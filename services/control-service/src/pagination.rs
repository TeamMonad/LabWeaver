//! Self-contained UUID keysets authenticated like Agent/Resource signed cursors.
use super::{ControlError, db};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use contracts::http::CursorPage;
use contracts::membership_governance::ControlListQuery;
use persistence_sqlx::cursor_key_id;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use sqlx::{Row, postgres::PgRow};
use subtle::ConstantTimeEq as _;
use uuid::Uuid;

const CURSOR_VERSION: u8 = 1;

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Cursor {
    version: u8,
    key_id: String,
    id: Uuid,
    // The complete endpoint/actor/object scope tuple, not an unauthenticated digest.
    scope: Value,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SignedCursor {
    cursor: Cursor,
    signature: String,
}

pub(super) struct SigningKeys {
    current: Vec<u8>,
    current_id: String,
    previous: Option<(Vec<u8>, String)>,
}

impl SigningKeys {
    pub fn new(current: Vec<u8>, previous: Option<Vec<u8>>) -> Result<Self, ControlError> {
        if current.len() < 32 || previous.as_ref().is_some_and(|key| key.len() < 32) {
            return Err(ControlError::ConfigurationInvalid);
        }
        let current_id = cursor_key_id("control", &current);
        let previous = previous.map(|key| {
            let id = cursor_key_id("control", &key);
            (key, id)
        });
        Ok(Self {
            current,
            current_id,
            previous,
        })
    }

    fn decode(&self, value: &str, scope: &Value) -> Result<Uuid, ControlError> {
        let bytes = URL_SAFE_NO_PAD
            .decode(value)
            .map_err(|_| ControlError::CursorInvalid)?;
        if URL_SAFE_NO_PAD.encode(&bytes) != value {
            return Err(ControlError::CursorInvalid);
        }
        let signed: SignedCursor =
            serde_json::from_slice(&bytes).map_err(|_| ControlError::CursorInvalid)?;
        if serde_json::to_vec(&signed).map_err(|_| ControlError::CursorInvalid)? != bytes {
            return Err(ControlError::CursorInvalid);
        }
        let key = if signed.cursor.key_id == self.current_id {
            &self.current
        } else {
            &self
                .previous
                .as_ref()
                .filter(|(_, id)| *id == signed.cursor.key_id)
                .ok_or(ControlError::CursorInvalid)?
                .0
        };
        let payload =
            serde_json::to_vec(&signed.cursor).map_err(|_| ControlError::CursorInvalid)?;
        let signature = URL_SAFE_NO_PAD
            .decode(&signed.signature)
            .map_err(|_| ControlError::CursorInvalid)?;
        if signature.len() != 32
            || !bool::from(
                signature
                    .as_slice()
                    .ct_eq(hmac_sha256(key, &payload).as_slice()),
            )
            || signed.cursor.version != CURSOR_VERSION
            || signed.cursor.scope != *scope
        {
            return Err(ControlError::CursorInvalid);
        }
        Ok(signed.cursor.id)
    }

    fn encode(&self, id: Uuid, scope: Value) -> Result<String, ControlError> {
        let cursor = Cursor {
            version: CURSOR_VERSION,
            key_id: self.current_id.clone(),
            id,
            scope,
        };
        let payload = serde_json::to_vec(&cursor).map_err(|_| ControlError::ContractInvalid)?;
        let signed = SignedCursor {
            cursor,
            signature: URL_SAFE_NO_PAD.encode(hmac_sha256(&self.current, &payload)),
        };
        let value = URL_SAFE_NO_PAD
            .encode(serde_json::to_vec(&signed).map_err(|_| ControlError::ContractInvalid)?);
        if value.len() > contracts::http::MAX_CURSOR_LENGTH {
            return Err(ControlError::ContractInvalid);
        }
        Ok(value)
    }
}

// Same HMAC-SHA256 construction as Agent/Resource history cursors; signatures are
// compared in constant time, and key IDs use the shared domain-separated helper.
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

pub(super) struct ListPage<'a> {
    pub after: Option<Uuid>,
    pub limit: u16,
    scope: Value,
    keys: &'a SigningKeys,
}

impl<'a> ListPage<'a> {
    pub fn new(
        query: &ControlListQuery,
        scope: &impl Serialize,
        keys: Option<&'a SigningKeys>,
    ) -> Result<Self, ControlError> {
        query.validate().map_err(|_| ControlError::CursorInvalid)?;
        let keys = keys.ok_or(ControlError::ConfigurationInvalid)?;
        let scope = serde_json::to_value(scope).map_err(|_| ControlError::ContractInvalid)?;
        let after = query
            .cursor
            .as_deref()
            .map(|value| keys.decode(value, &scope))
            .transpose()?;
        Ok(Self {
            after,
            limit: query.limit.unwrap_or(50),
            scope,
            keys,
        })
    }

    pub fn finish<T>(
        self,
        rows: &[PgRow],
        decode: impl Fn(&PgRow) -> Result<T, ControlError>,
    ) -> Result<CursorPage<T>, ControlError> {
        let next_cursor = if rows.len() > usize::from(self.limit) {
            let row = &rows[usize::from(self.limit) - 1];
            Some(
                self.keys
                    .encode(row.try_get("cursor_id").map_err(db)?, self.scope)?,
            )
        } else {
            None
        };
        let items = rows
            .iter()
            .take(usize::from(self.limit))
            .map(decode)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(CursorPage { items, next_cursor })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hmac_matches_rfc4231_and_missing_or_short_keys_fail_closed() {
        assert_eq!(
            hmac_sha256(&[0x0b; 20], b"Hi There"),
            [
                0xb0, 0x34, 0x4c, 0x61, 0xd8, 0xdb, 0x38, 0x53, 0x5c, 0xa8, 0xaf, 0xce, 0xaf, 0x0b,
                0xf1, 0x2b, 0x88, 0x1d, 0xc2, 0x00, 0xc9, 0x83, 0x3d, 0xa7, 0x26, 0xe9, 0x37, 0x6c,
                0x2e, 0x32, 0xcf, 0xf7,
            ]
        );
        assert!(SigningKeys::new(vec![1; 31], None).is_err());
        assert!(SigningKeys::new(vec![1; 32], Some(vec![2; 31])).is_err());
        assert!(matches!(
            ListPage::new(&ControlListQuery::default(), &"scope", None),
            Err(ControlError::ConfigurationInvalid)
        ));
    }

    #[test]
    fn rotation_accepts_only_current_and_explicit_previous_key() -> Result<(), ControlError> {
        let scope = serde_json::json!(["projects", Uuid::now_v7(), false]);
        let old = SigningKeys::new(vec![1; 32], None)?;
        let value = old.encode(Uuid::now_v7(), scope.clone())?;
        let rotated = SigningKeys::new(vec![2; 32], Some(vec![1; 32]))?;
        assert!(rotated.decode(&value, &scope).is_ok());
        assert!(
            SigningKeys::new(vec![2; 32], None)?
                .decode(&value, &scope)
                .is_err()
        );
        Ok(())
    }
}
