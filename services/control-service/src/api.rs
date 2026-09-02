//! Trusted-Gateway HTTP API for the Control authority.
#![allow(
    clippy::missing_errors_doc,
    clippy::too_many_lines,
    reason = "HTTP boundary functions keep validation adjacent to each route"
)]

use std::str::FromStr;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use contracts::authoring::{AgentRun, AgentTrackKind, CourseLlmEgressPolicy};
use contracts::http::{
    CandidateDecisionRequest, CompleteProblemPackageUploadRequest, CreateAgentRunRequest,
    CreateEnvironmentTemplateReleaseRequest, CreateEvaluationReleaseRequest,
    CreateProblemPackageUploadRequest, CreateWorkAgentRunRequest, CursorPage,
    EvaluationReleaseListQuery, IdempotencyKey, InternalAgentRunMutationRequest,
    InternalCreateAgentRunRequest, InternalWithdrawEvaluationReleaseRequest, OperationAccepted,
    StrongEtag, WithdrawEnvironmentTemplateReleaseRequest, WithdrawEvaluationReleaseRequest,
    resolve_sse_resume,
};
use contracts::{
    ActorId, AgentRunId, AuthorizationDecisionRequest, AuthorizationScope, BffSessionId,
    CandidateId, CourseId, DiagnosticCode, EvaluationReleaseId, EventId, OperationId,
    ProblemDetails, ProblemPackageId, ReleaseId, Revision, StreamSequence, UploadSessionId,
    UtcTimestamp,
};
use futures_util::stream;
use serde::Deserialize;
use time::OffsetDateTime;

use crate::clients::{AccessClient, AgentClient, DownstreamError, EvaluationClient};
use crate::{ControlError, ControlService};

const ACTOR_HEADER: &str = "x-labweaver-actor-id";
const SESSION_HEADER: &str = "x-labweaver-session-id";
/// Runtime state shared only by authenticated mTLS connections.
#[derive(Clone, Debug)]
pub struct ApiState {
    /// Control-owned transactional domain service.
    pub control: ControlService,
    /// Fail-closed Access authorization authority.
    pub access: AccessClient,
    /// Agent-owned run and artifact authority.
    pub agent: AgentClient,
    /// Evaluation-owned release authority.
    pub evaluation: EvaluationClient,
}

/// Caller principal for private single-tenant deployment (plain HTTP, no cert verification).
#[derive(Clone, Debug)]
pub struct GatewayPrincipal {
    /// Fixed URI SAN for private deployment.
    pub san_uri: String,
}

/// Builds the complete Issue #48 public control-plane route table.
pub fn router(state: Arc<ApiState>) -> Router {
    let router = Router::new()
        .route(
            "/api/v1/courses/{course_id}/problem-package-uploads",
            post(create_upload),
        )
        .route(
            "/api/v1/courses/{course_id}/problem-package-uploads/{upload_id}/complete",
            post(complete_upload),
        )
        .route(
            "/api/v1/courses/{course_id}/problem-packages/{package_id}",
            get(get_package),
        )
        .route(
            "/api/v1/courses/{course_id}/llm-egress-policies",
            post(create_policy),
        )
        .route(
            "/api/v1/courses/{course_id}/llm-egress-policies/active",
            get(get_policy),
        )
        .route(
            "/api/v1/courses/{course_id}/agent-runs",
            post(create_agent_run),
        )
        .route(
            "/api/v1/courses/{course_id}/work-agent-runs",
            post(create_work_agent_run),
        )
        .route(
            "/api/v1/courses/{course_id}/agent-runs/{run_id}",
            get(get_agent_run),
        )
        .route(
            "/api/v1/courses/{course_id}/agent-runs/{run_id}/cancel",
            post(cancel_agent_run),
        )
        .route(
            "/api/v1/courses/{course_id}/agent-runs/{run_id}/tracks/{track}/retry",
            post(retry_agent_run),
        )
        .route(
            "/api/v1/courses/{course_id}/environment-candidates/{candidate_id}",
            get(get_environment_candidate),
        )
        .route(
            "/api/v1/courses/{course_id}/environment-candidates/{candidate_id}/decisions",
            post(decide_environment_candidate),
        )
        .route(
            "/api/v1/courses/{course_id}/evaluation-candidates/{candidate_id}",
            get(get_evaluation_candidate),
        )
        .route(
            "/api/v1/courses/{course_id}/evaluation-candidates/{candidate_id}/decisions",
            post(decide_evaluation_candidate),
        )
        .route(
            "/api/v1/courses/{course_id}/evaluation-releases",
            post(create_evaluation_release).get(list_evaluation_releases),
        )
        .route(
            "/api/v1/courses/{course_id}/evaluation-releases/{release_id}",
            get(get_evaluation_release),
        )
        .route(
            "/api/v1/courses/{course_id}/evaluation-releases/{release_id}/withdraw",
            post(withdraw_evaluation_release),
        )
        .route(
            "/api/v1/courses/{course_id}/environment-template-releases",
            post(create_release).get(list_releases),
        )
        .route(
            "/api/v1/courses/{course_id}/environment-template-releases/{release_id}",
            get(get_release),
        )
        .route(
            "/api/v1/courses/{course_id}/environment-template-releases/{release_id}/withdraw",
            post(withdraw_release),
        )
        .route("/api/v1/courses/{course_id}/events", get(events))
        .with_state(state);
    telemetry::instrument_http(router, "control-service", "control-api")
}

/// Serves the Control router over plain HTTP for private single-university delivery.
///
/// Inner mTLS is disabled (ARC-09); `NetworkPolicy` / private network is the trust boundary.
pub async fn serve_mtls(
    listener: tokio::net::TcpListener,
    router: Router,
    _mtls: (),
) -> Result<(), std::io::Error> {
    serve_plain(listener, router).await
}

/// Plain-HTTP serving with a fixed private-tenant principal.
pub async fn serve_plain(
    listener: tokio::net::TcpListener,
    router: Router,
) -> Result<(), std::io::Error> {
    let router = router.layer(Extension(GatewayPrincipal {
        san_uri: "spiffe://labweaver/private-single-tenant".to_owned(),
    }));
    axum::serve(listener, router).await
}

async fn create_upload(
    State(state): State<Arc<ApiState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path(course_id): Path<CourseId>,
    headers: HeaderMap,
    Json(request): Json<CreateProblemPackageUploadRequest>,
) -> Result<Response, ApiError> {
    authorize(
        &state,
        &principal,
        &headers,
        "createProblemPackageUpload",
        course_id,
    )
    .await?;
    let key = idempotency(&headers)?;
    let session = state
        .control
        .create_upload(course_id, &request, &key, now()?)
        .await?;
    Ok(with_etag(StatusCode::CREATED, &session, session.revision))
}

async fn complete_upload(
    State(state): State<Arc<ApiState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path((course_id, upload_id)): Path<(CourseId, UploadSessionId)>,
    headers: HeaderMap,
    Json(_request): Json<CompleteProblemPackageUploadRequest>,
) -> Result<Response, ApiError> {
    authorize(
        &state,
        &principal,
        &headers,
        "completeProblemPackageUpload",
        course_id,
    )
    .await?;
    let expected = etag(&headers)?;
    let key = idempotency(&headers)?;
    let package = state
        .control
        .complete_upload(course_id, upload_id, expected, &key, now()?)
        .await?;
    Ok(with_etag(StatusCode::CREATED, &package, package.revision))
}

async fn get_package(
    State(state): State<Arc<ApiState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path((course_id, package_id)): Path<(CourseId, ProblemPackageId)>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    authorize(&state, &principal, &headers, "getProblemPackage", course_id).await?;
    let package = state.control.package(course_id, package_id).await?;
    Ok(with_etag(StatusCode::OK, &package, package.revision))
}

async fn create_policy(
    State(state): State<Arc<ApiState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path(course_id): Path<CourseId>,
    headers: HeaderMap,
    Json(policy): Json<CourseLlmEgressPolicy>,
) -> Result<Response, ApiError> {
    authorize(
        &state,
        &principal,
        &headers,
        "createCourseLlmPolicy",
        course_id,
    )
    .await?;
    let policy = state
        .control
        .activate_policy(course_id, policy, &idempotency(&headers)?)
        .await?;
    Ok(with_etag(StatusCode::CREATED, &policy, policy.revision))
}

async fn get_policy(
    State(state): State<Arc<ApiState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path(course_id): Path<CourseId>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    authorize(
        &state,
        &principal,
        &headers,
        "getActiveCourseLlmPolicy",
        course_id,
    )
    .await?;
    let policy = state.control.active_policy(course_id).await?;
    Ok(with_etag(StatusCode::OK, &policy, policy.revision))
}

async fn create_agent_run(
    State(state): State<Arc<ApiState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path(course_id): Path<CourseId>,
    headers: HeaderMap,
    Json(request): Json<CreateAgentRunRequest>,
) -> Result<Response, ApiError> {
    create_agent_run_for_class(
        state,
        principal,
        course_id,
        headers,
        request,
        contracts::authoring::EnvironmentClass::Experiment,
        "createAgentRun",
    )
    .await
}

async fn create_work_agent_run(
    State(state): State<Arc<ApiState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path(course_id): Path<CourseId>,
    headers: HeaderMap,
    Json(request): Json<CreateWorkAgentRunRequest>,
) -> Result<Response, ApiError> {
    create_agent_run_for_class(
        state,
        principal,
        course_id,
        headers,
        request.into(),
        contracts::authoring::EnvironmentClass::Work,
        "createWorkAgentRun",
    )
    .await
}

async fn create_agent_run_for_class(
    state: Arc<ApiState>,
    principal: GatewayPrincipal,
    course_id: CourseId,
    headers: HeaderMap,
    request: CreateAgentRunRequest,
    expected_environment_class: contracts::authoring::EnvironmentClass,
    authorization_operation: &'static str,
) -> Result<Response, ApiError> {
    authorize(
        &state,
        &principal,
        &headers,
        authorization_operation,
        course_id,
    )
    .await?;
    let key = idempotency(&headers)?;
    let package = state.control.package(course_id, request.package_id).await?;
    let object_locators = state
        .control
        .package_object_locators(course_id, &package)
        .await?;
    let policy = state.control.active_policy(course_id).await?;
    if package.revision != request.package_revision
        || policy.id != request.policy_id
        || policy.revision != request.policy_revision
    {
        return Err(ApiError::from(ControlError::RevisionConflict));
    }
    let run = state
        .agent
        .create(
            &InternalCreateAgentRunRequest {
                course_id,
                request,
                expected_environment_class,
                package,
                object_locators,
                policy,
            },
            &key,
            &headers,
        )
        .await?;
    state
        .control
        .project_agent_run(EventId::new(), &run)
        .await?;
    Ok(accepted(&run))
}

async fn get_agent_run(
    State(state): State<Arc<ApiState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path((course_id, run_id)): Path<(CourseId, AgentRunId)>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    authorize(&state, &principal, &headers, "getAgentRun", course_id).await?;
    let run = state.control.agent_run(course_id, run_id).await?;
    Ok(with_etag(StatusCode::OK, &run, run.revision))
}

async fn cancel_agent_run(
    State(state): State<Arc<ApiState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path((course_id, run_id)): Path<(CourseId, AgentRunId)>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    authorize(&state, &principal, &headers, "cancelAgentRun", course_id).await?;
    let key = idempotency(&headers)?;
    let expected_revision = etag(&headers)?;
    let projected = state.control.agent_run(course_id, run_id).await?;
    if projected.revision != expected_revision {
        return Err(ControlError::RevisionConflict.into());
    }
    let run = state
        .agent
        .cancel(
            run_id,
            &InternalAgentRunMutationRequest {
                course_id,
                expected_revision,
            },
            &key,
            &headers,
        )
        .await?;
    if run.course_id != course_id {
        return Err(DownstreamError::IdentityMismatch.into());
    }
    Ok(accepted(&run))
}

async fn retry_agent_run(
    State(state): State<Arc<ApiState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path((course_id, run_id, track)): Path<(CourseId, AgentRunId, String)>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    authorize(
        &state,
        &principal,
        &headers,
        "retryAgentRunTrack",
        course_id,
    )
    .await?;
    let track = parse_track(&track)?;
    let key = idempotency(&headers)?;
    let expected_revision = etag(&headers)?;
    let projected = state.control.agent_run(course_id, run_id).await?;
    if projected.revision != expected_revision {
        return Err(ControlError::RevisionConflict.into());
    }
    let run = state
        .agent
        .retry(
            run_id,
            track,
            &InternalAgentRunMutationRequest {
                course_id,
                expected_revision,
            },
            &key,
            &headers,
        )
        .await?;
    if run.course_id != course_id {
        return Err(DownstreamError::IdentityMismatch.into());
    }
    Ok(accepted(&run))
}

async fn get_environment_candidate(
    State(state): State<Arc<ApiState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path((course_id, candidate_id)): Path<(CourseId, CandidateId)>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    authorize(
        &state,
        &principal,
        &headers,
        "getEnvironmentCandidate",
        course_id,
    )
    .await?;
    let value = state
        .control
        .environment_candidate_view(course_id, candidate_id)
        .await?;
    Ok(with_etag(StatusCode::OK, &value, value.candidate.revision))
}

async fn get_evaluation_candidate(
    State(state): State<Arc<ApiState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path((course_id, candidate_id)): Path<(CourseId, CandidateId)>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    authorize(
        &state,
        &principal,
        &headers,
        "getEvaluationCandidate",
        course_id,
    )
    .await?;
    let value = state
        .control
        .evaluation_candidate_view(course_id, candidate_id)
        .await?;
    Ok(with_etag(StatusCode::OK, &value, value.candidate.revision))
}

async fn decide_environment_candidate(
    State(state): State<Arc<ApiState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path((course_id, candidate_id)): Path<(CourseId, CandidateId)>,
    headers: HeaderMap,
    Json(request): Json<CandidateDecisionRequest>,
) -> Result<Response, ApiError> {
    let actor = authorize(
        &state,
        &principal,
        &headers,
        "appendEnvironmentCandidateDecision",
        course_id,
    )
    .await?;
    let value = state
        .control
        .decide_candidate(
            course_id,
            candidate_id,
            AgentTrackKind::Environment,
            &request,
            actor,
            etag(&headers)?,
            &idempotency(&headers)?,
            now()?,
        )
        .await?;
    Ok(with_etag(
        StatusCode::CREATED,
        &value,
        value.candidate_revision,
    ))
}

async fn decide_evaluation_candidate(
    State(state): State<Arc<ApiState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path((course_id, candidate_id)): Path<(CourseId, CandidateId)>,
    headers: HeaderMap,
    Json(request): Json<CandidateDecisionRequest>,
) -> Result<Response, ApiError> {
    let actor = authorize(
        &state,
        &principal,
        &headers,
        "appendEvaluationCandidateDecision",
        course_id,
    )
    .await?;
    let value = state
        .control
        .decide_candidate(
            course_id,
            candidate_id,
            AgentTrackKind::Evaluation,
            &request,
            actor,
            etag(&headers)?,
            &idempotency(&headers)?,
            now()?,
        )
        .await?;
    Ok(with_etag(
        StatusCode::CREATED,
        &value,
        value.candidate_revision,
    ))
}

async fn create_evaluation_release(
    State(state): State<Arc<ApiState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path(course_id): Path<CourseId>,
    headers: HeaderMap,
    Json(request): Json<CreateEvaluationReleaseRequest>,
) -> Result<Response, ApiError> {
    let actor = authorize(
        &state,
        &principal,
        &headers,
        "createEvaluationRelease",
        course_id,
    )
    .await?;
    let key = idempotency(&headers)?;
    let trace = trace_id(&headers);
    let command = state
        .control
        .prepare_evaluation_release(course_id, &request, actor)
        .await?;
    let release = state.evaluation.publish(&command, &key, &headers).await?;
    if release.course_id != course_id
        || release.candidate_id != request.candidate_id
        || release.approval_id != request.approval_id
    {
        return Err(ApiError::internal(
            "LW_CONTROL_DOWNSTREAM_IDENTITY_MISMATCH",
        ));
    }
    tracing::info!(
        event = "control.evaluation_release.published",
        course_id = %course_id,
        actor_id = %actor,
        candidate_id = %request.candidate_id,
        approval_id = %request.approval_id,
        release_id = %release.id,
        revision = release.revision.get(),
        trace_id = %trace,
    );
    Ok(with_etag(StatusCode::CREATED, &release, release.revision))
}

async fn list_evaluation_releases(
    State(state): State<Arc<ApiState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path(course_id): Path<CourseId>,
    headers: HeaderMap,
    Query(query): Query<EvaluationReleaseListQuery>,
) -> Result<Response, ApiError> {
    authorize(
        &state,
        &principal,
        &headers,
        "listEvaluationReleases",
        course_id,
    )
    .await?;
    query
        .validate()
        .map_err(|_| ApiError::bad_request("LW_CONTRACT_DOCUMENT_INVALID"))?;
    Ok(Json(state.evaluation.list(course_id, &query, &headers).await?).into_response())
}

async fn get_evaluation_release(
    State(state): State<Arc<ApiState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path((course_id, release_id)): Path<(CourseId, EvaluationReleaseId)>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    authorize(
        &state,
        &principal,
        &headers,
        "getEvaluationRelease",
        course_id,
    )
    .await?;
    let release = state.evaluation.get(release_id, &headers).await?;
    if release.course_id != course_id {
        return Err(ApiError::forbidden("LW_AUTH_COURSE_SCOPE_DENIED"));
    }
    Ok(with_etag(StatusCode::OK, &release, release.revision))
}

async fn withdraw_evaluation_release(
    State(state): State<Arc<ApiState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path((course_id, release_id)): Path<(CourseId, EvaluationReleaseId)>,
    headers: HeaderMap,
    Json(request): Json<WithdrawEvaluationReleaseRequest>,
) -> Result<Response, ApiError> {
    let actor = authorize(
        &state,
        &principal,
        &headers,
        "withdrawEvaluationRelease",
        course_id,
    )
    .await?;
    let expected = etag(&headers)?;
    if expected != request.expected_revision {
        return Err(ApiError::precondition("LW_REVISION_CONFLICT"));
    }
    let trace = trace_id(&headers);
    let release = state
        .evaluation
        .withdraw(
            release_id,
            &InternalWithdrawEvaluationReleaseRequest {
                course_id,
                expected_revision: expected,
                withdrawn_by: actor,
                reason_code: request.reason_code.clone(),
            },
            &idempotency(&headers)?,
            &headers,
        )
        .await?;
    tracing::info!(
        event = "control.evaluation_release.withdrawn",
        course_id = %course_id,
        actor_id = %actor,
        release_id = %release.id,
        revision = release.revision.get(),
        diagnostic = request.reason_code.as_str(),
        trace_id = %trace,
    );
    Ok(with_etag(StatusCode::OK, &release, release.revision))
}

async fn create_release(
    State(state): State<Arc<ApiState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path(course_id): Path<CourseId>,
    headers: HeaderMap,
    Json(request): Json<CreateEnvironmentTemplateReleaseRequest>,
) -> Result<Response, ApiError> {
    let actor = authorize(
        &state,
        &principal,
        &headers,
        "createEnvironmentTemplateRelease",
        course_id,
    )
    .await?;
    let key = idempotency(&headers)?;
    let published_at = now()?;
    let trace_id = trace_id(&headers);
    let release = state
        .control
        .create_release(course_id, &request, actor, &key, published_at, &trace_id)
        .await?;
    Ok(Json(OperationAccepted {
        operation_id: OperationId::new(),
        revision: Revision::new(release.version)
            .map_err(|_| ApiError::internal("LW_CONTRACT_DOCUMENT_INVALID"))?,
        status_url: format!(
            "/api/v1/courses/{course_id}/environment-template-releases/{}",
            release.id
        ),
    })
    .into_response())
}

#[derive(Deserialize)]
struct ReleaseQuery {
    #[serde(default)]
    after_version: u64,
    #[serde(default = "default_limit")]
    limit: u32,
}
fn default_limit() -> u32 {
    100
}

async fn list_releases(
    State(state): State<Arc<ApiState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path(course_id): Path<CourseId>,
    headers: HeaderMap,
    Query(query): Query<ReleaseQuery>,
) -> Result<Response, ApiError> {
    authorize(
        &state,
        &principal,
        &headers,
        "listEnvironmentTemplateReleases",
        course_id,
    )
    .await?;
    let items = state
        .control
        .releases(course_id, query.after_version, query.limit)
        .await?;
    let next_cursor = items.last().map(|view| view.release.version.to_string());
    Ok(Json(CursorPage { items, next_cursor }).into_response())
}

async fn get_release(
    State(state): State<Arc<ApiState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path((course_id, release_id)): Path<(CourseId, ReleaseId)>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    authorize(
        &state,
        &principal,
        &headers,
        "getEnvironmentTemplateRelease",
        course_id,
    )
    .await?;
    let release = state.control.release(course_id, release_id).await?;
    Ok(with_etag(
        StatusCode::OK,
        &release,
        Revision::new(release.release.version)
            .map_err(|_| ApiError::internal("LW_CONTRACT_DOCUMENT_INVALID"))?,
    ))
}

async fn withdraw_release(
    State(state): State<Arc<ApiState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path((course_id, release_id)): Path<(CourseId, ReleaseId)>,
    headers: HeaderMap,
    Json(request): Json<WithdrawEnvironmentTemplateReleaseRequest>,
) -> Result<Response, ApiError> {
    let actor = authorize(
        &state,
        &principal,
        &headers,
        "withdrawEnvironmentTemplateRelease",
        course_id,
    )
    .await?;
    let expected = etag(&headers)?;
    let withdrawal = state
        .control
        .withdraw_release(
            course_id,
            release_id,
            expected.get(),
            actor,
            &request.reason_code,
            &idempotency(&headers)?,
            now()?,
            &trace_id(&headers),
        )
        .await?;
    Ok((StatusCode::CREATED, Json(withdrawal)).into_response())
}

#[derive(Deserialize)]
struct EventQuery {
    after: Option<u64>,
    #[serde(default = "default_limit")]
    limit: u32,
}

async fn events(
    State(state): State<Arc<ApiState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path(course_id): Path<CourseId>,
    headers: HeaderMap,
    Query(query): Query<EventQuery>,
) -> Result<Response, ApiError> {
    let authorization = authorize_decision(
        &state,
        &principal,
        &headers,
        "streamCourseEvents",
        course_id,
    )
    .await?;
    let last_event_id = headers
        .get("Last-Event-ID")
        .and_then(|value| value.to_str().ok())
        .map(str::parse::<u64>)
        .transpose()
        .map_err(|_| ApiError::bad_request("LW_SSE_CURSOR_GAP"))?
        .map(StreamSequence);
    let resume = resolve_sse_resume(last_event_id, query.after.map(StreamSequence))
        .map_err(|_| ApiError::bad_request("LW_SSE_CURSOR_GAP"))?;
    let after = match resume {
        contracts::http::SseResume::Beginning => None,
        contracts::http::SseResume::After(sequence) => Some(sequence.0),
    };
    let records = state
        .control
        .sse_page(course_id, after, query.limit, now()?)
        .await?;
    let cursor = records
        .last()
        .map_or(after.unwrap_or(0), |record| record.sequence);
    let output = stream::unfold(
        EventStreamState {
            control: state.control.clone(),
            course_id,
            cursor,
            pending: records.into(),
            valid_until: authorization.valid_until,
        },
        |mut state| async move {
            loop {
                let Ok(timestamp) = current_timestamp() else {
                    tracing::error!(
                        event = "control.sse.clock_failed",
                        diagnostic = "LW_CONTROL_CLOCK_INVALID",
                        course_id = %state.course_id,
                        cursor = state.cursor,
                    );
                    return None;
                };
                if timestamp >= state.valid_until {
                    return None;
                }
                if let Some(record) = state.pending.pop_front() {
                    state.cursor = record.sequence;
                    let event = Event::default()
                        .id(record.sequence.to_string())
                        .event(record.event_type)
                        .data(record.payload.to_string());
                    return Some((Ok::<_, std::convert::Infallible>(event), state));
                }
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                match state
                    .control
                    .sse_page(state.course_id, Some(state.cursor), 256, timestamp)
                    .await
                {
                    Ok(records) => state.pending = records.into(),
                    Err(error) => {
                        tracing::error!(
                            event = "control.sse.poll_failed",
                            diagnostic = %error,
                            course_id = %state.course_id,
                            cursor = state.cursor,
                        );
                        return None;
                    }
                }
            }
        },
    );
    Ok(Sse::new(output)
        .keep_alive(KeepAlive::default())
        .into_response())
}

struct EventStreamState {
    control: ControlService,
    course_id: CourseId,
    cursor: u64,
    pending: std::collections::VecDeque<crate::SseRecord>,
    valid_until: UtcTimestamp,
}

async fn authorize(
    state: &ApiState,
    principal: &GatewayPrincipal,
    headers: &HeaderMap,
    operation: &str,
    course_id: CourseId,
) -> Result<ActorId, ApiError> {
    Ok(
        authorize_decision(state, principal, headers, operation, course_id)
            .await?
            .actor
            .actor_id,
    )
}

async fn authorize_decision(
    state: &ApiState,
    principal: &GatewayPrincipal,
    headers: &HeaderMap,
    operation: &str,
    course_id: CourseId,
) -> Result<contracts::AuthorizationDecision, ApiError> {
    if principal.san_uri.trim().is_empty() {
        return Err(ApiError::forbidden("LW_AUTH_SERVICE_IDENTITY_DENIED"));
    }
    let actor_id = parse_header::<ActorId>(headers, ACTOR_HEADER)?;
    let session_id = parse_header::<BffSessionId>(headers, SESSION_HEADER)?;
    let decision = state
        .access
        .authorize(
            &AuthorizationDecisionRequest {
                operation_id: operation.to_owned(),
                actor_id,
                session_id,
                scope: AuthorizationScope::Course { course_id },
                authorization_revision: None,
                scope_revision: None,
            },
            headers,
        )
        .await?;
    let current = now()?;
    if decision.actor.actor_id != actor_id
        || decision.scope != (AuthorizationScope::Course { course_id })
        || decision.valid_until <= current
        || decision.diagnostic_code.is_some()
    {
        return Err(ApiError::forbidden("LW_AUTH_SCOPE_DENIED"));
    }
    Ok(decision)
}

fn idempotency(headers: &HeaderMap) -> Result<IdempotencyKey, ApiError> {
    let value = headers
        .get("Idempotency-Key")
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| ApiError::bad_request("LW_IDEMPOTENCY_KEY_REQUIRED"))?;
    IdempotencyKey::parse(value).map_err(|_| ApiError::bad_request("LW_IDEMPOTENCY_KEY_INVALID"))
}

fn etag(headers: &HeaderMap) -> Result<Revision, ApiError> {
    let value = headers
        .get("If-Match")
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| ApiError::precondition("LW_IF_MATCH_REQUIRED"))?;
    StrongEtag::parse(value)
        .map(|value| value.revision())
        .map_err(|_| ApiError::precondition("LW_REVISION_CONFLICT"))
}

fn parse_header<T: FromStr>(headers: &HeaderMap, name: &str) -> Result<T, ApiError> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| ApiError::unauthorized("LW_AUTH_REQUIRED"))
}

fn parse_track(value: &str) -> Result<AgentTrackKind, ApiError> {
    match value {
        "environment" => Ok(AgentTrackKind::Environment),
        "evaluation" => Ok(AgentTrackKind::Evaluation),
        _ => Err(ApiError::bad_request("LW_CONTRACT_DOCUMENT_INVALID")),
    }
}

fn now() -> Result<UtcTimestamp, ApiError> {
    current_timestamp().map_err(|()| ApiError::internal("LW_AUTH_TIMESTAMP_INVALID"))
}

fn trace_id(headers: &HeaderMap) -> String {
    let _ = headers;
    telemetry::current_request_context()
        .unwrap_or_else(telemetry::RequestContext::generate)
        .trace_id()
        .to_owned()
}

fn current_timestamp() -> Result<UtcTimestamp, ()> {
    let value = OffsetDateTime::now_utc();
    let value = value
        .replace_nanosecond((value.nanosecond() / 1_000_000) * 1_000_000)
        .map_err(|_| ())?;
    UtcTimestamp::from_utc(value).map_err(|_| ())
}

fn accepted(run: &AgentRun) -> Response {
    (StatusCode::ACCEPTED, Json(run)).into_response()
}

fn with_etag<T: serde::Serialize>(status: StatusCode, value: &T, revision: Revision) -> Response {
    let mut response = (status, Json(value)).into_response();
    let Ok(header) = HeaderValue::from_str(&StrongEtag::from_revision(revision).header_value())
    else {
        return ApiError::internal("LW_CONTRACT_DOCUMENT_INVALID").into_response();
    };
    response.headers_mut().insert(header::ETAG, header);
    response
}

/// RFC 9457 response with stable payload-free diagnostics.
#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    diagnostic: String,
    retryable: bool,
}

impl ApiError {
    fn bad_request(code: &str) -> Self {
        Self::new(StatusCode::BAD_REQUEST, code, false)
    }
    fn unauthorized(code: &str) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, code, false)
    }
    fn forbidden(code: &str) -> Self {
        Self::new(StatusCode::FORBIDDEN, code, false)
    }
    fn precondition(code: &str) -> Self {
        Self::new(StatusCode::PRECONDITION_FAILED, code, false)
    }
    fn internal(code: &str) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, code, false)
    }
    fn new(status: StatusCode, code: &str, retryable: bool) -> Self {
        Self {
            status,
            diagnostic: code.to_owned(),
            retryable,
        }
    }
}

impl From<ControlError> for ApiError {
    fn from(error: ControlError) -> Self {
        let status = match error {
            ControlError::UploadNotFound
            | ControlError::CandidateNotFound
            | ControlError::ReleaseNotFound
            | ControlError::NotFound
            | ControlError::PolicyNotFound => StatusCode::NOT_FOUND,
            ControlError::IdempotencyConflict
            | ControlError::OperationInProgress
            | ControlError::OperationLeaseLost
            | ControlError::UploadStateConflict
            | ControlError::RevisionConflict
            | ControlError::DecisionConflict
            | ControlError::CandidateKindMismatch
            | ControlError::ProjectionConflict
            | ControlError::ReleaseCandidateMismatch
            | ControlError::ArtifactMismatch => StatusCode::CONFLICT,
            ControlError::SseCursorExpired => StatusCode::GONE,
            ControlError::CourseMismatch => StatusCode::FORBIDDEN,
            ControlError::ConfigurationInvalid
            | ControlError::PersistenceFailed
            | ControlError::ObjectStore(_)
            | ControlError::ArtifactNotAuthoritative => StatusCode::SERVICE_UNAVAILABLE,
            _ => StatusCode::UNPROCESSABLE_ENTITY,
        };
        let retryable = matches!(
            error,
            ControlError::PersistenceFailed
                | ControlError::ObjectStore(_)
                | ControlError::OperationInProgress
                | ControlError::OperationLeaseLost
                | ControlError::ArtifactNotAuthoritative
        );
        let diagnostic = error.to_string();
        let diagnostic = diagnostic.split(':').next().unwrap_or(&diagnostic);
        Self::new(status, diagnostic, retryable)
    }
}

impl From<DownstreamError> for ApiError {
    fn from(error: DownstreamError) -> Self {
        let (status, retryable) = match error {
            DownstreamError::Denied => (StatusCode::FORBIDDEN, false),
            DownstreamError::NotFound => (StatusCode::NOT_FOUND, false),
            DownstreamError::Conflict => (StatusCode::CONFLICT, false),
            DownstreamError::Configuration
            | DownstreamError::IdentityMismatch
            | DownstreamError::ProtocolInvalid => (StatusCode::BAD_GATEWAY, false),
            DownstreamError::Unavailable => (StatusCode::SERVICE_UNAVAILABLE, true),
        };
        Self::new(status, &error.to_string(), retryable)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let context = telemetry::current_request_context()
            .unwrap_or_else(telemetry::RequestContext::generate);
        let request_id = context.request_id().to_owned();
        let diagnostic = DiagnosticCode::parse(self.diagnostic)
            .unwrap_or_else(|_| DiagnosticCode::registered("LW_CONTROL_UNCLASSIFIED_ERROR"));
        let problem = ProblemDetails {
            problem_type: format!(
                "urn:labweaver:problem:{}",
                diagnostic.as_str().to_ascii_lowercase()
            ),
            title: "Control request blocked".to_owned(),
            status: self.status.as_u16(),
            detail: "The request was rejected by a fail-closed control-plane boundary.".to_owned(),
            instance: format!("urn:labweaver:request:{request_id}"),
            diagnostic_code: diagnostic,
            request_id,
            trace_id: Some(context.trace_id().to_owned()),
            retryable: self.retryable,
            violations: Vec::new(),
        };
        if self.retryable || self.status.is_client_error() {
            tracing::warn!(
                event = "control.request.rejected",
                component = "api-error-boundary",
                operation = "http.request",
                outcome = "rejected",
                duration_ms = 0_u64,
                diagnostic_code = problem.diagnostic_code.as_str(),
                error_kind = "request_rejected",
                failure_stage = "control.request.finalize",
                retryable = self.retryable,
                safe_detail = "request_rejected",
            );
        } else {
            tracing::error!(
                event = "control.request.failed",
                component = "api-error-boundary",
                operation = "http.request",
                outcome = "failed",
                duration_ms = 0_u64,
                diagnostic_code = problem.diagnostic_code.as_str(),
                error_kind = "terminal_api_failure",
                failure_stage = "control.request.finalize",
                retryable = false,
                safe_detail = "redacted_unclassified",
            );
        }
        (
            self.status,
            [(header::CONTENT_TYPE, "application/problem+json")],
            Json(problem),
        )
            .into_response()
    }
}
