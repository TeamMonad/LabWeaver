//! Control-owned, durable inbox. This module never calls a resource mutation or ledger API.
use contracts::events::{
    CloudEvent, EVENT_CONTRACTS, ResourceLeaseChanged, ResourceRequestChanged, subjects,
};
use contracts::notifications::{
    Notification, NotificationEmailState, NotificationKind, NotificationListQuery,
    NotificationPage, NotificationUnreadCount,
};
use contracts::resource::{ResourceLeaseState, ResourceTarget};
use contracts::{ActorId, EventId, UtcTimestamp};
use persistence_sqlx::Sha256Digest;
use sqlx::{PgPool, Postgres, Row, Transaction};
use uuid::Uuid;

pub mod consumer;
pub mod smtp;

/// Payload-free diagnostics for projection and private inbox access.
#[derive(Debug, thiserror::Error)]
pub enum NotificationError {
    /// A wire event, query, or deployment value is invalid.
    #[error("LW_NOTIFICATION_INVALID")]
    Invalid,
    /// A stable owner event ID was reused for different bytes.
    #[error("LW_NOTIFICATION_EVENT_CONFLICT")]
    EventConflict,
    /// The recipient cannot currently see this notification.
    #[error("LW_NOTIFICATION_NOT_FOUND")]
    NotFound,
    /// Persistence is unavailable; delivery can retry within its bounded policy.
    #[error("LW_NOTIFICATION_PERSISTENCE_FAILED")]
    Persistence(#[from] sqlx::Error),
}
impl From<serde_json::Error> for NotificationError {
    fn from(_: serde_json::Error) -> Self {
        Self::Invalid
    }
}

/// UTC arithmetic is independent of the presentation timezone. An exact-window lease is
/// not short: its reminder is due at activation. Short leases get one immediate hint.
pub fn reminder_times(
    from: UtcTimestamp,
    until: UtcTimestamp,
    windows: &[u64],
) -> Result<Vec<(i64, UtcTimestamp, bool)>, NotificationError> {
    if until <= from || windows.len() > 16 {
        return Err(NotificationError::Invalid);
    }
    let mut result = Vec::with_capacity(windows.len());
    let mut short = false;
    for window in windows {
        let seconds = i64::try_from(*window).map_err(|_| NotificationError::Invalid)?;
        if !(1..=31_536_000).contains(&seconds) {
            return Err(NotificationError::Invalid);
        }
        let at = until
            .get()
            .checked_sub(time::Duration::seconds(seconds))
            .ok_or(NotificationError::Invalid)?;
        if at < from.get() {
            short = true;
        } else {
            result.push((
                seconds,
                UtcTimestamp::from_utc(at).map_err(|_| NotificationError::Invalid)?,
                false,
            ));
        }
    }
    if short {
        result.push((-1, from, true));
    }
    result.sort_by_key(|entry| entry.0);
    result.dedup_by_key(|entry| entry.0);
    Ok(result)
}

/// Validate the owner envelope before advancing the aggregate watermark. Sequence fences
/// prevent a delayed event from resurrecting a cancelled reminder after renewal/revocation.
pub async fn project(
    pool: &PgPool,
    bytes: &[u8],
    windows: &[u64],
    email_enabled: bool,
) -> Result<(), NotificationError> {
    if bytes.len() > 1_048_576 {
        return Err(NotificationError::Invalid);
    }
    let event: CloudEvent<serde_json::Value> =
        contracts::parse_strict_json(bytes).map_err(|_| NotificationError::Invalid)?;
    let contract = EVENT_CONTRACTS
        .iter()
        .copied()
        .find(|c| c.subject == event.subject)
        .ok_or(NotificationError::Invalid)?;
    event
        .validate(contract)
        .map_err(|_| NotificationError::Invalid)?;
    if event.aggregate_sequence.0 != event.aggregate_revision.get() {
        return Err(NotificationError::Invalid);
    }
    let lease_event = event.subject.starts_with("labweaver.resource.lease.");
    let (request, lease) = if lease_event {
        let data: ResourceLeaseChanged = serde_json::from_value(event.data)?;
        data.lease
            .validate()
            .map_err(|_| NotificationError::Invalid)?;
        if data.lease.request_id != data.request.id
            || data.lease.revision != event.aggregate_revision
        {
            return Err(NotificationError::Invalid);
        }
        (data.request, Some(data.lease))
    } else if event.subject.starts_with("labweaver.resource.request.") {
        let data: ResourceRequestChanged = serde_json::from_value(event.data)?;
        if data.request.revision != event.aggregate_revision {
            return Err(NotificationError::Invalid);
        }
        (data.request, None)
    } else {
        return Err(NotificationError::Invalid);
    };
    request.validate().map_err(|_| NotificationError::Invalid)?;
    if request.project_id != event.project_id || request.course_id != event.course_id {
        return Err(NotificationError::Invalid);
    }
    let aggregate = lease
        .as_ref()
        .map_or(request.id.as_uuid(), |l| l.id.as_uuid());
    let digest = Sha256Digest::of_bytes(bytes).to_string();
    let mut tx = pool.begin().await?;
    let inserted = sqlx::query("INSERT INTO control.notification_inbox(event_id,payload_sha256) VALUES($1,$2) ON CONFLICT DO NOTHING")
        .bind(event.id.as_uuid()).bind(&digest).execute(&mut *tx).await?.rows_affected();
    if inserted == 0 {
        let stored: String = sqlx::query_scalar(
            "SELECT payload_sha256 FROM control.notification_inbox WHERE event_id=$1",
        )
        .bind(event.id.as_uuid())
        .fetch_one(&mut *tx)
        .await?;
        return if stored == digest {
            Ok(())
        } else {
            Err(NotificationError::EventConflict)
        };
    }
    let advanced = sqlx::query("INSERT INTO control.notification_sources(aggregate_id,sequence,event_id) VALUES($1,$2,$3) ON CONFLICT(aggregate_id) DO UPDATE SET sequence=EXCLUDED.sequence,event_id=EXCLUDED.event_id WHERE notification_sources.sequence<EXCLUDED.sequence")
        .bind(aggregate).bind(i64::try_from(event.aggregate_sequence.0).map_err(|_| NotificationError::Invalid)?).bind(event.id.as_uuid()).execute(&mut *tx).await?.rows_affected();
    if advanced == 0 {
        tx.commit().await?;
        return Ok(());
    }
    if let Some(current) = &lease {
        let active = matches!(
            current.state,
            ResourceLeaseState::Active | ResourceLeaseState::Expiring
        );
        sqlx::query("UPDATE control.notifications SET cancelled=true WHERE aggregate_id=$1 AND reminder_seconds<>0 AND (NOT $3 OR deadline_at IS DISTINCT FROM $2)")
            .bind(aggregate).bind(current.expires_at.map(UtcTimestamp::get)).bind(active).execute(&mut *tx).await?;
    }
    let url = match (&request.target, lease_event) {
        (ResourceTarget::Environment { environment_id, .. }, true) => format!(
            "/researcher/environments?projectId={}&environmentId={environment_id}",
            request.project_id
        ),
        _ => format!(
            "/researcher/resources?projectId={}&requestId={}",
            request.project_id, request.id
        ),
    };
    let mut notice = Notification {
        id: EventId::new(),
        event_id: event.id,
        project_id: event.project_id,
        kind: if lease_event {
            NotificationKind::LeaseChanged
        } else {
            NotificationKind::ResourceDecision
        },
        object_url: url,
        summary: match event.subject.as_str() {
            subjects::RESOURCE_REQUEST_APPROVED => {
                "资源申请已批准；请打开对象查看当前分配进度和实际可用期限。"
            }
            subjects::RESOURCE_REQUEST_REJECTED => {
                "资源申请被拒绝；请打开申请查看处理结果，修改后按需重新申请。"
            }
            subjects::RESOURCE_LEASE_CREATED => {
                "租约已创建；尚不表示环境已经就绪，请打开对象查看准备进度。"
            }
            subjects::RESOURCE_LEASE_RENEWED => {
                "租约期限已更新；请打开对象核对当前期限并提前保存数据。"
            }
            subjects::RESOURCE_LEASE_REVOKED => {
                "租约授权已撤销；请打开对象查看停止和回收进度。撤销授权不表示资源已释放。"
            }
            subjects::RESOURCE_LEASE_EXPIRED => {
                "租约已到期；请打开对象核对资源回收和费用结算的当前状态。"
            }
            _ => "资源状态有更新；请打开对象查看当前状态与下一步。停止或撤销授权不表示资源已释放。",
        }
        .to_owned(),
        observed_at: event.time,
        deadline: lease.as_ref().and_then(|l| l.expires_at),
        read_at: None,
        email_state: if email_enabled {
            NotificationEmailState::Pending
        } else {
            NotificationEmailState::Disabled
        },
        email_attempts: 0,
        email_diagnostic: None,
        email_detail: None,
    };
    if lease_event
        || matches!(
            event.subject.as_str(),
            subjects::RESOURCE_REQUEST_APPROVED | subjects::RESOURCE_REQUEST_REJECTED
        )
    {
        insert(
            &mut tx,
            &notice,
            request.requester_id,
            aggregate,
            0,
            event.time,
            email_enabled,
        )
        .await?;
    }
    if event.subject == subjects::RESOURCE_REQUEST_SUBMITTED
        && windows
            .iter()
            .any(|window| request.requested_duration_seconds < *window)
    {
        notice.kind = NotificationKind::ShortLease;
        "申请租期短于至少一个提醒窗口；实际期限以资源批准和租约为准，请提前保存数据。"
            .clone_into(&mut notice.summary);
        insert(
            &mut tx,
            &notice,
            request.requester_id,
            aggregate,
            -1,
            event.time,
            email_enabled,
        )
        .await?;
    }
    if let Some(lease) = lease
        && matches!(
            lease.state,
            ResourceLeaseState::Active | ResourceLeaseState::Expiring
        )
        && let Some((from, until)) = lease.active_from.zip(lease.expires_at)
    {
        for (window, at, short) in reminder_times(from, until, windows)? {
            notice.id = EventId::new();
            notice.kind = if short {
                NotificationKind::ShortLease
            } else {
                NotificationKind::LeaseExpiryReminder
            };
            (if short {
                "本次租约短于至少一个提醒窗口；请留意到期时间，提前续期或保存数据。"
            } else {
                "租约即将到期；请打开对象确认当前期限，续期或保存数据。"
            })
            .clone_into(&mut notice.summary);
            insert(
                &mut tx,
                &notice,
                request.requester_id,
                aggregate,
                window,
                at,
                email_enabled,
            )
            .await?;
        }
    }
    tx.commit().await?;
    Ok(())
}
async fn insert(
    tx: &mut Transaction<'_, Postgres>,
    notice: &Notification,
    recipient: ActorId,
    aggregate: Uuid,
    window: i64,
    due: UtcTimestamp,
    email: bool,
) -> Result<(), NotificationError> {
    sqlx::query("INSERT INTO control.notifications(notification_id,event_id,recipient_id,project_id,aggregate_id,reminder_seconds,due_at,contract,email_state,email_next_at,deadline_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$7,$10) ON CONFLICT DO NOTHING")
        .bind(notice.id.as_uuid()).bind(notice.event_id.as_uuid()).bind(recipient.as_uuid()).bind(notice.project_id.as_uuid()).bind(aggregate).bind(window).bind(due.get()).bind(serde_json::to_value(notice)?).bind(if email { "pending" } else { "disabled" }).bind(notice.deadline.map(UtcTimestamp::get)).execute(&mut **tx).await?;
    Ok(())
}
// The recipient is not itself an object grant. Membership is rechecked on every list/count/read.
const VISIBLE: &str = "n.recipient_id=$1 AND NOT n.cancelled AND n.due_at<=clock_timestamp() AND (n.reminder_seconds<=0 OR n.deadline_at>clock_timestamp()) AND EXISTS(SELECT 1 FROM control.project_memberships m WHERE m.project_id=n.project_id AND m.actor_id=$1 AND m.state='active' AND (m.expires_at IS NULL OR m.expires_at>clock_timestamp()))";
/// Lists only this recipient's currently authorized, due notifications.
pub async fn list(
    pool: &PgPool,
    actor: ActorId,
    query: &NotificationListQuery,
) -> Result<NotificationPage, NotificationError> {
    let cursor = query
        .cursor
        .as_deref()
        .map(str::parse::<i64>)
        .transpose()
        .map_err(|_| NotificationError::Invalid)?
        .unwrap_or(i64::MAX);
    let limit = query.limit.unwrap_or(50);
    if cursor <= 0 || !(1..=100).contains(&limit) {
        return Err(NotificationError::Invalid);
    }
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!("SELECT n.* FROM control.notifications n WHERE {VISIBLE} AND n.ordinal<$2 ORDER BY n.ordinal DESC LIMIT $3")))
        .bind(actor.as_uuid()).bind(cursor).bind(i64::from(limit)+1).fetch_all(pool).await?;
    let more = rows.len() > usize::from(limit);
    let next_cursor = if more {
        rows.get(usize::from(limit) - 1)
            .map(|r| r.try_get::<i64, _>("ordinal"))
            .transpose()?
            .map(|n| n.to_string())
    } else {
        None
    };
    let items = rows
        .iter()
        .take(usize::from(limit))
        .map(decode)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(NotificationPage { items, next_cursor })
}
/// Counts unread rows using the same current membership fence as listing.
pub async fn unread(
    pool: &PgPool,
    actor: ActorId,
) -> Result<NotificationUnreadCount, NotificationError> {
    let count: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT count(*) FROM control.notifications n WHERE {VISIBLE} AND n.read_at IS NULL"
    )))
    .bind(actor.as_uuid())
    .fetch_one(pool)
    .await?;
    Ok(NotificationUnreadCount {
        unread_count: u64::try_from(count).map_err(|_| NotificationError::Invalid)?,
    })
}
/// Marks a visible notification read idempotently; a different recipient gets not found.
pub async fn mark_read(
    pool: &PgPool,
    actor: ActorId,
    id: EventId,
) -> Result<Notification, NotificationError> {
    let row = sqlx::query(sqlx::AssertSqlSafe(format!("UPDATE control.notifications n SET read_at=COALESCE(read_at,date_trunc('milliseconds',clock_timestamp())) WHERE {VISIBLE} AND notification_id=$2 RETURNING n.*")))
        .bind(actor.as_uuid()).bind(id.as_uuid()).fetch_optional(pool).await?.ok_or(NotificationError::NotFound)?;
    decode(&row)
}
fn decode(row: &sqlx::postgres::PgRow) -> Result<Notification, NotificationError> {
    let mut notice: Notification = serde_json::from_value(row.try_get("contract")?)?;
    notice.read_at = row
        .try_get::<Option<time::OffsetDateTime>, _>("read_at")?
        .map(UtcTimestamp::from_utc)
        .transpose()
        .map_err(|_| NotificationError::Invalid)?;
    notice.email_state = match row.try_get::<String, _>("email_state")?.as_str() {
        "disabled" => NotificationEmailState::Disabled,
        "sent" => NotificationEmailState::Sent,
        "failed" => NotificationEmailState::Failed,
        "exhausted" => NotificationEmailState::Exhausted,
        "no_verified_email" => NotificationEmailState::NoVerifiedEmail,
        "delivery_unknown" => NotificationEmailState::DeliveryUnknown,
        _ => NotificationEmailState::Pending,
    };
    notice.email_detail = if notice.email_state == NotificationEmailState::DeliveryUnknown {
        Some("邮件可能已送达，系统无法确认；为避免重复发送，不会自动重发。".to_owned())
    } else {
        None
    };
    notice.email_attempts = u16::try_from(row.try_get::<i16, _>("email_attempts")?)
        .map_err(|_| NotificationError::Invalid)?;
    notice.email_diagnostic = row.try_get("email_diagnostic")?;
    Ok(notice)
}
