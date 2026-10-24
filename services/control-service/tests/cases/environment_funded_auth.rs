//! Cryptographic service authentication fixture; it never constructs a principal.
use axum::{Json, Router, routing::get};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use rcgen::KeyPair;
use serde_json::json;
use std::{collections::BTreeSet, sync::Arc};

type Result<T, E = Box<dyn std::error::Error + Send + Sync>> = std::result::Result<T, E>;

pub(super) struct ServiceAuthority {
    pub verifier: Arc<auth::ServiceTokenVerifier>,
    pub token: String,
    pub access_token: String,
    pub usage_identity: auth::ServiceIdentity,
    server: tokio::task::JoinHandle<()>,
}
impl Drop for ServiceAuthority {
    fn drop(&mut self) {
        self.server.abort();
    }
}

pub(super) async fn start() -> Result<ServiceAuthority> {
    let key = KeyPair::generate()?;
    let public = key.public_key_raw();
    let jwk = json!({"kty":"EC","crv":"P-256","use":"sig","alg":"ES256","kid":"funded-journey",
        "x":URL_SAFE_NO_PAD.encode(&public[1..33]),"y":URL_SAFE_NO_PAD.encode(&public[33..65])});
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let issuer = format!(
        "http://localhost:{}/realms/funded-journey",
        listener.local_addr()?.port()
    );
    let discovery = json!({"issuer":issuer,"jwks_uri":format!("{issuer}/jwks"),
        "authorization_endpoint":format!("{issuer}/authorize"),"token_endpoint":format!("{issuer}/token"),
        "response_types_supported":["code"],"subject_types_supported":["public"],
        "id_token_signing_alg_values_supported":["ES256"],"grant_types_supported":["authorization_code","client_credentials"]});
    let router = Router::new()
        .route(
            "/realms/funded-journey/.well-known/openid-configuration",
            get(move || async move { Json(discovery) }),
        )
        .route(
            "/realms/funded-journey/jwks",
            get(move || async move { Json(json!({"keys":[jwk]})) }),
        );
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    let audience = "labweaver-environment";
    let client = "synthetic-resource";
    let verifier = Arc::new(
        auth::ServiceTokenVerifier::discover(
            auth::ServiceAuthConfig::new(
                &issuer,
                audience.into(),
                BTreeSet::from([client.into(), "synthetic-access".into()]),
                BTreeSet::new(),
                BTreeSet::from(["ES256".into()]),
                3600,
                1,
                auth::TransportSecurityMode::InsecureTestOnly,
            )?,
            auth::no_redirect_http_client(None, auth::TransportSecurityMode::InsecureTestOnly)?,
        )
        .await?,
    );
    let mut header = Header::new(Algorithm::ES256);
    header.kid = Some("funded-journey".into());
    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    let token = encode(
        &header,
        &json!({"iss":issuer,"sub":"service-account","azp":client,"aud":audience,
        "exp":now+300,"nbf":now-1,"resource_access":{audience:{"roles":["resource.environment.manage"]}}}),
        &EncodingKey::from_ec_der(&key.serialize_der()),
    )?;
    let access_token = encode(
        &header,
        &json!({"iss":issuer,"sub":"access-service-account","azp":"synthetic-access","aud":audience,
        "exp":now+300,"nbf":now-1,"resource_access":{audience:{"roles":["access.environment.forward"]}}}),
        &EncodingKey::from_ec_der(&key.serialize_der()),
    )?;
    let resource_audience = "labweaver-resource";
    let meter_client = "synthetic-environment";
    let usage_verifier = auth::ServiceTokenVerifier::discover(
        auth::ServiceAuthConfig::new(
            &issuer,
            resource_audience.into(),
            BTreeSet::from([meter_client.into()]),
            BTreeSet::from(["resource.usage.record".into()]),
            BTreeSet::from(["ES256".into()]),
            3600,
            1,
            auth::TransportSecurityMode::InsecureTestOnly,
        )?,
        auth::no_redirect_http_client(None, auth::TransportSecurityMode::InsecureTestOnly)?,
    )
    .await?;
    let usage_token = encode(
        &header,
        &json!({"iss":issuer,"sub":"environment-meter-account","azp":meter_client,"aud":resource_audience,
        "exp":now+300,"nbf":now-1,"resource_access":{resource_audience:{"roles":["resource.usage.record"]}}}),
        &EncodingKey::from_ec_der(&key.serialize_der()),
    )?;
    let mut usage_headers = axum::http::HeaderMap::new();
    usage_headers.insert("authorization", format!("Bearer {usage_token}").parse()?);
    let usage_identity = usage_verifier.authenticate(&usage_headers).await?;
    Ok(ServiceAuthority {
        verifier,
        token,
        access_token,
        usage_identity,
        server,
    })
}
