//! Access Service browser BFF entry points.

mod console;
mod grants;
#[path = "../../http_transport.rs"]
mod http_transport;
mod proxy;

use std::{collections::BTreeSet, net::SocketAddr, str::FromStr, sync::Arc};

use auth::{
    AccessAuthFile, AuthConfig, AuthorizationContext, BffSession, EnvironmentOwnerResolverClient,
    KeyRing, OidcProvider, OidcTransaction, RoleMappings, ServiceAuthConfig, ServiceAuthError,
    ServiceIdentity, ServiceTokenClient, ServiceTokenClientConfig, ServiceTokenVerifier,
    TransportSecurityMode, authorize, build_backchannel_logout_authorizer, build_bearer_authorizer,
    cleanup_expired_auth_state, consume_backchannel_logout, consume_oidc_transaction,
    create_bff_session, extract_platform_roles, load_bff_session, load_logout_hint,
    load_membership_snapshot, no_redirect_http_client, revoke_bff_session, upsert_actor,
};
use axum::{
    Json, Router,
    extract::{Form, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Redirect, Response},
    routing::{delete, get, post},
};
use contracts::{
    AuthSession, AuthenticatedActor, AuthorizationDecision, AuthorizationDecisionRequest,
    AuthorizationScope, CsrfTokenResponse, OperationScopeKind, Revision, UtcTimestamp,
    environment::EnvironmentOwnerResolutionRequest, operation_contract,
};
use persistence_sqlx::Sha256Digest;

use serde::Deserialize;
use sqlx::{PgPool, postgres::PgPoolOptions};
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

#[derive(Clone)]
struct AppState {
    config: AuthConfig,
    deployment: AccessAuthFile,
    provider: OidcProvider,
    oidc_http: reqwest::Client,
    bearer_authorizer: Arc<jwt_authorizer::Authorizer<auth::BearerClaims>>,
    backchannel_logout_authorizer: Arc<jwt_authorizer::Authorizer<auth::BackchannelLogoutClaims>>,
    service_token_verifier: Arc<ServiceTokenVerifier>,
    role_mappings: RoleMappings,
    pool: PgPool,
    key_ring: KeyRing,
    owner_resolver: EnvironmentOwnerResolverClient,
    console_gateway: console::ConsoleGateway,
    console_registry: console::ConsoleRegistry,
    console_proxy_owner: String,
    control_proxy: proxy::ControlGatewayProxy,
    environment_proxy: proxy::ControlGatewayProxy,
    evaluation_proxy: proxy::ControlGatewayProxy,
    resource_proxy: proxy::ResourceGatewayProxy,
    runtime_proxy: proxy::RuntimeGatewayProxy,
    metrics: telemetry::PrometheusHandle,
    nats: async_nats::Client,
}

/// Explicit audience and permission set for one internal downstream.
#[derive(Clone, Debug)]
pub(crate) struct ServiceTokenTarget {
    pub(crate) audience: String,
    pub(crate) scopes: BTreeSet<String>,
}

#[tokio::main]
async fn main() -> Result<(), StartupError> {
    telemetry::init(env!("CARGO_PKG_NAME"))?;
    let metrics = telemetry::init_metrics(env!("CARGO_PKG_NAME"))?;
    let deployment = load_deployment()?;
    let bind =
        SocketAddr::from_str(&deployment.browser.bind_addr).map_err(|_| StartupError::Config)?;
    let internal_bind = SocketAddr::from_str(&deployment.internal_tls.bind_addr)
        .map_err(|_| StartupError::Config)?;
    let internal_tls = http_transport::load_server_config(
        &deployment.internal_tls.server_certificate_file,
        &deployment.internal_tls.server_key_file,
    )
    .map_err(StartupError::HttpTransport)?;
    let state = build_app_state(deployment, metrics).await?;
    let router = browser_router(Arc::clone(&state));
    let internal_router = internal_router(Arc::clone(&state));
    let listener = tokio::net::TcpListener::bind(bind).await?;
    let internal_listener = tokio::net::TcpListener::bind(internal_bind).await?;
    let result = tokio::select! {
        result = axum::serve(listener, router) => result.map_err(StartupError::from),
        result = serve_internal(internal_listener, internal_router, internal_tls) => result,
        result = auth_cleanup_loop(Arc::clone(&state)) => result,
        result = grants::activation_loop(Arc::clone(&state)) => result.map_err(StartupError::from),
        result = grants::maintenance_loop(Arc::clone(&state)) => result.map_err(StartupError::from),
        result = grants::outbox_loop(Arc::clone(&state)) => result.map_err(StartupError::from),
        result = grants::environment_revocation_loop(Arc::clone(&state)) => result.map_err(StartupError::from),
        result = grants::environment_state_loop(Arc::clone(&state)) => result.map_err(StartupError::from),
    };
    state.console_registry.cancel_all().await;
    result?;
    Ok(())
}

#[allow(
    clippy::too_many_lines,
    reason = "the browser surface is intentionally enumerated in one auditable router"
)]
fn browser_router(state: Arc<AppState>) -> Router {
    let router = browser_routes().with_state(state);
    telemetry::instrument_http(router, "access-service", "browser-api")
}

#[allow(
    clippy::too_many_lines,
    reason = "the browser surface is intentionally enumerated in one auditable router"
)]
fn browser_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/auth/login", get(login))
        .route("/auth/callback", get(callback))
        .route("/auth/backchannel-logout", post(backchannel_logout))
        .route("/auth/logout", post(logout))
        .route("/api/v1/auth/session", get(session))
        .route("/api/v1/auth/csrf", get(csrf))
        .route(
            "/api/v1/me/ssh-public-keys",
            post(grants::create_ssh_key).get(grants::list_ssh_keys),
        )
        .route(
            "/api/v1/me/ssh-public-keys/{key_id}",
            delete(grants::delete_ssh_key),
        )
        .route(
            "/api/v1/environments/{environment_id}/access-grants",
            post(grants::create_access_grant).get(grants::list_access_grants),
        )
        .route(
            "/api/v1/access-grants/{grant_id}",
            get(grants::get_access_grant),
        )
        .route(
            "/api/v1/access-grants/{grant_id}/renew",
            post(grants::renew_access_grant),
        )
        .route(
            "/api/v1/access-grants/{grant_id}/revoke",
            post(grants::revoke_access_grant),
        )
        .route(
            "/api/v1/access-grants/{grant_id}/console-capabilities",
            get(console::list_capabilities).post(console::issue_capability),
        )
        .route("/connect/console/{opaque}", get(console::connect))
        .route(
            "/api/v1/environments",
            axum::routing::any(proxy::forward_environment),
        )
        .route(
            "/api/v1/environments/{environment_id}/freeze",
            post(proxy::forward_evaluation),
        )
        .route(
            "/api/v1/frozen-submissions/{submission_id}",
            get(proxy::forward_evaluation),
        )
        .route(
            "/api/v1/courses/{course_id}/me/evaluation-results",
            get(proxy::forward_evaluation),
        )
        .route(
            "/api/v1/courses/{course_id}/me/evaluation-results/{run_id}",
            get(proxy::forward_evaluation),
        )
        .merge(control_browser_router())
        .route(
            "/api/v1/environments/{environment_id}",
            axum::routing::any(proxy::forward_environment),
        )
        .route(
            "/api/v1/environments/{environment_id}/start",
            post(proxy::forward_environment),
        )
        .route(
            "/api/v1/environments/{environment_id}/stop",
            post(proxy::forward_environment),
        )
        .route(
            "/api/v1/environments/{environment_id}/restart",
            post(proxy::forward_environment),
        )
        .route(
            "/api/v1/environments/{environment_id}/cancel",
            post(proxy::forward_environment),
        )
        .route(
            "/api/v1/environments/{environment_id}/recover",
            post(proxy::forward_environment),
        )
        .route(
            "/api/v1/environments/{environment_id}/reset",
            post(proxy::forward_environment),
        )
        .route(
            "/api/v1/environments/{environment_id}/retry",
            post(proxy::forward_environment),
        )
        .route(
            "/api/v1/environments/{environment_id}/endpoints",
            get(proxy::forward_environment),
        )
        .route(
            "/api/v1/environments/{environment_id}/operations",
            get(proxy::forward_environment),
        )
        .route(
            "/api/v1/environments/{environment_id}/operations/{operation_id}",
            get(proxy::forward_environment),
        )
        .route(
            "/connect/{endpoint_grant_id}/",
            axum::routing::any(proxy::forward_runtime),
        )
        .route(
            "/connect/{endpoint_grant_id}/{*runtime_path}",
            axum::routing::any(proxy::forward_runtime),
        )
        .merge(resource_browser_router())
}

fn resource_browser_router() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/api/v1/resource-requests",
            axum::routing::any(proxy::forward_resource),
        )
        .route(
            "/api/v1/resource-requests/{request_id}",
            axum::routing::any(proxy::forward_resource),
        )
        .route(
            "/api/v1/resource-requests/{request_id}/{action}",
            post(proxy::forward_resource),
        )
        .route("/api/v1/resource-leases", get(proxy::forward_resource))
        .route(
            "/api/v1/resource-leases/{lease_id}",
            get(proxy::forward_resource),
        )
        .route(
            "/api/v1/resource-leases/{lease_id}/{action}",
            post(proxy::forward_resource),
        )
        .route(
            "/api/v1/projects/{project_id}/resource-requests",
            get(proxy::forward_resource).post(proxy::forward_resource),
        )
        .route(
            "/api/v1/projects/{project_id}/resource-leases",
            get(proxy::forward_resource),
        )
        .route(
            "/api/v1/resource/gpu-catalog",
            get(proxy::forward_resource).post(proxy::forward_resource),
        )
        .route(
            "/api/v1/resource/rates",
            get(proxy::forward_resource).post(proxy::forward_resource),
        )
        .route("/api/v1/resource/usage", post(proxy::forward_resource))
        .route(
            "/api/v1/projects/{project_id}/resource-budget",
            get(proxy::forward_resource).put(proxy::forward_resource),
        )
        .route(
            "/api/v1/projects/{project_id}/charges",
            get(proxy::forward_resource),
        )
        .route(
            "/api/v1/projects/{project_id}/charges/{charge_id}/adjustments",
            post(proxy::forward_resource),
        )
}

fn control_browser_router() -> Router<Arc<AppState>> {
    Router::new()
        .merge(project_browser_router())
        .merge(course_browser_router())
}

#[allow(
    clippy::too_many_lines,
    reason = "the browser route table is kept explicit so each public project path is reviewable"
)]
fn project_browser_router() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/api/v1/projects",
            axum::routing::any(proxy::forward_control),
        )
        .route(
            "/api/v1/projects/{project_id}",
            axum::routing::any(proxy::forward_control),
        )
        .route(
            "/api/v1/projects/{project_id}/archive",
            axum::routing::any(proxy::forward_control),
        )
        .route(
            "/api/v1/projects/{project_id}/members",
            axum::routing::any(proxy::forward_control),
        )
        .route(
            "/api/v1/projects/{project_id}/members/{actor_id}",
            axum::routing::any(proxy::forward_control),
        )
        .route(
            "/api/v1/projects/{project_id}/problem-package-uploads",
            axum::routing::any(proxy::forward_control),
        )
        .route(
            "/api/v1/projects/{project_id}/problem-package-uploads/{upload_id}/complete",
            axum::routing::any(proxy::forward_control),
        )
        .route(
            "/api/v1/projects/{project_id}/problem-packages/{package_id}",
            axum::routing::any(proxy::forward_control),
        )
        .route(
            "/api/v1/projects/{project_id}/llm-egress-policies",
            axum::routing::any(proxy::forward_control),
        )
        .route(
            "/api/v1/projects/{project_id}/llm-egress-policies/active",
            axum::routing::any(proxy::forward_control),
        )
        .route(
            "/api/v1/projects/{project_id}/agent-runs",
            axum::routing::any(proxy::forward_control),
        )
        .route(
            "/api/v1/projects/{project_id}/work-agent-runs",
            axum::routing::any(proxy::forward_control),
        )
        .route(
            "/api/v1/projects/{project_id}/work-configuration-runs",
            axum::routing::any(proxy::forward_control),
        )
        .route(
            "/api/v1/projects/{project_id}/agent-runs/{run_id}",
            axum::routing::any(proxy::forward_control),
        )
        .route(
            "/api/v1/projects/{project_id}/agent-runs/{run_id}/work-configuration/approve",
            axum::routing::any(proxy::forward_control),
        )
        .route(
            "/api/v1/projects/{project_id}/agent-runs/{run_id}/work-configuration/plan",
            axum::routing::any(proxy::forward_control),
        )
        .route(
            "/api/v1/projects/{project_id}/agent-runs/{run_id}/cancel",
            axum::routing::any(proxy::forward_control),
        )
        .route(
            "/api/v1/projects/{project_id}/agent-runs/{run_id}/tracks/{track}/retry",
            axum::routing::any(proxy::forward_control),
        )
        .route(
            "/api/v1/projects/{project_id}/environment-candidates/{candidate_id}",
            axum::routing::any(proxy::forward_control),
        )
        .route(
            "/api/v1/projects/{project_id}/environment-candidates/{candidate_id}/decisions",
            axum::routing::any(proxy::forward_control),
        )
        .route(
            "/api/v1/projects/{project_id}/evaluation-candidates/{candidate_id}",
            axum::routing::any(proxy::forward_control),
        )
        .route(
            "/api/v1/projects/{project_id}/evaluation-candidates/{candidate_id}/decisions",
            axum::routing::any(proxy::forward_control),
        )
        .route(
            "/api/v1/projects/{project_id}/authoring-approvals",
            axum::routing::any(proxy::forward_control),
        )
        .route(
            "/api/v1/projects/{project_id}/authoring-approvals/{approval_id}",
            axum::routing::any(proxy::forward_control),
        )
        .route(
            "/api/v1/projects/{project_id}/environment-template-releases",
            axum::routing::any(proxy::forward_control),
        )
        .route(
            "/api/v1/projects/{project_id}/environment-template-releases/{release_id}",
            axum::routing::any(proxy::forward_control),
        )
        .route(
            "/api/v1/projects/{project_id}/environment-template-releases/{release_id}/withdraw",
            axum::routing::any(proxy::forward_control),
        )
        .route(
            "/api/v1/projects/{project_id}/events",
            axum::routing::any(proxy::forward_control),
        )
}

fn course_browser_router() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/api/v1/courses/{course_id}/problem-package-uploads",
            axum::routing::any(proxy::forward_control),
        )
        .route(
            "/api/v1/courses/{course_id}/problem-package-uploads/{upload_id}/complete",
            axum::routing::any(proxy::forward_control),
        )
        .route(
            "/api/v1/courses/{course_id}/problem-packages/{package_id}",
            axum::routing::any(proxy::forward_control),
        )
        .route(
            "/api/v1/courses/{course_id}/llm-egress-policies",
            axum::routing::any(proxy::forward_control),
        )
        .route(
            "/api/v1/courses/{course_id}/llm-egress-policies/active",
            axum::routing::any(proxy::forward_control),
        )
        .route(
            "/api/v1/courses/{course_id}/agent-runs",
            axum::routing::any(proxy::forward_control),
        )
        .route(
            "/api/v1/courses/{course_id}/work-agent-runs",
            axum::routing::any(proxy::forward_control),
        )
        .route(
            "/api/v1/courses/{course_id}/agent-runs/{run_id}",
            axum::routing::any(proxy::forward_control),
        )
        .route(
            "/api/v1/courses/{course_id}/agent-runs/{run_id}/cancel",
            axum::routing::any(proxy::forward_control),
        )
        .route(
            "/api/v1/courses/{course_id}/agent-runs/{run_id}/tracks/{track}/retry",
            axum::routing::any(proxy::forward_control),
        )
        .route(
            "/api/v1/courses/{course_id}/environment-candidates/{candidate_id}",
            axum::routing::any(proxy::forward_control),
        )
        .route(
            "/api/v1/courses/{course_id}/environment-candidates/{candidate_id}/decisions",
            axum::routing::any(proxy::forward_control),
        )
        .route(
            "/api/v1/courses/{course_id}/evaluation-candidates/{candidate_id}",
            axum::routing::any(proxy::forward_control),
        )
        .route(
            "/api/v1/courses/{course_id}/evaluation-candidates/{candidate_id}/decisions",
            axum::routing::any(proxy::forward_control),
        )
        .route(
            "/api/v1/courses/{course_id}/evaluation-releases",
            axum::routing::any(proxy::forward_control),
        )
        .route(
            "/api/v1/courses/{course_id}/evaluation-releases/{release_id}",
            axum::routing::any(proxy::forward_control),
        )
        .route(
            "/api/v1/courses/{course_id}/evaluation-releases/{release_id}/withdraw",
            axum::routing::any(proxy::forward_control),
        )
        .route(
            "/api/v1/courses/{course_id}/events",
            axum::routing::any(proxy::forward_control),
        )
}

fn internal_router(state: Arc<AppState>) -> Router {
    let router = Router::new()
        .route("/internal/v1/auth/decision", post(authorization_decision))
        .route("/internal/v1/metrics", get(metrics_endpoint))
        .route("/internal/v1/ssh/authorize", post(grants::authorize_ssh))
        .route(
            "/internal/v1/sessions",
            post(grants::create_gateway_session),
        )
        .route(
            "/internal/v1/sessions/{session_id}/heartbeat",
            post(grants::heartbeat_gateway_session),
        )
        .route(
            "/internal/v1/sessions/{session_id}/close",
            post(grants::close_gateway_session),
        )
        .with_state(state);
    telemetry::instrument_http(router, "access-service", "internal-api")
}

#[allow(
    clippy::too_many_lines,
    reason = "startup wiring keeps authority and secret locator validation in one auditable sequence"
)]
async fn build_app_state(
    deployment: AccessAuthFile,
    metrics: telemetry::PrometheusHandle,
) -> Result<Arc<AppState>, StartupError> {
    if deployment.transport_security == TransportSecurityMode::InsecureTestOnly
        && std::env::var("LABWEAVER_ENABLE_INSECURE_AUTH_TEST_MODE").as_deref() != Ok("1")
    {
        return Err(StartupError::Config);
    }
    let config = AuthConfig::new_with_transport_security(
        &deployment.oidc.issuer,
        deployment.oidc.client_id.clone(),
        &deployment.oidc.redirect_uri,
        &deployment.oidc.post_logout_redirect_uri,
        deployment.oidc.audience.clone(),
        deployment.browser.allowed_origins.clone(),
        deployment.browser.session_ttl_seconds,
        deployment.transport_security,
    )?;
    let trusted_ca = optional_file(&deployment.oidc.trusted_ca_file)?;
    let client_secret = optional_file(&deployment.secrets.oidc_client_secret_file)?
        .map(|value| String::from_utf8(value).map(|secret| secret.trim().to_owned()))
        .transpose()
        .map_err(|_| StartupError::Config)?
        .filter(|secret| !secret.is_empty());
    let key_material = std::fs::read_to_string(&deployment.secrets.session_keyring_file)?;
    let key_ring = KeyRing::parse(
        deployment.secrets.active_session_key_id.clone(),
        &key_material,
    )?;
    let database_url = std::fs::read_to_string(&deployment.secrets.access_runtime_url_file)?;
    let pool = PgPoolOptions::new()
        .max_connections(deployment.browser.runtime_pool_max_connections)
        .connect(database_url.trim())
        .await?;
    cleanup_expired_auth_state(
        &pool,
        OffsetDateTime::now_utc(),
        deployment_duration(deployment.browser.session_retention_seconds)
            .map_err(|_| StartupError::Config)?,
    )
    .await
    .map_err(|error| match error {
        auth::RepositoryError::Database(error) => StartupError::Database(error),
        _ => StartupError::Config,
    })?;
    let oidc_http = no_redirect_http_client(trusted_ca.as_deref(), deployment.transport_security)?;
    let provider = OidcProvider::discover(&config, client_secret, trusted_ca.as_deref()).await?;
    let bearer_authorizer = Arc::new(
        build_bearer_authorizer(&config, &deployment.oidc, oidc_http.clone())
            .await
            .map_err(|_| StartupError::Jwt)?,
    );
    let backchannel_logout_authorizer = Arc::new(
        build_backchannel_logout_authorizer(&config, &deployment.oidc, oidc_http.clone())
            .await
            .map_err(|_| StartupError::Jwt)?,
    );
    // The browser audience and the internal service audience are separate
    // trust domains.  Internal tokens must never be minted for the browser
    // API audience just because Access also validates browser sessions.
    let access_target = required_target("LABWEAVER_SERVICE_AUDIENCE", "LABWEAVER_SERVICE_SCOPES")?;
    let control_target = required_target(
        "LABWEAVER_CONTROL_SERVICE_AUDIENCE",
        "LABWEAVER_CONTROL_SERVICE_SCOPES",
    )?;
    let environment_target = required_target(
        "LABWEAVER_ENVIRONMENT_SERVICE_AUDIENCE",
        "LABWEAVER_ENVIRONMENT_SERVICE_SCOPES",
    )?;
    let evaluation_target = required_target(
        "LABWEAVER_EVALUATION_SERVICE_AUDIENCE",
        "LABWEAVER_EVALUATION_SERVICE_SCOPES",
    )?;
    let resource_target = required_target(
        "LABWEAVER_RESOURCE_SERVICE_AUDIENCE",
        "LABWEAVER_RESOURCE_SERVICE_SCOPES",
    )?;
    let service_allowed_client_ids = required_service_client_ids()?;
    let service_auth_config = ServiceAuthConfig::new(
        &deployment.oidc.issuer,
        access_target.audience.clone(),
        service_allowed_client_ids,
        BTreeSet::new(),
        deployment.oidc.jwt_algorithms.clone(),
        deployment.oidc.jwks_refresh_seconds,
        deployment.oidc.jwks_retry_seconds,
        deployment.transport_security,
    )
    .map_err(|_| StartupError::ServiceAuth(ServiceAuthError::InvalidConfig))?;
    let service_token_verifier = Arc::new(
        ServiceTokenVerifier::discover(service_auth_config, oidc_http.clone())
            .await
            .map_err(StartupError::ServiceAuth)?,
    );
    let service_token_client = build_service_token_client(
        &deployment,
        &oidc_http,
        access_target.audience.clone(),
        access_target.scopes.clone(),
    )
    .await?;
    let role_mappings = RoleMappings::parse(deployment.oidc.role_mappings.clone())?;
    let resolver_config = deployment.environment_owner_resolver.contract();
    let resolver_ca = resolver_secret(&deployment, &resolver_config.ca_certificate_locator)?;
    let owner_resolver = EnvironmentOwnerResolverClient::new(
        &resolver_config,
        &resolver_ca,
        service_token_client.as_ref().clone(),
        environment_target.audience.clone(),
        environment_target.scopes.clone(),
        std::time::Duration::from_millis(
            deployment
                .environment_owner_resolver
                .retry_backoff_milliseconds,
        ),
        deployment.transport_security,
    )?;
    let environment_ca = resolver_secret(
        &deployment,
        &deployment.environment_gateway.ca_certificate_locator,
    )?;
    let console_gateway = console::ConsoleGateway::new(
        &deployment.environment_gateway.base_uri,
        &environment_ca,
        Arc::clone(&service_token_client),
        environment_target.clone(),
    )
    .map_err(|_| StartupError::Config)?;
    let control_proxy = build_control_proxy(
        &deployment,
        Arc::clone(&service_token_client),
        control_target,
    )?;
    let environment_proxy = build_service_proxy(
        &deployment,
        &deployment.environment_gateway,
        Arc::clone(&service_token_client),
        environment_target,
    )?;
    let evaluation_proxy = build_service_proxy(
        &deployment,
        &deployment.evaluation_gateway,
        Arc::clone(&service_token_client),
        evaluation_target,
    )?;
    let resource_ca = resolver_secret(
        &deployment,
        &deployment.resource_gateway.ca_certificate_locator,
    )?;
    let resource_delegation_key = resolver_secret(
        &deployment,
        &deployment.resource_gateway.delegation_key_locator,
    )?;
    let resource_proxy = proxy::ResourceGatewayProxy::new(
        &deployment.resource_gateway,
        &resource_ca,
        &resource_delegation_key,
        deployment.transport_security,
        Arc::clone(&service_token_client),
        resource_target,
    )?;
    let runtime_proxy = proxy::RuntimeGatewayProxy::new(&deployment.environment_gateway)?;
    let nats = grants::connect_nats(&deployment.nats).await?;
    Ok(Arc::new(AppState {
        config,
        deployment,
        provider,
        oidc_http,
        bearer_authorizer,
        backchannel_logout_authorizer,
        service_token_verifier,
        role_mappings,
        pool,
        key_ring,
        owner_resolver,
        console_gateway,
        console_registry: console::ConsoleRegistry::default(),
        console_proxy_owner: format!("access-proxy-{}", Uuid::now_v7()),
        control_proxy,
        environment_proxy,
        evaluation_proxy,
        resource_proxy,
        runtime_proxy,
        metrics,
        nats,
    }))
}

fn build_control_proxy(
    deployment: &AccessAuthFile,
    service_token_client: Arc<ServiceTokenClient>,
    service_token_target: ServiceTokenTarget,
) -> Result<proxy::ControlGatewayProxy, StartupError> {
    build_service_proxy(
        deployment,
        &deployment.control_gateway,
        service_token_client,
        service_token_target,
    )
}

fn build_service_proxy(
    deployment: &AccessAuthFile,
    config: &auth::ControlGatewayFileConfig,
    service_token_client: Arc<ServiceTokenClient>,
    service_token_target: ServiceTokenTarget,
) -> Result<proxy::ControlGatewayProxy, StartupError> {
    let ca = resolver_secret(deployment, &config.ca_certificate_locator)?;
    Ok(proxy::ControlGatewayProxy::new(
        config,
        &ca,
        deployment.transport_security,
        service_token_client,
        service_token_target,
    )?)
}

async fn build_service_token_client(
    deployment: &AccessAuthFile,
    http: &reqwest::Client,
    audience: String,
    scopes: BTreeSet<String>,
) -> Result<Arc<ServiceTokenClient>, StartupError> {
    let config = ServiceTokenClientConfig::new(
        &deployment.oidc.issuer,
        required("LABWEAVER_SERVICE_CLIENT_ID")?,
        read_required_secret(&required("LABWEAVER_SERVICE_CLIENT_SECRET_FILE")?)?,
        audience,
        scopes,
        required_u64("LABWEAVER_SERVICE_TOKEN_REFRESH_SKEW_SECONDS")?,
        deployment.transport_security,
    )?;
    Ok(Arc::new(
        ServiceTokenClient::discover(config, http.clone()).await?,
    ))
}

async fn auth_cleanup_loop(state: Arc<AppState>) -> Result<(), StartupError> {
    let interval =
        std::time::Duration::from_secs(state.deployment.browser.cleanup_interval_seconds);
    let mut ticker = tokio::time::interval_at(tokio::time::Instant::now() + interval, interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        match cleanup_expired_auth_state(
            &state.pool,
            OffsetDateTime::now_utc(),
            deployment_duration(state.deployment.browser.session_retention_seconds)
                .map_err(|_| StartupError::Config)?,
        )
        .await
        {
            Ok(report) => {
                metrics::counter!("labweaver_auth_cleanup_runs", "result" => "success")
                    .increment(1);
                metrics::counter!("labweaver_auth_cleanup_records", "kind" => "sessions_revoked")
                    .increment(report.sessions_revoked);
                metrics::counter!("labweaver_auth_cleanup_records", "kind" => "sessions_deleted")
                    .increment(report.sessions_deleted);
                metrics::counter!("labweaver_auth_cleanup_records", "kind" => "transactions_deleted")
                    .increment(report.transactions_deleted);
                metrics::counter!("labweaver_auth_cleanup_records", "kind" => "logout_events_deleted")
                    .increment(report.logout_events_deleted);
            }
            Err(_error) => {
                metrics::counter!("labweaver_auth_cleanup_runs", "result" => "failed").increment(1);
                tracing::error!(
                    event = "auth.cleanup.failed",
                    diagnostic_code = "LW_AUTH_MEMBERSHIP_UNAVAILABLE",
                    error_kind = "persistence",
                    failure_stage = "cleanup",
                    retryable = false
                );
            }
        }
    }
}

async fn serve_internal(
    listener: tokio::net::TcpListener,
    router: Router,
    tls: std::sync::Arc<rustls::ServerConfig>,
) -> Result<(), StartupError> {
    http_transport::serve_tls(listener, router, tls)
        .await
        .map_err(StartupError::HttpTransport)
}

fn load_deployment() -> Result<AccessAuthFile, StartupError> {
    let path = required("LABWEAVER_ACCESS_AUTH_CONFIG_FILE")?;
    let contents = std::fs::read_to_string(path)?;
    Ok(AccessAuthFile::parse_yaml(&contents)?)
}

fn optional_file(locator: &str) -> Result<Option<Vec<u8>>, StartupError> {
    if locator.is_empty() {
        return Ok(None);
    }
    Ok(Some(std::fs::read(locator)?))
}

fn resolver_secret(deployment: &AccessAuthFile, locator: &str) -> Result<Vec<u8>, StartupError> {
    let path = deployment
        .secrets
        .file_bindings
        .get(locator)
        .ok_or(StartupError::Config)?;
    Ok(std::fs::read(path)?)
}

async fn login(State(state): State<Arc<AppState>>) -> Result<Redirect, ApiError> {
    let transaction = OidcTransaction::generate()
        .map_err(|_| ApiError::unavailable("LW_AUTH_OIDC_RANDOMNESS_UNAVAILABLE"))?;
    let transaction_id = Uuid::now_v7();
    let encrypted = state
        .key_ring
        .encrypt(
            &serde_json::to_vec(&transaction)
                .map_err(|_| ApiError::internal("LW_AUTH_OIDC_STATE_REJECTED"))?,
            transaction_id.as_bytes(),
        )
        .map_err(|_| ApiError::internal("LW_AUTH_KEYRING_ENCRYPTION_FAILED"))?;
    let state_hash = Sha256Digest::of_bytes(transaction.state.as_bytes()).to_string();
    sqlx::query("INSERT INTO access.oidc_transactions (transaction_id, state_sha256, encrypted_payload, encryption_key_id, expires_at) VALUES ($1,$2,$3,$4,$5)")
        .bind(transaction_id)
        .bind(state_hash)
        .bind(encrypted.payload)
        .bind(encrypted.key_id)
        .bind(
            OffsetDateTime::now_utc()
                + deployment_duration(state.deployment.browser.oidc_transaction_ttl_seconds)?,
        )
        .execute(&state.pool)
        .await
        .map_err(|_| ApiError::unavailable("LW_AUTH_MEMBERSHIP_UNAVAILABLE"))?;
    let url = state
        .provider
        .authorization_url(&transaction)
        .map_err(ApiError::from)?;
    metrics::counter!("labweaver_auth_oidc_transactions", "result" => "created").increment(1);
    Ok(Redirect::temporary(url.as_str()))
}

#[derive(Deserialize)]
struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

async fn callback(
    State(state): State<Arc<AppState>>,
    Query(query): Query<CallbackQuery>,
) -> Result<Response, ApiError> {
    if query.error.is_some() {
        return Err(ApiError::unauthorized("LW_AUTH_TOKEN_INVALID"));
    }
    let code = query
        .code
        .ok_or_else(|| ApiError::unauthorized("LW_AUTH_OIDC_STATE_REQUIRED"))?;
    let returned_state = query
        .state
        .ok_or_else(|| ApiError::unauthorized("LW_AUTH_OIDC_STATE_REQUIRED"))?;
    let now = OffsetDateTime::now_utc();
    let transaction = consume_oidc_transaction(&state.pool, &state.key_ring, &returned_state, now)
        .await
        .map_err(ApiError::from)?;
    let identity = state
        .provider
        .exchange_code(code, &transaction, &state.oidc_http)
        .await
        .map_err(ApiError::from)?;
    identity.validate_expiry_at(now).map_err(ApiError::from)?;
    let roles = extract_platform_roles(
        &identity.claims,
        &state.deployment.oidc.role_claim_path,
        &state.role_mappings,
    )
    .map_err(ApiError::from)?
    .into_iter()
    .collect::<Vec<_>>();
    let actor = upsert_actor(&state.pool, state.config.issuer.as_str(), &identity.subject)
        .await
        .map_err(ApiError::from)?;
    let expires_at = auth::configured_session_expiry(
        now,
        deployment_duration(state.config.session_ttl_seconds)?,
    )
    .map_err(ApiError::from)?;
    let session = create_bff_session(
        &state.pool,
        &state.key_ring,
        auth::CreateBffSession {
            actor_id: actor.actor_id,
            roles,
            authorization_revision: 1,
            expires_at,
            idle_ttl: deployment_duration(state.deployment.browser.session_idle_ttl_seconds)?,
            oidc_sid: identity.sid,
            logout_hint: identity.logout_hint,
        },
        now,
    )
    .await
    .map_err(ApiError::from)?;
    let mut response =
        Redirect::temporary(state.config.post_logout_redirect_uri.as_str()).into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        session_cookie(&state, session.session_id, session.expires_at)?,
    );
    metrics::counter!("labweaver_auth_callbacks", "result" => "success").increment(1);
    Ok(response)
}

#[derive(Deserialize)]
struct BackchannelLogoutForm {
    logout_token: String,
}

async fn backchannel_logout(
    State(state): State<Arc<AppState>>,
    Form(form): Form<BackchannelLogoutForm>,
) -> Result<StatusCode, ApiError> {
    let claims = state
        .backchannel_logout_authorizer
        .check_auth(&form.logout_token)
        .await
        .map_err(|_| ApiError::unauthorized("LW_AUTH_TOKEN_INVALID"))?
        .claims;
    let sid = claims
        .sid
        .ok_or_else(|| ApiError::unauthorized("LW_AUTH_TOKEN_INVALID"))?;
    let now = OffsetDateTime::now_utc();
    let expires_at = OffsetDateTime::from_unix_timestamp(claims.exp)
        .map_err(|_| ApiError::unauthorized("LW_AUTH_TOKEN_INVALID"))?;
    let session_ids = sqlx::query_scalar::<_, Uuid>(
        "SELECT session_id FROM access.bff_sessions WHERE oidc_sid_sha256=$1 AND revoked_at IS NULL",
    )
    .bind(Sha256Digest::of_bytes(sid.as_bytes()).to_string())
    .fetch_all(&state.pool)
    .await
    .map_err(|_| ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))?;
    consume_backchannel_logout(
        &state.pool,
        state.config.issuer.as_str(),
        &claims.jti,
        &sid,
        expires_at,
        now,
    )
    .await
    .map_err(ApiError::from)?;
    for session_id in session_ids {
        console::terminate_bff_sessions(&state, session_id, "LW_AUTH_SESSION_REVOKED").await?;
    }
    metrics::counter!("labweaver_auth_sessions", "event" => "backchannel_logout").increment(1);
    Ok(StatusCode::NO_CONTENT)
}

async fn authorization_decision(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<AuthorizationDecisionRequest>,
) -> Result<Json<AuthorizationDecision>, ApiError> {
    let started = std::time::Instant::now();
    let now = OffsetDateTime::now_utc();
    let service = service_identity(&state, &headers, "access.authorization.decide").await?;
    let policy = operation_contract(&request.operation_id)
        .ok_or_else(|| ApiError::forbidden("LW_AUTH_SCOPE_DENIED"))?;
    if !scope_matches_kind(&request.scope, policy.scope) {
        return Err(ApiError::forbidden("LW_AUTH_SCOPE_DENIED"));
    }
    if let AuthorizationScope::Service { service_id } = &request.scope
        && service_id != &service.client_id
    {
        return Err(ApiError::forbidden("LW_AUTH_SERVICE_IDENTITY_DENIED"));
    }
    let session_id = request.session_id.as_uuid();
    let session = load_bff_session(
        &state.pool,
        &state.key_ring,
        session_id,
        deployment_duration(state.deployment.browser.session_idle_ttl_seconds)?,
        now,
    )
    .await
    .map_err(ApiError::from)?;
    let actor = actor_from_session(&session)?;
    if actor.actor_id != request.actor_id {
        return Err(ApiError::forbidden("LW_AUTH_SCOPE_DENIED"));
    }
    let memberships = load_membership_snapshot(&state.pool, session.actor_id)
        .await
        .map_err(ApiError::from)?;
    let requested_scope = request.scope.clone();
    let mut decision = authorize(
        &AuthorizationContext {
            actor,
            course_memberships: memberships.course_memberships,
            project_memberships: memberships.project_memberships,
            now,
        },
        requested_scope,
        &policy
            .allowed_roles
            .iter()
            .copied()
            .collect::<BTreeSet<_>>(),
    )
    .map_err(ApiError::from)?;
    resolve_environment_owner(&state, &mut decision, now).await?;
    cap_decision_expiry(&state, &mut decision, now)?;
    validate_observed_revisions(&request, &decision)?;
    tracing::info!(
        event = "auth.authorization.decision",
        component = "authorization",
        outcome = "permitted",
        duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        actor_id = %decision.actor.actor_id,
        operation = policy.operation_id,
        diagnostic_code = "LW_AUTH_DECISION_PERMIT",
        retryable = false,
    );
    metrics::counter!("labweaver_auth_authorization_decisions", "decision" => "permit")
        .increment(1);
    metrics::histogram!("labweaver_auth_authorization_duration_seconds")
        .record(started.elapsed().as_secs_f64());
    Ok(Json(decision))
}

async fn resolve_environment_owner(
    state: &AppState,
    decision: &mut AuthorizationDecision,
    now: OffsetDateTime,
) -> Result<(), ApiError> {
    let AuthorizationScope::Environment {
        project_id,
        course_id,
        environment_id,
        environment_revision,
    } = &decision.scope
    else {
        return Ok(());
    };
    let resolution = state
        .owner_resolver
        .resolve(
            &EnvironmentOwnerResolutionRequest {
                environment_id: *environment_id,
                project_id: *project_id,
                course_id: *course_id,
                owner_actor_id: decision.actor.actor_id,
                expected_revision: *environment_revision,
            },
            utc_timestamp(now)?,
        )
        .await
        .map_err(ApiError::from)?;
    decision.scope_revision = resolution.environment_revision;
    if resolution.eligibility_expires_at < decision.valid_until {
        decision.valid_until = resolution.eligibility_expires_at;
    }
    Ok(())
}

fn cap_decision_expiry(
    state: &AppState,
    decision: &mut AuthorizationDecision,
    now: OffsetDateTime,
) -> Result<(), ApiError> {
    let decision_ttl = utc_timestamp(
        now + deployment_duration(
            state
                .deployment
                .environment_owner_resolver
                .decision_ttl_seconds,
        )?,
    )?;
    if decision_ttl < decision.valid_until {
        decision.valid_until = decision_ttl;
    }
    Ok(())
}

fn validate_observed_revisions(
    request: &AuthorizationDecisionRequest,
    decision: &AuthorizationDecision,
) -> Result<(), ApiError> {
    if request
        .authorization_revision
        .is_some_and(|observed| observed.get() > decision.authorization_revision.get())
        || request
            .scope_revision
            .is_some_and(|observed| observed.get() > decision.scope_revision.get())
    {
        return Err(ApiError::forbidden("LW_AUTH_SCOPE_DENIED"));
    }
    Ok(())
}

async fn metrics_endpoint(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    let _service = service_identity(&state, &headers, "access.metrics.read").await?;
    Ok((
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        state.metrics.render(),
    ))
}

fn scope_matches_kind(scope: &AuthorizationScope, kind: OperationScopeKind) -> bool {
    matches!(
        (scope, kind),
        (AuthorizationScope::Global, OperationScopeKind::Global)
            | (
                AuthorizationScope::Course { .. },
                OperationScopeKind::Course
            )
            | (
                AuthorizationScope::Project { .. },
                OperationScopeKind::Project
            )
            | (
                AuthorizationScope::Environment { .. },
                OperationScopeKind::Environment
            )
            | (
                AuthorizationScope::Service { .. },
                OperationScopeKind::Service
            )
    )
}

async fn session(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<AuthSession>, ApiError> {
    let identity = authenticated_identity(&state, &headers).await?;
    let actor_id = Uuid::parse_str(&identity.actor.actor_id.to_string())
        .map_err(|_| ApiError::internal("LW_AUTH_SESSION_REJECTED"))?;
    let memberships = load_membership_snapshot(&state.pool, actor_id)
        .await
        .map_err(ApiError::from)?;
    let (scopes, authorization_revision, expires_at) = effective_session_scopes(
        &identity.actor,
        memberships,
        identity.authorization_revision,
        identity.expires_at,
        OffsetDateTime::now_utc(),
    )?;
    Ok(Json(AuthSession {
        actor: identity.actor,
        authorization_revision,
        scopes,
        expires_at,
    }))
}

fn effective_session_scopes(
    actor: &AuthenticatedActor,
    memberships: auth::MembershipSnapshot,
    initial_revision: Revision,
    initial_expiry: UtcTimestamp,
    now: OffsetDateTime,
) -> Result<(Vec<AuthorizationScope>, Revision, UtcTimestamp), ApiError> {
    let mut scopes = vec![AuthorizationScope::Global];
    let mut revision = initial_revision;
    let mut expiry = initial_expiry;
    for membership in memberships.course_memberships {
        if membership.actor_id == actor.actor_id
            && membership.state == contracts::MembershipState::Active
            && actor.roles.contains(&membership.role)
            && membership.expires_at.is_none_or(|value| value.get() > now)
        {
            revision = Revision::new(revision.get().max(membership.revision.get()))
                .map_err(|_| ApiError::internal("LW_AUTH_MEMBERSHIP_UNAVAILABLE"))?;
            if let Some(member_expiry) = membership.expires_at
                && member_expiry.get() < expiry.get()
            {
                expiry = member_expiry;
            }
            scopes.push(AuthorizationScope::Course {
                course_id: membership.course_id,
            });
        }
    }
    for membership in memberships.project_memberships {
        if membership.actor_id == actor.actor_id
            && membership.state == contracts::MembershipState::Active
            && actor.roles.contains(&membership.role)
            && membership.expires_at.is_none_or(|value| value.get() > now)
        {
            revision = Revision::new(revision.get().max(membership.revision.get()))
                .map_err(|_| ApiError::internal("LW_AUTH_MEMBERSHIP_UNAVAILABLE"))?;
            if let Some(member_expiry) = membership.expires_at
                && member_expiry.get() < expiry.get()
            {
                expiry = member_expiry;
            }
            scopes.push(AuthorizationScope::Project {
                project_id: membership.project_id,
            });
        }
    }
    Ok((scopes, revision, expiry))
}

async fn csrf(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<CsrfTokenResponse>, ApiError> {
    let session = authenticated_session(&state, &headers).await?;
    metrics::counter!("labweaver_auth_csrf_tokens", "result" => "issued").increment(1);
    Ok(Json(CsrfTokenResponse {
        csrf_token: session.csrf_token.expose().to_owned(),
        expires_at: utc_timestamp(session.expires_at)?,
    }))
}

async fn logout(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    require_browser_origin(&state, &headers)?;
    let session_id = cookie_session_id(&state, &headers)
        .ok_or_else(|| ApiError::unauthorized("LW_AUTH_SESSION_REJECTED"))?;
    let session = authenticated_session(&state, &headers).await?;
    let supplied = headers
        .get(state.deployment.browser.csrf_header_name.as_str())
        .and_then(|value| value.to_str().ok());
    auth::verify_csrf_token(&session.csrf_token, supplied).map_err(ApiError::from)?;
    let logout_hint = load_logout_hint(&state.pool, &state.key_ring, session_id)
        .await
        .map_err(ApiError::from)?;
    let logout_url = state
        .provider
        .logout_url(&logout_hint, state.config.post_logout_redirect_uri.as_str())
        .map_err(ApiError::from)?;
    revoke_bff_session(
        &state.pool,
        session_id,
        "LW_AUTH_SESSION_REVOKED",
        OffsetDateTime::now_utc(),
    )
    .await
    .map_err(ApiError::from)?;
    console::terminate_bff_sessions(&state, session_id, "LW_AUTH_SESSION_REVOKED").await?;
    let mut response = Redirect::to(logout_url.as_str()).into_response();
    response
        .headers_mut()
        .insert(header::SET_COOKIE, clear_session_cookie(&state)?);
    metrics::counter!("labweaver_auth_sessions", "event" => "logout").increment(1);
    Ok(response)
}

fn require_browser_origin(state: &AppState, headers: &HeaderMap) -> Result<(), ApiError> {
    let origin = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| ApiError::forbidden("LW_AUTH_CSRF_REJECTED"))?;
    if !state.config.allowed_origins.contains(origin) {
        return Err(ApiError::forbidden("LW_AUTH_CSRF_REJECTED"));
    }
    Ok(())
}

async fn authenticated_session(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<BffSession, ApiError> {
    let id = cookie_session_id(state, headers)
        .ok_or_else(|| ApiError::unauthorized("LW_AUTH_SESSION_REJECTED"))?;
    load_bff_session(
        &state.pool,
        &state.key_ring,
        id,
        deployment_duration(state.deployment.browser.session_idle_ttl_seconds)?,
        OffsetDateTime::now_utc(),
    )
    .await
    .map_err(ApiError::from)
}

struct AuthenticatedIdentity {
    actor: AuthenticatedActor,
    authorization_revision: Revision,
    expires_at: UtcTimestamp,
}

async fn authenticated_identity(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<AuthenticatedIdentity, ApiError> {
    if cookie_session_id(state, headers).is_some() {
        let session = authenticated_session(state, headers).await?;
        let expires_at = utc_timestamp(session.expires_at)?;
        return Ok(AuthenticatedIdentity {
            actor: actor_from_session(&session)?,
            authorization_revision: Revision::new(
                u64::try_from(session.authorization_revision)
                    .map_err(|_| ApiError::internal("LW_AUTH_SESSION_REJECTED"))?,
            )
            .map_err(|_| ApiError::unavailable("LW_AUTH_MEMBERSHIP_UNAVAILABLE"))?,
            expires_at,
        });
    }
    let token = state
        .bearer_authorizer
        .extract_token(headers)
        .ok_or_else(|| ApiError::unauthorized("LW_AUTH_REQUIRED"))?;
    let claims = state
        .bearer_authorizer
        .check_auth(&token)
        .await
        .map_err(|_| ApiError::unauthorized("LW_AUTH_TOKEN_INVALID"))?
        .claims;
    let expires_at = OffsetDateTime::from_unix_timestamp(claims.exp)
        .map_err(|_| ApiError::unauthorized("LW_AUTH_TOKEN_INVALID"))?;
    let roles = extract_platform_roles(
        &serde_json::Value::Object(claims.claims),
        &state.deployment.oidc.role_claim_path,
        &state.role_mappings,
    )
    .map_err(ApiError::from)?
    .into_iter()
    .collect();
    let local_actor = upsert_actor(&state.pool, state.config.issuer.as_str(), &claims.sub)
        .await
        .map_err(ApiError::from)?;
    Ok(AuthenticatedIdentity {
        actor: AuthenticatedActor {
            actor_id: local_actor
                .actor_id
                .to_string()
                .parse()
                .map_err(|_| ApiError::internal("LW_AUTH_TOKEN_INVALID"))?,
            roles,
            expires_at: utc_timestamp(expires_at)?,
        },
        authorization_revision: Revision::new(1)
            .map_err(|_| ApiError::internal("LW_AUTH_MEMBERSHIP_UNAVAILABLE"))?,
        expires_at: utc_timestamp(expires_at)?,
    })
}

fn actor_from_session(session: &BffSession) -> Result<AuthenticatedActor, ApiError> {
    Ok(AuthenticatedActor {
        actor_id: session
            .actor_id
            .to_string()
            .parse()
            .map_err(|_| ApiError::internal("LW_AUTH_SESSION_REJECTED"))?,
        roles: session.roles.clone(),
        expires_at: utc_timestamp(session.expires_at)?,
    })
}

fn utc_timestamp(value: OffsetDateTime) -> Result<UtcTimestamp, ApiError> {
    let millisecond_precision = value
        .replace_nanosecond((value.nanosecond() / 1_000_000) * 1_000_000)
        .map_err(|_| ApiError::internal("LW_AUTH_TIMESTAMP_INVALID"))?;
    UtcTimestamp::from_utc(millisecond_precision)
        .map_err(|_| ApiError::internal("LW_AUTH_TIMESTAMP_INVALID"))
}

fn cookie_session_id(state: &AppState, headers: &HeaderMap) -> Option<Uuid> {
    let cookie = headers.get(header::COOKIE)?.to_str().ok()?;
    cookie.split(';').find_map(|part| {
        let (name, value) = part.trim().split_once('=')?;
        (name == state.deployment.browser.session_cookie_name)
            .then(|| Uuid::parse_str(value).ok())
            .flatten()
    })
}

fn session_cookie(
    state: &AppState,
    session_id: Uuid,
    expires_at: OffsetDateTime,
) -> Result<HeaderValue, ApiError> {
    let max_age = (expires_at - OffsetDateTime::now_utc())
        .whole_seconds()
        .max(0);
    HeaderValue::from_str(&format!(
        "{}={session_id}; Path=/; Secure; HttpOnly; SameSite=Lax; Max-Age={max_age}",
        state.deployment.browser.session_cookie_name,
    ))
    .map_err(|_| ApiError::internal("LW_AUTH_SESSION_REJECTED"))
}

fn clear_session_cookie(state: &AppState) -> Result<HeaderValue, ApiError> {
    HeaderValue::from_str(&format!(
        "{}=; Path=/; Secure; HttpOnly; SameSite=Lax; Max-Age=0",
        state.deployment.browser.session_cookie_name,
    ))
    .map_err(|_| ApiError::internal("LW_AUTH_SESSION_REJECTED"))
}

fn required(name: &'static str) -> Result<String, StartupError> {
    std::env::var(name).map_err(|_| StartupError::Config)
}

fn required_u64(name: &'static str) -> Result<u64, StartupError> {
    required(name)?
        .parse::<u64>()
        .map_err(|_| StartupError::Config)
}

fn required_set(name: &'static str) -> Result<BTreeSet<String>, StartupError> {
    let values = required(name)?
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    if values.is_empty() {
        Err(StartupError::Config)
    } else {
        Ok(values)
    }
}

fn required_target(
    audience_name: &'static str,
    scopes_name: &'static str,
) -> Result<ServiceTokenTarget, StartupError> {
    Ok(ServiceTokenTarget {
        audience: required(audience_name)?,
        scopes: required_set(scopes_name)?,
    })
}

fn read_required_secret(path: &str) -> Result<String, StartupError> {
    let value = std::fs::read_to_string(path)?;
    let value = value.trim();
    if value.is_empty() {
        Err(StartupError::Config)
    } else {
        Ok(value.to_owned())
    }
}

fn required_service_client_ids() -> Result<BTreeSet<String>, StartupError> {
    let value = required("LABWEAVER_SERVICE_ALLOWED_CLIENT_IDS")?;
    let ids = value
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    if ids.is_empty() {
        Err(StartupError::Config)
    } else {
        Ok(ids)
    }
}

pub(crate) async fn service_identity(
    state: &AppState,
    headers: &HeaderMap,
    permission: &str,
) -> Result<ServiceIdentity, ApiError> {
    state
        .service_token_verifier
        .authenticate_with_permission(headers, permission)
        .await
        .map_err(ApiError::from)
}

fn deployment_duration(seconds: u64) -> Result<Duration, ApiError> {
    Ok(Duration::seconds(i64::try_from(seconds).map_err(|_| {
        ApiError::internal("LW_AUTH_CONFIG_SESSION_TTL_INVALID")
    })?))
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    diagnostic: &'static str,
}

impl ApiError {
    fn bad_request(diagnostic: &'static str) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            diagnostic,
        }
    }
    fn unauthorized(diagnostic: &'static str) -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            diagnostic,
        }
    }
    fn unavailable(diagnostic: &'static str) -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            diagnostic,
        }
    }
    fn forbidden(diagnostic: &'static str) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            diagnostic,
        }
    }
    fn internal(diagnostic: &'static str) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            diagnostic,
        }
    }
    fn conflict(diagnostic: &'static str) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            diagnostic,
        }
    }
    fn precondition(diagnostic: &'static str) -> Self {
        Self {
            status: StatusCode::PRECONDITION_FAILED,
            diagnostic,
        }
    }
    fn unprocessable(diagnostic: &'static str) -> Self {
        Self {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            diagnostic,
        }
    }
    fn not_found(diagnostic: &'static str) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            diagnostic,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let retryable = matches!(
            self.status,
            StatusCode::SERVICE_UNAVAILABLE
                | StatusCode::GATEWAY_TIMEOUT
                | StatusCode::TOO_MANY_REQUESTS
        );
        if self.status.is_server_error() && !retryable {
            tracing::error!(
                event = "auth.request.failed",
                component = "api-error-boundary",
                operation = "http.request",
                outcome = "failed",
                duration_ms = 0_u64,
                diagnostic_code = self.diagnostic,
                error_kind = "terminal_api_failure",
                failure_stage = "auth.request.finalize",
                retryable = false,
                safe_detail = "redacted_unclassified",
            );
        } else {
            tracing::warn!(
                event = "auth.request.rejected",
                component = "api-error-boundary",
                operation = "http.request",
                outcome = "rejected",
                duration_ms = 0_u64,
                diagnostic_code = self.diagnostic,
                error_kind = "request_rejected",
                failure_stage = "auth.request.finalize",
                retryable,
                safe_detail = "request_rejected",
            );
        }
        metrics::counter!(
            "labweaver_auth_http_failures",
            "diagnostic" => self.diagnostic,
            "status" => self.status.as_u16().to_string()
        )
        .increment(1);
        (
            self.status,
            Json(serde_json::json!({"diagnosticCode": self.diagnostic})),
        )
            .into_response()
    }
}

impl From<auth::OidcProviderError> for ApiError {
    fn from(_: auth::OidcProviderError) -> Self {
        Self::unavailable("LW_AUTH_JWKS_UNAVAILABLE")
    }
}
impl From<auth::RepositoryError> for ApiError {
    fn from(error: auth::RepositoryError) -> Self {
        match error {
            auth::RepositoryError::StateRejected
            | auth::RepositoryError::SessionRejected
            | auth::RepositoryError::ActorDisabled => {
                Self::unauthorized("LW_AUTH_SESSION_REJECTED")
            }
            auth::RepositoryError::ServiceIdentityDenied => {
                Self::forbidden("LW_AUTH_SERVICE_IDENTITY_DENIED")
            }
            auth::RepositoryError::Database(_) => {
                Self::unavailable("LW_AUTH_MEMBERSHIP_UNAVAILABLE")
            }
            auth::RepositoryError::LogoutReplay => {
                Self::unauthorized("LW_AUTH_LOGOUT_TOKEN_REPLAYED")
            }
            _ => Self::internal("LW_AUTH_SESSION_REJECTED"),
        }
    }
}
impl From<auth::AuthorizationError> for ApiError {
    fn from(error: auth::AuthorizationError) -> Self {
        match error {
            auth::AuthorizationError::IdentityExpired => {
                Self::unauthorized("LW_AUTH_IDENTITY_EXPIRED")
            }
            auth::AuthorizationError::RoleDenied
            | auth::AuthorizationError::CourseScopeDenied
            | auth::AuthorizationError::ProjectScopeDenied => {
                Self::forbidden("LW_AUTH_SCOPE_DENIED")
            }
        }
    }
}
impl From<auth::RoleClaimError> for ApiError {
    fn from(_: auth::RoleClaimError) -> Self {
        Self::unauthorized("LW_AUTH_ROLE_DENIED")
    }
}
impl From<auth::CsrfError> for ApiError {
    fn from(_: auth::CsrfError) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            diagnostic: "LW_AUTH_CSRF_REJECTED",
        }
    }
}

impl From<auth::OwnerResolverClientError> for ApiError {
    fn from(error: auth::OwnerResolverClientError) -> Self {
        match error {
            auth::OwnerResolverClientError::ScopeDenied => {
                Self::forbidden("LW_AUTH_ENVIRONMENT_SCOPE_DENIED")
            }
            auth::OwnerResolverClientError::Unavailable => {
                Self::unavailable("LW_AUTH_OWNER_RESOLVER_UNAVAILABLE")
            }
            auth::OwnerResolverClientError::ResponseInvalid => {
                Self::unavailable("LW_AUTH_OWNER_RESPONSE_INVALID")
            }
            auth::OwnerResolverClientError::Configuration => {
                Self::internal("LW_AUTH_CONFIG_BINDING_MISSING")
            }
        }
    }
}

impl From<ServiceAuthError> for ApiError {
    fn from(error: ServiceAuthError) -> Self {
        match error {
            ServiceAuthError::CredentialsMissing
            | ServiceAuthError::TokenRejected
            | ServiceAuthError::TokenExpired => Self::unauthorized("LW_AUTH_SERVICE_TOKEN_INVALID"),
            ServiceAuthError::PermissionDenied => {
                Self::forbidden("LW_AUTH_SERVICE_PERMISSION_DENIED")
            }
            ServiceAuthError::InvalidConfig
            | ServiceAuthError::JwksUnavailable
            | ServiceAuthError::EndpointTransport
            | ServiceAuthError::HttpClient => Self::unavailable("LW_AUTH_SERVICE_UNAVAILABLE"),
        }
    }
}

#[derive(Debug, thiserror::Error)]
enum StartupError {
    #[error("LW_AUTH_CONFIG_BINDING_MISSING")]
    Config,
    #[error("LW_AUTH_STARTUP_FAILED")]
    ConfigValue(#[from] auth::AuthConfigError),
    #[error("LW_AUTH_STARTUP_FAILED")]
    Telemetry(#[from] telemetry::TelemetryError),
    #[error("LW_AUTH_STARTUP_FAILED")]
    Provider(#[from] auth::OidcProviderError),
    #[error("LW_AUTH_STARTUP_FAILED")]
    Crypto(#[from] auth::CryptoError),
    #[error("LW_AUTH_STARTUP_FAILED")]
    OwnerResolver(#[from] auth::OwnerResolverClientError),
    #[error("LW_AUTH_STARTUP_FAILED")]
    ControlGateway(#[from] proxy::ControlGatewayError),
    #[error("LW_AUTH_STARTUP_FAILED")]
    GrantRuntime(#[from] grants::GrantRuntimeError),
    #[error("LW_AUTH_STARTUP_FAILED")]
    Role(#[from] auth::RoleClaimError),
    #[error("LW_AUTH_STARTUP_FAILED")]
    Jwt,
    #[error("LW_AUTH_STARTUP_FAILED")]
    ServiceAuth(#[source] ServiceAuthError),
    #[error("LW_AUTH_STARTUP_FAILED")]
    ServiceToken(#[from] auth::ServiceTokenClientError),
    #[error("LW_AUTH_STARTUP_FAILED")]
    HttpTransport(#[source] http_transport::HttpTransportError),
    #[error("LW_AUTH_STARTUP_FAILED")]
    Database(#[from] sqlx::Error),
    #[error("LW_AUTH_STARTUP_FAILED")]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use time::{Duration, OffsetDateTime};

    use super::{browser_routes, deployment_duration};

    #[test]
    fn browser_routes_register_without_conflicts() {
        let _router = browser_routes();
    }

    #[test]
    fn callback_session_expiry_uses_configured_lifetime_after_token_verification()
    -> Result<(), String> {
        let callback_at = OffsetDateTime::UNIX_EPOCH + Duration::seconds(1_000);
        let id_token_expiry = callback_at + Duration::seconds(5);
        let session_duration = deployment_duration(900)
            .map_err(|error| format!("configured duration: {}", error.diagnostic))?;
        let session_expiry = auth::configured_session_expiry(callback_at, session_duration)
            .map_err(|error| format!("configured session expiry: {error}"))?;

        assert_eq!(session_expiry, callback_at + Duration::seconds(900));
        assert!(session_expiry > id_token_expiry);
        Ok(())
    }
}
