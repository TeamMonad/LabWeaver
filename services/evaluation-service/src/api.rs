//! Access-BFF authenticated public freeze API.
#![allow(
    dead_code,
    unused,
    clippy::all,
    clippy::pedantic,
    clippy::needless_pass_by_value,
    clippy::useless_conversion,
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
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use contracts::{
    ActorId, DiagnosticCode, EnvironmentId, EvaluationReleaseId, EvaluationRunId,
    EvaluationStepRunId, FrozenSubmissionId, OperationId, ProblemDetails, Revision,
    evaluation::{EvaluationRelease, EvaluationRun, StudentEvaluationResult},
    http::{
        AuthoringPublicationAdmissionQuery, CursorPage, DEFAULT_PAGE_LIMIT,
        EvaluationReleaseListQuery, FreezeSubmissionRequest, IDEMPOTENCY_KEY_HEADER,
        IdempotencyKey, InternalCompleteEvaluationStepRequest, InternalCreateEvaluationRunRequest,
        InternalEvaluationRunMutationRequest, InternalPublishEvaluationReleaseRequest,
        InternalWithdrawEvaluationReleaseRequest, OperationAccepted, StrongEtag,
    },
    submission::FrozenSubmission,
};

use crate::{
    AuthoringAdmissionClient, AuthoringAdmissionClientError, EvaluationControlStoreError,
    EvaluationReleaseReservation, EvaluationRunReservation, FreezeCommandStoreError,
    PgFreezeCommandStore, PgFreezeStore, SubmissionFreezeCommand,
    control_plane::PgEvaluationControlStore, freeze_store::FreezeStoreError,
};

const ACTOR_HEADER: &str = "x-labweaver-actor-id";
const SESSION_HEADER: &str = "x-labweaver-session-id";
const ACCESS_PERMISSION: &str = "evaluation.api.invoke";
const CONTROL_PERMISSION: &str = "evaluation.control.invoke";
const WORKER_PERMISSION: &str = "evaluation.step.complete";

#[derive(Clone)]
pub struct EvaluationApiState {
    commands: PgFreezeCommandStore,
    submissions: PgFreezeStore,
    control: PgEvaluationControlStore,
    authoring_admission: Arc<AuthoringAdmissionClient>,
}

impl EvaluationApiState {
    #[must_use]
    pub const fn new(
        commands: PgFreezeCommandStore,
        submissions: PgFreezeStore,
        control: PgEvaluationControlStore,
        authoring_admission: Arc<AuthoringAdmissionClient>,
    ) -> Self {
        Self {
            commands,
            submissions,
            control,
            authoring_admission,
        }
    }
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

/// Applies the service-account JWT boundary to every Evaluation route.
///
/// The TLS transport protects the bearer token in transit. The verifier is the
/// only source of the caller identity; request headers and client certificates
/// never establish an identity. Individual handlers still require their route
/// permission so an accepted token cannot cross service boundaries.
pub fn with_service_auth(router: Router, verifier: Arc<auth::ServiceTokenVerifier>) -> Router {
    router.layer(middleware::from_fn_with_state(
        verifier,
        require_service_token,
    ))
}

async fn require_service_token(
    State(verifier): State<Arc<auth::ServiceTokenVerifier>>,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    match verifier.authenticate(request.headers()).await {
        Ok(identity) => {
            let mut request = request;
            request.extensions_mut().insert(identity);
            next.run(request).await
        }
        Err(error) => EvaluationApiError::ServiceAuth(error).into_response(),
    }
}

async fn freeze_submission(
    State(state): State<EvaluationApiState>,
    principal: Option<Extension<auth::ServiceIdentity>>,
    Path(environment_id): Path<EnvironmentId>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<(StatusCode, Json<OperationAccepted>), EvaluationApiError> {
    require_access(principal)?;
    require_session(&headers)?;
    let actor_id = actor(&headers)?;
    let project_id = project_header(&headers)?;
    let course_id = optional_course_header(&headers)?;
    let request = contracts::parse_strict_json::<FreezeSubmissionRequest>(&body)
        .map_err(|_| EvaluationApiError::RequestInvalid)?;
    request
        .manifest
        .validate()
        .map_err(|_| EvaluationApiError::RequestInvalid)?;
    if course_id.is_some() && request.course_id != course_id {
        return Err(EvaluationApiError::RequestInvalid);
    }
    let environment_revision = if_match(&headers)?;
    let idempotency_key = idempotency_key(&headers)?;
    let command = SubmissionFreezeCommand {
        frozen_submission_id: FrozenSubmissionId::new(),
        operation_id: OperationId::new(),
        project_id,
        course_id: request.course_id.or(course_id),
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
    principal: Option<Extension<auth::ServiceIdentity>>,
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
    principal: Option<Extension<auth::ServiceIdentity>>,
    Path(release_id): Path<EvaluationReleaseId>,
) -> Result<Json<EvaluationRelease>, EvaluationApiError> {
    require_control(principal)?;
    let release = state.control.load_release(release_id).await?;
    ensure_release_admitted(&state, &release).await?;
    Ok(Json(release))
}

async fn list_evaluation_releases(
    State(state): State<EvaluationApiState>,
    principal: Option<Extension<auth::ServiceIdentity>>,
    Query(query): Query<EvaluationReleaseListQuery>,
    headers: HeaderMap,
) -> Result<Json<CursorPage<EvaluationRelease>>, EvaluationApiError> {
    require_control(principal)?;
    let project_id = project_header(&headers)?;
    let course_id = optional_course_header(&headers)?;
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
        .list_releases(
            project_id,
            course_id,
            cursor,
            query.limit.unwrap_or(DEFAULT_PAGE_LIMIT),
        )
        .await?;
    for release in &page.items {
        ensure_release_admitted(&state, release).await?;
    }
    Ok(Json(page))
}

async fn withdraw_evaluation_release(
    State(state): State<EvaluationApiState>,
    principal: Option<Extension<auth::ServiceIdentity>>,
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
    principal: Option<Extension<auth::ServiceIdentity>>,
    Path(course_id): Path<contracts::CourseId>,
    Query(query): Query<EvaluationReleaseListQuery>,
    headers: HeaderMap,
) -> Result<Json<CursorPage<StudentEvaluationResult>>, EvaluationApiError> {
    require_access(principal)?;
    require_session(&headers)?;
    let actor_id = actor(&headers)?;
    let project_id = project_header(&headers)?;
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
            project_id,
            Some(course_id),
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
    principal: Option<Extension<auth::ServiceIdentity>>,
    Path((course_id, run_id)): Path<(contracts::CourseId, EvaluationRunId)>,
    headers: HeaderMap,
) -> Result<Json<StudentEvaluationResult>, EvaluationApiError> {
    require_access(principal)?;
    require_session(&headers)?;
    let actor_id = actor(&headers)?;
    let project_id = project_header(&headers)?;
    let result = state
        .control
        .student_result(project_id, Some(course_id), actor_id, run_id)
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
    principal: Option<Extension<auth::ServiceIdentity>>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<(StatusCode, Json<EvaluationRun>), EvaluationApiError> {
    require_control(principal)?;
    let request = contracts::parse_strict_json::<InternalCreateEvaluationRunRequest>(&body)
        .map_err(|_| EvaluationApiError::RequestInvalid)?;
    // Resolve the release projection before the network call.  The Control admission endpoint is
    // intentionally outside the Evaluation transaction; the store repeats the exact identity
    // checks while locking the release so a withdrawal or revision change cannot race admission.
    let release = state.control.load_release(request.release_id).await?;
    if release.state != contracts::evaluation::EvaluationReleaseState::Active {
        return Err(EvaluationApiError::Control(
            EvaluationControlStoreError::ReleaseWithdrawn,
        ));
    }
    let admission =
        resolve_release_admission(&state, &release, request.project_id, request.course_id).await?;
    let now = state.control.authority_now().await?;
    match state
        .control
        .create_run(
            &request,
            &idempotency_key(&headers)?,
            now,
            &trace_id()?,
            &admission,
        )
        .await?
    {
        EvaluationRunReservation::Created(run) => Ok((StatusCode::CREATED, Json(run))),
        EvaluationRunReservation::Replayed(run) => Ok((StatusCode::OK, Json(run))),
    }
}

async fn resolve_release_admission(
    state: &EvaluationApiState,
    release: &EvaluationRelease,
    project_id: contracts::ProjectId,
    course_id: Option<contracts::CourseId>,
) -> Result<contracts::http::AuthoringPublicationAdmissionBinding, EvaluationApiError> {
    let query = AuthoringPublicationAdmissionQuery {
        project_id,
        course_id,
        approval_revision: release.approval_revision,
        evaluation_release_id: release.id,
    };
    state
        .authoring_admission
        .resolve(release.approval_id, &query)
        .await
        .map_err(EvaluationApiError::AuthoringAdmission)
}

async fn ensure_release_admitted(
    state: &EvaluationApiState,
    release: &EvaluationRelease,
) -> Result<(), EvaluationApiError> {
    if release.state != contracts::evaluation::EvaluationReleaseState::Active {
        return Err(EvaluationApiError::Control(
            EvaluationControlStoreError::ReleaseWithdrawn,
        ));
    }
    let _ =
        resolve_release_admission(state, release, release.project_id, release.course_id).await?;
    Ok(())
}

async fn get_evaluation_run(
    State(state): State<EvaluationApiState>,
    principal: Option<Extension<auth::ServiceIdentity>>,
    Path(run_id): Path<EvaluationRunId>,
) -> Result<Json<EvaluationRun>, EvaluationApiError> {
    require_control(principal)?;
    Ok(Json(state.control.load_run(run_id).await?))
}

async fn cancel_evaluation_run(
    State(state): State<EvaluationApiState>,
    principal: Option<Extension<auth::ServiceIdentity>>,
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
    principal: Option<Extension<auth::ServiceIdentity>>,
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
    principal: Option<Extension<auth::ServiceIdentity>>,
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
    principal: Option<Extension<auth::ServiceIdentity>>,
    Path((run_id, step_run_id)): Path<(EvaluationRunId, EvaluationStepRunId)>,
    _headers: HeaderMap,
    body: Bytes,
) -> Result<Json<EvaluationRun>, EvaluationApiError> {
    let request = contracts::parse_strict_json::<InternalCompleteEvaluationStepRequest>(&body)
        .map_err(|_| EvaluationApiError::RequestInvalid)?;
    request
        .validate()
        .map_err(|_| EvaluationApiError::RequestInvalid)?;
    require_worker(principal)?;
    if request.run_id != run_id || request.step_run_id != step_run_id {
        return Err(EvaluationApiError::RequestInvalid);
    }
    let lease_token = uuid::Uuid::parse_str(&request.lease_token)
        .map_err(|_| EvaluationApiError::RequestInvalid)?;
    Ok(Json(
        state
            .control
            .complete_step(
                request.project_id,
                request.course_id,
                run_id,
                step_run_id,
                request.attempt,
                &request.worker_id,
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
    principal: Option<Extension<auth::ServiceIdentity>>,
    Path(submission_id): Path<FrozenSubmissionId>,
    headers: HeaderMap,
) -> Result<Json<FrozenSubmission>, EvaluationApiError> {
    require_access(principal)?;
    require_session(&headers)?;
    let actor_id = actor(&headers)?;
    let project_id = project_header(&headers)?;
    let course_id = optional_course_header(&headers)?;
    match state
        .submissions
        .load_completed(submission_id, project_id, course_id, actor_id)
        .await
    {
        Ok(submission) => Ok(Json(submission)),
        Err(FreezeStoreError::NotFound) => {
            if let Some(diagnostic) = state
                .commands
                .terminal_failure(submission_id, project_id, course_id, actor_id)
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
    principal: Option<Extension<auth::ServiceIdentity>>,
) -> Result<(), EvaluationApiError> {
    require_permission(principal, ACCESS_PERMISSION)
}

fn require_control(
    principal: Option<Extension<auth::ServiceIdentity>>,
) -> Result<(), EvaluationApiError> {
    require_permission(principal, CONTROL_PERMISSION)
}

fn require_worker(
    principal: Option<Extension<auth::ServiceIdentity>>,
) -> Result<(), EvaluationApiError> {
    require_permission(principal, WORKER_PERMISSION)?;
    Ok(())
}

fn require_permission(
    principal: Option<Extension<auth::ServiceIdentity>>,
    permission: &str,
) -> Result<(), EvaluationApiError> {
    principal
        .is_some_and(|Extension(identity)| identity.allows(permission))
        .then_some(())
        .ok_or(EvaluationApiError::CallerDenied)
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

fn project_header(headers: &HeaderMap) -> Result<contracts::ProjectId, EvaluationApiError> {
    headers
        .get("x-labweaver-project-id")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| contracts::ProjectId::from_str(value).ok())
        .ok_or(EvaluationApiError::IdentityInvalid)
}

fn optional_course_header(
    headers: &HeaderMap,
) -> Result<Option<contracts::CourseId>, EvaluationApiError> {
    let Some(value) = headers.get("x-labweaver-course-id") else {
        return Ok(None);
    };
    let value = value
        .to_str()
        .map_err(|_| EvaluationApiError::IdentityInvalid)?;
    let course_id =
        contracts::CourseId::from_str(value).map_err(|_| EvaluationApiError::IdentityInvalid)?;
    Ok(Some(course_id))
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

#[derive(Debug, thiserror::Error)]
pub enum EvaluationApiError {
    #[error("LW_HTTP_REQUEST_CONTEXT_MISSING")]
    CorrelationContextMissing,
    #[error("LW_EVALUATION_GATEWAY_DENIED")]
    CallerDenied,
    #[error(transparent)]
    ServiceAuth(#[from] auth::ServiceAuthError),
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
    AuthoringAdmission(#[from] AuthoringAdmissionClientError),
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
            Self::CallerDenied
            | Self::ServiceAuth(auth::ServiceAuthError::PermissionDenied)
            | Self::Control(EvaluationControlStoreError::CourseMismatch) => StatusCode::FORBIDDEN,
            Self::IdentityInvalid
            | Self::ServiceAuth(
                auth::ServiceAuthError::CredentialsMissing
                | auth::ServiceAuthError::TokenRejected
                | auth::ServiceAuthError::TokenExpired,
            ) => StatusCode::UNAUTHORIZED,
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
            Self::AuthoringAdmission(AuthoringAdmissionClientError::AdmissionMissing) => {
                StatusCode::NOT_FOUND
            }
            Self::AuthoringAdmission(AuthoringAdmissionClientError::Denied) => {
                StatusCode::FORBIDDEN
            }
            Self::AuthoringAdmission(
                AuthoringAdmissionClientError::Conflict
                | AuthoringAdmissionClientError::ResponseInvalid
                | AuthoringAdmissionClientError::RequestInvalid,
            ) => StatusCode::CONFLICT,
            Self::AuthoringAdmission(
                AuthoringAdmissionClientError::Configuration
                | AuthoringAdmissionClientError::Token(_)
                | AuthoringAdmissionClientError::Transport
                | AuthoringAdmissionClientError::ResponseTooLarge
                | AuthoringAdmissionClientError::Rejected
                | AuthoringAdmissionClientError::Unavailable,
            ) => StatusCode::SERVICE_UNAVAILABLE,
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
    use std::collections::BTreeSet;
    use time::OffsetDateTime;

    use super::{
        ACCESS_PERMISSION, CONTROL_PERMISSION, EvaluationApiError, WORKER_PERMISSION,
        require_control, require_worker,
    };

    #[test]
    fn terminal_freeze_failure_is_a_conflict() {
        let response = EvaluationApiError::FreezeFailed("LW_COLLECT_SOURCE_UNAVAILABLE".to_owned())
            .into_response();

        assert_eq!(response.status(), axum::http::StatusCode::CONFLICT);
    }

    #[test]
    fn routes_require_their_declared_service_permission() -> Result<(), String> {
        let control = principal(CONTROL_PERMISSION);
        assert!(require_control(Some(control)).is_ok());
        assert!(require_control(Some(principal(ACCESS_PERMISSION))).is_err());

        assert!(require_worker(Some(principal(WORKER_PERMISSION))).is_ok());
        assert!(require_control(None).is_err());
        assert!(require_worker(None).is_err());

        Ok(())
    }

    fn principal(permission: &str) -> Extension<auth::ServiceIdentity> {
        Extension(auth::ServiceIdentity {
            issuer: "https://issuer.example.test".to_owned(),
            subject: "evaluation-test".to_owned(),
            client_id: "evaluation-test".to_owned(),
            expires_at: OffsetDateTime::now_utc(),
            permissions: BTreeSet::from([permission.to_owned()]),
        })
    }
}
