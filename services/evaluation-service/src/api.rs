//! Access-BFF authenticated public freeze API.
#![allow(
    missing_docs,
    clippy::missing_errors_doc,
    reason = "the public contract and stable diagnostics define this narrow HTTP surface"
)]

use std::{str::FromStr, sync::Arc};

use auth::extract_mtls_principal;
use axum::{
    Extension, Json, Router,
    body::Bytes,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use contracts::{
    ActorId, DiagnosticCode, EnvironmentId, FrozenSubmissionId, OperationId, ProblemDetails,
    Revision,
    http::{FreezeSubmissionRequest, IdempotencyKey, OperationAccepted, StrongEtag},
    submission::FrozenSubmission,
};
use hyper_util::{
    rt::{TokioExecutor, TokioIo},
    server::conn::auto::Builder as HyperBuilder,
    service::TowerToHyperService,
};

use crate::{
    FreezeCommandStoreError, PgFreezeCommandStore, PgFreezeStore, SubmissionFreezeCommand,
    freeze_store::FreezeStoreError,
};

const ACCESS_SERVICE_SAN: &str = "spiffe://labweaver/access-service";
const ACTOR_HEADER: &str = "x-labweaver-actor-id";
const SESSION_HEADER: &str = "x-labweaver-session-id";

#[derive(Clone)]
pub struct EvaluationApiState {
    commands: PgFreezeCommandStore,
    submissions: PgFreezeStore,
}

impl EvaluationApiState {
    #[must_use]
    pub const fn new(commands: PgFreezeCommandStore, submissions: PgFreezeStore) -> Self {
        Self {
            commands,
            submissions,
        }
    }
}

#[derive(Clone, Debug)]
pub struct GatewayPrincipal {
    san_uri: String,
}

pub fn evaluation_api_router(state: EvaluationApiState) -> Router {
    Router::new()
        .route(
            "/api/v1/environments/{environment_id}/freeze",
            post(freeze_submission),
        )
        .route(
            "/api/v1/frozen-submissions/{submission_id}",
            get(get_frozen_submission),
        )
        .with_state(state)
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
    let idempotency_key = headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
        .ok_or(EvaluationApiError::IdempotencyRequired)
        .and_then(|value| {
            IdempotencyKey::parse(value).map_err(|_| EvaluationApiError::IdempotencyInvalid)
        })?;
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
        trace_id: format!("evaluation-api-{}", uuid::Uuid::now_v7()),
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

async fn get_frozen_submission(
    State(state): State<EvaluationApiState>,
    principal: Option<Extension<GatewayPrincipal>>,
    Path(submission_id): Path<FrozenSubmissionId>,
    headers: HeaderMap,
) -> Result<Json<FrozenSubmission>, EvaluationApiError> {
    require_access(principal)?;
    require_session(&headers)?;
    Ok(Json(
        state
            .submissions
            .load_completed(submission_id, actor(&headers)?)
            .await?,
    ))
}

fn require_access(
    principal: Option<Extension<GatewayPrincipal>>,
) -> Result<(), EvaluationApiError> {
    if principal.is_some_and(|Extension(principal)| principal.san_uri == ACCESS_SERVICE_SAN) {
        Ok(())
    } else {
        Err(EvaluationApiError::CallerDenied)
    }
}

fn actor(headers: &HeaderMap) -> Result<ActorId, EvaluationApiError> {
    headers
        .get(ACTOR_HEADER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| ActorId::from_str(value).ok())
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
                    event = "evaluation.mtls.handshake_denied",
                    diagnostic = "LW_AUTH_SERVICE_IDENTITY_DENIED"
                );
                return;
            };
            let Some(peer) = tls
                .get_ref()
                .1
                .peer_certificates()
                .and_then(|values| values.first())
            else {
                tracing::warn!(
                    event = "evaluation.mtls.peer_denied",
                    diagnostic = "LW_AUTH_SERVICE_IDENTITY_DENIED"
                );
                return;
            };
            let Ok(san_uri) = extract_mtls_principal(peer, &allowed) else {
                tracing::warn!(
                    event = "evaluation.mtls.peer_denied",
                    diagnostic = "LW_AUTH_SERVICE_IDENTITY_DENIED"
                );
                return;
            };
            let service = router.layer(Extension(GatewayPrincipal { san_uri }));
            if HyperBuilder::new(TokioExecutor::new())
                .serve_connection_with_upgrades(
                    TokioIo::new(tls),
                    TowerToHyperService::new(service),
                )
                .await
                .is_err()
            {
                tracing::warn!(
                    event = "evaluation.mtls.connection_failed",
                    diagnostic = "LW_EVALUATION_CONNECTION_FAILED"
                );
            }
        });
    }
}

#[derive(Debug, thiserror::Error)]
pub enum EvaluationApiError {
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
    #[error(transparent)]
    Command(#[from] FreezeCommandStoreError),
    #[error(transparent)]
    Submission(#[from] FreezeStoreError),
}

impl IntoResponse for EvaluationApiError {
    fn into_response(self) -> Response {
        let status = match self {
            Self::CallerDenied => StatusCode::FORBIDDEN,
            Self::IdentityInvalid => StatusCode::UNAUTHORIZED,
            Self::RequestInvalid | Self::IdempotencyRequired | Self::IdempotencyInvalid => {
                StatusCode::BAD_REQUEST
            }
            Self::RevisionRequired => StatusCode::PRECONDITION_REQUIRED,
            Self::RevisionInvalid => StatusCode::PRECONDITION_FAILED,
            Self::Command(FreezeCommandStoreError::IdempotencyConflict) => StatusCode::CONFLICT,
            Self::Submission(FreezeStoreError::NotFound) => StatusCode::NOT_FOUND,
            _ => StatusCode::SERVICE_UNAVAILABLE,
        };
        let diagnostic = self.to_string();
        tracing::warn!(event = "evaluation.api.rejected", %diagnostic, status = status.as_u16());
        (
            status,
            Json(ProblemDetails {
                problem_type: "urn:labweaver:problem:evaluation-request-rejected".to_owned(),
                title: "Evaluation request rejected".to_owned(),
                status: status.as_u16(),
                detail: "The freeze request could not be accepted.".to_owned(),
                instance: String::new(),
                diagnostic_code: DiagnosticCode::parse(diagnostic)
                    .unwrap_or_else(|_| DiagnosticCode::registered("LW_EVALUATION_REQUEST_FAILED")),
                request_id: uuid::Uuid::now_v7().to_string(),
                trace_id: None,
                retryable: status == StatusCode::SERVICE_UNAVAILABLE,
                violations: Vec::new(),
            }),
        )
            .into_response()
    }
}
