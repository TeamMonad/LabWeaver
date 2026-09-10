//! Dependency-aware Resource Service process runtime.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use sqlx::postgres::PgPoolOptions;
use tokio::sync::watch;

use crate::capacity::{
    CapacityProviderError, CapacityReconcileWorker, ResourceCapacityConfiguration,
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
const EVALUATION_SERVICE_CLIENT_ID: &str = "LABWEAVER_EVALUATION_SERVICE_CLIENT_ID";
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
    evaluation_service_client_id: String,
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
            sqlx::query_scalar("SELECT to_regclass('resource.resource_leases')::text")
                .fetch_one(&pool)
                .await?;
        if schema_ready.is_none() {
            return Err(ResourceProcessRuntimeError::SchemaMissing);
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
        let capacity_configuration: ResourceCapacityConfiguration = serde_json::from_slice(
            &std::fs::read(required_path(CAPACITY_CONFIG_FILE)?)
                .map_err(|_| ResourceProcessRuntimeError::CapacityConfiguration)?,
        )
        .map_err(|_| ResourceProcessRuntimeError::CapacityConfiguration)?;
        let service_verifier = discover_service_verifier().await?;
        let environment_token_client = discover_environment_token_client().await?;
        let access_service_client_id = required(ACCESS_SERVICE_CLIENT_ID)?;
        let environment_service_client_id = required(ENVIRONMENT_SERVICE_CLIENT_ID)?;
        let evaluation_service_client_id = required(EVALUATION_SERVICE_CLIENT_ID)?;
        let store = PgResourceStore::new(pool);
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
            evaluation_service_client_id,
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
            .with_evaluation_service_client_id(self.evaluation_service_client_id.clone())
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
            evaluation_service_client_id: _evaluation_service_client_id,
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
                run_outbox(outbox, settlement_store, outbox_shutdown)
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

async fn run_outbox(
    outbox: ResourceOutboxDispatcher,
    store: PgResourceStore,
    mut shutdown: watch::Receiver<bool>,
) -> Result<(), ResourceOutboxError> {
    let mut interval = tokio::time::interval(OUTBOX_INTERVAL);
    loop {
        tokio::select! {
            changed = shutdown.changed() => { if changed.is_err() || *shutdown.borrow() { return Ok(()); } }
            _ = interval.tick() => {
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

#[derive(Debug, thiserror::Error)]
pub enum ResourceProcessRuntimeError {
    #[error("LW_RESOURCE_CONFIG_MISSING: {0}")]
    MissingConfiguration(&'static str),
    #[error("LW_RESOURCE_CONFIG_PATH_INVALID: {0}")]
    PathConfiguration(&'static str),
    #[error("LW_RESOURCE_SCHEMA_MISSING")]
    SchemaMissing,
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
