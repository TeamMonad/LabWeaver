//! Service-account JWT coverage at the Environment HTTP boundary.

use std::{collections::BTreeSet, sync::Arc};

use auth::{
    ServiceAuthConfig, ServiceTokenVerifier, TransportSecurityMode, no_redirect_http_client,
};
use axum::{Json, Router, extract::State, routing::get};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use rcgen::KeyPair;
use serde_json::{Value, json};
use tokio::{
    net::TcpListener,
    sync::{RwLock, oneshot},
};

use environment_service::with_service_auth;

const AUDIENCE: &str = "labweaver-environment";
const CLIENT_ID: &str = "access-gateway";
const REQUIRED_PERMISSION: &str = "environment.read";

#[derive(Clone)]
struct AuthorityState {
    issuer: String,
    key: Arc<RwLock<SigningMaterial>>,
}

#[derive(Clone)]
struct SigningMaterial {
    kid: String,
    private_der: Vec<u8>,
    jwk: Value,
}

struct AuthorityHandle {
    issuer: String,
    key: SigningMaterial,
    shutdown: Option<oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for AuthorityHandle {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        self.task.abort();
    }
}

#[tokio::test]
async fn middleware_accepts_real_jwt_and_rejects_missing_or_wrong_permission()
-> Result<(), Box<dyn std::error::Error>> {
    let key = signing_material("environment")?;
    let authority = spawn_authority(key).await?;
    let verifier = ServiceTokenVerifier::discover(
        ServiceAuthConfig::new(
            &authority.issuer,
            AUDIENCE.to_owned(),
            BTreeSet::from([CLIENT_ID.to_owned()]),
            BTreeSet::from([REQUIRED_PERMISSION.to_owned()]),
            BTreeSet::from(["ES256".to_owned()]),
            3_600,
            1,
            TransportSecurityMode::InsecureTestOnly,
        )?,
        no_redirect_http_client(None, TransportSecurityMode::InsecureTestOnly)?,
    )
    .await?;

    let router = with_service_auth(
        Router::new().route("/protected", get(protected)),
        Arc::new(verifier),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
    let task = tokio::spawn(async move {
        tokio::select! {
            result = axum::serve(listener, router) => {
                let _ = result;
            }
            _ = &mut shutdown_rx => {}
        }
    });

    let client = reqwest::Client::new();
    let url = format!("http://{address}/protected");

    let missing = client.get(&url).send().await?;
    assert_eq!(missing.status(), reqwest::StatusCode::UNAUTHORIZED);

    let valid = signed_token(
        &authority.key,
        &authority.issuer,
        CLIENT_ID,
        AUDIENCE,
        [REQUIRED_PERMISSION],
    )?;
    let accepted = client.get(&url).bearer_auth(valid).send().await?;
    assert_eq!(accepted.status(), reqwest::StatusCode::OK);
    assert_eq!(accepted.text().await?, "protected");

    let wrong_permission = signed_token(
        &authority.key,
        &authority.issuer,
        CLIENT_ID,
        AUDIENCE,
        ["environment.write"],
    )?;
    let rejected = client
        .get(&url)
        .bearer_auth(wrong_permission)
        .send()
        .await?;
    assert_eq!(rejected.status(), reqwest::StatusCode::UNAUTHORIZED);

    let _ = shutdown_tx.send(());
    task.await?;
    Ok(())
}

async fn protected() -> &'static str {
    "protected"
}

fn signing_material(kid: &str) -> Result<SigningMaterial, rcgen::Error> {
    let key = KeyPair::generate()?;
    let public = key.public_key_raw();
    assert_eq!(public.len(), 65);
    assert_eq!(public[0], 4);
    let encode_component = |component: &[u8]| URL_SAFE_NO_PAD.encode(component);
    Ok(SigningMaterial {
        kid: kid.to_owned(),
        private_der: key.serialize_der(),
        jwk: json!({
            "kty": "EC",
            "crv": "P-256",
            "x": encode_component(&public[1..33]),
            "y": encode_component(&public[33..65]),
            "use": "sig",
            "alg": "ES256",
            "kid": kid
        }),
    })
}

fn signed_token(
    material: &SigningMaterial,
    issuer: &str,
    client_id: &str,
    audience: &str,
    permissions: [&str; 1],
) -> Result<String, jsonwebtoken::errors::Error> {
    let mut header = Header::new(Algorithm::ES256);
    header.kid = Some(material.kid.clone());
    let claims = json!({
        "iss": issuer,
        "sub": "service-account",
        "azp": client_id,
        "aud": audience,
        "exp": time::OffsetDateTime::now_utc().unix_timestamp() + 300,
        "nbf": time::OffsetDateTime::now_utc().unix_timestamp() - 1,
        "resource_access": {
            audience: {"roles": permissions}
        }
    });
    encode(
        &header,
        &claims,
        &EncodingKey::from_ec_der(&material.private_der),
    )
}

async fn spawn_authority(
    key: SigningMaterial,
) -> Result<AuthorityHandle, Box<dyn std::error::Error>> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let issuer = format!("http://localhost:{}/realms/test", address.port());
    let state = AuthorityState {
        issuer: issuer.clone(),
        key: Arc::new(RwLock::new(key.clone())),
    };
    let router = Router::new()
        .route(
            "/realms/test/.well-known/openid-configuration",
            get(discovery),
        )
        .route("/realms/test/jwks", get(jwks))
        .with_state(state);
    let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
    let task = tokio::spawn(async move {
        tokio::select! {
            result = axum::serve(listener, router) => {
                let _ = result;
            }
            _ = &mut shutdown_rx => {}
        }
    });
    Ok(AuthorityHandle {
        issuer,
        key,
        shutdown: Some(shutdown_tx),
        task,
    })
}

async fn discovery(State(state): State<AuthorityState>) -> Json<Value> {
    Json(json!({
        "issuer": state.issuer,
        "authorization_endpoint": format!("{}/authorize", state.issuer),
        "token_endpoint": format!("{}/token", state.issuer),
        "jwks_uri": format!("{}/jwks", state.issuer),
        "response_types_supported": ["code"],
        "subject_types_supported": ["public"],
        "id_token_signing_alg_values_supported": ["ES256"],
        "grant_types_supported": ["authorization_code", "client_credentials"]
    }))
}

async fn jwks(State(state): State<AuthorityState>) -> Json<Value> {
    let key = state.key.read().await;
    Json(json!({"keys": [key.jwk.clone()]}))
}
