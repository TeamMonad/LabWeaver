//! Service-to-service OAuth 2.0 client-credentials authentication.
//!
//! Browser sessions and internal service identities have different trust
//! boundaries.  This module keeps the internal boundary explicit: callers
//! obtain a short-lived bearer token for one configured audience, and servers
//! validate that token against the issuer's rotating JWKS before accepting
//! any request.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::{Duration, Instant},
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use http::{HeaderMap, HeaderValue, header::AUTHORIZATION};
use jsonwebtoken::Algorithm;
use jwt_authorizer::{Authorizer, JwtAuthorizer, Refresh, RefreshStrategy, Validation};
use openidconnect::{
    ClientId, ClientSecret, IssuerUrl, OAuth2TokenResponse,
    core::{CoreClient, CoreProviderMetadata, CoreTokenResponse, CoreTokenType},
};
use serde::Deserialize;
use time::OffsetDateTime;
use url::Url;

use crate::{TransportSecurityMode, no_redirect_http_client};

fn trace_service_auth_failure(
    operation: &'static str,
    failure_stage: &'static str,
    diagnostic_code: &'static str,
    retryable: bool,
) {
    tracing::warn!(
        event = "auth.service_token.failed",
        component = "service-auth",
        operation,
        outcome = "failed",
        failure_stage,
        diagnostic_code,
        retryable,
        safe_detail = "redacted",
    );
}

/// Configuration shared by an internal service JWT verifier.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServiceAuthConfig {
    /// Exact OIDC issuer used for discovery and `iss` validation.
    pub issuer: Url,
    /// Audience required in the access token.
    pub audience: String,
    /// OAuth client IDs allowed in the token's `azp` claim.
    pub allowed_client_ids: BTreeSet<String>,
    /// Permissions that every accepted token must carry.
    pub required_permissions: BTreeSet<String>,
    /// Explicit asymmetric signing algorithms accepted by the verifier.
    pub jwt_algorithms: BTreeSet<String>,
    /// JWKS refresh interval after a successful refresh.
    pub jwks_refresh_seconds: u64,
    /// Minimum delay before retrying a failed JWKS refresh.
    pub jwks_retry_seconds: u64,
    /// Transport policy used by OIDC discovery and JWKS requests.
    pub transport_security: TransportSecurityMode,
}

impl ServiceAuthConfig {
    /// Validates and constructs an internal verifier configuration.
    #[allow(clippy::too_many_arguments, reason = "mirrors deployment settings")]
    pub fn new(
        issuer: &str,
        audience: String,
        allowed_client_ids: BTreeSet<String>,
        required_permissions: BTreeSet<String>,
        jwt_algorithms: BTreeSet<String>,
        jwks_refresh_seconds: u64,
        jwks_retry_seconds: u64,
        transport_security: TransportSecurityMode,
    ) -> Result<Self, ServiceAuthConfigError> {
        let issuer = transport_url(issuer, transport_security)?;
        if audience.trim().is_empty() || allowed_client_ids.is_empty() {
            return Err(ServiceAuthConfigError::MissingBinding);
        }
        if allowed_client_ids
            .iter()
            .any(|client_id| !valid_binding(client_id))
        {
            return Err(ServiceAuthConfigError::MissingBinding);
        }
        if required_permissions
            .iter()
            .any(|permission| !valid_permission(permission))
        {
            return Err(ServiceAuthConfigError::InvalidPermission);
        }
        if jwt_algorithms.is_empty()
            || jwt_algorithms
                .iter()
                .any(|algorithm| parse_algorithm(algorithm).is_err())
        {
            return Err(ServiceAuthConfigError::AlgorithmRejected);
        }
        if jwks_refresh_seconds == 0 || jwks_retry_seconds == 0 {
            return Err(ServiceAuthConfigError::InvalidRefreshSettings);
        }
        Ok(Self {
            issuer,
            audience,
            allowed_client_ids,
            required_permissions,
            jwt_algorithms,
            jwks_refresh_seconds,
            jwks_retry_seconds,
            transport_security,
        })
    }
}

/// Configuration for an OAuth 2.0 client-credentials token client.
///
/// The client secret is intentionally excluded from the derived `Debug`
/// implementation.  Deployments should construct this value from a secret
/// binding and never from a checked-in configuration file.
#[derive(Clone, Eq, PartialEq)]
pub struct ServiceTokenClientConfig {
    /// Exact OIDC issuer used for token endpoint discovery.
    pub issuer: Url,
    /// OAuth client ID.
    pub client_id: String,
    client_secret: String,
    /// Default target audience requested from the authorization server.
    pub audience: String,
    /// Default permission roles that the caller expects in the target
    /// audience's `resource_access` claim.
    ///
    /// These values are an application authorization contract.  They are not
    /// OAuth `scope` parameters: Keycloak emits them from the intersection of
    /// the caller service account roles and its client role scope mappings.
    pub scopes: BTreeSet<String>,
    /// Refresh this many seconds before the token's expiry.
    pub refresh_skew_seconds: u64,
    /// Transport policy used by OIDC discovery and token requests.
    pub transport_security: TransportSecurityMode,
}

impl std::fmt::Debug for ServiceTokenClientConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ServiceTokenClientConfig")
            .field("issuer", &self.issuer)
            .field("client_id", &self.client_id)
            .field("client_secret", &"[REDACTED]")
            .field("audience", &self.audience)
            .field("scopes", &self.scopes)
            .field("refresh_skew_seconds", &self.refresh_skew_seconds)
            .field("transport_security", &self.transport_security)
            .finish()
    }
}

impl ServiceTokenClientConfig {
    /// Validates and constructs a token-client configuration.
    #[allow(clippy::too_many_arguments, reason = "mirrors deployment settings")]
    pub fn new(
        issuer: &str,
        client_id: String,
        client_secret: String,
        audience: String,
        scopes: BTreeSet<String>,
        refresh_skew_seconds: u64,
        transport_security: TransportSecurityMode,
    ) -> Result<Self, ServiceTokenClientError> {
        let issuer = transport_url(issuer, transport_security)
            .map_err(ServiceTokenClientError::InvalidConfig)?;
        if client_id.trim().is_empty()
            || client_secret.is_empty()
            || audience.trim().is_empty()
            || scopes.iter().any(|scope| !valid_permission(scope))
        {
            return Err(ServiceTokenClientError::InvalidConfig(
                ServiceAuthConfigError::MissingBinding,
            ));
        }
        if refresh_skew_seconds == 0 {
            return Err(ServiceTokenClientError::InvalidConfig(
                ServiceAuthConfigError::InvalidRefreshSettings,
            ));
        }
        Ok(Self {
            issuer,
            client_id,
            client_secret,
            audience,
            scopes,
            refresh_skew_seconds,
            transport_security,
        })
    }

    /// Returns a copy of the client secret for the token client constructor.
    ///
    /// This is kept as an explicit method so call sites visibly cross the
    /// deployment secret boundary instead of serializing the configuration.
    #[must_use]
    pub fn client_secret(&self) -> &str {
        &self.client_secret
    }
}

/// A cached token target.  The cache key contains both audience and expected
/// permission roles so a token minted for one downstream service can never be
/// reused for another.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct TokenCacheKey {
    audience: String,
    scopes: Vec<String>,
}

#[derive(Clone)]
struct CachedToken {
    value: String,
    expires_at: Instant,
}

/// OAuth 2.0 client-credentials provider with a short-lived, audience-scoped
/// in-memory token cache.
#[derive(Clone)]
pub struct ServiceTokenClient {
    metadata: CoreProviderMetadata,
    http: reqwest::Client,
    config: Arc<ServiceTokenClientConfig>,
    cache: Arc<tokio::sync::Mutex<BTreeMap<TokenCacheKey, CachedToken>>>,
}

impl ServiceTokenClient {
    /// Discovers the token endpoint using the supplied no-redirect HTTP client.
    pub async fn discover(
        config: ServiceTokenClientConfig,
        http: reqwest::Client,
    ) -> Result<Self, ServiceTokenClientError> {
        let issuer = IssuerUrl::new(config.issuer.to_string()).map_err(|_| {
            trace_service_auth_failure(
                "service_token_client.discover",
                "issuer_parse",
                "LW_AUTH_SERVICE_DISCOVERY_FAILED",
                false,
            );
            ServiceTokenClientError::Discovery
        })?;
        let metadata = CoreProviderMetadata::discover_async(issuer, &http)
            .await
            .map_err(|_| {
                trace_service_auth_failure(
                    "service_token_client.discover",
                    "oidc_discovery",
                    "LW_AUTH_SERVICE_DISCOVERY_FAILED",
                    true,
                );
                ServiceTokenClientError::Discovery
            })?;
        let token_endpoint = metadata.token_endpoint().ok_or_else(|| {
            trace_service_auth_failure(
                "service_token_client.discover",
                "token_endpoint_missing",
                "LW_AUTH_SERVICE_TOKEN_ENDPOINT_MISSING",
                false,
            );
            ServiceTokenClientError::TokenEndpoint
        })?;
        if !endpoint_transport_allowed(token_endpoint.url(), config.transport_security) {
            trace_service_auth_failure(
                "service_token_client.discover",
                "token_endpoint_transport",
                "LW_AUTH_SERVICE_ENDPOINT_TRANSPORT_REJECTED",
                false,
            );
            return Err(ServiceTokenClientError::EndpointTransport);
        }
        Ok(Self {
            metadata,
            http,
            config: Arc::new(config),
            cache: Arc::new(tokio::sync::Mutex::new(BTreeMap::new())),
        })
    }

    /// Builds a client after creating the controlled no-redirect HTTP client.
    pub async fn discover_with_trust(
        config: ServiceTokenClientConfig,
        trusted_ca_pem: Option<&[u8]>,
    ) -> Result<Self, ServiceTokenClientError> {
        let http =
            no_redirect_http_client(trusted_ca_pem, config.transport_security).map_err(|_| {
                trace_service_auth_failure(
                    "service_token_client.discover",
                    "http_client_build",
                    "LW_AUTH_SERVICE_HTTP_CLIENT_FAILED",
                    false,
                );
                ServiceTokenClientError::HttpClient
            })?;
        Self::discover(config, http).await
    }

    /// Returns an access token for the configured default audience and
    /// permission roles.
    pub async fn access_token(&self) -> Result<String, ServiceTokenClientError> {
        let scopes = self.config.scopes.clone();
        self.access_token_for(&self.config.audience, &scopes).await
    }

    /// Returns an access token for an explicitly permitted target audience and
    /// permission role set.
    ///
    /// Keycloak's standard client-credentials grant does not define an
    /// `audience` request parameter, and business permission names are client
    /// roles rather than registered OAuth client scopes.  The target and role
    /// set therefore remain local expectations used for cache partitioning and
    /// returned-token validation.  Keycloak is configured with explicit role
    /// scope mappings and its audience-resolve mapper, so the signed token
    /// carries the target audience only when the caller's service-account
    /// roles intersect that target scope.
    #[allow(
        clippy::too_many_lines,
        reason = "token exchange keeps validation and failure telemetry adjacent"
    )]
    pub async fn access_token_for(
        &self,
        audience: &str,
        scopes: &BTreeSet<String>,
    ) -> Result<String, ServiceTokenClientError> {
        if audience.trim().is_empty() || scopes.iter().any(|scope| !valid_permission(scope)) {
            trace_service_auth_failure(
                "service_token_client.access_token",
                "target_validation",
                "LW_AUTH_SERVICE_TARGET_INVALID",
                false,
            );
            return Err(ServiceTokenClientError::InvalidTarget);
        }
        let key = TokenCacheKey {
            audience: audience.to_owned(),
            scopes: scopes.iter().cloned().collect(),
        };
        let now = Instant::now();
        let mut cache = self.cache.lock().await;
        if let Some(cached) = cache.get(&key)
            && cached.expires_at > now
        {
            return Ok(cached.value.clone());
        }

        let client = CoreClient::from_provider_metadata(
            self.metadata.clone(),
            ClientId::new(self.config.client_id.clone()),
            Some(ClientSecret::new(self.config.client_secret.clone())),
        );
        let request = client.exchange_client_credentials().map_err(|_| {
            trace_service_auth_failure(
                "service_token_client.access_token",
                "token_request_build",
                "LW_AUTH_SERVICE_TOKEN_ENDPOINT_INVALID",
                false,
            );
            ServiceTokenClientError::TokenEndpointInvalid
        })?;
        let response: CoreTokenResponse =
            request.request_async(&self.http).await.map_err(|_| {
                trace_service_auth_failure(
                    "service_token_client.access_token",
                    "token_exchange",
                    "LW_AUTH_SERVICE_TOKEN_EXCHANGE_FAILED",
                    true,
                );
                ServiceTokenClientError::TokenExchange
            })?;
        if response.token_type() != &CoreTokenType::Bearer {
            trace_service_auth_failure(
                "service_token_client.access_token",
                "token_type_validation",
                "LW_AUTH_SERVICE_TOKEN_TYPE_REJECTED",
                false,
            );
            return Err(ServiceTokenClientError::TokenTypeRejected);
        }
        let expires_in = response.expires_in().ok_or_else(|| {
            trace_service_auth_failure(
                "service_token_client.access_token",
                "token_expiry_missing",
                "LW_AUTH_SERVICE_TOKEN_EXPIRY_MISSING",
                false,
            );
            ServiceTokenClientError::ExpiryMissing
        })?;
        let skew = Duration::from_secs(self.config.refresh_skew_seconds);
        if expires_in <= skew {
            trace_service_auth_failure(
                "service_token_client.access_token",
                "token_expiry_validation",
                "LW_AUTH_SERVICE_TOKEN_EXPIRY_TOO_SHORT",
                false,
            );
            return Err(ServiceTokenClientError::ExpiryTooShort);
        }
        let value = response.access_token().secret().to_owned();
        if value.is_empty() {
            trace_service_auth_failure(
                "service_token_client.access_token",
                "token_value_validation",
                "LW_AUTH_SERVICE_TOKEN_EXCHANGE_FAILED",
                false,
            );
            return Err(ServiceTokenClientError::TokenExchange);
        }
        if !token_has_audience(&value, audience) {
            trace_service_auth_failure(
                "service_token_client.access_token",
                "token_audience_validation",
                "LW_AUTH_SERVICE_TOKEN_AUDIENCE_REJECTED",
                false,
            );
            return Err(ServiceTokenClientError::TokenAudienceRejected);
        }
        let expires_at = now
            .checked_add(expires_in)
            .and_then(|expiry| expiry.checked_sub(skew))
            .ok_or(ServiceTokenClientError::ExpiryTooShort)?;
        cache.insert(
            key,
            CachedToken {
                value: value.clone(),
                expires_at,
            },
        );
        Ok(value)
    }

    /// Adds a bearer Authorization header using the configured target.
    pub async fn bearer_auth(
        &self,
        headers: &mut HeaderMap,
    ) -> Result<(), ServiceTokenClientError> {
        let token = self.access_token().await?;
        let mut value = HeaderValue::from_str(&format!("Bearer {token}")).map_err(|_| {
            trace_service_auth_failure(
                "service_token_client.bearer_auth",
                "authorization_header",
                "LW_AUTH_SERVICE_HEADER_INVALID",
                false,
            );
            ServiceTokenClientError::HeaderInvalid
        })?;
        value.set_sensitive(true);
        headers.insert(AUTHORIZATION, value);
        Ok(())
    }

    /// Adds a bearer Authorization header for an explicit target.
    pub async fn bearer_auth_for(
        &self,
        headers: &mut HeaderMap,
        audience: &str,
        scopes: &BTreeSet<String>,
    ) -> Result<(), ServiceTokenClientError> {
        let token = self.access_token_for(audience, scopes).await?;
        let mut value = HeaderValue::from_str(&format!("Bearer {token}")).map_err(|_| {
            trace_service_auth_failure(
                "service_token_client.bearer_auth",
                "authorization_header",
                "LW_AUTH_SERVICE_HEADER_INVALID",
                false,
            );
            ServiceTokenClientError::HeaderInvalid
        })?;
        value.set_sensitive(true);
        headers.insert(AUTHORIZATION, value);
        Ok(())
    }
}

/// Claims retained after a service JWT has passed signature and registered
/// claim validation.  Provider-specific permission claims remain in `claims`.
#[derive(Clone, Debug, Deserialize)]
pub struct ServiceTokenClaims {
    /// Issuer-local service-account subject.
    pub sub: String,
    /// Required hard expiry as Unix seconds.
    pub exp: i64,
    /// Authorized party bound to the configured OAuth client.
    pub azp: Option<String>,
    /// Provider-specific claims used for permission extraction.
    #[serde(flatten)]
    pub claims: serde_json::Map<String, serde_json::Value>,
}

impl ServiceTokenClaims {
    fn permissions(&self, audience: &str) -> Result<BTreeSet<String>, ClaimsError> {
        let resources = self
            .claims
            .get("resource_access")
            .and_then(serde_json::Value::as_object)
            .ok_or(ClaimsError::PermissionSourceInvalid)?;
        let resource = resources
            .get(audience)
            .and_then(serde_json::Value::as_object)
            .ok_or(ClaimsError::PermissionSourceInvalid)?;
        let roles = resource
            .get("roles")
            .and_then(serde_json::Value::as_array)
            .ok_or(ClaimsError::PermissionSourceInvalid)?;
        let mut permissions = BTreeSet::new();
        for role in roles {
            let role = role
                .as_str()
                .filter(|value| valid_permission(value))
                .ok_or(ClaimsError::PermissionSourceInvalid)?;
            permissions.insert(role.to_owned());
        }
        Ok(permissions)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ClaimsError {
    PermissionSourceInvalid,
}

/// Verified, request-independent service identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServiceIdentity {
    /// Configured issuer which validated the token.
    pub issuer: String,
    /// Issuer-local service-account subject.
    pub subject: String,
    /// OAuth client ID from the verified `azp` claim.
    pub client_id: String,
    /// Absolute JWT expiry.
    pub expires_at: OffsetDateTime,
    /// Provider roles/scopes/permissions carried by the token.
    pub permissions: BTreeSet<String>,
}

impl ServiceIdentity {
    /// Returns whether the service token carries a permission.
    #[must_use]
    pub fn allows(&self, permission: &str) -> bool {
        self.permissions.contains(permission)
    }

    /// Requires a permission from the verified service token.
    pub fn require(&self, permission: &str) -> Result<(), ServiceAuthError> {
        if self.allows(permission) {
            Ok(())
        } else {
            Err(ServiceAuthError::PermissionDenied)
        }
    }
}

/// JWKS-backed verifier for service-to-service bearer tokens.
#[derive(Clone)]
pub struct ServiceTokenVerifier {
    authorizer: Arc<Authorizer<ServiceTokenClaims>>,
    config: Arc<ServiceAuthConfig>,
}

impl ServiceTokenVerifier {
    /// Creates a verifier using OIDC discovery and the configured JWKS policy.
    pub async fn discover(
        config: ServiceAuthConfig,
        http: reqwest::Client,
    ) -> Result<Self, ServiceAuthError> {
        let algorithms = config
            .jwt_algorithms
            .iter()
            .map(|value| parse_algorithm(value))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| {
                trace_service_auth_failure(
                    "service_token_verifier.discover",
                    "algorithm_validation",
                    "LW_AUTH_SERVICE_CONFIG_INVALID",
                    false,
                );
                ServiceAuthError::InvalidConfig
            })?;
        let issuer = IssuerUrl::new(config.issuer.to_string()).map_err(|_| {
            trace_service_auth_failure(
                "service_token_verifier.discover",
                "issuer_parse",
                "LW_AUTH_SERVICE_CONFIG_INVALID",
                false,
            );
            ServiceAuthError::InvalidConfig
        })?;
        let metadata = CoreProviderMetadata::discover_async(issuer, &http)
            .await
            .map_err(|_| {
                trace_service_auth_failure(
                    "service_token_verifier.discover",
                    "oidc_discovery",
                    "LW_AUTH_SERVICE_JWKS_UNAVAILABLE",
                    true,
                );
                ServiceAuthError::JwksUnavailable
            })?;
        if metadata.issuer().as_str() != config.issuer.as_str()
            || !endpoint_transport_allowed(metadata.jwks_uri().url(), config.transport_security)
        {
            trace_service_auth_failure(
                "service_token_verifier.discover",
                "jwks_endpoint_validation",
                "LW_AUTH_SERVICE_ENDPOINT_TRANSPORT_REJECTED",
                false,
            );
            return Err(ServiceAuthError::EndpointTransport);
        }
        let required_permissions = config.required_permissions.clone();
        let allowed_client_ids = config.allowed_client_ids.clone();
        let audience = config.audience.clone();
        let jwks_url = metadata.jwks_uri().url().to_string();
        let authorizer = JwtAuthorizer::from_jwks_url(&jwks_url)
            .http_client(http)
            .validation(
                Validation::new()
                    .iss(&[config.issuer.as_str()])
                    .aud(&[config.audience.as_str()])
                    .nbf(true)
                    .leeway(0)
                    .algs(algorithms),
            )
            .check(move |claims: &ServiceTokenClaims| {
                !claims.sub.is_empty()
                    && claims
                        .azp
                        .as_deref()
                        .is_some_and(|client_id| allowed_client_ids.contains(client_id))
                    && claims
                        .permissions(&audience)
                        .is_ok_and(|permissions| required_permissions.is_subset(&permissions))
            })
            .refresh(Refresh {
                strategy: RefreshStrategy::KeyNotFound,
                refresh_interval: Duration::from_secs(config.jwks_refresh_seconds),
                retry_interval: Duration::from_secs(config.jwks_retry_seconds),
            })
            .build()
            .await
            .map_err(|_| {
                trace_service_auth_failure(
                    "service_token_verifier.discover",
                    "jwks_load",
                    "LW_AUTH_SERVICE_JWKS_UNAVAILABLE",
                    true,
                );
                ServiceAuthError::JwksUnavailable
            })?;
        Ok(Self {
            authorizer: Arc::new(authorizer),
            config: Arc::new(config),
        })
    }

    /// Creates a verifier with a controlled no-redirect trust client.
    pub async fn discover_with_trust(
        config: ServiceAuthConfig,
        trusted_ca_pem: Option<&[u8]>,
    ) -> Result<Self, ServiceAuthError> {
        let http =
            no_redirect_http_client(trusted_ca_pem, config.transport_security).map_err(|_| {
                trace_service_auth_failure(
                    "service_token_verifier.discover",
                    "http_client_build",
                    "LW_AUTH_SERVICE_HTTP_CLIENT_FAILED",
                    false,
                );
                ServiceAuthError::HttpClient
            })?;
        Self::discover(config, http).await
    }

    /// Authenticates the Authorization header and returns an independent
    /// service identity.  Browser session state is deliberately not consulted.
    pub async fn authenticate(
        &self,
        headers: &HeaderMap,
    ) -> Result<ServiceIdentity, ServiceAuthError> {
        let token = self.authorizer.extract_token(headers).ok_or_else(|| {
            trace_service_auth_failure(
                "service_token_verifier.authenticate",
                "credentials_extract",
                "LW_AUTH_SERVICE_CREDENTIALS_MISSING",
                false,
            );
            ServiceAuthError::CredentialsMissing
        })?;
        let claims = self
            .authorizer
            .check_auth(&token)
            .await
            .map_err(|_| {
                trace_service_auth_failure(
                    "service_token_verifier.authenticate",
                    "token_validation",
                    "LW_AUTH_SERVICE_TOKEN_REJECTED",
                    false,
                );
                ServiceAuthError::TokenRejected
            })?
            .claims;
        let expires_at = OffsetDateTime::from_unix_timestamp(claims.exp).map_err(|_| {
            trace_service_auth_failure(
                "service_token_verifier.authenticate",
                "token_expiry_parse",
                "LW_AUTH_SERVICE_TOKEN_REJECTED",
                false,
            );
            ServiceAuthError::TokenRejected
        })?;
        if expires_at <= OffsetDateTime::now_utc() {
            trace_service_auth_failure(
                "service_token_verifier.authenticate",
                "token_expiry_validation",
                "LW_AUTH_SERVICE_TOKEN_EXPIRED",
                false,
            );
            return Err(ServiceAuthError::TokenExpired);
        }
        let client_id = claims.azp.clone().ok_or_else(|| {
            trace_service_auth_failure(
                "service_token_verifier.authenticate",
                "client_binding",
                "LW_AUTH_SERVICE_TOKEN_REJECTED",
                false,
            );
            ServiceAuthError::TokenRejected
        })?;
        if !self.config.allowed_client_ids.contains(&client_id) {
            trace_service_auth_failure(
                "service_token_verifier.authenticate",
                "client_binding",
                "LW_AUTH_SERVICE_TOKEN_REJECTED",
                false,
            );
            return Err(ServiceAuthError::TokenRejected);
        }
        let permissions = claims.permissions(&self.config.audience).map_err(|_| {
            trace_service_auth_failure(
                "service_token_verifier.authenticate",
                "permission_claims",
                "LW_AUTH_SERVICE_TOKEN_REJECTED",
                false,
            );
            ServiceAuthError::TokenRejected
        })?;
        Ok(ServiceIdentity {
            issuer: self.config.issuer.to_string(),
            subject: claims.sub.clone(),
            client_id,
            expires_at,
            permissions,
        })
    }

    /// Authenticates the header and requires one route permission.
    pub async fn authenticate_with_permission(
        &self,
        headers: &HeaderMap,
        permission: &str,
    ) -> Result<ServiceIdentity, ServiceAuthError> {
        let identity = self.authenticate(headers).await?;
        identity.require(permission)?;
        Ok(identity)
    }
}

/// Errors raised while validating service-auth configuration.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ServiceAuthConfigError {
    /// Issuer URL violates the configured transport policy.
    #[error("LW_AUTH_CONFIG_URL_INVALID")]
    InvalidIssuer,
    /// Required issuer, audience, or client binding is absent.
    #[error("LW_AUTH_CONFIG_BINDING_MISSING")]
    MissingBinding,
    /// A permission or scope contains an unsafe value.
    #[error("LW_AUTH_CONFIG_PERMISSION_INVALID")]
    InvalidPermission,
    /// A symmetric, unsigned, or unknown signing algorithm was configured.
    #[error("LW_AUTH_CONFIG_ALGORITHM_INVALID")]
    AlgorithmRejected,
    /// JWKS refresh settings must be positive.
    #[error("LW_AUTH_CONFIG_REFRESH_INVALID")]
    InvalidRefreshSettings,
}

/// Errors raised while obtaining or attaching a service token.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ServiceTokenClientError {
    /// Client configuration is invalid.
    #[error(transparent)]
    InvalidConfig(ServiceAuthConfigError),
    /// Controlled HTTP client construction failed.
    #[error("LW_AUTH_SERVICE_HTTP_CLIENT_FAILED")]
    HttpClient,
    /// OIDC discovery failed.
    #[error("LW_AUTH_SERVICE_DISCOVERY_FAILED")]
    Discovery,
    /// Discovery did not contain a token endpoint.
    #[error("LW_AUTH_SERVICE_TOKEN_ENDPOINT_MISSING")]
    TokenEndpoint,
    /// Discovered endpoint violated the transport policy.
    #[error("LW_AUTH_SERVICE_ENDPOINT_TRANSPORT_REJECTED")]
    EndpointTransport,
    /// The requested audience or scope set is invalid.
    #[error("LW_AUTH_SERVICE_TARGET_INVALID")]
    InvalidTarget,
    /// The token endpoint could not be configured.
    #[error("LW_AUTH_SERVICE_TOKEN_ENDPOINT_INVALID")]
    TokenEndpointInvalid,
    /// The authorization server rejected the client-credentials exchange.
    #[error("LW_AUTH_SERVICE_TOKEN_EXCHANGE_FAILED")]
    TokenExchange,
    /// The authorization server returned a non-bearer token.
    #[error("LW_AUTH_SERVICE_TOKEN_TYPE_REJECTED")]
    TokenTypeRejected,
    /// The authorization server returned a token for another audience.
    #[error("LW_AUTH_SERVICE_TOKEN_AUDIENCE_REJECTED")]
    TokenAudienceRejected,
    /// The authorization server omitted `expires_in`.
    #[error("LW_AUTH_SERVICE_TOKEN_EXPIRY_MISSING")]
    ExpiryMissing,
    /// The returned token would expire before its refresh safety window.
    #[error("LW_AUTH_SERVICE_TOKEN_EXPIRY_TOO_SHORT")]
    ExpiryTooShort,
    /// The returned access token could not be attached to an HTTP header.
    #[error("LW_AUTH_SERVICE_HEADER_INVALID")]
    HeaderInvalid,
}

/// Errors raised while validating an inbound service request.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ServiceAuthError {
    /// Verifier configuration is invalid.
    #[error("LW_AUTH_SERVICE_CONFIG_INVALID")]
    InvalidConfig,
    /// OIDC discovery or JWKS loading failed.
    #[error("LW_AUTH_SERVICE_JWKS_UNAVAILABLE")]
    JwksUnavailable,
    /// The discovered JWKS endpoint violates transport policy.
    #[error("LW_AUTH_SERVICE_ENDPOINT_TRANSPORT_REJECTED")]
    EndpointTransport,
    /// Authorization credentials were absent or malformed.
    #[error("LW_AUTH_SERVICE_CREDENTIALS_MISSING")]
    CredentialsMissing,
    /// Signature, issuer, audience, expiry, algorithm, client, or claims were rejected.
    #[error("LW_AUTH_SERVICE_TOKEN_REJECTED")]
    TokenRejected,
    /// The token expired during final local validation.
    #[error("LW_AUTH_SERVICE_TOKEN_EXPIRED")]
    TokenExpired,
    /// The token did not carry a required permission.
    #[error("LW_AUTH_SERVICE_PERMISSION_DENIED")]
    PermissionDenied,
    /// Controlled HTTP client construction failed.
    #[error("LW_AUTH_SERVICE_HTTP_CLIENT_FAILED")]
    HttpClient,
}

fn valid_permission(value: &str) -> bool {
    !value.trim().is_empty()
        && value == value.trim()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
}

fn valid_binding(value: &str) -> bool {
    !value.trim().is_empty()
        && value == value.trim()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn token_has_audience(token: &str, expected: &str) -> bool {
    let mut segments = token.split('.');
    let (Some(header), Some(payload), Some(signature), None) = (
        segments.next(),
        segments.next(),
        segments.next(),
        segments.next(),
    ) else {
        return false;
    };
    if header.is_empty() || payload.is_empty() || signature.is_empty() {
        return false;
    }
    let Ok(payload) = URL_SAFE_NO_PAD.decode(payload) else {
        return false;
    };
    let Ok(claims) = serde_json::from_slice::<serde_json::Value>(&payload) else {
        return false;
    };
    match claims.get("aud") {
        Some(serde_json::Value::String(audience)) => audience == expected,
        Some(serde_json::Value::Array(audiences)) => audiences
            .iter()
            .any(|audience| audience.as_str() == Some(expected)),
        _ => false,
    }
}

fn parse_algorithm(value: &str) -> Result<Algorithm, ServiceAuthConfigError> {
    match value {
        "RS256" => Ok(Algorithm::RS256),
        "RS384" => Ok(Algorithm::RS384),
        "RS512" => Ok(Algorithm::RS512),
        "PS256" => Ok(Algorithm::PS256),
        "PS384" => Ok(Algorithm::PS384),
        "PS512" => Ok(Algorithm::PS512),
        "ES256" => Ok(Algorithm::ES256),
        "ES384" => Ok(Algorithm::ES384),
        "EdDSA" => Ok(Algorithm::EdDSA),
        _ => Err(ServiceAuthConfigError::AlgorithmRejected),
    }
}

fn transport_url(value: &str, mode: TransportSecurityMode) -> Result<Url, ServiceAuthConfigError> {
    let url = Url::parse(value).map_err(|_| ServiceAuthConfigError::InvalidIssuer)?;
    if url.query().is_some() || url.fragment().is_some() || url.host_str().is_none() {
        return Err(ServiceAuthConfigError::InvalidIssuer);
    }
    if endpoint_transport_allowed(&url, mode) {
        Ok(url)
    } else {
        Err(ServiceAuthConfigError::InvalidIssuer)
    }
}

fn endpoint_transport_allowed(url: &Url, mode: TransportSecurityMode) -> bool {
    if mode == TransportSecurityMode::Strict {
        return url.scheme() == "https";
    }
    matches!(url.scheme(), "http" | "https")
        && url.host_str().is_some_and(|host| {
            host.eq_ignore_ascii_case("localhost")
                || host
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|address| address.is_loopback())
        })
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::{
        ServiceAuthConfig, ServiceIdentity, ServiceTokenClaims, ServiceTokenClientConfig,
        TransportSecurityMode,
    };
    use std::collections::BTreeSet;
    use time::OffsetDateTime;

    #[test]
    fn service_config_requires_secure_issuer_and_asymmetric_algorithm() {
        let permissions = BTreeSet::from(["access.read".to_owned()]);
        let valid = ServiceAuthConfig::new(
            "https://issuer.example.test/realms/labweaver",
            "labweaver-access".to_owned(),
            BTreeSet::from(["access-gateway".to_owned()]),
            permissions.clone(),
            BTreeSet::from(["RS256".to_owned()]),
            600,
            10,
            TransportSecurityMode::Strict,
        );
        assert!(valid.is_ok());
        assert!(
            ServiceAuthConfig::new(
                "http://issuer.example.test/realms/labweaver",
                "labweaver-access".to_owned(),
                BTreeSet::from(["access-gateway".to_owned()]),
                permissions.clone(),
                BTreeSet::from(["HS256".to_owned()]),
                600,
                10,
                TransportSecurityMode::Strict,
            )
            .is_err()
        );
        assert!(
            ServiceAuthConfig::new(
                "http://127.0.0.1:8081/realms/labweaver",
                "labweaver-access".to_owned(),
                BTreeSet::from(["access-gateway".to_owned()]),
                permissions,
                BTreeSet::from(["RS256".to_owned()]),
                600,
                10,
                TransportSecurityMode::InsecureTestOnly,
            )
            .is_ok()
        );
    }

    #[test]
    fn client_debug_redacts_secret() {
        let config = ServiceTokenClientConfig::new(
            "https://issuer.example.test/realms/labweaver",
            "gateway".to_owned(),
            "do-not-log".to_owned(),
            "access".to_owned(),
            BTreeSet::from(["access.read".to_owned()]),
            30,
            TransportSecurityMode::Strict,
        )
        .expect("valid test configuration");
        let debug = format!("{config:?}");
        assert!(!debug.contains("do-not-log"));
        assert!(debug.contains("REDACTED"));
    }

    #[test]
    fn keycloak_permission_claims_are_normalized() {
        let claims: ServiceTokenClaims = serde_json::from_value(serde_json::json!({
            "sub": "service-subject",
            "exp": 2_000_000_000,
            "azp": "gateway",
            "scope": "openid access.read",
            "realm_access": {"roles": ["ignored"]},
            "resource_access": {"labweaver-access": {"roles": ["access.read", "access.role"]}}
        }))
        .expect("valid claims");
        let permissions = claims.permissions("labweaver-access").expect("permissions");
        let identity = ServiceIdentity {
            issuer: "https://issuer.example.test".to_owned(),
            subject: claims.sub,
            client_id: claims.azp.clone().expect("azp"),
            expires_at: OffsetDateTime::from_unix_timestamp(claims.exp).expect("expiry"),
            permissions,
        };
        assert!(identity.allows("access.read"));
        assert!(identity.allows("access.role"));
        assert!(!identity.allows("ignored"));
        assert!(identity.require("access.missing").is_err());
    }
}
