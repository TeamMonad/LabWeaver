use std::{
    collections::{BTreeMap, BTreeSet},
    str::FromStr,
};

use serde::Deserialize;
use url::Url;

use contracts::environment::EnvironmentOwnerResolverClientConfig;

/// TLS policy for auth authority clients.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum TransportSecurityMode {
    /// Require HTTPS and use either the system roots or an exclusive configured CA bundle.
    #[default]
    Strict,
    /// Permit loopback-only HTTP and invalid server certificates for disposable tests.
    InsecureTestOnly,
}

/// Explicit runtime configuration for the Keycloak relying party.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthConfig {
    /// Exact OIDC issuer URL discovered at startup.
    pub issuer: Url,
    /// Client identifier expected by Keycloak and token validation.
    pub client_id: String,
    /// Exact browser callback URL registered at Keycloak.
    pub redirect_uri: Url,
    /// Exact post-logout browser destination registered at Keycloak.
    pub post_logout_redirect_uri: Url,
    /// Required bearer-token audience.
    pub audience: String,
    /// Origins allowed to submit cookie-authenticated mutations.
    pub allowed_origins: BTreeSet<String>,
    /// Absolute BFF session lifetime in seconds.
    pub session_ttl_seconds: u64,
    /// Transport policy applied to Discovery and every discovered endpoint.
    pub transport_security: TransportSecurityMode,
}

/// Non-secret deployment configuration loaded from the reviewed YAML contract.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AccessAuthFile {
    /// TLS policy. `insecure-test-only` also requires an explicit process-level acknowledgement.
    #[serde(default)]
    pub transport_security: TransportSecurityMode,
    /// OIDC provider configuration.
    pub oidc: OidcFileConfig,
    /// Browser BFF configuration.
    pub browser: BrowserFileConfig,
    /// Internal mTLS listener configuration.
    pub internal_mtls: MtlsFileConfig,
    /// Environment-authoritative owner resolver client configuration.
    pub environment_owner_resolver: OwnerResolverFileConfig,
    /// Authenticated browser gateway for the Control public API.
    pub control_gateway: ControlGatewayFileConfig,
    /// Authenticated browser gateway for the Environment public API.
    pub environment_gateway: ControlGatewayFileConfig,
    /// Authenticated browser gateway for the freeze-only Evaluation API.
    pub evaluation_gateway: ControlGatewayFileConfig,
    /// `AccessGrant`, worker, and one-time authorization limits.
    pub grants: GrantRuntimeFileConfig,
    /// Mandatory mTLS `JetStream` connection.
    pub nats: NatsFileConfig,
    /// Secret-file locators for the Access runtime.
    pub secrets: SecretFileConfig,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[allow(
    missing_docs,
    reason = "YAML keys are documented by deploy/config/access-auth.yaml.example"
)]
#[serde(deny_unknown_fields)]
pub struct GrantRuntimeFileConfig {
    pub gateway_san_uris: Vec<String>,
    pub public_ssh_gateway_hostname: String,
    pub public_ssh_gateway_port: u16,
    pub public_ssh_gateway_host_key_fingerprint: String,
    pub default_ttl_seconds: u64,
    pub max_ttl_seconds: u64,
    pub authorization_token_ttl_seconds: u64,
    pub activation_poll_seconds: u64,
    pub activation_retry_seconds: u64,
    pub activation_max_attempts: u16,
    pub expiry_poll_seconds: u64,
    pub worker_lease_seconds: u64,
    pub max_keys_per_actor: u16,
    pub max_endpoints_per_grant: u16,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[allow(
    missing_docs,
    reason = "YAML keys are documented by deploy/config/access-auth.yaml.example"
)]
#[serde(deny_unknown_fields)]
pub struct NatsFileConfig {
    pub server: String,
    pub ca_certificate_file: String,
    pub client_certificate_file: String,
    pub client_private_key_file: String,
    pub credentials_file: String,
}

/// OIDC fields that vary by deployment.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[allow(
    missing_docs,
    reason = "YAML keys are documented by deploy/config/access-auth.yaml.example"
)]
#[serde(deny_unknown_fields)]
pub struct OidcFileConfig {
    pub issuer: String,
    pub client_id: String,
    pub audience: String,
    pub redirect_uri: String,
    pub post_logout_redirect_uri: String,
    pub trusted_ca_file: String,
    pub role_claim_path: Vec<String>,
    pub role_mappings: BTreeMap<String, String>,
    pub jwt_algorithms: BTreeSet<String>,
    pub jwks_refresh_seconds: u64,
    pub jwks_retry_seconds: u64,
}

/// Browser values that must not be compiled into a service image.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[allow(
    missing_docs,
    reason = "YAML keys are documented by deploy/config/access-auth.yaml.example"
)]
#[serde(deny_unknown_fields)]
pub struct BrowserFileConfig {
    pub bind_addr: String,
    pub allowed_origins: BTreeSet<String>,
    pub session_ttl_seconds: u64,
    pub session_idle_ttl_seconds: u64,
    pub oidc_transaction_ttl_seconds: u64,
    pub runtime_pool_max_connections: u32,
    pub cleanup_interval_seconds: u64,
    pub session_retention_seconds: u64,
    pub session_cookie_name: String,
    pub csrf_header_name: String,
}

/// Internal mTLS values supplied by deployment configuration.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[allow(
    missing_docs,
    reason = "YAML keys are documented by deploy/config/access-auth.yaml.example"
)]
#[serde(deny_unknown_fields)]
pub struct MtlsFileConfig {
    pub bind_addr: String,
    pub server_certificate_file: String,
    pub server_key_file: String,
    pub client_ca_file: String,
    pub allowed_san_uris: BTreeSet<String>,
    pub required_eku: String,
}

/// Secret locators which are resolved only by the deployment runtime.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[allow(
    missing_docs,
    reason = "YAML keys are documented by deploy/config/access-auth.yaml.example"
)]
#[serde(deny_unknown_fields)]
pub struct SecretFileConfig {
    pub oidc_client_secret_file: String,
    pub session_keyring_file: String,
    pub active_session_key_id: String,
    pub access_runtime_url_file: String,
    pub file_bindings: BTreeMap<String, String>,
}

/// Deployment-variable owner resolver values. Public semantics are validated
/// through [`EnvironmentOwnerResolverClientConfig`].
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[allow(
    missing_docs,
    reason = "YAML keys are documented by deploy/config/access-auth.yaml.example"
)]
#[serde(deny_unknown_fields)]
pub struct OwnerResolverFileConfig {
    pub resolver_uri: String,
    pub ca_certificate_locator: String,
    pub client_certificate_locator: String,
    pub client_private_key_locator: String,
    pub allowed_server_sans: Vec<String>,
    pub timeout_milliseconds: u64,
    pub max_retries: u8,
    pub retry_backoff_milliseconds: u64,
    pub decision_ttl_seconds: u64,
}

/// Fail-closed mTLS forwarding boundary from the browser BFF to Control.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[allow(
    missing_docs,
    reason = "YAML keys are documented by deploy/config/access-auth.yaml.example"
)]
#[serde(deny_unknown_fields)]
pub struct ControlGatewayFileConfig {
    pub base_uri: String,
    pub ca_certificate_locator: String,
    pub client_certificate_locator: String,
    pub client_private_key_locator: String,
    pub allowed_server_sans: Vec<String>,
    pub timeout_milliseconds: u64,
    pub max_request_bytes: usize,
    pub max_response_bytes: usize,
}

impl OwnerResolverFileConfig {
    /// Converts deployment YAML into the contract-owned resolver settings.
    #[must_use]
    pub fn contract(&self) -> EnvironmentOwnerResolverClientConfig {
        EnvironmentOwnerResolverClientConfig {
            resolver_uri: self.resolver_uri.clone(),
            ca_certificate_locator: self.ca_certificate_locator.clone(),
            client_certificate_locator: self.client_certificate_locator.clone(),
            client_private_key_locator: self.client_private_key_locator.clone(),
            allowed_server_sans: self.allowed_server_sans.clone(),
            timeout_milliseconds: self.timeout_milliseconds,
            max_retries: self.max_retries,
        }
    }
}

impl AccessAuthFile {
    /// Parses and validates the non-secret YAML configuration contract.
    pub fn parse_yaml(input: &str) -> Result<Self, AuthConfigError> {
        let parsed: Self =
            serde_yaml::from_str(input).map_err(|_| AuthConfigError::InvalidDeploymentFile)?;
        let _ = AuthConfig::new_with_transport_security(
            &parsed.oidc.issuer,
            parsed.oidc.client_id.clone(),
            &parsed.oidc.redirect_uri,
            &parsed.oidc.post_logout_redirect_uri,
            parsed.oidc.audience.clone(),
            parsed.browser.allowed_origins.clone(),
            parsed.browser.session_ttl_seconds,
            parsed.transport_security,
        )?;
        let resolver = parsed.environment_owner_resolver.contract();
        resolver
            .validate()
            .map_err(|_| AuthConfigError::InvalidDeploymentFile)?;
        if parsed.transport_security == TransportSecurityMode::InsecureTestOnly
            && !parsed.insecure_mode_is_loopback_only()
        {
            return Err(AuthConfigError::InvalidDeploymentFile);
        }
        let required_resolver_locators = [
            resolver.ca_certificate_locator.as_str(),
            resolver.client_certificate_locator.as_str(),
            resolver.client_private_key_locator.as_str(),
        ];
        if parsed.oidc.role_claim_path.is_empty()
            || parsed.oidc.role_claim_path.iter().any(|segment| {
                segment.is_empty()
                    || !segment
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
            })
            || parsed.oidc.role_mappings.is_empty()
            || parsed.oidc.jwt_algorithms.is_empty()
            || parsed.oidc.jwt_algorithms.iter().any(|algorithm| {
                !matches!(
                    algorithm.as_str(),
                    "RS256"
                        | "RS384"
                        | "RS512"
                        | "PS256"
                        | "PS384"
                        | "PS512"
                        | "ES256"
                        | "ES384"
                        | "EdDSA"
                )
            })
            || parsed.oidc.jwks_refresh_seconds == 0
            || parsed.oidc.jwks_retry_seconds == 0
            || parsed.browser.session_idle_ttl_seconds == 0
            || parsed.browser.session_idle_ttl_seconds > parsed.browser.session_ttl_seconds
            || !(60..=900).contains(&parsed.browser.oidc_transaction_ttl_seconds)
            || parsed.browser.runtime_pool_max_connections == 0
            || !(10..=3_600).contains(&parsed.browser.cleanup_interval_seconds)
            || !(3_600..=2_592_000).contains(&parsed.browser.session_retention_seconds)
            || parsed.browser.session_cookie_name != "__Host-labweaver_session"
            || parsed.browser.csrf_header_name != "X-CSRF-Token"
            || parsed
                .browser
                .bind_addr
                .parse::<std::net::SocketAddr>()
                .is_err()
            || parsed.internal_mtls.allowed_san_uris.is_empty()
            || parsed
                .internal_mtls
                .allowed_san_uris
                .iter()
                .any(|san| !valid_spiffe_uri(san))
            || parsed
                .internal_mtls
                .bind_addr
                .parse::<std::net::SocketAddr>()
                .is_err()
            || parsed.internal_mtls.required_eku != "clientAuth"
            || !(1..=5_000).contains(&parsed.environment_owner_resolver.retry_backoff_milliseconds)
            || !(1..=60).contains(&parsed.environment_owner_resolver.decision_ttl_seconds)
            || !parsed.service_gateway_is_valid(&parsed.control_gateway)
            || !parsed.service_gateway_is_valid(&parsed.environment_gateway)
            || !parsed.service_gateway_is_valid(&parsed.evaluation_gateway)
            || !parsed.access_runtime_is_valid()
            || required_resolver_locators.iter().any(|locator| {
                parsed
                    .secrets
                    .file_bindings
                    .get(*locator)
                    .is_none_or(|path| invalid_secret_file_path(path))
            })
        {
            return Err(AuthConfigError::InvalidDeploymentFile);
        }
        Ok(parsed)
    }

    fn access_runtime_is_valid(&self) -> bool {
        !self.grants.gateway_san_uris.is_empty()
            && self.grants.gateway_san_uris.iter().all(|san| {
                san.starts_with("spiffe://") && self.internal_mtls.allowed_san_uris.contains(san)
            })
            && self
                .grants
                .gateway_san_uris
                .iter()
                .collect::<BTreeSet<_>>()
                .len()
                == self.grants.gateway_san_uris.len()
            && matches!(
                url::Host::parse(&self.grants.public_ssh_gateway_hostname),
                Ok(url::Host::Domain(hostname))
                    if hostname == self.grants.public_ssh_gateway_hostname
                        && hostname.contains('.')
            )
            && self.grants.public_ssh_gateway_port == 2222
            && self.grants.public_ssh_gateway_host_key_fingerprint.len() == 50
            && self
                .grants
                .public_ssh_gateway_host_key_fingerprint
                .starts_with("SHA256:")
            && self
                .grants
                .public_ssh_gateway_host_key_fingerprint
                .bytes()
                .skip(7)
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/'))
            && self.grants.default_ttl_seconds > 0
            && self.grants.default_ttl_seconds <= self.grants.max_ttl_seconds
            && self.grants.max_ttl_seconds == 3_600
            && (5..=60).contains(&self.grants.authorization_token_ttl_seconds)
            && (1..=60).contains(&self.grants.activation_poll_seconds)
            && (1..=300).contains(&self.grants.activation_retry_seconds)
            && (1..=100).contains(&self.grants.activation_max_attempts)
            && (1..=60).contains(&self.grants.expiry_poll_seconds)
            && (5..=300).contains(&self.grants.worker_lease_seconds)
            && (1..=100).contains(&self.grants.max_keys_per_actor)
            && (1..=100).contains(&self.grants.max_endpoints_per_grant)
            && self.nats.server.starts_with("tls://")
            && [
                self.nats.ca_certificate_file.as_str(),
                self.nats.client_certificate_file.as_str(),
                self.nats.client_private_key_file.as_str(),
                self.nats.credentials_file.as_str(),
            ]
            .iter()
            .all(|path| !invalid_secret_file_path(path))
    }

    fn service_gateway_is_valid(&self, gateway: &ControlGatewayFileConfig) -> bool {
        let Ok(uri) = Url::parse(&gateway.base_uri) else {
            return false;
        };
        let locators = [
            gateway.ca_certificate_locator.as_str(),
            gateway.client_certificate_locator.as_str(),
            gateway.client_private_key_locator.as_str(),
        ];
        uri.scheme() == "https"
            && uri.host_str().is_some()
            && uri.path() == "/"
            && uri.query().is_none()
            && uri.fragment().is_none()
            && gateway
                .allowed_server_sans
                .iter()
                .any(|san| Some(san.as_str()) == uri.host_str())
            && (100..=30_000).contains(&gateway.timeout_milliseconds)
            && (1..=16 * 1024 * 1024).contains(&gateway.max_request_bytes)
            && (1..=32 * 1024 * 1024).contains(&gateway.max_response_bytes)
            && locators.iter().all(|locator| {
                self.secrets
                    .file_bindings
                    .get(*locator)
                    .is_some_and(|path| !invalid_secret_file_path(path))
            })
    }

    fn insecure_mode_is_loopback_only(&self) -> bool {
        let browser_bind = self.browser.bind_addr.parse::<std::net::SocketAddr>();
        let internal_bind = self.internal_mtls.bind_addr.parse::<std::net::SocketAddr>();
        let resolver = Url::parse(&self.environment_owner_resolver.resolver_uri);
        let control = Url::parse(&self.control_gateway.base_uri);
        let environment = Url::parse(&self.environment_gateway.base_uri);
        let evaluation = Url::parse(&self.evaluation_gateway.base_uri);
        browser_bind.is_ok_and(|address| address.ip().is_loopback())
            && internal_bind.is_ok_and(|address| address.ip().is_loopback())
            && resolver.is_ok_and(|url| url_host_is_loopback(&url))
            && control.is_ok_and(|url| url_host_is_loopback(&url))
            && environment.is_ok_and(|url| url_host_is_loopback(&url))
            && evaluation.is_ok_and(|url| url_host_is_loopback(&url))
    }
}

fn invalid_secret_file_path(path: &str) -> bool {
    path.trim().is_empty()
        || path.contains('\n')
        || path.contains("-----BEGIN")
        || path.starts_with("secret://")
}

impl AuthConfig {
    /// Validates all URL, origin, and lifetime invariants before any listener is opened.
    pub fn new(
        issuer: &str,
        client_id: String,
        redirect_uri: &str,
        post_logout_redirect_uri: &str,
        audience: String,
        allowed_origins: BTreeSet<String>,
        session_ttl_seconds: u64,
    ) -> Result<Self, AuthConfigError> {
        Self::new_with_transport_security(
            issuer,
            client_id,
            redirect_uri,
            post_logout_redirect_uri,
            audience,
            allowed_origins,
            session_ttl_seconds,
            TransportSecurityMode::Strict,
        )
    }

    /// Validates auth configuration against an explicit transport policy.
    #[allow(clippy::too_many_arguments, reason = "mirrors the deployment contract")]
    pub fn new_with_transport_security(
        issuer: &str,
        client_id: String,
        redirect_uri: &str,
        post_logout_redirect_uri: &str,
        audience: String,
        allowed_origins: BTreeSet<String>,
        session_ttl_seconds: u64,
        transport_security: TransportSecurityMode,
    ) -> Result<Self, AuthConfigError> {
        let issuer = transport_url("issuer", issuer, transport_security)?;
        let redirect_uri = browser_url("redirect URI", redirect_uri, transport_security)?;
        let post_logout_redirect_uri = browser_url(
            "post-logout redirect URI",
            post_logout_redirect_uri,
            transport_security,
        )?;
        if client_id.trim().is_empty() || audience.trim().is_empty() {
            return Err(AuthConfigError::MissingIdentityBinding);
        }
        if !(60..=86_400).contains(&session_ttl_seconds) {
            return Err(AuthConfigError::InvalidSessionLifetime);
        }
        if allowed_origins.is_empty()
            || allowed_origins
                .iter()
                .any(|origin| !is_exact_allowed_origin(origin, transport_security))
        {
            return Err(AuthConfigError::InvalidAllowedOrigins);
        }
        Ok(Self {
            issuer,
            client_id,
            redirect_uri,
            post_logout_redirect_uri,
            audience,
            allowed_origins,
            session_ttl_seconds,
            transport_security,
        })
    }
}

fn is_exact_allowed_origin(value: &str, mode: TransportSecurityMode) -> bool {
    let Ok(url) = Url::from_str(value) else {
        return false;
    };
    let transport_allowed = match mode {
        TransportSecurityMode::Strict => url.scheme() == "https",
        TransportSecurityMode::InsecureTestOnly => {
            matches!(url.scheme(), "http" | "https") && url_host_is_loopback(&url)
        }
    };
    transport_allowed
        && url.host_str().is_some()
        && value == url.origin().ascii_serialization()
        && url.query().is_none()
        && url.fragment().is_none()
        && url.username().is_empty()
        && url.password().is_none()
}

fn url_host_is_loopback(url: &Url) -> bool {
    url.host_str().is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost")
            || host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|address| address.is_loopback())
    })
}

fn valid_spiffe_uri(value: &str) -> bool {
    let Ok(url) = Url::parse(value) else {
        return false;
    };
    url.scheme() == "spiffe"
        && url.host_str().is_some()
        && !url.path().is_empty()
        && url.query().is_none()
        && url.fragment().is_none()
        && url.username().is_empty()
        && url.password().is_none()
}

fn https_url(role: &'static str, value: &str) -> Result<Url, AuthConfigError> {
    let url = Url::parse(value).map_err(|_| AuthConfigError::InvalidUrl(role))?;
    if url.scheme() != "https" || url.host_str().is_none() || url.query().is_some() {
        return Err(AuthConfigError::InvalidUrl(role));
    }
    Ok(url)
}

fn transport_url(
    role: &'static str,
    value: &str,
    mode: TransportSecurityMode,
) -> Result<Url, AuthConfigError> {
    if mode == TransportSecurityMode::Strict {
        return https_url(role, value);
    }
    let url = Url::parse(value).map_err(|_| AuthConfigError::InvalidUrl(role))?;
    if !matches!(url.scheme(), "http" | "https")
        || !url_host_is_loopback(&url)
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(AuthConfigError::InvalidUrl(role));
    }
    Ok(url)
}

fn https_or_loopback_url(role: &'static str, value: &str) -> Result<Url, AuthConfigError> {
    let url = Url::parse(value).map_err(|_| AuthConfigError::InvalidUrl(role))?;
    let loopback = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "::1"));
    if !(url.scheme() == "https" || (url.scheme() == "http" && loopback))
        || url.fragment().is_some()
    {
        return Err(AuthConfigError::InvalidUrl(role));
    }
    Ok(url)
}

fn browser_url(
    role: &'static str,
    value: &str,
    mode: TransportSecurityMode,
) -> Result<Url, AuthConfigError> {
    if mode == TransportSecurityMode::InsecureTestOnly {
        transport_url(role, value, mode)
    } else {
        https_or_loopback_url(role, value)
    }
}

/// Configuration failures that must stop startup.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum AuthConfigError {
    /// A URL is malformed or violates the transport policy.
    #[error("LW_AUTH_CONFIG_URL_INVALID: invalid {0}")]
    InvalidUrl(&'static str),
    /// Client and audience must be explicit.
    #[error("LW_AUTH_CONFIG_BINDING_MISSING: client ID and audience are required")]
    MissingIdentityBinding,
    /// Sessions cannot be unbounded.
    #[error("LW_AUTH_CONFIG_SESSION_TTL_INVALID: session lifetime must be 60-86400 seconds")]
    InvalidSessionLifetime,
    /// Browser mutation origins must be exact HTTPS origins.
    #[error("LW_AUTH_CONFIG_ORIGIN_INVALID: allowed origins must be non-empty HTTPS origins")]
    InvalidAllowedOrigins,
    /// Deployment YAML was malformed or violated mandatory security invariants.
    #[error("LW_AUTH_CONFIG_BINDING_MISSING: deployment auth configuration is invalid")]
    InvalidDeploymentFile,
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{AccessAuthFile, AuthConfig, TransportSecurityMode};

    #[test]
    fn configuration_rejects_insecure_issuer_and_unbounded_session() {
        let origins = BTreeSet::from(["https://portal.example.test/".to_owned()]);
        assert!(
            AuthConfig::new(
                "http://issuer.example.test/realms/labweaver",
                "web".into(),
                "https://portal.example.test/auth/callback",
                "https://portal.example.test/",
                "labweaver-api".into(),
                origins.clone(),
                900,
            )
            .is_err()
        );
        assert!(
            AuthConfig::new(
                "https://issuer.example.test/realms/labweaver",
                "web".into(),
                "https://portal.example.test/auth/callback",
                "https://portal.example.test/",
                "labweaver-api".into(),
                origins,
                0,
            )
            .is_err()
        );
    }

    #[test]
    fn insecure_transport_is_explicit_and_loopback_only() {
        let local = AuthConfig::new_with_transport_security(
            "http://127.0.0.1:8081/realms/labweaver",
            "web".into(),
            "http://127.0.0.1:8080/auth/callback",
            "http://127.0.0.1:8080/",
            "api".into(),
            BTreeSet::from(["http://127.0.0.1:8080".to_owned()]),
            900,
            TransportSecurityMode::InsecureTestOnly,
        );
        assert!(local.is_ok());
        let remote = AuthConfig::new_with_transport_security(
            "http://keycloak.example.test/realms/labweaver",
            "web".into(),
            "http://127.0.0.1:8080/auth/callback",
            "http://127.0.0.1:8080/",
            "api".into(),
            BTreeSet::from(["http://127.0.0.1:8080".to_owned()]),
            900,
            TransportSecurityMode::InsecureTestOnly,
        );
        assert!(remote.is_err());
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one complete deployment fixture exercises deny-unknown-fields parsing"
    )]
    fn deployment_file_rejects_unpinned_browser_security_values() {
        let valid = r#"
oidc:
  issuer: https://issuer.example.test/realms/labweaver
  client_id: web
  audience: api
  redirect_uri: https://portal.example.test/auth/callback
  post_logout_redirect_uri: https://portal.example.test/
  trusted_ca_file: ""
  role_claim_path: [realm_access, roles]
  role_mappings: { teacher: teacher }
  jwt_algorithms: [RS256]
  jwks_refresh_seconds: 600
  jwks_retry_seconds: 10
browser:
  bind_addr: 127.0.0.1:8080
  allowed_origins: [https://portal.example.test]
  session_ttl_seconds: 900
  session_idle_ttl_seconds: 300
  oidc_transaction_ttl_seconds: 300
  runtime_pool_max_connections: 10
  cleanup_interval_seconds: 60
  session_retention_seconds: 86400
  session_cookie_name: __Host-labweaver_session
  csrf_header_name: X-CSRF-Token
secrets:
  oidc_client_secret_file: ""
  session_keyring_file: keyring
  active_session_key_id: active
  access_runtime_url_file: database-url
  file_bindings:
    "secret://environment-owner-resolver/ca": resolver-ca
    "secret://access-service/resolver-cert": resolver-cert
    "secret://access-service/resolver-key": resolver-key
    "secret://control-gateway/ca": control-ca
    "secret://access-service/control-client-cert": control-cert
    "secret://access-service/control-client-key": control-key
internal_mtls:
  bind_addr: 127.0.0.1:9443
  server_certificate_file: server-cert
  server_key_file: server-key
  client_ca_file: client-ca
  allowed_san_uris: [spiffe://labweaver/gateway]
  required_eku: clientAuth
environment_owner_resolver:
  resolver_uri: https://environment-owner-resolver.example.test:9444
  ca_certificate_locator: secret://environment-owner-resolver/ca
  client_certificate_locator: secret://access-service/resolver-cert
  client_private_key_locator: secret://access-service/resolver-key
  allowed_server_sans: [environment-owner-resolver.example.test]
  timeout_milliseconds: 2000
  max_retries: 1
  retry_backoff_milliseconds: 100
  decision_ttl_seconds: 5
control_gateway:
  base_uri: https://control-service.example.test:9444/
  ca_certificate_locator: secret://control-gateway/ca
  client_certificate_locator: secret://access-service/control-client-cert
  client_private_key_locator: secret://access-service/control-client-key
  allowed_server_sans: [control-service.example.test]
  timeout_milliseconds: 5000
  max_request_bytes: 1048576
  max_response_bytes: 8388608
environment_gateway:
  base_uri: https://environment-service.example.test:9446/
  ca_certificate_locator: secret://control-gateway/ca
  client_certificate_locator: secret://access-service/control-client-cert
  client_private_key_locator: secret://access-service/control-client-key
  allowed_server_sans: [environment-service.example.test]
  timeout_milliseconds: 5000
  max_request_bytes: 1048576
  max_response_bytes: 8388608
evaluation_gateway:
  base_uri: https://evaluation-service.example.test:9447/
  ca_certificate_locator: secret://control-gateway/ca
  client_certificate_locator: secret://access-service/control-client-cert
  client_private_key_locator: secret://access-service/control-client-key
  allowed_server_sans: [evaluation-service.example.test]
  timeout_milliseconds: 5000
  max_request_bytes: 1048576
  max_response_bytes: 8388608
grants:
  gateway_san_uris: [spiffe://labweaver/gateway]
  public_ssh_gateway_hostname: demo.lab.lan
  public_ssh_gateway_port: 2222
  public_ssh_gateway_host_key_fingerprint: SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA
  default_ttl_seconds: 1800
  max_ttl_seconds: 3600
  authorization_token_ttl_seconds: 30
  activation_poll_seconds: 2
  activation_retry_seconds: 10
  activation_max_attempts: 8
  expiry_poll_seconds: 5
  worker_lease_seconds: 30
  max_keys_per_actor: 10
  max_endpoints_per_grant: 16
nats:
  server: tls://nats.example.test:4222
  ca_certificate_file: nats-ca
  client_certificate_file: nats-cert
  client_private_key_file: nats-key
  credentials_file: nats-creds
"#;
        assert!(AccessAuthFile::parse_yaml(valid).is_ok());
        assert!(
            AccessAuthFile::parse_yaml(&valid.replace("__Host-labweaver_session", "session"))
                .is_err()
        );
        assert!(
            AccessAuthFile::parse_yaml(&valid.replace(
                "https://portal.example.test]",
                "https://portal.example.test/]"
            ))
            .is_err()
        );
        assert!(
            AccessAuthFile::parse_yaml(
                &valid.replace("spiffe://labweaver/gateway", "https://labweaver/gateway")
            )
            .is_err()
        );
    }

    #[test]
    fn checked_in_deployment_example_is_a_valid_contract() {
        let example = include_str!("../../../deploy/config/access-auth.yaml.example");
        assert!(AccessAuthFile::parse_yaml(example).is_ok());
    }
}
