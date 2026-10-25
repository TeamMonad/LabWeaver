//! Resource lists share the existing canonical, HMAC-authenticated cursor protocol.
use super::{PgResourceStore, ResourceStoreError, decode_resource_cursor, encode_resource_cursor};
use contracts::http::{CursorPage, ResourceListQuery};
use contracts::resource::credits::{CreditHold, CreditLedgerEntry};
use contracts::resource::{
    GpuCatalogEntry, ResourceCharge, ResourceLease, ResourceRate, ResourceRequest,
    ResourceUsageRecord,
};
use contracts::{ActorId, CourseId, ProjectId};
use persistence_sqlx::cursor_key_id;
use serde::{Deserialize, Serialize};
use sqlx::{Row, postgres::PgRow};

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ResourceListCursor {
    version: u8,
    key_id: String,
    scope: String,
    ordinal: i64,
}

impl PgResourceStore {
    // Every caller supplies static SQL; scope and authorization values are bound before LIMIT.
    #[allow(
        clippy::too_many_arguments,
        reason = "explicit SQL authorization binds and decoder stay at one read boundary"
    )]
    async fn list_page<T>(
        &self,
        collection: &str,
        project: Option<ProjectId>,
        actor: Option<ActorId>,
        query: &ResourceListQuery,
        sql: &'static str,
        decode: impl Fn(&PgRow) -> Result<T, ResourceStoreError>,
    ) -> Result<CursorPage<T>, ResourceStoreError> {
        query
            .validate()
            .map_err(|_| ResourceStoreError::InvalidCursor)?;
        if collection != "project-leases" && query.course_id.is_some() {
            return Err(ResourceStoreError::InvalidCursor);
        }
        let scope = serde_json::to_string(&(collection, project, actor, query.course_id))?;
        let cursor = query
            .cursor
            .as_deref()
            .map(|value| {
                let cursor: ResourceListCursor = decode_resource_cursor(
                    value,
                    self.cursor_signing_key_current.as_slice(),
                    self.cursor_signing_key_previous
                        .as_deref()
                        .map(Vec::as_slice),
                    |cursor: &ResourceListCursor| &cursor.key_id,
                )?;
                if cursor.version != 1 || cursor.scope != scope || cursor.ordinal <= 0 {
                    return Err(ResourceStoreError::InvalidCursor);
                }
                Ok(cursor)
            })
            .transpose()?;
        let limit = usize::from(query.limit.unwrap_or(50));
        let rows = sqlx::query(sql)
            .bind(project.map(ProjectId::as_uuid))
            .bind(actor.map(ActorId::as_uuid))
            .bind(query.course_id.map(CourseId::as_uuid))
            .bind(cursor.as_ref().map(|cursor| cursor.ordinal))
            .bind(i64::try_from(limit + 1).map_err(|_| ResourceStoreError::InvalidCursor)?)
            .fetch_all(&self.pool)
            .await?;
        let has_more = rows.len() > limit;
        let visible = &rows[..rows.len().min(limit)];
        let next_cursor = if has_more {
            let last = visible.last().ok_or(ResourceStoreError::InvalidCursor)?;
            Some(encode_resource_cursor(
                &ResourceListCursor {
                    version: 1,
                    key_id: cursor_key_id("resource", self.cursor_signing_key_current.as_slice()),
                    scope,
                    ordinal: last.try_get("ordinal")?,
                },
                self.cursor_signing_key_current.as_slice(),
            )?)
        } else {
            None
        };
        let items = visible.iter().map(decode).collect::<Result<_, _>>()?;
        Ok(CursorPage { items, next_cursor })
    }

    pub async fn list_all_requests(
        &self,
        query: &ResourceListQuery,
    ) -> Result<CursorPage<ResourceRequest>, ResourceStoreError> {
        self.list_page("requests", None, None, query,
            "SELECT contract,ordinal FROM resource.resource_requests WHERE $1::uuid IS NULL AND $2::uuid IS NULL AND $3::uuid IS NULL AND ($4::bigint IS NULL OR ordinal<$4) ORDER BY ordinal DESC LIMIT $5",
            |row| super::decode_request(row.try_get("contract")?)).await
    }

    pub async fn list_all_leases(
        &self,
        query: &ResourceListQuery,
    ) -> Result<CursorPage<ResourceLease>, ResourceStoreError> {
        self.list_page("leases", None, None, query,
            "SELECT contract,ordinal FROM resource.resource_leases WHERE $1::uuid IS NULL AND $2::uuid IS NULL AND $3::uuid IS NULL AND ($4::bigint IS NULL OR ordinal<$4) ORDER BY ordinal DESC LIMIT $5",
            |row| super::decode_lease(row.try_get("contract")?)).await
    }

    pub async fn list_project_leases(
        &self,
        project: ProjectId,
        actor: Option<ActorId>,
        query: &ResourceListQuery,
    ) -> Result<CursorPage<ResourceLease>, ResourceStoreError> {
        self.list_page("project-leases", Some(project), actor, query,
            "SELECT l.contract,l.ordinal FROM resource.resource_leases l JOIN resource.resource_requests r ON r.request_id=l.request_id WHERE r.project_id=$1 AND ($2::uuid IS NULL OR r.requester_id=$2) AND ($3::uuid IS NULL OR r.course_id=$3) AND ($4::bigint IS NULL OR l.ordinal<$4) ORDER BY l.ordinal DESC LIMIT $5",
            |row| super::decode_lease(row.try_get("contract")?)).await
    }

    pub async fn list_rates(
        &self,
        query: &ResourceListQuery,
    ) -> Result<CursorPage<ResourceRate>, ResourceStoreError> {
        self.list_page("rates", None, None, query,
            "SELECT contract,ordinal FROM resource.resource_rates WHERE $1::uuid IS NULL AND $2::uuid IS NULL AND $3::uuid IS NULL AND ($4::bigint IS NULL OR ordinal<$4) ORDER BY ordinal DESC LIMIT $5",
            |row| super::decode_rate(row.try_get("contract")?)).await
    }

    pub async fn list_gpu_catalog(
        &self,
        query: &ResourceListQuery,
    ) -> Result<CursorPage<GpuCatalogEntry>, ResourceStoreError> {
        self.list_page("gpu-catalog", None, None, query,
            "SELECT contract,ordinal FROM resource.gpu_catalog_entries WHERE $1::uuid IS NULL AND $2::uuid IS NULL AND $3::uuid IS NULL AND ($4::bigint IS NULL OR ordinal<$4) ORDER BY ordinal DESC LIMIT $5",
            |row| super::decode_gpu_catalog(row.try_get("contract")?)).await
    }

    pub async fn list_charges(
        &self,
        project: ProjectId,
        query: &ResourceListQuery,
    ) -> Result<CursorPage<ResourceCharge>, ResourceStoreError> {
        self.list_page("charges", Some(project), None, query,
            "SELECT contract,ordinal FROM resource.resource_charges WHERE project_id=$1 AND $2::uuid IS NULL AND $3::uuid IS NULL AND ($4::bigint IS NULL OR ordinal<$4) ORDER BY ordinal DESC LIMIT $5",
            |row| super::decode_charge(row.try_get("contract")?)).await
    }

    pub async fn list_usage(
        &self,
        project: ProjectId,
        query: &ResourceListQuery,
    ) -> Result<contracts::resource::ResourceUsagePage, ResourceStoreError> {
        let page: CursorPage<ResourceUsageRecord> = self.list_page("usage", Some(project), None, query,
            "SELECT contract,ordinal FROM resource.resource_usage_records WHERE project_id=$1 AND $2::uuid IS NULL AND $3::uuid IS NULL AND ($4::bigint IS NULL OR ordinal<$4) ORDER BY ordinal DESC LIMIT $5",
            |row| super::decode_usage(row.try_get("contract")?)).await?;
        Ok(contracts::resource::ResourceUsagePage {
            items: page.items,
            next_cursor: page.next_cursor,
        })
    }

    pub async fn list_credit_ledger(
        &self,
        project: ProjectId,
        query: &ResourceListQuery,
    ) -> Result<contracts::resource::credits::CreditLedgerPage, ResourceStoreError> {
        let page: CursorPage<CreditLedgerEntry> = self.list_page("credit-ledger", Some(project), None, query,
            "SELECT contract,ordinal FROM resource.credit_ledger_entries WHERE project_id=$1 AND $2::uuid IS NULL AND $3::uuid IS NULL AND ($4::bigint IS NULL OR ordinal<$4) ORDER BY ordinal DESC LIMIT $5",
            |row| Ok(serde_json::from_value(row.try_get("contract")?)?)).await?;
        Ok(contracts::resource::credits::CreditLedgerPage {
            items: page.items,
            next_cursor: page.next_cursor,
        })
    }

    pub async fn list_credit_holds(
        &self,
        project: ProjectId,
        query: &ResourceListQuery,
    ) -> Result<contracts::resource::credits::CreditHoldPage, ResourceStoreError> {
        let page: CursorPage<CreditHold> = self.list_page("credit-holds", Some(project), None, query,
            "SELECT *,amount::text AS amount_scaled,remaining::text AS remaining_scaled FROM resource.credit_holds WHERE project_id=$1 AND $2::uuid IS NULL AND $3::uuid IS NULL AND ($4::bigint IS NULL OR ordinal<$4) ORDER BY ordinal DESC LIMIT $5",
            |row| crate::credits::decode_hold(row).map_err(Into::into)).await?;
        Ok(contracts::resource::credits::CreditHoldPage {
            items: page.items,
            next_cursor: page.next_cursor,
        })
    }
}
