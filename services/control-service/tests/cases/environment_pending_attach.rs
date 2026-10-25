//! Approved but not activated attachment over real JWT/HTTP and `PostgreSQL` authorities.
use super::{PgAuthority, Result, UnusedAdmission, service_auth};
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use contracts::events::{CloudEvent, EVENT_CONTRACTS, ReleasePublished, SPEC_VERSION, subjects};
use contracts::http::CreateEnvironmentRequest;
use contracts::resource::{ResourceApproval, ResourceTarget};
use contracts::{ActorId, EventId, Revision, Sequence, UtcTimestamp};
use environment_service::{
    EnvironmentApiState, EnvironmentRuntimeAdmission, FreezeBindingConfiguration,
    FreezeBindingService, NatsAccessRevoker, NatsResourceLeaseVerifier, PgEnvironmentStore,
    PgReleaseProjectionStore,
};
use resource_service::store::PgResourceStore;
use sqlx::{PgPool, Row};
use std::{sync::Arc, time::Duration};
use testcontainers::{
    GenericImage,
    core::{IntoContainerPort, WaitFor},
    runners::AsyncRunner,
};
use tower::ServiceExt;

#[allow(
    clippy::too_many_lines,
    reason = "Approval, scope denials and activation use one real request and persistent API key"
)]
pub(crate) async fn assert_before_activation(
    pool: &PgPool,
    resource: &PgResourceStore,
    approval: &ResourceApproval,
) -> Result {
    super::super::super::apply_domain_migrations(pool, persistence_sqlx::Domain::Environment)
        .await
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    let request = resource.load(approval.request_id).await?;
    let ResourceTarget::Environment {
        release_id,
        release_version,
        ..
    } = request.target
    else {
        return Err("environment target required".into());
    };
    let row = sqlx::query("SELECT release_contract,candidate_contract FROM control.runtime_authorization_configurations WHERE release_id=$1")
        .bind(release_id.as_uuid()).fetch_one(pool).await?;
    let release = serde_json::from_value(row.try_get("release_contract")?)?;
    let candidate: contracts::authoring::EnvironmentCandidate =
        serde_json::from_value(row.try_get("candidate_contract")?)?;
    let projection = ReleasePublished {
        release,
        environment_spec: candidate.spec,
    };
    let releases = PgReleaseProjectionStore::new(pool.clone());
    let contract = EVENT_CONTRACTS
        .iter()
        .find(|event| event.subject == subjects::ENVIRONMENT_TEMPLATE_RELEASE_PUBLISHED)
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
                aggregate_revision: Revision::new(release_version)?,
                aggregate_sequence: Sequence(1),
                trace_id: "funded-pending-attachment".into(),
                data: projection,
            },
        )
        .await?;
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
    let store = PgEnvironmentStore::new(pool.clone());
    let freeze = FreezeBindingService::new_with_admission_resolver(
        pool.clone(),
        releases.clone(),
        FreezeBindingConfiguration {
            container_workspace_storage_class: "synthetic-storage".into(),
            vm: None,
        },
        UnusedAdmission,
    )?;
    let router = environment_service::environment_api_router(
        EnvironmentApiState::new(
            store,
            releases.clone(),
            NatsAccessRevoker::new("unused-revoke".into(), nats.clone(), Duration::from_secs(1))?,
            NatsResourceLeaseVerifier::new("unused-lease".into(), nats, Duration::from_secs(1))?,
            freeze,
        )
        .with_runtime_admission(EnvironmentRuntimeAdmission::new(
            releases,
            Arc::new(PgAuthority(resource.clone(), pool.clone())),
        )),
    );
    let authority = service_auth::start().await?;
    let router = environment_service::with_service_auth(router, authority.verifier.clone());
    let public = CreateEnvironmentRequest {
        resource_request_id: request.id,
        project_id: request.project_id,
        course_id: request.course_id,
        release_id,
        release_version,
        display_label: None,
    };
    let before = counts(pool).await?;
    for _ in 0..2 {
        let response = send(
            &router,
            &authority.access_token,
            &public,
            request.requester_id,
        )
        .await?;
        assert_eq!(response.0, StatusCode::CONFLICT, "{}", response.1);
        assert_eq!(
            response.1["diagnosticCode"],
            "LW_ENVIRONMENT_ALLOCATION_PENDING"
        );
        assert_eq!(
            counts(pool).await?,
            before,
            "pending attachment must not reserve an API intent, instance, operation, Outbox or hold"
        );
    }
    let mut wrong_project = public.clone();
    wrong_project.project_id = contracts::ProjectId::new();
    let mut wrong_course = public.clone();
    wrong_course.course_id = Some(contracts::CourseId::new());
    let mut wrong_release = public.clone();
    wrong_release.release_id = contracts::ReleaseId::new();
    let mut wrong_version = public.clone();
    wrong_version.release_version += 1;
    for (input, actor) in [
        (public.clone(), ActorId::new()),
        (wrong_project, request.requester_id),
        (wrong_course, request.requester_id),
        (wrong_release, request.requester_id),
        (wrong_version, request.requester_id),
    ] {
        let response = send(&router, &authority.access_token, &input, actor).await?;
        assert_ne!(response.0, StatusCode::ACCEPTED);
        assert_ne!(
            response.1["diagnosticCode"], "LW_ENVIRONMENT_ALLOCATION_PENDING",
            "wrong ownership or target cannot be called pending"
        );
        assert_eq!(counts(pool).await?, before);
    }
    // An expired approval must still satisfy valid_until > created_at. Wall-clock minus one
    // second can precede creation when this fixture runs quickly.
    let mut expired = approval.clone();
    expired.valid_until =
        UtcTimestamp::from_utc(approval.created_at.get() + time::Duration::milliseconds(1))?;
    assert!(expired.valid_until < resource.current_time().await?);
    for revoked in [true, false] {
        if revoked {
            sqlx::query("UPDATE resource.resource_approvals SET revoked_at=clock_timestamp() WHERE approval_id=$1")
                .bind(approval.id.as_uuid()).execute(pool).await?;
        } else {
            sqlx::query("UPDATE resource.resource_approvals SET valid_until=$2,contract=$3 WHERE approval_id=$1")
                .bind(approval.id.as_uuid()).bind(expired.valid_until.get())
                .bind(serde_json::to_value(&expired)?).execute(pool).await?;
        }
        let response = send(
            &router,
            &authority.access_token,
            &public,
            request.requester_id,
        )
        .await?;
        assert_eq!(response.0, StatusCode::CONFLICT);
        assert_eq!(response.1["diagnosticCode"], "LW_RUNTIME_LEASE_NOT_ACTIVE");
        assert_eq!(counts(pool).await?, before);
        sqlx::query("UPDATE resource.resource_approvals SET revoked_at=NULL,valid_until=$2,contract=$3 WHERE approval_id=$1")
            .bind(approval.id.as_uuid()).bind(approval.valid_until.get())
            .bind(serde_json::to_value(approval)?).execute(pool).await?;
    }
    // The parent activates this exact request/lease, then retries funded-browser-create
    // through the same persistent ledger and proves a single Environment and hold.
    Ok(())
}

async fn counts(pool: &PgPool) -> Result<(i64, i64, i64, i64, i64)> {
    Ok(sqlx::query_as("SELECT (SELECT count(*) FROM environment.environment_instances), (SELECT count(*) FROM environment.environment_operations), (SELECT count(*) FROM environment.outbox_events), (SELECT count(*) FROM environment.idempotency_ledger), (SELECT count(*) FROM resource.credit_holds)")
        .fetch_one(pool).await?)
}

async fn send(
    router: &Router,
    token: &str,
    request: &CreateEnvironmentRequest,
    actor: ActorId,
) -> Result<(StatusCode, serde_json::Value)> {
    let mut input = Request::builder()
        .method("POST")
        .uri("/api/v1/environments")
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {token}"))
        .header("x-labweaver-actor-id", actor.to_string())
        .header("x-labweaver-session-id", uuid::Uuid::now_v7().to_string())
        .header("idempotency-key", "funded-browser-create")
        .body(Body::from(serde_json::to_vec(request)?))?;
    input
        .extensions_mut()
        .insert(telemetry::RequestContext::generate());
    let response = router.clone().oneshot(input).await?;
    let status = response.status();
    Ok((
        status,
        serde_json::from_slice(&to_bytes(response.into_body(), 1_048_576).await?)?,
    ))
}
