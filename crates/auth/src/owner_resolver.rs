//! Fail-closed JWT client for the Environment-authoritative owner resolver.

use std::{collections::BTreeSet, time::Duration};

use contracts::{
    UtcTimestamp,
    environment::{
        EnvironmentConsoleEligibility, EnvironmentConsoleEligibilityRequest,
        EnvironmentEndpointEligibility, EnvironmentEndpointEligibilityRequest,
        EnvironmentOwnerResolution, EnvironmentOwnerResolutionRequest,
        EnvironmentOwnerResolverClientConfig,
    },
    http::StrongEtag,
};
use reqwest::{Certificate, Client, StatusCode, Url, header};

use crate::{ServiceTokenClient, TransportSecurityMode};

/// Configured Environment owner resolver client.
#[derive(Clone)]
pub struct EnvironmentOwnerResolverClient {
    client: Client,
    service_token_client: ServiceTokenClient,
    service_token_audience: String,
    service_token_scopes: BTreeSet<String>,
    base_uri: Url,
    max_retries: u8,
    retry_backoff: Duration,
}

impl EnvironmentOwnerResolverClient {
    /// Builds a JWT-authenticated client from deployment-resolved trust material.
    ///
    /// The service token client is injected so token acquisition and caching use
    /// the same configured issuer, audience, and permission policy as the
    /// caller's other internal requests.  The resolver connection itself still
    /// uses the deployment CA for server-only TLS in strict mode.
    pub fn new(
        config: &EnvironmentOwnerResolverClientConfig,
        trusted_ca_pem: &[u8],
        service_token_client: ServiceTokenClient,
        service_token_audience: String,
        service_token_scopes: BTreeSet<String>,
        retry_backoff: Duration,
        transport_security: TransportSecurityMode,
    ) -> Result<Self, OwnerResolverClientError> {
        config
            .validate()
            .map_err(|_| OwnerResolverClientError::Configuration)?;
        if retry_backoff.is_zero() || retry_backoff > Duration::from_secs(5) {
            return Err(OwnerResolverClientError::Configuration);
        }
        if service_token_audience.trim().is_empty()
            || service_token_scopes.is_empty()
            || service_token_scopes.iter().any(|scope| {
                scope.trim().is_empty()
                    || scope != scope.trim()
                    || scope.len() > 128
                    || !scope.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':')
                    })
            })
        {
            return Err(OwnerResolverClientError::Configuration);
        }
        let base_uri = Url::parse(&config.resolver_uri)
            .map_err(|_| OwnerResolverClientError::Configuration)?;
        if !resolver_endpoint_allowed(&base_uri, transport_security) {
            return Err(OwnerResolverClientError::Configuration);
        }
        let roots = Certificate::from_pem_bundle(trusted_ca_pem)
            .map_err(|_| OwnerResolverClientError::Configuration)?;
        if roots.is_empty() {
            return Err(OwnerResolverClientError::Configuration);
        }
        let mut client_builder = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_millis(config.timeout_milliseconds));
        if transport_security == TransportSecurityMode::InsecureTestOnly {
            client_builder = client_builder.danger_accept_invalid_certs(true);
        } else {
            client_builder = client_builder
                .https_only(true)
                .tls_built_in_root_certs(false);
        }
        for root in roots {
            client_builder = client_builder.add_root_certificate(root);
        }
        let client = client_builder
            .build()
            .map_err(|_| OwnerResolverClientError::Configuration)?;
        Ok(Self {
            client,
            service_token_client,
            service_token_audience,
            service_token_scopes,
            base_uri,
            max_retries: config.max_retries,
            retry_backoff,
        })
    }

    /// Resolves ownership using the Environment authority and validates the
    /// response identity, revision, `ETag`, and expiry before returning it.
    pub async fn resolve(
        &self,
        request: &EnvironmentOwnerResolutionRequest,
        now: UtcTimestamp,
    ) -> Result<EnvironmentOwnerResolution, OwnerResolverClientError> {
        let started = std::time::Instant::now();
        let result = self.resolve_inner(request, now).await;
        let outcome = match &result {
            Ok(_) => "success",
            Err(OwnerResolverClientError::ScopeDenied) => "denied",
            Err(OwnerResolverClientError::Unavailable) => "unavailable",
            Err(OwnerResolverClientError::ResponseInvalid) => "invalid_response",
            Err(OwnerResolverClientError::Configuration) => "configuration",
        };
        metrics::counter!("labweaver_auth_owner_resolutions", "result" => outcome).increment(1);
        metrics::histogram!("labweaver_auth_owner_resolution_duration_seconds")
            .record(started.elapsed().as_secs_f64());
        result
    }

    /// Resolves the exact endpoint set required to activate one `AccessGrant`.
    pub async fn resolve_endpoint_eligibility(
        &self,
        request: &EnvironmentEndpointEligibilityRequest,
        now: UtcTimestamp,
    ) -> Result<EnvironmentEndpointEligibility, OwnerResolverClientError> {
        let mut endpoint = self.base_uri.clone();
        endpoint.set_path(&format!(
            "/internal/v1/environments/{}/endpoint-eligibility:resolve",
            request.environment_id
        ));
        for attempt in 0..=self.max_retries {
            let mut headers = header::HeaderMap::new();
            self.service_token_client
                .bearer_auth_for(
                    &mut headers,
                    &self.service_token_audience,
                    &self.service_token_scopes,
                )
                .await
                .map_err(|_| OwnerResolverClientError::Unavailable)?;
            match self
                .client
                .post(endpoint.clone())
                .headers(headers)
                .json(request)
                .send()
                .await
            {
                Ok(response) if response.status().is_success() => {
                    return validate_endpoint_response(response, request, now).await;
                }
                Ok(response)
                    if matches!(
                        response.status(),
                        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
                    ) =>
                {
                    return Err(OwnerResolverClientError::ScopeDenied);
                }
                Ok(response) if response.status() == StatusCode::SERVICE_UNAVAILABLE => {
                    if attempt == self.max_retries {
                        return Err(OwnerResolverClientError::Unavailable);
                    }
                }
                Ok(_) => return Err(OwnerResolverClientError::ResponseInvalid),
                Err(_) if attempt == self.max_retries => {
                    return Err(OwnerResolverClientError::Unavailable);
                }
                Err(_) => {}
            }
            let multiplier = 1_u32 << u32::from(attempt);
            let delay = self
                .retry_backoff
                .checked_mul(multiplier)
                .ok_or(OwnerResolverClientError::Configuration)?;
            tokio::time::sleep(delay).await;
        }
        Err(OwnerResolverClientError::Unavailable)
    }

    /// Resolves one exact browser-console admission snapshot.
    pub async fn resolve_console_eligibility(
        &self,
        request: &EnvironmentConsoleEligibilityRequest,
        now: UtcTimestamp,
    ) -> Result<EnvironmentConsoleEligibility, OwnerResolverClientError> {
        let mut endpoint = self.base_uri.clone();
        endpoint.set_path(&format!(
            "/internal/v1/environments/{}/console-eligibility:resolve",
            request.environment_id
        ));
        for attempt in 0..=self.max_retries {
            let mut headers = header::HeaderMap::new();
            self.service_token_client
                .bearer_auth_for(
                    &mut headers,
                    &self.service_token_audience,
                    &self.service_token_scopes,
                )
                .await
                .map_err(|_| OwnerResolverClientError::Unavailable)?;
            match self
                .client
                .post(endpoint.clone())
                .headers(headers)
                .json(request)
                .send()
                .await
            {
                Ok(response) if response.status().is_success() => {
                    return validate_console_response(response, request, now).await;
                }
                Ok(response)
                    if matches!(
                        response.status(),
                        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
                    ) =>
                {
                    return Err(OwnerResolverClientError::ScopeDenied);
                }
                Ok(response) if response.status() == StatusCode::SERVICE_UNAVAILABLE => {
                    if attempt == self.max_retries {
                        return Err(OwnerResolverClientError::Unavailable);
                    }
                }
                Ok(_) => return Err(OwnerResolverClientError::ResponseInvalid),
                Err(_) if attempt == self.max_retries => {
                    return Err(OwnerResolverClientError::Unavailable);
                }
                Err(_) => {}
            }
            let multiplier = 1_u32 << u32::from(attempt);
            let delay = self
                .retry_backoff
                .checked_mul(multiplier)
                .ok_or(OwnerResolverClientError::Configuration)?;
            tokio::time::sleep(delay).await;
        }
        Err(OwnerResolverClientError::Unavailable)
    }

    async fn resolve_inner(
        &self,
        request: &EnvironmentOwnerResolutionRequest,
        now: UtcTimestamp,
    ) -> Result<EnvironmentOwnerResolution, OwnerResolverClientError> {
        let mut endpoint = self.base_uri.clone();
        endpoint.set_path(&format!(
            "/internal/v1/environments/{}/owner:resolve",
            request.environment_id
        ));
        for attempt in 0..=self.max_retries {
            let mut headers = header::HeaderMap::new();
            self.service_token_client
                .bearer_auth_for(
                    &mut headers,
                    &self.service_token_audience,
                    &self.service_token_scopes,
                )
                .await
                .map_err(|_| OwnerResolverClientError::Unavailable)?;
            match self
                .client
                .post(endpoint.clone())
                .headers(headers)
                .json(request)
                .send()
                .await
            {
                Ok(response) if response.status().is_success() => {
                    return validate_response(response, request, now).await;
                }
                Ok(response)
                    if matches!(
                        response.status(),
                        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
                    ) =>
                {
                    return Err(OwnerResolverClientError::ScopeDenied);
                }
                Ok(response) if response.status() == StatusCode::SERVICE_UNAVAILABLE => {
                    if attempt == self.max_retries {
                        return Err(OwnerResolverClientError::Unavailable);
                    }
                }
                Ok(_) => return Err(OwnerResolverClientError::ResponseInvalid),
                Err(_) if attempt == self.max_retries => {
                    return Err(OwnerResolverClientError::Unavailable);
                }
                Err(_) => {}
            }
            let multiplier = 1_u32 << u32::from(attempt);
            let delay = self
                .retry_backoff
                .checked_mul(multiplier)
                .ok_or(OwnerResolverClientError::Configuration)?;
            tokio::time::sleep(delay).await;
        }
        Err(OwnerResolverClientError::Unavailable)
    }
}

fn resolver_endpoint_allowed(url: &Url, transport_security: TransportSecurityMode) -> bool {
    if transport_security == TransportSecurityMode::Strict {
        return url.scheme() == "https" && url.host_str().is_some();
    }
    matches!(url.scheme(), "http" | "https")
        && url.host_str().is_some_and(|host| {
            host.eq_ignore_ascii_case("localhost")
                || host
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|address| address.is_loopback())
        })
}

async fn validate_endpoint_response(
    response: reqwest::Response,
    request: &EnvironmentEndpointEligibilityRequest,
    now: UtcTimestamp,
) -> Result<EnvironmentEndpointEligibility, OwnerResolverClientError> {
    let etag = response
        .headers()
        .get(header::ETAG)
        .and_then(|value| value.to_str().ok())
        .ok_or(OwnerResolverClientError::ResponseInvalid)?
        .to_owned();
    let resolution = response
        .json::<EnvironmentEndpointEligibility>()
        .await
        .map_err(|_| OwnerResolverClientError::ResponseInvalid)?;
    let expected_etag = StrongEtag::from_revision(resolution.environment_revision).header_value();
    resolution
        .validate_for(request, now)
        .map_err(|_| OwnerResolverClientError::ResponseInvalid)?;
    if etag != expected_etag {
        return Err(OwnerResolverClientError::ResponseInvalid);
    }
    Ok(resolution)
}

async fn validate_console_response(
    response: reqwest::Response,
    request: &EnvironmentConsoleEligibilityRequest,
    now: UtcTimestamp,
) -> Result<EnvironmentConsoleEligibility, OwnerResolverClientError> {
    let etag = response
        .headers()
        .get(header::ETAG)
        .and_then(|value| value.to_str().ok())
        .ok_or(OwnerResolverClientError::ResponseInvalid)?
        .to_owned();
    let resolution = response
        .json::<EnvironmentConsoleEligibility>()
        .await
        .map_err(|_| OwnerResolverClientError::ResponseInvalid)?;
    let expected_etag = StrongEtag::from_revision(resolution.environment_revision).header_value();
    resolution
        .validate_for(request, now)
        .map_err(|_| OwnerResolverClientError::ResponseInvalid)?;
    if etag != expected_etag {
        return Err(OwnerResolverClientError::ResponseInvalid);
    }
    Ok(resolution)
}

async fn validate_response(
    response: reqwest::Response,
    request: &EnvironmentOwnerResolutionRequest,
    now: UtcTimestamp,
) -> Result<EnvironmentOwnerResolution, OwnerResolverClientError> {
    let etag = response
        .headers()
        .get(header::ETAG)
        .and_then(|value| value.to_str().ok())
        .ok_or(OwnerResolverClientError::ResponseInvalid)?
        .to_owned();
    let resolution = response
        .json::<EnvironmentOwnerResolution>()
        .await
        .map_err(|_| OwnerResolverClientError::ResponseInvalid)?;
    let expected_etag = StrongEtag::from_revision(resolution.environment_revision).header_value();
    if resolution.environment_id != request.environment_id
        || resolution.project_id != request.project_id
        || resolution.course_id != request.course_id
        || resolution.owner_actor_id != request.owner_actor_id
        || resolution.environment_revision != request.expected_revision
        || resolution.eligibility_expires_at <= now
        || etag != expected_etag
    {
        return Err(OwnerResolverClientError::ResponseInvalid);
    }
    Ok(resolution)
}

/// Owner resolver failures mapped by Access at the final HTTP boundary.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum OwnerResolverClientError {
    /// Deployment configuration violates the owner resolver contract.
    #[error("LW_AUTH_CONFIG_BINDING_MISSING")]
    Configuration,
    /// The Environment authority rejected course, owner, or revision binding.
    #[error("LW_AUTH_ENVIRONMENT_SCOPE_DENIED")]
    ScopeDenied,
    /// The resolver or its authoritative store was unavailable after bounded retries.
    #[error("LW_AUTH_OWNER_RESOLVER_UNAVAILABLE")]
    Unavailable,
    /// A successful resolver response did not match the signed request boundary.
    #[error("LW_AUTH_OWNER_RESPONSE_INVALID")]
    ResponseInvalid,
}
