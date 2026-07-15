//! Control-only mTLS API for the Agent authority.
#![allow(clippy::missing_errors_doc)]

use std::sync::Arc;

use auth::extract_mtls_principal;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use contracts::authoring::AgentTrackKind;
use contracts::http::{
    IdempotencyKey, InternalAgentRunMutationRequest, InternalAgentRunOutcome,
    InternalCreateAgentRunRequest, InternalImageArtifactResolution,
};
use contracts::{
    AgentRunId, DiagnosticCode, ImageArtifactId, ProblemDetails, Sha256Digest, UtcTimestamp,
};
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto::Builder as HyperBuilder;
use hyper_util::service::TowerToHyperService;
use serde_json::Value;
use sqlx::Row;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::run_store::{
    AgentRunReservation, AgentRunStoreError, PostgresAgentRunStore, StoredCandidate,
};

/// Agent internal API state.
#[derive(Clone, Debug)]
pub struct AgentApiState {
    /// Agent-owned run repository.
    pub store: PostgresAgentRunStore,
}

/// Exact allowlisted Control URI SAN.
#[derive(Clone, Debug)]
pub struct ControlPrincipal {
    /// Verified URI SAN value.
    pub san_uri: String,
}

/// Builds all Control-to-Agent routes.
pub fn router(state: Arc<AgentApiState>) -> Router {
    Router::new()
        .route("/internal/v1/agent-runs", post(create_run))
        .route("/internal/v1/agent-runs/{run_id}", get(get_run))
        .route("/internal/v1/agent-runs/{run_id}/cancel", post(cancel_run))
        .route(
            "/internal/v1/agent-runs/{run_id}/tracks/{track}/retry",
            post(retry_track),
        )
        .route("/internal/v1/agent-runs/{run_id}/outcome", get(get_outcome))
        .route(
            "/internal/v1/image-artifacts/{artifact_id}",
            get(get_artifact),
        )
        .with_state(state)
}

/// Serves internal Agent routes only after CA verification and exact Control URI SAN extraction.
pub async fn serve_mtls(
    listener: tokio::net::TcpListener,
    router: Router,
    mtls: auth::MtlsServerConfig,
) -> Result<(), std::io::Error> {
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::clone(&mtls.server_config));
    loop {
        let (stream, _) = listener.accept().await?;
        let acceptor = acceptor.clone();
        let router = router.clone();
        let allowed = mtls.allowed_san_uris.clone();
        tokio::spawn(async move {
            let Ok(tls) = acceptor.accept(stream).await else {
                tracing::warn!(
                    event = "agent.mtls.handshake_denied",
                    diagnostic = "LW_AUTH_SERVICE_IDENTITY_DENIED"
                );
                return;
            };
            let Some(peer) = tls
                .get_ref()
                .1
                .peer_certificates()
                .and_then(|certificates| certificates.first())
            else {
                tracing::warn!(
                    event = "agent.mtls.peer_denied",
                    diagnostic = "LW_AUTH_SERVICE_IDENTITY_DENIED"
                );
                return;
            };
            let Ok(san_uri) = extract_mtls_principal(peer, &allowed) else {
                tracing::warn!(
                    event = "agent.mtls.peer_denied",
                    diagnostic = "LW_AUTH_SERVICE_IDENTITY_DENIED"
                );
                return;
            };
            let service = router.layer(Extension(ControlPrincipal { san_uri }));
            if HyperBuilder::new(TokioExecutor::new())
                .serve_connection_with_upgrades(
                    TokioIo::new(tls),
                    TowerToHyperService::new(service),
                )
                .await
                .is_err()
            {
                tracing::warn!(
                    event = "agent.mtls.connection_failed",
                    diagnostic = "LW_AGENT_CONNECTION_FAILED"
                );
            }
        });
    }
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
            run_id,
            request.expected_revision,
            &idempotency(&headers)?,
            now()?,
        )
        .await?;
    Ok(Json(run).into_response())
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
    let row = sqlx::query("SELECT contract,policy_evaluation FROM agent.image_artifacts WHERE image_artifact_id=$1 AND state='verified'")
        .bind(artifact_id.as_uuid()).fetch_optional(state.store.pool()).await.map_err(|_| AgentApiError::persistence())?.ok_or_else(AgentApiError::not_found)?;
    let artifact: contracts::supply_chain::ImageArtifact = serde_json::from_value(
        row.try_get::<Value, _>("contract")
            .map_err(|_| AgentApiError::contract())?,
    )
    .map_err(|_| AgentApiError::contract())?;
    let policy_evaluation: contracts::supply_chain::ImagePolicyEvaluation = serde_json::from_value(
        row.try_get::<Value, _>("policy_evaluation")
            .map_err(|_| AgentApiError::contract())?,
    )
    .map_err(|_| AgentApiError::contract())?;
    let resolution_sha256 = Sha256Digest::of_canonical(&serde_json::json!({"artifactId":artifact_id,"artifact":artifact,"policyEvaluation":policy_evaluation})).map_err(|_| AgentApiError::contract())?;
    let resolution = InternalImageArtifactResolution {
        artifact_id,
        artifact,
        policy_evaluation,
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
    headers
        .get("traceparent")
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty() && value.len() <= 256)
        .map_or_else(|| Uuid::now_v7().to_string(), str::to_owned)
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
impl IntoResponse for AgentApiError {
    fn into_response(self) -> Response {
        let request_id = Uuid::now_v7().to_string();
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
            trace_id: None,
            retryable: self.retryable,
            violations: Vec::new(),
        };
        (self.status, Json(problem)).into_response()
    }
}
