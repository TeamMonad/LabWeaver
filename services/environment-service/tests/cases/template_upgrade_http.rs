//! Real JWT-authenticated HTTP, `PostgreSQL`, `JetStream` and production lifecycle wiring.
//! NATS peers validate the existing service wire protocol; only Kubernetes execution is simulated.
use super::{
    AUDIENCE, AuthorityHandle, CLIENT_ID, signed_token, signing_material, spawn_authority,
};
use crate::support::{
    self,
    manual_release::{insert_projection, projection},
    requested_instance, revision,
};
use async_trait::async_trait;
use auth::{
    ServiceAuthConfig, ServiceTokenVerifier, TransportSecurityMode, no_redirect_http_client,
};
use contracts::authoring::EnvironmentClass;
use contracts::environment::{
    EnvironmentInstance, EnvironmentLeaseAuthorization, EnvironmentLeaseState,
    EnvironmentLeaseVerificationRequest, EnvironmentLeaseVerificationResponse,
    EnvironmentLifecycleCommandData, EnvironmentOperationKind, ObservedEnvironmentState,
    OperationState,
};
use contracts::events::{DATA_SCHEMA_BASE, SPEC_VERSION, subjects};
use contracts::http::{
    EnvironmentOperationAccepted, StrongEtag, UpgradeEnvironmentTemplateRequest,
};
use contracts::{
    ArtifactRef, EnvironmentId, EventId, LeaseId, PolicyId, ReleaseId, ResourceRequestId, Revision,
    Sequence, UtcTimestamp,
};
use environment_service::{
    ContainerApplyObservation, ContainerBackendFence, ContainerProvider, ContainerProviderBackend,
    ContainerProviderConfiguration, ContainerReleasePolicy, ContainerResourcePlan,
    EnvironmentApiState, FreezeBindingConfiguration, FreezeBindingService,
    JetStreamCommandConsumer, LifecycleCommand, LifecycleCommandMessage, NatsAccessRevoker,
    NatsResourceLeaseVerifier, PgEnvironmentStore, PgReleaseProjectionStore, ProviderFailure,
    ProviderFailureCode, ProviderRegistry, ReconcileWorker, Reconciler, WorkAdmissionClientError,
    WorkAdmissionResolver, environment_api_router, with_service_auth,
};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, postgres::PgPoolOptions};
use std::{
    collections::BTreeSet,
    error::Error,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering},
    },
    time::Duration,
};
use testcontainers::{
    ContainerAsync, GenericImage, ImageExt,
    core::{IntoContainerPort, WaitFor},
    runners::AsyncRunner,
};
use testcontainers_modules::postgres::Postgres;
use tokio::sync::Mutex;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;
const FORWARD_PERMISSION: &str = "access.environment.forward";
const REVOKE_SUBJECT: &str = "labweaver.access.environment.revoke.v1";
const LEASE_SUBJECT: &str = "labweaver.resource.lease.verify.v1";
const QUARANTINE_SUBJECT: &str = "private.environment.command.quarantine.v1";

// These six wire values are the receiver's allowlist in
// services/access-service/src/grants.rs:1666-1676 (valid_environment_revocation_request).
// Access and Environment currently retain separate private DTOs/lists, not a shared contracts DTO.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum AccessReason {
    #[serde(rename = "environment_stopped")]
    Stopped,
    #[serde(rename = "environment_restarted")]
    Restarted,
    #[serde(rename = "environment_reset")]
    Reset,
    #[serde(rename = "environment_deleted")]
    Deleted,
    #[serde(rename = "environment_cancelled")]
    Cancelled,
    #[serde(rename = "environment_expired")]
    Expired,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RevokeRequest {
    version: u8,
    environment_id: EnvironmentId,
    environment_revision: Revision,
    reason: AccessReason,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RevokeResponse {
    version: u8,
    environment_id: EnvironmentId,
    environment_revision: Revision,
    access_revocation_revision: Revision,
}

#[derive(Default)]
struct Peers {
    acknowledge: AtomicBool,
    lease_mode: AtomicU8,
    revocations: AtomicUsize,
    lease_queries: AtomicUsize,
}

struct Tasks(Vec<tokio::task::AbortHandle>);
impl Tasks {
    fn track<T>(&mut self, handle: tokio::task::JoinHandle<T>) {
        self.0.push(handle.abort_handle());
        drop(handle);
    }
}
impl Drop for Tasks {
    fn drop(&mut self) {
        for task in &self.0 {
            task.abort();
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct InstalledDeployment {
    image: String,
    namespace: String,
    workspace_claim: String,
}

#[derive(Default)]
struct KubernetesBoundary {
    installed: Mutex<Option<InstalledDeployment>>,
    restart_count: AtomicUsize,
}

impl KubernetesBoundary {
    async fn install(
        &self,
        plan: &ContainerResourcePlan,
    ) -> Result<ContainerApplyObservation, ProviderFailure> {
        let deployment = plan
            .resources
            .iter()
            .find(|item| item.kind == "Deployment")
            .ok_or_else(rejected)?;
        let image = deployment
            .document
            .pointer("/spec/template/spec/containers/0/image")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(rejected)?
            .to_owned();
        let volumes = deployment
            .document
            .pointer("/spec/template/spec/volumes")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(rejected)?;
        let workspace_claim = volumes
            .iter()
            .find(|volume| volume["name"] == "workspace")
            .and_then(|volume| volume.pointer("/persistentVolumeClaim/claimName"))
            .and_then(serde_json::Value::as_str)
            .ok_or_else(rejected)?
            .to_owned();
        let ready = image == plan.image;
        *self.installed.lock().await = Some(InstalledDeployment {
            image,
            namespace: plan.namespace.clone(),
            workspace_claim,
        });
        observation(ready)
    }
}

fn rejected() -> ProviderFailure {
    ProviderFailure {
        code: ProviderFailureCode::Rejected,
        retryable: false,
    }
}
fn observation(ready: bool) -> Result<ContainerApplyObservation, ProviderFailure> {
    let now = time::OffsetDateTime::now_utc();
    let now = now
        .replace_nanosecond(u32::from(now.millisecond()) * 1_000_000)
        .map_err(|_| rejected())?;
    Ok(ContainerApplyObservation {
        ready,
        observed_at: UtcTimestamp::from_utc(now).map_err(|_| rejected())?,
    })
}

#[async_trait]
impl ContainerProviderBackend for KubernetesBoundary {
    async fn apply(
        &self,
        _fence: &ContainerBackendFence,
        plan: &ContainerResourcePlan,
    ) -> Result<ContainerApplyObservation, ProviderFailure> {
        self.install(plan).await
    }
    async fn observe(
        &self,
        _fence: &ContainerBackendFence,
        plan: &ContainerResourcePlan,
    ) -> Result<ContainerApplyObservation, ProviderFailure> {
        observation(
            self.installed
                .lock()
                .await
                .as_ref()
                .is_some_and(|installed| installed.image == plan.image),
        )
    }
    async fn restart(
        &self,
        _fence: &ContainerBackendFence,
        plan: &ContainerResourcePlan,
        _revision: Revision,
    ) -> Result<ContainerApplyObservation, ProviderFailure> {
        self.restart_count.fetch_add(1, Ordering::SeqCst);
        self.install(plan).await
    }
    async fn scale(
        &self,
        _fence: &ContainerBackendFence,
        _plan: &ContainerResourcePlan,
        _replicas: u32,
    ) -> Result<ContainerApplyObservation, ProviderFailure> {
        Err(rejected())
    }
    async fn delete_namespace(
        &self,
        _fence: &ContainerBackendFence,
        _plan: &ContainerResourcePlan,
    ) -> Result<ArtifactRef, ProviderFailure> {
        Err(rejected())
    }
}

// This suite never serves Work configuration/freeze requests. Such a request must fail closed.
struct UnusedAdmission;
#[async_trait]
impl WorkAdmissionResolver for UnusedAdmission {
    async fn resolve(
        &self,
        _run: contracts::AgentRunId,
        _query: &contracts::http::WorkConfigurationAdmissionQuery,
        _now: UtcTimestamp,
    ) -> Result<contracts::http::WorkConfigurationAdmissionBinding, WorkAdmissionClientError> {
        Err(WorkAdmissionClientError::Configuration)
    }
}

async fn spawn_peers(
    client: &async_nats::Client,
    authorization: EnvironmentLeaseAuthorization,
    peers: Arc<Peers>,
    tasks: &mut Tasks,
) -> TestResult {
    let mut revocations = client.subscribe(REVOKE_SUBJECT).await?;
    let mut leases = client.subscribe(LEASE_SUBJECT).await?;
    client.flush().await?;
    let responder = client.clone();
    let revoke_peers = Arc::clone(&peers);
    let revoke_id = authorization.environment_id;
    tasks.track(tokio::spawn(async move {
        while let Some(message) = revocations.next().await {
            let request: RevokeRequest = contracts::parse_strict_json(&message.payload)?;
            assert_eq!(request.version, 1);
            assert_eq!(request.environment_id, revoke_id);
            assert_eq!(request.reason, AccessReason::Restarted);
            revoke_peers.revocations.fetch_add(1, Ordering::SeqCst);
            if !revoke_peers.acknowledge.load(Ordering::SeqCst) {
                continue;
            }
            let reply = message.reply.ok_or("revocation reply missing")?;
            let response = RevokeResponse {
                version: 1,
                environment_id: request.environment_id,
                environment_revision: request.environment_revision,
                access_revocation_revision: revision(9),
            };
            responder
                .publish(reply, serde_json::to_vec(&response)?.into())
                .await?;
        }
        Ok::<(), Box<dyn Error + Send + Sync>>(())
    }));
    let responder = client.clone();
    tasks.track(tokio::spawn(async move {
        while let Some(message) = leases.next().await {
            let request: EnvironmentLeaseVerificationRequest =
                contracts::parse_strict_json(&message.payload)?;
            assert_eq!(request.version, 1);
            assert_eq!(request.environment_id, authorization.environment_id);
            assert_eq!(request.lease_id, authorization.lease_id);
            assert_eq!(request.owner_actor_id, authorization.owner_actor_id);
            assert_eq!(request.project_id, authorization.project_id);
            assert_eq!(request.course_id, authorization.course_id);
            assert_eq!(request.capacity_binding, authorization.capacity_binding);
            peers.lease_queries.fetch_add(1, Ordering::SeqCst);
            let mut current = authorization.clone();
            let mode = peers.lease_mode.load(Ordering::SeqCst);
            if mode == 1 {
                current.active_from = UtcTimestamp::from_utc(
                    authorization.active_from.get() - time::Duration::hours(2),
                )?;
                current.expires_at = UtcTimestamp::from_utc(
                    authorization.active_from.get() - time::Duration::hours(1),
                )?;
            }
            let response = EnvironmentLeaseVerificationResponse {
                version: 1,
                state: if mode == 2 {
                    EnvironmentLeaseState::Revoked
                } else {
                    EnvironmentLeaseState::Active
                },
                authorization: (mode != 2).then_some(current),
            };
            responder
                .publish(
                    message.reply.ok_or("lease reply missing")?,
                    serde_json::to_vec(&response)?.into(),
                )
                .await?;
        }
        Ok::<(), Box<dyn Error + Send + Sync>>(())
    }));
    Ok(())
}

async fn requested_work(store: &PgEnvironmentStore) -> TestResult<EnvironmentInstance> {
    let now = store.current_time().await?;
    let mut instance = requested_instance();
    instance.class = EnvironmentClass::Work;
    instance.lease_id = Some(LeaseId::new());
    instance.capacity_binding = Some("http-upgrade-workspace".to_owned());
    instance.eligibility_expires_at = Some(UtcTimestamp::from_utc(
        now.get() + time::Duration::hours(1),
    )?);
    instance.operation.actor_id = instance.owner_id;
    instance.operation.accepted_at = now;
    instance.operation.next_attempt_at = now;
    instance.operation.deadline_at =
        UtcTimestamp::from_utc(now.get() + time::Duration::minutes(10))?;
    instance.operation.lease_authorization = Some(EnvironmentLeaseAuthorization {
        resource_request_id: ResourceRequestId::new(),
        lease_id: instance.lease_id.ok_or("lease missing")?,
        lease_revision: revision(1),
        environment_id: instance.id,
        project_id: instance.project_id,
        course_id: instance.course_id,
        owner_actor_id: instance.owner_id,
        capacity_binding: instance.capacity_binding.clone().ok_or("claim missing")?,
        approved_resources: instance.approved_resources.clone(),
        gpu_allocation: None,
        active_from: UtcTimestamp::from_utc(now.get() - time::Duration::seconds(1))?,
        expires_at: instance.eligibility_expires_at.ok_or("expiry missing")?,
    });
    Ok(instance)
}

fn provider_worker(
    store: PgEnvironmentStore,
    releases: PgReleaseProjectionStore,
    backend: Arc<KubernetesBoundary>,
) -> TestResult<ReconcileWorker> {
    let provider = ContainerProvider::new(
        "container-primary-v1".to_owned(),
        backend,
        Arc::new(releases),
        ContainerProviderConfiguration::new(
            ContainerReleasePolicy::new(PolicyId::new(), revision(1), revision(1))?,
            "harbor.internal/labweaver-system".to_owned(),
            "labweaver-platform".to_owned(),
            "access-service".to_owned(),
            "harbor-course-pull".to_owned(),
            "nfs-rwx".to_owned(),
            "ReadWriteMany",
        )?,
    )?;
    let mut registry = ProviderRegistry::default();
    registry.register(Arc::new(provider))?;
    Ok(ReconcileWorker::new(
        store,
        Reconciler::new(registry, Duration::from_secs(1))?,
        Duration::from_secs(2),
        Duration::from_millis(10),
    )?)
}

struct Harness {
    _tasks: Tasks,
    _authority: AuthorityHandle,
    _postgres: ContainerAsync<Postgres>,
    _nats: ContainerAsync<GenericImage>,
    pool: PgPool,
    store: PgEnvironmentStore,
    worker: ReconcileWorker,
    backend: Arc<KubernetesBoundary>,
    client: async_nats::Client,
    peers: Arc<Peers>,
    ready: EnvironmentInstance,
    target: contracts::events::ReleasePublished,
    base: String,
    token: String,
}

impl Harness {
    async fn start() -> TestResult<Self> {
        let postgres = Postgres::default().with_tag("17.5-alpine").start().await?;
        let url = format!(
            "postgres://postgres:postgres@127.0.0.1:{}/postgres",
            postgres.get_host_port_ipv4(5432).await?
        );
        let pool = PgPoolOptions::new()
            .max_connections(6)
            .connect(&url)
            .await?;
        support::apply_environment_migrations(&pool).await?;
        let nats = GenericImage::new("nats", "2.11.8-alpine")
            .with_exposed_port(4222.tcp())
            .with_wait_for(WaitFor::message_on_stderr("Server is ready"))
            .with_cmd(["-js"])
            .start()
            .await?;
        let client = async_nats::connect(format!(
            "nats://127.0.0.1:{}",
            nats.get_host_port_ipv4(4222).await?
        ))
        .await?;
        let store = PgEnvironmentStore::new(pool.clone());
        let releases = PgReleaseProjectionStore::new(pool.clone());
        let instance = requested_work(&store).await?;
        let source = projection(&instance)?;
        insert_projection(&pool, &source).await?;
        let mut target = source;
        target.release.id = ReleaseId::new();
        target.release.version += 1;
        let digest = format!("sha256:{}", "b".repeat(64));
        let manual = target
            .release
            .manual_approval
            .as_mut()
            .ok_or("manual approval missing")?;
        manual.platform_image.resolved_digest.clone_from(&digest);
        manual.platform_image.repin_generation += 1;
        if let contracts::supply_chain::ImageArtifact::CatalogContainer {
            digest: artifact_digest,
            repin_generation,
            ..
        } = &mut target.release.artifact
        {
            artifact_digest.clone_from(&digest);
            *repin_generation += 1;
        }
        insert_projection(&pool, &target).await?;
        let backend = Arc::new(KubernetesBoundary::default());
        let worker = provider_worker(store.clone(), releases.clone(), Arc::clone(&backend))?;
        store.create("http-upgrade-create", &instance).await?;
        drive_ready(&store, &worker, instance.id).await?;
        let ready = store.load(instance.id).await?;
        let peers = Arc::new(Peers::default());
        let mut tasks = Tasks(Vec::new());
        spawn_peers(
            &client,
            ready
                .operation
                .lease_authorization
                .clone()
                .ok_or("authorization missing")?,
            Arc::clone(&peers),
            &mut tasks,
        )
        .await?;
        let (authority, base, token) =
            serve_authenticated_api(&pool, &store, &releases, &client, &mut tasks).await?;
        Ok(Self {
            _tasks: tasks,
            _authority: authority,
            _postgres: postgres,
            _nats: nats,
            pool,
            store,
            worker,
            backend,
            client,
            peers,
            ready,
            target,
            base,
            token,
        })
    }

    async fn upgrade(&self, key: &str, authenticated: bool) -> TestResult<reqwest::Response> {
        let mut request = reqwest::Client::builder()
            .no_proxy()
            .build()?
            .post(format!(
                "{}/api/v1/environments/{}/template-upgrade",
                self.base, self.ready.id
            ))
            .header("x-labweaver-actor-id", self.ready.owner_id.to_string())
            .header("x-labweaver-session-id", uuid::Uuid::now_v7().to_string())
            .header(
                "If-Match",
                StrongEtag::from_revision(self.ready.revision).header_value(),
            )
            .header("Idempotency-Key", key)
            .json(&UpgradeEnvironmentTemplateRequest {
                target_release_id: self.target.release.id,
                target_release_version: self.target.release.version,
                confirmed: true,
            });
        if authenticated {
            request = request.bearer_auth(&self.token);
        }
        Ok(request.send().await?)
    }
}

async fn serve_authenticated_api(
    pool: &PgPool,
    store: &PgEnvironmentStore,
    releases: &PgReleaseProjectionStore,
    client: &async_nats::Client,
    tasks: &mut Tasks,
) -> TestResult<(AuthorityHandle, String, String)> {
    let authority = spawn_authority(signing_material("template-upgrade")?).await?;
    let verifier = ServiceTokenVerifier::discover(
        ServiceAuthConfig::new(
            &authority.issuer,
            AUDIENCE.to_owned(),
            BTreeSet::from([CLIENT_ID.to_owned()]),
            BTreeSet::from([FORWARD_PERMISSION.to_owned()]),
            BTreeSet::from(["ES256".to_owned()]),
            3600,
            1,
            TransportSecurityMode::InsecureTestOnly,
        )?,
        no_redirect_http_client(None, TransportSecurityMode::InsecureTestOnly)?,
    )
    .await?;
    let token = signed_token(
        &authority.key,
        &authority.issuer,
        CLIENT_ID,
        AUDIENCE,
        [FORWARD_PERMISSION],
    )?;
    let api = environment_api_router(EnvironmentApiState::new(
        store.clone(),
        releases.clone(),
        NatsAccessRevoker::new(
            REVOKE_SUBJECT.to_owned(),
            client.clone(),
            Duration::from_millis(200),
        )?,
        NatsResourceLeaseVerifier::new(
            LEASE_SUBJECT.to_owned(),
            client.clone(),
            Duration::from_secs(2),
        )?,
        FreezeBindingService::new_with_admission_resolver(
            pool.clone(),
            releases.clone(),
            FreezeBindingConfiguration {
                container_workspace_storage_class: "nfs-rwx".to_owned(),
                vm: None,
            },
            UnusedAdmission,
        )?,
    ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}", listener.local_addr()?);
    tasks.track(tokio::spawn(async move {
        axum::serve(listener, with_service_auth(api, Arc::new(verifier))).await
    }));
    Ok((authority, base, token))
}

async fn drive_ready(
    store: &PgEnvironmentStore,
    worker: &ReconcileWorker,
    environment_id: EnvironmentId,
) -> TestResult {
    for _ in 0..10 {
        worker
            .run_once("http-upgrade-runtime", store.current_time().await?)
            .await?;
        let current = store.load(environment_id).await?;
        if current.observed_state == ObservedEnvironmentState::Ready {
            return Ok(());
        }
        if current.operation.state == OperationState::Failed {
            return Err(format!("runtime failed: {:?}", current.last_diagnostic_code).into());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    Err("runtime did not become ready".into())
}

#[tokio::test]
async fn authenticated_http_upgrade_requires_restart_revocation_ack_and_preserves_workspace()
-> TestResult {
    let harness = Harness::start().await?;
    let original = harness
        .backend
        .installed
        .lock()
        .await
        .clone()
        .ok_or("source runtime missing")?;
    let anonymous = harness.upgrade("http-upgrade-anonymous", false).await?;
    assert_eq!(anonymous.status(), reqwest::StatusCode::UNAUTHORIZED);
    let before: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM environment.environment_operations WHERE environment_id=$1",
    )
    .bind(harness.ready.id.as_uuid())
    .fetch_one(&harness.pool)
    .await?;
    let refused = harness.upgrade("http-upgrade-no-ack", true).await?;
    assert_eq!(refused.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
    let refusal: serde_json::Value = refused.json().await?;
    assert_eq!(
        refusal["diagnosticCode"],
        "LW_ENVIRONMENT_ACCESS_REVOCATION_TIMEOUT"
    );
    assert_eq!(harness.store.load(harness.ready.id).await?, harness.ready);
    let after: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM environment.environment_operations WHERE environment_id=$1",
    )
    .bind(harness.ready.id.as_uuid())
    .fetch_one(&harness.pool)
    .await?;
    assert_eq!(
        before, after,
        "failed revocation cannot enter upgrade persistence"
    );
    assert_eq!(
        harness.backend.installed.lock().await.as_ref(),
        Some(&original)
    );
    harness.peers.acknowledge.store(true, Ordering::SeqCst);
    let accepted = harness.upgrade("http-upgrade-acknowledged", true).await?;
    let status = accepted.status();
    let value: serde_json::Value = accepted.json().await?;
    assert_eq!(status, reqwest::StatusCode::ACCEPTED, "{value}");
    let accepted: EnvironmentOperationAccepted = serde_json::from_value(value)?;
    assert_eq!(
        harness.store.load(harness.ready.id).await?.release_id,
        harness.target.release.id
    );
    drive_ready(&harness.store, &harness.worker, harness.ready.id).await?;
    let upgraded = harness.store.load(harness.ready.id).await?;
    let installed = harness
        .backend
        .installed
        .lock()
        .await
        .clone()
        .ok_or("target runtime missing")?;
    assert_ne!(installed.image, original.image);
    assert_eq!(
        installed.image,
        harness
            .target
            .release
            .artifact
            .container_reference()
            .map(|(repository, digest)| format!("{repository}@{digest}"))
            .ok_or("container identity missing")?
    );
    assert_eq!(installed.namespace, original.namespace);
    assert_eq!(installed.workspace_claim, original.workspace_claim);
    assert_eq!(upgraded.capacity_binding, harness.ready.capacity_binding);
    assert_eq!(upgraded.lease_id, harness.ready.lease_id);
    assert_eq!(upgraded.operation.state, OperationState::Succeeded);
    let replay = harness.upgrade("http-upgrade-acknowledged", true).await?;
    assert_eq!(replay.status(), reqwest::StatusCode::ACCEPTED);
    assert_eq!(
        replay.json::<EnvironmentOperationAccepted>().await?,
        accepted
    );
    assert_eq!(harness.peers.revocations.load(Ordering::SeqCst), 2);
    assert_eq!(harness.backend.restart_count.load(Ordering::SeqCst), 1);
    Ok(())
}

#[tokio::test]
async fn authenticated_http_restart_rejects_expired_and_revoked_leases_after_revocation_ack()
-> TestResult {
    const COUNTS: &str = "SELECT (SELECT count(*) FROM environment.environment_instances), \
        (SELECT count(*) FROM environment.environment_operations), \
        (SELECT count(*) FROM environment.outbox_events)";
    let harness = Harness::start().await?;
    harness.peers.acknowledge.store(true, Ordering::SeqCst);
    let before: (i64, i64, i64) = sqlx::query_as(COUNTS).fetch_one(&harness.pool).await?;
    let installed = harness.backend.installed.lock().await.clone();
    let client = reqwest::Client::builder().no_proxy().build()?;

    for (index, (label, mode)) in [("expired-active", 1), ("revoked", 2)]
        .into_iter()
        .enumerate()
    {
        harness.peers.lease_mode.store(mode, Ordering::SeqCst);
        let response = client
            .post(format!(
                "{}/api/v1/environments/{}/restart",
                harness.base, harness.ready.id
            ))
            .bearer_auth(&harness.token)
            .header("x-labweaver-actor-id", harness.ready.owner_id.to_string())
            .header("x-labweaver-session-id", uuid::Uuid::now_v7().to_string())
            .header(
                "If-Match",
                StrongEtag::from_revision(harness.ready.revision).header_value(),
            )
            .header("Idempotency-Key", format!("http-restart-lease-{label}"))
            .send()
            .await?;
        assert_eq!(
            response.status(),
            reqwest::StatusCode::SERVICE_UNAVAILABLE,
            "{label}"
        );
        let problem: serde_json::Value = response.json().await?;
        assert_eq!(
            problem["diagnosticCode"], "LW_ENVIRONMENT_LEASE_VERIFICATION_RESPONSE_INVALID",
            "{label}: {problem}"
        );
        assert_eq!(
            harness.peers.revocations.load(Ordering::SeqCst),
            index + 1,
            "{label}: production revocation must receive its valid ack before the lease denial"
        );
        assert_eq!(
            harness.peers.lease_queries.load(Ordering::SeqCst),
            index + 1,
            "{label}: the signed HTTP request must reach lease verification"
        );
        assert_eq!(
            harness.store.load(harness.ready.id).await?,
            harness.ready,
            "{label}"
        );
        let after: (i64, i64, i64) = sqlx::query_as(COUNTS).fetch_one(&harness.pool).await?;
        assert_eq!(
            after, before,
            "{label}: instance, operation and Outbox counts must not change"
        );
        assert_eq!(
            harness.backend.restart_count.load(Ordering::SeqCst),
            0,
            "{label}"
        );
        assert_eq!(
            *harness.backend.installed.lock().await,
            installed,
            "{label}"
        );
    }
    Ok(())
}

fn restart_message(
    instance: &EnvironmentInstance,
    now: UtcTimestamp,
) -> TestResult<LifecycleCommandMessage> {
    Ok(LifecycleCommandMessage {
        specversion: SPEC_VERSION.to_owned(),
        id: EventId::new(),
        source: "urn:labweaver:environment-service".to_owned(),
        event_type: subjects::ENVIRONMENT_LIFECYCLE_REQUESTED.to_owned(),
        subject: subjects::ENVIRONMENT_LIFECYCLE_REQUESTED.to_owned(),
        time: now,
        datacontenttype: "application/json".to_owned(),
        dataschema: format!("{DATA_SCHEMA_BASE}/environment-lifecycle-requested.schema.json"),
        project_id: instance.project_id,
        course_id: instance.course_id,
        aggregate_revision: instance.revision,
        aggregate_sequence: Sequence(1),
        trace_id: "nats-restart-review".to_owned(),
        data: EnvironmentLifecycleCommandData {
            idempotency_key: "nats-restart-review".to_owned(),
            create: None,
            command: LifecycleCommand {
                environment_id: instance.id,
                kind: EnvironmentOperationKind::Restart,
                expected_revision: instance.revision,
                actor_id: instance.owner_id,
                trace_id: "nats-restart-review".to_owned(),
                accepted_at: now,
                deadline_at: UtcTimestamp::from_utc(now.get() + time::Duration::minutes(10))?,
                access_revocation_revision: Some(revision(9)),
                preserve_mutable_disk: true,
                max_attempts: 3,
                reset_target: None,
            },
        },
    })
}

#[tokio::test]
async fn nats_work_restart_requires_current_lease_and_deduplicates_execution() -> TestResult {
    let harness = Harness::start().await?;
    let context = async_nats::jetstream::new(harness.client.clone());
    context
        .create_stream(async_nats::jetstream::stream::Config {
            name: "REVIEW_COMMANDS".to_owned(),
            subjects: vec![subjects::ENVIRONMENT_LIFECYCLE_REQUESTED.to_owned()],
            ..Default::default()
        })
        .await?
        .create_consumer(async_nats::jetstream::consumer::pull::Config {
            durable_name: Some("review-restart".to_owned()),
            filter_subject: subjects::ENVIRONMENT_LIFECYCLE_REQUESTED.to_owned(),
            ack_policy: async_nats::jetstream::consumer::AckPolicy::Explicit,
            ..Default::default()
        })
        .await?;
    context
        .create_stream(async_nats::jetstream::stream::Config {
            name: "REVIEW_QUARANTINE".to_owned(),
            subjects: vec![QUARANTINE_SUBJECT.to_owned()],
            ..Default::default()
        })
        .await?;
    let mut consumer = JetStreamCommandConsumer::bind(
        harness.client.clone(),
        "REVIEW_COMMANDS",
        "review-restart",
        QUARANTINE_SUBJECT,
    )
    .await?;
    let verifier = NatsResourceLeaseVerifier::new(
        LEASE_SUBJECT.to_owned(),
        harness.client.clone(),
        Duration::from_secs(2),
    )?;
    let before: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM environment.environment_operations WHERE environment_id=$1",
    )
    .bind(harness.ready.id.as_uuid())
    .fetch_one(&harness.pool)
    .await?;
    for mode in [1, 2] {
        harness.peers.lease_mode.store(mode, Ordering::SeqCst);
        let message = restart_message(&harness.ready, harness.store.current_time().await?)?;
        context
            .publish(
                subjects::ENVIRONMENT_LIFECYCLE_REQUESTED,
                serde_json::to_vec(&message)?.into(),
            )
            .await?
            .await?;
        let outcome = tokio::time::timeout(
            Duration::from_secs(5),
            consumer.process_next(&harness.store, &verifier),
        )
        .await??;
        assert_eq!(
            outcome,
            environment_service::CommandConsumeOutcome::Rejected
        );
        assert_eq!(harness.store.load(harness.ready.id).await?, harness.ready);
    }
    harness.peers.lease_mode.store(0, Ordering::SeqCst);
    let message = restart_message(&harness.ready, harness.store.current_time().await?)?;
    for expected in [
        environment_service::CommandConsumeOutcome::Applied,
        environment_service::CommandConsumeOutcome::Ignored,
    ] {
        context
            .publish(
                subjects::ENVIRONMENT_LIFECYCLE_REQUESTED,
                serde_json::to_vec(&message)?.into(),
            )
            .await?
            .await?;
        let outcome = tokio::time::timeout(
            Duration::from_secs(5),
            consumer.process_next(&harness.store, &verifier),
        )
        .await??;
        assert_eq!(outcome, expected);
    }
    let current = harness.store.load(harness.ready.id).await?;
    assert_eq!(current.operation.kind, EnvironmentOperationKind::Restart);
    assert!(current.operation.lease_authorization.is_some());
    let after: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM environment.environment_operations WHERE environment_id=$1",
    )
    .bind(harness.ready.id.as_uuid())
    .fetch_one(&harness.pool)
    .await?;
    assert_eq!(after, before + 1);
    drive_ready(&harness.store, &harness.worker, harness.ready.id).await?;
    assert_eq!(harness.backend.restart_count.load(Ordering::SeqCst), 1);
    assert_eq!(harness.peers.lease_queries.load(Ordering::SeqCst), 4);
    Ok(())
}
