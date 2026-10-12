//! Dependency-aware Resource Service process runtime.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use sqlx::postgres::PgPoolOptions;
use tokio::sync::watch;

use crate::capacity::{
    CapacityProviderError, CapacityReconcileWorker, GpuCatalogSeed, ResourceCapacityConfiguration,
};
use crate::messaging::{NatsLeaseResponderError, NatsLeaseVerificationResponder};
use crate::outbox::{ResourceOutboxDispatcher, ResourceOutboxError};
use crate::store::{PgResourceStore, resource_error_kind, resource_error_safe_detail};
use auth::{
    ServiceAuthConfig, ServiceTokenClient, ServiceTokenClientConfig, ServiceTokenVerifier,
    TransportSecurityMode,
};

const DATABASE_URL: &str = "LABWEAVER_DATABASE_URL";
const NATS_SERVER: &str = "LABWEAVER_NATS_SERVER";
const NATS_CA_PATH: &str = "LABWEAVER_NATS_CA_PATH";
const NATS_CLIENT_CERTIFICATE_PATH: &str = "LABWEAVER_NATS_CLIENT_CERT_PATH";
const NATS_CLIENT_PRIVATE_KEY_PATH: &str = "LABWEAVER_NATS_CLIENT_KEY_PATH";
const NATS_CREDENTIALS_PATH: &str = "LABWEAVER_NATS_CREDENTIALS_PATH";
const LEASE_VERIFICATION_SUBJECT: &str = "LABWEAVER_RESOURCE_LEASE_VERIFICATION_SUBJECT";
const CAPACITY_CONFIG_FILE: &str = "LABWEAVER_RESOURCE_CAPACITY_CONFIG_FILE";
const SERVICE_OIDC_ISSUER: &str = "LABWEAVER_SERVICE_OIDC_ISSUER";
const SERVICE_AUDIENCE: &str = "LABWEAVER_SERVICE_AUDIENCE";
const SERVICE_ALLOWED_CLIENT_IDS: &str = "LABWEAVER_SERVICE_ALLOWED_CLIENT_IDS";
const SERVICE_JWT_ALGORITHMS: &str = "LABWEAVER_SERVICE_JWT_ALGORITHMS";
const SERVICE_JWKS_REFRESH_SECONDS: &str = "LABWEAVER_SERVICE_JWKS_REFRESH_SECONDS";
const SERVICE_JWKS_RETRY_SECONDS: &str = "LABWEAVER_SERVICE_JWKS_RETRY_SECONDS";
const SERVICE_OIDC_CA: &str = "LABWEAVER_SERVICE_OIDC_CA";
const SERVICE_CLIENT_ID: &str = "LABWEAVER_SERVICE_CLIENT_ID";
const SERVICE_CLIENT_SECRET_FILE: &str = "LABWEAVER_SERVICE_CLIENT_SECRET_FILE";
const SERVICE_SCOPES: &str = "LABWEAVER_SERVICE_SCOPES";
const SERVICE_TOKEN_REFRESH_SKEW_SECONDS: &str = "LABWEAVER_SERVICE_TOKEN_REFRESH_SKEW_SECONDS";
const ACCESS_SERVICE_CLIENT_ID: &str = "LABWEAVER_ACCESS_SERVICE_CLIENT_ID";
const ENVIRONMENT_SERVICE_CLIENT_ID: &str = "LABWEAVER_ENVIRONMENT_SERVICE_CLIENT_ID";
const CURSOR_SIGNING_KEY_CURRENT_FILE: &str = "LABWEAVER_CURSOR_SIGNING_KEY_CURRENT_FILE";
const CURSOR_SIGNING_KEY_PREVIOUS_FILE: &str = "LABWEAVER_CURSOR_SIGNING_KEY_PREVIOUS_FILE";
const TASK_SERVICE_CLIENT_IDS: &str = "LABWEAVER_TASK_SERVICE_CLIENT_IDS";
const ENVIRONMENT_SERVICE_AUDIENCE: &str = "labweaver-environment";
const OUTBOX_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
const OUTBOX_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);

/// Production dependency graph for Resource-owned Lease authorization.
pub struct ResourceProcessRuntime {
    responder: NatsLeaseVerificationResponder,
    store: PgResourceStore,
    service_verifier: Arc<ServiceTokenVerifier>,
    access_service_client_id: String,
    environment_service_client_id: String,
    task_service_client_ids: BTreeSet<String>,
    cursor_signing_key_current: Arc<Vec<u8>>,
    cursor_signing_key_previous: Option<Arc<Vec<u8>>>,
    build_service_client_id: Option<String>,
    capacity_worker: CapacityReconcileWorker,
    outbox: ResourceOutboxDispatcher,
    readiness: Arc<AtomicBool>,
    _shutdown_sender: watch::Sender<bool>,
    shutdown: watch::Receiver<bool>,
}

impl ResourceProcessRuntime {
    /// Opens only explicit authority dependencies and refuses startup before migrations exist.
    pub async fn from_env() -> Result<Self, ResourceProcessRuntimeError> {
        let pool = PgPoolOptions::new()
            .max_connections(16)
            .connect(&required(DATABASE_URL)?)
            .await?;
        let schema_ready: Option<String> =
            sqlx::query_scalar("SELECT CASE WHEN to_regclass('resource.credit_accounts') IS NOT NULL AND to_regclass('resource.credit_holds') IS NOT NULL AND to_regclass('resource.credit_ledger_entries') IS NOT NULL THEN to_regclass('resource.resource_leases')::text END")
                .fetch_one(&pool)
                .await?;
        if schema_ready.is_none() {
            return Err(ResourceProcessRuntimeError::SchemaMissing);
        }
        let deadline_ready: bool = sqlx::query_scalar(
            "SELECT EXISTS (
             SELECT 1 FROM information_schema.columns WHERE table_schema='resource'
             AND table_name='resource_requests' AND column_name='approval_deadline_at')",
        )
        .fetch_one(&pool)
        .await?;
        if !deadline_ready {
            return Err(ResourceProcessRuntimeError::SchemaMissing);
        }
        let missing_deadlines: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM resource.resource_requests
             WHERE target_kind='task' AND state='reviewing' AND approval_deadline_at IS NULL",
        )
        .fetch_one(&pool)
        .await?;
        if missing_deadlines != 0 {
            return Err(ResourceProcessRuntimeError::ApprovalDeadlineMissing);
        }
        let client = connect_nats_mtls(
            &required(NATS_SERVER)?,
            required_path(NATS_CA_PATH)?,
            required_path(NATS_CLIENT_CERTIFICATE_PATH)?,
            required_path(NATS_CLIENT_PRIVATE_KEY_PATH)?,
            required_path(NATS_CREDENTIALS_PATH)?,
        )
        .await?;
        let responder = NatsLeaseVerificationResponder::new(
            required(LEASE_VERIFICATION_SUBJECT)?,
            client.clone(),
        )?;
        let capacity_configuration = ResourceCapacityConfiguration::parse(
            &std::fs::read(required_path(CAPACITY_CONFIG_FILE)?)
                .map_err(|_| ResourceProcessRuntimeError::CapacityConfiguration)?,
        )
        .map_err(|_| ResourceProcessRuntimeError::CapacityConfiguration)?;
        let service_verifier = discover_service_verifier().await?;
        let environment_token_client = discover_environment_token_client().await?;
        let access_service_client_id = required(ACCESS_SERVICE_CLIENT_ID)?;
        let environment_service_client_id = required(ENVIRONMENT_SERVICE_CLIENT_ID)?;
        let task_service_client_ids = required_set(TASK_SERVICE_CLIENT_IDS)?;
        let cursor_current = read_secret_key(&required_path(CURSOR_SIGNING_KEY_CURRENT_FILE)?)?;
        let cursor_previous = optional_path(CURSOR_SIGNING_KEY_PREVIOUS_FILE)?
            .map(|path| read_secret_key(&path))
            .transpose()?;
        let build_service_client_id = std::env::var("LABWEAVER_AGENT_SERVICE_CLIENT_ID")
            .ok()
            .filter(|v| !v.trim().is_empty());
        let store = PgResourceStore::new(pool)
            .with_cursor_signing_keys(cursor_current.clone(), cursor_previous.clone())
            .with_environment_credit_owner(environment_service_client_id.clone())
        seed_reviewed_gpu_catalog(&store, &capacity_configuration.gpu_catalog_seed).await?;
        let outbox = ResourceOutboxDispatcher::new(store.pool(), client, OUTBOX_TIMEOUT)
            .map_err(ResourceProcessRuntimeError::Outbox)?;
        let capacity_worker = capacity_configuration
            .build_worker(store.clone(), environment_token_client)
            .map_err(ResourceProcessRuntimeError::CapacityProvider)?;
        let (shutdown_sender, shutdown) = watch::channel(false);
        Ok(Self {
            responder,
            store,
            service_verifier,
            access_service_client_id,
            environment_service_client_id,
            task_service_client_ids,
            cursor_signing_key_current: Arc::new(cursor_current),
            cursor_signing_key_previous: cursor_previous.map(Arc::new),
            build_service_client_id,
            capacity_worker,
            outbox,
            readiness: Arc::new(AtomicBool::new(true)),
            _shutdown_sender: shutdown_sender,
            shutdown,
        })
    }

    #[must_use]
    pub fn readiness(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.readiness)
    }

    #[must_use]
    pub fn api_state(&self) -> crate::api::ResourceApiState {
        crate::api::ResourceApiState::new(self.store.for_http())
            .with_service_verifier(Arc::clone(&self.service_verifier))
            .with_access_service_client_id(self.access_service_client_id.clone())
            .with_environment_service_client_id(self.environment_service_client_id.clone())
            .with_cursor_signing_keys(
                self.cursor_signing_key_current.as_ref().clone(),
                self.cursor_signing_key_previous.as_deref().cloned(),
            )
            .with_task_service_client_ids(self.task_service_client_ids.clone())
            .with_build_service_client_id(self.build_service_client_id.clone())
    }

    /// Keeps the responder live. A failed authoritative dependency flips readiness false.
    pub async fn run(self) -> Result<(), ResourceProcessRuntimeError> {
        let Self {
            responder,
            store,
            capacity_worker,
            outbox,
            readiness,
            service_verifier: _service_verifier,
            access_service_client_id: _access_service_client_id,
            environment_service_client_id: _environment_service_client_id,
            cursor_signing_key_current: _cursor_signing_key_current,
            cursor_signing_key_previous: _cursor_signing_key_previous,
            task_service_client_ids: _task_service_client_ids,
            build_service_client_id: _build_service_client_id,
            _shutdown_sender,
            shutdown,
        } = self;
        let responder_shutdown = shutdown.clone();
        let outbox_shutdown = shutdown.clone();
        let settlement_store = store.clone();
        let result = tokio::try_join!(
            async {
                responder
                    .serve(store, responder_shutdown)
                    .await
                    .map_err(ResourceProcessRuntimeError::Responder)
            },
            async {
                capacity_worker
                    .run(shutdown)
                    .await
                    .map_err(ResourceProcessRuntimeError::Store)
            },
            async {
                run_dispatch_and_accounting(outbox, settlement_store, outbox_shutdown)
                    .await
                    .map_err(ResourceProcessRuntimeError::Outbox)
            },
        )
        .map(|_| ());
        if result.is_err() {
            readiness.store(false, Ordering::Release);
        }
        result
    }
}

/// Provisions the reviewed GPU classes before the capacity worker observes them.
///
/// Validation already happened when the configuration was parsed, so a seed rejection here is a
/// durable conflict and fails startup rather than rewriting the reviewed catalog. An unseeded
/// class simply stays absent and cannot grant capacity.
async fn seed_reviewed_gpu_catalog(
    store: &PgResourceStore,
    seeds: &[GpuCatalogSeed],
) -> Result<(), ResourceProcessRuntimeError> {
    if seeds.is_empty() {
        return Ok(());
    }
    let existing: BTreeSet<String> = store
        .list_gpu_catalog()
        .await
        .map_err(ResourceProcessRuntimeError::Store)?
        .into_iter()
        .map(|entry| entry.class)
        .collect();
    let created = seeds
        .iter()
        .filter(|seed| !existing.contains(&seed.class))
        .count();
    let catalog = store
        .seed_gpu_catalog(seeds)
        .await
        .map_err(ResourceProcessRuntimeError::Store)?;
    tracing::info!(
        event = "resource.gpu_catalog.seeded",
        created,
        already_existing = seeds.len() - created,
        catalog_rows = catalog.len(),
    );
    Ok(())
}

async fn run_dispatch_and_accounting(
    outbox: ResourceOutboxDispatcher,
    store: PgResourceStore,
    shutdown: watch::Receiver<bool>,
) -> Result<(), ResourceOutboxError> {
    tokio::try_join!(
        run_outbox(outbox, shutdown.clone()),
        run_accounting(store, shutdown)
    )
    .map(|_| ())
}

async fn run_outbox(
    outbox: ResourceOutboxDispatcher,
    mut shutdown: watch::Receiver<bool>,
) -> Result<(), ResourceOutboxError> {
    let mut interval = tokio::time::interval(OUTBOX_INTERVAL);
    loop {
        tokio::select! {
            biased;
            changed = shutdown.changed() => { if changed.is_err() || *shutdown.borrow() { return Ok(()); } }
            _ = interval.tick() => {
                if let Err(error) = store.expire_pending_task_once().await {
                    let safe_detail = resource_error_safe_detail(&error);
                    tracing::warn!(event = "resource.task_expiry.failed", outcome = "will_retry",
                        error_kind = resource_error_kind(&error), failure_stage = "task.expiry",
                        safe_detail = safe_detail.as_str());
                }
                if let Err(ref e) = outbox.dispatch_once().await {
                    let safe_detail = e.safe_detail();
                    tracing::warn!(
                        component = "outbox",
                        event = "resource.outbox.failed",
                        operation = "nats.publish",
                        outcome = "will_retry",
                        error_kind = e.error_kind(),
                        failure_stage = e.failure_stage(),
                        safe_detail = safe_detail.as_str(),
                    );
                }

            }

        }
    }
}

async fn run_accounting(
    store: PgResourceStore,
    mut shutdown: watch::Receiver<bool>,
) -> Result<(), ResourceOutboxError> {
    let credits = crate::credits::PgCreditStore::new(store.pool());
    let mut interval = tokio::time::interval(OUTBOX_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            biased;
            changed=shutdown.changed()=>{if changed.is_err() || *shutdown.borrow(){return Ok(());}}
            _=interval.tick()=>{
                if let Err(error)=store.reconcile_build_deadlines_once().await {
                    tracing::warn!(event="resource.build_cost.deadline_failed",diagnostic_code=%error,outcome="will_retry");
                }
                if let Err(error)=store.reconcile_environment_funding_deadlines_once().await {
                    tracing::warn!(event="resource.environment_cost.deadline_failed",diagnostic_code=%error,outcome="will_retry");
                }
                if let Err(error)=credits.expire_unknown_once().await {
                    tracing::warn!(event="resource.credits.unknown_deadline_failed",diagnostic_code=%error,outcome="will_retry");
                }
                if let Err(error)=credits.reconcile_terminal_once().await {
                    tracing::warn!(event="resource.credits.recovery_failed",diagnostic_code=%error,outcome="will_retry");
                }
                if let Err(error) = store.settle_pending_usage_once().await {
                    let safe_detail = resource_error_safe_detail(&error);
                    tracing::warn!(
                        component = "settlement",
                        event = "resource.settlement.failed",
                        operation = "settle_pending_usage",
                        outcome = "will_retry",
                        error_kind = resource_error_kind(&error),
                        failure_stage = "settlement.worker",
                        safe_detail = safe_detail.as_str(),
                    );
                }
            }
        }
    }
}

async fn discover_service_verifier()
-> Result<Arc<ServiceTokenVerifier>, ResourceProcessRuntimeError> {
    let issuer = required(SERVICE_OIDC_ISSUER)?;
    let audience = required(SERVICE_AUDIENCE)?;
    let allowed_client_ids = required_set(SERVICE_ALLOWED_CLIENT_IDS)?;
    let algorithms = required_set(SERVICE_JWT_ALGORITHMS)?;
    let jwks_refresh_seconds = required_u64(SERVICE_JWKS_REFRESH_SECONDS)?;
    let jwks_retry_seconds = required_u64(SERVICE_JWKS_RETRY_SECONDS)?;
    let ca = std::fs::read(required_path(SERVICE_OIDC_CA)?)
        .map_err(|_| ResourceProcessRuntimeError::ServiceAuthConfiguration)?;
    let http = auth::no_redirect_http_client(Some(&ca), TransportSecurityMode::Strict)
        .map_err(|_| ResourceProcessRuntimeError::ServiceAuthConfiguration)?;
    let config = ServiceAuthConfig::new(
        &issuer,
        audience,
        allowed_client_ids,
        BTreeSet::new(),
        algorithms,
        jwks_refresh_seconds,
        jwks_retry_seconds,
        TransportSecurityMode::Strict,
    )
    .map_err(|_| ResourceProcessRuntimeError::ServiceAuthConfiguration)?;
    ServiceTokenVerifier::discover(config, http)
        .await
        .map(Arc::new)
        .map_err(ResourceProcessRuntimeError::ServiceAuth)
}

async fn discover_environment_token_client()
-> Result<ServiceTokenClient, ResourceProcessRuntimeError> {
    let oidc_ca = std::fs::read(required_path(SERVICE_OIDC_CA)?)
        .map_err(|_| ResourceProcessRuntimeError::ServiceAuthConfiguration)?;
    let oidc_http = auth::no_redirect_http_client(Some(&oidc_ca), TransportSecurityMode::Strict)
        .map_err(|_| ResourceProcessRuntimeError::ServiceAuthConfiguration)?;
    let scopes = required_set(SERVICE_SCOPES)?;
    if !scopes.contains("resource.environment.manage") {
        return Err(ResourceProcessRuntimeError::ServiceAuthConfiguration);
    }
    let config = ServiceTokenClientConfig::new(
        &required(SERVICE_OIDC_ISSUER)?,
        required(SERVICE_CLIENT_ID)?,
        read_secret(&required_path(SERVICE_CLIENT_SECRET_FILE)?)?,
        ENVIRONMENT_SERVICE_AUDIENCE.to_owned(),
        scopes,
        required_u64(SERVICE_TOKEN_REFRESH_SKEW_SECONDS)?,
        TransportSecurityMode::Strict,
    )
    .map_err(ResourceProcessRuntimeError::ServiceTokenClient)?;
    ServiceTokenClient::discover(config, oidc_http)
        .await
        .map_err(ResourceProcessRuntimeError::ServiceTokenClient)
}

async fn connect_nats_mtls(
    server: &str,
    ca_path: PathBuf,
    client_certificate_path: PathBuf,
    client_private_key_path: PathBuf,
    credentials_path: PathBuf,
) -> Result<async_nats::Client, ResourceProcessRuntimeError> {
    let options = async_nats::ConnectOptions::new()
        .require_tls(true)
        .add_root_certificates(ca_path)
        .add_client_certificate(client_certificate_path, client_private_key_path)
        .credentials_file(credentials_path)
        .await
        .map_err(|_| ResourceProcessRuntimeError::NatsCredentials)?;
    options
        .connect(server)
        .await
        .map_err(|_| ResourceProcessRuntimeError::NatsConnect)
}

fn required(name: &'static str) -> Result<String, ResourceProcessRuntimeError> {
    let value =
        std::env::var(name).map_err(|_| ResourceProcessRuntimeError::MissingConfiguration(name))?;
    if value.trim().is_empty() {
        return Err(ResourceProcessRuntimeError::MissingConfiguration(name));
    }
    Ok(value)
}

fn required_path(name: &'static str) -> Result<PathBuf, ResourceProcessRuntimeError> {
    let path = PathBuf::from(required(name)?);
    if !path.is_absolute() {
        return Err(ResourceProcessRuntimeError::PathConfiguration(name));
    }
    Ok(path)
}
fn optional_path(name: &'static str) -> Result<Option<PathBuf>, ResourceProcessRuntimeError> {
    let Some(value) = std::env::var_os(name) else {
        return Ok(None);
    };
    let path = PathBuf::from(value);
    if !path.is_absolute() {
        return Err(ResourceProcessRuntimeError::PathConfiguration(name));
    }
    Ok(Some(path))
}

fn required_set(name: &'static str) -> Result<BTreeSet<String>, ResourceProcessRuntimeError> {
    let value = required(name)?;
    let values = value
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    if values.is_empty() {
        return Err(ResourceProcessRuntimeError::MissingConfiguration(name));
    }
    Ok(values)
}

fn required_u64(name: &'static str) -> Result<u64, ResourceProcessRuntimeError> {
    let value = required(name)?;
    let value = value
        .parse::<u64>()
        .map_err(|_| ResourceProcessRuntimeError::ServiceAuthConfiguration)?;
    if value == 0 {
        return Err(ResourceProcessRuntimeError::ServiceAuthConfiguration);
    }
    Ok(value)
}

fn read_secret(path: &PathBuf) -> Result<String, ResourceProcessRuntimeError> {
    let value = std::fs::read_to_string(path)
        .map_err(|_| ResourceProcessRuntimeError::ServiceAuthConfiguration)?;
    let value = value.trim();
    if value.is_empty() || value.bytes().any(|byte| byte.is_ascii_control()) {
        return Err(ResourceProcessRuntimeError::ServiceAuthConfiguration);
    }
    Ok(value.to_owned())
}
fn read_secret_key(path: &PathBuf) -> Result<Vec<u8>, ResourceProcessRuntimeError> {
    let value =
        std::fs::read(path).map_err(|_| ResourceProcessRuntimeError::ServiceAuthConfiguration)?;
    if value.len() < 32 {
        return Err(ResourceProcessRuntimeError::ServiceAuthConfiguration);
    }
    Ok(value)
}

#[derive(Debug, thiserror::Error)]
pub enum ResourceProcessRuntimeError {
    #[error("LW_RESOURCE_CONFIG_MISSING: {0}")]
    MissingConfiguration(&'static str),
    #[error("LW_RESOURCE_CONFIG_PATH_INVALID: {0}")]
    PathConfiguration(&'static str),
    #[error("LW_RESOURCE_SCHEMA_MISSING")]
    SchemaMissing,
    #[error("LW_RESOURCE_TASK_APPROVAL_DEADLINE_MISSING")]
    ApprovalDeadlineMissing,
    #[error("LW_RESOURCE_DATABASE_CONNECT_FAILED")]
    Database(#[from] sqlx::Error),
    #[error("LW_RESOURCE_NATS_CREDENTIALS_INVALID")]
    NatsCredentials,
    #[error("LW_RESOURCE_NATS_CONNECT_FAILED")]
    NatsConnect,
    #[error("LW_RESOURCE_CAPACITY_CONFIGURATION_INVALID")]
    CapacityConfiguration,
    #[error("LW_RESOURCE_CAPACITY_PROVIDER_INITIALIZATION_FAILED: {0}")]
    CapacityProvider(#[source] CapacityProviderError),
    #[error("LW_RESOURCE_OUTBOX_FAILED: {0}")]
    Outbox(#[source] ResourceOutboxError),
    #[error("LW_RESOURCE_STORE_FAILED: {0}")]
    Store(#[source] crate::store::ResourceStoreError),
    #[error("LW_RESOURCE_NATS_RESPONDER_FAILED: {0}")]
    Responder(#[from] NatsLeaseResponderError),
    #[error("LW_AUTH_SERVICE_CONFIG_INVALID")]
    ServiceAuthConfiguration,
    #[error("LW_AUTH_SERVICE_UNAVAILABLE: {0}")]
    ServiceAuth(#[source] auth::ServiceAuthError),
    #[error("LW_AUTH_SERVICE_TOKEN_CLIENT_UNAVAILABLE: {0}")]
    ServiceTokenClient(#[source] auth::ServiceTokenClientError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use contracts::resource::credits::{
        CreditAdminOperation, CreditHoldRequest, CreditHoldResolutionRequest,
    };
    use contracts::resource::{
        FixedDecimal, ResourceRequest, ResourceRequestState, ResourceTarget, WorkloadResources,
    };
    use contracts::{
        ActorId, EnvironmentId, PlatformRole, ProjectId, ReleaseId, ResourceRequestId, Revision,
    };
    use futures_util::StreamExt;
    use testcontainers::{
        GenericImage, ImageExt,
        core::{IntoContainerPort, WaitFor},
        runners::AsyncRunner,
    };
    use testcontainers_modules::postgres::Postgres;

    #[tokio::test]
    #[allow(
        clippy::too_many_lines,
        reason = "the real database and broker fault scenario keeps its ordering and observable assertions together"
    )]
    async fn stalled_jetstream_ack_does_not_block_unknown_fund_release()
    -> Result<(), Box<dyn std::error::Error>> {
        let postgres = Postgres::default().with_tag("17.5-alpine").start().await?;
        let pool = PgPoolOptions::new()
            .max_connections(8)
            .connect(&format!(
                "postgres://postgres:postgres@127.0.0.1:{}/postgres",
                postgres.get_host_port_ipv4(5432).await?
            ))
            .await?;
        let mut files = std::fs::read_dir(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../migrations/resource"),
        )?
        .map(|e| e.map(|e| e.path()))
        .collect::<Result<Vec<_>, _>>()?;
        files.sort();
        let mut ddl = "CREATE SCHEMA resource;SET search_path TO resource;\n".to_owned();
        for file in files {
            if file.extension().is_some_and(|e| e == "sql") {
                ddl.push_str(&std::fs::read_to_string(file)?);
                ddl.push('\n');
            }
        }
        sqlx::raw_sql(&ddl).execute(&pool).await?;
        let store = PgResourceStore::new(pool.clone());
        let now = store.current_time().await?;
        let project = ProjectId::new();
        let request = ResourceRequestId::new();
        store
            .create(
                &request.to_string(),
                &ResourceRequest {
                    id: request,
                    generation: 1,
                    request_key: request.to_string(),
                    requester_id: ActorId::new(),
                    project_id: project,
                    course_id: None,
                    target: ResourceTarget::Environment {
                        environment_id: EnvironmentId::new(),
                        release_id: ReleaseId::new(),
                        release_version: 1,
                    },
                    requested_resources: WorkloadResources {
                        cpu_millicores: 1,
                        memory_bytes: 1,
                        storage_bytes: 1,
                        gpu: None,
                    },
                    requested_duration_seconds: 60,
                    state: ResourceRequestState::Reviewing,
                    revision: Revision::new(1)?,
                    created_at: now,
                    updated_at: now,
                    diagnostic_code: None,
                },
                "synthetic-stalled-outbox",
            )
            .await?;
        let credits = crate::credits::PgCreditStore::new(pool.clone());
        let five = FixedDecimal::parse("5.000000")?;
        credits
            .admin(
                "stall-grant",
                ActorId::new(),
                &[PlatformRole::PlatformAdmin],
                &CreditAdminOperation::Grant {
                    project_id: project,
                    amount: five.clone(),
                    reason: "isolated stalled broker allowance".into(),
                },
            )
            .await?;
        let hold = credits
            .hold(
                "stall-hold",
                "environment",
                &CreditHoldRequest {
                    project_id: project,
                    request_id: request,
                    business_key: "stalled-observation".into(),
                    amount: five,
                },
            )
            .await?;
        credits
            .resolve(
                "stall-unknown",
                "environment",
                hold.id,
                &CreditHoldResolutionRequest::Unknown {
                    reason: "measurement unavailable".into(),
                },
            )
            .await?;
        sqlx::query("UPDATE resource.credit_holds SET unknown_since=date_trunc('milliseconds',clock_timestamp())-interval '1 hour 1 second',reconciliation_due_at=date_trunc('milliseconds',clock_timestamp())-interval '1 second' WHERE hold_id=$1").bind(hold.id).execute(&pool).await?;
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
        // A real core subscription receives the publish but deliberately withholds its ACK.
        let mut messages = client
            .subscribe(contracts::events::subjects::RESOURCE_REQUEST_SUBMITTED)
            .await?;
        client.flush().await?;
        let dispatcher =
            ResourceOutboxDispatcher::new(pool.clone(), client, std::time::Duration::from_secs(2))?;
        let (stop, shutdown) = watch::channel(false);
        let worker = tokio::spawn(run_dispatch_and_accounting(dispatcher, store, shutdown));
        tokio::time::timeout(std::time::Duration::from_secs(3), messages.next())
            .await?
            .ok_or("publish was not delivered to the fault boundary")?;
        let result = tokio::time::timeout(std::time::Duration::from_millis(750), async {
            loop {
                if credits.account(project).await?.reserved == FixedDecimal::zero() {
                    return Ok::<_, crate::credits::CreditError>(());
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await;
        stop.send(true)?;
        tokio::time::timeout(std::time::Duration::from_secs(5), worker).await???;
        assert!(
            result.is_ok(),
            "a withheld broker ACK delayed release of an already expired unknown hold"
        );
        result??;
        let published: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM resource.outbox_events WHERE published_at IS NOT NULL",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(published, 0);
        let account = credits.account(project).await?;
        assert_eq!(account.unknown_count, 1);
        assert_eq!(account.spent, FixedDecimal::zero());
        Ok(())
    }
}
