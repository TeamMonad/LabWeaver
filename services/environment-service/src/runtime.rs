use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use sqlx::postgres::PgPoolOptions;
use tokio::net::TcpListener;

use crate::{
    MtlsConfig, MtlsServerError, OwnerResolver, OwnerResolverPolicy, PgEnvironmentStore,
    owner_resolver_router, serve_owner_resolver_mtls,
};

const DATABASE_URL: &str = "LABWEAVER_DATABASE_URL";
const BIND_ADDRESS: &str = "LABWEAVER_OWNER_RESOLVER_BIND_ADDR";
const CLIENT_CA_PATH: &str = "LABWEAVER_OWNER_RESOLVER_CLIENT_CA_PATH";
const SERVER_CERTIFICATE_PATH: &str = "LABWEAVER_OWNER_RESOLVER_SERVER_CERT_PATH";
const SERVER_PRIVATE_KEY_PATH: &str = "LABWEAVER_OWNER_RESOLVER_SERVER_KEY_PATH";
const ALLOWED_CALLER_SANS: &str = "LABWEAVER_OWNER_RESOLVER_ALLOWED_CALLER_SANS";

/// Fully initialized owner-resolver runtime; construction fails before any port reports ready.
pub struct OwnerResolverRuntime {
    listener: TcpListener,
    router: axum::Router,
    tls: MtlsConfig,
}

impl OwnerResolverRuntime {
    /// Loads explicit environment configuration, connects `PostgreSQL`, and binds the mTLS port.
    pub async fn from_env() -> Result<Self, OwnerResolverRuntimeError> {
        let database_url = required(DATABASE_URL)?;
        let bind_address = SocketAddr::from_str(&required(BIND_ADDRESS)?)
            .map_err(|_| OwnerResolverRuntimeError::Configuration(BIND_ADDRESS))?;
        let client_ca_path = required_path(CLIENT_CA_PATH)?;
        let server_certificate_path = required_path(SERVER_CERTIFICATE_PATH)?;
        let server_private_key_path = required_path(SERVER_PRIVATE_KEY_PATH)?;
        let allowed_sans = required(ALLOWED_CALLER_SANS)?
            .split(',')
            .map(str::trim)
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let policy = OwnerResolverPolicy::new(allowed_sans)
            .map_err(|_| OwnerResolverRuntimeError::Configuration(ALLOWED_CALLER_SANS))?;

        let client_ca = read_secret(&client_ca_path)?;
        let server_certificate = read_secret(&server_certificate_path)?;
        let server_private_key = read_secret(&server_private_key_path)?;
        let tls = MtlsConfig::from_pem(&client_ca, &server_certificate, &server_private_key)?;
        let pool = PgPoolOptions::new()
            .max_connections(8)
            .connect(&database_url)
            .await
            .map_err(OwnerResolverRuntimeError::Database)?;
        let schema_ready: bool = sqlx::query_scalar(
            "SELECT to_regclass('environment.environment_instances') IS NOT NULL",
        )
        .fetch_one(&pool)
        .await
        .map_err(OwnerResolverRuntimeError::Database)?;
        if !schema_ready {
            return Err(OwnerResolverRuntimeError::SchemaUnavailable);
        }
        let listener = TcpListener::bind(bind_address)
            .await
            .map_err(OwnerResolverRuntimeError::Bind)?;
        let resolver = OwnerResolver::new(PgEnvironmentStore::new(pool), policy);
        Ok(Self {
            listener,
            router: owner_resolver_router(resolver),
            tls,
        })
    }

    /// Serves until SIGINT/SIGTERM; every request has passed client-certificate verification.
    pub async fn serve(self) -> Result<(), OwnerResolverRuntimeError> {
        let address = self
            .listener
            .local_addr()
            .map_err(OwnerResolverRuntimeError::Bind)?;
        tracing::info!(
            event = "environment.owner_resolver.started",
            %address,
            transport = "mtls"
        );
        serve_owner_resolver_mtls(self.listener, self.router, self.tls, shutdown_signal()).await?;
        tracing::info!(event = "environment.owner_resolver.stopped");
        Ok(())
    }
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate());
        let Ok(mut terminate) = terminate else {
            tracing::error!(event = "environment.owner_resolver.shutdown_signal_failed");
            return;
        };
        tokio::select! {
            result = tokio::signal::ctrl_c() => {
                if let Err(error) = result {
                    tracing::error!(event = "environment.owner_resolver.shutdown_signal_failed", %error);
                }
            }
            _ = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    if let Err(error) = tokio::signal::ctrl_c().await {
        tracing::error!(event = "environment.owner_resolver.shutdown_signal_failed", %error);
    }
}

fn required(name: &'static str) -> Result<String, OwnerResolverRuntimeError> {
    let value = std::env::var(name).map_err(|_| OwnerResolverRuntimeError::Configuration(name))?;
    if value.trim().is_empty() {
        return Err(OwnerResolverRuntimeError::Configuration(name));
    }
    Ok(value)
}

fn required_path(name: &'static str) -> Result<PathBuf, OwnerResolverRuntimeError> {
    let path = PathBuf::from(required(name)?);
    if !path.is_absolute() {
        return Err(OwnerResolverRuntimeError::Configuration(name));
    }
    Ok(path)
}

fn read_secret(path: &Path) -> Result<Vec<u8>, OwnerResolverRuntimeError> {
    std::fs::read(path).map_err(|_| OwnerResolverRuntimeError::SecretRead)
}

/// Stable startup failures which never expose database URLs, paths, or certificate contents.
#[derive(Debug, thiserror::Error)]
pub enum OwnerResolverRuntimeError {
    #[error("LW_ENV_OWNER_RUNTIME_CONFIGURATION_INVALID: {0}")]
    Configuration(&'static str),
    #[error("LW_ENV_OWNER_RUNTIME_SECRET_READ_FAILED")]
    SecretRead,
    #[error("LW_ENV_OWNER_RUNTIME_DATABASE_UNAVAILABLE")]
    Database(#[source] sqlx::Error),
    #[error("LW_ENV_OWNER_RUNTIME_SCHEMA_UNAVAILABLE")]
    SchemaUnavailable,
    #[error("LW_ENV_OWNER_RUNTIME_BIND_FAILED")]
    Bind(#[source] std::io::Error),
    #[error(transparent)]
    Tls(#[from] MtlsServerError),
}
