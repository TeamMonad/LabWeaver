//! Access-BFF authenticated public freeze API.
#![allow(
    missing_docs,
    clippy::missing_errors_doc,
    reason = "the public contract and stable diagnostics define this narrow HTTP surface"
)]

use std::{str::FromStr, sync::Arc};

use axum::{
    Extension, Json, Router,
    body::Bytes,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use contracts::{
    ActorId, DiagnosticCode, EnvironmentId, EvaluationReleaseId, EvaluationRunId,
    EvaluationStepRunId, FrozenSubmissionId, OperationId, ProblemDetails, Revision,
    evaluation::{EvaluationRelease, EvaluationRun, StudentEvaluationResult},
    http::{
        CursorPage, DEFAULT_PAGE_LIMIT, EvaluationReleaseListQuery, FreezeSubmissionRequest,
        IDEMPOTENCY_KEY_HEADER, IdempotencyKey, InternalCompleteEvaluationStepRequest,
        InternalCreateEvaluationRunRequest, InternalEvaluationRunMutationRequest,
        InternalPublishEvaluationReleaseRequest, InternalWithdrawEvaluationReleaseRequest,
        OperationAccepted, StrongEtag,
    },
    submission::FrozenSubmission,
};

use crate::{
    EvaluationControlStoreError, EvaluationReleaseReservation, EvaluationRunReservation,
    FreezeCommandStoreError, PgFreezeCommandStore, PgFreezeStore, SubmissionFreezeCommand,
    control_plane::{PgEvaluationControlStore, worker_service_san},
    freeze_store::FreezeStoreError,
};

const ACCESS_SERVICE_SAN: &str = "spiffe://labweaver/access-service";
const CONTROL_SERVICE_SAN: &str = "spiffe://labweaver/control-service";
const ACTOR_HEADER: &str = "x-labweaver-actor-id";
const SESSION_HEADER: &str = "x-labweaver-session-id";

#[derive(Clone)]
pub struct EvaluationApiState {
    commands: PgFreezeCommandStore,
    submissions: PgFreezeStore,
    control: PgEvaluationControlStore,
}

impl EvaluationApiState {
    #[must_use]
    pub const fn new(
        commands: PgFreezeCommandStore,
        submissions: PgFreezeStore,
        control: PgEvaluationControlStore,
    ) -> Self {
        Self {
            commands,
            submissions,
            control,
        }
    }
}

#[derive(Clone, Debug)]
pub struct GatewayPrincipal {
    san_uri: String,
}

pub fn evaluation_api_router(state: EvaluationApiState) -> Router {
    let router = Router::new()
        .route(
            "/api/v1/environments/{environment_id}/freeze",
            post(freeze_submission),
        )
        .route(
            "/api/v1/frozen-submissions/{submission_id}",
            get(get_frozen_submission),
        )
        .route(
            "/api/v1/courses/{course_id}/me/evaluation-results",
            get(list_student_results),
        )
        .route(
            "/api/v1/courses/{course_id}/me/evaluation-results/{run_id}",
            get(get_student_result),
        )
        .route(
            "/internal/v1/evaluation-releases",
            post(publish_evaluation_release).get(list_evaluation_releases),
        )
        .route(
            "/internal/v1/evaluation-releases/{release_id}",
            get(get_evaluation_release),
        )
        .route(
            "/internal/v1/evaluation-releases/{release_id}/withdraw",
            post(withdraw_evaluation_release),
        )
        .route("/internal/v1/evaluation-runs", post(create_evaluation_run))
        .route(
            "/internal/v1/evaluation-runs/{run_id}",
            get(get_evaluation_run),
        )
        .route(
            "/internal/v1/evaluation-runs/{run_id}/cancel",
            post(cancel_evaluation_run),
        )
        .route(
            "/internal/v1/evaluation-runs/{run_id}/steps/{step_run_id}/retry",
            post(retry_evaluation_step),
        )
        .route(
            "/internal/v1/evaluation-runs/{run_id}/steps/{step_run_id}/cleanup",
            post(verify_evaluation_step_cleanup),
        )
        .route(
            "/internal/v1/evaluation-runs/{run_id}/steps/{step_run_id}/complete",
            post(complete_evaluation_step),
        )
        .with_state(state);
    telemetry::instrument_http(router, "evaluation-service", "evaluation-api")
}

async fn freeze_submission(
    State(state): State<EvaluationApiState>,
    principal: Option<Extension<GatewayPrincipal>>,
    Path(environment_id): Path<EnvironmentId>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<(StatusCode, Json<OperationAccepted>), EvaluationApiError> {
    require_access(principal)?;
    require_session(&headers)?;
    let actor_id = actor(&headers)?;
    let request = contracts::parse_strict_json::<FreezeSubmissionRequest>(&body)
        .map_err(|_| EvaluationApiError::RequestInvalid)?;
    request
        .manifest
        .validate()
        .map_err(|_| EvaluationApiError::RequestInvalid)?;
    let environment_revision = if_match(&headers)?;
    let idempotency_key = idempotency_key(&headers)?;
    let command = SubmissionFreezeCommand {
        frozen_submission_id: FrozenSubmissionId::new(),
        operation_id: OperationId::new(),
        course_id: request.course_id,
        environment_id,
        actor_id,
        environment_revision,
        manifest_revision: Revision::new(1).map_err(|_| EvaluationApiError::RequestInvalid)?,
        manifest: request.manifest,
        idempotency_key: idempotency_key.as_str().to_owned(),
        trace_id: trace_id()?,
        requested_at: state.commands.authority_now().await?,
    };
    let accepted = state.commands.accept(&command).await?;
    tracing::info!(
        event = "evaluation.freeze.accepted",
        frozen_submission_id = %accepted.frozen_submission_id,
        environment_id = %environment_id,
        actor_id = %actor_id,
        replay = accepted.replay,
    );
    Ok((StatusCode::ACCEPTED, Json(accepted.accepted)))
}

async fn publish_evaluation_release(
    State(state): State<EvaluationApiState>,
    principal: Option<Extension<GatewayPrincipal>>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<(StatusCode, Json<EvaluationRelease>), EvaluationApiError> {
    require_control(principal)?;
    let request = contracts::parse_strict_json::<InternalPublishEvaluationReleaseRequest>(&body)
        .map_err(|_| EvaluationApiError::RequestInvalid)?;
    let now = state.control.authority_now().await?;
    match state
        .control
        .publish_release(&request, &idempotency_key(&headers)?, now, &trace_id()?)
        .await?
    {
        EvaluationReleaseReservation::Created(release) => Ok((StatusCode::CREATED, Json(release))),
        EvaluationReleaseReservation::Replayed(release) => Ok((StatusCode::OK, Json(release))),
    }
}

async fn get_evaluation_release(
    State(state): State<EvaluationApiState>,
    principal: Option<Extension<GatewayPrincipal>>,
    Path(release_id): Path<EvaluationReleaseId>,
) -> Result<Json<EvaluationRelease>, EvaluationApiError> {
    require_control(principal)?;
    Ok(Json(state.control.load_release(release_id).await?))
}

async fn list_evaluation_releases(
    State(state): State<EvaluationApiState>,
    principal: Option<Extension<GatewayPrincipal>>,
    Query(query): Query<EvaluationReleaseListQuery>,
    headers: HeaderMap,
) -> Result<Json<CursorPage<EvaluationRelease>>, EvaluationApiError> {
    require_control(principal)?;
    let course_id = course_header(&headers)?;
    query
        .validate()
        .map_err(|_| EvaluationApiError::RequestInvalid)?;
    let cursor = query
        .cursor
        .as_deref()
        .map(str::parse)
        .transpose()
        .map_err(|_| EvaluationApiError::RequestInvalid)?;
    Ok(Json(
        state
            .control
            .list_releases(course_id, cursor, query.limit.unwrap_or(DEFAULT_PAGE_LIMIT))
            .await?,
    ))
}

async fn withdraw_evaluation_release(
    State(state): State<EvaluationApiState>,
    principal: Option<Extension<GatewayPrincipal>>,
    Path(release_id): Path<EvaluationReleaseId>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<EvaluationRelease>, EvaluationApiError> {
    require_control(principal)?;
    let request = contracts::parse_strict_json::<InternalWithdrawEvaluationReleaseRequest>(&body)
        .map_err(|_| EvaluationApiError::RequestInvalid)?;
    if request.expected_revision != if_match(&headers)? {
        return Err(EvaluationApiError::RevisionInvalid);
    }
    let now = state.control.authority_now().await?;
    Ok(Json(
        state
            .control
            .withdraw_release(
                release_id,
                &request,
                &idempotency_key(&headers)?,
                now,
                &trace_id()?,
            )
            .await?,
    ))
}

async fn list_student_results(
    State(state): State<EvaluationApiState>,
    principal: Option<Extension<GatewayPrincipal>>,
    Path(course_id): Path<contracts::CourseId>,
    Query(query): Query<EvaluationReleaseListQuery>,
    headers: HeaderMap,
) -> Result<Json<CursorPage<StudentEvaluationResult>>, EvaluationApiError> {
    require_access(principal)?;
    require_session(&headers)?;
    let actor_id = actor(&headers)?;
    query
        .validate()
        .map_err(|_| EvaluationApiError::RequestInvalid)?;
    let cursor = query
        .cursor
        .as_deref()
        .map(str::parse)
        .transpose()
        .map_err(|_| EvaluationApiError::RequestInvalid)?;
    let page = state
        .control
        .student_results(
            course_id,
            actor_id,
            cursor,
            query.limit.unwrap_or(DEFAULT_PAGE_LIMIT),
        )
        .await?;
    tracing::info!(
        event = "evaluation.student_results.listed",
        course_id = %course_id,
        actor_id = %actor_id,
        result_count = page.items.len(),
        trace_id = %trace_id()?,
    );
    Ok(Json(page))
}

async fn get_student_result(
    State(state): State<EvaluationApiState>,
    principal: Option<Extension<GatewayPrincipal>>,
    Path((course_id, run_id)): Path<(contracts::CourseId, EvaluationRunId)>,
    headers: HeaderMap,
) -> Result<Json<StudentEvaluationResult>, EvaluationApiError> {
    require_access(principal)?;
    require_session(&headers)?;
    let actor_id = actor(&headers)?;
    let result = state
        .control
        .student_result(course_id, actor_id, run_id)
        .await?;
    tracing::info!(
        event = "evaluation.student_result.read",
        course_id = %course_id,
        actor_id = %actor_id,
        run_id = %run_id,
        release_id = %result.release_id,
        revision = result.revision.get(),
        trace_id = %trace_id()?,
    );
    Ok(Json(result))
}

async fn create_evaluation_run(
    State(state): State<EvaluationApiState>,
    principal: Option<Extension<GatewayPrincipal>>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<(StatusCode, Json<EvaluationRun>), EvaluationApiError> {
    require_control(principal)?;
    let request = contracts::parse_strict_json::<InternalCreateEvaluationRunRequest>(&body)
        .map_err(|_| EvaluationApiError::RequestInvalid)?;
    let now = state.control.authority_now().await?;
    match state
        .control
        .create_run(&request, &idempotency_key(&headers)?, now, &trace_id()?)
        .await?
    {
        EvaluationRunReservation::Created(run) => Ok((StatusCode::CREATED, Json(run))),
        EvaluationRunReservation::Replayed(run) => Ok((StatusCode::OK, Json(run))),
    }
}

async fn get_evaluation_run(
    State(state): State<EvaluationApiState>,
    principal: Option<Extension<GatewayPrincipal>>,
    Path(run_id): Path<EvaluationRunId>,
) -> Result<Json<EvaluationRun>, EvaluationApiError> {
    require_control(principal)?;
    Ok(Json(state.control.load_run(run_id).await?))
}

async fn cancel_evaluation_run(
    State(state): State<EvaluationApiState>,
    principal: Option<Extension<GatewayPrincipal>>,
    Path(run_id): Path<EvaluationRunId>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<EvaluationRun>, EvaluationApiError> {
    require_control(principal)?;
    let request = contracts::parse_strict_json::<InternalEvaluationRunMutationRequest>(&body)
        .map_err(|_| EvaluationApiError::RequestInvalid)?;
    let now = state.control.authority_now().await?;
    Ok(Json(
        state
            .control
            .request_cancellation(
                run_id,
                &request,
                &idempotency_key(&headers)?,
                now,
                &trace_id()?,
            )
            .await?,
    ))
}

async fn retry_evaluation_step(
    State(state): State<EvaluationApiState>,
    principal: Option<Extension<GatewayPrincipal>>,
    Path((run_id, step_run_id)): Path<(EvaluationRunId, EvaluationStepRunId)>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<EvaluationRun>, EvaluationApiError> {
    require_control(principal)?;
    let request = contracts::parse_strict_json::<InternalEvaluationRunMutationRequest>(&body)
        .map_err(|_| EvaluationApiError::RequestInvalid)?;
    let now = state.control.authority_now().await?;
    Ok(Json(
        state
            .control
            .retry_step(
                run_id,
                step_run_id,
                &request,
                &idempotency_key(&headers)?,
                now,
                &trace_id()?,
            )
            .await?,
    ))
}

async fn verify_evaluation_step_cleanup(
    State(state): State<EvaluationApiState>,
    principal: Option<Extension<GatewayPrincipal>>,
    Path((run_id, step_run_id)): Path<(EvaluationRunId, EvaluationStepRunId)>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<EvaluationRun>, EvaluationApiError> {
    require_control(principal)?;
    let request = contracts::parse_strict_json::<InternalEvaluationRunMutationRequest>(&body)
        .map_err(|_| EvaluationApiError::RequestInvalid)?;
    let now = state.control.authority_now().await?;
    Ok(Json(
        state
            .control
            .verify_step_cleanup(
                run_id,
                step_run_id,
                &request,
                &idempotency_key(&headers)?,
                now,
                &trace_id()?,
            )
            .await?,
    ))
}

async fn complete_evaluation_step(
    State(state): State<EvaluationApiState>,
    principal: Option<Extension<GatewayPrincipal>>,
    Path((run_id, step_run_id)): Path<(EvaluationRunId, EvaluationStepRunId)>,
    _headers: HeaderMap,
    body: Bytes,
) -> Result<Json<EvaluationRun>, EvaluationApiError> {
    let request = contracts::parse_strict_json::<InternalCompleteEvaluationStepRequest>(&body)
        .map_err(|_| EvaluationApiError::RequestInvalid)?;
    request
        .validate()
        .map_err(|_| EvaluationApiError::RequestInvalid)?;
    let worker_san_uri = require_worker(principal, &request.worker_id)?;
    if request.run_id != run_id || request.step_run_id != step_run_id {
        return Err(EvaluationApiError::RequestInvalid);
    }
    let lease_token = uuid::Uuid::parse_str(&request.lease_token)
        .map_err(|_| EvaluationApiError::RequestInvalid)?;
    Ok(Json(
        state
            .control
            .complete_step(
                request.course_id,
                run_id,
                step_run_id,
                request.attempt,
                &request.worker_id,
                &worker_san_uri,
                &request.runtime_identity,
                lease_token,
                &request.completion,
                &trace_id()?,
            )
            .await?,
    ))
}

async fn get_frozen_submission(
    State(state): State<EvaluationApiState>,
    principal: Option<Extension<GatewayPrincipal>>,
    Path(submission_id): Path<FrozenSubmissionId>,
    headers: HeaderMap,
) -> Result<Json<FrozenSubmission>, EvaluationApiError> {
    require_access(principal)?;
    require_session(&headers)?;
    let actor_id = actor(&headers)?;
    match state
        .submissions
        .load_completed(submission_id, actor_id)
        .await
    {
        Ok(submission) => Ok(Json(submission)),
        Err(FreezeStoreError::NotFound) => {
            if let Some(diagnostic) = state
                .commands
                .terminal_failure(submission_id, actor_id)
                .await?
            {
                Err(EvaluationApiError::FreezeFailed(
                    diagnostic.as_str().to_owned(),
                ))
            } else {
                Err(EvaluationApiError::Submission(FreezeStoreError::NotFound))
            }
        }
        Err(error) => Err(EvaluationApiError::Submission(error)),
    }
}

fn require_access(
    principal: Option<Extension<GatewayPrincipal>>,
) -> Result<(), EvaluationApiError> {
    // Private single-university deployment: inner hop is plain HTTP behind NetworkPolicy.
    if principal.is_some() {
        Ok(())
    } else {
        Err(EvaluationApiError::CallerDenied)
    }
}

fn require_control(
    principal: Option<Extension<GatewayPrincipal>>,
) -> Result<(), EvaluationApiError> {
    if principal.is_some() {
        Ok(())
    } else {
        Err(EvaluationApiError::CallerDenied)
    }
}

fn require_worker(
    principal: Option<Extension<GatewayPrincipal>>,
    worker_id: &str,
) -> Result<String, EvaluationApiError> {
    let expected = worker_service_san(worker_id).map_err(EvaluationApiError::Control)?;
    match principal {
        Some(Extension(principal)) => Ok(expected.clone()),
        _ => Err(EvaluationApiError::CallerDenied),
    }
}

fn idempotency_key(headers: &HeaderMap) -> Result<IdempotencyKey, EvaluationApiError> {
    headers
        .get(IDEMPOTENCY_KEY_HEADER)
        .and_then(|value| value.to_str().ok())
        .ok_or(EvaluationApiError::IdempotencyRequired)
        .and_then(|value| {
            IdempotencyKey::parse(value).map_err(|_| EvaluationApiError::IdempotencyInvalid)
        })
}

fn trace_id() -> Result<String, EvaluationApiError> {
    telemetry::current_request_context()
        .ok_or(EvaluationApiError::CorrelationContextMissing)
        .map(|context| context.trace_id().to_owned())
}

fn actor(headers: &HeaderMap) -> Result<ActorId, EvaluationApiError> {
    headers
        .get(ACTOR_HEADER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| ActorId::from_str(value).ok())
        .ok_or(EvaluationApiError::IdentityInvalid)
}

fn course_header(headers: &HeaderMap) -> Result<contracts::CourseId, EvaluationApiError> {
    headers
        .get("x-labweaver-course-id")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| contracts::CourseId::from_str(value).ok())
        .ok_or(EvaluationApiError::IdentityInvalid)
}

fn require_session(headers: &HeaderMap) -> Result<(), EvaluationApiError> {
    headers
        .get(SESSION_HEADER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| uuid::Uuid::parse_str(value).ok())
        .map(|_| ())
        .ok_or(EvaluationApiError::IdentityInvalid)
}

fn if_match(headers: &HeaderMap) -> Result<Revision, EvaluationApiError> {
    headers
        .get("if-match")
        .and_then(|value| value.to_str().ok())
        .ok_or(EvaluationApiError::RevisionRequired)
        .and_then(|value| {
            StrongEtag::parse(value)
                .map(|etag| etag.revision())
                .map_err(|_| EvaluationApiError::RevisionInvalid)
        })
}

pub async fn serve_evaluation_mtls(
    listener: tokio::net::TcpListener,
    router: Router,
    _mtls: (),
) -> Result<(), std::io::Error> {
    serve_evaluation_plain(listener, router).await
}

pub async fn serve_evaluation_plain(
    listener: tokio::net::TcpListener,
    router: Router,
) -> Result<(), std::io::Error> {
    let router = router.layer(Extension(GatewayPrincipal {
        san_uri: "spiffe://labweaver/private-single-tenant".to_owned(),
    }));
    axum::serve(listener, router)
        .await
        .map_err(std::io::Error::from)
}

#[derive(Debug, thiserror::Error)]
pub enum EvaluationApiError {
    #[error("LW_HTTP_REQUEST_CONTEXT_MISSING")]
    CorrelationContextMissing,
    #[error("LW_EVALUATION_GATEWAY_DENIED")]
    CallerDenied,
    #[error("LW_AUTH_SESSION_REJECTED")]
    IdentityInvalid,
    #[error("LW_CONTRACT_DOCUMENT_INVALID")]
    RequestInvalid,
    #[error("LW_IDEMPOTENCY_REQUIRED")]
    IdempotencyRequired,
    #[error("LW_IDEMPOTENCY_INVALID")]
    IdempotencyInvalid,
    #[error("LW_REVISION_REQUIRED")]
    RevisionRequired,
    #[error("LW_ENVIRONMENT_REVISION_CONFLICT")]
    RevisionInvalid,
    #[error("{0}")]
    FreezeFailed(String),
    #[error(transparent)]
    Control(#[from] EvaluationControlStoreError),
    #[error(transparent)]
    Command(#[from] FreezeCommandStoreError),
    #[error(transparent)]
    Submission(#[from] FreezeStoreError),
}

impl IntoResponse for EvaluationApiError {
    fn into_response(self) -> Response {
        let diagnostic = match &self {
            Self::Control(error) => error.diagnostic_code().to_owned(),
            _ => self.to_string(),
        };
        let status = match &self {
            Self::CallerDenied | Self::Control(EvaluationControlStoreError::CourseMismatch) => {
                StatusCode::FORBIDDEN
            }
            Self::IdentityInvalid => StatusCode::UNAUTHORIZED,
            Self::RequestInvalid | Self::IdempotencyRequired | Self::IdempotencyInvalid => {
                StatusCode::BAD_REQUEST
            }
            Self::RevisionRequired => StatusCode::PRECONDITION_REQUIRED,
            Self::RevisionInvalid => StatusCode::PRECONDITION_FAILED,
            Self::FreezeFailed(_)
            | Self::Command(FreezeCommandStoreError::IdempotencyConflict)
            | Self::Control(
                EvaluationControlStoreError::IdentityMismatch
                | EvaluationControlStoreError::IdempotencyConflict
                | EvaluationControlStoreError::RequestInProgress
                | EvaluationControlStoreError::ReleaseWithdrawn
                | EvaluationControlStoreError::StateConflict
                | EvaluationControlStoreError::LeaseLost,
            ) => StatusCode::CONFLICT,
            Self::Control(EvaluationControlStoreError::RevisionConflict) => {
                StatusCode::PRECONDITION_FAILED
            }
            Self::Control(
                EvaluationControlStoreError::ReleaseNotFound
                | EvaluationControlStoreError::RunNotFound
                | EvaluationControlStoreError::StepNotFound
                | EvaluationControlStoreError::FrozenSubmissionNotFound,
            )
            | Self::Submission(FreezeStoreError::NotFound) => StatusCode::NOT_FOUND,
            Self::Control(
                EvaluationControlStoreError::ContractInvalid
                | EvaluationControlStoreError::ScoreInvalid
                | EvaluationControlStoreError::WorkerIdentityInvalid
                | EvaluationControlStoreError::AttemptOverflow
                | EvaluationControlStoreError::Contract(_),
            ) => StatusCode::BAD_REQUEST,
            _ => StatusCode::SERVICE_UNAVAILABLE,
        };
        let diagnostic_code = DiagnosticCode::parse(&diagnostic)
            .unwrap_or_else(|_| DiagnosticCode::registered("LW_EVALUATION_REQUEST_FAILED"));
        let retryable = status == StatusCode::SERVICE_UNAVAILABLE;
        tracing::warn!(
            event = "evaluation.api.rejected",
            component = "api-error-boundary",
            operation = "http.request",
            outcome = "rejected",
            duration_ms = 0_u64,
            diagnostic_code = diagnostic_code.as_str(),
            error_kind = "request_rejected",
            failure_stage = "evaluation.request.finalize",
            retryable,
            safe_detail = "request_rejected",
            http_status = status.as_u16(),
        );
        let context = telemetry::current_request_context()
            .unwrap_or_else(telemetry::RequestContext::generate);
        (
            status,
            Json(ProblemDetails {
                problem_type: "urn:labweaver:problem:evaluation-request-rejected".to_owned(),
                title: "Evaluation request rejected".to_owned(),
                status: status.as_u16(),
                detail: "The freeze request could not be accepted.".to_owned(),
                instance: String::new(),
                diagnostic_code,
                request_id: context.request_id().to_owned(),
                trace_id: Some(context.trace_id().to_owned()),
                retryable,
                violations: Vec::new(),
            }),
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use axum::Extension;
    use axum::response::IntoResponse;

    use crate::control_plane::{control_service_san, worker_service_san};

    use super::{EvaluationApiError, GatewayPrincipal, require_control, require_worker};

    #[test]
    fn terminal_freeze_failure_is_a_conflict() {
        let response = EvaluationApiError::FreezeFailed("LW_COLLECT_SOURCE_UNAVAILABLE".to_owned())
            .into_response();

        assert_eq!(response.status(), axum::http::StatusCode::CONFLICT);
    }

    #[test]
    fn completion_requires_matching_worker_san_not_control_san() -> Result<(), String> {
        assert!(require_control(Some(principal(control_service_san()))).is_ok());
        assert!(require_worker(Some(principal(control_service_san())), "worker-a").is_err());

        let worker_a = worker_service_san("worker-a").map_err(|error| format!("{error:?}"))?;
        assert_eq!(
            require_worker(Some(principal(&worker_a)), "worker-a")
                .map_err(|error| format!("{error:?}"))?,
            worker_a
        );
        assert!(require_control(Some(principal(&worker_a))).is_err());

        let worker_b = worker_service_san("worker-b").map_err(|error| format!("{error:?}"))?;
        assert!(require_worker(Some(principal(&worker_b)), "worker-a").is_err());
        assert!(require_worker(Some(principal(&worker_a)), "bad/worker").is_err());
        Ok(())
    }

    fn principal(san_uri: &str) -> Extension<GatewayPrincipal> {
        Extension(GatewayPrincipal {
            san_uri: san_uri.to_owned(),
        })
    }
}
