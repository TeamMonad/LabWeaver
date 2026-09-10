//! Service-account JWT coverage at the Agent HTTP boundary.

use std::{collections::BTreeSet, sync::Arc};

use agent_service::api::{CONTROL_PERMISSION, LLM_REVIEW_CREATE_PERMISSION, with_service_auth};
use auth::{
    ServiceAuthConfig, ServiceTokenVerifier, TransportSecurityMode, no_redirect_http_client,
};
use axum::{
    Router,
    routing::{get, post},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use rcgen::KeyPair;
use serde_json::{Value, json};
use tokio::{net::TcpListener, sync::oneshot};

const AUDIENCE: &str = "labweaver-agent";
const CONTROL_CLIENT_ID: &str = "labweaver-control";
const EVALUATION_CLIENT_ID: &str = "labweaver-evaluation";

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

#[derive(Clone)]
struct AuthorityState {
    issuer: String,
    jwk: Value,
}

#[tokio::test]
async fn middleware_requires_control_jwt_and_permission() -> Result<(), Box<dyn std::error::Error>>
{
    let authority_key = signing_material("control")?;
    let authority = spawn_authority(&authority_key).await?;
    let verifier = ServiceTokenVerifier::discover(
        ServiceAuthConfig::new(
            &authority.issuer,
            AUDIENCE.to_owned(),
            BTreeSet::from([CONTROL_CLIENT_ID.to_owned()]),
            BTreeSet::new(),
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

    let forged_key = signing_material("control")?;
    let forged = signed_token(
        &forged_key,
        &authority.issuer,
        CONTROL_CLIENT_ID,
        &[CONTROL_PERMISSION],
    )?;
    let forged_response = client.get(&url).bearer_auth(forged).send().await?;
    assert_eq!(forged_response.status(), reqwest::StatusCode::UNAUTHORIZED);

    let valid = signed_token(
        &authority.key,
        &authority.issuer,
        CONTROL_CLIENT_ID,
        &[CONTROL_PERMISSION],
    )?;
    let accepted = client.get(&url).bearer_auth(valid).send().await?;
    assert_eq!(accepted.status(), reqwest::StatusCode::OK);
    assert_eq!(accepted.text().await?, "protected");

    let wrong_permission = signed_token(
        &authority.key,
        &authority.issuer,
        CONTROL_CLIENT_ID,
        &["agent.read"],
    )?;
    let rejected = client
        .get(&url)
        .bearer_auth(wrong_permission)
        .send()
        .await?;
    assert_eq!(rejected.status(), reqwest::StatusCode::FORBIDDEN);

    let _ = shutdown_tx.send(());
    task.await?;
    Ok(())
}

#[tokio::test]
async fn middleware_uses_route_permissions_for_control_and_review_clients()
-> Result<(), Box<dyn std::error::Error>> {
    let authority_key = signing_material("production")?;
    let authority = spawn_authority(&authority_key).await?;
    let verifier = ServiceTokenVerifier::discover(
        ServiceAuthConfig::new(
            &authority.issuer,
            AUDIENCE.to_owned(),
            BTreeSet::from([
                CONTROL_CLIENT_ID.to_owned(),
                EVALUATION_CLIENT_ID.to_owned(),
            ]),
            BTreeSet::new(),
            BTreeSet::from(["ES256".to_owned()]),
            3_600,
            1,
            TransportSecurityMode::InsecureTestOnly,
        )?,
        no_redirect_http_client(None, TransportSecurityMode::InsecureTestOnly)?,
    )
    .await?;

    let router = with_service_auth(
        Router::new()
            .route("/internal/v1/agent-runs", post(control_route))
            .route("/internal/v1/llm-reviews", post(review_route)),
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
    let control_url = format!("http://{address}/internal/v1/agent-runs");
    let review_url = format!("http://{address}/internal/v1/llm-reviews");
    let control_token = signed_token(
        &authority.key,
        &authority.issuer,
        CONTROL_CLIENT_ID,
        &[CONTROL_PERMISSION],
    )?;
    let control_route_response = client
        .post(&control_url)
        .bearer_auth(&control_token)
        .send()
        .await?;
    assert_eq!(control_route_response.status(), reqwest::StatusCode::OK);
    assert_eq!(control_route_response.text().await?, "control");
    let control_review_response = client
        .post(&review_url)
        .bearer_auth(&control_token)
        .send()
        .await?;
    assert_eq!(
        control_review_response.status(),
        reqwest::StatusCode::FORBIDDEN
    );

    let evaluation_token = signed_token(
        &authority.key,
        &authority.issuer,
        EVALUATION_CLIENT_ID,
        &[LLM_REVIEW_CREATE_PERMISSION],
    )?;
    let evaluation_review_response = client
        .post(&review_url)
        .bearer_auth(&evaluation_token)
        .send()
        .await?;
    assert_eq!(evaluation_review_response.status(), reqwest::StatusCode::OK);
    assert_eq!(evaluation_review_response.text().await?, "review");
    let evaluation_control_response = client
        .post(&control_url)
        .bearer_auth(&evaluation_token)
        .send()
        .await?;
    assert_eq!(
        evaluation_control_response.status(),
        reqwest::StatusCode::FORBIDDEN
    );

    let _ = shutdown_tx.send(());
    task.await?;
    Ok(())
}

async fn protected() -> &'static str {
    "protected"
}

async fn control_route() -> &'static str {
    "control"
}

async fn review_route() -> &'static str {
    "review"
}

fn signing_material(kid: &str) -> Result<SigningMaterial, rcgen::Error> {
    let key = KeyPair::generate()?;
    let public = key.public_key_raw();
    assert_eq!(public.len(), 65);
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
    permissions: &[&str],
) -> Result<String, jsonwebtoken::errors::Error> {
    let mut header = Header::new(Algorithm::ES256);
    header.kid = Some(material.kid.clone());
    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    let claims = json!({
        "iss": issuer,
        "sub": "service-account",
        "azp": client_id,
        "aud": AUDIENCE,
        "exp": now + 300,
        "nbf": now - 1,
        "resource_access": {
            AUDIENCE: {"roles": permissions}
        }
    });
    encode(
        &header,
        &claims,
        &EncodingKey::from_ec_der(&material.private_der),
    )
}

async fn spawn_authority(
    key: &SigningMaterial,
) -> Result<AuthorityHandle, Box<dyn std::error::Error>> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let issuer = format!("http://localhost:{}/realms/test", address.port());
    let state = AuthorityState {
        issuer: issuer.clone(),
        jwk: key.jwk.clone(),
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
        key: SigningMaterial {
            kid: key.kid.clone(),
            private_der: key.private_der.clone(),
            jwk: key.jwk.clone(),
        },
        shutdown: Some(shutdown_tx),
        task,
    })
}

async fn discovery(
    axum::extract::State(state): axum::extract::State<AuthorityState>,
) -> axum::Json<Value> {
    axum::Json(json!({
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

async fn jwks(
    axum::extract::State(state): axum::extract::State<AuthorityState>,
) -> axum::Json<Value> {
    axum::Json(json!({"keys": [state.jwk]}))
}
