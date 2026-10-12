//! Real Resource HTTP/SQL lifecycle; faults only delay or lose a committed reply.

use std::{
    io::Cursor,
    sync::atomic::{AtomicBool, Ordering},
};

use super::*;
use auth::{ServiceTokenClient, ServiceTokenClientConfig};
use axum::{
    extract::Request,
    middleware::Next,
    response::{IntoResponse, Response},
};
use contracts::http::TaskResourceStatus;
use contracts::resource::{CapacityClaimState, ResourceLeaseState};
use hyper_util::{
    rt::{TokioExecutor, TokioIo},
    server::conn::auto::Builder,
    service::TowerToHyperService,
};
use reqwest::Url;
use rustls::{ServerConfig, pki_types::PrivateKeyDer};
use task_execution::resource::{
    ResourceClient, ResourceClientConfiguration, TaskResourceError, TaskResourceLifecycle,
};
use tokio_rustls::TlsAcceptor;
use tokio_util::sync::CancellationToken;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

#[derive(Clone)]
struct Faults {
    store: PgResourceStore,
    approve_after_review_read: Arc<AtomicBool>,
    lose_ack_reply: Arc<AtomicBool>,
    lose_release_reply: Arc<AtomicBool>,
    cancel_after_ack: Arc<AtomicBool>,
    cancel_after_claim: Arc<AtomicBool>,
    lose_request_reply: Arc<AtomicBool>,
    cancellation: CancellationToken,
}

async fn inject_fault(State(faults): State<Faults>, request: Request, next: Next) -> Response {
    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    let response = next.run(request).await;
    if !response.status().is_success() {
        return response;
    }
    if method == Method::GET
        && path.ends_with("/request")
        && faults.lose_request_reply.swap(false, Ordering::SeqCst)
    {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    if method == Method::GET
        && path.ends_with("/request")
        && faults
            .approve_after_review_read
            .swap(false, Ordering::SeqCst)
    {
        let result = async {
            let id: TaskRunId = path.split('/').nth(4).ok_or("missing task id")?.parse()?;
            let reviewing = faults.store.load_task_request(id).await?;
            approve(&faults.store, &reviewing).await?;
            // The response already contains Reviewing. Deliver it after the
            // client's deadline, while SQL now contains Allocating.
            tokio::time::sleep(Duration::from_millis(25)).await;
            TestResult::Ok(())
        }
        .await;
        if let Err(error) = result {
            return (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response();
        }
    }
    if method == Method::POST && path.ends_with("/ack") {
        if faults.cancel_after_ack.swap(false, Ordering::SeqCst) {
            faults.cancellation.cancel();
        }
        if faults.lose_ack_reply.swap(false, Ordering::SeqCst) {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
    }
    if method == Method::POST
        && path.ends_with("/claim")
        && faults.cancel_after_claim.swap(false, Ordering::SeqCst)
    {
        faults.cancellation.cancel();
    }
    if method == Method::POST
        && path.ends_with("/release")
        && faults.lose_release_reply.swap(false, Ordering::SeqCst)
    {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    response
}

struct Fixture {
    _postgres: testcontainers::ContainerAsync<Postgres>,
    _authority: ResourceAuthAuthority,
    _temporary: tempfile::TempDir,
    server: tokio::task::JoinHandle<()>,
    pool: sqlx::PgPool,
    faults: Faults,
    lifecycle: TaskResourceLifecycle,
    task_run_id: TaskRunId,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

impl Fixture {
    async fn start() -> TestResult<Self> {
        Self::start_for_client(RESOURCE_EVALUATION_CLIENT_ID).await
    }

    async fn start_for_client(task_client_id: &str) -> TestResult<Self> {
        let (postgres, pool) = migrated_pool().await?;
        let store = PgResourceStore::new(pool.clone());
        let authority = spawn_resource_auth_authority_for_client(
            resource_signing_material("task-lifecycle")?,
            task_client_id,
        )
        .await?;
        let verifier = build_resource_verifier(&authority.issuer).await?;
        let faults = Faults {
            store: store.clone(),
            approve_after_review_read: Arc::new(AtomicBool::new(false)),
            lose_ack_reply: Arc::new(AtomicBool::new(false)),
            lose_release_reply: Arc::new(AtomicBool::new(false)),
            cancel_after_ack: Arc::new(AtomicBool::new(false)),
            cancel_after_claim: Arc::new(AtomicBool::new(false)),
            lose_request_reply: Arc::new(AtomicBool::new(false)),
            cancellation: CancellationToken::new(),
        };
        let state = resource_service::api::ResourceApiState::new(store)
            .with_service_verifier(Arc::new(verifier))
            .with_task_service_client_ids(BTreeSet::from([task_client_id.to_owned()]));
        let router = resource_service::api::resource_api_router(state).layer(
            axum::middleware::from_fn_with_state(faults.clone(), inject_fault),
        );
        let certificate = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()])?;
        let pem = certificate.cert.pem();
        let key_pem = certificate.signing_key.serialize_pem();
        let certs = rustls_pemfile::certs(&mut Cursor::new(pem.as_bytes()))
            .collect::<Result<Vec<_>, _>>()?;
        let key: PrivateKeyDer<'static> =
            rustls_pemfile::private_key(&mut Cursor::new(key_pem.as_bytes()))?
                .ok_or("TLS key missing")?;
        let tls = Arc::new(
            ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(certs, key)?,
        );
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let base_uri = Url::parse(&format!(
            "https://localhost:{}/",
            listener.local_addr()?.port()
        ))?;
        let server = tokio::spawn(async move {
            let acceptor = TlsAcceptor::from(tls);
            while let Ok((stream, _)) = listener.accept().await {
                let acceptor = acceptor.clone();
                let router = router.clone();
                tokio::spawn(async move {
                    let Ok(stream) = acceptor.accept(stream).await else {
                        return;
                    };
                    let connection = Builder::new(TokioExecutor::new())
                        .serve_connection_with_upgrades(
                            TokioIo::new(stream),
                            TowerToHyperService::new(router),
                        )
                        .into_owned();
                    let _ = connection.await;
                });
            }
        });
        let temporary = tempfile::tempdir()?;
        let ca_file = temporary.path().join("resource-ca.pem");
        std::fs::write(&ca_file, pem)?;
        let scopes: BTreeSet<_> = task_execution::resource::required_scopes_for_diagnostics()
            .iter()
            .map(|value| (*value).to_owned())
            .collect();
        let tokens = ServiceTokenClient::discover(
            ServiceTokenClientConfig::new(
                &authority.issuer,
                task_client_id.to_owned(),
                "local-test-secret".to_owned(),
                RESOURCE_AUTH_AUDIENCE.to_owned(),
                scopes.clone(),
                1,
                TransportSecurityMode::InsecureTestOnly,
            )?,
            reqwest::Client::builder().no_proxy().build()?,
        )
        .await?;
        let client = ResourceClient::from_configuration(
            ResourceClientConfiguration {
                base_uri,
                ca_file,
                timeout_milliseconds: 5_000,
                max_request_bytes: 1024 * 1024,
                max_response_bytes: 1024 * 1024,
                audience: RESOURCE_AUTH_AUDIENCE.to_owned(),
            },
            Arc::new(tokens),
            scopes,
        )?;
        let task_run_id = TaskRunId::new();
        let lifecycle = TaskResourceLifecycle::new(
            client,
            task_run_id,
            ProjectId::new(),
            None,
            ActorId::new(),
            format!("evaluation-{task_run_id}"),
            WorkloadResources {
                cpu_millicores: 250,
                memory_bytes: 256 * 1024 * 1024,
                storage_bytes: 512 * 1024 * 1024,
                gpu: None,
            },
            300,
        )?;
        Ok(Self {
            _postgres: postgres,
            _authority: authority,
            _temporary: temporary,
            server,
            pool,
            faults,
            lifecycle,
            task_run_id,
        })
    }

    async fn wait_for_claim(&self) -> TestResult<TaskResourceStatus> {
        Ok(self
            .lifecycle
            .claim_after_approval(Duration::from_millis(1), &self.faults.cancellation)
            .await?)
    }

    async fn event_count(&self) -> TestResult<i64> {
        Ok(
            sqlx::query_scalar("SELECT count(*) FROM resource.outbox_events")
                .fetch_one(&self.pool)
                .await?,
        )
    }

    async fn expire_wait(&self) -> TestResult {
        sqlx::query("UPDATE resource.resource_requests SET approval_deadline_at=created_at+interval '1 millisecond' WHERE task_run_id=$1")
            .bind(self.task_run_id.as_uuid()).execute(&self.pool).await?;
        Ok(())
    }
}

#[tokio::test]
async fn restart_and_changed_configuration_keep_first_deadline_and_lost_timeout_cause() -> TestResult
{
    let fixture = Fixture::start().await?;
    let request = fixture.lifecycle.create(Duration::from_secs(30)).await?;
    let deadline: time::OffsetDateTime = sqlx::query_scalar(
        "SELECT approval_deadline_at FROM resource.resource_requests WHERE request_id=$1",
    )
    .bind(request.id.as_uuid())
    .fetch_one(&fixture.pool)
    .await?;
    let recovered = fixture.lifecycle.clone();
    recovered.create(Duration::from_hours(1)).await?;
    let replayed_deadline: time::OffsetDateTime = sqlx::query_scalar(
        "SELECT approval_deadline_at FROM resource.resource_requests WHERE request_id=$1",
    )
    .bind(request.id.as_uuid())
    .fetch_one(&fixture.pool)
    .await?;
    assert_eq!(
        deadline, replayed_deadline,
        "recovery/configuration changes cannot extend the first wait"
    );
    fixture.expire_wait().await?;
    fixture
        .faults
        .lose_request_reply
        .store(true, Ordering::SeqCst);
    assert!(
        fixture.wait_for_claim().await.is_err(),
        "the first timeout reply is lost after its commit"
    );
    let restarted_store = PgResourceStore::new(fixture.pool.clone());
    let timeout = restarted_store
        .load_task_request(fixture.task_run_id)
        .await?;
    assert_eq!(timeout.state, ResourceRequestState::Cancelled);
    assert_eq!(
        timeout.diagnostic_code.as_deref(),
        Some("LW_TASK_RESOURCE_APPROVAL_TIMEOUT")
    );
    let events = fixture.event_count().await?;
    recovered.create(Duration::from_secs(30)).await?;
    assert!(matches!(
        recovered
            .claim_after_approval(Duration::from_millis(1), &CancellationToken::new())
            .await,
        Err(TaskResourceError::ResourceApprovalTimeout)
    ));
    assert_eq!(fixture.event_count().await?, events);
    assert!(!restarted_store.expire_pending_task_once().await?);
    Ok(())
}

#[tokio::test]
async fn expired_approval_releases_before_handoff_and_cannot_be_revived() -> TestResult {
    for claim_first in [false, true] {
        let fixture = Fixture::start().await?;
        let request = fixture.lifecycle.create(Duration::from_secs(30)).await?;
        approve(&fixture.faults.store, &request).await?;
        let claimed = if claim_first {
            Some(fixture.wait_for_claim().await?)
        } else {
            None
        };
        sqlx::query("UPDATE resource.resource_approvals SET valid_until=created_at+interval '1 millisecond' WHERE request_id=$1")
            .bind(request.id.as_uuid()).execute(&fixture.pool).await?;
        if let Some(claimed) = claimed {
            assert!(matches!(
                fixture
                    .lifecycle
                    .acknowledge(&claimed, "labweaver-evaluation")
                    .await,
                Err(TaskResourceError::ResourceApprovalExpired)
            ));
        } else {
            assert!(fixture.faults.store.expire_pending_task_once().await?);
        }
        let released = fixture.lifecycle.load_status().await?;
        assert!(released.cleanup_confirmed);
        assert_eq!(
            released.request.diagnostic_code.as_deref(),
            Some("LW_RESOURCE_APPROVAL_EXPIRED")
        );
        let events = fixture.event_count().await?;
        sqlx::query("UPDATE resource.resource_approvals SET valid_until=clock_timestamp()+interval '1 hour' WHERE request_id=$1")
            .bind(request.id.as_uuid()).execute(&fixture.pool).await?;
        assert!(matches!(
            fixture
                .lifecycle
                .claim_after_approval(Duration::from_millis(1), &CancellationToken::new())
                .await,
            Err(TaskResourceError::ResourceApprovalExpired)
        ));
        fixture
            .lifecycle
            .cancel("cancel after expired grant cleanup")
            .await?;
        assert_eq!(fixture.event_count().await?, events);
    }
    Ok(())
}

#[tokio::test]
async fn background_expiry_cancels_pending_but_keeps_approval_that_already_won() -> TestResult {
    let fixture = Fixture::start().await?;
    fixture.lifecycle.create(Duration::from_secs(30)).await?;
    fixture.expire_wait().await?;
    let restarted_store = PgResourceStore::new(fixture.pool.clone());
    assert!(restarted_store.expire_pending_task_once().await?);
    assert_eq!(
        restarted_store
            .load_task_request(fixture.task_run_id)
            .await?
            .state,
        ResourceRequestState::Cancelled
    );
    assert!(!restarted_store.expire_pending_task_once().await?);

    let approved = Fixture::start().await?;
    let request = approved.lifecycle.create(Duration::from_secs(30)).await?;
    approve(&approved.faults.store, &request).await?;
    approved.expire_wait().await?;
    assert!(!approved.faults.store.expire_pending_task_once().await?);
    let claimed = approved.wait_for_claim().await?;
    let active = approved
        .lifecycle
        .acknowledge(&claimed, "labweaver-evaluation")
        .await?;
    assert!(approved.lifecycle.release(&active).await?.cleanup_confirmed);
    Ok(())
}

async fn approve(
    store: &PgResourceStore,
    request: &ResourceRequest,
) -> TestResult<ResourceRequest> {
    let now = store.current_time().await?;
    let approval = ResourceApproval {
        runtime_authorization: None,
        id: ResourceApprovalId::new(),
        request_id: request.id,
        request_revision: request.revision,
        approver_id: ActorId::new(),
        provider_binding: "kubernetes-standard".to_owned(),
        approved_resources: request.requested_resources.clone(),
        approved_duration_seconds: 300,
        reason: "local lifecycle test approval".to_owned(),
        valid_until: UtcTimestamp::from_utc(now.get() + time::Duration::hours(1))?,
        created_at: now,
    };
    let allocation = PendingAllocation {
        claim: CapacityClaim {
            id: CapacityClaimId::new(),
            request_id: request.id,
            approval_id: approval.id,
            provider_binding: approval.provider_binding.clone(),
            workload_resources: request.requested_resources.clone(),
            quota_resources: request.requested_resources.clone(),
            gpu_allocation: None,
            state: CapacityClaimState::Reserved,
            revision: Revision::new(1)?,
        },
        lease_id: LeaseId::new(),
    };
    Ok(store
        .approve(
            &format!("approve-{}", request.id),
            request.id,
            &approval,
            &allocation,
            ApprovalPolicy {
                min_duration_seconds: 60,
                max_duration_seconds: 3600,
            },
            "trace-task-test",
        )
        .await?)
}

#[tokio::test]
async fn approval_committed_after_stale_review_read_continues_same_task() -> TestResult {
    for client_id in [RESOURCE_EVALUATION_CLIENT_ID, RESOURCE_AGENT_CLIENT_ID] {
        let fixture = Fixture::start_for_client(client_id).await?;
        let request = fixture.lifecycle.create(Duration::from_secs(30)).await?;
        fixture
            .faults
            .approve_after_review_read
            .store(true, Ordering::SeqCst);
        let claimed = fixture.wait_for_claim().await?;
        assert_eq!(claimed.request.id, request.id);
        assert_eq!(claimed.task_run_id, fixture.task_run_id);
        assert_eq!(claimed.claim.state, CapacityClaimState::Provisioning);
        assert!(!claimed.cleanup_confirmed);
        assert_eq!(
            fixture
                .faults
                .store
                .load_task_request(fixture.task_run_id)
                .await?
                .state,
            ResourceRequestState::Allocating
        );
        let active = fixture
            .lifecycle
            .acknowledge(&claimed, "labweaver-evaluation")
            .await?;
        assert_eq!(active.lease.state, ResourceLeaseState::Active);
        assert!(fixture.lifecycle.release(&active).await?.cleanup_confirmed);
    }
    Ok(())
}

#[tokio::test]
async fn cancellation_during_claim_settles_and_releases_the_committed_claim() -> TestResult {
    let fixture = Fixture::start().await?;
    let request = fixture.lifecycle.create(Duration::from_secs(30)).await?;
    approve(&fixture.faults.store, &request).await?;
    fixture
        .faults
        .cancel_after_claim
        .store(true, Ordering::SeqCst);
    assert!(matches!(fixture.wait_for_claim().await,
        Err(error) if matches!(error.downcast_ref::<TaskResourceError>(), Some(TaskResourceError::Cancelled))));
    let released = fixture.lifecycle.load_status().await?;
    assert!(released.cleanup_confirmed);
    assert_eq!(released.request.state, ResourceRequestState::Expired);
    assert_eq!(released.claim.state, CapacityClaimState::Released);
    assert_eq!(released.lease.state, ResourceLeaseState::Revoked);
    fixture
        .lifecycle
        .cancel("cancel replay after claim cleanup")
        .await?;
    Ok(())
}

#[tokio::test]
async fn active_snapshot_cannot_bypass_authoritative_lease_expiry() -> TestResult {
    let fixture = Fixture::start().await?;
    let request = fixture.lifecycle.create(Duration::from_secs(30)).await?;
    approve(&fixture.faults.store, &request).await?;
    let claimed = fixture.wait_for_claim().await?;
    let active = fixture
        .lifecycle
        .acknowledge(&claimed, "labweaver-evaluation")
        .await?;
    let expires_at = UtcTimestamp::from_utc(
        active
            .lease
            .active_from
            .ok_or("active lease has no start")?
            .get()
            + time::Duration::milliseconds(1),
    )?;
    let mut lease = active.lease.clone();
    lease.expires_at = Some(expires_at);
    sqlx::query("UPDATE resource.resource_leases SET expires_at=$2,contract=$3 WHERE lease_id=$1")
        .bind(lease.id.as_uuid())
        .bind(expires_at.get())
        .bind(serde_json::to_value(&lease)?)
        .execute(&fixture.pool)
        .await?;
    let events = fixture.event_count().await?;
    assert!(
        fixture
            .lifecycle
            .acknowledge(&active, "labweaver-evaluation")
            .await
            .is_err(),
        "an earlier Active snapshot cannot admit an expired lease"
    );
    assert_eq!(fixture.event_count().await?, events + 1);
    let pending_cleanup = fixture.lifecycle.load_status().await?;
    assert!(!pending_cleanup.cleanup_confirmed);
    assert_eq!(
        pending_cleanup.request.diagnostic_code.as_deref(),
        Some("LW_RESOURCE_TASK_LEASE_EXPIRED")
    );
    assert_eq!(fixture.event_count().await?, events + 1);
    assert!(fixture.lifecycle.release(&active).await?.cleanup_confirmed);
    Ok(())
}

#[tokio::test]
async fn waiting_timeout_fences_late_approval_and_never_allocates() -> TestResult {
    let fixture = Fixture::start().await?;
    let request = fixture.lifecycle.create(Duration::from_secs(30)).await?;
    sqlx::query("UPDATE resource.resource_requests SET approval_deadline_at=created_at+interval '1 millisecond' WHERE task_run_id=$1")
        .bind(fixture.task_run_id.as_uuid()).execute(&fixture.pool).await?;
    assert!(
        matches!(fixture.wait_for_claim().await, Err(error) if matches!(error.downcast_ref::<TaskResourceError>(), Some(TaskResourceError::ResourceApprovalTimeout)))
    );
    assert_eq!(
        fixture
            .faults
            .store
            .load_task_request(fixture.task_run_id)
            .await?
            .state,
        ResourceRequestState::Cancelled
    );
    assert!(approve(&fixture.faults.store, &request).await.is_err());
    assert!(
        fixture
            .lifecycle
            .create(Duration::from_secs(30))
            .await
            .is_ok(),
        "a lost create reply is replayable without reviving the request"
    );
    assert!(
        matches!(fixture.wait_for_claim().await, Err(error) if matches!(error.downcast_ref::<TaskResourceError>(), Some(TaskResourceError::ResourceApprovalTimeout)))
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM resource.capacity_claims")
        .fetch_one(&fixture.pool)
        .await?;
    assert_eq!(count, 0);
    Ok(())
}

#[tokio::test]
async fn committed_ack_and_release_with_lost_replies_replay_without_new_events() -> TestResult {
    let fixture = Fixture::start().await?;
    let request = fixture.lifecycle.create(Duration::from_secs(30)).await?;
    approve(&fixture.faults.store, &request).await?;
    let claimed = fixture.wait_for_claim().await?;
    fixture.faults.lose_ack_reply.store(true, Ordering::SeqCst);
    assert!(
        fixture
            .lifecycle
            .acknowledge(&claimed, "labweaver-evaluation")
            .await
            .is_err()
    );
    let active = fixture.lifecycle.load_status().await?;
    let events = fixture.event_count().await?;
    assert_eq!(
        fixture
            .lifecycle
            .acknowledge(&claimed, "labweaver-evaluation")
            .await?,
        active
    );
    let recovered = fixture.wait_for_claim().await?;
    assert_eq!(
        fixture
            .lifecycle
            .acknowledge(&recovered, "labweaver-evaluation")
            .await?,
        active
    );
    assert_eq!(fixture.event_count().await?, events);
    fixture
        .faults
        .lose_release_reply
        .store(true, Ordering::SeqCst);
    assert!(fixture.lifecycle.release(&active).await.is_err());
    let released = fixture.lifecycle.load_status().await?;
    assert!(released.cleanup_confirmed);
    let events = fixture.event_count().await?;
    assert_eq!(fixture.lifecycle.release(&active).await?, released);
    assert_eq!(fixture.event_count().await?, events);
    assert!(
        fixture
            .lifecycle
            .acknowledge(&claimed, "labweaver-evaluation")
            .await
            .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn cancellation_after_approval_and_committed_ack_converges_with_retry() -> TestResult {
    let fixture = Fixture::start().await?;
    let request = fixture.lifecycle.create(Duration::from_secs(30)).await?;
    approve(&fixture.faults.store, &request).await?;
    let claimed = fixture.wait_for_claim().await?;
    fixture
        .faults
        .cancel_after_ack
        .store(true, Ordering::SeqCst);
    let active = fixture
        .lifecycle
        .acknowledge(&claimed, "labweaver-evaluation")
        .await?;
    assert!(fixture.faults.cancellation.is_cancelled());
    assert_eq!(active.request.state, ResourceRequestState::Active);
    fixture
        .faults
        .lose_release_reply
        .store(true, Ordering::SeqCst);
    assert!(
        fixture
            .lifecycle
            .cancel("task cancelled before workload submission")
            .await
            .is_err()
    );
    fixture
        .lifecycle
        .cancel("task cancelled before workload submission")
        .await?;
    assert!(fixture.lifecycle.load_status().await?.cleanup_confirmed);
    assert!(
        fixture
            .faults
            .store
            .claim_task_resource(fixture.task_run_id, "trace-stale-claim")
            .await
            .is_err()
    );
    Ok(())
}
