//! Fail-closed mTLS client for the Environment-authoritative owner resolver.

use std::time::Duration;

use contracts::{
    UtcTimestamp,
    environment::{
        EnvironmentOwnerResolution, EnvironmentOwnerResolutionRequest,
        EnvironmentOwnerResolverClientConfig,
    },
    http::StrongEtag,
};
use reqwest::{Certificate, Client, Identity, StatusCode, Url, header};

use crate::TransportSecurityMode;

/// Configured Environment owner resolver client.
#[derive(Clone)]
pub struct EnvironmentOwnerResolverClient {
    client: Client,
    base_uri: Url,
    max_retries: u8,
    retry_backoff: Duration,
}

impl EnvironmentOwnerResolverClient {
    /// Builds an mTLS client from deployment-resolved certificate material.
    pub fn new(
        config: &EnvironmentOwnerResolverClientConfig,
        ca_certificate_pem: &[u8],
        client_certificate_pem: &[u8],
        client_private_key_pem: &[u8],
        retry_backoff: Duration,
        transport_security: TransportSecurityMode,
    ) -> Result<Self, OwnerResolverClientError> {
        config
            .validate()
            .map_err(|_| OwnerResolverClientError::Configuration)?;
        if retry_backoff.is_zero() || retry_backoff > Duration::from_secs(5) {
            return Err(OwnerResolverClientError::Configuration);
        }
        let base_uri = Url::parse(&config.resolver_uri)
            .map_err(|_| OwnerResolverClientError::Configuration)?;
        let server_name = base_uri
            .host_str()
            .ok_or(OwnerResolverClientError::Configuration)?;
        if !config
            .allowed_server_sans
            .iter()
            .any(|allowed| allowed == server_name)
        {
            return Err(OwnerResolverClientError::Configuration);
        }
        if transport_security == TransportSecurityMode::InsecureTestOnly
            && !server_name.eq_ignore_ascii_case("localhost")
            && !server_name
                .parse::<std::net::IpAddr>()
                .is_ok_and(|address| address.is_loopback())
        {
            return Err(OwnerResolverClientError::Configuration);
        }

        let roots = Certificate::from_pem_bundle(ca_certificate_pem)
            .map_err(|_| OwnerResolverClientError::CertificateMaterial)?;
        if roots.is_empty() {
            return Err(OwnerResolverClientError::CertificateMaterial);
        }
        let mut identity_pem =
            Vec::with_capacity(client_certificate_pem.len() + client_private_key_pem.len() + 1);
        identity_pem.extend_from_slice(client_certificate_pem);
        identity_pem.push(b'\n');
        identity_pem.extend_from_slice(client_private_key_pem);
        let identity = Identity::from_pem(&identity_pem)
            .map_err(|_| OwnerResolverClientError::CertificateMaterial)?;

        let mut builder = Client::builder()
            .https_only(true)
            .tls_built_in_root_certs(false)
            .redirect(reqwest::redirect::Policy::none())
            .identity(identity)
            .timeout(Duration::from_millis(config.timeout_milliseconds));
        if transport_security == TransportSecurityMode::InsecureTestOnly {
            builder = builder.danger_accept_invalid_certs(true);
        }
        for root in roots {
            builder = builder.add_root_certificate(root);
        }
        let client = builder
            .build()
            .map_err(|_| OwnerResolverClientError::CertificateMaterial)?;
        Ok(Self {
            client,
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
            Err(OwnerResolverClientError::CertificateMaterial) => "certificate",
        };
        metrics::counter!("labweaver_auth_owner_resolutions", "result" => outcome).increment(1);
        metrics::histogram!("labweaver_auth_owner_resolution_duration_seconds")
            .record(started.elapsed().as_secs_f64());
        result
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
            match self
                .client
                .post(endpoint.clone())
                .json(request)
                .send()
                .await
            {
                Ok(response) if response.status().is_success() => {
                    return validate_response(response, request, now).await;
                }
                Ok(response) if response.status() == StatusCode::FORBIDDEN => {
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
    /// A configured CA, client certificate, or private key was malformed.
    #[error("LW_AUTH_CONFIG_BINDING_MISSING")]
    CertificateMaterial,
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
