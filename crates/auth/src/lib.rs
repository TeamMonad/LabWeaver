//! Keycloak/OIDC authentication and `LabWeaver` scope-authorization primitives.
//!
//! This crate owns no HTTP server, database connection, or provider fallback. Service adapters
//! provide those boundaries and must map every [`AuthError`] to a stable problem response.

#![allow(
    clippy::missing_errors_doc,
    reason = "the public API exposes small validation helpers with typed errors"
)]

/// Base-role and authoritative scope evaluation.
pub mod authorization;
/// Fail-fast OIDC relying-party configuration.
pub mod config;
pub mod crypto;
/// Synchronizer CSRF tokens for BFF sessions.
pub mod csrf;
/// Bearer JWT verifier with JWKS rotation.
pub mod jwt;
/// NATS mTLS connection helper shared across services.
pub mod nats;
/// One-time OIDC Authorization Code + PKCE transaction values.
pub mod oidc;
/// Environment-authoritative owner resolver mTLS client.
pub mod owner_resolver;
pub mod provider;
pub mod repository;
/// Short-lived signed identity delegation from Access to Resource.
pub mod resource_delegation;
pub mod roles;

pub use authorization::{AuthorizationContext, AuthorizationError, authorize};
pub use config::{
    AccessAuthFile, AuthConfig, AuthConfigError, ControlGatewayFileConfig, GrantRuntimeFileConfig,
    MtlsFileConfig, NatsFileConfig, ResourceGatewayFileConfig, TransportSecurityMode,
};
pub use crypto::{CryptoError, EncryptedValue, KeyRing};
pub use csrf::{CsrfError, CsrfToken, verify_csrf_token};
pub use jwt::{
    BackchannelLogoutClaims, BearerClaims, JwtVerifierError, build_backchannel_logout_authorizer,
    build_bearer_authorizer,
};
pub use nats::NatsMtlsError;
pub use oidc::{OidcTransaction, OidcTransactionError};
pub use owner_resolver::{EnvironmentOwnerResolverClient, OwnerResolverClientError};
pub use provider::{
    OidcProvider, OidcProviderError, VerifiedOidcIdentity, no_redirect_http_client,
};
pub use repository::{
    AuthCleanupReport, BffSession, CreateBffSession, LocalActor, MembershipSnapshot,
    RepositoryError, cleanup_expired_auth_state, consume_backchannel_logout,
    consume_oidc_transaction, create_bff_session, load_bff_session, load_logout_hint,
    load_membership_snapshot, require_service_identity, revoke_bff_session,
    revoke_bff_sessions_by_sid, upsert_actor,
};
pub use resource_delegation::{
    ResourceDelegation, ResourceDelegationError, decode_resource_delegation,
    encode_resource_delegation,
};
pub use roles::{RoleClaimError, RoleMappings, extract_platform_roles};

/// Typed failure at the authentication/authorization boundary.
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    /// Configuration is unsafe or incomplete.
    #[error(transparent)]
    Config(#[from] AuthConfigError),
    /// Browser OIDC transaction state was invalid.
    #[error(transparent)]
    OidcTransaction(#[from] OidcTransactionError),
    /// Permission or scope was denied.
    #[error(transparent)]
    Authorization(#[from] AuthorizationError),
}
