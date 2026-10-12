//! Environment consumer journey after the normal Control publication and Resource funded lease.
//! Only service transport/auth and Kubernetes are external synthetic boundaries; the authorities,
//! original approval, hold, aggregate, operation ledger and meter use the real PostgreSQL stores.
#![allow(clippy::too_many_lines)]
use async_trait::async_trait;
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use contracts::environment::{
    EnvironmentLeaseAuthorization, EnvironmentRuntimeAuthorizationResolution,
    ResolveEnvironmentRuntimeAuthorizationRequest, ResourceEnvironmentHandoff,
};
use contracts::events::{CloudEvent, EVENT_CONTRACTS, ReleasePublished, SPEC_VERSION, subjects};
use contracts::resource::environment_funding::EnvironmentCreditGrant;
use contracts::{EventId, Revision, Sequence, UtcTimestamp};
use environment_service::{
    EnvironmentRuntimeAdmission, EnvironmentRuntimeAuthority, PgEnvironmentStore,
    PgReleaseProjectionStore, ResourceUsageClientError,
};
use resource_service::{
    runtime_authorization::CurrentEnvironmentAuthorization, store::PgResourceStore,
};
use sqlx::{PgPool, Row};
use std::{collections::BTreeSet, sync::Arc, time::Duration};
use tower::ServiceExt;
type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;
const OWNER: &str = "synthetic-environment";

#[derive(Clone)]
struct UnusedAdmission;
#[async_trait]
impl environment_service::WorkAdmissionResolver for UnusedAdmission {
    async fn resolve(
        &self,
        _id: contracts::AgentRunId,
        _query: &contracts::http::WorkConfigurationAdmissionQuery,
        _now: UtcTimestamp,
    ) -> std::result::Result<
        contracts::http::WorkConfigurationAdmissionBinding,
        environment_service::WorkAdmissionClientError,
    > {
        Err(environment_service::WorkAdmissionClientError::Configuration)
    }
}

#[derive(Clone)]
struct PgAuthority(PgResourceStore, PgPool);
#[async_trait]
impl EnvironmentRuntimeAuthority for PgAuthority {
    async fn resolve_runtime_authorization(
        &self,
        input: &ResolveEnvironmentRuntimeAuthorizationRequest,
    ) -> std::result::Result<EnvironmentRuntimeAuthorizationResolution, ResourceUsageClientError>
    {
        let value = resource_service::runtime_authorization::current_environment_authorization(
            &self.1,
            input.resource_request_id,
            input.expected_lease_id,
        )
        .await
        .map_err(|_| ResourceUsageClientError::InvalidResponse)?;
        let policy = &value.applied_configuration.configuration.runtime_policy;
        Ok(EnvironmentRuntimeAuthorizationResolution {
            valid_until: value.lease.expires_at.min(value.approval_valid_until),
            provider_binding: value.provider_binding,
            owner_actor_id: value.lease.owner_actor_id,
            default_container_uid: policy.default_container_uid,
            privileged_execution_location: policy.privileged_execution_location.clone(),
            authorization: value.binding,
            lease_authorization: value.lease,
        })
    }
    async fn resolve_credit_grant(
        &self,
        lease: &EnvironmentLeaseAuthorization,
    ) -> std::result::Result<EnvironmentCreditGrant, ResourceUsageClientError> {
        self.0
            .environment_credit_grant(OWNER, lease.lease_id)
            .await
            .map_err(|_| ResourceUsageClientError::InvalidResponse)
    }
}

pub(super) async fn exercise(
    pool: &PgPool,
    resource: &PgResourceStore,
    original: &CurrentEnvironmentAuthorization,
) -> Result {
    super::super::apply_domain_migrations(pool, persistence_sqlx::Domain::Environment).await?;
    let row=sqlx::query("SELECT release_contract,candidate_contract FROM control.runtime_authorization_configurations WHERE release_id=$1")
        .bind(match original.binding.target {contracts::resource::ResourceTarget::Environment {release_id,..}=>release_id.as_uuid(),_=>return Err("environment target".into())}).fetch_one(pool).await?;
    let release: contracts::supply_chain::EnvironmentTemplateRelease =
        serde_json::from_value(row.try_get("release_contract")?)?;
    let candidate: contracts::authoring::EnvironmentCandidate =
        serde_json::from_value(row.try_get("candidate_contract")?)?;
    let projection = ReleasePublished {
        release,
        environment_spec: candidate.spec,
    };
    assert_ne!(
        projection.release.project_id, original.lease.project_id,
        "the normal classroom payer differs from the teacher's authoring project"
    );
    let releases = PgReleaseProjectionStore::new(pool.clone());
    let contract = EVENT_CONTRACTS
        .iter()
        .find(|v| v.subject == subjects::ENVIRONMENT_TEMPLATE_RELEASE_PUBLISHED)
        .ok_or("release event")?;
    releases
        .accept(
            "funded-env-release",
            &CloudEvent {
                specversion: SPEC_VERSION.into(),
                id: EventId::new(),
                source: contract.source().into(),
                event_type: contract.event_type.into(),
                subject: contract.subject.into(),
                time: projection.release.published_at,
                datacontenttype: "application/json".into(),
                dataschema: contract.data_schema(),
                project_id: projection.release.project_id,
                course_id: projection.release.course_id,
                aggregate_revision: Revision::new(1)?,
                aggregate_sequence: Sequence(1),
                trace_id: "funded-environment".into(),
                data: projection.clone(),
            },
        )
        .await?;
    let store = PgEnvironmentStore::new(pool.clone());
    let admission = EnvironmentRuntimeAdmission::new(
        releases.clone(),
        Arc::new(PgAuthority(resource.clone(), pool.clone())),
    );
    let grant = resource
        .environment_credit_grant(OWNER, original.lease.lease_id)
        .await?;
    assert!(grant.can_authorize_execution);
    let claim: contracts::resource::CapacityClaim = serde_json::from_value(
        sqlx::query_scalar::<_, serde_json::Value>(
            "SELECT contract FROM resource.capacity_claims WHERE claim_id=$1",
        )
        .bind(grant.claim_id.as_uuid())
        .fetch_one(pool)
        .await?,
    )?;
    let request = resource.load(grant.request_id).await?;
    let handoff = ResourceEnvironmentHandoff {
        version: 1,
        request_id: request.id,
        request_revision: request.revision,
        lease_id: original.lease.lease_id,
        lease_revision: original.lease.lease_revision,
        claim_id: claim.id,
        claim_revision: claim.revision,
        environment_id: original.lease.environment_id,
        project_id: request.project_id,
        course_id: request.course_id,
        owner_actor_id: request.requester_id,
        display_label: request.request_key.clone(),
        release_id: projection.release.id,
        release_version: projection.release.version,
        provider_binding: claim.provider_binding.clone(),
        capacity_binding: claim.id.to_string(),
        approved_resources: claim.workload_resources.clone(),
        gpu_allocation: claim.gpu_allocation.clone(),
        trace_id: "funded-original-handoff".into(),
    };
    assert_eq!(
        handoff.class_for_release(&projection)?,
        contracts::authoring::EnvironmentClass::Experiment
    );
    use futures_util::StreamExt;
    use testcontainers::core::{IntoContainerPort, WaitFor};
    use testcontainers::{GenericImage, runners::AsyncRunner};
    let broker = GenericImage::new("nats", "2.11.8-alpine")
        .with_exposed_port(4222.tcp())
        .with_wait_for(WaitFor::message_on_stderr("Server is ready"))
        .start()
        .await?;
    let nats = async_nats::connect(format!(
        "nats://127.0.0.1:{}",
        broker.get_host_port_ipv4(4222).await?
    ))
    .await?;
    let mut subscription = nats.subscribe("funded-lease-verify").await?;
    nats.flush().await?;
    let responder = nats.clone();
    let authority = resource.clone();
    let replies = tokio::spawn(async move {
        while let Some(message) = subscription.next().await {
            let Some(reply) = message.reply else { continue };
            let Ok(input) = serde_json::from_slice::<
                contracts::environment::EnvironmentLeaseVerificationRequest,
            >(&message.payload) else {
                continue;
            };
            let Ok(now) = authority.current_time().await else {
                continue;
            };
            let Ok(value) = authority.verify_environment_lease(&input, now).await else {
                continue;
            };
            let Ok(body) = serde_json::to_vec(&value) else {
                continue;
            };
            let _ = responder.publish(reply, body.into()).await;
        }
    });
    let freeze = environment_service::FreezeBindingService::new_with_admission_resolver(
        pool.clone(),
        releases.clone(),
        environment_service::FreezeBindingConfiguration {
            container_workspace_storage_class: "synthetic-storage".into(),
            vm: None,
        },
        UnusedAdmission,
    )?;
    let api = environment_service::environment_api_router(
        environment_service::EnvironmentApiState::new(
            store.clone(),
            releases,
            environment_service::NatsAccessRevoker::new(
                "unused-access-revoke".into(),
                nats.clone(),
                Duration::from_secs(2),
            )?,
            environment_service::NatsResourceLeaseVerifier::new(
                "funded-lease-verify".into(),
                nats,
                Duration::from_secs(2),
            )?,
            freeze,
        )
        .with_runtime_admission(admission.clone()),
    );
    let caller = auth::ServiceIdentity {
        issuer: "synthetic".into(),
        subject: "synthetic-resource".into(),
        client_id: "synthetic-resource".into(),
        expires_at: time::OffsetDateTime::now_utc() + time::Duration::hours(1),
        permissions: BTreeSet::from(["resource.environment.manage".into()]),
    };
    let send = |router: Router, body: serde_json::Value| {
        let caller = caller.clone();
        async move {
            let mut req = Request::builder()
                .method("POST")
                .uri("/internal/v1/resource/work-handoffs")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))?;
            req.extensions_mut().insert(caller);
            req.extensions_mut()
                .insert(telemetry::RequestContext::generate());
            let response = router.oneshot(req).await?;
            let status = response.status();
            let body = to_bytes(response.into_body(), 1_048_576).await?;
            Ok::<_, Box<dyn std::error::Error>>((
                status,
                serde_json::from_slice::<serde_json::Value>(&body)?,
            ))
        }
    };
    let first = send(api.clone(), serde_json::to_value(&handoff)?).await?;
    assert_eq!(first.0, StatusCode::ACCEPTED, "{}", first.1);
    let replay = send(api.clone(), serde_json::to_value(&handoff)?).await?;
    assert_eq!(replay, first);
    let instance = store.load(handoff.environment_id).await?;
    assert_eq!(
        instance.class,
        contracts::authoring::EnvironmentClass::Experiment
    );
    assert!(instance.has_resource_lease());
    assert_eq!(instance.generation, 1);
    assert_eq!(
        instance.operation.lease_authorization.as_ref(),
        Some(&original.lease)
    );
    let public = contracts::http::CreateEnvironmentRequest {
        resource_request_id: request.id,
        project_id: request.project_id,
        course_id: request.course_id,
        release_id: projection.release.id,
        release_version: projection.release.version,
        display_label: None,
    };
    let attached = store
        .bind_funded_api_create(
            "funded-browser-create",
            &public,
            request.requester_id,
            instance.id,
        )
        .await?;
    assert_eq!(attached.environment_id, instance.id);
    assert_eq!(attached.operation_id, instance.operation.id);
    assert_eq!(
        store
            .bind_funded_api_create(
                "funded-browser-create",
                &public,
                request.requester_id,
                instance.id
            )
            .await?,
        attached
    );
    assert_eq!(
        admission.for_instance(&instance).await?.lease_authorization,
        original.lease
    );
    assert_eq!(
        resource
            .environment_credit_grant(OWNER, original.lease.lease_id)
            .await?
            .hold
            .id,
        grant.hold.id
    );
    let counts:(i64,i64,i64)=sqlx::query_as("SELECT (SELECT count(*) FROM environment.environment_instances WHERE environment_id=$1),(SELECT count(*) FROM environment.environment_operations WHERE environment_id=$1),(SELECT count(*) FROM resource.credit_holds WHERE request_id=$2)").bind(instance.id.as_uuid()).bind(request.id.as_uuid()).fetch_one(pool).await?;
    assert_eq!(counts, (1, 1, 1));
    let old_reservations: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM resource.environment_resource_reservations WHERE environment_id=$1",
    )
    .bind(instance.id.as_uuid())
    .fetch_one(pool)
    .await?;
    assert_eq!(old_reservations, 0);
    let meter: serde_json::Value = sqlx::query_scalar(
        "SELECT contract FROM environment.resource_metering_state WHERE environment_id=$1",
    )
    .bind(instance.id.as_uuid())
    .fetch_one(pool)
    .await?;
    assert_eq!(meter["target"]["kind"], "resource_request");
    assert_eq!(meter["target"]["requestId"], request.id.to_string());
    let mut foreign = handoff.clone();
    foreign.environment_id = contracts::EnvironmentId::new();
    assert_ne!(
        send(api, serde_json::to_value(&foreign)?).await?.0,
        StatusCode::ACCEPTED
    );
    replies.abort();
    Ok(())
}
