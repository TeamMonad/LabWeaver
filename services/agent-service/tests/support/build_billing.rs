//! Real Resource `SQLx` authority and signed service JWT over TLS; only the issuer is synthetic.
#![allow(
    dead_code,
    clippy::too_many_lines,
    clippy::expect_used,
    reason = "isolated cryptographic integration fixture"
)]
use auth::{
    ServiceAuthConfig, ServiceTokenClient, ServiceTokenClientConfig, ServiceTokenVerifier,
    TransportSecurityMode,
};
use axum::{
    Json, Router,
    extract::State,
    routing::{get, post},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::{collections::BTreeSet, sync::Arc};
use task_execution::resource::{ResourceClient, ResourceClientConfiguration};
const CLIENT: &str = "synthetic-build-agent";
const AUDIENCE: &str = "labweaver-resource";
#[derive(Clone)]
struct TokenAuthority {
    issuer: String,
    jwk: Value,
    token: String,
}
async fn discovery(State(s): State<TokenAuthority>) -> Json<Value> {
    Json(
        json!({"issuer":s.issuer,"authorization_endpoint":format!("{}/authorize",s.issuer),"token_endpoint":format!("{}/token",s.issuer),"jwks_uri":format!("{}/jwks",s.issuer),"response_types_supported":["code"],"subject_types_supported":["public"],"id_token_signing_alg_values_supported":["ES256"],"grant_types_supported":["client_credentials"]}),
    )
}
async fn jwks(State(s): State<TokenAuthority>) -> Json<Value> {
    Json(json!({"keys":[s.jwk]}))
}
async fn token(State(s): State<TokenAuthority>) -> Json<Value> {
    Json(json!({"access_token":s.token,"token_type":"Bearer","expires_in":300}))
}
struct TlsListener {
    listener: tokio::net::TcpListener,
    acceptor: tokio_rustls::TlsAcceptor,
}
impl axum::serve::Listener for TlsListener {
    type Io = tokio_rustls::server::TlsStream<tokio::net::TcpStream>;
    type Addr = std::net::SocketAddr;
    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            let (stream, addr) = self.listener.accept().await.expect("fixture TLS");
            if let Ok(io) = self.acceptor.accept(stream).await {
                return (io, addr);
            }
        }
    }
    fn local_addr(&self) -> std::io::Result<Self::Addr> {
        self.listener.local_addr()
    }
}
pub struct BuildBillingFixture {
    pub client: ResourceClient,
    pub store: resource_service::store::PgResourceStore,
    pub credits: resource_service::credits::PgCreditStore,
    pool: PgPool,
    _directory: tempfile::TempDir,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}
impl Drop for BuildBillingFixture {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}
impl BuildBillingFixture {
    pub async fn start(pool: PgPool) -> Result<Self, Box<dyn std::error::Error>> {
        let existing: bool = sqlx::query_scalar("SELECT to_regnamespace('resource') IS NOT NULL")
            .fetch_one(&pool)
            .await?;
        if !existing {
            let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../migrations");
            let catalog = persistence_sqlx::MigrationCatalog::load(&root.join("catalog.yaml"))?;
            let domain = catalog
                .domains
                .iter()
                .find(|d| d.name == persistence_sqlx::Domain::Resource)
                .ok_or("resource migration catalog")?;
            let mut ddl = "CREATE SCHEMA resource;SET search_path TO resource;\n".to_owned();
            for migration in &domain.migrations {
                ddl.push_str(&persistence_sqlx::MigrationCatalog::read_verified_sql(
                    &root, migration,
                )?);
                ddl.push('\n');
            }
            sqlx::raw_sql(sqlx::AssertSqlSafe(&*ddl))
                .execute(&pool)
                .await?;
        }
        let signing = rcgen::KeyPair::generate()?;
        let public = signing.public_key_raw();
        let kid = "synthetic-build-key";
        let jwk = json!({"kty":"EC","crv":"P-256","x":URL_SAFE_NO_PAD.encode(&public[1..33]),"y":URL_SAFE_NO_PAD.encode(&public[33..65]),"use":"sig","alg":"ES256","kid":kid});
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let issuer = format!(
            "http://localhost:{}/realms/synthetic",
            listener.local_addr()?.port()
        );
        let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::ES256);
        header.kid = Some(kid.into());
        let token_value = jsonwebtoken::encode(
            &header,
            &json!({"iss":issuer,"sub":"synthetic-service-account","azp":CLIENT,"aud":AUDIENCE,"exp":time::OffsetDateTime::now_utc().unix_timestamp()+3600,"nbf":time::OffsetDateTime::now_utc().unix_timestamp()-1,"resource_access":{(AUDIENCE):{"roles":["resource.credits.hold","resource.usage.record"]}}}),
            &jsonwebtoken::EncodingKey::from_ec_der(&signing.serialize_der()),
        )?;
        let app = Router::new()
            .route(
                "/realms/synthetic/.well-known/openid-configuration",
                get(discovery),
            )
            .route("/realms/synthetic/jwks", get(jwks))
            .route("/realms/synthetic/token", post(token))
            .with_state(TokenAuthority {
                issuer: issuer.clone(),
                jwk,
                token: token_value,
            });
        let issuer_task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let verifier = ServiceTokenVerifier::discover(
            ServiceAuthConfig::new(
                &issuer,
                AUDIENCE.into(),
                BTreeSet::from([CLIENT.into()]),
                BTreeSet::new(),
                BTreeSet::from(["ES256".into()]),
                3600,
                1,
                TransportSecurityMode::InsecureTestOnly,
            )?,
            auth::no_redirect_http_client(None, TransportSecurityMode::InsecureTestOnly)?,
        )
        .await?;
        let scopes = BTreeSet::from([
            "resource.credits.hold".into(),
            "resource.usage.record".into(),
        ]);
        let token_client = ServiceTokenClient::discover(
            ServiceTokenClientConfig::new(
                &issuer,
                CLIENT.into(),
                "synthetic-client-secret".into(),
                AUDIENCE.into(),
                scopes.clone(),
                30,
                TransportSecurityMode::InsecureTestOnly,
            )?,
            auth::no_redirect_http_client(None, TransportSecurityMode::InsecureTestOnly)?,
        )
        .await?;
        let tls = rcgen::generate_simple_self_signed(vec!["localhost".into()])?;
        let tls_config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(
                vec![tls.cert.der().clone()],
                rustls::pki_types::PrivateKeyDer::Pkcs8(tls.signing_key.serialize_der().into()),
            )?;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
        let store = resource_service::store::PgResourceStore::new(pool.clone());
        let credits = resource_service::credits::PgCreditStore::new(pool.clone());
        let state = resource_service::api::ResourceApiState::new(store.clone())
            .with_service_verifier(Arc::new(verifier))
            .with_task_service_client_ids(BTreeSet::from([CLIENT.into()]))
            .with_build_service_client_id(Some(CLIENT.into()));
        let app = resource_service::api::resource_api_router(state);
        let server = tokio::spawn(async move {
            let _ = axum::serve(
                TlsListener {
                    listener,
                    acceptor: tokio_rustls::TlsAcceptor::from(Arc::new(tls_config)),
                },
                app,
            )
            .await;
        });
        let directory = tempfile::tempdir()?;
        let ca = directory.path().join("resource-ca.pem");
        std::fs::write(&ca, tls.cert.pem())?;
        let client = ResourceClient::from_configuration(
            ResourceClientConfiguration {
                base_uri: format!("https://localhost:{port}/").parse()?,
                ca_file: ca,
                timeout_milliseconds: 5000,
                max_request_bytes: 1_048_576,
                max_response_bytes: 8_388_608,
                audience: AUDIENCE.into(),
            },
            Arc::new(token_client),
            scopes,
        )?;
        Ok(Self {
            client,
            store,
            credits,
            pool,
            _directory: directory,
            tasks: vec![issuer_task, server],
        })
    }
    pub async fn synthetic_tariff(
        &self,
        builder: &str,
        value: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.store
            .create_rate(
                &uuid::Uuid::new_v4().to_string(),
                &contracts::http::CreateResourceRateRequest {
                    unit: contracts::resource::ResourceBillingUnit::BuildSecond,
                    unit_quantity: 1,
                    gpu_class: None,
                    gpu_mode: None,
                    builder_binding: Some(builder.into()),
                    model_binding: None,
                    max_preauthorization: None,
                    unit_price: contracts::resource::Money {
                        currency: "CREDITS".into(),
                        amount: contracts::resource::FixedDecimal::parse(value)?,
                    },
                    effective_from: contracts::UtcTimestamp::from_utc(
                        self.store.current_time().await?.get() - time::Duration::hours(1),
                    )?,
                    effective_until: None,
                },
            )
            .await?;
        Ok(())
    }
    pub async fn seed_command(
        &self,
        command: &contracts::events::AgentBuildRequested,
        attempt: u32,
        token: uuid::Uuid,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let hash = persistence_sqlx::Sha256Digest::of_canonical(command)?;
        sqlx::query("INSERT INTO agent.build_commands(build_request_id,project_id,course_id,command_sha256,idempotency_key,state,command,attempt,worker_id,lease_token,lease_expires_at) VALUES($1,$2,$3,$4,$5,'running',$6,$7,'synthetic-fence-worker',$8,clock_timestamp()+interval '5 minutes') ON CONFLICT(build_request_id) DO UPDATE SET state='running',attempt=$7,lease_token=$8,lease_expires_at=clock_timestamp()+interval '5 minutes',cancellation_requested=false,completed_at=NULL,diagnostic_code=NULL,retryable=NULL,cleanup_verified=NULL,worker_id=COALESCE(agent.build_commands.worker_id,EXCLUDED.worker_id)")
            .bind(command.request.id.as_uuid()).bind(command.request.project_id.as_uuid()).bind(command.request.course_id.map(contracts::CourseId::as_uuid)).bind(hash.to_string()).bind(&command.idempotency_key).bind(serde_json::to_value(command)?).bind(i32::try_from(attempt)?).bind(token).execute(&self.pool).await?;
        let balance = self.credits.account(command.request.project_id).await?;
        if balance.balance == contracts::resource::FixedDecimal::zero() {
            self.credits
                .admin(
                    &uuid::Uuid::new_v4().to_string(),
                    contracts::ActorId::new(),
                    &[contracts::PlatformRole::PlatformAdmin],
                    &contracts::resource::credits::CreditAdminOperation::Grant {
                        project_id: command.request.project_id,
                        amount: contracts::resource::FixedDecimal::parse("100.000000")?,
                        reason: "isolated fencing regression allowance".into(),
                    },
                )
                .await?;
        }
        Ok(())
    }
}
