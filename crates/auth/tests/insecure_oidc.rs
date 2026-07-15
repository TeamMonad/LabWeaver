//! Loopback-only coverage for the explicitly acknowledged insecure test transport.

use std::collections::BTreeSet;

use auth::{AuthConfig, OidcProvider, TransportSecurityMode, no_redirect_http_client};
use axum::{Json, Router, extract::State, routing::get};
use serde_json::{Value, json};
use tokio::net::TcpListener;

#[tokio::test]
async fn insecure_oidc_discovery_is_usable_only_on_loopback()
-> Result<(), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let issuer = format!("http://{address}/realms/labweaver");
    let router = Router::new()
        .route(
            "/realms/labweaver/.well-known/openid-configuration",
            get(discovery),
        )
        .route("/realms/labweaver/jwks", get(jwks))
        .with_state(issuer.clone());
    let server = tokio::spawn(async move { axum::serve(listener, router).await });

    let config = AuthConfig::new_with_transport_security(
        &issuer,
        "labweaver-web".to_owned(),
        "http://127.0.0.1:38080/auth/callback",
        "http://127.0.0.1:38080/",
        "labweaver-api".to_owned(),
        BTreeSet::from(["http://127.0.0.1:38080".to_owned()]),
        900,
        TransportSecurityMode::InsecureTestOnly,
    )?;
    let http = no_redirect_http_client(None, TransportSecurityMode::InsecureTestOnly)?;
    OidcProvider::discover_with_http_client(&config, http).await?;

    server.abort();
    Ok(())
}

async fn discovery(State(issuer): State<String>) -> Json<Value> {
    Json(json!({
        "issuer": issuer,
        "authorization_endpoint": format!("{issuer}/authorize"),
        "token_endpoint": format!("{issuer}/token"),
        "jwks_uri": format!("{issuer}/jwks"),
        "end_session_endpoint": format!("{issuer}/logout"),
        "response_types_supported": ["code"],
        "subject_types_supported": ["public"],
        "id_token_signing_alg_values_supported": ["RS256"]
    }))
}

async fn jwks() -> Json<Value> {
    Json(json!({"keys": []}))
}
