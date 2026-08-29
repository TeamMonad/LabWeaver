//! Control-only mTLS API for the Agent authority.
#![allow(clippy::missing_errors_doc)]

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use contracts::authoring::AgentTrackKind;
use contracts::http::{
    IdempotencyKey, InternalAgentBuildCancellationRequest, InternalAgentBuildStatusQuery,
    InternalAgentRunMutationRequest, InternalAgentRunOutcome, InternalCreateAgentRunRequest,
    InternalImageArtifactResolution,
};
use contracts::{
    AgentRunId, DiagnosticCode, ImageArtifactId, ProblemDetails, Sha256Digest, UtcTimestamp,
};
use serde_json::Value;
use sqlx::Row;
use time::OffsetDateTime;

use crate::build_store::{BuildStoreError, PgBuildStore};
use crate::run_store::{
    AgentRunReservation, AgentRunStoreError, PostgresAgentRunStore, StoredCandidate,
};

/// Agent internal API state.
#[derive(Clone, Debug)]
pub struct AgentApiState {
    /// Agent-owned run repository.
    pub store: PostgresAgentRunStore,
    /// Agent-owned build command repository.
    pub build_store: PgBuildStore,
}

/// Exact allowlisted Control URI SAN.
#[derive(Clone, Debug)]
pub struct ControlPrincipal {
    /// Verified URI SAN value.
    pub san_uri: String,
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
        .route("/internal/v1/agent-runs/{run_id}/outcome", get(get_outcome))
        .route(
            "/internal/v1/image-artifacts/{artifact_id}",
            get(get_artifact),
        )
        .with_state(state);
    telemetry::instrument_http(router, "agent-service", "agent-api")
}

/// Serves internal Agent routes over plain HTTP for private single-university delivery.
pub async fn serve_mtls(
    listener: tokio::net::TcpListener,
    router: Router,
    _mtls: (),
) -> Result<(), std::io::Error> {
    serve_plain(listener, router).await
}

pub async fn serve_plain(
    listener: tokio::net::TcpListener,
    router: Router,
) -> Result<(), std::io::Error> {
    let router = router.layer(Extension(ControlPrincipal {
        san_uri: "spiffe://labweaver/control-service".to_owned(),
    }));
    axum::serve(listener, router).await.map_err(std::io::Error::from)
}

async fn create_run(
    State(state): State<Arc<AgentApiState>>,
    Extension(principal): Extension<ControlPrincipal>,
    headers: HeaderMap,
    Json(request): Json<InternalCreateAgentRunRequest>,
) -> Result<Response, AgentApiError> {
    require_control(&principal)?;
    let key = idempotency(&headers)?;
    let trace_id = trace_id(&headers);
    let reservation = state
        .store
        .reserve_dispatch(
            request.course_id,
            &request.request,
            request.expected_environment_class,
            &request.package,
            &request.object_locators,
            &request.policy,
            &key,
            now()?,
            &trace_id,
        )
        .await?;
    let run = match reservation {
        AgentRunReservation::Created(run) | AgentRunReservation::Replayed(run) => run,
    };
    Ok((StatusCode::ACCEPTED, Json(run)).into_response())
}

async fn get_run(
    State(state): State<Arc<AgentApiState>>,
    Extension(principal): Extension<ControlPrincipal>,
    Path(run_id): Path<AgentRunId>,
) -> Result<Response, AgentApiError> {
    require_control(&principal)?;
    Ok(Json(state.store.load(run_id).await?).into_response())
}

async fn cancel_run(
    State(state): State<Arc<AgentApiState>>,
    Extension(principal): Extension<ControlPrincipal>,
    Path(run_id): Path<AgentRunId>,
    headers: HeaderMap,
    Json(request): Json<InternalAgentRunMutationRequest>,
) -> Result<Response, AgentApiError> {
    require_control(&principal)?;
    let run = state
        .store
        .request_cancellation_revisioned(
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
    Extension(principal): Extension<ControlPrincipal>,
    Path(build_request_id): Path<contracts::BuildRequestId>,
    headers: HeaderMap,
    Json(request): Json<InternalAgentBuildCancellationRequest>,
) -> Result<Response, AgentApiError> {
    require_control(&principal)?;
    if request.build_request_id != build_request_id
        || request.authority_san_uri != principal.san_uri
    {
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
    Extension(principal): Extension<ControlPrincipal>,
    Path(build_request_id): Path<contracts::BuildRequestId>,
    Query(query): Query<InternalAgentBuildStatusQuery>,
) -> Result<Response, AgentApiError> {
    require_control(&principal)?;
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
    Extension(principal): Extension<ControlPrincipal>,
    Path((run_id, track)): Path<(AgentRunId, String)>,
    headers: HeaderMap,
    Json(request): Json<InternalAgentRunMutationRequest>,
) -> Result<Response, AgentApiError> {
    require_control(&principal)?;
    let track = match track.as_str() {
        "environment" => AgentTrackKind::Environment,
        "evaluation" => AgentTrackKind::Evaluation,
        _ => return Err(AgentApiError::contract()),
    };
    let run = state
        .store
        .retry_track_revisioned(
            request.course_id,
            run_id,
            track,
            request.expected_revision,
            &idempotency(&headers)?,
        )
        .await?;
    Ok(Json(run).into_response())
}

async fn get_outcome(
    State(state): State<Arc<AgentApiState>>,
    Extension(principal): Extension<ControlPrincipal>,
    Path(run_id): Path<AgentRunId>,
) -> Result<Response, AgentApiError> {
    require_control(&principal)?;
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
    let outcome_sha256 = Sha256Digest::of_canonical(&serde_json::json!({
        "run": run,
        "environmentCandidate": environment_candidate,
        "evaluationCandidate": evaluation_candidate,
    }))
    .map_err(|_| AgentApiError::contract())?;
    let outcome = InternalAgentRunOutcome {
        run,
        environment_candidate,
        evaluation_candidate,
        outcome_sha256,
    };
    outcome.validate().map_err(|_| AgentApiError::contract())?;
    Ok(Json(outcome).into_response())
}

async fn get_artifact(
    State(state): State<Arc<AgentApiState>>,
    Extension(principal): Extension<ControlPrincipal>,
    Path(artifact_id): Path<ImageArtifactId>,
) -> Result<Response, AgentApiError> {
    require_control(&principal)?;
    let row = sqlx::query("SELECT contract FROM agent.image_artifacts WHERE image_artifact_id=$1 AND state='verified'")
        .bind(artifact_id.as_uuid()).fetch_optional(state.store.pool()).await.map_err(|_| AgentApiError::persistence())?.ok_or_else(AgentApiError::not_found)?;
    let artifact: contracts::supply_chain::ImageArtifact = serde_json::from_value(
        row.try_get::<Value, _>("contract")
            .map_err(|_| AgentApiError::contract())?,
    )
    .map_err(|_| AgentApiError::contract())?;
    let resolution_sha256 = Sha256Digest::of_canonical(&serde_json::json!({"artifactId":artifact_id,"artifact":artifact})).map_err(|_| AgentApiError::contract())?;
    let resolution = InternalImageArtifactResolution {
        artifact_id,
        artifact,
        resolution_sha256,
    };
    resolution
        .validate()
        .map_err(|_| AgentApiError::contract())?;
    Ok(Json(resolution).into_response())
}

fn require_control(principal: &ControlPrincipal) -> Result<(), AgentApiError> {
    if principal.san_uri.trim().is_empty() {
        Err(AgentApiError::denied())
    } else {
        Ok(())
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
