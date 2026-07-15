//! OIDC Discovery and authorization-request construction.

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use openidconnect::{
    AuthorizationCode, ClientId, ClientSecret, CsrfToken, IssuerUrl, LogoutRequest, Nonce,
    PkceCodeChallenge, PkceCodeVerifier, PostLogoutRedirectUrl, ProviderMetadataWithLogout,
    RedirectUrl, Scope, TokenResponse,
    core::{CoreAuthenticationFlow, CoreClient, CoreIdToken},
};
use reqwest::redirect::Policy;
use serde::Deserialize;
use time::OffsetDateTime;
use url::Url;

use crate::{AuthConfig, OidcTransaction, TransportSecurityMode};

/// Discovered Keycloak relying-party client.
#[derive(Clone)]
pub struct OidcProvider {
    metadata: ProviderMetadataWithLogout,
    client_id: String,
    client_secret: Option<String>,
    redirect_uri: String,
}

impl OidcProvider {
    /// Performs Discovery through a no-redirect HTTP client to avoid SSRF redirect chains.
    pub async fn discover(
        config: &AuthConfig,
        client_secret: Option<String>,
        trusted_ca_pem: Option<&[u8]>,
    ) -> Result<Self, OidcProviderError> {
        let http = no_redirect_http_client(trusted_ca_pem, config.transport_security)?;
        Self::discover_with_http_client(config, http)
            .await
            .map(|mut provider| {
                provider.client_secret = client_secret;
                provider
            })
    }

    /// Performs discovery with an explicitly configured trust store. This is
    /// intended for controlled deployments and disposable TLS integration tests.
    pub async fn discover_with_http_client(
        config: &AuthConfig,
        http: reqwest::Client,
    ) -> Result<Self, OidcProviderError> {
        let metadata = ProviderMetadataWithLogout::discover_async(
            IssuerUrl::new(config.issuer.to_string()).map_err(|_| OidcProviderError::Issuer)?,
            &http,
        )
        .await
        .map_err(|_| OidcProviderError::Discovery)?;
        if metadata
            .additional_metadata()
            .end_session_endpoint
            .is_none()
        {
            return Err(OidcProviderError::EndSessionEndpoint);
        }
        validate_discovered_endpoints(&metadata, config.transport_security)?;
        Ok(Self {
            metadata,
            client_id: config.client_id.clone(),
            client_secret: None,
            redirect_uri: config.redirect_uri.to_string(),
        })
    }

    /// Builds an authorization-code request with mandatory PKCE S256, state and nonce.
    pub fn authorization_url(
        &self,
        transaction: &OidcTransaction,
    ) -> Result<Url, OidcProviderError> {
        let verifier = PkceCodeVerifier::new(transaction.pkce_verifier.clone());
        let client = CoreClient::from_provider_metadata(
            self.metadata.clone(),
            ClientId::new(self.client_id.clone()),
            None,
        )
        .set_redirect_uri(
            RedirectUrl::new(self.redirect_uri.clone())
                .map_err(|_| OidcProviderError::RedirectUri)?,
        );
        let state = transaction.state.clone();
        let nonce = transaction.nonce.clone();
        let (url, _, _) = client
            .authorize_url(
                CoreAuthenticationFlow::AuthorizationCode,
                move || CsrfToken::new(state),
                move || Nonce::new(nonce),
            )
            .add_scope(Scope::new("openid".to_owned()))
            .set_pkce_challenge(PkceCodeChallenge::from_code_verifier_sha256(&verifier))
            .url();
        Ok(url)
    }

    /// Exchanges an authorization code, validates the signed ID token, and
    /// returns only the values needed to create a local session. OIDC tokens
    /// are deliberately not returned or persisted.
    pub async fn exchange_code(
        &self,
        code: String,
        transaction: &OidcTransaction,
        http: &reqwest::Client,
    ) -> Result<VerifiedOidcIdentity, OidcProviderError> {
        let client = CoreClient::from_provider_metadata(
            self.metadata.clone(),
            ClientId::new(self.client_id.clone()),
            self.client_secret.clone().map(ClientSecret::new),
        )
        .set_redirect_uri(
            RedirectUrl::new(self.redirect_uri.clone())
                .map_err(|_| OidcProviderError::RedirectUri)?,
        );
        let response = client
            .exchange_code(AuthorizationCode::new(code))
            .map_err(|_| OidcProviderError::TokenEndpoint)?
            .set_pkce_verifier(PkceCodeVerifier::new(transaction.pkce_verifier.clone()))
            .request_async(http)
            .await
            .map_err(|_| OidcProviderError::TokenExchange)?;
        let id_token = response
            .id_token()
            .ok_or(OidcProviderError::IdTokenMissing)?;
        let claims = id_token
            .claims(
                &client.id_token_verifier(),
                &Nonce::new(transaction.nonce.clone()),
            )
            .map_err(|_| OidcProviderError::IdTokenRejected)?;
        if claims.authorized_party().map(|azp| azp.as_str()) != Some(self.client_id.as_str()) {
            return Err(OidcProviderError::AuthorizedPartyRejected);
        }
        let encoded = id_token.to_string();
        let payload = encoded
            .split('.')
            .nth(1)
            .ok_or(OidcProviderError::IdTokenRejected)?;
        let payload = URL_SAFE_NO_PAD
            .decode(payload)
            .map_err(|_| OidcProviderError::IdTokenRejected)?;
        let raw: RawIdTokenClaims =
            serde_json::from_slice(&payload).map_err(|_| OidcProviderError::IdTokenRejected)?;
        let expires_at = OffsetDateTime::from_unix_timestamp(raw.exp)
            .map_err(|_| OidcProviderError::IdTokenRejected)?;
        Ok(VerifiedOidcIdentity {
            subject: claims.subject().as_str().to_owned(),
            expires_at,
            sid: raw.sid,
            claims: serde_json::Value::Object(raw.claims),
            logout_hint: encoded,
        })
    }

    /// Builds a standards-based RP-Initiated Logout URL from Discovery
    /// metadata and the verified ID token retained only in encrypted session
    /// storage.
    pub fn logout_url(
        &self,
        id_token_hint: &str,
        post_logout_redirect_uri: &str,
    ) -> Result<Url, OidcProviderError> {
        let endpoint = self
            .metadata
            .additional_metadata()
            .end_session_endpoint
            .clone()
            .ok_or(OidcProviderError::EndSessionEndpoint)?;
        let id_token = id_token_hint
            .parse::<CoreIdToken>()
            .map_err(|_| OidcProviderError::IdTokenRejected)?;
        let redirect = PostLogoutRedirectUrl::new(post_logout_redirect_uri.to_owned())
            .map_err(|_| OidcProviderError::RedirectUri)?;
        Ok(LogoutRequest::from(endpoint)
            .set_id_token_hint(&id_token)
            .set_client_id(ClientId::new(self.client_id.clone()))
            .set_post_logout_redirect_uri(redirect)
            .http_get_url())
    }
}

/// Builds the controlled OIDC HTTP client. Redirects are disabled and a
/// configured private CA replaces the system roots in strict mode.
pub fn no_redirect_http_client(
    trusted_ca_pem: Option<&[u8]>,
    transport_security: TransportSecurityMode,
) -> Result<reqwest::Client, OidcProviderError> {
    let mut builder = reqwest::Client::builder().redirect(Policy::none());
    if transport_security == TransportSecurityMode::Strict {
        builder = builder.https_only(true);
    } else {
        builder = builder.danger_accept_invalid_certs(true);
    }
    if let Some(pem) = trusted_ca_pem {
        let certificates =
            reqwest::Certificate::from_pem_bundle(pem).map_err(|_| OidcProviderError::TrustedCa)?;
        if certificates.is_empty() {
            return Err(OidcProviderError::TrustedCa);
        }
        if transport_security == TransportSecurityMode::Strict {
            builder = builder.tls_built_in_root_certs(false);
        }
        for certificate in certificates {
            builder = builder.add_root_certificate(certificate);
        }
    }
    builder.build().map_err(|_| OidcProviderError::HttpClient)
}

fn validate_discovered_endpoints(
    metadata: &ProviderMetadataWithLogout,
    transport_security: TransportSecurityMode,
) -> Result<(), OidcProviderError> {
    let token_endpoint = metadata
        .token_endpoint()
        .ok_or(OidcProviderError::TokenEndpoint)?;
    let logout_endpoint = metadata
        .additional_metadata()
        .end_session_endpoint
        .as_ref()
        .ok_or(OidcProviderError::EndSessionEndpoint)?;
    let endpoints = [
        metadata.authorization_endpoint().url(),
        token_endpoint.url(),
        metadata.jwks_uri().url(),
        logout_endpoint.url(),
    ];
    if endpoints
        .iter()
        .any(|url| !endpoint_transport_allowed(url, transport_security))
    {
        return Err(OidcProviderError::EndpointTransport);
    }
    Ok(())
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

/// Verified identity attributes safe to hand to the local actor mapper.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedOidcIdentity {
    /// Opaque issuer-local subject, never logged or returned by an API.
    pub subject: String,
    /// ID-token hard expiry.
    pub expires_at: OffsetDateTime,
    /// Provider session identifier used only as a digest for back-channel logout.
    pub sid: Option<String>,
    /// Signed claim payload used for deployment-configured role extraction.
    pub claims: serde_json::Value,
    /// Verified ID token retained only long enough to encrypt a logout hint in
    /// the server-side session; it must never be sent to a browser API.
    pub logout_hint: String,
}

#[derive(Deserialize)]
struct RawIdTokenClaims {
    exp: i64,
    #[serde(default)]
    sid: Option<String>,
    #[serde(flatten)]
    claims: serde_json::Map<String, serde_json::Value>,
}

/// Provider failures expose no upstream payload and map to retryable service failures.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum OidcProviderError {
    /// Secure HTTP client construction failed.
    #[error("LW_AUTH_OIDC_HTTP_CLIENT_FAILED")]
    HttpClient,
    /// Configured issuer could not become an OIDC issuer URL.
    #[error("LW_AUTH_CONFIG_URL_INVALID")]
    Issuer,
    /// Redirect URI cannot be represented by the OIDC client.
    #[error("LW_AUTH_CONFIG_URL_INVALID")]
    RedirectUri,
    /// Discovery or provider metadata validation failed.
    #[error("LW_AUTH_JWKS_UNAVAILABLE")]
    Discovery,
    /// Discovery metadata omitted the mandatory RP-Initiated Logout endpoint.
    #[error("LW_AUTH_CONFIG_BINDING_MISSING")]
    EndSessionEndpoint,
    /// The configured private trust anchor was malformed.
    #[error("LW_AUTH_CONFIG_BINDING_MISSING")]
    TrustedCa,
    /// Discovery metadata omitted a usable token endpoint.
    #[error("LW_AUTH_OIDC_TOKEN_EXCHANGE_FAILED")]
    TokenEndpoint,
    /// A discovered endpoint violated the configured transport policy.
    #[error("LW_AUTH_CONFIG_URL_INVALID")]
    EndpointTransport,
    /// The authorization server did not complete the code exchange.
    #[error("LW_AUTH_OIDC_TOKEN_EXCHANGE_FAILED")]
    TokenExchange,
    /// The provider did not return a signed ID token.
    #[error("LW_AUTH_OIDC_TOKEN_INVALID")]
    IdTokenMissing,
    /// ID-token signature, issuer, audience, expiry, or nonce validation failed.
    #[error("LW_AUTH_OIDC_TOKEN_INVALID")]
    IdTokenRejected,
    /// The token did not bind its authorized party to this client.
    #[error("LW_AUTH_OIDC_TOKEN_INVALID")]
    AuthorizedPartyRejected,
}

#[cfg(test)]
mod tests {
    use super::{TransportSecurityMode, Url, endpoint_transport_allowed};

    #[test]
    fn discovered_endpoint_transport_is_strict_or_loopback_only()
    -> Result<(), Box<dyn std::error::Error>> {
        let secure = Url::parse("https://keycloak.example.test/token")?;
        let local_http = Url::parse("http://127.0.0.1:18080/token")?;
        let remote_http = Url::parse("http://keycloak.example.test/token")?;
        assert!(endpoint_transport_allowed(
            &secure,
            TransportSecurityMode::Strict
        ));
        assert!(!endpoint_transport_allowed(
            &local_http,
            TransportSecurityMode::Strict
        ));
        assert!(endpoint_transport_allowed(
            &local_http,
            TransportSecurityMode::InsecureTestOnly
        ));
        assert!(!endpoint_transport_allowed(
            &remote_http,
            TransportSecurityMode::InsecureTestOnly
        ));
        Ok(())
    }
}
