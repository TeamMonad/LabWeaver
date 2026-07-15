use std::collections::BTreeSet;

use axum::extract::rejection::{JsonRejection, PathRejection};
use axum::extract::{Extension, Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use contracts::environment::{
    DesiredEnvironmentState, EndpointHealth, EnvironmentInstance, EnvironmentOwnerResolution,
    EnvironmentOwnerResolutionRequest, ObservedEnvironmentState,
};
use contracts::http::StrongEtag;
use contracts::{DiagnosticCode, EnvironmentId, EventId, ProblemDetails, UtcTimestamp};

use crate::{EnvironmentStoreError, PgEnvironmentStore};

/// Peer identity produced only after the serving TLS layer verifies the client certificate.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedCallerIdentity {
    sans: BTreeSet<String>,
}

impl VerifiedCallerIdentity {
    /// Constructs the identity passed by the mTLS acceptor after certificate verification.
    pub(crate) fn from_mtls_peer_sans<I, S>(sans: I) -> Result<Self, OwnerResolverError>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let sans = sans.into_iter().map(Into::into).collect::<BTreeSet<_>>();
        if sans.is_empty() || sans.iter().any(|san| !valid_san(san)) {
            return Err(OwnerResolverError::CallerUntrusted);
        }
        Ok(Self { sans })
    }
}

/// Exact caller SAN policy; wildcard and empty policies are not supported.
#[derive(Clone, Debug)]
pub struct OwnerResolverPolicy {
    allowed_caller_sans: BTreeSet<String>,
}

impl OwnerResolverPolicy {
    pub fn new<I, S>(allowed_caller_sans: I) -> Result<Self, OwnerResolverError>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let allowed_caller_sans = allowed_caller_sans
            .into_iter()
            .map(Into::into)
            .collect::<BTreeSet<_>>();
        if allowed_caller_sans.is_empty() || allowed_caller_sans.iter().any(|san| !valid_san(san)) {
            return Err(OwnerResolverError::PolicyInvalid);
        }
        Ok(Self {
            allowed_caller_sans,
        })
    }

    fn allows(&self, caller: &VerifiedCallerIdentity) -> bool {
        !self.allowed_caller_sans.is_disjoint(&caller.sans)
    }
}

/// Environment-owned resolver backed only by the Environment runtime database role.
#[derive(Clone)]
pub struct OwnerResolver {
    store: PgEnvironmentStore,
    policy: OwnerResolverPolicy,
}

impl OwnerResolver {
    #[must_use]
    pub fn new(store: PgEnvironmentStore, policy: OwnerResolverPolicy) -> Self {
        Self { store, policy }
    }

    pub async fn resolve(
        &self,
        caller: &VerifiedCallerIdentity,
        request: &EnvironmentOwnerResolutionRequest,
    ) -> Result<EnvironmentOwnerResolution, OwnerResolverError> {
        if !self.policy.allows(caller) {
            return Err(OwnerResolverError::CallerUntrusted);
        }
        let (instance, authority_now) = self
            .store
            .load_for_owner_resolution(request.environment_id)
            .await
            .map_err(|error| match error {
                EnvironmentStoreError::EnvironmentNotFound => OwnerResolverError::ScopeMismatch,
                other => OwnerResolverError::Unavailable(other),
            })?;
        authorize_owner_resolution(&instance, request, authority_now)
    }
}

/// Performs the fail-closed ownership decision against one authoritative aggregate.
pub fn authorize_owner_resolution(
    instance: &EnvironmentInstance,
    request: &EnvironmentOwnerResolutionRequest,
    now: UtcTimestamp,
) -> Result<EnvironmentOwnerResolution, OwnerResolverError> {
    if instance.id != request.environment_id
        || instance.course_id != request.course_id
        || instance.owner_id != request.owner_actor_id
        || instance.revision != request.expected_revision
    {
        return Err(OwnerResolverError::ScopeMismatch);
    }
    if instance.desired_state != DesiredEnvironmentState::Running
        || instance.observed_state != ObservedEnvironmentState::Ready
        || instance.eligibility_expires_at <= now
        || instance.endpoints.is_empty()
        || instance.endpoints.iter().any(|endpoint| {
            endpoint.health != EndpointHealth::Healthy || endpoint.revision != instance.revision
        })
    {
        return Err(OwnerResolverError::EnvironmentUnavailable);
    }
    Ok(EnvironmentOwnerResolution {
        environment_id: instance.id,
        course_id: instance.course_id,
        owner_actor_id: instance.owner_id,
        environment_revision: instance.revision,
        eligibility_expires_at: instance.eligibility_expires_at,
    })
}

/// Builds the internal route. A TLS acceptor must inject `VerifiedCallerIdentity`.
pub fn owner_resolver_router(resolver: OwnerResolver) -> Router {
    Router::new()
        .route(
            "/internal/v1/environments/{environment_id}/owner:resolve",
            post(resolve_owner),
        )
        .with_state(resolver)
}

async fn resolve_owner(
    State(resolver): State<OwnerResolver>,
    path: Result<Path<EnvironmentId>, PathRejection>,
    caller: Option<Extension<VerifiedCallerIdentity>>,
    body: Result<Json<EnvironmentOwnerResolutionRequest>, JsonRejection>,
) -> Result<Response, OwnerResolverError> {
    let Path(environment_id) = path.map_err(|_| OwnerResolverError::RequestInvalid)?;
    let Json(request) = body.map_err(|_| OwnerResolverError::RequestInvalid)?;
    if request.environment_id != environment_id {
        return Err(OwnerResolverError::ScopeMismatch);
    }
    let Extension(caller) = caller.ok_or(OwnerResolverError::CallerUntrusted)?;
    let resolution = resolver.resolve(&caller, &request).await?;
    let etag = StrongEtag::from_revision(resolution.environment_revision).header_value();
    let mut response = Json(resolution).into_response();
    response.headers_mut().insert(
        header::ETAG,
        HeaderValue::from_str(&etag).map_err(|_| OwnerResolverError::ResponseInvalid)?,
    );
    Ok(response)
}

fn valid_san(san: &str) -> bool {
    !san.is_empty()
        && san.len() <= 253
        && !san.contains('*')
        && san
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.:".contains(&byte))
}

#[derive(Debug, thiserror::Error)]
pub enum OwnerResolverError {
    #[error("LW_CONTRACT_DOCUMENT_INVALID")]
    RequestInvalid,
    #[error("LW_ENV_OWNER_CALLER_UNTRUSTED")]
    CallerUntrusted,
    #[error("LW_ENV_OWNER_POLICY_INVALID")]
    PolicyInvalid,
    #[error("LW_ENV_OWNER_SCOPE_MISMATCH")]
    ScopeMismatch,
    #[error("LW_ENV_OWNER_UNAVAILABLE")]
    EnvironmentUnavailable,
    #[error("LW_ENV_OWNER_RESOLVER_UNAVAILABLE")]
    Unavailable(EnvironmentStoreError),
    #[error("LW_ENV_OWNER_RESPONSE_INVALID")]
    ResponseInvalid,
}

impl OwnerResolverError {
    const fn response_fields(&self) -> (StatusCode, &'static str, bool) {
        match self {
            Self::RequestInvalid => (
                StatusCode::BAD_REQUEST,
                "LW_CONTRACT_DOCUMENT_INVALID",
                false,
            ),
            Self::CallerUntrusted => (
                StatusCode::FORBIDDEN,
                "LW_ENV_OWNER_CALLER_UNTRUSTED",
                false,
            ),
            Self::PolicyInvalid => (
                StatusCode::SERVICE_UNAVAILABLE,
                "LW_ENV_OWNER_POLICY_INVALID",
                false,
            ),
            Self::ScopeMismatch => (StatusCode::FORBIDDEN, "LW_ENV_OWNER_SCOPE_MISMATCH", false),
            Self::EnvironmentUnavailable => {
                (StatusCode::FORBIDDEN, "LW_ENV_OWNER_UNAVAILABLE", false)
            }
            Self::Unavailable(_) => (
                StatusCode::SERVICE_UNAVAILABLE,
                "LW_ENV_OWNER_RESOLVER_UNAVAILABLE",
                true,
            ),
            Self::ResponseInvalid => (
                StatusCode::SERVICE_UNAVAILABLE,
                "LW_ENV_OWNER_RESPONSE_INVALID",
                false,
            ),
        }
    }
}

impl IntoResponse for OwnerResolverError {
    fn into_response(self) -> Response {
        let (status, code, retryable) = self.response_fields();
        (
            status,
            [(header::CONTENT_TYPE, "application/problem+json")],
            Json(ProblemDetails {
                problem_type: "urn:labweaver:problem:environment-owner-resolution".to_owned(),
                title: "Environment owner resolution denied".to_owned(),
                status: status.as_u16(),
                detail: "The authoritative Environment ownership check did not succeed.".to_owned(),
                instance: String::new(),
                diagnostic_code: DiagnosticCode::registered(code),
                request_id: EventId::new().to_string(),
                trace_id: None,
                retryable,
                violations: Vec::new(),
            }),
        )
            .into_response()
    }
}
