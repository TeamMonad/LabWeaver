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
    let shutdown = control_service::service_runtime::subscribe_shutdown();
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
        assert!(!worker.deliver_one(&pool, &shutdown).await?);
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
        assert!(worker.deliver_one(&pool, &shutdown).await?);
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
    assert!(!worker.deliver_one(&pool, &shutdown).await?);
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
    assert!(worker.deliver_one(&pool, &shutdown).await?);
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
    assert!(!worker.deliver_one(&pool, &shutdown).await?);
    // A new verified recipient and a separate delivery attempt exercise SMTP DATA/ACK success.
    auth::update_notification_email(
        &pool,
        actor.actor_id,
        &serde_json::json!({"email":"recipient@example.invalid","email_verified":true}),
    )
    .await?;
    sqlx::query("UPDATE control.notifications SET email_state='pending',email_attempts=0,email_next_at=clock_timestamp()-interval '1 second'").execute(&pool).await?;
    assert!(worker.deliver_one(&pool, &shutdown).await?);
    let sent = notifications::list(
        &pool,
        request.requester_id,
        &NotificationListQuery::default(),
    )
    .await?
    .items
    .remove(0);
    assert_eq!(sent.email_state, NotificationEmailState::Sent);
    assert!(!worker.deliver_one(&pool, &shutdown).await?);
    for (label, expected) in [
        ("permanent_550", NotificationEmailState::Failed),
        ("disconnect", NotificationEmailState::DeliveryUnknown),
        ("timeout", NotificationEmailState::DeliveryUnknown),
    ] {
        sqlx::query("UPDATE control.notifications SET email_state='pending',email_attempts=0,email_next_at=clock_timestamp()-interval '1 second'").execute(&pool).await?;
        assert!(worker.deliver_one(&pool, &shutdown).await?, "{label}");
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
        assert!(
            !worker.deliver_one(&pool, &shutdown).await?,
            "{label} must not resend"
        );
    }
    sqlx::query("UPDATE control.notifications SET email_state='sending',email_attempts=1,email_next_at=clock_timestamp()-interval '1 second'").execute(&pool).await?;
    assert!(!worker.deliver_one(&pool, &shutdown).await?);
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
    assert!(!worker.deliver_one(&pool, &shutdown).await?);
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

struct NotificationNats {
    _server: testcontainers::ContainerAsync<GenericImage>,
    endpoint: String,
    publisher: async_nats::jetstream::Context,
    operator: async_nats::jetstream::Context,
}

impl NotificationNats {
    async fn new() -> Result<Self> {
        let config = std::process::Command::new("python3")
            .arg(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../tests/fixtures/notification_nats.py"),
            )
            .output()?;
        assert!(config.status.success(), "NATS permission fixture failed");
        let server = GenericImage::new("nats", "2.11.8-alpine")
            .with_exposed_port(4222.tcp())
            .with_wait_for(WaitFor::message_on_stderr("Server is ready"))
            .with_copy_to("/etc/nats/notifications.json", config.stdout)
            .with_cmd(["-c", "/etc/nats/notifications.json"])
            .start()
            .await?;
        let endpoint = format!(
            "nats://127.0.0.1:{}",
            server.get_host_port_ipv4(4222).await?
        );
        let operator = async_nats::ConnectOptions::new()
            .user_and_password("operator".into(), "operator-fixture".into())
            .connect(&endpoint)
            .await?;
        let publisher = async_nats::ConnectOptions::new()
            .user_and_password("resource-service".into(), "identity-fixture".into())
            .connect(&endpoint)
            .await?;
        let operator = async_nats::jetstream::new(operator);
        operator
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
                ack_wait: Duration::from_secs(5),
                ack_policy: async_nats::jetstream::consumer::AckPolicy::Explicit,
                ..Default::default()
            })
            .await?;
        Ok(Self {
            _server: server,
            endpoint,
            publisher: async_nats::jetstream::new(publisher),
            operator,
        })
    }
    async fn publish(&self, bytes: Vec<u8>) -> Result<EventId> {
        let event: CloudEvent<serde_json::Value> = serde_json::from_slice(&bytes)?;
        self.publisher
            .publish(event.subject, bytes.into())
            .await?
            .await?;
        Ok(event.id)
    }
}

async fn owner_notification_event(
    pool: &PgPool,
    request: &ResourceRequest,
    key: &str,
) -> Result<Vec<u8>> {
    let store = resource_service::store::PgResourceStore::new(pool.clone());
    member(pool, request).await?;
    store
        .create_task(key, request, 60_000, "notification-process-test")
        .await?;
    let payload: serde_json::Value = sqlx::query_scalar(
        "SELECT payload FROM resource.outbox_events WHERE aggregate_id=$1 AND subject=$2",
    )
    .bind(request.id.as_uuid())
    .bind(subjects::RESOURCE_REQUEST_SUBMITTED)
    .fetch_one(pool)
    .await?;
    Ok(serde_json::to_vec(&payload)?)
}

#[tokio::test]
#[ignore = "real process entrypoint launched by the notification shutdown regressions"]
async fn notification_worker_process() -> Result {
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&std::env::var("LW_NOTIFICATION_TEST_DATABASE")?)
        .await?;
    let client = async_nats::ConnectOptions::new()
        .user_and_password("control-service".into(), "identity-fixture".into())
        .connect(std::env::var("LW_NOTIFICATION_TEST_NATS")?)
        .await?;
    let consumer = NotificationConsumer::bind(client).await?;
    let smtp = std::env::var("LW_NOTIFICATION_TEST_SMTP")
        .ok()
        .map(|config| serde_json::from_str::<SmtpConfiguration>(&config))
        .transpose()?
        .map(SmtpWorker::new)
        .transpose()?;
    let email = smtp.is_some();
    let shutdown = control_service::service_runtime::subscribe_shutdown();
    let smtp_shutdown = shutdown.clone();
    let mut observers = tokio::task::JoinSet::new();
    observers.spawn(async {
        control_service::service_runtime::wait_for_shutdown().await;
        println!("NOTIFICATIONS_STOPPING");
    });
    println!("NOTIFICATIONS_READY");
    Box::pin(control_service::service_runtime::supervise(
        "control-notifications-test",
        async {
            tokio::try_join!(
                consumer.run(pool.clone(), vec![86400], email, shutdown),
                async {
                    if let Some(worker) = smtp {
                        worker.run(pool, smtp_shutdown).await?;
                    }
                    Ok::<(), NotificationError>(())
                }
            )?;
            Ok::<(), NotificationError>(())
        },
    ))
    .await?;
    Ok(())
}

fn spawn_notification_process(
    database: &str,
    nats: &str,
    smtp: Option<&serde_json::Value>,
) -> Result<tokio::process::Child> {
    let mut process = tokio::process::Command::new(std::env::current_exe()?);
    process
        .args([
            "--exact",
            "notification_worker_process",
            "--ignored",
            "--nocapture",
        ])
        .env("LW_NOTIFICATION_TEST_DATABASE", database)
        .env("LW_NOTIFICATION_TEST_NATS", nats)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .kill_on_drop(true);
    if let Some(config) = smtp {
        process.env("LW_NOTIFICATION_TEST_SMTP", serde_json::to_string(config)?);
    }
    Ok(process.spawn()?)
}

async fn notification_process_marker(
    output: &mut tokio::io::Lines<tokio::io::BufReader<tokio::process::ChildStdout>>,
    marker: &str,
) -> Result {
    tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(line) = output.next_line().await? {
            if line.contains(marker) {
                return Ok::<(), std::io::Error>(());
            }
        }
        Err(std::io::Error::other(format!(
            "notification process ended before {marker}"
        )))
    })
    .await
    .map_err(|error| format!("notification worker did not emit {marker}: {error}"))?
    .map_err(|error| format!("read notification worker marker {marker}: {error}"))?;
    Ok(())
}

async fn notification_process_signal(process: &tokio::process::Child, signal: &str) -> Result {
    assert!(
        tokio::process::Command::new("kill")
            .args([
                signal,
                &process
                    .id()
                    .ok_or("notification process exited early")?
                    .to_string()
            ])
            .status()
            .await?
            .success()
    );
    Ok(())
}

async fn wait_notification_event(pool: &PgPool, id: EventId) -> Result {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let stored: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM control.notification_inbox WHERE event_id=$1)",
            )
            .bind(id.as_uuid())
            .fetch_one(pool)
            .await?;
            if stored {
                return Ok::<(), sqlx::Error>(());
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .map_err(|error| {
        format!("notification event {id} was not projected after restart: {error}")
    })??;
    Ok(())
}

#[tokio::test]
async fn notification_process_sigterm_drains_owned_event_and_restart_takes_unprocessed_event()
-> Result {
    use tokio::io::AsyncBufReadExt as _;
    let (postgres, pool) = database().await?;
    support::apply_domain_migrations(&pool, Domain::Resource).await?;
    let nats = NotificationNats::new().await?;
    let database_url = format!(
        "postgres://postgres:postgres@127.0.0.1:{}/postgres",
        postgres.get_host_port_ipv4(5432).await?
    );
    let now = resource_service::store::PgResourceStore::new(pool.clone())
        .current_time()
        .await?;
    let first = request(now)?;
    let first_bytes = owner_notification_event(&pool, &first, "notification-process-first").await?;
    let second = request(now)?;
    let second_bytes =
        owner_notification_event(&pool, &second, "notification-process-second").await?;
    let mut process = spawn_notification_process(&database_url, &nats.endpoint, None)?;
    let mut output =
        tokio::io::BufReader::new(process.stdout.take().ok_or("worker stdout missing")?).lines();
    notification_process_marker(&mut output, "NOTIFICATIONS_READY").await?;
    let mut gate = pool.begin().await?;
    sqlx::query("LOCK TABLE control.notifications IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *gate)
        .await?;
    let first_id = nats.publish(first_bytes.clone()).await?;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let blocked: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE wait_event_type='Lock' AND query LIKE '%control.notifications%' AND pid<>pg_backend_pid())").fetch_one(&pool).await?;
            if blocked { return Ok::<(), sqlx::Error>(()); }
            tokio::task::yield_now().await;
        }
    }).await??;
    notification_process_signal(&process, "-TERM").await?;
    notification_process_marker(&mut output, "NOTIFICATIONS_STOPPING").await?;
    let second_id = nats.publish(second_bytes).await?;
    gate.rollback().await?;
    assert!(
        tokio::time::timeout(Duration::from_secs(25), process.wait())
            .await??
            .success()
    );
    wait_notification_event(&pool, first_id).await?;
    let second_stored: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM control.notification_inbox WHERE event_id=$1)",
    )
    .bind(second_id.as_uuid())
    .fetch_one(&pool)
    .await?;
    assert!(
        !second_stored,
        "SIGTERM must stop notification acquisition after the owned event drains"
    );
    let mut restarted = spawn_notification_process(&database_url, &nats.endpoint, None)?;
    wait_notification_event(&pool, second_id).await?;
    nats.publish(first_bytes).await?;
    let stream = nats
        .operator
        .get_stream("LABWEAVER_RESOURCE_EVENTS")
        .await?;
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let mut consumer: async_nats::jetstream::consumer::PullConsumer = stream
                .get_consumer("control-notifications-v1")
                .await
                .map_err(std::io::Error::other)?;
            if consumer.info().await?.ack_floor.stream_sequence >= 3 {
                return Ok::<(), Box<dyn Error>>(());
            }
            tokio::task::yield_now().await;
        }
    })
    .await??;
    notification_process_signal(&restarted, "-TERM").await?;
    assert!(
        tokio::time::timeout(Duration::from_secs(25), restarted.wait())
            .await??
            .success()
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM control.notifications")
            .fetch_one(&pool)
            .await?,
        2
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM resource.outbox_events")
            .fetch_one(&pool)
            .await?,
        2
    );
    Ok(())
}

struct SmtpBoundary {
    config: serde_json::Value,
    deliveries: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    held: tokio::sync::mpsc::Receiver<tokio::sync::oneshot::Sender<()>>,
    _tasks: tokio::task::JoinSet<std::io::Result<()>>,
    _secrets: tempfile::TempDir,
}

impl SmtpBoundary {
    async fn new() -> Result<Self> {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()])?;
        let secrets = tempfile::tempdir()?;
        let ca = secrets.path().join("smtp-ca.pem");
        let password = secrets.path().join("password");
        std::fs::write(&ca, cert.cert.pem())?;
        std::fs::write(&password, "smtp-fixture-password")?;
        let tls = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(
                vec![cert.cert.der().clone()],
                rustls::pki_types::PrivateKeyDer::Pkcs8(
                    rustls::pki_types::PrivatePkcs8KeyDer::from(cert.signing_key.serialize_der()),
                ),
            )?;
        let acceptor = tokio_rustls::TlsAcceptor::from(std::sync::Arc::new(tls));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
        let deliveries = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed = deliveries.clone();
        let (sender, held) = tokio::sync::mpsc::channel(2);
        let mut tasks = tokio::task::JoinSet::new();
        tasks.spawn(async move {
            for _ in 0..2 {
                let (socket, _) = listener.accept().await?;
                let tls = acceptor.accept(socket).await?;
                let mut io = BufReader::new(tls);
                io.get_mut().write_all(b"220 fixture ESMTP\r\n").await?;
                loop {
                    let mut line = String::new();
                    if io.read_line(&mut line).await? == 0 {
                        break;
                    }
                    if line.starts_with("DATA") {
                        io.get_mut().write_all(b"354 send data\r\n").await?;
                        loop {
                            let mut content = String::new();
                            if io.read_line(&mut content).await? == 0 || content == ".\r\n" {
                                break;
                            }
                        }
                        observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        let (release, wait) = tokio::sync::oneshot::channel();
                        sender.send(release).await.map_err(std::io::Error::other)?;
                        if wait.await.is_ok() {
                            let _ = io.get_mut().write_all(b"250 queued\r\n").await;
                        }
                        break;
                    }
                    let response = if line.starts_with("EHLO") {
                        "250-fixture\r\n250 AUTH PLAIN\r\n"
                    } else if line.starts_with("AUTH") {
                        "235 authenticated\r\n"
                    } else if line.starts_with("MAIL") || line.starts_with("RCPT") {
                        "250 accepted\r\n"
                    } else {
                        "221 bye\r\n"
                    };
                    io.get_mut().write_all(response.as_bytes()).await?;
                }
            }
            Ok(())
        });
        let config = serde_json::json!({"host":"localhost","port":port,"tls":"tls","username":"fixture","passwordFile":password,"caFile":ca,"from":"notices@example.invalid","portalBaseUrl":"https://portal.example.invalid"});
        Ok(Self {
            config,
            deliveries,
            held,
            _tasks: tasks,
            _secrets: secrets,
        })
    }
    async fn accepted(&mut self) -> Result<tokio::sync::oneshot::Sender<()>> {
        Ok(
            tokio::time::timeout(Duration::from_secs(10), self.held.recv())
                .await
                .map_err(|error| {
                    format!("SMTP DATA was not accepted before fixture deadline: {error}")
                })?
                .ok_or("SMTP fixture ended before accepting DATA")?,
        )
    }
}

async fn wait_notification_email(pool: &PgPool, id: EventId, expected: &str) -> Result {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let state: String = sqlx::query_scalar(
                "SELECT email_state FROM control.notifications WHERE notification_id=$1",
            )
            .bind(id.as_uuid())
            .fetch_one(pool)
            .await?;
            if state == expected {
                return Ok::<(), sqlx::Error>(());
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .map_err(|error| format!("notification {id} did not reach SMTP state {expected}: {error}"))??;
    Ok(())
}

#[tokio::test]
async fn smtp_process_sigterm_drains_one_send_and_crashed_sending_becomes_unknown_without_resend()
-> Result {
    use tokio::io::AsyncBufReadExt as _;
    let (postgres, pool) = database().await?;
    support::apply_domain_migrations(&pool, Domain::Resource).await?;
    let nats = NotificationNats::new().await?;
    let mut smtp = SmtpBoundary::new().await?;
    let database_url = format!(
        "postgres://postgres:postgres@127.0.0.1:{}/postgres",
        postgres.get_host_port_ipv4(5432).await?
    );
    let actor = auth::upsert_actor(
        &pool,
        "https://oidc.fixture.invalid",
        "smtp-process-recipient",
    )
    .await?;
    auth::update_notification_email(
        &pool,
        actor.actor_id,
        &serde_json::json!({"email":"recipient@example.invalid","email_verified":true}),
    )
    .await?;
    let store = resource_service::store::PgResourceStore::new(pool.clone());
    let now = store.current_time().await?;
    let mut first = request(now)?;
    first.requester_id = actor.actor_id.to_string().parse()?;
    let first_bytes = owner_notification_event(&pool, &first, "smtp-process-first").await?;
    let mut second = request(now)?;
    second.requester_id = first.requester_id;
    let second_bytes = owner_notification_event(&pool, &second, "smtp-process-second").await?;
    let mut process =
        spawn_notification_process(&database_url, &nats.endpoint, Some(&smtp.config))?;
    let mut output =
        tokio::io::BufReader::new(process.stdout.take().ok_or("worker stdout missing")?).lines();
    notification_process_marker(&mut output, "NOTIFICATIONS_READY").await?;
    nats.publish(first_bytes).await?;
    let first_ack = smtp.accepted().await?;
    notification_process_signal(&process, "-TERM").await?;
    notification_process_marker(&mut output, "NOTIFICATIONS_STOPPING").await?;
    notifications::project(&pool, &second_bytes, &[86400], true).await?;
    let second_notice = notifications::list(
        &pool,
        second.requester_id,
        &NotificationListQuery::default(),
    )
    .await?
    .items
    .into_iter()
    .find(|notice| notice.project_id == second.project_id)
    .ok_or("second notification missing")?;
    first_ack
        .send(())
        .map_err(|()| "SMTP DATA ACK gate closed")?;
    assert!(
        tokio::time::timeout(Duration::from_secs(25), process.wait())
            .await??
            .success()
    );
    let first_notice =
        notifications::list(&pool, first.requester_id, &NotificationListQuery::default())
            .await?
            .items
            .into_iter()
            .find(|notice| notice.project_id == first.project_id)
            .ok_or("first notification missing")?;
    assert_eq!(first_notice.email_state, NotificationEmailState::Sent);
    assert_eq!(first_notice.email_attempts, 1);
    assert_eq!(
        sqlx::query_scalar::<_, i16>(
            "SELECT email_attempts FROM control.notifications WHERE notification_id=$1"
        )
        .bind(second_notice.id.as_uuid())
        .fetch_one(&pool)
        .await?,
        0,
        "SIGTERM must not claim mail queued while an accepted send drains"
    );
    let mut crashing =
        spawn_notification_process(&database_url, &nats.endpoint, Some(&smtp.config))?;
    let lost_ack = smtp.accepted().await?;
    wait_notification_email(&pool, second_notice.id, "sending").await?;
    notification_process_signal(&crashing, "-KILL").await?;
    assert!(
        !tokio::time::timeout(Duration::from_secs(10), crashing.wait())
            .await??
            .success()
    );
    drop(lost_ack);
    sqlx::query("UPDATE control.notifications SET email_next_at=clock_timestamp()-interval '1 millisecond' WHERE notification_id=$1 AND email_state='sending'").bind(second_notice.id.as_uuid()).execute(&pool).await?;
    let mut restarted =
        spawn_notification_process(&database_url, &nats.endpoint, Some(&smtp.config))?;
    wait_notification_email(&pool, second_notice.id, "delivery_unknown").await?;
    notification_process_signal(&restarted, "-TERM").await?;
    assert!(
        tokio::time::timeout(Duration::from_secs(25), restarted.wait())
            .await??
            .success()
    );
    assert_eq!(
        smtp.deliveries.load(std::sync::atomic::Ordering::SeqCst),
        2,
        "uncertain SMTP acceptance must never be resent after lease expiry"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i16>(
            "SELECT email_attempts FROM control.notifications WHERE notification_id=$1"
        )
        .bind(second_notice.id.as_uuid())
        .fetch_one(&pool)
        .await?,
        1
    );
    assert_eq!(store.load(first.id).await?.revision, first.revision);
    assert_eq!(store.load(second.id).await?.revision, second.revision);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM resource.outbox_events")
            .fetch_one(&pool)
            .await?,
        2
    );
    Ok(())
}

#[tokio::test]
async fn smtp_stops_before_new_claim_after_expiry_sweep_io() -> Result {
    let (_postgres, pool) = database().await?;
    support::apply_domain_migrations(&pool, Domain::Resource).await?;
    let smtp = SmtpBoundary::new().await?;
    let store = resource_service::store::PgResourceStore::new(pool.clone());
    let request = request(store.current_time().await?)?;
    let bytes = owner_notification_event(&pool, &request, "smtp-stop-claim-boundary").await?;
    notifications::project(&pool, &bytes, &[86400], true).await?;
    let mut gate = pool.begin().await?;
    sqlx::query("LOCK TABLE control.notifications IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *gate)
        .await?;
    let worker = SmtpWorker::new(serde_json::from_value(smtp.config.clone())?)?;
    let (stop, shutdown) = tokio::sync::watch::channel(false);
    let mut workers = tokio::task::JoinSet::new();
    workers.spawn(worker.run(pool.clone(), shutdown));
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let blocked: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE wait_event_type='Lock' AND query LIKE 'UPDATE control.notifications SET email_state=%delivery_unknown%' AND pid<>pg_backend_pid())").fetch_one(&pool).await?;
            if blocked { return Ok::<(), sqlx::Error>(()); }
            tokio::task::yield_now().await;
        }
    }).await??;
    stop.send_replace(true);
    gate.rollback().await?;
    tokio::time::timeout(Duration::from_secs(2), workers.join_next())
        .await?
        .ok_or("SMTP worker missing")???;
    assert_eq!(
        sqlx::query_scalar::<_, i16>("SELECT email_attempts FROM control.notifications")
            .fetch_one(&pool)
            .await?,
        0,
        "shutdown during expiry sweep must be checked before the following claim statement"
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT email_state FROM control.notifications")
            .fetch_one(&pool)
            .await?,
        "pending"
    );
    Ok(())
}
