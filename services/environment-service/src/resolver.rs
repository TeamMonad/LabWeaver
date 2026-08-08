use std::collections::BTreeSet;

use axum::body::Bytes;
use axum::extract::rejection::PathRejection;
use axum::extract::{Extension, Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use contracts::access::ConsoleLeaseFence;
use contracts::authoring::{EnvironmentClass, EnvironmentRuntimeSpec, RuntimeKind};
use contracts::environment::{
    DesiredEnvironmentState, EndpointHealth, EnvironmentAccessSubjectKind,
    EnvironmentConsoleBinding, EnvironmentConsoleEligibility, EnvironmentConsoleEligibilityRequest,
    EnvironmentEndpointEligibility, EnvironmentEndpointEligibilityRequest, EnvironmentInstance,
    EnvironmentOwnerResolution, EnvironmentOwnerResolutionRequest, ObservedEnvironmentState,
};
use contracts::http::StrongEtag;
use contracts::{DiagnosticCode, EnvironmentId, EventId, ProblemDetails, UtcTimestamp};

use crate::{
    ContainerReleaseResolver, EnvironmentStoreError, PgEnvironmentStore, PgReleaseProjectionStore,
    ReleaseProjectionError,
};

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

    /// Returns whether the verified peer certificate carried one exact URI SAN.
    #[must_use]
    pub fn contains_san(&self, san: &str) -> bool {
        self.sans.contains(san)
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
    releases: PgReleaseProjectionStore,
    policy: OwnerResolverPolicy,
}

impl OwnerResolver {
    #[must_use]
    pub fn new(
        store: PgEnvironmentStore,
        releases: PgReleaseProjectionStore,
        policy: OwnerResolverPolicy,
    ) -> Self {
        Self {
            store,
            releases,
            policy,
        }
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

    pub async fn resolve_endpoint_eligibility(
        &self,
        caller: &VerifiedCallerIdentity,
        request: &EnvironmentEndpointEligibilityRequest,
    ) -> Result<EnvironmentEndpointEligibility, OwnerResolverError> {
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
        authorize_endpoint_eligibility(&instance, request, authority_now)
    }

    pub async fn resolve_console_eligibility(
        &self,
        caller: &VerifiedCallerIdentity,
        request: &EnvironmentConsoleEligibilityRequest,
    ) -> Result<EnvironmentConsoleEligibility, OwnerResolverError> {
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
        authorize_console_instance(&instance, request, authority_now)?;
        let release = self
            .releases
            .resolve(instance.release_id, instance.release_version)
            .await
            .map_err(OwnerResolverError::ReleaseUnavailable)?;
        if release.withdrawn_at.is_some()
            || release.projection.release.course_id != instance.course_id
            || release.projection.release.runtime_kind != instance.runtime_kind
            || release.projection.environment_spec.class != instance.class
        {
            return Err(OwnerResolverError::EnvironmentUnavailable);
        }
        let binding = match &release.projection.environment_spec.runtime {
            EnvironmentRuntimeSpec::Container {
                terminal: Some(terminal),
                ..
            } if instance.runtime_kind == RuntimeKind::Container => {
                EnvironmentConsoleBinding::Xterm {
                    terminal: terminal.as_ref().clone(),
                }
            }
            EnvironmentRuntimeSpec::VirtualMachine { .. }
                if instance.runtime_kind == RuntimeKind::VirtualMachine =>
            {
                EnvironmentConsoleBinding::Novnc
            }
            _ => return Err(OwnerResolverError::EnvironmentUnavailable),
        };
        let lease_fence = match instance.class {
            EnvironmentClass::Experiment => None,
            EnvironmentClass::Work => {
                let authorization = instance
                    .operation
                    .lease_authorization
                    .as_ref()
                    .ok_or(OwnerResolverError::EnvironmentUnavailable)?;
                if authorization.expires_at <= authority_now {
                    return Err(OwnerResolverError::EnvironmentUnavailable);
                }
                Some(ConsoleLeaseFence {
                    lease_id: authorization.lease_id,
                    lease_revision: authorization.lease_revision,
                    expires_at: authorization.expires_at,
                })
            }
        };
        let eligibility_expires_at = lease_fence
            .as_ref()
            .map_or(instance.eligibility_expires_at, |fence| {
                std::cmp::min(instance.eligibility_expires_at, fence.expires_at)
            });
        let resolution = EnvironmentConsoleEligibility {
            environment_id: instance.id,
            course_id: instance.course_id,
            owner_actor_id: instance.owner_id,
            environment_class: instance.class,
            runtime_kind: instance.runtime_kind,
            environment_revision: instance.revision,
            release_id: instance.release_id,
            release_version: instance.release_version,
            eligibility_expires_at,
            lease_fence,
            binding,
        };
        resolution
            .validate_for(request, authority_now)
            .map_err(|_| OwnerResolverError::ResponseInvalid)?;
        Ok(resolution)
    }
}

fn authorize_console_instance(
    instance: &EnvironmentInstance,
    request: &EnvironmentConsoleEligibilityRequest,
    now: UtcTimestamp,
) -> Result<(), OwnerResolverError> {
    if instance.id != request.environment_id
        || instance.course_id != request.course_id
        || (request.subject_kind == EnvironmentAccessSubjectKind::Owner
            && instance.owner_id != request.actor_id)
        || instance.revision != request.expected_revision
    {
        return Err(OwnerResolverError::ScopeMismatch);
    }
    if instance.desired_state != DesiredEnvironmentState::Running
        || instance.observed_state != ObservedEnvironmentState::Ready
        || instance.eligibility_expires_at <= now
    {
        return Err(OwnerResolverError::EnvironmentUnavailable);
    }
    Ok(())
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

/// Returns only the requested healthy endpoint facts after exact scope validation.
pub fn authorize_endpoint_eligibility(
    instance: &EnvironmentInstance,
    request: &EnvironmentEndpointEligibilityRequest,
    now: UtcTimestamp,
) -> Result<EnvironmentEndpointEligibility, OwnerResolverError> {
    if instance.id != request.environment_id
        || instance.course_id != request.course_id
        || (request.subject_kind == EnvironmentAccessSubjectKind::Owner
            && instance.owner_id != request.actor_id)
        || instance.revision != request.expected_revision
    {
        return Err(OwnerResolverError::ScopeMismatch);
    }
    let requested = request
        .endpoint_ids
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    if requested.is_empty() || requested.len() != request.endpoint_ids.len() {
        return Err(OwnerResolverError::RequestInvalid);
    }
    if instance.desired_state != DesiredEnvironmentState::Running
        || instance.observed_state != ObservedEnvironmentState::Ready
        || instance.eligibility_expires_at <= now
    {
        return Err(OwnerResolverError::EnvironmentUnavailable);
    }
    let endpoints = instance
        .endpoints
        .iter()
        .filter(|endpoint| requested.contains(&endpoint.id))
        .cloned()
        .collect::<Vec<_>>();
    if endpoints.len() != requested.len()
        || endpoints.iter().any(|endpoint| {
            endpoint.health != EndpointHealth::Healthy || endpoint.revision != instance.revision
        })
    {
        return Err(OwnerResolverError::EndpointUnavailable);
    }
    Ok(EnvironmentEndpointEligibility {
        environment_id: instance.id,
        course_id: instance.course_id,
        owner_actor_id: instance.owner_id,
        environment_revision: instance.revision,
        eligibility_expires_at: instance.eligibility_expires_at,
        endpoints,
    })
}

/// Builds the internal route. A TLS acceptor must inject `VerifiedCallerIdentity`.
pub fn owner_resolver_router(resolver: OwnerResolver) -> Router {
    Router::new()
        .route(
            "/internal/v1/environments/{environment_id}/owner:resolve",
            post(resolve_owner),
        )
        .route(
            "/internal/v1/environments/{environment_id}/endpoint-eligibility:resolve",
            post(resolve_endpoint_eligibility),
        )
        .route(
            "/internal/v1/environments/{environment_id}/console-eligibility:resolve",
            post(resolve_console_eligibility),
        )
        .with_state(resolver)
}

async fn resolve_console_eligibility(
    State(resolver): State<OwnerResolver>,
    path: Result<Path<EnvironmentId>, PathRejection>,
    caller: Option<Extension<VerifiedCallerIdentity>>,
    body: Bytes,
) -> Result<Response, OwnerResolverError> {
    let Path(environment_id) = path.map_err(|_| OwnerResolverError::RequestInvalid)?;
    let request = contracts::parse_strict_json::<EnvironmentConsoleEligibilityRequest>(&body)
        .map_err(|_| OwnerResolverError::RequestInvalid)?;
    if request.environment_id != environment_id {
        return Err(OwnerResolverError::ScopeMismatch);
    }
    let Extension(caller) = caller.ok_or(OwnerResolverError::CallerUntrusted)?;
    let resolution = resolver
        .resolve_console_eligibility(&caller, &request)
        .await?;
    let etag = StrongEtag::from_revision(resolution.environment_revision).header_value();
    let mut response = Json(resolution).into_response();
    response.headers_mut().insert(
        header::ETAG,
        HeaderValue::from_str(&etag).map_err(|_| OwnerResolverError::ResponseInvalid)?,
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

async fn resolve_endpoint_eligibility(
    State(resolver): State<OwnerResolver>,
    path: Result<Path<EnvironmentId>, PathRejection>,
    caller: Option<Extension<VerifiedCallerIdentity>>,
    body: Bytes,
) -> Result<Response, OwnerResolverError> {
    let Path(environment_id) = path.map_err(|_| OwnerResolverError::RequestInvalid)?;
    let request = contracts::parse_strict_json::<EnvironmentEndpointEligibilityRequest>(&body)
        .map_err(|_| OwnerResolverError::RequestInvalid)?;
    if request.environment_id != environment_id {
        return Err(OwnerResolverError::ScopeMismatch);
    }
    let Extension(caller) = caller.ok_or(OwnerResolverError::CallerUntrusted)?;
    let resolution = resolver
        .resolve_endpoint_eligibility(&caller, &request)
        .await?;
    let etag = StrongEtag::from_revision(resolution.environment_revision).header_value();
    let mut response = Json(resolution).into_response();
    response.headers_mut().insert(
        header::ETAG,
        HeaderValue::from_str(&etag).map_err(|_| OwnerResolverError::ResponseInvalid)?,
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

async fn resolve_owner(
    State(resolver): State<OwnerResolver>,
    path: Result<Path<EnvironmentId>, PathRejection>,
    caller: Option<Extension<VerifiedCallerIdentity>>,
    body: Bytes,
) -> Result<Response, OwnerResolverError> {
    let Path(environment_id) = path.map_err(|_| OwnerResolverError::RequestInvalid)?;
    let request = contracts::parse_strict_json::<EnvironmentOwnerResolutionRequest>(&body)
        .map_err(|_| OwnerResolverError::RequestInvalid)?;
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
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

fn valid_san(san: &str) -> bool {
    if san.is_empty() || san.len() > 253 || san.contains('*') {
        return false;
    }
    if san.starts_with("spiffe://") {
        return url::Url::parse(san).is_ok_and(|uri| {
            uri.scheme() == "spiffe"
                && uri.host_str().is_some()
                && uri.username().is_empty()
                && uri.password().is_none()
                && uri.port().is_none()
                && uri.path() != "/"
                && uri.query().is_none()
                && uri.fragment().is_none()
        });
    }
    san.bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || b"-_.:".contains(&byte))
}

#[cfg(test)]
mod san_tests {
    use super::valid_san;

    #[test]
    fn accepts_exact_dns_and_spiffe_sans_but_rejects_ambiguous_uris() {
        assert!(valid_san("access-service.internal"));
        assert!(valid_san("spiffe://labweaver/access-service"));
        assert!(!valid_san("spiffe://labweaver/"));
        assert!(!valid_san("spiffe://user@labweaver/access-service"));
        assert!(!valid_san("spiffe://labweaver/access-*"));
        assert!(!valid_san("https://labweaver/access-service"));
    }
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
    #[error("LW_ENV_ENDPOINT_UNAVAILABLE")]
    EndpointUnavailable,
    #[error("LW_ENV_OWNER_RESOLVER_UNAVAILABLE")]
    Unavailable(EnvironmentStoreError),
    #[error("LW_ENV_CONSOLE_RELEASE_UNAVAILABLE")]
    ReleaseUnavailable(ReleaseProjectionError),
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
            Self::EndpointUnavailable => {
                (StatusCode::FORBIDDEN, "LW_ENV_ENDPOINT_UNAVAILABLE", false)
            }
            Self::Unavailable(_) => (
                StatusCode::SERVICE_UNAVAILABLE,
                "LW_ENV_OWNER_RESOLVER_UNAVAILABLE",
                true,
            ),
            Self::ReleaseUnavailable(_) => (
                StatusCode::SERVICE_UNAVAILABLE,
                "LW_ENV_CONSOLE_RELEASE_UNAVAILABLE",
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
