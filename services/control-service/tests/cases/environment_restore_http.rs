//! Signed service JWTs traverse real Resource and Environment routers; only external identity/NATS boundaries are fixtures.
use super::*;
use async_trait::async_trait;
use axum::{
    Json, Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
    routing::get,
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use contracts::environment::{
    DesiredEnvironmentState, EnvironmentInstance, EnvironmentLeaseAuthorization,
    EnvironmentOperationKind, EnvironmentRuntimeAuthorizationResolution, OperationState,
    ResolveEnvironmentRuntimeAuthorizationRequest,
};
use environment_service::{
    EnvironmentRuntimeAdmission, EnvironmentRuntimeAuthority, PgEnvironmentStore,
    ResourceUsageClientError,
};
use std::{collections::BTreeSet, sync::Arc, time::Duration};
use tower::ServiceExt;

struct Jwt {
    resource: Arc<auth::ServiceTokenVerifier>,
    environment: Arc<auth::ServiceTokenVerifier>,
    funding_token: String,
    resolve_token: String,
    foreign_token: String,
    access_token: String,
    server: tokio::task::JoinHandle<()>,
}
impl Drop for Jwt {
    fn drop(&mut self) {
        self.server.abort();
    }
}
impl Jwt {
    async fn new() -> TestResult<Self> {
        let key = rcgen::KeyPair::generate()?;
        let public = key.public_key_raw();
        let jwk = serde_json::json!({"kty":"EC","crv":"P-256","use":"sig","alg":"ES256","kid":"restore-jwt",
            "x":URL_SAFE_NO_PAD.encode(&public[1..33]),"y":URL_SAFE_NO_PAD.encode(&public[33..65])});
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let issuer = format!(
            "http://localhost:{}/realms/restore",
            listener.local_addr()?.port()
        );
        let discovery = serde_json::json!({"issuer":issuer,"jwks_uri":format!("{issuer}/jwks"),"authorization_endpoint":format!("{issuer}/authorize"),"token_endpoint":format!("{issuer}/token"),"response_types_supported":["code"],"subject_types_supported":["public"],"id_token_signing_alg_values_supported":["ES256"],"grant_types_supported":["client_credentials"]});
        let router = Router::new()
            .route(
                "/realms/restore/.well-known/openid-configuration",
                get(move || async move { Json(discovery) }),
            )
            .route(
                "/realms/restore/jwks",
                get(move || async move { Json(serde_json::json!({"keys":[jwk]})) }),
            );
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        let mut verifiers = Vec::new();
        for audience in ["labweaver-resource", "labweaver-environment"] {
            verifiers.push(Arc::new(
                auth::ServiceTokenVerifier::discover(
                    auth::ServiceAuthConfig::new(
                        &issuer,
                        audience.into(),
                        BTreeSet::from([
                            OWNER.into(),
                            "synthetic-access".into(),
                            "synthetic-foreign".into(),
                        ]),
                        BTreeSet::new(),
                        BTreeSet::from(["ES256".into()]),
                        3600,
                        1,
                        auth::TransportSecurityMode::InsecureTestOnly,
                    )?,
                    auth::no_redirect_http_client(
                        None,
                        auth::TransportSecurityMode::InsecureTestOnly,
                    )?,
                )
                .await?,
            ));
        }
        let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::ES256);
        header.kid = Some("restore-jwt".into());
        let now = time::OffsetDateTime::now_utc().unix_timestamp();
        let sign = |audience: &str, client: &str, scopes: &[&str]| -> TestResult<String> {
            Ok(jsonwebtoken::encode(
                &header,
                &serde_json::json!({"iss":issuer,"sub":format!("service-account-{client}"),"azp":client,"aud":audience,"exp":now+300,"nbf":now-1,"resource_access":{audience:{"roles":scopes}}}),
                &jsonwebtoken::EncodingKey::from_ec_der(&key.serialize_der()),
            )?)
        };
        Ok(Self {
            resource: verifiers.remove(0),
            environment: verifiers.remove(0),
            funding_token: sign(
                "labweaver-resource",
                OWNER,
                &["resource.credits.hold", "resource.environment.resolve"],
            )?,
            resolve_token: sign(
                "labweaver-resource",
                OWNER,
                &["resource.environment.resolve"],
            )?,
            foreign_token: sign(
                "labweaver-resource",
                "synthetic-foreign",
                &["resource.credits.hold"],
            )?,
            access_token: sign(
                "labweaver-environment",
                "synthetic-access",
                &["access.environment.forward"],
            )?,
            server,
        })
    }
}
async fn send(
    router: &Router,
    path: &str,
    token: &str,
    key: Option<&str>,
    body: Option<serde_json::Value>,
) -> TestResult<(StatusCode, serde_json::Value)> {
    let mut request = Request::builder()
        .method(if body.is_some() { "POST" } else { "GET" })
        .uri(path)
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json");
    if let Some(key) = key {
        request = request.header("idempotency-key", key);
    }
    let mut request = request.body(Body::from(
        body.map_or_else(String::new, |value| value.to_string()),
    ))?;
    request
        .extensions_mut()
        .insert(telemetry::RequestContext::generate());
    let response = router.clone().oneshot(request).await?;
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1_048_576).await?;
    Ok((status, serde_json::from_slice(&bytes)?))
}
#[derive(Clone)]
struct HttpResource {
    router: Router,
    token: String,
}
impl HttpResource {
    async fn call<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        key: Option<&str>,
        body: Option<serde_json::Value>,
    ) -> Result<T, ResourceUsageClientError> {
        let (status, body) = send(&self.router, path, &self.token, key, body)
            .await
            .map_err(|_| ResourceUsageClientError::InvalidResponse)?;
        if !status.is_success() {
            return Err(ResourceUsageClientError::Problem {
                diagnostic_code: body["diagnosticCode"]
                    .as_str()
                    .ok_or(ResourceUsageClientError::InvalidResponse)?
                    .into(),
                retryable: false,
            });
        }
        serde_json::from_value(body).map_err(|_| ResourceUsageClientError::InvalidResponse)
    }
}
#[async_trait]
impl EnvironmentRuntimeAuthority for HttpResource {
    async fn restore_funding(
        &self,
        input: &RestoreEnvironmentFundingRequest,
    ) -> Result<EnvironmentCreditGrant, ResourceUsageClientError> {
        self.call(
            "/internal/v1/environment-restore-funding",
            Some(&format!(
                "restore-{}-{}",
                input.lease_id, input.restored_generation
            )),
            Some(
                serde_json::to_value(input)
                    .map_err(|_| ResourceUsageClientError::InvalidResponse)?,
            ),
        )
        .await
    }
    async fn resolve_runtime_authorization(
        &self,
        input: &ResolveEnvironmentRuntimeAuthorizationRequest,
    ) -> Result<EnvironmentRuntimeAuthorizationResolution, ResourceUsageClientError> {
        self.call(
            "/internal/v1/environment-runtime-authorization",
            None,
            Some(
                serde_json::to_value(input)
                    .map_err(|_| ResourceUsageClientError::InvalidResponse)?,
            ),
        )
        .await
    }
    async fn resolve_credit_grant(
        &self,
        lease: &EnvironmentLeaseAuthorization,
    ) -> Result<EnvironmentCreditGrant, ResourceUsageClientError> {
        self.call(
            &format!("/internal/v1/environment-credit-grants/{}", lease.lease_id),
            None,
            None,
        )
        .await
    }
}
fn resource_router(f: &Fixture, jwt: &Jwt) -> Router {
    resource_service::api::resource_api_router(
        resource_service::api::ResourceApiState::new(f.store.clone())
            .with_service_verifier(jwt.resource.clone())
            .with_environment_service_client_id(OWNER.into()),
    )
}

#[tokio::test]
async fn restore_funding_service_jwt_scope_owner_and_idempotency_are_enforced() -> TestResult {
    let f = Box::pin(Fixture::new()).await?;
    let jwt = Jwt::new().await?;
    let router = resource_router(&f, &jwt);
    let body = serde_json::to_value(&f.input)?;
    for token in [&jwt.resolve_token, &jwt.foreign_token] {
        let response = send(
            &router,
            "/internal/v1/environment-restore-funding",
            token,
            Some("restore-http"),
            Some(body.clone()),
        )
        .await?;
        assert_eq!(response.0, StatusCode::FORBIDDEN, "{}", response.1);
        assert_eq!(f.holds().await?, 1);
    }
    let missing = send(
        &router,
        "/internal/v1/environment-restore-funding",
        &jwt.funding_token,
        None,
        Some(body.clone()),
    )
    .await?;
    assert_eq!(missing.0, StatusCode::BAD_REQUEST, "{}", missing.1);
    let first = send(
        &router,
        "/internal/v1/environment-restore-funding",
        &jwt.funding_token,
        Some("restore-http"),
        Some(body.clone()),
    )
    .await?;
    assert_eq!(first.0, StatusCode::OK, "{}", first.1);
    assert_eq!(
        first,
        send(
            &router,
            "/internal/v1/environment-restore-funding",
            &jwt.funding_token,
            Some("restore-http"),
            Some(body)
        )
        .await?
    );
    assert_eq!(f.holds().await?, 2);
    Ok(())
}

struct NoAgent;
#[async_trait]
impl environment_service::WorkAdmissionResolver for NoAgent {
    async fn resolve(
        &self,
        _id: contracts::AgentRunId,
        _query: &contracts::http::WorkConfigurationAdmissionQuery,
        _now: UtcTimestamp,
    ) -> Result<
        contracts::http::WorkConfigurationAdmissionBinding,
        environment_service::WorkAdmissionClientError,
    > {
        Err(environment_service::WorkAdmissionClientError::Configuration)
    }
}

async fn retained_environment(f: &Fixture) -> TestResult<EnvironmentInstance> {
    let store = PgEnvironmentStore::new(f.roles.environment.clone());
    let now = store.current_time().await?;
    let value = resource_service::runtime_authorization::current_environment_authorization(
        &f.roles.resource,
        f.request.id,
        Some(f.input.lease_id),
    )
    .await?;
    let runtime = EnvironmentRuntimeAuthorizationResolution {
        default_container_uid: value
            .applied_configuration
            .configuration
            .runtime_policy
            .default_container_uid,
        valid_until: value.lease.expires_at.min(value.approval_valid_until),
        provider_binding: value.provider_binding,
        owner_actor_id: value.lease.owner_actor_id,
        authorization: value.binding,
        lease_authorization: value.lease,
    };
    let command = environment_service::LifecycleCommand {
        environment_id: f.input.environment_id,
        kind: EnvironmentOperationKind::Create,
        expected_revision: Revision::new(1)?,
        actor_id: f.request.requester_id,
        trace_id: "restore-http".into(),
        accepted_at: now,
        deadline_at: UtcTimestamp::from_utc(now.get() + time::Duration::minutes(5))?,
        access_revocation_revision: None,
        preserve_mutable_disk: false,
        max_attempts: 3,
        reset_target: None,
    };
    store
        .accept_api_command(
            "restore-environment-create",
            &command,
            Some(&contracts::environment::EnvironmentCreateSpec {
                project_id: f.request.project_id,
                course_id: None,
                owner_actor_id: f.request.requester_id,
                display_label: "restore HTTP environment".into(),
                class: contracts::authoring::EnvironmentClass::Work,
                runtime_kind: RuntimeKind::Container,
                release_id: f.projection.release.id,
                release_version: f.projection.release.version,
                provider_binding: f.approval.provider_binding.clone(),
                runtime_authorization: Some(runtime.clone()),
                lease_id: Some(f.input.lease_id),
                capacity_binding: Some(f.item.claim.id.to_string()),
                approved_resources: f.request.requested_resources.clone(),
                gpu_allocation: None,
                retention: f.projection.environment_spec.retention.clone(),
                eligibility_expires_at: f.projection.environment_spec.retention.retain_until,
            }),
            Some(runtime.lease_authorization),
            f.request.project_id,
            None,
        )
        .await?;
    let mut retained = store.load(f.input.environment_id).await?;
    // Physical cleanup boundary fixture: the same retained disk facts were consumed by Resource.
    retained.revision = f.status.revision;
    retained.generation = f.status.generation;
    retained.observed_generation = retained.generation;
    retained.desired_state = DesiredEnvironmentState::Deleted;
    retained.observed_state = ObservedEnvironmentState::Deleted;
    retained.disk_retention = f.status.disk_retention.clone();
    retained.operation.kind = EnvironmentOperationKind::Delete;
    retained.operation.state = OperationState::Succeeded;
    retained.operation.accepted_revision = retained.revision;
    retained.validate()?;
    sqlx::query("UPDATE environment.environment_instances SET revision=$2,desired_state='deleted',observed_state='deleted',contract=$3 WHERE environment_id=$1")
        .bind(retained.id.as_uuid()).bind(i64::try_from(retained.revision.get())?).bind(serde_json::to_value(&retained)?).execute(&f.roles.environment).await?;
    Ok(retained)
}

#[tokio::test]
async fn restore_funding_environment_recover_rejection_stays_retained_and_retry_starts_once()
-> TestResult {
    use futures_util::StreamExt as _;
    use testcontainers::{
        core::{IntoContainerPort, WaitFor},
        runners::AsyncRunner,
    };
    let f = Box::pin(Fixture::new()).await?;
    let retained = retained_environment(&f).await?;
    let jwt = Jwt::new().await?;
    let store = PgEnvironmentStore::new(f.roles.environment.clone());
    let broker = testcontainers::GenericImage::new("nats", "2.11.8-alpine")
        .with_exposed_port(4222.tcp())
        .with_wait_for(WaitFor::message_on_stderr("Server is ready"))
        .start()
        .await?;
    let nats = async_nats::connect(format!(
        "nats://127.0.0.1:{}",
        broker.get_host_port_ipv4(4222).await?
    ))
    .await?;
    let mut requests = nats
        .subscribe(contracts::access::ACCESS_REVOCATION_SUBJECT)
        .await?;
    nats.flush().await?;
    let responder = nats.clone();
    let revocations = tokio::spawn(async move {
        while let Some(message) = requests.next().await {
            let input: contracts::access::AccessRevocationRequest =
                serde_json::from_slice(&message.payload)?;
            let response = contracts::access::AccessRevocationResponse {
                version: 1,
                environment_id: input.environment_id,
                environment_revision: input.environment_revision,
                access_revocation_revision: input.environment_revision,
            };
            responder
                .publish(
                    message.reply.ok_or("reply")?,
                    serde_json::to_vec(&response)?.into(),
                )
                .await?;
        }
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    });
    let releases = environment_service::PgReleaseProjectionStore::new(f.roles.environment.clone());
    let event = contracts::events::EVENT_CONTRACTS
        .iter()
        .find(|event| {
            event.subject == contracts::events::subjects::ENVIRONMENT_TEMPLATE_RELEASE_PUBLISHED
        })
        .ok_or("release event")?;
    releases
        .accept(
            "restore-http-release",
            &contracts::events::CloudEvent {
                specversion: contracts::events::SPEC_VERSION.into(),
                id: EventId::new(),
                source: event.source().into(),
                event_type: event.event_type.into(),
                subject: event.subject.into(),
                time: f.projection.release.published_at,
                datacontenttype: "application/json".into(),
                dataschema: event.data_schema(),
                project_id: f.request.project_id,
                course_id: None,
                aggregate_revision: Revision::new(f.projection.release.version)?,
                aggregate_sequence: contracts::Sequence(1),
                trace_id: "restore-http".into(),
                data: f.projection.clone(),
            },
        )
        .await?;
    let runtime = EnvironmentRuntimeAdmission::new(
        releases.clone(),
        Arc::new(HttpResource {
            router: resource_router(&f, &jwt),
            token: jwt.funding_token.clone(),
        }),
    );
    let freeze = environment_service::FreezeBindingService::new_with_admission_resolver(
        f.roles.environment.clone(),
        releases.clone(),
        environment_service::FreezeBindingConfiguration {
            container_workspace_storage_class: "synthetic-storage".into(),
            vm: None,
        },
        NoAgent,
    )?;
    let api = environment_service::with_service_auth(
        environment_service::environment_api_router(
            environment_service::EnvironmentApiState::new(
                store.clone(),
                releases,
                environment_service::NatsAccessRevoker::new(
                    contracts::access::ACCESS_REVOCATION_SUBJECT.into(),
                    nats.clone(),
                    Duration::from_secs(2),
                )?,
                environment_service::NatsResourceLeaseVerifier::new(
                    "unused-restore-lease".into(),
                    nats,
                    Duration::from_secs(2),
                )?,
                freeze,
            )
            .with_runtime_admission(runtime.clone()),
        ),
        jwt.environment.clone(),
    );
    let recover = |revision: Revision| {
        let api = api.clone();
        let token = jwt.access_token.clone();
        let actor = f.request.requester_id;
        let id = retained.id;
        async move {
            let mut request = Request::builder()
                .method("POST")
                .uri(format!("/api/v1/environments/{id}/recover"))
                .header("authorization", format!("Bearer {token}"))
                .header("x-labweaver-actor-id", actor.to_string())
                .header("x-labweaver-session-id", Uuid::now_v7().to_string())
                .header("idempotency-key", "restore-http-recover")
                .header("if-match", format!("\"{}\"", revision.get()))
                .body(Body::empty())?;
            request
                .extensions_mut()
                .insert(telemetry::RequestContext::generate());
            let response = api.oneshot(request).await?;
            Ok::<_, Box<dyn std::error::Error>>((
                response.status(),
                serde_json::from_slice::<serde_json::Value>(
                    &to_bytes(response.into_body(), 1_048_576).await?,
                )?,
            ))
        }
    };
    let mut expired = f.approval.clone();
    expired.valid_until =
        UtcTimestamp::from_utc(expired.created_at.get() + time::Duration::milliseconds(1))?;
    sqlx::query(
        "UPDATE resource.resource_approvals SET valid_until=$2,contract=$3 WHERE approval_id=$1",
    )
    .bind(expired.id.as_uuid())
    .bind(expired.valid_until.get())
    .bind(serde_json::to_value(expired)?)
    .execute(&f.roles.provisioner)
    .await?;
    let rejected = recover(retained.revision).await?;
    assert_eq!(rejected.0, StatusCode::CONFLICT, "{}", rejected.1);
    assert_eq!(rejected.1["diagnosticCode"], "LW_RESOURCE_APPROVAL_INVALID");
    let stopped = store.load(retained.id).await?;
    assert_eq!(stopped.desired_state, DesiredEnvironmentState::Stopped);
    assert_eq!(stopped.observed_state, ObservedEnvironmentState::Stopped);
    assert_eq!(stopped.generation, f.input.restored_generation);
    assert_eq!(
        stopped.last_diagnostic_code.as_deref(),
        Some("LW_RESOURCE_APPROVAL_INVALID")
    );
    assert_eq!(
        stopped.operation.state,
        OperationState::Succeeded,
        "recover must not enqueue start/provision"
    );
    assert!(
        store
            .claim_due("restore-no-effects", Duration::from_secs(5))
            .await?
            .is_none(),
        "funding rejection must not schedule any Kubernetes work"
    );
    assert_eq!(f.holds().await?, 1);
    sqlx::query(
        "UPDATE resource.resource_approvals SET valid_until=$2,contract=$3 WHERE approval_id=$1",
    )
    .bind(f.approval.id.as_uuid())
    .bind(f.approval.valid_until.get())
    .bind(serde_json::to_value(&f.approval)?)
    .execute(&f.roles.provisioner)
    .await?;
    let accepted = recover(retained.revision).await?;
    assert_eq!(accepted.0, StatusCode::ACCEPTED, "{}", accepted.1);
    assert_eq!(f.holds().await?, 2);
    assert_eq!(accepted, recover(retained.revision).await?);
    let stopped = store.load(retained.id).await?;
    assert!(stopped.last_diagnostic_code.is_none());
    let resolution = runtime.for_instance(&stopped).await?;
    let now = store.current_time().await?;
    let command = environment_service::LifecycleCommand {
        environment_id: stopped.id,
        kind: EnvironmentOperationKind::Start,
        expected_revision: stopped.revision,
        actor_id: stopped.owner_id,
        trace_id: "restore-start".into(),
        accepted_at: now,
        deadline_at: resolution.valid_until,
        access_revocation_revision: stopped.operation.access_revocation_revision,
        preserve_mutable_disk: true,
        max_attempts: 3,
        reset_target: None,
    };
    store
        .accept_api_command(
            "restore-start",
            &command,
            None,
            Some(resolution.lease_authorization),
            stopped.project_id,
            stopped.course_id,
        )
        .await?;
    let starting = store.load(stopped.id).await?;
    assert_eq!(starting.generation, 4);
    assert_eq!(
        starting.observed_state,
        ObservedEnvironmentState::Provisioning
    );
    assert!(
        runtime.for_instance(&starting).await.is_ok(),
        "provider preflight reuses same restore admission"
    );
    assert_eq!(f.holds().await?, 2);
    revocations.abort();
    Ok(())
}
