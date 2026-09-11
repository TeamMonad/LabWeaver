//! Production Control Service process entry point.

use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use artifact_store::{S3Credential, S3ImmutableObjectStore, S3StoreConfig};
use auth::{
    ServerTlsFileConfig, ServiceAuthConfig, ServiceAuthError, ServiceTokenClient,
    ServiceTokenClientConfig, ServiceTokenVerifier, TransportSecurityMode,
};
use control_service::api::{ApiState, authenticated_router};
use control_service::clients::{
    AccessClient, AgentClient, EnvironmentClient, EvaluationClient, ServiceHttpClientConfig,
};
use control_service::messaging::{
    AgentBuildConsumer, AgentRunConsumer, AuthoringPublicationConsumer, ControlOutboxDispatcher,
    connect_nats_mtls,
};
use control_service::{ControlConfig, ControlService};
use serde::Deserialize;
use sqlx::postgres::PgPoolOptions;
use time::OffsetDateTime;

#[path = "../../http_transport.rs"]
mod http_transport;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DeploymentFile {
    database_url_file: String,
    database_max_connections: u32,
    gateway_tls: ServerTlsFileConfig,
    access_service: ServiceHttpClientConfig,
    agent_service: ServiceHttpClientConfig,
    environment_service: ServiceHttpClientConfig,
    evaluation_service: ServiceHttpClientConfig,
    object_store: S3StoreConfig,
    object_store_access_key_file: String,
    object_store_secret_key_file: String,
    object_store_session_token_file: Option<String>,
    control: ControlConfig,
    cleanup_interval_seconds: u64,
    nats: NatsFileConfig,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct NatsFileConfig {
    server: String,
    ca_file: String,
    client_certificate_file: String,
    client_private_key_file: String,
    credentials_file: String,
    stream_name: String,
    consumer_name: String,
    quarantine_subject: String,
    build_consumer_name: String,
    build_quarantine_subject: String,
    authoring_stream_name: String,
    authoring_consumer_name: String,
    authoring_quarantine_subject: String,
    publish_timeout_milliseconds: u64,
    outbox_poll_milliseconds: u64,
}

#[tokio::main]
#[allow(clippy::too_many_lines)]
async fn main() -> Result<(), StartupError> {
    telemetry::init(env!("CARGO_PKG_NAME"))?;
    let deployment = load_deployment()?;
    validate_deployment(&deployment)?;
    let tls = http_transport::load_server_config(
        &deployment.gateway_tls.server_certificate_file,
        &deployment.gateway_tls.server_key_file,
    )?;
    let (service_token_verifier, service_token_client) = discover_service_auth().await?;
    let database_url = read_trimmed(&deployment.database_url_file)?;
    let pool = PgPoolOptions::new()
        .max_connections(deployment.database_max_connections)
        .connect(&database_url)
        .await?;
    verify_schema(&pool).await?;
    let objects = Arc::new(
        S3ImmutableObjectStore::new(
            deployment.object_store,
            S3Credential {
                access_key_id: read_trimmed(&deployment.object_store_access_key_file)?,
                secret_access_key: read_trimmed(&deployment.object_store_secret_key_file)?,
                session_token: deployment
                    .object_store_session_token_file
                    .as_deref()
                    .map(read_trimmed)
                    .transpose()?,
            },
        )
        .await?,
    );
    let service = ControlService::new(pool.clone(), objects, deployment.control)?;
    let access = AccessClient::new_authenticated(
        deployment.access_service,
        Arc::clone(&service_token_client),
    )?;
    let agent = AgentClient::new_authenticated(
        deployment.agent_service,
        Arc::clone(&service_token_client),
    )?;
    let environment = EnvironmentClient::new_authenticated(
        deployment.environment_service,
        Arc::clone(&service_token_client),
    )?;
    let evaluation = EvaluationClient::new_authenticated(
        deployment.evaluation_service,
        Arc::clone(&service_token_client),
    )?;
    let state = Arc::new(ApiState {
        control: service.clone(),
        access,
        agent: agent.clone(),
        environment,
        evaluation: evaluation.clone(),
        service_token_verifier: Arc::clone(&service_token_verifier),
    });
    let bind = SocketAddr::from_str(&deployment.gateway_tls.bind_addr)
        .map_err(|_| StartupError::Configuration)?;
    let listener = tokio::net::TcpListener::bind(bind).await?;
    let nats = connect_nats_mtls(
        &deployment.nats.server,
        deployment.nats.ca_file.into(),
        deployment.nats.client_certificate_file.into(),
        deployment.nats.client_private_key_file.into(),
        deployment.nats.credentials_file.into(),
    )
    .await?;
    let outbox = ControlOutboxDispatcher::new(
        pool,
        nats.clone(),
        Duration::from_millis(deployment.nats.publish_timeout_milliseconds),
    )?;
    let consumer = AgentRunConsumer::bind(
        nats.clone(),
        &deployment.nats.stream_name,
        &deployment.nats.consumer_name,
        &deployment.nats.quarantine_subject,
    )
    .await?;
    let build_consumer = AgentBuildConsumer::bind(
        nats.clone(),
        &deployment.nats.stream_name,
        &deployment.nats.build_consumer_name,
        &deployment.nats.build_quarantine_subject,
    )
    .await?;
    let authoring_consumer = AuthoringPublicationConsumer::bind(
        nats,
        &deployment.nats.authoring_stream_name,
        &deployment.nats.authoring_consumer_name,
        &deployment.nats.authoring_quarantine_subject,
    )
    .await?;
    let consumer_control = state.control.clone();
    let build_consumer_control = state.control.clone();
    let build_consumer_agent = agent.clone();
    let authoring_consumer_control = state.control.clone();
    let authoring_consumer_evaluation = evaluation;
    let interval = std::time::Duration::from_secs(deployment.cleanup_interval_seconds);
    let outbox_interval = Duration::from_millis(deployment.nats.outbox_poll_milliseconds);
    tokio::select! {
        result = http_transport::serve_tls(
            listener,
            authenticated_router(&state, &service_token_verifier),
            tls,
        ) => result?,
        result = cleanup_loop(service, interval) => result?,
        result = consumer_loop(consumer, consumer_control, agent) => result?,
        result = build_consumer_loop(build_consumer, build_consumer_control, build_consumer_agent) => result?,
        result = authoring_consumer_loop(authoring_consumer, authoring_consumer_control, authoring_consumer_evaluation) => result?,
        result = outbox_loop(outbox, outbox_interval) => result?,
    }
    Ok(())
}

async fn discover_service_auth()
-> Result<(Arc<ServiceTokenVerifier>, Arc<ServiceTokenClient>), StartupError> {
    let issuer = required_env("LABWEAVER_SERVICE_OIDC_ISSUER")?;
    let audience = required_env("LABWEAVER_SERVICE_AUDIENCE")?;
    let allowed_client_ids = required_set("LABWEAVER_SERVICE_ALLOWED_CLIENT_IDS")?;
    let scopes = required_set("LABWEAVER_SERVICE_SCOPES")?;
    let algorithms = required_set("LABWEAVER_SERVICE_JWT_ALGORITHMS")?;
    let jwks_refresh_seconds = required_u64("LABWEAVER_SERVICE_JWKS_REFRESH_SECONDS")?;
    let jwks_retry_seconds = required_u64("LABWEAVER_SERVICE_JWKS_RETRY_SECONDS")?;
    let refresh_skew_seconds = required_u64("LABWEAVER_SERVICE_TOKEN_REFRESH_SKEW_SECONDS")?;
    let client_id = required_env("LABWEAVER_SERVICE_CLIENT_ID")?;
    let client_secret = read_trimmed(&required_env("LABWEAVER_SERVICE_CLIENT_SECRET_FILE")?)?;
    let oidc_ca = std::fs::read(required_env("LABWEAVER_SERVICE_OIDC_CA")?)?;
    let transport = TransportSecurityMode::Strict;
    let http = auth::no_redirect_http_client(Some(&oidc_ca), transport)
        .map_err(|_| StartupError::ServiceAuth(ServiceAuthError::HttpClient))?;
    let verifier_config = ServiceAuthConfig::new(
        &issuer,
        audience.clone(),
        allowed_client_ids,
        BTreeSet::new(),
        algorithms,
        jwks_refresh_seconds,
        jwks_retry_seconds,
        transport,
    )
    .map_err(|_| StartupError::ServiceAuth(ServiceAuthError::InvalidConfig))?;
    let verifier = Arc::new(
        ServiceTokenVerifier::discover(verifier_config, http.clone())
            .await
            .map_err(StartupError::ServiceAuth)?,
    );
    let client_config = ServiceTokenClientConfig::new(
        &issuer,
        client_id,
        client_secret,
        audience,
        scopes,
        refresh_skew_seconds,
        transport,
    )
    .map_err(StartupError::ServiceToken)?;
    let client = Arc::new(
        ServiceTokenClient::discover(client_config, http)
            .await
            .map_err(StartupError::ServiceToken)?,
    );
    Ok((verifier, client))
}

async fn build_consumer_loop(
    mut consumer: AgentBuildConsumer,
    control: ControlService,
    agent: AgentClient,
) -> Result<(), StartupError> {
    loop {
        consumer.process_next(&control, &agent).await?;
    }
}

async fn consumer_loop(
    mut consumer: AgentRunConsumer,
    control: ControlService,
    agent: AgentClient,
) -> Result<(), StartupError> {
    loop {
        consumer.process_next(&control, &agent).await?;
    }
}

async fn authoring_consumer_loop(
    mut consumer: AuthoringPublicationConsumer,
    control: ControlService,
    evaluation: EvaluationClient,
) -> Result<(), StartupError> {
    loop {
        consumer.process_next(&control, &evaluation).await?;
    }
}

async fn outbox_loop(
    outbox: ControlOutboxDispatcher,
    interval: Duration,
) -> Result<(), StartupError> {
    if interval.is_zero() || interval > Duration::from_mins(1) {
        return Err(StartupError::Configuration);
    }
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        loop {
            match outbox.dispatch_once().await {
                Ok(true) => tracing::debug!(event = "control.outbox.published"),
                Ok(false) => break,
                Err(error) if error.retryable() => {
                    tracing::warn!(
                        event = "control.outbox.retry_scheduled",
                        diagnostic_code = error.diagnostic_code(),
                        error_kind = "message_publish",
                        failure_stage = "outbox_publish",
                        retryable = true,
                        retry_after_ms = interval.as_millis()
                    );
                    break;
                }
                Err(error) => return Err(error.into()),
            }
        }
    }
}

fn load_deployment() -> Result<DeploymentFile, StartupError> {
    let path =
        std::env::var("LABWEAVER_CONTROL_CONFIG_FILE").map_err(|_| StartupError::Configuration)?;
    let content = std::fs::read_to_string(path)?;
    serde_yaml::from_str(&content).map_err(|_| StartupError::Configuration)
}

fn required_env(name: &'static str) -> Result<String, StartupError> {
    std::env::var(name).map_err(|_| StartupError::Configuration)
}

fn required_u64(name: &'static str) -> Result<u64, StartupError> {
    required_env(name)?
        .parse::<u64>()
        .map_err(|_| StartupError::Configuration)
}

fn required_set(name: &'static str) -> Result<BTreeSet<String>, StartupError> {
    let values = required_env(name)?
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    if values.is_empty() {
        Err(StartupError::Configuration)
    } else {
        Ok(values)
    }
}

fn validate_deployment(deployment: &DeploymentFile) -> Result<(), StartupError> {
    if deployment.database_max_connections == 0
        || deployment.database_max_connections > 100
        || deployment.cleanup_interval_seconds == 0
        || deployment.cleanup_interval_seconds > 3_600
        || deployment.nats.publish_timeout_milliseconds == 0
        || deployment.nats.publish_timeout_milliseconds > 300_000
        || deployment.nats.outbox_poll_milliseconds == 0
        || deployment.nats.outbox_poll_milliseconds > 60_000
        || deployment.control.package_object_prefix.trim_matches('/')
            != deployment.object_store.object_prefix.trim_matches('/')
    {
        return Err(StartupError::Configuration);
    }
    Ok(())
}

fn read_trimmed(path: &str) -> Result<String, StartupError> {
    let value = std::fs::read_to_string(path)?;
    let value = value.trim();
    if value.is_empty() {
        return Err(StartupError::Configuration);
    }
    Ok(value.to_owned())
}

async fn verify_schema(pool: &sqlx::PgPool) -> Result<(), StartupError> {
    let ready: bool = sqlx::query_scalar(
        "SELECT to_regclass('control.problem_packages') IS NOT NULL \
         AND to_regclass('control.projects') IS NOT NULL \
         AND to_regclass('control.sse_course_cursors') IS NOT NULL \
         AND to_regclass('control.sse_project_cursors') IS NOT NULL \
         AND to_regclass('control.sse_project_events') IS NOT NULL \
         AND to_regclass('control.image_artifact_projections') IS NOT NULL \
         AND to_regclass('control.container_build_projections') IS NOT NULL \
         AND to_regclass('control.authoring_approval_publications') IS NOT NULL",
    )
    .fetch_one(pool)
    .await?;
    if !ready {
        return Err(StartupError::SchemaUnavailable);
    }
    Ok(())
}

async fn cleanup_loop(
    service: ControlService,
    interval: std::time::Duration,
) -> Result<(), StartupError> {
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        let value = OffsetDateTime::now_utc();
        let value = value
            .replace_nanosecond((value.nanosecond() / 1_000_000) * 1_000_000)
            .map_err(|_| StartupError::Clock)?;
        let now = contracts::UtcTimestamp::from_utc(value).map_err(|_| StartupError::Clock)?;
        let outcome = service.cleanup_one_object(now).await?;
        let purged = service.purge_expired_sse(now).await?;
        let cleanup = match outcome {
            control_service::CleanupOutcome::Idle => "idle",
            control_service::CleanupOutcome::Deleted => "deleted",
            control_service::CleanupOutcome::RetryScheduled { .. } => "retry_scheduled",
        };
        tracing::debug!(
            event = "control.maintenance.completed",
            cleanup,
            sse_purged = purged
        );
    }
}

#[derive(Debug, thiserror::Error)]
enum StartupError {
    #[error("LW_CONTROL_CONFIG_INVALID")]
    Configuration,
    #[error("LW_CONTROL_SCHEMA_UNAVAILABLE")]
    SchemaUnavailable,
    #[error("LW_CONTROL_CLOCK_INVALID")]
    Clock,
    #[error("LW_CONTROL_SERVICE_AUTH_INVALID")]
    ServiceAuth(#[source] ServiceAuthError),
    #[error("LW_CONTROL_SERVICE_TOKEN_INVALID")]
    ServiceToken(#[source] auth::ServiceTokenClientError),
    #[error(transparent)]
    HttpTransport(#[from] http_transport::HttpTransportError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Database(#[from] sqlx::Error),
    #[error(transparent)]
    Telemetry(#[from] telemetry::TelemetryError),
    #[error(transparent)]
    ObjectStore(#[from] artifact_store::ObjectStoreError),
    #[error(transparent)]
    Control(#[from] control_service::ControlError),
    #[error(transparent)]
    Downstream(#[from] control_service::clients::DownstreamError),
    #[error(transparent)]
    Messaging(#[from] control_service::messaging::MessagingError),
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::case_sensitive_file_extension_comparisons)]
mod deployment_contract_tests {
    use super::DeploymentFile;

    #[test]
    fn checked_in_platform_example_matches_the_runtime_contract() {
        let example = include_str!("../../../deploy/config/control-plane.yaml.example");
        let deployment: DeploymentFile =
            serde_yaml::from_str(example).expect("control deployment example must deserialize");

        assert!(deployment.database_url_file.starts_with('/'));
        assert!(deployment.nats.server.starts_with("tls://"));
        assert!(deployment.nats.build_consumer_name.ends_with("-v1"));
        assert!(deployment.nats.authoring_consumer_name.ends_with("-v1"));
        assert!(
            deployment
                .nats
                .quarantine_subject
                .starts_with("labweaver.agent.quarantine.control_")
        );
        assert!(
            deployment
                .nats
                .build_quarantine_subject
                .starts_with("labweaver.agent.quarantine.control_")
        );
        assert_ne!(
            deployment.nats.quarantine_subject,
            deployment.nats.build_quarantine_subject
        );
        assert!(
            deployment
                .nats
                .authoring_quarantine_subject
                .starts_with("labweaver.control.quarantine.")
        );
        assert!(!example.contains(".v2"));
    }
}
