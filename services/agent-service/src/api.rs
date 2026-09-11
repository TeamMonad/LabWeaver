//! Control-only service API for the Agent authority.
#![allow(clippy::missing_errors_doc)]

use std::sync::Arc;

use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use contracts::authoring::AgentTrackKind;
use contracts::http::{
    AgentLlmReviewQuery, AgentWorkExecutionIntentQuery, GeneratedArtifactQuery, IdempotencyKey,
    InternalAgentBuildCancellationRequest, InternalAgentBuildStatusQuery,
    InternalAgentLlmReviewRequest, InternalAgentRunMutationRequest, InternalAgentRunOutcome,
    InternalApproveWorkConfigurationRequest, InternalCreateAgentRunRequest,
    InternalImageArtifactResolution,
};
use contracts::{
    AgentRunId, ArtifactId, DiagnosticCode, ImageArtifactId, ProblemDetails, UtcTimestamp,
};
use serde_json::Value;
use sqlx::Row;
use time::OffsetDateTime;

use crate::build_store::{BuildStoreError, PgBuildStore};
use crate::generated_artifacts::{GeneratedArtifactStore, GeneratedArtifactStoreError};
use crate::llm_review::{LlmReviewStore, LlmReviewStoreError};
use crate::run_store::{
    AgentRunReservation, AgentRunStoreError, PostgresAgentRunStore, StoredCandidate,
};

/// Permission required for every Control-to-Agent request.
pub const CONTROL_PERMISSION: &str = "agent.control.invoke";
/// Evaluation permission for creating an advisory review.
pub const LLM_REVIEW_CREATE_PERMISSION: &str = "agent.llm_review.create";
/// Evaluation permission for reading an advisory review.
pub const LLM_REVIEW_READ_PERMISSION: &str = "agent.llm_review.read";
/// Evaluation permission for cancelling an advisory review.
pub const LLM_REVIEW_CANCEL_PERMISSION: &str = "agent.llm_review.cancel";

/// Agent internal API state.
#[derive(Clone, Debug)]
pub struct AgentApiState {
    /// Agent-owned run repository.
    pub store: PostgresAgentRunStore,
    /// Agent-owned build command repository.
    pub build_store: PgBuildStore,
    /// Agent-owned generated object resolutions.
    pub generated_artifacts: GeneratedArtifactStore,
    /// Agent-owned advisory review queue and receipt store.
    pub llm_reviews: LlmReviewStore,
}

/// Builds all Control-to-Agent routes.
pub fn router(state: Arc<AgentApiState>) -> Router {
    let router = Router::new()
        .route("/internal/v1/agent-runs", post(create_run))
        .route("/internal/v1/agent-runs/{run_id}", get(get_run))
        .route("/internal/v1/agent-runs/{run_id}/cancel", post(cancel_run))
        .route(
            "/internal/v1/build-requests/{build_request_id}/cancel",
            post(cancel_build),
        )
        .route(
            "/internal/v1/build-requests/{build_request_id}",
            get(get_build),
        )
        .route(
            "/internal/v1/agent-runs/{run_id}/tracks/{track}/retry",
            post(retry_track),
        )
        .route(
            "/internal/v1/agent-runs/{run_id}/work-configuration/approve",
            post(approve_work_configuration),
        )
        .route("/internal/v1/agent-runs/{run_id}/outcome", get(get_outcome))
        .route(
            "/internal/v1/image-artifacts/{artifact_id}",
            get(get_artifact),
        )
        .route(
            "/internal/v1/generated-artifacts/{artifact_id}",
            get(get_generated_artifact),
        )
        .route(
            "/internal/v1/agent-runs/{run_id}/work-execution-intent",
            get(get_work_execution_intent),
        )
        .route("/internal/v1/llm-reviews", post(create_llm_review))
        .route(
            "/internal/v1/llm-reviews/{task_run_id}",
            get(get_llm_review),
        )
        .route(
            "/internal/v1/llm-reviews/{task_run_id}/cancel",
            post(cancel_llm_review),
        )
        .with_state(state);
    telemetry::instrument_http(router, "agent-service", "agent-api")
}

/// Applies the service-account JWT boundary to every Agent route.
///
/// The verifier is the only source of caller identity. The TLS transport
/// protects the bearer token in transit, while the configured permission binds
/// the route tree to Control's service account capability.
pub fn with_service_auth(router: Router, verifier: Arc<auth::ServiceTokenVerifier>) -> Router {
    router.layer(middleware::from_fn_with_state(
        verifier,
        require_service_token,
    ))
}

async fn require_service_token(
    State(verifier): State<Arc<auth::ServiceTokenVerifier>>,
    mut request: Request,
    next: Next,
) -> Response {
    let permission = request_permission(request.method(), request.uri().path());
    match verifier
        .authenticate_with_permission(request.headers(), permission)
        .await
    {
        Ok(identity) => {
            request.extensions_mut().insert(identity);
            next.run(request).await
        }
        Err(error) => AgentApiError::service_auth(&error).into_response(),
    }
}

async fn create_run(
    State(state): State<Arc<AgentApiState>>,
    caller: Option<Extension<auth::ServiceIdentity>>,
    headers: HeaderMap,
    Json(request): Json<InternalCreateAgentRunRequest>,
) -> Result<Response, AgentApiError> {
    require_control(caller)?;
    let key = idempotency(&headers)?;
    let trace_id = trace_id(&headers);
    let reservation = state
        .store
        .reserve_internal_dispatch(&request, &key, now()?, &trace_id)
        .await?;
    let run = match reservation {
        AgentRunReservation::Created(run) | AgentRunReservation::Replayed(run) => run,
    };
    Ok((StatusCode::ACCEPTED, Json(run)).into_response())
}

async fn get_run(
    State(state): State<Arc<AgentApiState>>,
    caller: Option<Extension<auth::ServiceIdentity>>,
    Path(run_id): Path<AgentRunId>,
) -> Result<Response, AgentApiError> {
    require_control(caller)?;
    Ok(Json(state.store.load(run_id).await?).into_response())
}

async fn cancel_run(
    State(state): State<Arc<AgentApiState>>,
    caller: Option<Extension<auth::ServiceIdentity>>,
    Path(run_id): Path<AgentRunId>,
    headers: HeaderMap,
    Json(request): Json<InternalAgentRunMutationRequest>,
) -> Result<Response, AgentApiError> {
    require_control(caller)?;
    let run = state
        .store
        .request_cancellation_revisioned(
            request.project_id,
            request.course_id,
            run_id,
            request.expected_revision,
            &idempotency(&headers)?,
            now()?,
        )
        .await?;
    Ok(Json(run).into_response())
}

async fn cancel_build(
    State(state): State<Arc<AgentApiState>>,
    caller: Option<Extension<auth::ServiceIdentity>>,
    Path(build_request_id): Path<contracts::BuildRequestId>,
    headers: HeaderMap,
    Json(request): Json<InternalAgentBuildCancellationRequest>,
) -> Result<Response, AgentApiError> {
    require_control(caller)?;
    if request.build_request_id != build_request_id {
        return Err(AgentApiError::denied());
    }
    let result = state
        .build_store
        .request_cancellation(&request, &idempotency(&headers)?)
        .await?;
    Ok((StatusCode::ACCEPTED, Json(result)).into_response())
}

async fn get_build(
    State(state): State<Arc<AgentApiState>>,
    caller: Option<Extension<auth::ServiceIdentity>>,
    Path(build_request_id): Path<contracts::BuildRequestId>,
    Query(query): Query<InternalAgentBuildStatusQuery>,
) -> Result<Response, AgentApiError> {
    require_control(caller)?;
    Ok(Json(
        state
            .build_store
            .load_status(build_request_id, &query)
            .await?,
    )
    .into_response())
}

async fn retry_track(
    State(state): State<Arc<AgentApiState>>,
    caller: Option<Extension<auth::ServiceIdentity>>,
    Path((run_id, track)): Path<(AgentRunId, String)>,
    headers: HeaderMap,
    Json(request): Json<InternalAgentRunMutationRequest>,
) -> Result<Response, AgentApiError> {
    require_control(caller)?;
    let track = match track.as_str() {
        "environment" => AgentTrackKind::Environment,
        "evaluation" => AgentTrackKind::Evaluation,
        "work_configuration" => AgentTrackKind::WorkConfiguration,
        _ => return Err(AgentApiError::contract()),
    };
    let run = state
        .store
        .retry_track_revisioned(
            request.project_id,
            request.course_id,
            run_id,
            track,
            request.expected_revision,
            &idempotency(&headers)?,
        )
        .await?;
    Ok(Json(run).into_response())
}

async fn approve_work_configuration(
    State(state): State<Arc<AgentApiState>>,
    caller: Option<Extension<auth::ServiceIdentity>>,
    Path(run_id): Path<AgentRunId>,
    headers: HeaderMap,
    Json(request): Json<InternalApproveWorkConfigurationRequest>,
) -> Result<Response, AgentApiError> {
    require_control(caller)?;
    let run = state
        .store
        .approve_work_configuration(run_id, &request, &idempotency(&headers)?, now()?)
        .await?;
    Ok((StatusCode::ACCEPTED, Json(run)).into_response())
}

async fn get_outcome(
    State(state): State<Arc<AgentApiState>>,
    caller: Option<Extension<auth::ServiceIdentity>>,
    Path(run_id): Path<AgentRunId>,
) -> Result<Response, AgentApiError> {
    require_control(caller)?;
    let run = state.store.load(run_id).await?;
    let checkpoints = state.store.load_checkpoints(run_id).await?;
    let mut environment_candidate = None;
    let mut evaluation_candidate = None;
    for checkpoint in checkpoints {
        match checkpoint.candidate {
            Some(StoredCandidate::Environment(candidate)) => {
                environment_candidate = Some(candidate);
            }
            Some(StoredCandidate::Evaluation(candidate)) => evaluation_candidate = Some(candidate),
            None => {}
        }
    }
    let outcome = InternalAgentRunOutcome {
        plan: run.plan.clone(),
        run,
        environment_candidate,
        evaluation_candidate,
    };
    outcome.validate().map_err(|_| AgentApiError::contract())?;
    Ok(Json(outcome).into_response())
}

async fn get_artifact(
    State(state): State<Arc<AgentApiState>>,
    caller: Option<Extension<auth::ServiceIdentity>>,
    Path(artifact_id): Path<ImageArtifactId>,
) -> Result<Response, AgentApiError> {
    require_control(caller)?;
    let row = sqlx::query("SELECT contract FROM agent.image_artifacts WHERE image_artifact_id=$1 AND state='verified'")
        .bind(artifact_id.as_uuid()).fetch_optional(state.store.pool()).await.map_err(|_| AgentApiError::persistence())?.ok_or_else(AgentApiError::not_found)?;
    let artifact: contracts::supply_chain::ImageArtifact = serde_json::from_value(
        row.try_get::<Value, _>("contract")
            .map_err(|_| AgentApiError::contract())?,
    )
    .map_err(|_| AgentApiError::contract())?;
    let resolution = InternalImageArtifactResolution {
        artifact_id,
        artifact,
    };
    resolution
        .validate()
        .map_err(|_| AgentApiError::contract())?;
    Ok(Json(resolution).into_response())
}

async fn get_generated_artifact(
    State(state): State<Arc<AgentApiState>>,
    caller: Option<Extension<auth::ServiceIdentity>>,
    Path(artifact_id): Path<ArtifactId>,
    Query(query): Query<GeneratedArtifactQuery>,
) -> Result<Response, AgentApiError> {
    require_control(caller)?;
    Ok(Json(state.generated_artifacts.get(artifact_id, &query).await?).into_response())
}

async fn get_work_execution_intent(
    State(state): State<Arc<AgentApiState>>,
    caller: Option<Extension<auth::ServiceIdentity>>,
    Path(run_id): Path<AgentRunId>,
    Query(query): Query<AgentWorkExecutionIntentQuery>,
) -> Result<Response, AgentApiError> {
    require_control(caller)?;
    if query.execution_id.is_nil() {
        return Err(AgentApiError::contract());
    }
    let metadata = state
        .store
        .work_execution_intent_metadata(run_id, &query)
        .await?;
    Ok(Json(metadata).into_response())
}

async fn create_llm_review(
    State(state): State<Arc<AgentApiState>>,
    caller: Option<Extension<auth::ServiceIdentity>>,
    headers: HeaderMap,
    Json(request): Json<InternalAgentLlmReviewRequest>,
) -> Result<Response, AgentApiError> {
    require_permission(caller, LLM_REVIEW_CREATE_PERMISSION)?;
    let receipt = state
        .llm_reviews
        .enqueue(&request, &idempotency(&headers)?, now()?)
        .await?;
    Ok((StatusCode::ACCEPTED, Json(receipt)).into_response())
}

async fn get_llm_review(
    State(state): State<Arc<AgentApiState>>,
    caller: Option<Extension<auth::ServiceIdentity>>,
    Path(task_run_id): Path<contracts::TaskRunId>,
    Query(query): Query<AgentLlmReviewQuery>,
) -> Result<Response, AgentApiError> {
    require_permission(caller, LLM_REVIEW_READ_PERMISSION)?;
    let receipt = state.llm_reviews.get(task_run_id, &query).await?;
    Ok(Json(receipt).into_response())
}

async fn cancel_llm_review(
    State(state): State<Arc<AgentApiState>>,
    caller: Option<Extension<auth::ServiceIdentity>>,
    Path(task_run_id): Path<contracts::TaskRunId>,
    Query(query): Query<AgentLlmReviewQuery>,
    headers: HeaderMap,
) -> Result<Response, AgentApiError> {
    require_permission(caller, LLM_REVIEW_CANCEL_PERMISSION)?;
    let receipt = state
        .llm_reviews
        .cancel(task_run_id, &query, &idempotency(&headers)?, now()?)
        .await?;
    Ok(Json(receipt).into_response())
}

fn require_control(caller: Option<Extension<auth::ServiceIdentity>>) -> Result<(), AgentApiError> {
    require_permission(caller, CONTROL_PERMISSION)
}

fn require_permission(
    caller: Option<Extension<auth::ServiceIdentity>>,
    permission: &str,
) -> Result<(), AgentApiError> {
    match caller {
        Some(Extension(identity)) if identity.allows(permission) => Ok(()),
        _ => Err(AgentApiError::denied()),
    }
}

fn request_permission(method: &Method, path: &str) -> &'static str {
    match (method, path) {
        (&Method::POST, "/internal/v1/llm-reviews") => LLM_REVIEW_CREATE_PERMISSION,
        (&Method::GET, path) if path.starts_with("/internal/v1/llm-reviews/") => {
            LLM_REVIEW_READ_PERMISSION
        }
        (&Method::POST, path)
            if path.starts_with("/internal/v1/llm-reviews/") && path.ends_with("/cancel") =>
        {
            LLM_REVIEW_CANCEL_PERMISSION
        }
        _ => CONTROL_PERMISSION,
    }
}
fn idempotency(headers: &HeaderMap) -> Result<IdempotencyKey, AgentApiError> {
    headers
        .get("Idempotency-Key")
        .and_then(|value| value.to_str().ok())
        .ok_or_else(AgentApiError::contract)
        .and_then(|value| IdempotencyKey::parse(value).map_err(|_| AgentApiError::contract()))
}
fn trace_id(headers: &HeaderMap) -> String {
    let _ = headers;
    telemetry::current_request_context()
        .unwrap_or_else(telemetry::RequestContext::generate)
        .trace_id()
        .to_owned()
}
fn now() -> Result<UtcTimestamp, AgentApiError> {
    let value = OffsetDateTime::now_utc();
    let value = value
        .replace_nanosecond((value.nanosecond() / 1_000_000) * 1_000_000)
        .map_err(|_| AgentApiError::contract())?;
    UtcTimestamp::from_utc(value).map_err(|_| AgentApiError::contract())
}

#[derive(Debug)]
struct AgentApiError {
    status: StatusCode,
    diagnostic: &'static str,
    retryable: bool,
}
impl AgentApiError {
    fn contract() -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            diagnostic: "LW_CONTRACT_DOCUMENT_INVALID",
            retryable: false,
        }
    }
    fn denied() -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            diagnostic: "LW_AUTH_SERVICE_IDENTITY_DENIED",
            retryable: false,
        }
    }
    fn not_found() -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            diagnostic: "LW_AGENT_RUN_STATE_CONFLICT",
            retryable: false,
        }
    }
    fn persistence() -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            diagnostic: "LW_AGENT_PERSISTENCE_FAILED",
            retryable: true,
        }
    }

    fn service_auth(error: &auth::ServiceAuthError) -> Self {
        let (status, diagnostic, retryable) = match error {
            auth::ServiceAuthError::CredentialsMissing => (
                StatusCode::UNAUTHORIZED,
                "LW_AUTH_SERVICE_CREDENTIALS_MISSING",
                false,
            ),
            auth::ServiceAuthError::TokenRejected => (
                StatusCode::UNAUTHORIZED,
                "LW_AUTH_SERVICE_TOKEN_REJECTED",
                false,
            ),
            auth::ServiceAuthError::TokenExpired => (
                StatusCode::UNAUTHORIZED,
                "LW_AUTH_SERVICE_TOKEN_EXPIRED",
                false,
            ),
            auth::ServiceAuthError::PermissionDenied => (
                StatusCode::FORBIDDEN,
                "LW_AUTH_SERVICE_PERMISSION_DENIED",
                false,
            ),
            auth::ServiceAuthError::InvalidConfig => (
                StatusCode::SERVICE_UNAVAILABLE,
                "LW_AUTH_SERVICE_CONFIG_INVALID",
                false,
            ),
            auth::ServiceAuthError::JwksUnavailable => (
                StatusCode::SERVICE_UNAVAILABLE,
                "LW_AUTH_SERVICE_JWKS_UNAVAILABLE",
                true,
            ),
            auth::ServiceAuthError::EndpointTransport => (
                StatusCode::SERVICE_UNAVAILABLE,
                "LW_AUTH_SERVICE_ENDPOINT_TRANSPORT_REJECTED",
                false,
            ),
            auth::ServiceAuthError::HttpClient => (
                StatusCode::SERVICE_UNAVAILABLE,
                "LW_AUTH_SERVICE_HTTP_CLIENT_FAILED",
                false,
            ),
        };
        Self {
            status,
            diagnostic,
            retryable,
        }
    }
}

impl From<GeneratedArtifactStoreError> for AgentApiError {
    fn from(error: GeneratedArtifactStoreError) -> Self {
        match error {
            GeneratedArtifactStoreError::NotFound => Self::not_found(),
            GeneratedArtifactStoreError::InvalidMetadata
            | GeneratedArtifactStoreError::IdentityMismatch => Self::contract(),
            GeneratedArtifactStoreError::Persistence | GeneratedArtifactStoreError::Storage(_) => {
                Self::persistence()
            }
        }
    }
}

impl From<LlmReviewStoreError> for AgentApiError {
    fn from(error: LlmReviewStoreError) -> Self {
        let status = match error {
            LlmReviewStoreError::CourseMismatch => StatusCode::FORBIDDEN,
            LlmReviewStoreError::NotFound => StatusCode::NOT_FOUND,
            LlmReviewStoreError::IdempotencyConflict
            | LlmReviewStoreError::InProgress
            | LlmReviewStoreError::StateConflict
            | LlmReviewStoreError::LeaseLost => StatusCode::CONFLICT,
            LlmReviewStoreError::PersistenceFailed => StatusCode::SERVICE_UNAVAILABLE,
            LlmReviewStoreError::IdentityMismatch
            | LlmReviewStoreError::WorkerIdentityInvalid
            | LlmReviewStoreError::InvalidContract
            | LlmReviewStoreError::ClockInvalid => StatusCode::UNPROCESSABLE_ENTITY,
        };
        Self {
            status,
            diagnostic: error.diagnostic_code(),
            retryable: matches!(
                error,
                LlmReviewStoreError::InProgress | LlmReviewStoreError::PersistenceFailed
            ),
        }
    }
}

impl From<AgentRunStoreError> for AgentApiError {
    fn from(error: AgentRunStoreError) -> Self {
        let status = match error {
            AgentRunStoreError::CourseMismatch => StatusCode::FORBIDDEN,
            AgentRunStoreError::RunNotFound => StatusCode::NOT_FOUND,
            AgentRunStoreError::IdempotencyConflict
            | AgentRunStoreError::RunInProgress
            | AgentRunStoreError::StateConflict
            | AgentRunStoreError::LeaseLost => StatusCode::CONFLICT,
            AgentRunStoreError::PersistenceFailed => StatusCode::SERVICE_UNAVAILABLE,
            _ => StatusCode::UNPROCESSABLE_ENTITY,
        };
        Self {
            status,
            diagnostic: error.diagnostic_code(),
            retryable: matches!(
                error,
                AgentRunStoreError::RunInProgress | AgentRunStoreError::PersistenceFailed
            ),
        }
    }
}
impl From<BuildStoreError> for AgentApiError {
    fn from(error: BuildStoreError) -> Self {
        let status = match error {
            BuildStoreError::AuthorityMismatch | BuildStoreError::CourseMismatch => {
                StatusCode::FORBIDDEN
            }
            BuildStoreError::NotFound => StatusCode::NOT_FOUND,
            BuildStoreError::StateConflict
            | BuildStoreError::IdempotencyConflict
            | BuildStoreError::RequestInProgress
            | BuildStoreError::FenceLost
            | BuildStoreError::RetryUnsafe
            | BuildStoreError::ClockInvalid => StatusCode::CONFLICT,
            BuildStoreError::ConfigurationInvalid
            | BuildStoreError::ContractInvalid
            | BuildStoreError::IdentityMismatch
            | BuildStoreError::RequestExpired => StatusCode::UNPROCESSABLE_ENTITY,
            BuildStoreError::PersistenceFailed | BuildStoreError::Database(_) => {
                StatusCode::SERVICE_UNAVAILABLE
            }
        };
        let retryable = matches!(
            error,
            BuildStoreError::RequestInProgress
                | BuildStoreError::PersistenceFailed
                | BuildStoreError::Database(_)
        );
        let diagnostic = match error {
            BuildStoreError::ConfigurationInvalid => "LW_AGENT_BUILD_STORE_CONFIGURATION_INVALID",
            BuildStoreError::ContractInvalid => "LW_AGENT_BUILD_CONTRACT_INVALID",
            BuildStoreError::IdentityMismatch => "LW_AGENT_BUILD_IDENTITY_MISMATCH",
            BuildStoreError::AuthorityMismatch => "LW_AGENT_BUILD_AUTHORITY_MISMATCH",
            BuildStoreError::CourseMismatch => "LW_AGENT_BUILD_COURSE_MISMATCH",
            BuildStoreError::NotFound => "LW_AGENT_BUILD_NOT_FOUND",
            BuildStoreError::StateConflict => "LW_AGENT_BUILD_STATE_CONFLICT",
            BuildStoreError::IdempotencyConflict => "LW_AGENT_BUILD_IDEMPOTENCY_CONFLICT",
            BuildStoreError::RequestInProgress => "LW_AGENT_BUILD_REQUEST_IN_PROGRESS",
            BuildStoreError::RequestExpired => "LW_AGENT_BUILD_CANCELLATION_EXPIRED",
            BuildStoreError::FenceLost => "LW_AGENT_BUILD_FENCE_LOST",
            BuildStoreError::RetryUnsafe => "LW_AGENT_BUILD_RETRY_WITHOUT_CLEANUP_FORBIDDEN",
            BuildStoreError::ClockInvalid => "LW_AGENT_BUILD_CLOCK_INVALID",
            BuildStoreError::PersistenceFailed => "LW_AGENT_BUILD_PERSISTENCE_FAILED",
            BuildStoreError::Database(_) => "LW_AGENT_BUILD_DATABASE_FAILED",
        };
        Self {
            status,
            diagnostic,
            retryable,
        }
    }
}
impl IntoResponse for AgentApiError {
    fn into_response(self) -> Response {
        let context = telemetry::current_request_context()
            .unwrap_or_else(telemetry::RequestContext::generate);
        let request_id = context.request_id().to_owned();
        let problem = ProblemDetails {
            problem_type: format!(
                "urn:labweaver:problem:{}",
                self.diagnostic.to_ascii_lowercase()
            ),
            title: "Agent request blocked".to_owned(),
            status: self.status.as_u16(),
            detail: "The request was rejected by a fail-closed Agent boundary.".to_owned(),
            instance: format!("urn:labweaver:request:{request_id}"),
            diagnostic_code: DiagnosticCode::registered(self.diagnostic),
            request_id,
            trace_id: Some(context.trace_id().to_owned()),
            retryable: self.retryable,
            violations: Vec::new(),
        };
        if self.retryable || self.status.is_client_error() {
            tracing::warn!(
                event = "agent.request.rejected",
                component = "api-error-boundary",
                operation = "http.request",
                outcome = "rejected",
                duration_ms = 0_u64,
                diagnostic_code = self.diagnostic,
                error_kind = "request_rejected",
                failure_stage = "agent.request.finalize",
                retryable = self.retryable,
                safe_detail = "request_rejected",
            );
        } else {
            tracing::error!(
                event = "agent.request.failed",
                component = "api-error-boundary",
                operation = "http.request",
                outcome = "failed",
                duration_ms = 0_u64,
                diagnostic_code = self.diagnostic,
                error_kind = "terminal_api_failure",
                failure_stage = "agent.request.finalize",
                retryable = false,
                safe_detail = "redacted_unclassified",
            );
        }
        (self.status, Json(problem)).into_response()
    }
}
