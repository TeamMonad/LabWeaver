//! Dependency-aware Resource Service process runtime.

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
use crate::store::PgResourceStore;

const DATABASE_URL: &str = "LABWEAVER_DATABASE_URL";
const NATS_SERVER: &str = "LABWEAVER_NATS_SERVER";
const NATS_CA_PATH: &str = "LABWEAVER_NATS_CA_PATH";
const NATS_CLIENT_CERTIFICATE_PATH: &str = "LABWEAVER_NATS_CLIENT_CERT_PATH";
const NATS_CLIENT_PRIVATE_KEY_PATH: &str = "LABWEAVER_NATS_CLIENT_KEY_PATH";
const NATS_CREDENTIALS_PATH: &str = "LABWEAVER_NATS_CREDENTIALS_PATH";
const LEASE_VERIFICATION_SUBJECT: &str = "LABWEAVER_RESOURCE_LEASE_VERIFICATION_SUBJECT";
const CAPACITY_CONFIG_FILE: &str = "LABWEAVER_RESOURCE_CAPACITY_CONFIG_FILE";
const OUTBOX_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
const OUTBOX_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);

/// Production dependency graph for Resource-owned Lease authorization.
pub struct ResourceProcessRuntime {
    responder: NatsLeaseVerificationResponder,
    store: PgResourceStore,
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
        let store = PgResourceStore::new(pool);
        let outbox = ResourceOutboxDispatcher::new(store.pool(), client, OUTBOX_TIMEOUT)
            .map_err(ResourceProcessRuntimeError::Outbox)?;
        let capacity_worker = capacity_configuration
            .build_worker(store.clone())
            .map_err(ResourceProcessRuntimeError::CapacityProvider)?;
        let (shutdown_sender, shutdown) = watch::channel(false);
        Ok(Self {
            responder,
            store,
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

    /// Keeps the responder live. A failed authoritative dependency flips readiness false.
    pub async fn run(self) -> Result<(), ResourceProcessRuntimeError> {
        let Self {
            responder,
            store,
            capacity_worker,
            outbox,
            readiness,
            _shutdown_sender,
            shutdown,
        } = self;
        let responder_shutdown = shutdown.clone();
        let outbox_shutdown = shutdown.clone();
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
                run_outbox(outbox, outbox_shutdown)
                    .await
                    .map_err(ResourceProcessRuntimeError::Outbox)
            },
        )
        .map(|_| ());
        if result.is_err() {
            readiness.store(false, Ordering::Release);
        }
        result.map_err(Into::into)
    }
}

async fn run_outbox(
    outbox: ResourceOutboxDispatcher,
    mut shutdown: watch::Receiver<bool>,
) -> Result<(), ResourceOutboxError> {
    let mut interval = tokio::time::interval(OUTBOX_INTERVAL);
    loop {
        tokio::select! {
            changed = shutdown.changed() => { if changed.is_err() || *shutdown.borrow() { return Ok(()); } }
            _ = interval.tick() => { let _ = outbox.dispatch_once().await?; }
        }
    }
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
}
