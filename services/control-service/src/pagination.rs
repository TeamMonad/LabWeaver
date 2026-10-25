//! Self-contained UUID keysets, following Access/Evaluation's canonical base64 JSON cursors.
use super::{ControlError, canonical_hash, db};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use contracts::http::CursorPage;
use contracts::membership_governance::ControlListQuery;
use persistence_sqlx::Sha256Digest;
use serde::{Deserialize, Serialize};
use sqlx::{Row, postgres::PgRow};
use uuid::Uuid;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    id: Uuid,
    scope: Sha256Digest,
}

pub(super) struct ListPage {
    pub after: Option<Uuid>,
    pub limit: u16,
    scope: Sha256Digest,
}

impl ListPage {
    pub fn new(query: &ControlListQuery, scope: &impl Serialize) -> Result<Self, ControlError> {
        query
            .validate()
            .map_err(|_| ControlError::ContractInvalid)?;
        let scope = canonical_hash(scope)?;
        let after = query
            .cursor
            .as_deref()
            .map(|value| {
                let bytes = URL_SAFE_NO_PAD
                    .decode(value)
                    .map_err(|_| ControlError::ContractInvalid)?;
                let cursor: Cursor =
                    serde_json::from_slice(&bytes).map_err(|_| ControlError::ContractInvalid)?;
                if cursor.scope != scope || URL_SAFE_NO_PAD.encode(&bytes) != value {
                    return Err(ControlError::ContractInvalid);
                }
                Ok(cursor.id)
            })
            .transpose()?;
        Ok(Self {
            after,
            limit: query.limit.unwrap_or(50),
            scope,
        })
    }

    pub fn finish<T>(
        &self,
        rows: &[PgRow],
        decode: impl Fn(&PgRow) -> Result<T, ControlError>,
    ) -> Result<CursorPage<T>, ControlError> {
        let next_cursor =
            if rows.len() > usize::from(self.limit) {
                let row = &rows[usize::from(self.limit) - 1];
                let cursor = Cursor {
                    id: row.try_get("cursor_id").map_err(db)?,
                    scope: self.scope,
                };
                Some(URL_SAFE_NO_PAD.encode(
                    serde_json::to_vec(&cursor).map_err(|_| ControlError::ContractInvalid)?,
                ))
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
