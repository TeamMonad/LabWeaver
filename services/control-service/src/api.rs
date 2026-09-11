//! Trusted-Gateway HTTP API for the Control authority.
#![allow(
    clippy::missing_errors_doc,
    clippy::too_many_lines,
    reason = "HTTP boundary functions keep validation adjacent to each route"
)]

use std::str::FromStr;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::Request;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Extension, Json, Router};
use contracts::authoring::{AgentRun, AgentRunPurpose, AgentTrackKind, ProjectLlmEgressPolicy};
use contracts::http::{
    AddProjectMembershipRequest, AgentWorkExecutionIntentQuery, ApproveWorkConfigurationRequest,
    AuthoringPublicationAdmissionQuery, CandidateDecisionRequest, CompleteAuthoringApprovalRequest,
    CompleteProblemPackageUploadRequest, CreateAgentRunRequest,
    CreateEnvironmentTemplateReleaseRequest, CreateEvaluationReleaseRequest,
    CreateProblemPackageUploadRequest, CreateWorkConfigurationRunRequest, CursorPage,
    EvaluationReleaseListQuery, GeneratedArtifactKind, GeneratedArtifactQuery, IdempotencyKey,
    InternalAgentRunMutationRequest, InternalApproveWorkConfigurationRequest,
    InternalCreateAgentRunRequest, InternalWithdrawEvaluationReleaseRequest, OperationAccepted,
    RemoveProjectMembershipRequest, StrongEtag, WithdrawEnvironmentTemplateReleaseRequest,
    WithdrawEvaluationReleaseRequest, WorkConfigurationAdmissionQuery, WorkConfigurationPlanView,
    resolve_sse_resume,
};
use contracts::{
    ActorId, AgentRunId, AuthorizationDecisionRequest, AuthorizationScope, BffSessionId,
    CandidateId, CourseId, DiagnosticCode, EvaluationReleaseId, EventId, OperationId, PlatformRole,
    ProblemDetails, ProblemPackageId, ProjectId, ReleaseId, Revision, StreamSequence,
    UploadSessionId, UtcTimestamp,
};
use futures_util::stream;
use persistence_sqlx::Sha256Digest;
use serde::Deserialize;
use time::OffsetDateTime;

use crate::clients::{
    AccessClient, AgentClient, DownstreamError, EnvironmentClient, EvaluationClient,
};
use crate::{ControlError, ControlService};
use auth::ServiceTokenVerifier;

const ACTOR_HEADER: &str = "x-labweaver-actor-id";
const SESSION_HEADER: &str = "x-labweaver-session-id";
/// Runtime state shared only by authenticated mTLS connections.
#[derive(Clone)]
pub struct ApiState {
    /// Control-owned transactional domain service.
    pub control: ControlService,
    /// Fail-closed Access authorization authority.
    pub access: AccessClient,
    /// Agent-owned run and artifact authority.
    pub agent: AgentClient,
    /// Evaluation-owned release authority.
    pub evaluation: EvaluationClient,
    /// Environment-owned Work runtime and lease authority.
    pub environment: EnvironmentClient,
    /// Verifies the Access service account before any public route is reached.
    pub service_token_verifier: Arc<ServiceTokenVerifier>,
}

/// Caller principal derived from the verified service JWT.
#[derive(Clone, Debug)]
pub struct GatewayPrincipal {
    /// OAuth client ID bound to the verified service account.
    pub client_id: String,
}

/// Builds the complete Issue #48 public control-plane route table.
pub fn router(state: Arc<ApiState>) -> Router {
    let router = Router::new()
        .route("/api/v1/projects", get(list_projects).post(create_project))
        .route(
            "/api/v1/projects/{project_id}",
            get(get_project).patch(update_project),
        )
        .route(
            "/api/v1/projects/{project_id}/archive",
            post(archive_project),
        )
        .route(
            "/api/v1/projects/{project_id}/members",
            get(list_project_memberships).post(add_project_membership),
        )
        .route(
            "/api/v1/projects/{project_id}/members/{actor_id}",
            delete(remove_project_membership),
        )
        .route(
            "/api/v1/projects/{project_id}/problem-package-uploads",
            post(create_project_upload),
        )
        .route(
            "/api/v1/projects/{project_id}/problem-package-uploads/{upload_id}/complete",
            post(complete_project_upload),
        )
        .route(
            "/api/v1/projects/{project_id}/problem-packages/{package_id}",
            get(get_project_package),
        )
        .route(
            "/api/v1/projects/{project_id}/llm-egress-policies",
            post(create_project_policy),
        )
        .route(
            "/api/v1/projects/{project_id}/llm-egress-policies/active",
            get(get_project_policy),
        )
        .route(
            "/api/v1/projects/{project_id}/agent-runs",
            post(create_project_agent_run),
        )
        .route(
            "/api/v1/projects/{project_id}/work-configuration-runs",
            post(create_project_work_configuration_run),
        )
        .route(
            "/api/v1/projects/{project_id}/agent-runs/{run_id}/work-configuration/approve",
            post(approve_project_work_configuration),
        )
        .route(
            "/api/v1/projects/{project_id}/agent-runs/{run_id}/work-configuration/plan",
            get(get_project_work_configuration_plan),
        )
        .route(
            "/api/v1/projects/{project_id}/agent-runs/{run_id}",
            get(get_project_agent_run),
        )
        .route(
            "/api/v1/projects/{project_id}/agent-runs/{run_id}/cancel",
            post(cancel_project_agent_run),
        )
        .route(
            "/api/v1/projects/{project_id}/agent-runs/{run_id}/tracks/{track}/retry",
            post(retry_project_agent_run),
        )
        .route(
            "/api/v1/projects/{project_id}/environment-candidates/{candidate_id}",
            get(get_project_environment_candidate),
        )
        .route(
            "/api/v1/projects/{project_id}/environment-candidates/{candidate_id}/decisions",
            post(decide_project_environment_candidate),
        )
        .route(
            "/api/v1/projects/{project_id}/evaluation-candidates/{candidate_id}",
            get(get_project_evaluation_candidate),
        )
        .route(
            "/api/v1/projects/{project_id}/evaluation-candidates/{candidate_id}/decisions",
            post(decide_project_evaluation_candidate),
        )
        .route(
            "/api/v1/projects/{project_id}/authoring-approvals",
            post(complete_project_authoring_approval),
        )
        .route(
            "/api/v1/projects/{project_id}/authoring-approvals/{approval_id}",
            get(get_project_authoring_approval),
        )
        .route(
            "/api/v1/projects/{project_id}/environment-template-releases",
            get(list_project_releases).post(create_project_work_release),
        )
        .route(
            "/api/v1/projects/{project_id}/environment-template-releases/{release_id}",
            get(get_project_release),
        )
        .route(
            "/api/v1/projects/{project_id}/environment-template-releases/{release_id}/withdraw",
            post(withdraw_project_release),
        )
        .route("/api/v1/projects/{project_id}/events", get(project_events))
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
        .route("/api/v1/courses/{course_id}/events", get(events))
        .with_state(state);
    telemetry::instrument_http(router, "control-service", "control-api")
}

/// Builds the production router with the service-account boundary enabled.
///
/// User authorization remains performed by Access on each request. The
/// service token only proves that the caller is the configured trusted
/// gateway; it never supplies an actor or project scope.
pub fn authenticated_router(state: &Arc<ApiState>, verifier: &Arc<ServiceTokenVerifier>) -> Router {
    let public = router(Arc::clone(state)).layer(middleware::from_fn_with_state(
        Arc::clone(verifier),
        require_service_token,
    ));
    let internal = telemetry::instrument_http(
        Router::new()
            .route(
                "/internal/v1/authoring-publications/{approval_id}/admission",
                get(get_internal_authoring_publication_admission),
            )
            .with_state(Arc::clone(state))
            .layer(middleware::from_fn_with_state(
                Arc::clone(verifier),
                require_authoring_service_token,
            )),
        "control-service",
        "control-api",
    );
    let work_admission = telemetry::instrument_http(
        Router::new()
            .route(
                "/internal/v1/agent-runs/{run_id}/work-configuration-admission",
                get(get_internal_work_configuration_admission),
            )
            .with_state(Arc::clone(state))
            .layer(middleware::from_fn_with_state(
                Arc::clone(verifier),
                require_work_admission_service_token,
            )),
        "control-service",
        "control-api",
    );
    let evaluation_policy = telemetry::instrument_http(
        Router::new()
            .route(
                "/internal/v1/projects/{project_id}/llm-egress-policy",
                get(get_internal_project_llm_egress_policy),
            )
            .with_state(Arc::clone(state))
            .layer(middleware::from_fn_with_state(
                Arc::clone(verifier),
                require_evaluation_service_token,
            )),
        "control-service",
        "control-api",
    );
    Router::new()
        .merge(public)
        .merge(internal)
        .merge(work_admission)
        .merge(evaluation_policy)
}

async fn require_service_token(
    State(verifier): State<Arc<ServiceTokenVerifier>>,
    request: Request<Body>,
    next: Next,
) -> Response {
    match verifier
        .authenticate_with_permission(request.headers(), "access.control.forward")
        .await
    {
        Ok(identity) => {
            let mut request = request;
            request.extensions_mut().insert(GatewayPrincipal {
                client_id: identity.client_id,
            });
            next.run(request).await
        }
        Err(error) => ApiError::from(error).into_response(),
    }
}

async fn require_authoring_service_token(
    State(verifier): State<Arc<ServiceTokenVerifier>>,
    request: Request<Body>,
    next: Next,
) -> Response {
    match verifier
        .authenticate_with_permission(request.headers(), "control.authoring.read")
        .await
    {
        Ok(_) => next.run(request).await,
        Err(error) => ApiError::from(error).into_response(),
    }
}

async fn require_work_admission_service_token(
    State(verifier): State<Arc<ServiceTokenVerifier>>,
    request: Request<Body>,
    next: Next,
) -> Response {
    match verifier
        .authenticate_with_permission(request.headers(), "control.agent_run.read")
        .await
    {
        Ok(_) => next.run(request).await,
        Err(error) => ApiError::from(error).into_response(),
    }
}

async fn require_evaluation_service_token(
    State(verifier): State<Arc<ServiceTokenVerifier>>,
    request: Request<Body>,
    next: Next,
) -> Response {
    match verifier
        .authenticate_with_permission(request.headers(), "control.llm_policy.read")
        .await
    {
        Ok(_) => next.run(request).await,
        Err(error) => ApiError::from(error).into_response(),
    }
}

async fn list_projects(
    State(state): State<Arc<ApiState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let decision = authorize_global(&state, &principal, &headers, "listProjects").await?;
    let projects = state
        .control
        .list_projects(
            decision.actor.actor_id,
            decision.actor.roles.contains(&PlatformRole::PlatformAdmin),
        )
        .await?;
    Ok(Json(projects).into_response())
}

async fn create_project(
    State(state): State<Arc<ApiState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    headers: HeaderMap,
    Json(request): Json<contracts::CreateProjectRequest>,
) -> Result<Response, ApiError> {
    let decision = authorize_global(&state, &principal, &headers, "createProject").await?;
    let owner_role = project_owner_role(&decision.actor.roles)?;
    let project = state
        .control
        .create_project(
            decision.actor.actor_id,
            owner_role,
            &request,
            &idempotency(&headers)?,
            now()?,
        )
        .await?;
    Ok(with_etag(StatusCode::CREATED, &project, project.revision))
}

async fn get_project(
    State(state): State<Arc<ApiState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path(project_id): Path<ProjectId>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    authorize_project(&state, &principal, &headers, "getProject", project_id).await?;
    let project = state.control.project(project_id).await?;
    Ok(with_etag(StatusCode::OK, &project, project.revision))
}

async fn update_project(
    State(state): State<Arc<ApiState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path(project_id): Path<ProjectId>,
    headers: HeaderMap,
    Json(request): Json<contracts::UpdateProjectRequest>,
) -> Result<Response, ApiError> {
    let decision =
        authorize_project(&state, &principal, &headers, "updateProject", project_id).await?;
    let project = state
        .control
        .update_project(
            project_id,
            decision.actor.actor_id,
            decision.actor.roles.contains(&PlatformRole::PlatformAdmin),
            &request,
            &idempotency(&headers)?,
            now()?,
        )
        .await?;
    Ok(with_etag(StatusCode::OK, &project, project.revision))
}

async fn archive_project(
    State(state): State<Arc<ApiState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path(project_id): Path<ProjectId>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let decision =
        authorize_project(&state, &principal, &headers, "archiveProject", project_id).await?;
    let project = state
        .control
        .archive_project(
            project_id,
            decision.actor.actor_id,
            decision.actor.roles.contains(&PlatformRole::PlatformAdmin),
            etag(&headers)?,
            &idempotency(&headers)?,
            now()?,
        )
        .await?;
    Ok(with_etag(StatusCode::OK, &project, project.revision))
}

async fn list_project_memberships(
    State(state): State<Arc<ApiState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path(project_id): Path<ProjectId>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    authorize_project(
        &state,
        &principal,
        &headers,
        "listProjectMemberships",
        project_id,
    )
    .await?;
    Ok(Json(state.control.project_memberships(project_id).await?).into_response())
}

async fn add_project_membership(
    State(state): State<Arc<ApiState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path(project_id): Path<ProjectId>,
    headers: HeaderMap,
    Json(request): Json<AddProjectMembershipRequest>,
) -> Result<Response, ApiError> {
    let decision = authorize_project(
        &state,
        &principal,
        &headers,
        "addProjectMembership",
        project_id,
    )
    .await?;
    let membership = state
        .control
        .add_project_membership(
            project_id,
            decision.actor.actor_id,
            decision.actor.roles.contains(&PlatformRole::PlatformAdmin),
            &request,
            &idempotency(&headers)?,
            now()?,
        )
        .await?;
    Ok(with_etag(
        StatusCode::CREATED,
        &membership,
        membership.revision,
    ))
}

async fn remove_project_membership(
    State(state): State<Arc<ApiState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path((project_id, target_actor_id)): Path<(ProjectId, ActorId)>,
    headers: HeaderMap,
    Json(request): Json<RemoveProjectMembershipRequest>,
) -> Result<Response, ApiError> {
    let decision = authorize_project(
        &state,
        &principal,
        &headers,
        "removeProjectMembership",
        project_id,
    )
    .await?;
    let membership = state
        .control
        .remove_project_membership(
            project_id,
            target_actor_id,
            decision.actor.actor_id,
            decision.actor.roles.contains(&PlatformRole::PlatformAdmin),
            &request,
            &idempotency(&headers)?,
            now()?,
        )
        .await?;
    Ok(with_etag(StatusCode::OK, &membership, membership.revision))
}

async fn create_project_upload(
    State(state): State<Arc<ApiState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path(project_id): Path<ProjectId>,
    headers: HeaderMap,
    Json(request): Json<CreateProblemPackageUploadRequest>,
) -> Result<Response, ApiError> {
    authorize_project(
        &state,
        &principal,
        &headers,
        "createProjectProblemPackageUpload",
        project_id,
    )
    .await?;
    let session = state
        .control
        .create_project_upload(project_id, &request, &idempotency(&headers)?, now()?)
        .await?;
    Ok(with_etag(StatusCode::CREATED, &session, session.revision))
}

async fn complete_project_upload(
    State(state): State<Arc<ApiState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path((project_id, upload_id)): Path<(ProjectId, UploadSessionId)>,
    headers: HeaderMap,
    Json(_request): Json<CompleteProblemPackageUploadRequest>,
) -> Result<Response, ApiError> {
    authorize_project(
        &state,
        &principal,
        &headers,
        "completeProjectProblemPackageUpload",
        project_id,
    )
    .await?;
    let package = state
        .control
        .complete_project_upload(
            project_id,
            upload_id,
            etag(&headers)?,
            &idempotency(&headers)?,
            now()?,
        )
        .await?;
    Ok(with_etag(StatusCode::CREATED, &package, package.revision))
}

async fn get_project_package(
    State(state): State<Arc<ApiState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path((project_id, package_id)): Path<(ProjectId, ProblemPackageId)>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    authorize_project(
        &state,
        &principal,
        &headers,
        "getProjectProblemPackage",
        project_id,
    )
    .await?;
    let package = state
        .control
        .project_package(project_id, package_id)
        .await?;
    Ok(with_etag(StatusCode::OK, &package, package.revision))
}

async fn create_project_policy(
    State(state): State<Arc<ApiState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path(project_id): Path<ProjectId>,
    headers: HeaderMap,
    Json(policy): Json<ProjectLlmEgressPolicy>,
) -> Result<Response, ApiError> {
    authorize_project(
        &state,
        &principal,
        &headers,
        "createProjectLlmPolicy",
        project_id,
    )
    .await?;
    let policy = state
        .control
        .activate_project_policy(project_id, policy, &idempotency(&headers)?)
        .await?;
    Ok(with_etag(StatusCode::CREATED, &policy, policy.revision))
}

async fn get_project_policy(
    State(state): State<Arc<ApiState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path(project_id): Path<ProjectId>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    authorize_project(
        &state,
        &principal,
        &headers,
        "getActiveProjectLlmPolicy",
        project_id,
    )
    .await?;
    let policy = state.control.active_project_policy(project_id).await?;
    Ok(with_etag(StatusCode::OK, &policy, policy.revision))
}

async fn create_project_agent_run(
    State(state): State<Arc<ApiState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path(project_id): Path<ProjectId>,
    headers: HeaderMap,
    Json(request): Json<CreateAgentRunRequest>,
) -> Result<Response, ApiError> {
    let environment_class = request.environment_class;
    create_project_agent_run_for_class(
        state,
        principal,
        project_id,
        headers,
        request,
        environment_class,
        "createProjectAgentRun",
    )
    .await
}

async fn create_project_work_configuration_run(
    State(state): State<Arc<ApiState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path(project_id): Path<ProjectId>,
    headers: HeaderMap,
    Json(request): Json<CreateWorkConfigurationRunRequest>,
) -> Result<Response, ApiError> {
    let decision = authorize_project(
        &state,
        &principal,
        &headers,
        "createProjectWorkConfigurationRun",
        project_id,
    )
    .await?;
    if request.project_id != project_id {
        return Err(ControlError::ProjectMismatch.into());
    }
    let project = state.control.project(project_id).await?;
    if project.state == contracts::ProjectState::Archived || request.course_id != project.course_id
    {
        return Err(ControlError::ProjectMismatch.into());
    }
    let target = state
        .environment
        .work_configuration_target(
            request.environment_id,
            &contracts::environment::EnvironmentWorkConfigurationTargetQuery {
                project_id,
                course_id: project.course_id,
                actor_id: decision.actor.actor_id,
                expected_revision: request.environment_revision,
            },
            &headers,
        )
        .await?;
    if target.environment_revision != request.environment_revision {
        return Err(ControlError::RevisionConflict.into());
    }
    let package = state
        .control
        .project_package(project_id, request.package_id)
        .await?;
    let object_locators = state
        .control
        .project_package_object_locators(project_id, &package)
        .await?;
    let policy = state.control.active_project_policy(project_id).await?;
    if package.revision != request.package_revision
        || policy.id != request.policy_id
        || policy.revision != request.policy_revision
    {
        return Err(ControlError::RevisionConflict.into());
    }
    let preauthorization = match (
        request.preauthorization_id,
        request.preauthorization_revision,
    ) {
        (Some(id), Some(revision)) => Some(
            state
                .control
                .work_configuration_preauthorization(project_id, id, revision)
                .await?,
        ),
        (None, None) => None,
        _ => return Err(ControlError::ContractInvalid.into()),
    };
    if let Some(grant) = &preauthorization
        && (grant.actor_id != decision.actor.actor_id
            || grant.environment_id != request.environment_id
            || grant.environment_revision != request.environment_revision)
    {
        return Err(ControlError::ProjectMismatch.into());
    }
    let run = state
        .agent
        .create(
            &InternalCreateAgentRunRequest {
                project_id,
                course_id: project.course_id,
                request: contracts::http::InternalAgentRunRequest::WorkConfiguration(
                    request.clone(),
                ),
                purpose: AgentRunPurpose::WorkConfiguration {
                    environment_id: request.environment_id,
                    environment_revision: request.environment_revision,
                    actor_id: decision.actor.actor_id,
                    runtime_kind: target.runtime_kind,
                },
                package,
                object_locators,
                policy,
                preauthorization,
            },
            &idempotency(&headers)?,
            &headers,
        )
        .await?;
    if run.project_id != project_id || run.course_id != project.course_id {
        return Err(DownstreamError::IdentityMismatch.into());
    }
    state
        .control
        .project_agent_run(EventId::new(), &run)
        .await?;
    Ok(accepted(&run))
}

async fn create_project_agent_run_for_class(
    state: Arc<ApiState>,
    principal: GatewayPrincipal,
    project_id: ProjectId,
    headers: HeaderMap,
    request: CreateAgentRunRequest,
    expected_environment_class: contracts::authoring::EnvironmentClass,
    authorization_operation: &'static str,
) -> Result<Response, ApiError> {
    authorize_project(
        &state,
        &principal,
        &headers,
        authorization_operation,
        project_id,
    )
    .await?;
    let project = state.control.project(project_id).await?;
    if project.state == contracts::ProjectState::Archived
        || request.project_id != project_id
        || request.course_id != project.course_id
    {
        return Err(ControlError::ProjectMismatch.into());
    }
    let key = idempotency(&headers)?;
    let package = state
        .control
        .project_package(project_id, request.package_id)
        .await?;
    let object_locators = state
        .control
        .project_package_object_locators(project_id, &package)
        .await?;
    let policy = state.control.active_project_policy(project_id).await?;
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
                project_id,
                course_id: project.course_id,
                request: contracts::http::InternalAgentRunRequest::Authoring(request),
                purpose: AgentRunPurpose::Authoring {
                    environment_class: expected_environment_class,
                },
                package,
                object_locators,
                policy,
                preauthorization: None,
            },
            &key,
            &headers,
        )
        .await?;
    if run.project_id != project_id || run.course_id != project.course_id {
        return Err(DownstreamError::IdentityMismatch.into());
    }
    state
        .control
        .project_agent_run(EventId::new(), &run)
        .await?;
    Ok(accepted(&run))
}

async fn get_project_agent_run(
    State(state): State<Arc<ApiState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path((project_id, run_id)): Path<(ProjectId, AgentRunId)>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    authorize_project(
        &state,
        &principal,
        &headers,
        "getProjectAgentRun",
        project_id,
    )
    .await?;
    let run = current_project_agent_run(&state, project_id, run_id).await?;
    Ok(with_etag(StatusCode::OK, &run, run.revision))
}

async fn get_project_work_configuration_plan(
    State(state): State<Arc<ApiState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path((project_id, run_id)): Path<(ProjectId, AgentRunId)>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    authorize_project(
        &state,
        &principal,
        &headers,
        "getProjectWorkConfigurationPlan",
        project_id,
    )
    .await?;
    let run = current_project_agent_run(&state, project_id, run_id).await?;
    let AgentRunPurpose::WorkConfiguration {
        environment_id,
        environment_revision,
        ..
    } = run.purpose
    else {
        return Err(ControlError::CandidateKindMismatch.into());
    };
    let plan = run.plan.clone().ok_or(ControlError::NotFound)?;
    let package = state
        .control
        .project_package(project_id, run.package_id)
        .await?;
    let query = GeneratedArtifactQuery {
        project_id,
        course_id: run.course_id,
        package_id: run.package_id,
        package_revision: package.revision,
    };
    if plan.environment_id != environment_id || plan.environment_revision != environment_revision {
        return Err(ControlError::PersistenceIdentityMismatch.into());
    }
    let script_record = state
        .agent
        .generated_artifact(plan.script_artifact.artifact_id, &query)
        .await?;
    if script_record.kind != GeneratedArtifactKind::WorkScript
        || script_record.artifact != plan.script_artifact
    {
        return Err(DownstreamError::IdentityMismatch.into());
    }
    let script_content = String::from_utf8(
        state
            .control
            .read_generated_artifact(&script_record)
            .await?,
    )
    .map_err(|_| ControlError::ObjectStoreIdentityMismatch)?;
    let verification_script_content = if let Some(reference) = &plan.verification_script_artifact {
        let record = state
            .agent
            .generated_artifact(reference.artifact_id, &query)
            .await?;
        if record.kind != GeneratedArtifactKind::VerificationScript || record.artifact != *reference
        {
            return Err(DownstreamError::IdentityMismatch.into());
        }
        Some(
            String::from_utf8(state.control.read_generated_artifact(&record).await?)
                .map_err(|_| ControlError::ObjectStoreIdentityMismatch)?,
        )
    } else {
        None
    };
    let view = WorkConfigurationPlanView {
        plan,
        script_content,
        verification_script_content,
    };
    Ok(with_etag(StatusCode::OK, &view, view.plan.revision))
}

async fn approve_project_work_configuration(
    State(state): State<Arc<ApiState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path((project_id, run_id)): Path<(ProjectId, AgentRunId)>,
    headers: HeaderMap,
    Json(request): Json<ApproveWorkConfigurationRequest>,
) -> Result<Response, ApiError> {
    let decision = authorize_project(
        &state,
        &principal,
        &headers,
        "approveProjectWorkConfigurationRun",
        project_id,
    )
    .await?;
    let run = current_project_agent_run(&state, project_id, run_id).await?;
    let AgentRunPurpose::WorkConfiguration { .. } = run.purpose else {
        return Err(ControlError::CandidateKindMismatch.into());
    };
    let key = idempotency(&headers)?;
    let approval_time = now()?;
    let grant = state
        .control
        .prepare_work_configuration_approval(
            project_id,
            run_id,
            &run,
            decision.actor.actor_id,
            &request,
            &key,
            approval_time,
        )
        .await?;
    let updated = state
        .agent
        .approve_work_configuration(
            run_id,
            &InternalApproveWorkConfigurationRequest {
                project_id,
                course_id: run.course_id,
                expected_run_revision: request.expected_run_revision,
                preauthorization: grant,
            },
            &key,
            &headers,
        )
        .await?;
    if updated.project_id != project_id
        || updated.course_id != run.course_id
        || updated.state != contracts::authoring::AgentRunState::Running
    {
        return Err(DownstreamError::IdentityMismatch.into());
    }
    state
        .control
        .project_agent_run(EventId::new(), &updated)
        .await?;
    Ok(accepted(&updated))
}

async fn cancel_project_agent_run(
    State(state): State<Arc<ApiState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path((project_id, run_id)): Path<(ProjectId, AgentRunId)>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    authorize_project(
        &state,
        &principal,
        &headers,
        "cancelProjectAgentRun",
        project_id,
    )
    .await?;
    let expected_revision = etag(&headers)?;
    let current = current_project_agent_run(&state, project_id, run_id).await?;
    if current.revision != expected_revision {
        return Err(ControlError::RevisionConflict.into());
    }
    let run = state
        .agent
        .cancel(
            run_id,
            &InternalAgentRunMutationRequest {
                project_id,
                course_id: current.course_id,
                expected_revision,
            },
            &idempotency(&headers)?,
            &headers,
        )
        .await?;
    if run.id != run_id || run.project_id != project_id || run.course_id != current.course_id {
        return Err(DownstreamError::IdentityMismatch.into());
    }
    Ok(accepted(&run))
}

async fn retry_project_agent_run(
    State(state): State<Arc<ApiState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path((project_id, run_id, track)): Path<(ProjectId, AgentRunId, String)>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    authorize_project(
        &state,
        &principal,
        &headers,
        "retryProjectAgentRunTrack",
        project_id,
    )
    .await?;
    let expected_revision = etag(&headers)?;
    let current = current_project_agent_run(&state, project_id, run_id).await?;
    if current.revision != expected_revision {
        return Err(ControlError::RevisionConflict.into());
    }
    let run = state
        .agent
        .retry(
            run_id,
            parse_track(&track)?,
            &InternalAgentRunMutationRequest {
                project_id,
                course_id: current.course_id,
                expected_revision,
            },
            &idempotency(&headers)?,
            &headers,
        )
        .await?;
    if run.id != run_id || run.project_id != project_id || run.course_id != current.course_id {
        return Err(DownstreamError::IdentityMismatch.into());
    }
    Ok(accepted(&run))
}

async fn get_project_environment_candidate(
    State(state): State<Arc<ApiState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path((project_id, candidate_id)): Path<(ProjectId, CandidateId)>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    authorize_project(
        &state,
        &principal,
        &headers,
        "getProjectEnvironmentCandidate",
        project_id,
    )
    .await?;
    let value = state
        .control
        .project_environment_candidate_view(project_id, candidate_id)
        .await?;
    Ok(with_etag(StatusCode::OK, &value, value.candidate.revision))
}

async fn get_project_evaluation_candidate(
    State(state): State<Arc<ApiState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path((project_id, candidate_id)): Path<(ProjectId, CandidateId)>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    authorize_project(
        &state,
        &principal,
        &headers,
        "getProjectEvaluationCandidate",
        project_id,
    )
    .await?;
    let value = state
        .control
        .project_evaluation_candidate_view(project_id, candidate_id)
        .await?;
    Ok(with_etag(StatusCode::OK, &value, value.candidate.revision))
}

async fn decide_project_environment_candidate(
    State(state): State<Arc<ApiState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path((project_id, candidate_id)): Path<(ProjectId, CandidateId)>,
    headers: HeaderMap,
    Json(request): Json<CandidateDecisionRequest>,
) -> Result<Response, ApiError> {
    let decision = authorize_project(
        &state,
        &principal,
        &headers,
        "appendProjectEnvironmentCandidateDecision",
        project_id,
    )
    .await?;
    let approval = state
        .control
        .decide_project_candidate(
            project_id,
            candidate_id,
            AgentTrackKind::Environment,
            &request,
            decision.actor.actor_id,
            etag(&headers)?,
            &idempotency(&headers)?,
            now()?,
        )
        .await?;
    Ok(with_etag(
        StatusCode::CREATED,
        &approval,
        approval.candidate_revision,
    ))
}

async fn decide_project_evaluation_candidate(
    State(state): State<Arc<ApiState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path((project_id, candidate_id)): Path<(ProjectId, CandidateId)>,
    headers: HeaderMap,
    Json(request): Json<CandidateDecisionRequest>,
) -> Result<Response, ApiError> {
    let decision = authorize_project(
        &state,
        &principal,
        &headers,
        "appendProjectEvaluationCandidateDecision",
        project_id,
    )
    .await?;
    let approval = state
        .control
        .decide_project_candidate(
            project_id,
            candidate_id,
            AgentTrackKind::Evaluation,
            &request,
            decision.actor.actor_id,
            etag(&headers)?,
            &idempotency(&headers)?,
            now()?,
        )
        .await?;
    Ok(with_etag(
        StatusCode::CREATED,
        &approval,
        approval.candidate_revision,
    ))
}

async fn complete_project_authoring_approval(
    State(state): State<Arc<ApiState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path(project_id): Path<ProjectId>,
    headers: HeaderMap,
    Json(request): Json<CompleteAuthoringApprovalRequest>,
) -> Result<Response, ApiError> {
    let decision = authorize_project(
        &state,
        &principal,
        &headers,
        "completeProjectAuthoringApproval",
        project_id,
    )
    .await?;
    let approval = state
        .control
        .complete_authoring_approval(
            project_id,
            &request,
            decision.actor.actor_id,
            &idempotency(&headers)?,
            now()?,
            &trace_id(&headers),
        )
        .await?;
    Ok(with_etag(StatusCode::CREATED, &approval, approval.revision))
}

async fn get_project_authoring_approval(
    State(state): State<Arc<ApiState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path((project_id, approval_id)): Path<(ProjectId, contracts::ApprovalId)>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    authorize_project(
        &state,
        &principal,
        &headers,
        "getProjectAuthoringApproval",
        project_id,
    )
    .await?;
    let status = state
        .control
        .authoring_approval_publication_status(project_id, approval_id)
        .await?;
    Ok(with_etag(StatusCode::OK, &status, status.revision))
}

async fn get_internal_authoring_publication_admission(
    State(state): State<Arc<ApiState>>,
    Path(approval_id): Path<contracts::ApprovalId>,
    Query(query): Query<AuthoringPublicationAdmissionQuery>,
) -> Result<Response, ApiError> {
    let binding = state
        .control
        .authoring_publication_admission(approval_id, &query)
        .await?;
    Ok(Json(binding).into_response())
}

async fn get_internal_project_llm_egress_policy(
    State(state): State<Arc<ApiState>>,
    Path(project_id): Path<ProjectId>,
) -> Result<Response, ApiError> {
    let policy = state.control.active_project_policy(project_id).await?;
    Ok(with_etag(StatusCode::OK, &policy, policy.revision))
}

async fn get_internal_work_configuration_admission(
    State(state): State<Arc<ApiState>>,
    Path(run_id): Path<AgentRunId>,
    Query(query): Query<WorkConfigurationAdmissionQuery>,
) -> Result<Response, ApiError> {
    let recovery = if let Some(execution_id) = query.execution_id {
        Some(
            state
                .agent
                .work_execution_intent(
                    run_id,
                    &AgentWorkExecutionIntentQuery {
                        project_id: query.project_id,
                        course_id: query.course_id,
                        execution_id,
                    },
                )
                .await?,
        )
    } else {
        None
    };
    // The Control projection is fed asynchronously from the Agent event stream.  Resolve the
    // current Agent aggregate first so a just-approved run is not rejected because its approval
    // event has not reached the projection yet, and a cancellation cannot be hidden by an older
    // projected revision.  This request is deliberately outside Control's database transaction;
    // the returned revision is fenced by the admission binding below.
    let authoritative_run = state.agent.get(run_id).await?;
    authoritative_run
        .validate()
        .map_err(|_| DownstreamError::IdentityMismatch)?;
    if authoritative_run.id != run_id
        || authoritative_run.project_id != query.project_id
        || authoritative_run.course_id != query.course_id
    {
        return Err(DownstreamError::IdentityMismatch.into());
    }
    let mut binding = state
        .control
        .work_configuration_admission_with_run(
            run_id,
            &query,
            &authoritative_run,
            recovery.as_ref(),
        )
        .await?;
    if let Some(plan) = binding.plan.as_ref() {
        let package = state
            .control
            .project_package(query.project_id, authoritative_run.package_id)
            .await?;
        let artifact_query = GeneratedArtifactQuery {
            project_id: query.project_id,
            course_id: authoritative_run.course_id,
            package_id: authoritative_run.package_id,
            package_revision: package.revision,
        };
        let script_record = state
            .agent
            .generated_artifact(plan.script_artifact.artifact_id, &artifact_query)
            .await?;
        if script_record.kind != GeneratedArtifactKind::WorkScript
            || script_record.artifact != plan.script_artifact
        {
            return Err(DownstreamError::IdentityMismatch.into());
        }
        let script_bytes = state
            .control
            .read_generated_artifact(&script_record)
            .await?;
        binding.script_sha256 = Sha256Digest::of_bytes(&script_bytes).to_string();
        if let Some(intent) = recovery.as_ref()
            && intent.script_sha256 != binding.script_sha256
        {
            return Err(DownstreamError::IdentityMismatch.into());
        }
        if let Some(reference) = plan.verification_script_artifact.as_ref() {
            let record = state
                .agent
                .generated_artifact(reference.artifact_id, &artifact_query)
                .await?;
            if record.kind != GeneratedArtifactKind::VerificationScript
                || record.artifact != *reference
            {
                return Err(DownstreamError::IdentityMismatch.into());
            }
            let bytes = state.control.read_generated_artifact(&record).await?;
            binding.verification_script_sha256 = Some(Sha256Digest::of_bytes(&bytes).to_string());
            if let Some(intent) = recovery.as_ref()
                && intent.verification_script_sha256 != binding.verification_script_sha256
            {
                return Err(DownstreamError::IdentityMismatch.into());
            }
        } else if recovery
            .as_ref()
            .is_some_and(|intent| intent.verification_script_sha256.is_some())
        {
            return Err(DownstreamError::IdentityMismatch.into());
        }
    }
    binding
        .validate_recovery_for(query.execution_id)
        .map_err(|_| DownstreamError::IdentityMismatch)?;
    Ok(Json(binding).into_response())
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
    Json(policy): Json<ProjectLlmEgressPolicy>,
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
                project_id: request.project_id,
                course_id: request.course_id,
                request: contracts::http::InternalAgentRunRequest::Authoring(request),
                purpose: AgentRunPurpose::Authoring {
                    environment_class: expected_environment_class,
                },
                package,
                object_locators,
                policy,
                preauthorization: None,
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
                project_id: projected.project_id,
                course_id: projected.course_id,
                expected_revision,
            },
            &key,
            &headers,
        )
        .await?;
    if run.course_id != Some(course_id) {
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
                project_id: projected.project_id,
                course_id: projected.course_id,
                expected_revision,
            },
            &key,
            &headers,
        )
        .await?;
    if run.course_id != Some(course_id) {
        return Err(DownstreamError::IdentityMismatch.into());
    }
    Ok(accepted(&run))
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
    if release.course_id != request.course_id
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
    if release.course_id != Some(course_id) {
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
    let current_release = state.evaluation.get(release_id, &headers).await?;
    let release = state
        .evaluation
        .withdraw(
            release_id,
            &InternalWithdrawEvaluationReleaseRequest {
                project_id: current_release.project_id,
                course_id: current_release.course_id,
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

async fn create_project_work_release(
    State(state): State<Arc<ApiState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path(project_id): Path<ProjectId>,
    headers: HeaderMap,
    Json(request): Json<CreateEnvironmentTemplateReleaseRequest>,
) -> Result<Response, ApiError> {
    let decision = authorize_project(
        &state,
        &principal,
        &headers,
        "createEnvironmentTemplateRelease",
        project_id,
    )
    .await?;
    let key = idempotency(&headers)?;
    let published_at = now()?;
    let trace_id = trace_id(&headers);
    let release = state
        .control
        .create_project_work_release(
            project_id,
            &request,
            decision.actor.actor_id,
            decision.actor.roles.contains(&PlatformRole::PlatformAdmin),
            &key,
            published_at,
            &trace_id,
        )
        .await?;
    Ok(Json(OperationAccepted {
        operation_id: OperationId::new(),
        revision: Revision::new(release.version)
            .map_err(|_| ApiError::internal("LW_CONTRACT_DOCUMENT_INVALID"))?,
        status_url: format!(
            "/api/v1/projects/{project_id}/environment-template-releases/{}",
            release.id,
        ),
    })
    .into_response())
}

#[derive(Deserialize)]
struct ProjectReleaseQuery {
    cursor: Option<String>,
    #[serde(default = "default_project_release_limit")]
    limit: u32,
    #[serde(rename = "courseId")]
    course_id: Option<CourseId>,
}

fn default_project_release_limit() -> u32 {
    50
}

fn default_limit() -> u32 {
    100
}

fn release_cursor(value: Option<&str>) -> Result<u64, ApiError> {
    let Some(value) = value else {
        return Ok(0);
    };
    if value.is_empty() || value.len() > 512 {
        return Err(ApiError::bad_request("LW_CONTRACT_DOCUMENT_INVALID"));
    }
    value
        .parse::<u64>()
        .map_err(|_| ApiError::bad_request("LW_CONTRACT_DOCUMENT_INVALID"))
}

async fn list_project_releases(
    State(state): State<Arc<ApiState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path(project_id): Path<ProjectId>,
    headers: HeaderMap,
    Query(query): Query<ProjectReleaseQuery>,
) -> Result<Response, ApiError> {
    let decision = authorize_project(
        &state,
        &principal,
        &headers,
        "listEnvironmentTemplateReleases",
        project_id,
    )
    .await?;
    let after_version = release_cursor(query.cursor.as_deref())?;
    let items = state
        .control
        .project_releases(
            project_id,
            query.course_id,
            after_version,
            query.limit,
            decision.actor.actor_id,
        )
        .await?;
    let next_cursor = items.last().map(|view| view.release.version.to_string());
    Ok(Json(CursorPage { items, next_cursor }).into_response())
}

async fn get_project_release(
    State(state): State<Arc<ApiState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path((project_id, release_id)): Path<(ProjectId, ReleaseId)>,
    headers: HeaderMap,
    Query(query): Query<ProjectReleaseQuery>,
) -> Result<Response, ApiError> {
    let decision = authorize_project(
        &state,
        &principal,
        &headers,
        "getEnvironmentTemplateRelease",
        project_id,
    )
    .await?;
    let release = state
        .control
        .project_release(
            project_id,
            release_id,
            query.course_id,
            decision.actor.actor_id,
        )
        .await?;
    Ok(with_etag(
        StatusCode::OK,
        &release,
        Revision::new(release.release.version)
            .map_err(|_| ApiError::internal("LW_CONTRACT_DOCUMENT_INVALID"))?,
    ))
}

async fn withdraw_project_release(
    State(state): State<Arc<ApiState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path((project_id, release_id)): Path<(ProjectId, ReleaseId)>,
    headers: HeaderMap,
    Json(request): Json<WithdrawEnvironmentTemplateReleaseRequest>,
) -> Result<Response, ApiError> {
    let decision = authorize_project(
        &state,
        &principal,
        &headers,
        "withdrawEnvironmentTemplateRelease",
        project_id,
    )
    .await?;
    let expected = etag(&headers)?;
    let withdrawal = state
        .control
        .withdraw_project_release(
            project_id,
            release_id,
            expected.get(),
            decision.actor.actor_id,
            decision.actor.roles.contains(&PlatformRole::PlatformAdmin),
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

async fn project_events(
    State(state): State<Arc<ApiState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path(project_id): Path<ProjectId>,
    headers: HeaderMap,
    Query(query): Query<EventQuery>,
) -> Result<Response, ApiError> {
    let authorization = authorize_project(
        &state,
        &principal,
        &headers,
        "streamProjectEvents",
        project_id,
    )
    .await?;
    let last_event_id = headers
        .get("Last-Event-ID")
        .and_then(|value| value.to_str().ok())
        .map(str::parse::<u64>)
        .transpose()
        .map_err(|_| ApiError::bad_request("LW_SSE_CURSOR_GAP"))?;
    let after = resolve_sse_resume(
        last_event_id.map(StreamSequence),
        query.after.map(StreamSequence),
    )
    .map_err(|_| ApiError::bad_request("LW_SSE_CURSOR_GAP"))?;
    let after = match after {
        contracts::http::SseResume::Beginning => None,
        contracts::http::SseResume::After(sequence) => Some(sequence.0),
    };
    let records = state
        .control
        .project_sse_page(project_id, after, query.limit, now()?)
        .await?;
    let cursor = records
        .last()
        .map_or(after.unwrap_or(0), |record| record.sequence);
    let output = stream::unfold(
        ProjectEventStreamState {
            control: state.control.clone(),
            project_id,
            cursor,
            pending: records.into(),
            valid_until: authorization.valid_until,
        },
        |mut state| async move {
            loop {
                let Ok(timestamp) = current_timestamp() else {
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
                    .project_sse_page(state.project_id, Some(state.cursor), 256, timestamp)
                    .await
                {
                    Ok(records) => state.pending = records.into(),
                    Err(error) => {
                        tracing::error!(event = "control.project_sse.poll_failed", diagnostic = %error,
                            project_id = %state.project_id, cursor = state.cursor);
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

struct ProjectEventStreamState {
    control: ControlService,
    project_id: ProjectId,
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

async fn authorize_global(
    state: &ApiState,
    principal: &GatewayPrincipal,
    headers: &HeaderMap,
    operation: &str,
) -> Result<contracts::AuthorizationDecision, ApiError> {
    if principal.client_id.trim().is_empty() {
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
                scope: AuthorizationScope::Global,
                authorization_revision: None,
                scope_revision: None,
            },
            headers,
        )
        .await?;
    let current = now()?;
    if decision.actor.actor_id != actor_id
        || decision.scope != AuthorizationScope::Global
        || decision.valid_until <= current
        || decision.diagnostic_code.is_some()
    {
        return Err(ApiError::forbidden("LW_AUTH_SCOPE_DENIED"));
    }
    Ok(decision)
}

async fn authorize_project(
    state: &ApiState,
    principal: &GatewayPrincipal,
    headers: &HeaderMap,
    operation: &str,
    project_id: ProjectId,
) -> Result<contracts::AuthorizationDecision, ApiError> {
    if principal.client_id.trim().is_empty() {
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
                scope: AuthorizationScope::Project { project_id },
                authorization_revision: None,
                scope_revision: None,
            },
            headers,
        )
        .await?;
    let current = now()?;
    if decision.actor.actor_id != actor_id
        || decision.scope != (AuthorizationScope::Project { project_id })
        || decision.valid_until <= current
        || decision.diagnostic_code.is_some()
    {
        return Err(ApiError::forbidden("LW_AUTH_SCOPE_DENIED"));
    }
    Ok(decision)
}

fn project_owner_role(roles: &[PlatformRole]) -> Result<PlatformRole, ApiError> {
    if roles.contains(&PlatformRole::PlatformAdmin) {
        Ok(PlatformRole::PlatformAdmin)
    } else if roles.contains(&PlatformRole::Teacher) {
        Ok(PlatformRole::Teacher)
    } else if roles.contains(&PlatformRole::Student) {
        Ok(PlatformRole::Student)
    } else {
        Err(ApiError::forbidden("LW_AUTH_ROLE_DENIED"))
    }
}

async fn authorize_decision(
    state: &ApiState,
    principal: &GatewayPrincipal,
    headers: &HeaderMap,
    operation: &str,
    course_id: CourseId,
) -> Result<contracts::AuthorizationDecision, ApiError> {
    if principal.client_id.trim().is_empty() {
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

/// Reads the current Agent aggregate after the project membership decision has succeeded.
///
/// Agent state reaches Control's projection asynchronously. Public Work controls therefore use
/// this authenticated downstream read for the revision and plan while keeping the projection as
/// an event-fed read model for listings and history. The project lookup binds the optional course
/// association to the route scope before any run state is used.
async fn current_project_agent_run(
    state: &ApiState,
    project_id: ProjectId,
    run_id: AgentRunId,
) -> Result<AgentRun, ApiError> {
    let project = state.control.project(project_id).await?;
    let run = state.agent.get(run_id).await?;
    run.validate()
        .map_err(|_| DownstreamError::ProtocolInvalid)?;
    if run.id != run_id || run.project_id != project_id || run.course_id != project.course_id {
        return Err(DownstreamError::IdentityMismatch.into());
    }
    Ok(run)
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
        "work_configuration" => Ok(AgentTrackKind::WorkConfiguration),
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
            ControlError::CourseMismatch | ControlError::ProjectMismatch => StatusCode::FORBIDDEN,
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

impl From<auth::ServiceAuthError> for ApiError {
    fn from(error: auth::ServiceAuthError) -> Self {
        match error {
            auth::ServiceAuthError::PermissionDenied => {
                Self::forbidden("LW_AUTH_SERVICE_PERMISSION_DENIED")
            }
            auth::ServiceAuthError::CredentialsMissing
            | auth::ServiceAuthError::TokenRejected
            | auth::ServiceAuthError::TokenExpired => {
                Self::unauthorized("LW_AUTH_SERVICE_TOKEN_INVALID")
            }
            auth::ServiceAuthError::InvalidConfig
            | auth::ServiceAuthError::JwksUnavailable
            | auth::ServiceAuthError::EndpointTransport
            | auth::ServiceAuthError::HttpClient => Self::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "LW_AUTH_SERVICE_UNAVAILABLE",
                true,
            ),
        }
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
