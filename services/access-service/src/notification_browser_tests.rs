//! Real OIDC browser login, Access proxy and Control notification HTTP routes with `PostgreSQL`.
use super::*;
use contracts::{
    ActorId, EventId, ProjectId, ResourceRequestId, Revision, Sequence, TaskRunId, UtcTimestamp,
};
use control_service::clients::{
    AccessClient, AgentClient, EnvironmentClient, EvaluationClient, ServiceHttpClientConfig,
};

async fn notification_token(
    State(state): State<Authority>,
    headers: HeaderMap,
    Form(form): Form<std::collections::HashMap<String, String>>,
) -> Response {
    let credentials = format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode("access-test:fixture-secret")
    );
    if headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        != Some(credentials.as_str())
        || form.get("grant_type").map(String::as_str) != Some("client_credentials")
    {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let now = OffsetDateTime::now_utc().unix_timestamp();
    let mut header = Header::new(Algorithm::ES256);
    header.kid = Some("fixture".into());
    let claims = json!({"iss":state.issuer,"aud":["labweaver-control","labweaver-access"],"azp":"access-test","sub":"service-account-access","iat":now,"nbf":now-1,"exp":now+300,"resource_access":{"labweaver-control":{"roles":["access.control.forward"]},"labweaver-access":{"roles":["access.authorization.decide"]}}});
    match jsonwebtoken::encode(&header, &claims, &state.signing_key) {
        Ok(token) => Json(json!({"access_token":token,"token_type":"Bearer","expires_in":300}))
            .into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one real browser-to-Access-to-Control transaction proves authentication, CSRF and idempotent reads"
)]
async fn real_access_notification_read_is_csrf_protected_and_idempotent()
-> Result<(), Box<dyn Error>> {
    let image = std::env::var("LABWEAVER_TEST_MINIO_IMAGE")?;
    let (name, tag) = image
        .rsplit_once(':')
        .ok_or("MinIO test image tag required")?;
    let minio = GenericImage::new(name, tag)
        .with_exposed_port(9000.tcp())
        .with_wait_for(WaitFor::message_on_stderr("API:"))
        .with_env_var("MINIO_ROOT_USER", "labweaver-test")
        .with_env_var("MINIO_ROOT_PASSWORD", "labweaver-test-secret")
        .with_cmd(["server", "/data"])
        .start()
        .await?;
    let objects = Arc::new(
        artifact_store::S3ImmutableObjectStore::new_for_test(
            artifact_store::S3StoreConfig {
                binding: "notification-test".into(),
                endpoint: format!("http://127.0.0.1:{}", minio.get_host_port_ipv4(9000).await?)
                    .parse()?,
                bucket: "notifications".into(),
                region: "us-east-1".into(),
                object_prefix: "materials".into(),
                upload_ttl_seconds: 300,
                max_object_bytes: 1024,
                force_path_style: true,
                ca_bundle_file: None,
            },
            artifact_store::S3Credential {
                access_key_id: "labweaver-test".into(),
                secret_access_key: "labweaver-test-secret".into(),
                session_token: None,
            },
        )
        .await?,
    );
    objects.ensure_bucket_for_test().await?;
    let runtime = tempfile::tempdir()?;
    let ca_path = runtime.path().join("service-ca.pem");
    let fixture=real_oidc_fixture_with("localhost",|_|Ok(()),async move |app,_,tasks| {
        let tls=rcgen::generate_simple_self_signed(vec!["127.0.0.1".into()])?;
        let ca=tls.cert.pem();std::fs::write(&ca_path,&ca)?;
        let server_tls=http_transport::server_config(ca.as_bytes(),tls.signing_key.serialize_pem().as_bytes())?;
        let listener=TcpListener::bind("127.0.0.1:0").await?;
        let issuer=format!("https://{}/realms/test",listener.local_addr()?);
        let key=KeyPair::generate()?;let public=key.public_key_raw();let x=URL_SAFE_NO_PAD.encode(&public[1..33]);let y=URL_SAFE_NO_PAD.encode(&public[33..65]);
        let authority=Authority {issuer:issuer.clone(),signing_key:Arc::new(EncodingKey::from_ec_der(&key.serialize_der())),decoding_key:DecodingKey::from_ec_components(&x,&y)?,jwk:json!({"kty":"EC","crv":"P-256","x":x,"y":y,"use":"sig","alg":"ES256","kid":"fixture"}),admin:Uuid::now_v7(),requests:Arc::new(Mutex::new(Vec::new())),upstream:"control",environment:None};
        let authority_router=Router::new().route("/realms/test/.well-known/openid-configuration",get(discovery)).route("/realms/test/jwks",get(jwks)).route("/realms/test/token",post(notification_token)).with_state(authority);
        let authority_tls=Arc::clone(&server_tls);
        tasks.0.push(tokio::spawn(async move {let _=http_transport::serve_tls(listener,authority_router,authority_tls).await;}));
        let http=no_redirect_http_client(Some(ca.as_bytes()),TransportSecurityMode::Strict)?;
        let tokens=Arc::new(ServiceTokenClient::discover(ServiceTokenClientConfig::new(&issuer,"access-test".into(),"fixture-secret".into(),"labweaver-control".into(),BTreeSet::from(["access.control.forward".into()]),30,TransportSecurityMode::Strict)?,http.clone()).await?);
        app.service_token_verifier=Arc::new(ServiceTokenVerifier::discover(ServiceAuthConfig::new(&issuer,"labweaver-access".into(),BTreeSet::from(["access-test".into()]),BTreeSet::new(),BTreeSet::from(["ES256".into()]),600,1,TransportSecurityMode::Strict)?,http.clone()).await?);
        let internal_listener=TcpListener::bind("127.0.0.1:0").await?;
        let internal_base=format!("https://{}/",internal_listener.local_addr()?);
        let internal=internal_router(Arc::new(app.clone()));let internal_tls=Arc::clone(&server_tls);
        tasks.0.push(tokio::spawn(async move {let _=http_transport::serve_tls(internal_listener,internal,internal_tls).await;}));
        let control_listener=TcpListener::bind("127.0.0.1:0").await?;
        let control_base=format!("https://{}/",control_listener.local_addr()?);
        let client_config=|base:&str| -> Result<ServiceHttpClientConfig,Box<dyn Error>> { Ok(ServiceHttpClientConfig {base_url:base.parse()?,ca_certificate_file:ca_path.to_string_lossy().into_owned(),timeout_milliseconds:5000}) };
        let yaml:serde_yaml::Value=serde_yaml::from_str(include_str!("../../../deploy/config/control-plane.yaml.example"))?;
        let control=control_service::ControlService::new(app.pool.clone(),objects,serde_yaml::from_value(yaml["control"].clone())?)?;
        let verifier=Arc::new(ServiceTokenVerifier::discover(ServiceAuthConfig::new(&issuer,"labweaver-control".into(),BTreeSet::from(["access-test".into()]),BTreeSet::new(),BTreeSet::from(["ES256".into()]),600,1,TransportSecurityMode::Strict)?,http).await?);
        let state=Arc::new(control_service::api::ApiState {control,access:AccessClient::new_authenticated(client_config(&internal_base)?,Arc::clone(&tokens))?,agent:AgentClient::new_authenticated(client_config(&control_base)?,Arc::clone(&tokens))?,environment:EnvironmentClient::new_authenticated(client_config(&control_base)?,Arc::clone(&tokens))?,evaluation:EvaluationClient::new_authenticated(client_config(&control_base)?,Arc::clone(&tokens))?,service_token_verifier:Arc::clone(&verifier)});
        let router=control_service::api::authenticated_router(&state,&verifier);
        tasks.0.push(tokio::spawn(async move {let _=http_transport::serve_tls(control_listener,router,server_tls).await;}));
        let mut config=app.deployment.control_gateway.clone();config.base_uri=control_base;config.allowed_server_sans=vec!["127.0.0.1".into()];
        app.control_proxy=proxy::ControlGatewayProxy::new(&config,ca.as_bytes(),TransportSecurityMode::Strict,tokens,ServiceTokenTarget {audience:"labweaver-control".into(),scopes:BTreeSet::from(["access.control.forward".into()])})?;
        Ok(())
    },Router::new()).await?;
    let browser = browser_client(&fixture.portal_ca, &fixture.provider_ca)?;
    let login = browser
        .get(format!("{}/auth/login", fixture.base))
        .send()
        .await?;
    let callback = provider_callback(
        &browser,
        login
            .headers()
            .get(header::LOCATION)
            .ok_or("authorization URL")?
            .to_str()?,
        &fixture.callback_uri,
    )
    .await?;
    assert_eq!(
        browser.get(callback).send().await?.status(),
        StatusCode::TEMPORARY_REDIRECT
    );
    let session: Value = browser
        .get(format!("{}/api/v1/auth/session", fixture.base))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let actor: ActorId = session["actor"]["actorId"]
        .as_str()
        .ok_or("real actor")?
        .parse()?;
    let project = ProjectId::new();
    let request_id = ResourceRequestId::new();
    let now = UtcTimestamp::from_utc(OffsetDateTime::now_utc().replace_nanosecond(0)?)?;
    sqlx::query("INSERT INTO control.project_memberships(project_id,actor_id,role,object_role,state,revision) VALUES($1,$2,'teacher','owner','active',1)").bind(project.as_uuid()).bind(actor.as_uuid()).execute(&fixture.app.pool).await?;
    let event = contracts::events::CloudEvent {
        specversion: "1.0".into(),
        id: EventId::new(),
        source: "urn:labweaver:resource-service".into(),
        event_type: contracts::events::subjects::RESOURCE_REQUEST_SUBMITTED.into(),
        subject: contracts::events::subjects::RESOURCE_REQUEST_SUBMITTED.into(),
        time: now,
        datacontenttype: "application/json".into(),
        dataschema: format!(
            "{}/resource-request-submitted.schema.json",
            contracts::events::DATA_SCHEMA_BASE
        ),
        project_id: project,
        course_id: None,
        aggregate_revision: Revision::new(1)?,
        aggregate_sequence: Sequence(1),
        trace_id: "notification-browser".into(),
        data: contracts::events::ResourceRequestChanged {
            request: contracts::resource::ResourceRequest {
                id: request_id,
                generation: 1,
                request_key: "browser-notification".into(),
                requester_id: actor,
                project_id: project,
                course_id: None,
                target: contracts::resource::ResourceTarget::Task {
                    task_run_id: TaskRunId::new(),
                },
                requested_resources: contracts::resource::WorkloadResources {
                    cpu_millicores: 100,
                    memory_bytes: 1024,
                    storage_bytes: 1024,
                    gpu: None,
                },
                requested_duration_seconds: 300,
                state: contracts::resource::ResourceRequestState::Reviewing,
                revision: Revision::new(1)?,
                created_at: now,
                updated_at: now,
                diagnostic_code: None,
            },
        },
    };
    control_service::notifications::project(
        &fixture.app.pool,
        &serde_json::to_vec(&event)?,
        &[3600],
        false,
    )
    .await?;
    let count_url = format!("{}/api/v1/notifications/unread-count", fixture.base);
    let count: Value = browser
        .get(&count_url)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(count["unreadCount"], 1);
    let page: Value = browser
        .get(format!("{}/api/v1/notifications", fixture.base))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let id = page["items"][0]["id"].as_str().ok_or("notification id")?;
    let read_url = format!("{}/api/v1/notifications/{id}/read", fixture.base);
    let bad = browser
        .post(&read_url)
        .header(header::ORIGIN, &fixture.base)
        .header("x-csrf-token", "invalid")
        .send()
        .await?;
    assert_eq!(bad.status(), StatusCode::FORBIDDEN);
    let count: Value = browser
        .get(&count_url)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(count["unreadCount"], 1);
    let csrf: Value = browser
        .get(format!("{}/api/v1/auth/csrf", fixture.base))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let csrf = csrf["csrfToken"].as_str().ok_or("CSRF token")?;
    let first: Value = browser
        .post(&read_url)
        .header(header::ORIGIN, &fixture.base)
        .header("x-csrf-token", csrf)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let repeated: Value = browser
        .post(&read_url)
        .header(header::ORIGIN, &fixture.base)
        .header("x-csrf-token", csrf)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert!(first["readAt"].is_string());
    assert_eq!(first["readAt"], repeated["readAt"]);
    let count: Value = browser
        .get(&count_url)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(count["unreadCount"], 0);
    Ok(())
}
