use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;

use auth::{
    ServiceAuthConfig, ServiceAuthError, ServiceTokenClient, ServiceTokenClientConfig,
    ServiceTokenClientError, ServiceTokenVerifier, TransportSecurityMode,
};
use sqlx::postgres::PgPoolOptions;
use tokio::net::TcpListener;

use crate::{
    EnvironmentApiState, OwnerResolver, PgEnvironmentStore, PgReleaseProjectionStore,
    TerminalBridgeError, TerminalExecutorGateway, environment_api_router, owner_resolver_router,
    terminal_bridge_router, with_service_auth,
};

const DATABASE_URL: &str = "LABWEAVER_DATABASE_URL";
const BIND_ADDRESS: &str = "LABWEAVER_OWNER_RESOLVER_BIND_ADDR";
const SERVER_CERTIFICATE_PATH: &str = "LABWEAVER_OWNER_RESOLVER_SERVER_CERT_PATH";
const SERVER_PRIVATE_KEY_PATH: &str = "LABWEAVER_OWNER_RESOLVER_SERVER_KEY_PATH";
const SERVICE_OIDC_ISSUER: &str = "LABWEAVER_SERVICE_OIDC_ISSUER";
const SERVICE_OIDC_CA_PATH: &str = "LABWEAVER_SERVICE_OIDC_CA";
const SERVICE_AUDIENCE: &str = "LABWEAVER_SERVICE_AUDIENCE";
const SERVICE_ALLOWED_CLIENT_IDS: &str = "LABWEAVER_SERVICE_ALLOWED_CLIENT_IDS";
const SERVICE_JWT_ALGORITHMS: &str = "LABWEAVER_SERVICE_JWT_ALGORITHMS";
const SERVICE_JWKS_REFRESH_SECONDS: &str = "LABWEAVER_SERVICE_JWKS_REFRESH_SECONDS";
const SERVICE_JWKS_RETRY_SECONDS: &str = "LABWEAVER_SERVICE_JWKS_RETRY_SECONDS";
const SERVICE_CLIENT_ID: &str = "LABWEAVER_SERVICE_CLIENT_ID";
const SERVICE_CLIENT_SECRET_FILE: &str = "LABWEAVER_SERVICE_CLIENT_SECRET_FILE";
const SERVICE_SCOPES: &str = "LABWEAVER_SERVICE_SCOPES";
const SERVICE_TOKEN_REFRESH_SKEW_SECONDS: &str = "LABWEAVER_SERVICE_TOKEN_REFRESH_SKEW_SECONDS";

/// Fully initialized owner-resolver runtime; construction fails before any port reports ready.
pub struct OwnerResolverRuntime {
    listener: TcpListener,
    router: axum::Router,
    tls: Arc<rustls::ServerConfig>,
}

impl OwnerResolverRuntime {
    /// Loads explicit environment configuration, connects `PostgreSQL`, and binds the TLS port.
    pub async fn from_env(api: EnvironmentApiState) -> Result<Self, OwnerResolverRuntimeError> {
        let database_url = required(DATABASE_URL)?;
        let bind_address = SocketAddr::from_str(&required(BIND_ADDRESS)?)
            .map_err(|_| OwnerResolverRuntimeError::Configuration(BIND_ADDRESS))?;
        let server_certificate_path = required_path(SERVER_CERTIFICATE_PATH)?;
        let server_private_key_path = required_path(SERVER_PRIVATE_KEY_PATH)?;
        let tls = crate::http_transport::load_server_config(
            &server_certificate_path,
            &server_private_key_path,
        )?;
        let (service_verifier, service_token_client) = discover_service_auth().await?;
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
        let resolver = OwnerResolver::new(
            PgEnvironmentStore::new(pool.clone()),
            PgReleaseProjectionStore::new(pool),
        );
        let console_gateway = TerminalExecutorGateway::from_env(service_token_client)?;
        let router = owner_resolver_router(resolver)
            .merge(environment_api_router(api.clone()))
            .merge(terminal_bridge_router(api, console_gateway));
        Ok(Self {
            listener,
            router: with_service_auth(router, service_verifier),
            tls,
        })
    }

    /// Serves the owner, public Environment, and console routes over TLS.
    pub async fn serve(self) -> Result<(), OwnerResolverRuntimeError> {
        let address = self
            .listener
            .local_addr()
            .map_err(OwnerResolverRuntimeError::Bind)?;
        tracing::info!(
            event = "environment.owner_resolver.started",
            %address,
            transport = "tls",
            identity = "jwt"
        );
        crate::http_transport::serve_tls(self.listener, self.router, self.tls).await?;
        tracing::info!(event = "environment.owner_resolver.stopped");
        Ok(())
    }
}

async fn discover_service_auth()
-> Result<(Arc<ServiceTokenVerifier>, Arc<ServiceTokenClient>), OwnerResolverRuntimeError> {
    let issuer = required(SERVICE_OIDC_ISSUER)?;
    let audience = required(SERVICE_AUDIENCE)?;
    let allowed_client_ids = required_set(SERVICE_ALLOWED_CLIENT_IDS)?;
    let algorithms = required_set(SERVICE_JWT_ALGORITHMS)?;
    let jwks_refresh_seconds = required_u64(SERVICE_JWKS_REFRESH_SECONDS)?;
    let jwks_retry_seconds = required_u64(SERVICE_JWKS_RETRY_SECONDS)?;
    let client_id = required(SERVICE_CLIENT_ID)?;
    let client_secret = read_text_secret(&required_path(SERVICE_CLIENT_SECRET_FILE)?)?;
    let scopes = required_set(SERVICE_SCOPES)?;
    let refresh_skew_seconds = required_u64(SERVICE_TOKEN_REFRESH_SKEW_SECONDS)?;
    let ca = read_secret(&required_path(SERVICE_OIDC_CA_PATH)?)?;
    let http = auth::no_redirect_http_client(Some(&ca), TransportSecurityMode::Strict)
        .map_err(|_| OwnerResolverRuntimeError::ServiceAuth(ServiceAuthError::HttpClient))?;
    let config = ServiceAuthConfig::new(
        &issuer,
        audience.clone(),
        allowed_client_ids,
        BTreeSet::new(),
        algorithms,
        jwks_refresh_seconds,
        jwks_retry_seconds,
        TransportSecurityMode::Strict,
    )
    .map_err(|_| OwnerResolverRuntimeError::ServiceAuth(ServiceAuthError::InvalidConfig))?;
    let verifier = ServiceTokenVerifier::discover(config, http.clone())
        .await
        .map(Arc::new)
        .map_err(OwnerResolverRuntimeError::ServiceAuth)?;
    let client_config = ServiceTokenClientConfig::new(
        &issuer,
        client_id,
        client_secret,
        audience,
        scopes,
        refresh_skew_seconds,
        TransportSecurityMode::Strict,
    )
    .map_err(OwnerResolverRuntimeError::ServiceToken)?;
    let client = ServiceTokenClient::discover(client_config, http)
        .await
        .map(Arc::new)
        .map_err(OwnerResolverRuntimeError::ServiceToken)?;
    Ok((verifier, client))
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

fn required_set(name: &'static str) -> Result<BTreeSet<String>, OwnerResolverRuntimeError> {
    let values = required(name)?
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    if values.is_empty() {
        return Err(OwnerResolverRuntimeError::Configuration(name));
    }
    Ok(values)
}

fn required_u64(name: &'static str) -> Result<u64, OwnerResolverRuntimeError> {
    let value = required(name)?
        .parse::<u64>()
        .map_err(|_| OwnerResolverRuntimeError::Configuration(name))?;
    if value == 0 {
        return Err(OwnerResolverRuntimeError::Configuration(name));
    }
    Ok(value)
}

fn read_secret(path: &Path) -> Result<Vec<u8>, OwnerResolverRuntimeError> {
    std::fs::read(path).map_err(|_| OwnerResolverRuntimeError::SecretRead)
}

fn read_text_secret(path: &Path) -> Result<String, OwnerResolverRuntimeError> {
    let value =
        String::from_utf8(read_secret(path)?).map_err(|_| OwnerResolverRuntimeError::SecretRead)?;
    let value = value.trim();
    if value.is_empty() || value.bytes().any(|byte| byte.is_ascii_control()) {
        return Err(OwnerResolverRuntimeError::SecretRead);
    }
    Ok(value.to_owned())
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
    HttpTransport(#[from] crate::http_transport::HttpTransportError),
    #[error(transparent)]
    ServiceAuth(#[from] ServiceAuthError),
    #[error(transparent)]
    ServiceToken(#[from] ServiceTokenClientError),
    #[error(transparent)]
    Console(#[from] TerminalBridgeError),
}
