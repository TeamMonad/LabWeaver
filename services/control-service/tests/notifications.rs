//! Real `PostgreSQL` and `JetStream` exercise durable notification projection and private reads.
#![allow(
    clippy::too_many_lines,
    reason = "integration scenarios keep owner transitions and assertions together"
)]
use base64::Engine as _;
use contracts::events::{
    CloudEvent, EVENT_CONTRACTS, ResourceLeaseChanged, SPEC_VERSION, subjects,
};
use contracts::notifications::{NotificationEmailState, NotificationKind, NotificationListQuery};
use contracts::resource::{
    ResourceLease, ResourceLeaseState, ResourceRequest, ResourceRequestState, ResourceTarget,
    WorkloadResources,
};
use contracts::{
    ActorId, CapacityClaimId, EventId, LeaseId, ProjectId, ResourceRequestId, Revision, Sequence,
    TaskRunId, UtcTimestamp,
};
use control_service::notifications::{
    self, NotificationError,
    consumer::NotificationConsumer,
    smtp::{SmtpConfiguration, SmtpWorker},
};
use persistence_sqlx::Domain;
use sqlx::{PgPool, postgres::PgPoolOptions};
use std::{error::Error, time::Duration};
use testcontainers::{
    GenericImage, ImageExt,
    core::{IntoContainerPort, WaitFor},
    runners::AsyncRunner,
};
use testcontainers_modules::postgres::Postgres;
mod support;
type Result<T = ()> = std::result::Result<T, Box<dyn Error>>;

async fn database() -> Result<(testcontainers::ContainerAsync<Postgres>, PgPool)> {
    let pg = Postgres::default().with_tag("17.5-alpine").start().await?;
    let pool = PgPoolOptions::new()
        .max_connections(8)
        .connect(&format!(
            "postgres://postgres:postgres@127.0.0.1:{}/postgres",
            pg.get_host_port_ipv4(5432).await?
        ))
        .await?;
    sqlx::query("CREATE ROLE lw_control_runtime NOLOGIN")
        .execute(&pool)
        .await?;
    support::apply_domain_migrations(&pool, Domain::Control).await?;
    support::apply_domain_migrations(&pool, Domain::Access).await?;
    Ok((pg, pool))
}
fn request(now: UtcTimestamp) -> Result<ResourceRequest> {
    Ok(ResourceRequest {
        id: ResourceRequestId::new(),
        generation: 1,
        request_key: "notification-test".into(),
        requester_id: ActorId::new(),
        project_id: ProjectId::new(),
        course_id: None,
        target: ResourceTarget::Task {
            task_run_id: TaskRunId::new(),
        },
        requested_resources: WorkloadResources {
            cpu_millicores: 100,
            memory_bytes: 1024,
            storage_bytes: 1024,
            gpu: None,
        },
        requested_duration_seconds: 1800,
        state: ResourceRequestState::Reviewing,
        revision: Revision::new(1)?,
        created_at: now,
        updated_at: now,
        diagnostic_code: None,
    })
}
async fn member(pool: &PgPool, request: &ResourceRequest) -> Result {
    sqlx::query("INSERT INTO control.project_memberships(project_id,actor_id,role,object_role,state,revision) VALUES($1,$2,'student','owner','active',1)").bind(request.project_id.as_uuid()).bind(request.requester_id.as_uuid()).execute(pool).await?;
    Ok(())
}
fn lease_event(
    request: &ResourceRequest,
    from: UtcTimestamp,
    until: UtcTimestamp,
    subject: &str,
) -> Result<CloudEvent<ResourceLeaseChanged>> {
    let contract = EVENT_CONTRACTS
        .iter()
        .find(|c| c.subject == subject)
        .ok_or("missing event contract")?;
    Ok(CloudEvent {
        specversion: SPEC_VERSION.into(),
        id: EventId::new(),
        source: contract.source().into(),
        event_type: subject.into(),
        subject: subject.into(),
        time: from,
        datacontenttype: "application/json".into(),
        dataschema: contract.data_schema(),
        project_id: request.project_id,
        course_id: None,
        aggregate_revision: Revision::new(2)?,
        aggregate_sequence: Sequence(2),
        trace_id: "notification-test".into(),
        data: ResourceLeaseChanged {
            request: request.clone(),
            lease: ResourceLease {
                id: LeaseId::new(),
                request_id: request.id,
                claim_id: CapacityClaimId::new(),
                state: ResourceLeaseState::Active,
                revision: Revision::new(2)?,
                active_from: Some(from),
                expires_at: Some(until),
                revoke_reason_code: None,
                created_at: from,
                updated_at: from,
            },
        },
    })
}
#[tokio::test]
async fn owner_outbox_replay_is_durable_without_smtp_and_current_membership_fences_every_read()
-> Result {
    let (_pg, pool) = database().await?;
    support::apply_domain_migrations(&pool, Domain::Resource).await?;
    let store = resource_service::store::PgResourceStore::new(pool.clone());
    let request = request(store.current_time().await?)?;
    member(&pool, &request).await?;
    store
        .create_task(
            "notification-create",
            &request,
            60_000,
            "notification-trace",
        )
        .await?;
    let payload: serde_json::Value =
        sqlx::query_scalar("SELECT payload FROM resource.outbox_events WHERE subject=$1")
            .bind(subjects::RESOURCE_REQUEST_SUBMITTED)
            .fetch_one(&pool)
            .await?;
    let bytes = serde_json::to_vec(&payload)?;
    notifications::project(&pool, &bytes, &[86_400, 3600], false).await?;
    notifications::project(&pool, &bytes, &[86_400, 3600], false).await?;
    let page = notifications::list(
        &pool,
        request.requester_id,
        &NotificationListQuery::default(),
    )
    .await?;
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].kind, NotificationKind::ShortLease);
    assert_eq!(page.items[0].email_state, NotificationEmailState::Disabled);
    assert_eq!(
        notifications::unread(&pool, request.requester_id)
            .await?
            .unread_count,
        1
    );
    let id = page.items[0].id;
    let read = notifications::mark_read(&pool, request.requester_id, id).await?;
    assert_eq!(
        notifications::mark_read(&pool, request.requester_id, id)
            .await?
            .read_at,
        read.read_at
    );
    assert!(matches!(
        notifications::mark_read(&pool, ActorId::new(), id).await,
        Err(NotificationError::NotFound)
    ));
    let rejected = store
        .reject_or_cancel(
            "notification-reject",
            request.id,
            request.revision,
            ResourceRequestState::Rejected,
            request.requester_id,
            "notification-reject-trace",
        )
        .await?;
    let decision: serde_json::Value =
        sqlx::query_scalar("SELECT payload FROM resource.outbox_events WHERE subject=$1")
            .bind(subjects::RESOURCE_REQUEST_REJECTED)
            .fetch_one(&pool)
            .await?;
    let decision_bytes = serde_json::to_vec(&decision)?;
    notifications::project(&pool, &decision_bytes, &[86400, 3600], false).await?;
    notifications::project(&pool, &decision_bytes, &[86400, 3600], false).await?;
    let decisions = notifications::list(
        &pool,
        request.requester_id,
        &NotificationListQuery::default(),
    )
    .await?;
    assert_eq!(decisions.items.len(), 2);
    assert_eq!(decisions.items[0].kind, NotificationKind::ResourceDecision);
    assert!(decisions.items[0].summary.contains("被拒绝"));
    sqlx::query("UPDATE control.project_memberships SET state='revoked' WHERE actor_id=$1")
        .bind(request.requester_id.as_uuid())
        .execute(&pool)
        .await?;
    assert!(
        notifications::list(
            &pool,
            request.requester_id,
            &NotificationListQuery::default()
        )
        .await?
        .items
        .is_empty()
    );
    assert_eq!(
        notifications::unread(&pool, request.requester_id)
            .await?
            .unread_count,
        0
    );
    assert!(matches!(
        notifications::mark_read(&pool, request.requester_id, id).await,
        Err(NotificationError::NotFound)
    ));
    assert_eq!(store.load(request.id).await?.revision, rejected.revision);
    Ok(())
}
#[tokio::test]
async fn lease_windows_deduplicate_renew_revoke_and_ignore_stale_events_across_database_timezones()
-> Result {
    let (_pg, pool) = database().await?;
    let now: time::OffsetDateTime =
        sqlx::query_scalar("SELECT date_trunc('milliseconds',clock_timestamp())")
            .fetch_one(&pool)
            .await?;
    let from = UtcTimestamp::from_utc(now - time::Duration::minutes(30))?;
    let until = UtcTimestamp::from_utc(from.get() + time::Duration::hours(1))?;
    let request = request(from)?;
    member(&pool, &request).await?;
    let event = lease_event(&request, from, until, subjects::RESOURCE_LEASE_ACTIVATED)?;
    let bytes = serde_json::to_vec(&event)?;
    for zone in ["UTC", "America/New_York", "Asia/Shanghai"] {
        sqlx::query("SELECT set_config('TimeZone',$1,false)")
            .bind(zone)
            .execute(&pool)
            .await?;
        notifications::project(&pool, &bytes, &[86_400, 3600], false).await?;
    }
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM control.notifications")
        .fetch_one(&pool)
        .await?;
    assert_eq!(count, 3); // lifecycle + exact 1h + one short-window hint
    let due: time::OffsetDateTime =
        sqlx::query_scalar("SELECT due_at FROM control.notifications WHERE reminder_seconds=3600")
            .fetch_one(&pool)
            .await?;
    assert_eq!(due, from.get());
    let mut changed = event.clone();
    changed.id = EventId::new();
    changed.subject = subjects::RESOURCE_LEASE_EXPIRING.into();
    changed.event_type = changed.subject.clone();
    changed.dataschema = EVENT_CONTRACTS
        .iter()
        .find(|c| c.subject == changed.subject)
        .ok_or("event")?
        .data_schema();
    changed.aggregate_revision = Revision::new(3)?;
    changed.aggregate_sequence = Sequence(3);
    changed.data.lease.revision = changed.aggregate_revision;
    changed.data.lease.state = ResourceLeaseState::Expiring;
    notifications::project(
        &pool,
        &serde_json::to_vec(&changed)?,
        &[86_400, 3600],
        false,
    )
    .await?;
    let reminders: i64 =
        sqlx::query_scalar("SELECT count(*) FROM control.notifications WHERE reminder_seconds<>0")
            .fetch_one(&pool)
            .await?;
    assert_eq!(reminders, 2);
    let mut renewed = changed;
    renewed.id = EventId::new();
    renewed.subject = subjects::RESOURCE_LEASE_RENEWED.into();
    renewed.event_type.clone_from(&renewed.subject);
    renewed.dataschema = EVENT_CONTRACTS
        .iter()
        .find(|c| c.subject == renewed.subject)
        .ok_or("renewed contract")?
        .data_schema();
    renewed.aggregate_revision = Revision::new(4)?;
    renewed.aggregate_sequence = Sequence(4);
    renewed.data.lease.revision = renewed.aggregate_revision;
    renewed.data.lease.state = ResourceLeaseState::Active;
    let new_until = UtcTimestamp::from_utc(until.get() + time::Duration::hours(2))?;
    renewed.data.lease.expires_at = Some(new_until);
    notifications::project(
        &pool,
        &serde_json::to_vec(&renewed)?,
        &[86_400, 3600],
        false,
    )
    .await?;
    let old_active:i64=sqlx::query_scalar("SELECT count(*) FROM control.notifications WHERE reminder_seconds<>0 AND deadline_at=$1 AND NOT cancelled").bind(until.get()).fetch_one(&pool).await?;
    assert_eq!(old_active, 0);
    let mut conflicting = renewed.clone();
    conflicting.data.lease.expires_at = Some(until);
    assert!(matches!(
        notifications::project(
            &pool,
            &serde_json::to_vec(&conflicting)?,
            &[86_400, 3600],
            false
        )
        .await,
        Err(NotificationError::EventConflict)
    ));
    let mut revoked = renewed;
    revoked.id = EventId::new();
    revoked.subject = subjects::RESOURCE_LEASE_REVOKED.into();
    revoked.event_type = revoked.subject.clone();
    revoked.dataschema = EVENT_CONTRACTS
        .iter()
        .find(|c| c.subject == revoked.subject)
        .ok_or("event")?
        .data_schema();
    revoked.aggregate_revision = Revision::new(5)?;
    revoked.aggregate_sequence = Sequence(5);
    revoked.data.lease.revision = revoked.aggregate_revision;
    revoked.data.lease.state = ResourceLeaseState::Revoked;
    notifications::project(
        &pool,
        &serde_json::to_vec(&revoked)?,
        &[86_400, 3600],
        false,
    )
    .await?;
    let mut stale = event;
    stale.id = EventId::new();
    notifications::project(&pool, &serde_json::to_vec(&stale)?, &[86_400, 3600], false).await?;
    let active: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM control.notifications WHERE reminder_seconds<>0 AND NOT cancelled",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(active, 0);
    let page = notifications::list(
        &pool,
        request.requester_id,
        &NotificationListQuery {
            cursor: None,
            limit: Some(1),
        },
    )
    .await?;
    assert_eq!(page.items.len(), 1);
    assert!(page.next_cursor.is_some());
    assert_eq!(page.items[0].deadline, Some(new_until));
    assert!(page.items[0].summary.contains("不表示资源已释放"));
    let next = notifications::list(
        &pool,
        request.requester_id,
        &NotificationListQuery {
            cursor: page.next_cursor,
            limit: Some(100),
        },
    )
    .await?;
    assert!(!next.items.iter().any(|n| n.id == page.items[0].id));
    Ok(())
}
#[test]
fn utc_short_and_exact_window_boundaries() -> Result {
    let from: UtcTimestamp = "2026-03-08T06:30:00.000Z".parse()?;
    let until = UtcTimestamp::from_utc(from.get() + time::Duration::hours(1))?;
    let reminders = notifications::reminder_times(from, until, &[86400, 3600])?;
    assert_eq!(reminders.len(), 2);
    assert_eq!(reminders.iter().find(|r| r.0 == 3600).ok_or("1h")?.1, from);
    assert_eq!(reminders.iter().filter(|r| r.2).count(), 1);
    let short = UtcTimestamp::from_utc(from.get() + time::Duration::seconds(3599))?;
    assert_eq!(
        notifications::reminder_times(from, short, &[86400, 3600])?.len(),
        1
    );
    assert!(notifications::reminder_times(from, from, &[3600]).is_err());
    assert!(
        "2026-03-08T01:30:00.000-05:00"
            .parse::<UtcTimestamp>()
            .is_err()
    );
    Ok(())
}
#[tokio::test]
async fn jetstream_replay_projects_once_after_restart() -> Result {
    let (_pg, pool) = database().await?;
    let now: time::OffsetDateTime =
        sqlx::query_scalar("SELECT date_trunc('milliseconds',clock_timestamp())")
            .fetch_one(&pool)
            .await?;
    let from = UtcTimestamp::from_utc(now - time::Duration::minutes(30))?;
    let request = request(from)?;
    member(&pool, &request).await?;
    let event = lease_event(
        &request,
        from,
        UtcTimestamp::from_utc(from.get() + time::Duration::hours(1))?,
        subjects::RESOURCE_LEASE_ACTIVATED,
    )?;
    let config = std::process::Command::new("python3")
        .arg(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../tests/fixtures/notification_nats.py"),
        )
        .output()?;
    assert!(config.status.success(), "NATS permission fixture failed");
    let nats = GenericImage::new("nats", "2.11.8-alpine")
        .with_exposed_port(4222.tcp())
        .with_wait_for(WaitFor::message_on_stderr("Server is ready"))
        .with_copy_to("/etc/nats/notifications.json", config.stdout)
        .with_cmd(["-c", "/etc/nats/notifications.json"])
        .start()
        .await?;
    let endpoint = format!("nats://127.0.0.1:{}", nats.get_host_port_ipv4(4222).await?);
    let operator = async_nats::ConnectOptions::new()
        .user_and_password("operator".into(), "operator-fixture".into())
        .connect(&endpoint)
        .await?;
    let resource = async_nats::ConnectOptions::new()
        .user_and_password("resource-service".into(), "identity-fixture".into())
        .connect(&endpoint)
        .await?;
    let client = async_nats::ConnectOptions::new()
        .user_and_password("control-service".into(), "identity-fixture".into())
        .connect(&endpoint)
        .await?;
    let context = async_nats::jetstream::new(operator);
    let publisher = async_nats::jetstream::new(resource);
    context
        .create_stream(async_nats::jetstream::stream::Config {
            name: "LABWEAVER_RESOURCE_EVENTS".into(),
            subjects: vec!["labweaver.resource.>".into()],
            ..Default::default()
        })
        .await?
        .create_consumer(async_nats::jetstream::consumer::pull::Config {
            durable_name: Some("control-notifications-v1".into()),
            filter_subjects: vec!["labweaver.resource.>".into()],
            max_deliver: 3,
            ack_policy: async_nats::jetstream::consumer::AckPolicy::Explicit,
            ..Default::default()
        })
        .await?;
    for _ in 0..2 {
        publisher
            .publish(event.subject.clone(), serde_json::to_vec(&event)?.into())
            .await?
            .await?;
    }
    let mut created = lease_event(
        &request,
        from,
        UtcTimestamp::from_utc(from.get() + time::Duration::hours(1))?,
        subjects::RESOURCE_LEASE_CREATED,
    )?;
    created.aggregate_revision = Revision::new(1)?;
    created.aggregate_sequence = Sequence(1);
    created.data.lease.revision = created.aggregate_revision;
    created.data.lease.state = ResourceLeaseState::Allocating;
    created.data.lease.active_from = None;
    created.data.lease.expires_at = None;
    publisher
        .publish(
            created.subject.clone(),
            serde_json::to_vec(&created)?.into(),
        )
        .await?
        .await?;
    let mut fenced = created;
    fenced.id = EventId::new();
    fenced.subject = subjects::RESOURCE_LEASE_FENCED.into();
    fenced.event_type.clone_from(&fenced.subject);
    fenced.dataschema = EVENT_CONTRACTS
        .iter()
        .find(|c| c.subject == fenced.subject)
        .ok_or("fenced contract")?
        .data_schema();
    fenced.aggregate_revision = Revision::new(2)?;
    fenced.aggregate_sequence = Sequence(2);
    fenced.data.lease.revision = fenced.aggregate_revision;
    fenced.data.lease.state = ResourceLeaseState::Revoked;
    publisher
        .publish(fenced.subject.clone(), serde_json::to_vec(&fenced)?.into())
        .await?
        .await?;
    let consumer = NotificationConsumer::bind(client.clone()).await?;
    let (stop, receiver) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(consumer.run(pool.clone(), vec![86400, 3600], false, receiver));
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let stream = context.get_stream("LABWEAVER_RESOURCE_EVENTS").await?;
            let mut consumer: async_nats::jetstream::consumer::PullConsumer = stream
                .get_consumer("control-notifications-v1")
                .await
                .map_err(std::io::Error::other)?;
            if consumer.info().await?.ack_floor.stream_sequence >= 4 {
                break Ok::<(), Box<dyn Error>>(());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await??;
    stop.send_replace(true);
    tokio::time::timeout(Duration::from_secs(2), task).await???;
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM control.notifications")
        .fetch_one(&pool)
        .await?;
    assert_eq!(count, 5);
    let _restarted = NotificationConsumer::bind(client).await?;
    assert_eq!(
        notifications::unread(&pool, request.requester_id)
            .await?
            .unread_count,
        5
    );
    Ok(())
}

#[tokio::test]
async fn smtp_failure_is_visible_bounded_and_cannot_repeat_owner_operations() -> Result {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let (_pg, pool) = database().await?;
    support::apply_domain_migrations(&pool, Domain::Resource).await?;
    let store = resource_service::store::PgResourceStore::new(pool.clone());
    let actor = auth::upsert_actor(&pool, "https://oidc.fixture.invalid", "smtp-recipient").await?;
    auth::update_notification_email(
        &pool,
        actor.actor_id,
        &serde_json::json!({"email":"recipient@example.invalid","email_verified":true}),
    )
    .await?;
    let mut request = request(store.current_time().await?)?;
    request.requester_id = actor.actor_id.to_string().parse()?;
    let course_id = contracts::CourseId::new();
    request.course_id = Some(course_id);
    sqlx::query("INSERT INTO control.course_memberships(course_id,actor_id,role,state,revision) VALUES($1,$2,'student','active',1)").bind(course_id.as_uuid()).bind(actor.actor_id).execute(&pool).await?;
    member(&pool, &request).await?;
    store
        .create_task(
            "smtp-request-create",
            &request,
            60_000,
            "smtp-notification-trace",
        )
        .await?;
    let payload: serde_json::Value =
        sqlx::query_scalar("SELECT payload FROM resource.outbox_events WHERE subject=$1")
            .bind(subjects::RESOURCE_REQUEST_SUBMITTED)
            .fetch_one(&pool)
            .await?;
    let bytes = serde_json::to_vec(&payload)?;
    notifications::project(&pool, &bytes, &[86400], true).await?;
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()])?;
    let temp = tempfile::tempdir()?;
    let ca = temp.path().join("smtp-ca.pem");
    let password = temp.path().join("password");
    std::fs::write(&ca, cert.cert.pem())?;
    std::fs::write(&password, "smtp-fixture-password")?;
    let tls = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(
            vec![cert.cert.der().clone()],
            rustls::pki_types::PrivateKeyDer::Pkcs8(rustls::pki_types::PrivatePkcs8KeyDer::from(
                cert.signing_key.serialize_der(),
            )),
        )?;
    let acceptor = tokio_rustls::TlsAcceptor::from(std::sync::Arc::new(tls));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let server = tokio::spawn(async move {
        let mut attempts = 0;
        let mut body = String::new();
        for _ in 0..9 {
            let (socket, _) = listener.accept().await?;
            let tls = acceptor.accept(socket).await?;
            let mut io = BufReader::new(tls);
            io.get_mut().write_all(b"220 fixture ESMTP\r\n").await?;
            loop {
                let mut line = String::new();
                if io.read_line(&mut line).await? == 0 {
                    break;
                }
                let response = if line.starts_with("EHLO") {
                    "250-fixture\r\n250 AUTH PLAIN\r\n"
                } else if line.starts_with("AUTH") {
                    "235 authenticated\r\n"
                } else if line.starts_with("MAIL") || line.starts_with("RCPT") {
                    "250 accepted\r\n"
                } else if line.starts_with("DATA") {
                    attempts += 1;
                    match attempts {
                        1..=5 => "451 fixture delivery unavailable\r\n",
                        6 => "354 send data\r\n",
                        7 => "550 fixture permanently rejected\r\n",
                        8 => break,
                        _ => {
                            tokio::time::sleep(Duration::from_secs(12)).await;
                            break;
                        }
                    }
                } else {
                    "221 bye\r\n"
                };
                io.get_mut().write_all(response.as_bytes()).await?;
                if line.starts_with("DATA") && attempts == 6 {
                    loop {
                        let mut content = String::new();
                        if io.read_line(&mut content).await? == 0 || content == ".\r\n" {
                            break;
                        }
                        body.push_str(&content);
                    }
                    io.get_mut().write_all(b"250 queued\r\n").await?;
                }
                if line.starts_with("DATA") || line.starts_with("QUIT") {
                    break;
                }
            }
        }
        Ok::<_, std::io::Error>((attempts, body))
    });
    let worker = SmtpWorker::new(SmtpConfiguration {
        host: "localhost".into(),
        port,
        tls: "tls".into(),
        username: "fixture".into(),
        password_file: password.to_string_lossy().into_owned(),
        ca_file: ca.to_string_lossy().into_owned(),
        from: "notices@example.invalid".into(),
        portal_base_url: "https://portal.example.invalid".into(),
    })?;
    let notice_id = notifications::list(
        &pool,
        request.requester_id,
        &NotificationListQuery::default(),
    )
    .await?
    .items[0]
        .id;
    for revoked in [true, false] {
        sqlx::query("UPDATE control.course_memberships SET state=CASE WHEN $2 THEN 'revoked' ELSE 'active' END,expires_at=CASE WHEN $2 THEN NULL ELSE clock_timestamp()-interval '1 second' END WHERE course_id=$1").bind(course_id.as_uuid()).bind(revoked).execute(&pool).await?;
        assert!(
            notifications::list(
                &pool,
                request.requester_id,
                &NotificationListQuery::default()
            )
            .await?
            .items
            .is_empty()
        );
        assert_eq!(
            notifications::unread(&pool, request.requester_id)
                .await?
                .unread_count,
            0
        );
        assert!(matches!(
            notifications::mark_read(&pool, request.requester_id, notice_id).await,
            Err(NotificationError::NotFound)
        ));
        assert!(!worker.deliver_one(&pool).await?);
        let attempts: i16 = sqlx::query_scalar(
            "SELECT email_attempts FROM control.notifications WHERE notification_id=$1",
        )
        .bind(notice_id.as_uuid())
        .fetch_one(&pool)
        .await?;
        assert_eq!(attempts, 0);
        let project_active:bool=sqlx::query_scalar("SELECT state='active' FROM control.project_memberships WHERE project_id=$1 AND actor_id=$2").bind(request.project_id.as_uuid()).bind(actor.actor_id).fetch_one(&pool).await?;
        assert!(project_active);
    }
    sqlx::query(
        "UPDATE control.course_memberships SET state='active',expires_at=NULL WHERE course_id=$1",
    )
    .bind(course_id.as_uuid())
    .execute(&pool)
    .await?;
    for attempt in 1..=5 {
        notifications::project(&pool, &bytes, &[86400], true).await?;
        assert!(worker.deliver_one(&pool).await?);
        let page = notifications::list(
            &pool,
            request.requester_id,
            &NotificationListQuery::default(),
        )
        .await?;
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].email_attempts, attempt);
        assert_eq!(
            page.items[0].email_diagnostic.as_deref(),
            Some("LW_NOTIFICATION_SMTP_FAILED")
        );
        assert_eq!(
            page.items[0].email_state,
            if attempt < 5 {
                NotificationEmailState::Failed
            } else {
                NotificationEmailState::Exhausted
            }
        );
        sqlx::query(
            "UPDATE control.notifications SET email_next_at=clock_timestamp()-interval '1 second'",
        )
        .execute(&pool)
        .await?;
    }
    assert!(!worker.deliver_one(&pool).await?);
    assert_eq!(store.load(request.id).await?.revision, request.revision);
    let outbox: i64 = sqlx::query_scalar("SELECT count(*) FROM resource.outbox_events")
        .fetch_one(&pool)
        .await?;
    assert_eq!(outbox, 1);
    // Removing verified identity metadata is a terminal non-alert outcome, not an SMTP retry.
    auth::update_notification_email(
        &pool,
        actor.actor_id,
        &serde_json::json!({"email":"recipient@example.invalid","email_verified":false}),
    )
    .await?;
    sqlx::query("UPDATE control.notifications SET email_state='pending',email_attempts=0,email_next_at=clock_timestamp()-interval '1 second'").execute(&pool).await?;
    assert!(worker.deliver_one(&pool).await?);
    let notice = notifications::list(
        &pool,
        request.requester_id,
        &NotificationListQuery::default(),
    )
    .await?
    .items
    .remove(0);
    assert_eq!(notice.email_state, NotificationEmailState::NoVerifiedEmail);
    assert!(notice.email_diagnostic.is_none());
    assert!(!worker.deliver_one(&pool).await?);
    // A new verified recipient and a separate delivery attempt exercise SMTP DATA/ACK success.
    auth::update_notification_email(
        &pool,
        actor.actor_id,
        &serde_json::json!({"email":"recipient@example.invalid","email_verified":true}),
    )
    .await?;
    sqlx::query("UPDATE control.notifications SET email_state='pending',email_attempts=0,email_next_at=clock_timestamp()-interval '1 second'").execute(&pool).await?;
    assert!(worker.deliver_one(&pool).await?);
    let sent = notifications::list(
        &pool,
        request.requester_id,
        &NotificationListQuery::default(),
    )
    .await?
    .items
    .remove(0);
    assert_eq!(sent.email_state, NotificationEmailState::Sent);
    assert!(!worker.deliver_one(&pool).await?);
    for (label, expected) in [
        ("permanent_550", NotificationEmailState::Failed),
        ("disconnect", NotificationEmailState::DeliveryUnknown),
        ("timeout", NotificationEmailState::DeliveryUnknown),
    ] {
        sqlx::query("UPDATE control.notifications SET email_state='pending',email_attempts=0,email_next_at=clock_timestamp()-interval '1 second'").execute(&pool).await?;
        assert!(worker.deliver_one(&pool).await?, "{label}");
        let notice = notifications::list(
            &pool,
            request.requester_id,
            &NotificationListQuery::default(),
        )
        .await?
        .items
        .remove(0);
        assert_eq!(notice.email_state, expected, "{label}");
        assert_eq!(notice.email_attempts, 1);
        assert!(!worker.deliver_one(&pool).await?, "{label} must not resend");
    }
    sqlx::query("UPDATE control.notifications SET email_state='sending',email_attempts=1,email_next_at=clock_timestamp()-interval '1 second'").execute(&pool).await?;
    assert!(!worker.deliver_one(&pool).await?);
    let recovered = notifications::list(
        &pool,
        request.requester_id,
        &NotificationListQuery::default(),
    )
    .await?
    .items
    .remove(0);
    assert_eq!(
        recovered.email_state,
        NotificationEmailState::DeliveryUnknown
    );
    assert_eq!(recovered.email_attempts, 1);
    assert!(!worker.deliver_one(&pool).await?);
    let (attempts, body) = tokio::time::timeout(Duration::from_secs(15), server).await???;
    assert_eq!(attempts, 9);
    let (headers, encoded) = body
        .split_once("\r\n\r\n")
        .ok_or("SMTP MIME separator missing")?;
    assert!(headers.contains("Message-ID:"));
    let decoded = if headers.contains("Content-Transfer-Encoding: base64") {
        base64::engine::general_purpose::STANDARD.decode(encoded.lines().collect::<String>())?
    } else if headers.contains("Content-Transfer-Encoding: quoted-printable") {
        quoted_printable::decode(encoded.as_bytes(), quoted_printable::ParseMode::Strict)?
    } else {
        encoded.as_bytes().to_vec()
    };
    let text = String::from_utf8(decoded)?;
    assert!(text.contains("https://portal.example.invalid/researcher/resources?projectId="));
    assert!(!text.contains("smtp-fixture-password"));
    sqlx::query("UPDATE control.notifications SET email_state='pending',email_attempts=0,email_next_at=clock_timestamp()-interval '1 second'").execute(&pool).await?;
    let (_stop, receiver) = tokio::sync::watch::channel(true);
    tokio::time::timeout(Duration::from_secs(1), worker.run(pool.clone(), receiver)).await??;
    let after_stop = notifications::list(
        &pool,
        request.requester_id,
        &NotificationListQuery::default(),
    )
    .await?
    .items
    .remove(0);
    assert_eq!(after_stop.email_state, NotificationEmailState::Pending);
    assert_eq!(after_stop.email_attempts, 0);
    Ok(())
}

#[tokio::test]
async fn elapsed_deadlines_do_not_appear_as_upcoming_notifications() -> Result {
    let (_pg, pool) = database().await?;
    let now: time::OffsetDateTime =
        sqlx::query_scalar("SELECT date_trunc('milliseconds',clock_timestamp())")
            .fetch_one(&pool)
            .await?;
    let from = UtcTimestamp::from_utc(now - time::Duration::hours(2))?;
    let until = UtcTimestamp::from_utc(now - time::Duration::hours(1))?;
    let request = request(from)?;
    member(&pool, &request).await?;
    let event = lease_event(&request, from, until, subjects::RESOURCE_LEASE_ACTIVATED)?;
    notifications::project(&pool, &serde_json::to_vec(&event)?, &[86400, 3600], false).await?;
    let page = notifications::list(
        &pool,
        request.requester_id,
        &NotificationListQuery::default(),
    )
    .await?;
    assert_eq!(page.items.len(), 2);
    assert!(
        page.items
            .iter()
            .all(|notice| notice.kind != NotificationKind::LeaseExpiryReminder)
    );
    assert_eq!(
        notifications::unread(&pool, request.requester_id)
            .await?
            .unread_count,
        2
    );
    Ok(())
}

#[test]
fn bundle_derived_notification_windows_are_validated_without_enabling_email() -> Result {
    use control_service::notifications::smtp::NotificationConfiguration;
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("notifications.json");
    for windows in [
        vec![7200, 600],
        vec![],
        vec![600, 600],
        vec![0],
        vec![31_536_001],
    ] {
        std::fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({"smtp":null,"expiryReminderSeconds":windows}))?,
        )?;
        let actual = NotificationConfiguration::load(&path);
        if windows == [7200, 600] || windows.is_empty() {
            let config = actual?;
            assert!(config.smtp.is_none());
            assert_eq!(config.expiry_reminder_seconds, windows);
        } else {
            assert!(matches!(actual, Err(NotificationError::Invalid)));
        }
    }
    Ok(())
}
