//! Access Service browser BFF entry points.

mod grants;
mod proxy;

use std::{collections::BTreeSet, net::SocketAddr, str::FromStr, sync::Arc};

use auth::{
    AccessAuthFile, AuthConfig, AuthorizationContext, BffSession, EnvironmentOwnerResolverClient,
    KeyRing, OidcProvider, OidcTransaction, RoleMappings, TransportSecurityMode, authorize,
    build_backchannel_logout_authorizer, build_bearer_authorizer, cleanup_expired_auth_state,
    consume_backchannel_logout, consume_oidc_transaction, create_bff_session,
    extract_mtls_principal, extract_platform_roles, load_bff_session, load_logout_hint,
    load_membership_snapshot, load_mtls_server_config, no_redirect_http_client,
    require_service_identity, revoke_bff_session, upsert_actor,
};
use axum::{
    Json, Router,
    extract::{Extension, Form, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Redirect, Response},
    routing::{delete, get, post},
};
use contracts::{
    AuthSession, AuthenticatedActor, AuthorizationDecision, AuthorizationDecisionRequest,
    AuthorizationScope, CsrfTokenResponse, OperationScopeKind, Revision, UtcTimestamp,
    environment::EnvironmentOwnerResolutionRequest, operation_authorization,
};
use hyper_util::{
    rt::{TokioExecutor, TokioIo},
    server::conn::auto::Builder as HyperBuilder,
    service::TowerToHyperService,
};
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
    role_mappings: RoleMappings,
    pool: PgPool,
    key_ring: KeyRing,
    owner_resolver: EnvironmentOwnerResolverClient,
    control_proxy: proxy::ControlGatewayProxy,
    environment_proxy: proxy::ControlGatewayProxy,
    evaluation_proxy: proxy::ControlGatewayProxy,
    runtime_proxy: proxy::RuntimeGatewayProxy,
    metrics: telemetry::PrometheusHandle,
    nats: async_nats::Client,
}

#[derive(Clone)]
struct MtlsPrincipal {
    san_uri: String,
}

#[tokio::main]
async fn main() -> Result<(), StartupError> {
    telemetry::init(env!("CARGO_PKG_NAME"))?;
    let metrics = telemetry::init_metrics(env!("CARGO_PKG_NAME"))?;
    let deployment = load_deployment()?;
    let bind =
        SocketAddr::from_str(&deployment.browser.bind_addr).map_err(|_| StartupError::Config)?;
    let internal_bind = SocketAddr::from_str(&deployment.internal_mtls.bind_addr)
        .map_err(|_| StartupError::Config)?;
    let state = build_app_state(deployment, metrics).await?;
    let router = browser_router(Arc::clone(&state));
    let internal_router = internal_router(Arc::clone(&state));
    let listener = tokio::net::TcpListener::bind(bind).await?;
    let internal_listener = tokio::net::TcpListener::bind(internal_bind).await?;
    let mtls = load_mtls_server_config(&state.deployment.internal_mtls)?;
    tokio::select! {
        result = axum::serve(listener, router) => result.map_err(StartupError::from)?,
        result = serve_internal_mtls(internal_listener, internal_router, mtls) => result?,
        result = auth_cleanup_loop(Arc::clone(&state)) => result?,
        result = grants::activation_loop(Arc::clone(&state)) => result?,
        result = grants::maintenance_loop(Arc::clone(&state)) => result?,
        result = grants::outbox_loop(Arc::clone(&state)) => result?,
        result = grants::environment_revocation_loop(Arc::clone(&state)) => result?,
    }
    Ok(())
}

fn browser_router(state: Arc<AppState>) -> Router {
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
            "/api/v1/courses/{*control_path}",
            axum::routing::any(proxy::forward_control),
        )
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
        .with_state(state)
}

fn internal_router(state: Arc<AppState>) -> Router {
    Router::new()
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
        .with_state(state)
}

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
    let role_mappings = RoleMappings::parse(deployment.oidc.role_mappings.clone())?;
    let resolver_config = deployment.environment_owner_resolver.contract();
    let resolver_ca = resolver_secret(&deployment, &resolver_config.ca_certificate_locator)?;
    let resolver_certificate =
        resolver_secret(&deployment, &resolver_config.client_certificate_locator)?;
    let resolver_key = resolver_secret(&deployment, &resolver_config.client_private_key_locator)?;
    let owner_resolver = EnvironmentOwnerResolverClient::new(
        &resolver_config,
        &resolver_ca,
        &resolver_certificate,
        &resolver_key,
        std::time::Duration::from_millis(
            deployment
                .environment_owner_resolver
                .retry_backoff_milliseconds,
        ),
        deployment.transport_security,
    )?;
    let control_proxy = build_control_proxy(&deployment)?;
    let environment_proxy = build_service_proxy(&deployment, &deployment.environment_gateway)?;
    let evaluation_proxy = build_service_proxy(&deployment, &deployment.evaluation_gateway)?;
    let runtime_proxy = proxy::RuntimeGatewayProxy::new(&deployment.environment_gateway)?;
    let nats = grants::connect_nats(&deployment.nats).await?;
    Ok(Arc::new(AppState {
        config,
        deployment,
        provider,
        oidc_http,
        bearer_authorizer,
        backchannel_logout_authorizer,
        role_mappings,
        pool,
        key_ring,
        owner_resolver,
        control_proxy,
        environment_proxy,
        evaluation_proxy,
        runtime_proxy,
        metrics,
        nats,
    }))
}

fn build_control_proxy(
    deployment: &AccessAuthFile,
) -> Result<proxy::ControlGatewayProxy, StartupError> {
    build_service_proxy(deployment, &deployment.control_gateway)
}

fn build_service_proxy(
    deployment: &AccessAuthFile,
    config: &auth::ControlGatewayFileConfig,
) -> Result<proxy::ControlGatewayProxy, StartupError> {
    let ca = resolver_secret(deployment, &config.ca_certificate_locator)?;
    let certificate = resolver_secret(deployment, &config.client_certificate_locator)?;
    let key = resolver_secret(deployment, &config.client_private_key_locator)?;
    Ok(proxy::ControlGatewayProxy::new(
        config,
        &ca,
        &certificate,
        &key,
        deployment.transport_security,
    )?)
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
            Err(error) => {
                metrics::counter!("labweaver_auth_cleanup_runs", "result" => "failed").increment(1);
                tracing::error!(
                    event = "auth.cleanup.failed",
                    diagnostic = "LW_AUTH_MEMBERSHIP_UNAVAILABLE",
                    error = %error
                );
            }
        }
    }
}

async fn serve_internal_mtls(
    listener: tokio::net::TcpListener,
    router: Router,
    mtls: auth::MtlsServerConfig,
) -> Result<(), StartupError> {
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::clone(&mtls.server_config));
    loop {
        let (stream, _) = listener.accept().await?;
        let acceptor = acceptor.clone();
        let router = router.clone();
        let allowed_san_uris = mtls.allowed_san_uris.clone();
        tokio::spawn(async move {
            let tls = match acceptor.accept(stream).await {
                Ok(tls) => tls,
                Err(error) => {
                    metrics::counter!("labweaver_auth_mtls_handshakes", "result" => "denied")
                        .increment(1);
                    tracing::warn!(event = "auth.mtls.handshake_denied", diagnostic = "LW_AUTH_SERVICE_IDENTITY_DENIED", error = %error);
                    return;
                }
            };
            let Some(peer) = tls
                .get_ref()
                .1
                .peer_certificates()
                .and_then(|certificates| certificates.first())
            else {
                tracing::warn!(
                    event = "auth.mtls.peer_denied",
                    diagnostic = "LW_AUTH_SERVICE_IDENTITY_DENIED"
                );
                return;
            };
            let san_uri = match extract_mtls_principal(peer, &allowed_san_uris) {
                Ok(principal) => principal,
                Err(error) => {
                    metrics::counter!("labweaver_auth_mtls_handshakes", "result" => "denied")
                        .increment(1);
                    tracing::warn!(event = "auth.mtls.peer_denied", diagnostic = "LW_AUTH_SERVICE_IDENTITY_DENIED", error = %error);
                    return;
                }
            };
            metrics::counter!("labweaver_auth_mtls_handshakes", "result" => "accepted")
                .increment(1);
            let service = router.layer(Extension(MtlsPrincipal { san_uri }));
            let io = TokioIo::new(tls);
            if let Err(error) = HyperBuilder::new(TokioExecutor::new())
                .serve_connection_with_upgrades(io, TowerToHyperService::new(service))
                .await
            {
                tracing::warn!(event = "auth.mtls.connection_failed", diagnostic = "LW_AUTH_SERVICE_IDENTITY_DENIED", error = %error);
            }
        });
    }
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
    let state_hash = contracts::Sha256Digest::of_bytes(transaction.state.as_bytes()).to_string();
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
    let expires_at = std::cmp::min(
        identity.expires_at,
        now + deployment_duration(state.config.session_ttl_seconds)?,
    );
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
    metrics::counter!("labweaver_auth_sessions", "event" => "backchannel_logout").increment(1);
    Ok(StatusCode::NO_CONTENT)
}

async fn authorization_decision(
    State(state): State<Arc<AppState>>,
    Extension(principal): Extension<MtlsPrincipal>,
    Json(request): Json<AuthorizationDecisionRequest>,
) -> Result<Json<AuthorizationDecision>, ApiError> {
    let started = std::time::Instant::now();
    let now = OffsetDateTime::now_utc();
    require_service_identity(&state.pool, &principal.san_uri, now)
        .await
        .map_err(ApiError::from)?;
    let policy = operation_authorization(&request.operation_id)
        .ok_or_else(|| ApiError::forbidden("LW_AUTH_SCOPE_DENIED"))?;
    if !scope_matches_kind(&request.scope, policy.scope) {
        return Err(ApiError::forbidden("LW_AUTH_SCOPE_DENIED"));
    }
    if let AuthorizationScope::Service { service_id } = &request.scope {
        if service_id != &principal.san_uri {
            return Err(ApiError::forbidden("LW_AUTH_SERVICE_IDENTITY_DENIED"));
        }
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
        actor_id = %decision.actor.actor_id,
        operation = policy.operation_id,
        scope = ?policy.scope,
        decision = "permit",
        diagnostic = "LW_AUTH_DECISION_PERMIT"
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
    Extension(principal): Extension<MtlsPrincipal>,
) -> Result<impl IntoResponse, ApiError> {
    require_service_identity(&state.pool, &principal.san_uri, OffsetDateTime::now_utc())
        .await
        .map_err(ApiError::from)?;
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
            if let Some(member_expiry) = membership.expires_at {
                if member_expiry.get() < expiry.get() {
                    expiry = member_expiry;
                }
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
            if let Some(member_expiry) = membership.expires_at {
                if member_expiry.get() < expiry.get() {
                    expiry = member_expiry;
                }
            }
            scopes.push(AuthorizationScope::Project {
                course_id: membership.course_id,
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
            auth::OwnerResolverClientError::Configuration
            | auth::OwnerResolverClientError::CertificateMaterial => {
                Self::internal("LW_AUTH_CONFIG_BINDING_MISSING")
            }
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
    Mtls(#[from] auth::MtlsError),
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
    Database(#[from] sqlx::Error),
    #[error("LW_AUTH_STARTUP_FAILED")]
    Io(#[from] std::io::Error),
}
