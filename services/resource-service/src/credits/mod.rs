//! Transactional Credits authority. All rows belong to Resource.
use contracts::resource::credits::{
    CreditAccount, CreditAdminOperation, CreditHold, CreditHoldRequest,
    CreditHoldResolutionRequest, CreditHoldState, CreditLedgerEntry, CreditLedgerPage,
    CreditRateSnapshot,
};
use contracts::resource::{FixedDecimal, ResourceCharge};
use contracts::{ActorId, EventId, PlatformRole, ProjectId, Revision, UtcTimestamp};
use persistence_sqlx::{
    Domain, IdempotencyDecision, IdempotencyStore, PersistenceError, Sha256Digest,
};
use serde::Serialize;
use sqlx::{PgPool, Postgres, Row, Transaction};
use uuid::Uuid;

#[derive(Clone)]
pub struct PgCreditStore {
    pool: PgPool,
}
type Tx<'a> = Transaction<'a, Postgres>;

#[derive(Debug, thiserror::Error)]
pub enum CreditError {
    #[error("LW_CREDITS_INVALID")]
    Invalid,
    #[error("LW_CREDITS_SCOPE_DENIED")]
    Denied,
    #[error("LW_CREDITS_INSUFFICIENT")]
    Insufficient,
    #[error("LW_CREDITS_CONFLICT")]
    Conflict,
    #[error("LW_CREDITS_NOT_FOUND")]
    NotFound,
    #[error("LW_CREDITS_STORAGE")]
    Storage(#[from] sqlx::Error),
    #[error("LW_CREDITS_PERSISTENCE")]
    Persistence(#[from] PersistenceError),
    #[error("LW_CREDITS_WIRE")]
    Wire(#[from] serde_json::Error),
    #[error("LW_CREDITS_PRECISION")]
    Precision(#[from] contracts::resource::ResourceError),
}

impl PgCreditStore {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// A missing account reads as zero. Reads do not create spendable Credits.
    pub async fn account(&self, project_id: ProjectId) -> Result<CreditAccount, CreditError> {
        let row = sqlx::query("SELECT COALESCE(a.balance,0)::text AS balance, COALESCE(a.reserved,0)::text AS reserved, COALESCE(a.spent,0)::text AS spent, COALESCE(a.platform_absorbed,0)::text AS absorbed, COALESCE(a.revision,1) AS revision, COALESCE((SELECT sum(remaining) FROM resource.credit_holds WHERE project_id=$1 AND measurement_unknown),0)::text AS unknown, (SELECT count(*) FROM resource.credit_holds WHERE project_id=$1 AND measurement_unknown) AS unknown_count FROM (SELECT $1::uuid AS project_id) p LEFT JOIN resource.credit_accounts a USING(project_id)").bind(project_id.as_uuid()).fetch_one(&self.pool).await?;
        let balance = number(&row, "balance")?;
        let reserved = number(&row, "reserved")?;
        let unknown = number(&row, "unknown")?;
        Ok(CreditAccount {
            project_id,
            currency: "CREDITS".into(),
            balance: decimal(balance)?,
            available: decimal(balance - reserved)?,
            reserved: decimal(reserved)?,
            spent: decimal(number(&row, "spent")?)?,
            pending: decimal(reserved - unknown)?,
            unknown: decimal(unknown)?,
            unknown_count: u64::try_from(row.try_get::<i64, _>("unknown_count")?)
                .map_err(|_| CreditError::Invalid)?,
            platform_absorbed: decimal(number(&row, "absorbed")?)?,
            revision: Revision::new(
                u64::try_from(row.try_get::<i64, _>("revision")?)
                    .map_err(|_| CreditError::Invalid)?,
            )
            .map_err(|_| CreditError::Invalid)?,
        })
    }

    pub async fn ledger(
        &self,
        project: ProjectId,
        page: u32,
        page_size: u16,
        offset: u32,
    ) -> Result<CreditLedgerPage, CreditError> {
        let rows = sqlx::query("SELECT contract FROM resource.credit_ledger_entries WHERE project_id=$1 ORDER BY created_at DESC,entry_id DESC LIMIT $2 OFFSET $3").bind(project.as_uuid()).bind(i64::from(page_size)+1).bind(i64::from(offset)).fetch_all(&self.pool).await?;
        let has_more = rows.len() > usize::from(page_size);
        let items = rows
            .into_iter()
            .take(usize::from(page_size))
            .map(|r| Ok(serde_json::from_value(r.try_get("contract")?)?))
            .collect::<Result<Vec<_>, CreditError>>()?;
        Ok(CreditLedgerPage {
            items,
            page,
            page_size,
            has_more,
        })
    }

    pub async fn holds(
        &self,
        project: ProjectId,
        page: u32,
        page_size: u16,
        offset: u32,
    ) -> Result<contracts::resource::credits::CreditHoldPage, CreditError> {
        let rows=sqlx::query("SELECT *,amount::text AS amount_scaled,remaining::text AS remaining_scaled FROM resource.credit_holds WHERE project_id=$1 ORDER BY created_at DESC,hold_id DESC LIMIT $2 OFFSET $3").bind(project.as_uuid()).bind(i64::from(page_size)+1).bind(i64::from(offset)).fetch_all(&self.pool).await?;
        let has_more = rows.len() > usize::from(page_size);
        let items = rows
            .iter()
            .take(usize::from(page_size))
            .map(decode_hold)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(contracts::resource::credits::CreditHoldPage {
            items,
            page,
            page_size,
            has_more,
        })
    }

    /// Reclaims terminal pre-execution holds, including #200 deadline transitions.
    /// Expiring resources and unreleased capacity remain reserved.
    pub async fn reconcile_terminal_once(&self) -> Result<bool, CreditError> {
        let mut tx = self.pool.begin().await?;
        let request:Option<Uuid>=sqlx::query_scalar("SELECT r.request_id FROM resource.resource_requests r WHERE r.state IN ('cancelled','rejected','expired') AND EXISTS(SELECT 1 FROM resource.credit_holds h WHERE h.request_id=r.request_id AND h.state IN ('pending','unknown')) AND NOT EXISTS(SELECT 1 FROM resource.capacity_claims c WHERE c.request_id=r.request_id AND c.state<>'released') ORDER BY r.request_id FOR UPDATE OF r SKIP LOCKED LIMIT 1").fetch_optional(&mut *tx).await?;
        let Some(request) = request else {
            tx.commit().await?;
            return Ok(false);
        };
        let request_id = request
            .to_string()
            .parse()
            .map_err(|_| CreditError::Invalid)?;
        release_request_holds(&mut tx, request_id).await?;
        tx.commit().await?;
        Ok(true)
    }

    /// Admin role is checked again at the domain boundary. Transfers lock sorted projects.
    pub async fn admin(
        &self,
        key: &str,
        actor: ActorId,
        roles: &[PlatformRole],
        input: &CreditAdminOperation,
    ) -> Result<Vec<CreditLedgerEntry>, CreditError> {
        if !roles.contains(&PlatformRole::PlatformAdmin) {
            return Err(CreditError::Denied);
        }
        let mut tx = self.pool.begin().await?;
        if let Some(replay) = reserve(&mut tx, "credits_admin", key, &(actor, input)).await? {
            return Ok(serde_json::from_value(replay)?);
        }
        let op = Uuid::now_v7();
        let now = now(&mut tx).await?;
        let (mut entries, reason) = admin_entries(&mut tx, input, op, now).await?;
        validate_reason(reason)?;
        let mut projects = entries
            .iter()
            .filter_map(|e| e.project_id)
            .collect::<Vec<_>>();
        projects.sort_unstable();
        projects.dedup();
        for project in projects {
            lock_account(&mut tx, project).await?;
        }
        for e in &mut entries {
            e.reason = reason.clone();
            e.actor_id = Some(actor);
            apply(&mut tx, e).await?;
        }
        complete(&mut tx, "credits_admin", key, &entries).await?;
        tx.commit().await?;
        tracing::info!(event="resource.credits.admin_committed",operation_id=%op,actor_id=%actor,entry_count=entries.len());
        Ok(entries)
    }

    /// The caller must be an authenticated configured workload owner, never the browser gateway.
    pub async fn hold(
        &self,
        key: &str,
        owner: &str,
        input: &CreditHoldRequest,
    ) -> Result<CreditHold, CreditError> {
        positive(&input.amount)?;
        let mut tx = self.pool.begin().await?;
        let hold = hold_in_transaction(&mut tx, key, owner, input).await?;
        tx.commit().await?;
        Ok(hold)
    }

    /// Settlement consumes at most the remaining preauthorization. Late cost is platform cost.
    pub async fn resolve(
        &self,
        key: &str,
        owner: &str,
        hold_id: Uuid,
        input: &CreditHoldResolutionRequest,
    ) -> Result<CreditHold, CreditError> {
        let mut tx = self.pool.begin().await?;
        if let Some(replay) =
            reserve(&mut tx, "credits_resolve", key, &(owner, hold_id, input)).await?
        {
            return Ok(serde_json::from_value(replay)?);
        }
        let event_id = match input {
            CreditHoldResolutionRequest::Settle { event_id, .. } => Some(*event_id),
            CreditHoldResolutionRequest::Unknown { event_id, .. } => *event_id,
            CreditHoldResolutionRequest::Release { .. } => None,
        };
        let event_key = event_id.map(|event| event.to_string());
        if let Some(event_key) = &event_key
            && let Some(replay) = reserve(
                &mut tx,
                "credits_event",
                event_key,
                &(owner, hold_id, input),
            )
            .await?
        {
            let hold: CreditHold = serde_json::from_value(replay)?;
            complete(&mut tx, "credits_resolve", key, &hold).await?;
            tx.commit().await?;
            return Ok(hold);
        }
        let hold = resolve_in_transaction(&mut tx, owner, hold_id, input, None, event_id).await?;
        if let Some(event_key) = event_key {
            complete(&mut tx, "credits_event", &event_key, &hold).await?;
        }
        complete(&mut tx, "credits_resolve", key, &hold).await?;
        tx.commit().await?;
        Ok(hold)
    }
}

fn validate_reason(reason: &str) -> Result<(), CreditError> {
    if reason.trim().is_empty() || reason.len() > 500 {
        Err(CreditError::Invalid)
    } else {
        Ok(())
    }
}
fn positive(amount: &FixedDecimal) -> Result<(), CreditError> {
    if amount.scaled() <= 0 || amount.scaled().unsigned_abs() >= 10_u128.pow(38) {
        Err(CreditError::Invalid)
    } else {
        Ok(())
    }
}
fn decimal(value: i128) -> Result<FixedDecimal, CreditError> {
    Ok(FixedDecimal::from_scaled(value)?)
}
fn number(row: &sqlx::postgres::PgRow, name: &str) -> Result<i128, CreditError> {
    row.try_get::<String, _>(name)?
        .parse()
        .map_err(|_| CreditError::Invalid)
}
fn state_name(state: CreditHoldState) -> &'static str {
    match state {
        CreditHoldState::Pending => "pending",
        CreditHoldState::Unknown => "unknown",
        CreditHoldState::Settled => "settled",
        CreditHoldState::Released => "released",
    }
}
pub(crate) fn decode_hold(row: &sqlx::postgres::PgRow) -> Result<CreditHold, CreditError> {
    let state: String = row.try_get("state")?;
    Ok(CreditHold {
        id: row.try_get("hold_id")?,
        project_id: row
            .try_get::<Uuid, _>("project_id")?
            .to_string()
            .parse()
            .map_err(|_| CreditError::Invalid)?,
        request_id: row
            .try_get::<Option<Uuid>, _>("request_id")?
            .map(|id| id.to_string().parse())
            .transpose()
            .map_err(|_| CreditError::Invalid)?,
        build_request_id: row
            .try_get::<Option<Uuid>, _>("build_request_id")?
            .map(|id| id.to_string().parse())
            .transpose()
            .map_err(|_| CreditError::Invalid)?,
        build_attempt: row
            .try_get::<Option<i32>, _>("build_attempt")?
            .map(u32::try_from)
            .transpose()
            .map_err(|_| CreditError::Invalid)?,
        business_key: row.try_get("business_key")?,
        owner_client_id: row.try_get("owner_client_id")?,
        amount: decimal(number(row, "amount_scaled")?)?,
        remaining: decimal(number(row, "remaining_scaled")?)?,
        state: match state.as_str() {
            "pending" => CreditHoldState::Pending,
            "unknown" => CreditHoldState::Unknown,
            "settled" => CreditHoldState::Settled,
            "released" => CreditHoldState::Released,
            _ => return Err(CreditError::Invalid),
        },
        reason: row.try_get("reason")?,
        measurement_unknown: row.try_get("measurement_unknown")?,
        unknown_since: optional_time(row, "unknown_since")?,
        reconciliation_due_at: optional_time(row, "reconciliation_due_at")?,
        expired_unknown_at: optional_time(row, "expired_unknown_at")?,
        created_at: UtcTimestamp::from_utc(row.try_get("created_at")?)
            .map_err(|_| CreditError::Invalid)?,
    })
}
async fn now(tx: &mut Tx<'_>) -> Result<UtcTimestamp, CreditError> {
    UtcTimestamp::from_utc(
        sqlx::query_scalar("SELECT date_trunc('milliseconds',clock_timestamp())")
            .fetch_one(&mut **tx)
            .await?,
    )
    .map_err(|_| CreditError::Invalid)
}
async fn lock_account(tx: &mut Tx<'_>, project: ProjectId) -> Result<(), CreditError> {
    sqlx::query(
        "INSERT INTO resource.credit_accounts(project_id) VALUES($1) ON CONFLICT DO NOTHING",
    )
    .bind(project.as_uuid())
    .execute(&mut **tx)
    .await?;
    sqlx::query("SELECT project_id FROM resource.credit_accounts WHERE project_id=$1 FOR UPDATE")
        .bind(project.as_uuid())
        .fetch_one(&mut **tx)
        .await?;
    Ok(())
}
#[allow(clippy::too_many_arguments)]
fn entry(
    operation_id: Uuid,
    project_id: Option<ProjectId>,
    balance: i128,
    reserved: i128,
    spent: i128,
    absorbed: i128,
    kind: &str,
    created_at: UtcTimestamp,
) -> Result<CreditLedgerEntry, CreditError> {
    Ok(CreditLedgerEntry {
        id: Uuid::now_v7(),
        operation_id,
        project_id,
        hold_id: None,
        reversal_of: None,
        kind: kind.into(),
        source_event_id: None,
        charge_id: None,
        usage_record_id: None,
        rate_snapshots: Vec::new(),
        balance_delta: decimal(balance)?,
        reserved_delta: decimal(reserved)?,
        spent_delta: decimal(spent)?,
        platform_absorbed: decimal(absorbed)?,
        reason: String::new(),
        actor_id: None,
        owner_client_id: None,
        created_at,
    })
}
async fn apply(tx: &mut Tx<'_>, e: &CreditLedgerEntry) -> Result<(), CreditError> {
    if let Some(project) = e.project_id {
        let result=sqlx::query("UPDATE resource.credit_accounts SET balance=balance+$2::text::numeric,reserved=reserved+$3::text::numeric,spent=spent+$4::text::numeric,platform_absorbed=platform_absorbed+$5::text::numeric,revision=revision+1 WHERE project_id=$1 AND balance+$2::text::numeric>=reserved+$3::text::numeric AND reserved+$3::text::numeric>=0 AND spent+$4::text::numeric>=0").bind(project.as_uuid()).bind(e.balance_delta.scaled().to_string()).bind(e.reserved_delta.scaled().to_string()).bind(e.spent_delta.scaled().to_string()).bind(e.platform_absorbed.scaled().to_string()).execute(&mut **tx).await?;
        if result.rows_affected() != 1 {
            return Err(CreditError::Insufficient);
        }
    }
    sqlx::query("INSERT INTO resource.credit_ledger_entries(entry_id,operation_id,project_id,hold_id,reversal_of,source_event_id,charge_id,usage_record_id,balance_delta,reserved_delta,spent_delta,platform_absorbed,contract,created_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9::text::numeric,$10::text::numeric,$11::text::numeric,$12::text::numeric,$13,$14)")
        .bind(e.id)
        .bind(e.operation_id)
        .bind(e.project_id.map(ProjectId::as_uuid))
        .bind(e.hold_id)
        .bind(e.reversal_of)
        .bind(e.source_event_id.map(contracts::EventId::as_uuid))
        .bind(e.charge_id.map(contracts::ChargeId::as_uuid))
        .bind(e.usage_record_id.map(contracts::UsageRecordId::as_uuid))
        .bind(e.balance_delta.scaled().to_string())
        .bind(e.reserved_delta.scaled().to_string())
        .bind(e.spent_delta.scaled().to_string())
        .bind(e.platform_absorbed.scaled().to_string())
        .bind(serde_json::to_value(e)?)
        .bind(e.created_at.get())
        .execute(&mut **tx)
        .await?;
    Ok(())
}
pub(crate) async fn reserve<T: Serialize>(
    tx: &mut Tx<'_>,
    operation: &str,
    key: &str,
    input: &T,
) -> Result<Option<serde_json::Value>, CreditError> {
    contracts::http::IdempotencyKey::parse(key).map_err(|_| CreditError::Invalid)?;
    let hash = Sha256Digest::of_canonical(input).map_err(|_| CreditError::Invalid)?;
    match IdempotencyStore::reserve(tx, Domain::Resource, operation, key, hash).await? {
        IdempotencyDecision::Reserved => Ok(None),
        IdempotencyDecision::Replay(value) => Ok(Some(value)),
        _ => Err(CreditError::Conflict),
    }
}
pub(crate) async fn complete<T: Serialize>(
    tx: &mut Tx<'_>,
    operation: &str,
    key: &str,
    value: &T,
) -> Result<(), CreditError> {
    IdempotencyStore::complete(
        tx,
        Domain::Resource,
        operation,
        key,
        &serde_json::to_value(value)?,
    )
    .await?;
    Ok(())
}

async fn admin_entries<'a>(
    tx: &mut Tx<'_>,
    input: &'a CreditAdminOperation,
    op: Uuid,
    now: UtcTimestamp,
) -> Result<(Vec<CreditLedgerEntry>, &'a String), CreditError> {
    let result = match input {
        CreditAdminOperation::Grant {
            project_id,
            amount,
            reason,
        } => {
            positive(amount)?;
            (
                vec![
                    entry(
                        op,
                        Some(*project_id),
                        amount.scaled(),
                        0,
                        0,
                        0,
                        "grant",
                        now,
                    )?,
                    entry(op, None, -amount.scaled(), 0, 0, 0, "grant", now)?,
                ],
                reason,
            )
        }
        CreditAdminOperation::Adjust {
            project_id,
            amount,
            reason,
        } => {
            if amount.scaled() == 0 || amount.scaled().unsigned_abs() >= 10_u128.pow(38) {
                return Err(CreditError::Invalid);
            }
            (
                vec![
                    entry(
                        op,
                        Some(*project_id),
                        amount.scaled(),
                        0,
                        0,
                        0,
                        "adjust",
                        now,
                    )?,
                    entry(op, None, -amount.scaled(), 0, 0, 0, "adjust", now)?,
                ],
                reason,
            )
        }
        CreditAdminOperation::Transfer {
            project_id,
            destination_project_id,
            amount,
            reason,
        } => {
            positive(amount)?;
            if project_id == destination_project_id {
                return Err(CreditError::Invalid);
            }
            (
                vec![
                    entry(
                        op,
                        Some(*project_id),
                        -amount.scaled(),
                        0,
                        0,
                        0,
                        "transfer",
                        now,
                    )?,
                    entry(
                        op,
                        Some(*destination_project_id),
                        amount.scaled(),
                        0,
                        0,
                        0,
                        "transfer",
                        now,
                    )?,
                ],
                reason,
            )
        }
        CreditAdminOperation::Reverse { entry_id, reason } => {
            let reversed = reverse_entries(tx, *entry_id, op, now).await?;
            (reversed, reason)
        }
    };
    Ok(result)
}

async fn reverse_entries(
    tx: &mut Tx<'_>,
    entry_id: Uuid,
    op: Uuid,
    now: UtcTimestamp,
) -> Result<Vec<CreditLedgerEntry>, CreditError> {
    let operation: Uuid = sqlx::query_scalar(
        "SELECT operation_id FROM resource.credit_ledger_entries WHERE entry_id=$1",
    )
    .bind(entry_id)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(CreditError::NotFound)?;
    let originals=sqlx::query("SELECT contract FROM resource.credit_ledger_entries WHERE operation_id=$1 ORDER BY entry_id FOR UPDATE").bind(operation).fetch_all(&mut **tx).await?;
    let mut reversed = Vec::new();
    for row in originals {
        let original: CreditLedgerEntry = serde_json::from_value(row.try_get("contract")?)?;
        if original.reversal_of.is_some()
            || matches!(original.kind.as_str(), "hold" | "release" | "unknown")
        {
            return Err(CreditError::Conflict);
        }
        let used: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM resource.credit_ledger_entries WHERE reversal_of=$1)",
        )
        .bind(original.id)
        .fetch_one(&mut **tx)
        .await?;
        if used {
            return Err(CreditError::Conflict);
        }
        let mut e = entry(
            op,
            original.project_id,
            -original.balance_delta.scaled(),
            0,
            -original.spent_delta.scaled(),
            0,
            "reverse",
            now,
        )?;
        e.reversal_of = Some(original.id);
        e.hold_id = original.hold_id;
        reversed.push(e);
    }
    Ok(reversed)
}

pub(crate) async fn resolve_in_transaction(
    tx: &mut Tx<'_>,
    owner: &str,
    hold_id: Uuid,
    input: &CreditHoldResolutionRequest,
    charge: Option<&ResourceCharge>,
    source_event_id: Option<EventId>,
) -> Result<CreditHold, CreditError> {
    // Account-before-hold is the common lock order with hold creation.
    let project: Uuid =
        sqlx::query_scalar("SELECT project_id FROM resource.credit_holds WHERE hold_id=$1")
            .bind(hold_id)
            .fetch_optional(&mut **tx)
            .await?
            .ok_or(CreditError::NotFound)?;
    let project_id: ProjectId = project
        .to_string()
        .parse()
        .map_err(|_| CreditError::Invalid)?;
    lock_account(tx, project_id).await?;
    let row=sqlx::query("SELECT *,amount::text AS amount_scaled,remaining::text AS remaining_scaled FROM resource.credit_holds WHERE hold_id=$1 FOR UPDATE").bind(hold_id).fetch_one(&mut **tx).await?;
    let mut hold = decode_hold(&row)?;
    if hold.owner_client_id != owner {
        return Err(CreditError::Denied);
    }
    if matches!(input, CreditHoldResolutionRequest::Settle { .. }) {
        let current = now(tx).await?;
        expire_hold(tx, &mut hold, current).await?;
    }
    if matches!(input, CreditHoldResolutionRequest::Unknown { .. }) && hold.unknown_since.is_none()
    {
        let first = now(tx).await?;
        hold.unknown_since = Some(first);
        hold.reconciliation_due_at = Some(
            UtcTimestamp::from_utc(
                first.get()
                    + time::Duration::seconds(i64::from(
                        contracts::resource::costing::UNKNOWN_USAGE_RECONCILIATION_SECONDS,
                    )),
            )
            .map_err(|_| CreditError::Invalid)?,
        );
    }
    if charge.is_none()
        && hold.business_key == "metered-usage"
        && matches!(input, CreditHoldResolutionRequest::Settle { .. })
    {
        return Err(CreditError::Invalid);
    }
    if let Some(event_id) = source_event_id {
        let duplicate: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM resource.credit_ledger_entries WHERE hold_id=$1 AND source_event_id=$2 AND project_id IS NOT NULL)",
        )
        .bind(hold.id)
        .bind(event_id.as_uuid())
        .fetch_one(&mut **tx)
        .await?;
        if duplicate {
            return Ok(hold);
        }
    }
    let (debit, released, absorbed, kind, reason) = hold_movement(&mut hold, input)?;
    hold.reason = Some(reason.clone());
    sqlx::query("UPDATE resource.credit_holds SET remaining=$2::text::numeric,state=$3,reason=$4,measurement_unknown=$5,unknown_since=$6,reconciliation_due_at=$7,expired_unknown_at=$8 WHERE hold_id=$1").bind(hold.id).bind(hold.remaining.scaled().to_string()).bind(state_name(hold.state)).bind(&reason).bind(hold.measurement_unknown).bind(hold.unknown_since.map(UtcTimestamp::get)).bind(hold.reconciliation_due_at.map(UtcTimestamp::get)).bind(hold.expired_unknown_at.map(UtcTimestamp::get)).execute(&mut **tx).await?;
    let op = Uuid::now_v7();
    let now = now(tx).await?;
    let mut e = entry(
        op,
        Some(hold.project_id),
        -debit,
        -released,
        debit,
        absorbed,
        kind,
        now,
    )?;
    e.hold_id = Some(hold.id);
    if let CreditHoldResolutionRequest::Settle { event_id, .. } = input {
        e.source_event_id = Some(*event_id);
    }
    e.source_event_id = source_event_id.or(e.source_event_id);
    if let Some(charge) = charge {
        e.charge_id = Some(charge.id);
        e.usage_record_id = Some(charge.usage_record_id);
        e.rate_snapshots = charge
            .lines
            .iter()
            .map(|line| CreditRateSnapshot {
                rate_id: line.rate_id,
                rate_revision: line.rate_revision,
            })
            .collect();
    }
    e.reason = reason.clone();
    e.owner_client_id = Some(hold.owner_client_id.clone());
    apply(tx, &e).await?;
    if debit > 0 {
        let mut counterpart = entry(op, None, debit, 0, 0, 0, kind, now)?;
        counterpart.hold_id = Some(hold.id);
        counterpart.source_event_id = e.source_event_id;
        counterpart.charge_id = e.charge_id;
        counterpart.usage_record_id = e.usage_record_id;
        counterpart.rate_snapshots = e.rate_snapshots.clone();
        counterpart.reason = reason;
        counterpart.owner_client_id = Some(hold.owner_client_id.clone());
        apply(tx, &counterpart).await?;
    }
    Ok(())
}

fn hold_movement(
    hold: &mut CreditHold,
    input: &CreditHoldResolutionRequest,
) -> Result<(i128, i128, i128, &'static str, String), CreditError> {
    let terminal = matches!(
        hold.state,
        CreditHoldState::Settled | CreditHoldState::Released
    );
    let remaining = hold.remaining.scaled();
    let mut debit = 0;
    let mut released = 0;
    let mut absorbed = 0;
    let (kind, reason) = match input {
        CreditHoldResolutionRequest::Settle {
            amount, finalize, ..
        } => {
            if amount.scaled() < 0 || amount.scaled().unsigned_abs() >= 10_u128.pow(38) {
                return Err(CreditError::Invalid);
            }
            if *finalize {
                hold.measurement_unknown = false;
            }
            debit = amount.scaled().min(remaining);
            absorbed = amount.scaled() - debit;
            released = if *finalize { remaining } else { debit };
            if !terminal {
                hold.state = if *finalize {
                    CreditHoldState::Settled
                } else {
                    CreditHoldState::Pending
                };
            }
            ("settle", "measured cost".to_owned())
        }
        CreditHoldResolutionRequest::Release { reason } => {
            validate_reason(reason)?;
            released = remaining;
            if !terminal {
                hold.state = CreditHoldState::Released;
            }
            ("release", reason.clone())
        }
        CreditHoldResolutionRequest::Unknown { reason, .. } => {
            validate_reason(reason)?;
            hold.measurement_unknown = true;
            if !terminal {
                hold.state = CreditHoldState::Unknown;
            }
            ("unknown", reason.clone())
        }
    };
    hold.remaining = decimal(remaining - released)?;
    Ok((debit, released, absorbed, kind, reason))
}

/// Joins existing fixed-rate usage settlement to its explicit metered-usage hold.
/// An unbound historical charge remains a calculation, never an implicit wallet debit.
pub(crate) async fn settle_metered_charge(
    tx: &mut Tx<'_>,
    usage: &contracts::resource::ResourceUsageRecord,
    charge: &contracts::resource::ResourceCharge,
) -> Result<(), CreditError> {
    let Some(request_id) = usage.target.request_id() else {
        return Ok(());
    };
    let row=sqlx::query("SELECT hold_id,owner_client_id FROM resource.credit_holds WHERE request_id=$1 AND business_key='metered-usage'").bind(request_id.as_uuid()).fetch_optional(&mut **tx).await?;
    let Some(row) = row else {
        return Ok(());
    };
    charge.validate().map_err(CreditError::Precision)?;
    if charge.total.currency != "CREDITS"
        || charge
            .lines
            .iter()
            .any(|line| line.unit_price.currency != "CREDITS")
    {
        return Err(CreditError::Invalid);
    }
    let hold_id: Uuid = row.try_get("hold_id")?;
    let owner: String = row.try_get("owner_client_id")?;
    let input = CreditHoldResolutionRequest::Settle {
        event_id: usage.source_event_id,
        amount: charge.total.amount.clone(),
        finalize: false,
    };
    let key = usage.source_event_id.to_string();
    if reserve(tx, "credits_event", &key, &(&owner, hold_id, &input))
        .await?
        .is_none()
    {
        let hold = resolve_in_transaction(
            tx,
            &owner,
            hold_id,
            &input,
            Some(charge),
            Some(usage.source_event_id),
        )
        .await?;
        complete(tx, "credits_event", &key, &hold).await?;
    }
    Ok(())
}

/// Physical cleanup releases unused pending funds. Unknown measurements stay visible.
pub(crate) async fn release_request_holds(
    tx: &mut Tx<'_>,
    request_id: contracts::ResourceRequestId,
) -> Result<(), CreditError> {
    let holds=sqlx::query("SELECT hold_id,owner_client_id FROM resource.credit_holds WHERE request_id=$1 AND state IN ('pending','unknown') ORDER BY hold_id").bind(request_id.as_uuid()).fetch_all(&mut **tx).await?;
    for row in holds {
        let id: Uuid = row.try_get("hold_id")?;
        let owner: String = row.try_get("owner_client_id")?;
        resolve_in_transaction(
            tx,
            &owner,
            id,
            &CreditHoldResolutionRequest::Release {
                reason: "resource physical cleanup confirmed".into(),
            },
            None,
            None,
        )
        .await?;
    }
    Ok(())
}

/// Records an immutable unknown meter observation without charging zero.
pub(crate) async fn observe_unknown_usage(
    tx: &mut Tx<'_>,
    usage: &contracts::resource::ResourceUsageRecord,
) -> Result<(), CreditError> {
    let contracts::resource::UsageMeasurement::Unknown { reason } = &usage.measurement else {
        return Ok(());
    };
    let Some(request_id) = usage.target.request_id() else {
        return Ok(());
    };
    let row=sqlx::query("SELECT hold_id,owner_client_id FROM resource.credit_holds WHERE request_id=$1 AND business_key='metered-usage'").bind(request_id.as_uuid()).fetch_optional(&mut **tx).await?;
    let Some(row) = row else {
        return Ok(());
    };
    let id: Uuid = row.try_get("hold_id")?;
    let owner: String = row.try_get("owner_client_id")?;
    resolve_in_transaction(
        tx,
        &owner,
        id,
        &CreditHoldResolutionRequest::Unknown {
            event_id: Some(usage.source_event_id),
            reason: reason.clone(),
        },
        None,
        Some(usage.source_event_id),
    )
    .await?;
    Ok(())
}

/// Shared atomic entry used by Resource-priced holds.
pub(crate) async fn hold_in_transaction(
    tx: &mut Tx<'_>,
    key: &str,
    owner: &str,
    input: &CreditHoldRequest,
) -> Result<CreditHold, CreditError> {
    if input.amount.scaled() < 0 || input.amount.scaled().unsigned_abs() >= 10_u128.pow(38) {
        return Err(CreditError::Invalid);
    }
    if owner.is_empty() || input.business_key.trim().is_empty() || input.business_key.len() > 128 {
        return Err(CreditError::Invalid);
    }
    if let Some(replay) = reserve(tx, "credits_hold", key, &(owner, input)).await? {
        return Ok(serde_json::from_value(replay)?);
    }
    let scope = sqlx::query(
        "SELECT project_id,state FROM resource.resource_requests WHERE request_id=$1 FOR UPDATE",
    )
    .bind(input.request_id.as_uuid())
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(CreditError::NotFound)?;
    let project: Uuid = scope.try_get("project_id")?;
    let request_state: String = scope.try_get("state")?;
    if !matches!(
        request_state.as_str(),
        "reviewing" | "allocating" | "active"
    ) {
        return Err(CreditError::Conflict);
    }
    if project != input.project_id.as_uuid() {
        return Err(CreditError::Denied);
    }
    stage_target_hold(
        tx,
        key,
        owner,
        input.project_id,
        Some(input.request_id),
        None,
        None,
        &input.business_key,
        &input.amount,
    )
    .await
}

/// Only the Resource build admission owner calls this after locking the real Agent command.
pub(crate) async fn build_hold_in_transaction(
    tx: &mut Tx<'_>,
    key: &str,
    owner: &str,
    project: ProjectId,
    build: contracts::BuildRequestId,
    attempt: u32,
    amount: &FixedDecimal,
) -> Result<CreditHold, CreditError> {
    stage_target_hold(
        tx,
        key,
        owner,
        project,
        None,
        Some(build),
        Some(attempt),
        "build-metered",
        amount,
    )
    .await
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
async fn stage_target_hold(
    tx: &mut Tx<'_>,
    key: &str,
    owner: &str,
    project: ProjectId,
    request: Option<contracts::ResourceRequestId>,
    build: Option<contracts::BuildRequestId>,
    attempt: Option<u32>,
    business: &str,
    amount: &FixedDecimal,
) -> Result<CreditHold, CreditError> {
    if owner.is_empty() || amount.scaled() < 0 || amount.scaled().unsigned_abs() >= 10_u128.pow(38)
    {
        return Err(CreditError::Invalid);
    }
    lock_account(tx, project).await?;
    let row=sqlx::query("SELECT *,amount::text AS amount_scaled,remaining::text AS remaining_scaled FROM resource.credit_holds WHERE request_id IS NOT DISTINCT FROM $1 AND build_request_id IS NOT DISTINCT FROM $2 AND build_attempt IS NOT DISTINCT FROM $3 AND business_key=$4")
      .bind(request.map(contracts::ResourceRequestId::as_uuid)).bind(build.map(contracts::BuildRequestId::as_uuid)).bind(attempt.map(i32::try_from).transpose().map_err(|_|CreditError::Invalid)?).bind(business).fetch_optional(&mut **tx).await?;
    if let Some(row) = row {
        let existing = decode_hold(&row)?;
        if existing.owner_client_id != owner
            || existing.project_id != project
            || existing.amount != *amount
        {
            return Err(CreditError::Conflict);
        }
        if request.is_some() {
            complete(tx, "credits_hold", key, &existing).await?;
        }
        return Ok(existing);
    }
    let hold = CreditHold {
        id: Uuid::now_v7(),
        project_id: project,
        request_id: request,
        build_request_id: build,
        build_attempt: attempt,
        business_key: business.into(),
        owner_client_id: owner.into(),
        amount: amount.clone(),
        remaining: amount.clone(),
        state: CreditHoldState::Pending,
        measurement_unknown: false,
        reason: None,
        created_at: now(tx).await?,
        unknown_since: None,
        reconciliation_due_at: None,
        expired_unknown_at: None,
    };
    sqlx::query("INSERT INTO resource.credit_holds(hold_id,project_id,request_id,build_request_id,build_attempt,business_key,owner_client_id,amount,remaining,state,created_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8::text::numeric,$8::text::numeric,'pending',$9)")
      .bind(hold.id).bind(project.as_uuid()).bind(request.map(contracts::ResourceRequestId::as_uuid)).bind(build.map(contracts::BuildRequestId::as_uuid)).bind(attempt.map(i32::try_from).transpose().map_err(|_|CreditError::Invalid)?).bind(business).bind(owner).bind(amount.scaled().to_string()).bind(hold.created_at.get()).execute(&mut **tx).await?;
    let mut e = entry(
        Uuid::now_v7(),
        Some(project),
        0,
        amount.scaled(),
        0,
        0,
        "hold",
        hold.created_at,
    )?;
    e.hold_id = Some(hold.id);
    e.reason = "preauthorized upper bound".into();
    e.owner_client_id = Some(owner.into());
    apply(tx, &e).await?;
    if request.is_some() {
        complete(tx, "credits_hold", key, &hold).await?;
    }
    Ok(hold)
}

/// Execution cannot reuse a released, spent or uncertain preauthorization snapshot.
pub(crate) async fn require_funded_in_transaction(
    tx: &mut Tx<'_>,
    owner: &str,
    id: Uuid,
    amount: &FixedDecimal,
) -> Result<(), CreditError> {
    let project: Uuid =
        sqlx::query_scalar("SELECT project_id FROM resource.credit_holds WHERE hold_id=$1")
            .bind(id)
            .fetch_one(&mut **tx)
            .await?;
    lock_account(
        tx,
        project
            .to_string()
            .parse()
            .map_err(|_| CreditError::Invalid)?,
    )
    .await?;
    let row=sqlx::query("SELECT *,amount::text AS amount_scaled,remaining::text AS remaining_scaled FROM resource.credit_holds WHERE hold_id=$1 FOR UPDATE").bind(id).fetch_one(&mut **tx).await?;
    let hold = decode_hold(&row)?;
    if hold.owner_client_id != owner {
        return Err(CreditError::Denied);
    }
    if hold.state != CreditHoldState::Pending
        || hold.measurement_unknown
        || hold.remaining.scaled() < amount.scaled()
    {
        return Err(CreditError::Insufficient);
    }
    Ok(())
}

fn optional_time(
    row: &sqlx::postgres::PgRow,
    name: &str,
) -> Result<Option<UtcTimestamp>, CreditError> {
    row.try_get::<Option<time::OffsetDateTime>, _>(name)?
        .map(UtcTimestamp::from_utc)
        .transpose()
        .map_err(|_| CreditError::Invalid)
}

async fn expire_hold(
    tx: &mut Tx<'_>,
    hold: &mut CreditHold,
    cutoff: UtcTimestamp,
) -> Result<bool, CreditError> {
    if !hold.measurement_unknown
        || hold.expired_unknown_at.is_some()
        || hold.reconciliation_due_at.is_none_or(|due| due > cutoff)
    {
        return Ok(false);
    }
    hold.expired_unknown_at = Some(cutoff);
    if matches!(
        hold.state,
        CreditHoldState::Pending | CreditHoldState::Unknown
    ) {
        apply_resolution(
            tx,
            hold,
            &CreditHoldResolutionRequest::Release {
                reason:
                    "one-hour unknown reconciliation deadline reached; late cost is platform cost"
                        .into(),
            },
        )
        .await?;
    } else {
        sqlx::query("UPDATE resource.credit_holds SET expired_unknown_at=$2 WHERE hold_id=$1 AND expired_unknown_at IS NULL").bind(hold.id).bind(cutoff.get()).execute(&mut **tx).await?;
    }
    Ok(true)
}
impl PgCreditStore {
    /// The maintenance loop uses the `PostgreSQL` clock as its observation cutoff.
    pub async fn expire_unknown_once(&self) -> Result<u64, CreditError> {
        let mut tx = self.pool.begin().await?;
        let cutoff = now(&mut tx).await?;
        tx.commit().await?;
        self.expire_unknown_at(cutoff).await
    }
    /// Applies one bounded batch at a trusted maintenance clock observation.
    pub async fn expire_unknown_at(&self, cutoff: UtcTimestamp) -> Result<u64, CreditError> {
        let candidates=sqlx::query("SELECT hold_id FROM resource.credit_holds WHERE measurement_unknown AND expired_unknown_at IS NULL AND reconciliation_due_at<=$1 ORDER BY project_id,hold_id LIMIT 100").bind(cutoff.get()).fetch_all(&self.pool).await?;
        let mut expired = 0;
        for candidate in candidates {
            let id: Uuid = candidate.try_get("hold_id")?;
            let mut tx = self.pool.begin().await?;
            let project: Uuid =
                sqlx::query_scalar("SELECT project_id FROM resource.credit_holds WHERE hold_id=$1")
                    .bind(id)
                    .fetch_one(&mut *tx)
                    .await?;
            lock_account(
                &mut tx,
                project
                    .to_string()
                    .parse()
                    .map_err(|_| CreditError::Invalid)?,
            )
            .await?;
            let row=sqlx::query("SELECT *,amount::text AS amount_scaled,remaining::text AS remaining_scaled FROM resource.credit_holds WHERE hold_id=$1 FOR UPDATE").bind(id).fetch_one(&mut *tx).await?;
            let mut hold = decode_hold(&row)?;
            if expire_hold(&mut tx, &mut hold, cutoff).await? {
                expired += 1;
            }
            tx.commit().await?;
        }
        Ok(expired)
    }
}

/// A replayed handoff retains the identity that funded the original request.
pub(crate) async fn check_request_hold_owner(
    tx: &mut Tx<'_>,
    request: contracts::ResourceRequestId,
    owner: &str,
    require_paid: bool,
) -> Result<(), CreditError> {
    let existing:Option<String>=sqlx::query_scalar("SELECT owner_client_id FROM resource.credit_holds WHERE request_id=$1 AND business_key='metered-usage'").bind(request.as_uuid()).fetch_optional(&mut **tx).await?;
    match existing {
        Some(existing) if existing != owner => Err(CreditError::Denied),
        None if require_paid => Err(CreditError::Insufficient),
        _ => Ok(()),
    }
}

/// Funds the cumulative approved intervals, reusing any original excess preauthorization.
/// The native lease revision and immutable window identity fence replays before this entry.
pub(crate) async fn extend_environment_hold(
    tx: &mut Tx<'_>,
    owner: &str,
    id: Uuid,
    required_total: &FixedDecimal,
) -> Result<CreditHold, CreditError> {
    if required_total.scaled() < 0 || required_total.scaled().unsigned_abs() >= 10_u128.pow(38) {
        return Err(CreditError::Invalid);
    }
    let project: Uuid =
        sqlx::query_scalar("SELECT project_id FROM resource.credit_holds WHERE hold_id=$1")
            .bind(id)
            .fetch_one(&mut **tx)
            .await?;
    let project_id = project
        .to_string()
        .parse()
        .map_err(|_| CreditError::Invalid)?;
    lock_account(tx, project_id).await?;
    let row=sqlx::query("SELECT *,amount::text AS amount_scaled,remaining::text AS remaining_scaled FROM resource.credit_holds WHERE hold_id=$1 FOR UPDATE").bind(id).fetch_one(&mut **tx).await?;
    let mut hold = decode_hold(&row)?;
    if hold.owner_client_id != owner {
        return Err(CreditError::Denied);
    }
    if hold.state != CreditHoldState::Pending
        || hold.measurement_unknown
        || hold.expired_unknown_at.is_some()
    {
        return Err(CreditError::Insufficient);
    }
    let amount = decimal(
        required_total
            .scaled()
            .saturating_sub(hold.amount.scaled())
            .max(0),
    )?;
    let available: String = sqlx::query_scalar(
        "SELECT (balance-reserved)::text FROM resource.credit_accounts WHERE project_id=$1",
    )
    .bind(project)
    .fetch_one(&mut **tx)
    .await?;
    if available
        .parse::<i128>()
        .map_err(|_| CreditError::Invalid)?
        < amount.scaled()
    {
        return Err(CreditError::Insufficient);
    }
    hold.amount = decimal(
        hold.amount
            .scaled()
            .checked_add(amount.scaled())
            .ok_or(CreditError::Invalid)?,
    )?;
    hold.remaining = decimal(
        hold.remaining
            .scaled()
            .checked_add(amount.scaled())
            .ok_or(CreditError::Invalid)?,
    )?;
    sqlx::query("UPDATE resource.credit_holds SET amount=$2::text::numeric,remaining=$3::text::numeric WHERE hold_id=$1").bind(id).bind(hold.amount.scaled().to_string()).bind(hold.remaining.scaled().to_string()).execute(&mut **tx).await?;
    if amount.scaled() > 0 {
        let mut movement = entry(
            Uuid::now_v7(),
            Some(project_id),
            0,
            amount.scaled(),
            0,
            0,
            "hold",
            now(tx).await?,
        )?;
        movement.hold_id = Some(id);
        movement.owner_client_id = Some(owner.into());
        movement.reason = "approved Environment additional interval".into();
        apply(tx, &movement).await?;
    }
    Ok(hold)
}
