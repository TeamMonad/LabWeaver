//! Administrator platform-image gateway: the Agent stays the catalog authority, Control owns the
//! release impact hint, the staging session and the completion fence.
//!
//! Every upstream failure is expected to survive to the browser with the Agent's own diagnostic
//! and status, and every staged archive version is registered for exact cleanup.

use std::{
    collections::{BTreeMap, BTreeSet},
    io::Cursor,
    str::FromStr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

use artifact_store::{
    ImmutableObjectStore, ObjectStoreError, PlatformImageMultipartPart,
    PlatformImageMultipartPartInput, PlatformImageMultipartUpload, PresignedPlatformImagePart,
    PresignedUpload, VerifiedObject, VerifiedObjectFile,
};
use async_trait::async_trait;
use auth::{
    ServiceAuthConfig, ServiceTokenClient, ServiceTokenClientConfig, ServiceTokenVerifier,
    TransportSecurityMode, no_redirect_http_client,
};
use axum::{
    Json, Router,
    body::{Body, to_bytes},
    extract::{Path, State},
    http::{HeaderValue, Request, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use contracts::authoring::{
    CandidateApproval, CandidateDecision, EnvironmentCandidate, ProjectLlmEgressPolicy, RuntimeKind,
};
use contracts::http::{
    CancelPlatformImageUploadRequest, CandidateDecisionRequest, CompletePlatformImageUploadPart,
    CompletePlatformImageUploadRequest, CreateEnvironmentTemplateReleaseRequest,
    CreatePlatformImageUploadRequest, DisablePlatformImageRequest, EnvironmentCandidateView,
    IdempotencyKey, InternalPlatformImageDisableRequest, InternalPlatformImageImportEnqueueRequest,
    InternalPlatformImageImportJobStatus, InternalPlatformImageRegistrationRequest,
    InternalPlatformImageRepinRequest, OperationAccepted, PLATFORM_IMAGE_ARCHIVE_MEDIA_TYPE,
    PLATFORM_IMAGE_UPLOAD_PART_SIZE_BYTES, PlatformImageCatalog, PlatformImageCatalogView,
    PlatformImageEntry, PlatformImageEntryView, PlatformImageImportJobState, PlatformImageKind,
    PlatformImageStatus, PlatformImageUploadSession, PlatformImageUploadState,
    PlatformImageUploadStatus, RegisterPlatformImageRequest, RepinPlatformImageRequest, StrongEtag,
};
use contracts::supply_chain::{
    EnvironmentTemplateRelease, EnvironmentTemplateReleaseView, ImageArtifact,
    VirtualMachineDiskFormat,
};
use contracts::{
    ActorId, AgentRunId, ApprovalId, ArtifactId, ArtifactRef, AuthenticatedActor,
    AuthorizationDecision, AuthorizationDecisionRequest, BffSessionId, BuildRequestId, CandidateId,
    CourseId, DiagnosticCode, ImageArtifactId, PlatformImageId, PlatformRole, PolicyId,
    ProblemDetails, Project, ProjectId, ProjectState, ReleaseId, Revision, UploadSessionId,
    UtcTimestamp,
};
use control_service::api::{ApiState, GatewayPrincipal, router};
use control_service::clients::ServiceHttpClientConfig;
use control_service::{
    ContainerBuildPolicy, ControlConfig, ControlService, EvaluationRuntimePolicy,
};
use hyper_util::{
    rt::{TokioExecutor, TokioIo},
    server::conn::auto::Builder,
    service::TowerToHyperService,
};
use persistence_sqlx::{Domain, Sha256Digest};
use rcgen::{
    BasicConstraints, CertificateParams, CertifiedIssuer, ExtendedKeyUsagePurpose, IsCa, KeyPair,
    KeyUsagePurpose,
};
use rustls::{ServerConfig, pki_types::PrivateKeyDer};
use serde_json::{Value, json};
use sqlx::{PgPool, postgres::PgPoolOptions};
use testcontainers::{ImageExt, runners::AsyncRunner};
use testcontainers_modules::postgres::Postgres;
use time::Duration;
use tokio::{net::TcpListener, sync::oneshot};
use tokio_rustls::TlsAcceptor;
use tower::ServiceExt;

mod support;

const SERVICE_TOKEN: &str = "eyJhbGciOiJub25lIn0.eyJhdWQiOlsibGFid2VhdmVyLWFjY2VzcyIsImxhYndlYXZlci1hZ2VudCIsImxhYndlYXZlci1lbnZpcm9ubWVudCIsImxhYndlYXZlci1ldmFsdWF0aW9uIl19.signature";
const UPLOAD_TTL_SECONDS: u64 = 900;
const FROZEN_OBJECT_VERSION: &str = "staged-version-1";
const CONFLICT_BINDING: &str = "conflict-binding";
const UNRESOLVABLE_DIGEST: &str = "sha256:unresolvable";

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one administrator route test covers the listing, mutation, upload and import fences"
)]
async fn platform_image_admin_routes_gateway_the_agent_authority()
-> Result<(), Box<dyn std::error::Error>> {
    let postgres = Postgres::default().with_tag("17.5-alpine").start().await?;
    let pool = PgPoolOptions::new()
        .max_connections(8)
        .connect(&format!(
            "postgres://postgres:postgres@127.0.0.1:{}/postgres",
            postgres.get_host_port_ipv4(5432).await?
        ))
        .await?;
    support::apply_domain_migrations(&pool, Domain::Control).await?;

    let now = import_now()?;
    let actor_id = ActorId::new();
    let session_id = BffSessionId::new();
    let project_id = ProjectId::new();
    let course_id = CourseId::new();

    let pinned_digest = format!("sha256:{}", Sha256Digest::of_bytes(b"pinned-image"));
    let other_digest = format!("sha256:{}", Sha256Digest::of_bytes(b"other-image"));
    let virtual_machine_digest = format!(
        "sha256:{}",
        Sha256Digest::of_bytes(b"virtual-machine-image")
    );
    let unpinned_digest = format!("sha256:{}", Sha256Digest::of_bytes(b"unpinned-image"));
    let pinned_entry = platform_image_entry(
        "ubuntu-24.04-v1",
        PlatformImageKind::Container,
        &pinned_digest,
        now,
    );
    let other_entry = platform_image_entry(
        "python-3.12-v1",
        PlatformImageKind::Container,
        &other_digest,
        now,
    );
    let virtual_machine_entry = platform_image_entry(
        "debian-13-v1",
        PlatformImageKind::VirtualMachine,
        &virtual_machine_digest,
        now,
    );
    let unpinned_entry = platform_image_entry(
        "node-22-v1",
        PlatformImageKind::Container,
        &unpinned_digest,
        now,
    );
    let imported_entry = platform_image_entry(
        "imported-archive-v1",
        PlatformImageKind::Container,
        &pinned_digest,
        now,
    );

    let live_release = release(
        project_id,
        course_id,
        RuntimeKind::Container,
        &pinned_digest,
        1,
        actor_id,
        now,
    )?;
    let withdrawn_release = release(
        project_id,
        course_id,
        RuntimeKind::Container,
        &pinned_digest,
        2,
        actor_id,
        now,
    )?;
    let other_release = release(
        project_id,
        course_id,
        RuntimeKind::Container,
        &other_digest,
        3,
        actor_id,
        now,
    )?;
    let virtual_machine_release = release(
        project_id,
        course_id,
        RuntimeKind::VirtualMachine,
        &virtual_machine_digest,
        4,
        actor_id,
        now,
    )?;
    insert_release(&pool, &live_release).await?;
    insert_release(&pool, &withdrawn_release).await?;
    insert_release(&pool, &other_release).await?;
    insert_release(&pool, &virtual_machine_release).await?;
    withdraw_release(&pool, &withdrawn_release, actor_id, now).await?;

    let objects = Arc::new(PlatformImageObjects::new(b"oci-layout-archive".to_vec()));

    let (ca_pem, leaf_pem, leaf_key_pem, jwk) = tls_material()?;
    let temp_dir = tempfile::tempdir()?;
    let ca_path = temp_dir.path().join("control-platform-image-ca.pem");
    std::fs::write(&ca_path, &ca_pem)?;
    let authority = spawn_authority(jwk).await?;
    let admin = Arc::new(AtomicBool::new(true));
    let access_server = spawn_access_server(&leaf_pem, &leaf_key_pem, Arc::clone(&admin)).await?;
    let received = Arc::new(Mutex::new(Vec::new()));
    let agent_server = spawn_agent_server(
        &leaf_pem,
        &leaf_key_pem,
        AgentState {
            entries: vec![
                pinned_entry.clone(),
                other_entry.clone(),
                virtual_machine_entry.clone(),
                unpinned_entry.clone(),
            ],
            imported: imported_entry.clone(),
            received: Arc::clone(&received),
        },
    )
    .await?;
    let service_token_client = Arc::new(service_token_client(&authority.issuer).await?);
    let access = control_service::clients::AccessClient::new_authenticated(
        service_config(&access_server.base_url, &ca_path),
        Arc::clone(&service_token_client),
    )?;
    let agent = control_service::clients::AgentClient::new_authenticated(
        service_config(&agent_server.base_url, &ca_path),
        Arc::clone(&service_token_client),
    )?;
    let environment = control_service::clients::EnvironmentClient::new_authenticated(
        service_config(&agent_server.base_url, &ca_path),
        Arc::clone(&service_token_client),
    )?;
    let evaluation = control_service::clients::EvaluationClient::new_authenticated(
        service_config(&agent_server.base_url, &ca_path),
        Arc::clone(&service_token_client),
    )?;
    let verifier = ServiceTokenVerifier::discover(
        ServiceAuthConfig::new(
            &authority.issuer,
            "labweaver-agent".to_owned(),
            BTreeSet::from(["control-service".to_owned()]),
            BTreeSet::new(),
            BTreeSet::from(["ES256".to_owned()]),
            3_600,
            1,
            TransportSecurityMode::InsecureTestOnly,
        )?,
        no_redirect_http_client(None, TransportSecurityMode::InsecureTestOnly)?,
    )
    .await?;
    let service = ControlService::new(pool.clone(), objects, config()?)?;
    let app = router(Arc::new(ApiState {
        control: service.clone(),
        access,
        agent,
        evaluation,
        environment,
        service_token_verifier: Arc::new(verifier),
    }))
    .layer(axum::Extension(GatewayPrincipal {
        client_id: "access-gateway".to_owned(),
    }));

    // Listing: only non-withdrawn releases count, keyed by the exact pinned digest for containers
    // and by the base-disk source digest for virtual machines.
    let list_response = app
        .clone()
        .oneshot(admin_request(
            "/api/v1/admin/images".to_owned(),
            "GET",
            actor_id,
            session_id,
            None,
            None,
        )?)
        .await?;
    assert_eq!(list_response.status(), StatusCode::OK);
    let catalog: PlatformImageCatalogView =
        serde_json::from_slice(&to_bytes(list_response.into_body(), usize::MAX).await?)?;
    assert_eq!(catalog.entries.len(), 4);
    assert_eq!(catalog.entries[0].entry.binding, "ubuntu-24.04-v1");
    assert_eq!(catalog.entries[0].release_reference_count, 1);
    assert_eq!(catalog.entries[1].release_reference_count, 1);
    assert_eq!(
        catalog.entries[2].entry.kind,
        PlatformImageKind::VirtualMachine
    );
    assert_eq!(catalog.entries[2].release_reference_count, 1);
    assert_eq!(catalog.entries[3].release_reference_count, 0);

    // Registration forwards the verified Access decision actor, never a browser-supplied one.
    let register_response = app
        .clone()
        .oneshot(admin_request(
            "/api/v1/admin/images".to_owned(),
            "POST",
            actor_id,
            session_id,
            Some("register-key"),
            Some(serde_json::to_vec(&RegisterPlatformImageRequest {
                kind: PlatformImageKind::Container,
                binding: "golang-1.24-v1".to_owned(),
                source_reference: "harbor.lab.lan/labweaver-system/golang:1.24".to_owned(),
                trust_revision: 1,
                reason: "reviewed base image".to_owned(),
            })?),
        )?)
        .await?;
    assert_eq!(register_response.status(), StatusCode::CREATED);
    assert!(register_response.headers().get("etag").is_none());
    let registered: PlatformImageEntryView =
        serde_json::from_slice(&to_bytes(register_response.into_body(), usize::MAX).await?)?;
    assert_eq!(registered.entry.binding, "golang-1.24-v1");
    let register_body = received_body(&received, "register")?;
    assert_eq!(
        register_body.get("actorId").and_then(Value::as_str),
        Some(actor_id.to_string().as_str())
    );
    assert_eq!(
        register_body.get("binding").and_then(Value::as_str),
        Some("golang-1.24-v1")
    );

    // Repin and disable stay routed with an exact UUIDv7 catalog identity.
    let repin_response = app
        .clone()
        .oneshot(admin_request(
            format!("/api/v1/admin/images/{}/repin", pinned_entry.catalog_id),
            "POST",
            actor_id,
            session_id,
            Some("repin-key"),
            Some(serde_json::to_vec(&RepinPlatformImageRequest {
                expected_digest: pinned_digest.clone(),
                trust_revision: 2,
                reason: "re-resolve the reviewed tag".to_owned(),
            })?),
        )?)
        .await?;
    assert_eq!(repin_response.status(), StatusCode::OK);
    let repinned: PlatformImageEntryView =
        serde_json::from_slice(&to_bytes(repin_response.into_body(), usize::MAX).await?)?;
    assert_eq!(repinned.entry.catalog_id, pinned_entry.catalog_id);
    assert_eq!(repinned.release_reference_count, 1);

    let disable_response = app
        .clone()
        .oneshot(admin_request(
            format!("/api/v1/admin/images/{}/disable", other_entry.catalog_id),
            "POST",
            actor_id,
            session_id,
            Some("disable-key"),
            Some(serde_json::to_vec(&DisablePlatformImageRequest {
                expected_digest: other_digest.clone(),
                reason: "superseded by a newer base image".to_owned(),
            })?),
        )?)
        .await?;
    assert_eq!(disable_response.status(), StatusCode::OK);
    let disabled: PlatformImageEntryView =
        serde_json::from_slice(&to_bytes(disable_response.into_body(), usize::MAX).await?)?;
    assert_eq!(disabled.entry.catalog_id, other_entry.catalog_id);
    assert_eq!(disabled.entry.status, PlatformImageStatus::Disabled);

    // Upstream RFC 9457 failures keep the Agent diagnostic instead of a local classification.
    let conflict_response = app
        .clone()
        .oneshot(admin_request(
            "/api/v1/admin/images".to_owned(),
            "POST",
            actor_id,
            session_id,
            Some("conflict-key"),
            Some(serde_json::to_vec(&RegisterPlatformImageRequest {
                kind: PlatformImageKind::Container,
                binding: CONFLICT_BINDING.to_owned(),
                source_reference: "harbor.lab.lan/labweaver-system/conflict:v1".to_owned(),
                trust_revision: 1,
                reason: "already pinned".to_owned(),
            })?),
        )?)
        .await?;
    assert_eq!(conflict_response.status(), StatusCode::CONFLICT);
    let conflict_problem = problem_body(conflict_response).await?;
    assert_eq!(
        conflict_problem.diagnostic_code.as_str(),
        "LW_PLATFORM_IMAGE_STATE_CONFLICT"
    );

    let reference_response = app
        .clone()
        .oneshot(admin_request(
            format!("/api/v1/admin/images/{}/repin", pinned_entry.catalog_id),
            "POST",
            actor_id,
            session_id,
            Some("reference-key"),
            Some(serde_json::to_vec(&RepinPlatformImageRequest {
                expected_digest: UNRESOLVABLE_DIGEST.to_owned(),
                trust_revision: 2,
                reason: "unresolvable reference".to_owned(),
            })?),
        )?)
        .await?;
    assert_eq!(
        reference_response.status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let reference_problem = problem_body(reference_response).await?;
    assert_eq!(
        reference_problem.diagnostic_code.as_str(),
        "LW_PLATFORM_IMAGE_REFERENCE_INVALID"
    );

    // A non-admin decision is refused before any Agent call.
    admin.store(false, Ordering::SeqCst);
    let denied_response = app
        .clone()
        .oneshot(admin_request(
            "/api/v1/admin/images".to_owned(),
            "GET",
            actor_id,
            session_id,
            None,
            None,
        )?)
        .await?;
    assert_eq!(denied_response.status(), StatusCode::FORBIDDEN);
    let denied_problem = problem_body(denied_response).await?;
    assert_eq!(
        denied_problem.diagnostic_code.as_str(),
        "LW_AUTH_SCOPE_DENIED"
    );
    admin.store(true, Ordering::SeqCst);

    // One staged upload authority per archive, with a pending row and a revision ETag.
    let upload_response = app
        .clone()
        .oneshot(admin_request(
            "/api/v1/admin/images/uploads".to_owned(),
            "POST",
            actor_id,
            session_id,
            Some("upload-key"),
            Some(serde_json::to_vec(&CreatePlatformImageUploadRequest {
                kind: PlatformImageKind::Container,
                binding: "java-21-v1".to_owned(),
                target_reference: "harbor.lab.lan/labweaver-system/java:21".to_owned(),
                archive_bytes: 4_096,
                archive_media_type: PLATFORM_IMAGE_ARCHIVE_MEDIA_TYPE.to_owned(),
                disk_format: None,
                disk_path: None,
                capacity_bytes: None,
                trust_revision: 1,
                reason: "reviewed OCI archive".to_owned(),
            })?),
        )?)
        .await?;
    let upload_status = upload_response.status();
    let upload_etag = upload_response.headers().get("etag").cloned();
    let upload_bytes = to_bytes(upload_response.into_body(), usize::MAX).await?;
    assert_eq!(
        upload_status,
        StatusCode::CREATED,
        "{}",
        String::from_utf8_lossy(&upload_bytes)
    );
    assert_eq!(upload_etag, Some(HeaderValue::from_static("\"rev-1\"")));
    let session: PlatformImageUploadSession = serde_json::from_slice(&upload_bytes)?;
    assert_eq!(session.archive_bytes, 4_096);
    assert_eq!(
        session.upload_target.parts[0].upload_url,
        "https://objects.example.invalid/fake-problem-packages/platform-image-uploads/".to_owned()
            + &session.upload_id.to_string()
            + "/1"
    );
    assert_eq!(
        upload_state(&pool, session.upload_id).await?,
        ("pending".to_owned(), None, None)
    );

    let malformed_complete = app
        .clone()
        .oneshot(admin_request(
            format!(
                "/api/v1/admin/images/uploads/{}/complete",
                session.upload_id
            ),
            "POST",
            actor_id,
            session_id,
            Some("malformed-complete-key"),
            Some(serde_json::to_vec(&CompletePlatformImageUploadRequest {
                parts: Vec::new(),
            })?),
        )?)
        .await?;
    assert_eq!(
        malformed_complete.status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        problem_body(malformed_complete)
            .await?
            .diagnostic_code
            .as_str(),
        "LW_PLATFORM_IMAGE_UPLOAD_INVALID"
    );

    // Completion only queues durable work. The HTTP operation remains fast and the browser reads
    // the resulting state through the status route while Control and Agent workers run.
    let complete_response = app
        .clone()
        .oneshot(admin_request(
            format!(
                "/api/v1/admin/images/uploads/{}/complete",
                session.upload_id
            ),
            "POST",
            actor_id,
            session_id,
            Some("complete-key"),
            Some(serde_json::to_vec(&completion_request(&session))?),
        )?)
        .await?;
    let complete_status = complete_response.status();
    let complete_bytes = to_bytes(complete_response.into_body(), usize::MAX).await?;
    assert_eq!(
        complete_status,
        StatusCode::ACCEPTED,
        "{}",
        String::from_utf8_lossy(&complete_bytes)
    );
    let queued: PlatformImageUploadStatus = serde_json::from_slice(&complete_bytes)?;
    assert_eq!(queued.upload_id, session.upload_id);
    assert_eq!(queued.state, PlatformImageUploadState::Queued);
    assert_eq!(queued.revision, Revision::new(2)?);
    assert_eq!(
        upload_state(&pool, session.upload_id).await?,
        ("queued".to_owned(), None, None)
    );

    let status_response = app
        .clone()
        .oneshot(admin_request(
            format!("/api/v1/admin/images/uploads/{}", session.upload_id),
            "GET",
            actor_id,
            session_id,
            None,
            None,
        )?)
        .await?;
    assert_eq!(status_response.status(), StatusCode::OK);
    let current: PlatformImageUploadStatus =
        serde_json::from_slice(&to_bytes(status_response.into_body(), usize::MAX).await?)?;
    assert_eq!(current.upload_id, queued.upload_id);
    assert_eq!(current.state, queued.state);
    assert_eq!(current.revision, queued.revision);
    assert!(current.upload_target.is_some());
    assert_eq!(current.uploaded_parts.len(), 1);

    let cancel_response = app
        .clone()
        .oneshot(admin_request(
            format!("/api/v1/admin/images/uploads/{}/cancel", session.upload_id),
            "POST",
            actor_id,
            session_id,
            Some("cancel-key"),
            Some(serde_json::to_vec(&CancelPlatformImageUploadRequest {
                expected_revision: queued.revision,
            })?),
        )?)
        .await?;
    assert_eq!(cancel_response.status(), StatusCode::ACCEPTED);
    let cancelled: PlatformImageUploadStatus =
        serde_json::from_slice(&to_bytes(cancel_response.into_body(), usize::MAX).await?)?;
    assert_eq!(cancelled.state, PlatformImageUploadState::Cancelling);
    assert_eq!(cancelled.revision, Revision::new(3)?);
    assert_eq!(
        upload_state(&pool, session.upload_id).await?,
        ("queued".to_owned(), None, None)
    );

    let cancel_readback = app
        .clone()
        .oneshot(admin_request(
            format!("/api/v1/admin/images/uploads/{}", session.upload_id),
            "GET",
            actor_id,
            session_id,
            None,
            None,
        )?)
        .await?;
    assert_eq!(cancel_readback.status(), StatusCode::OK);
    let cancel_readback: PlatformImageUploadStatus =
        serde_json::from_slice(&to_bytes(cancel_readback.into_body(), usize::MAX).await?)?;
    assert_eq!(cancel_readback.state, PlatformImageUploadState::Cancelling);
    assert_eq!(cancel_readback.revision, Revision::new(3)?);

    let expired_session = service
        .create_platform_image_upload(
            actor_id,
            &upload_request("expired-http"),
            &IdempotencyKey::parse("create-expired-http")?,
            import_now()?,
        )
        .await?;
    sqlx::query("UPDATE control.platform_image_upload_sessions SET expires_at=date_trunc('milliseconds',clock_timestamp())-interval '1 second',created_at=date_trunc('milliseconds',clock_timestamp())-interval '2 seconds' WHERE upload_id=$1")
        .bind(expired_session.upload_id.as_uuid()).execute(&pool).await?;
    let expired_response = app
        .clone()
        .oneshot(admin_request(
            format!(
                "/api/v1/admin/images/uploads/{}/complete",
                expired_session.upload_id
            ),
            "POST",
            actor_id,
            session_id,
            Some("complete-expired-http"),
            Some(serde_json::to_vec(&completion_request(&expired_session))?),
        )?)
        .await?;
    assert_eq!(expired_response.status(), StatusCode::GONE);
    assert_eq!(
        problem_body(expired_response)
            .await?
            .diagnostic_code
            .as_str(),
        "LW_PLATFORM_IMAGE_UPLOAD_EXPIRED"
    );

    // A virtual-machine upload carries the reviewed disk descriptor through the staging fence and
    // into the internal import request the Agent wraps.
    let vm_upload = app
        .clone()
        .oneshot(admin_request(
            "/api/v1/admin/images/uploads".to_owned(),
            "POST",
            actor_id,
            session_id,
            Some("vm-upload-key"),
            Some(serde_json::to_vec(&CreatePlatformImageUploadRequest {
                kind: PlatformImageKind::VirtualMachine,
                binding: "ubuntu-24.04-vm-v1".to_owned(),
                target_reference: "harbor.lab.lan/labweaver-system/ubuntu-vm:24.04".to_owned(),
                archive_bytes: 8_192,
                archive_media_type: PLATFORM_IMAGE_ARCHIVE_MEDIA_TYPE.to_owned(),
                disk_format: Some(VirtualMachineDiskFormat::Qcow2),
                disk_path: Some("disk/disk.qcow2".to_owned()),
                capacity_bytes: Some(10_737_418_240),
                trust_revision: 1,
                reason: "reviewed VM base disk".to_owned(),
            })?),
        )?)
        .await?;
    assert_eq!(vm_upload.status(), StatusCode::CREATED);
    let vm_session: PlatformImageUploadSession =
        serde_json::from_slice(&to_bytes(vm_upload.into_body(), usize::MAX).await?)?;
    assert_eq!(
        vm_session.disk_format,
        Some(VirtualMachineDiskFormat::Qcow2)
    );
    assert_eq!(vm_session.disk_path.as_deref(), Some("disk/disk.qcow2"));
    assert_eq!(vm_session.capacity_bytes, Some(10_737_418_240));

    let vm_complete = app
        .clone()
        .oneshot(admin_request(
            format!(
                "/api/v1/admin/images/uploads/{}/complete",
                vm_session.upload_id
            ),
            "POST",
            actor_id,
            session_id,
            Some("vm-complete-key"),
            Some(serde_json::to_vec(&completion_request(&vm_session))?),
        )?)
        .await?;
    let vm_complete_status = vm_complete.status();
    let vm_complete_bytes = to_bytes(vm_complete.into_body(), usize::MAX).await?;
    assert_eq!(
        vm_complete_status,
        StatusCode::ACCEPTED,
        "{}",
        String::from_utf8_lossy(&vm_complete_bytes)
    );
    let vm_queued: PlatformImageUploadStatus = serde_json::from_slice(&vm_complete_bytes)?;
    assert_eq!(vm_queued.state, PlatformImageUploadState::Queued);

    // A partially declared VM descriptor fails closed at staging before any row is written.
    let partial_vm = app
        .clone()
        .oneshot(admin_request(
            "/api/v1/admin/images/uploads".to_owned(),
            "POST",
            actor_id,
            session_id,
            Some("vm-partial-key"),
            Some(serde_json::to_vec(&CreatePlatformImageUploadRequest {
                kind: PlatformImageKind::VirtualMachine,
                binding: "ubuntu-24.04-vm-v1".to_owned(),
                target_reference: "harbor.lab.lan/labweaver-system/ubuntu-vm:24.04".to_owned(),
                archive_bytes: 8_192,
                archive_media_type: PLATFORM_IMAGE_ARCHIVE_MEDIA_TYPE.to_owned(),
                disk_format: Some(VirtualMachineDiskFormat::Qcow2),
                disk_path: None,
                capacity_bytes: Some(10_737_418_240),
                trust_revision: 1,
                reason: "misdeclared VM disk".to_owned(),
            })?),
        )?)
        .await?;
    assert_eq!(partial_vm.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        problem_body(partial_vm).await?.diagnostic_code.as_str(),
        "LW_PLATFORM_IMAGE_UPLOAD_INVALID"
    );

    // A downstream rejection is resolved by the durable Agent and Control workers after this
    // request. The gateway only acknowledges the queued operation here.
    let rejected_response = app
        .clone()
        .oneshot(admin_request(
            "/api/v1/admin/images/uploads".to_owned(),
            "POST",
            actor_id,
            session_id,
            Some("rejected-upload-key"),
            Some(serde_json::to_vec(&CreatePlatformImageUploadRequest {
                kind: PlatformImageKind::Container,
                binding: CONFLICT_BINDING.to_owned(),
                target_reference: "harbor.lab.lan/labweaver-system/conflict:v1".to_owned(),
                archive_bytes: 2_048,
                archive_media_type: PLATFORM_IMAGE_ARCHIVE_MEDIA_TYPE.to_owned(),
                disk_format: None,
                disk_path: None,
                capacity_bytes: None,
                trust_revision: 1,
                reason: "conflicting catalog binding".to_owned(),
            })?),
        )?)
        .await?;
    assert_eq!(rejected_response.status(), StatusCode::CREATED);
    let rejected_session: PlatformImageUploadSession =
        serde_json::from_slice(&to_bytes(rejected_response.into_body(), usize::MAX).await?)?;
    let rejected_complete = app
        .clone()
        .oneshot(admin_request(
            format!(
                "/api/v1/admin/images/uploads/{}/complete",
                rejected_session.upload_id
            ),
            "POST",
            actor_id,
            session_id,
            Some("rejected-complete-key"),
            Some(serde_json::to_vec(&completion_request(&rejected_session))?),
        )?)
        .await?;
    let rejected_status = rejected_complete.status();
    let rejected_bytes = to_bytes(rejected_complete.into_body(), usize::MAX).await?;
    assert_eq!(
        rejected_status,
        StatusCode::ACCEPTED,
        "{}",
        String::from_utf8_lossy(&rejected_bytes)
    );
    let rejected_queued: PlatformImageUploadStatus = serde_json::from_slice(&rejected_bytes)?;
    assert_eq!(rejected_queued.state, PlatformImageUploadState::Queued);
    assert_eq!(
        upload_state(&pool, rejected_session.upload_id).await?,
        ("queued".to_owned(), None, None)
    );
    Ok(())
}

#[derive(Clone)]
struct AccessState {
    admin: Arc<AtomicBool>,
}

#[derive(Clone)]
struct AgentState {
    entries: Vec<PlatformImageEntry>,
    imported: PlatformImageEntry,
    received: Arc<Mutex<Vec<(&'static str, Value)>>>,
}

struct AuthorityHandle {
    issuer: String,
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

struct TlsServiceHandle {
    base_url: reqwest::Url,
    shutdown: Option<oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for TlsServiceHandle {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        self.task.abort();
    }
}

/// Fake immutable store that freezes one deterministic staged archive version per object key.
struct PlatformImageObjects {
    bytes: Vec<u8>,
    frozen: Mutex<BTreeMap<String, ArtifactRef>>,
    multipart: Mutex<BTreeMap<String, FakeMultipartUpload>>,
    available: AtomicBool,
    read_failure: AtomicBool,
    versions: Mutex<Vec<String>>,
}

#[derive(Clone, Debug)]
struct FakeMultipartUpload {
    upload_id: String,
    size_bytes: u64,
    part_count: u32,
    aborted: bool,
}

impl PlatformImageObjects {
    fn new(bytes: Vec<u8>) -> Self {
        Self {
            bytes,
            frozen: Mutex::new(BTreeMap::new()),
            multipart: Mutex::new(BTreeMap::new()),
            available: AtomicBool::new(true),
            read_failure: AtomicBool::new(false),
            versions: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl ImmutableObjectStore for PlatformImageObjects {
    fn binding(&self) -> &'static str {
        "test-object-store"
    }

    async fn presign_upload(
        &self,
        _key: &str,
        _size_bytes: u64,
        _media_type: &str,
        now: UtcTimestamp,
    ) -> Result<PresignedUpload, ObjectStoreError> {
        Ok(PresignedUpload {
            url: "https://objects.example.invalid/platform-image-archive".to_owned(),
            required_headers: BTreeMap::from([(
                "x-amz-server-side-encryption".to_owned(),
                "AES256".to_owned(),
            )]),
            expires_at: UtcTimestamp::from_utc(
                now.get()
                    + Duration::seconds(
                        i64::try_from(UPLOAD_TTL_SECONDS)
                            .map_err(|_| ObjectStoreError::ConfigurationInvalid)?,
                    ),
            )
            .map_err(|_| ObjectStoreError::ConfigurationInvalid)?,
        })
    }

    async fn create_platform_image_multipart_upload(
        &self,
        key: &str,
        size_bytes: u64,
        _media_type: &str,
        now: UtcTimestamp,
    ) -> Result<PlatformImageMultipartUpload, ObjectStoreError> {
        let part_count = u32::try_from(
            size_bytes
                .checked_add(PLATFORM_IMAGE_UPLOAD_PART_SIZE_BYTES - 1)
                .ok_or(ObjectStoreError::ObjectTooLarge)?
                / PLATFORM_IMAGE_UPLOAD_PART_SIZE_BYTES,
        )
        .map_err(|_| ObjectStoreError::ObjectTooLarge)?;
        let upload_id = format!("fake-{key}");
        self.multipart
            .lock()
            .map_err(|_| ObjectStoreError::ObjectUnavailable)?
            .insert(
                key.to_owned(),
                FakeMultipartUpload {
                    upload_id: upload_id.clone(),
                    size_bytes,
                    part_count,
                    aborted: false,
                },
            );
        self.presign_platform_image_multipart_upload(
            key,
            &upload_id,
            size_bytes,
            now,
            UtcTimestamp::from_utc(
                now.get()
                    + Duration::seconds(
                        i64::try_from(UPLOAD_TTL_SECONDS)
                            .map_err(|_| ObjectStoreError::ConfigurationInvalid)?,
                    ),
            )
            .map_err(|_| ObjectStoreError::ConfigurationInvalid)?,
        )
        .await
    }

    async fn presign_platform_image_multipart_upload(
        &self,
        key: &str,
        upload_id: &str,
        size_bytes: u64,
        _now: UtcTimestamp,
        expires_at: UtcTimestamp,
    ) -> Result<PlatformImageMultipartUpload, ObjectStoreError> {
        let multipart = self
            .multipart
            .lock()
            .map_err(|_| ObjectStoreError::ObjectUnavailable)?
            .get(key)
            .filter(|upload| !upload.aborted && upload.upload_id == upload_id)
            .cloned()
            .ok_or(ObjectStoreError::ObjectNotFound)?;
        if multipart.size_bytes != size_bytes {
            return Err(ObjectStoreError::ObjectIdentityMismatch);
        }
        let parts = (1..=multipart.part_count)
            .map(|part_number| PresignedPlatformImagePart {
                part_number,
                url: format!("https://objects.example.invalid/{upload_id}/{part_number}"),
                required_headers: BTreeMap::new(),
                expires_at,
            })
            .collect();
        Ok(PlatformImageMultipartUpload {
            upload_id: multipart.upload_id,
            part_size_bytes: PLATFORM_IMAGE_UPLOAD_PART_SIZE_BYTES,
            part_count: multipart.part_count,
            parts,
            expires_at,
        })
    }

    async fn find_platform_image_multipart_uploads(
        &self,
        key: &str,
    ) -> Result<Vec<String>, ObjectStoreError> {
        Ok(self
            .multipart
            .lock()
            .map_err(|_| ObjectStoreError::ObjectUnavailable)?
            .get(key)
            .filter(|upload| !upload.aborted)
            .map(|upload| vec![upload.upload_id.clone()])
            .unwrap_or_default())
    }

    async fn list_platform_image_multipart_parts(
        &self,
        key: &str,
        upload_id: &str,
    ) -> Result<Vec<PlatformImageMultipartPart>, ObjectStoreError> {
        let multipart = self
            .multipart
            .lock()
            .map_err(|_| ObjectStoreError::ObjectUnavailable)?
            .get(key)
            .filter(|upload| !upload.aborted && upload.upload_id == upload_id)
            .cloned()
            .ok_or(ObjectStoreError::ObjectNotFound)?;
        Ok((1..=multipart.part_count)
            .map(|part_number| PlatformImageMultipartPart {
                part_number,
                etag: format!("etag-{part_number}"),
                size_bytes: if part_number == multipart.part_count {
                    multipart.size_bytes
                        - PLATFORM_IMAGE_UPLOAD_PART_SIZE_BYTES * u64::from(part_number - 1)
                } else {
                    PLATFORM_IMAGE_UPLOAD_PART_SIZE_BYTES
                },
            })
            .collect())
    }

    async fn complete_platform_image_multipart_upload(
        &self,
        key: &str,
        upload_id: &str,
        size_bytes: u64,
        parts: &[PlatformImageMultipartPartInput],
    ) -> Result<(), ObjectStoreError> {
        let observed = self
            .list_platform_image_multipart_parts(key, upload_id)
            .await?;
        if observed.len() != parts.len()
            || observed.iter().zip(parts).any(|(observed, requested)| {
                observed.part_number != requested.part_number || observed.etag != requested.etag
            })
            || observed.iter().map(|part| part.size_bytes).sum::<u64>() != size_bytes
        {
            return Err(ObjectStoreError::ObjectIdentityMismatch);
        }
        Ok(())
    }

    async fn abort_platform_image_multipart_upload(
        &self,
        key: &str,
        upload_id: &str,
    ) -> Result<(), ObjectStoreError> {
        let mut uploads = self
            .multipart
            .lock()
            .map_err(|_| ObjectStoreError::ObjectUnavailable)?;
        let upload = uploads
            .get_mut(key)
            .filter(|upload| upload.upload_id == upload_id)
            .ok_or(ObjectStoreError::ObjectNotFound)?;
        upload.aborted = true;
        Ok(())
    }

    async fn read_verified(
        &self,
        key: &str,
        expected: &ArtifactRef,
    ) -> Result<VerifiedObject, ObjectStoreError> {
        let frozen = self
            .frozen
            .lock()
            .map_err(|_| ObjectStoreError::ObjectUnavailable)?;
        match frozen.get(key) {
            Some(reference) if reference == expected => Ok(VerifiedObject {
                reference: reference.clone(),
                bytes: self.bytes.clone(),
            }),
            _ => Err(ObjectStoreError::ObjectIdentityMismatch),
        }
    }

    async fn read_verified_file(
        &self,
        key: &str,
        expected: &ArtifactRef,
    ) -> Result<VerifiedObjectFile, ObjectStoreError> {
        let verified = self.read_verified(key, expected).await?;
        VerifiedObjectFile::from_bytes(verified.reference, &verified.bytes)
    }

    async fn freeze_current(
        &self,
        key: &str,
        expected_size: u64,
        media_type: &str,
    ) -> Result<VerifiedObject, ObjectStoreError> {
        let reference = ArtifactRef {
            artifact_id: ArtifactId::new(),
            store_binding: self.binding().to_owned(),
            object_version: FROZEN_OBJECT_VERSION.to_owned(),
            size_bytes: expected_size,
            media_type: media_type.to_owned(),
        };
        self.frozen
            .lock()
            .map_err(|_| ObjectStoreError::ObjectUnavailable)?
            .insert(key.to_owned(), reference.clone());
        let mut versions = self
            .versions
            .lock()
            .map_err(|_| ObjectStoreError::ObjectUnavailable)?;
        if !versions.contains(&FROZEN_OBJECT_VERSION.to_owned()) {
            versions.push(FROZEN_OBJECT_VERSION.to_owned());
        }
        Ok(VerifiedObject {
            reference,
            bytes: self.bytes.clone(),
        })
    }

    async fn freeze_current_file(
        &self,
        key: &str,
        expected_size: u64,
        media_type: &str,
    ) -> Result<VerifiedObjectFile, ObjectStoreError> {
        let verified = self.freeze_current(key, expected_size, media_type).await?;
        VerifiedObjectFile::from_bytes(verified.reference, &verified.bytes)
    }

    async fn freeze_current_reference(
        &self,
        key: &str,
        expected_size: u64,
        media_type: &str,
    ) -> Result<ArtifactRef, ObjectStoreError> {
        if self.read_failure.load(Ordering::SeqCst) {
            return Err(ObjectStoreError::ObjectUnavailable);
        }
        if !self.available.load(Ordering::SeqCst) {
            return Err(ObjectStoreError::ObjectNotFound);
        }
        Ok(self
            .freeze_current(key, expected_size, media_type)
            .await?
            .reference)
    }

    async fn list_key_versions(&self, _: &str) -> Result<Vec<String>, ObjectStoreError> {
        if self.read_failure.load(Ordering::SeqCst) {
            return Err(ObjectStoreError::ObjectUnavailable);
        }
        self.versions
            .lock()
            .map(|versions| versions.clone())
            .map_err(|_| ObjectStoreError::ObjectUnavailable)
    }

    async fn delete_orphan(&self, _key: &str, _version: &str) -> Result<(), ObjectStoreError> {
        Err(ObjectStoreError::DeleteFailed)
    }
}

fn platform_image_entry(
    binding: &str,
    kind: PlatformImageKind,
    digest: &str,
    now: UtcTimestamp,
) -> PlatformImageEntry {
    PlatformImageEntry {
        catalog_id: PlatformImageId::new(),
        kind,
        binding: binding.to_owned(),
        source_reference: format!("harbor.lab.lan/labweaver-system/{binding}:v1"),
        resolved_digest: digest.to_owned(),
        media_type: "application/vnd.oci.image.manifest.v1+json".to_owned(),
        size_bytes: 4_194_304,
        status: PlatformImageStatus::Active,
        trust_revision: 1,
        repin_generation: 1,
        capacity_bytes: None,
        disk_sha256: None,
        format: None,
        pinned_at: now,
        updated_at: now,
    }
}

fn release(
    project_id: ProjectId,
    course_id: CourseId,
    runtime_kind: RuntimeKind,
    digest: &str,
    version: u64,
    actor_id: ActorId,
    now: UtcTimestamp,
) -> Result<EnvironmentTemplateRelease, Box<dyn std::error::Error>> {
    let candidate_id = CandidateId::new();
    let candidate_revision = Revision::new(1)?;
    let artifact = match runtime_kind {
        RuntimeKind::Container => ImageArtifact::Container {
            id: ImageArtifactId::new(),
            build_request_id: BuildRequestId::new(),
            repository: "harbor.lab.lan/labweaver-system/base".to_owned(),
            digest: digest.to_owned(),
        },
        RuntimeKind::VirtualMachine => ImageArtifact::VirtualMachine {
            id: ImageArtifactId::new(),
            base_disk: contracts::supply_chain::VirtualMachineBaseDisk {
                binding: "debian-13-v1".to_owned(),
                source_registry_digest: format!("docker://quay.io/containerdisks/debian@{digest}"),
                capacity_bytes: 10_737_418_240,
            },
            format: contracts::supply_chain::VirtualMachineDiskFormat::Qcow2,
        },
    };
    Ok(EnvironmentTemplateRelease {
        id: ReleaseId::new(),
        project_id,
        course_id: Some(course_id),
        version,
        candidate_id,
        agent_run_id: contracts::AgentRunId::new(),
        candidate_revision,
        runtime_kind,
        approval: CandidateApproval {
            id: ApprovalId::new(),
            candidate_id,
            candidate_revision,
            policy_revision: Revision::new(1)?,
            trust_revision: Revision::new(1)?,
            actor_id,
            decision: CandidateDecision::Approved,
            reason: "approved base image".to_owned(),
            decided_at: now,
        },
        artifact,
        published_by: actor_id,
        published_at: now,
    })
}

async fn insert_release(
    pool: &PgPool,
    release: &EnvironmentTemplateRelease,
) -> Result<(), Box<dyn std::error::Error>> {
    let contract = serde_json::to_value(release)?;
    sqlx::query(
        "INSERT INTO control.environment_template_releases \
         (release_id,project_id,course_id,version,environment_candidate_id,candidate_revision, \
          spec_sha256,image_artifact_id,contract,published_at) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)",
    )
    .bind(release.id.as_uuid())
    .bind(release.project_id.as_uuid())
    .bind(release.course_id.map(CourseId::as_uuid))
    .bind(i64::try_from(release.version)?)
    .bind(release.candidate_id.as_uuid())
    .bind(i64::try_from(release.candidate_revision.get())?)
    .bind(Sha256Digest::of_bytes(b"spec").to_string())
    .bind(release.artifact.id().as_uuid())
    .bind(contract)
    .bind(release.published_at.get())
    .execute(pool)
    .await?;
    Ok(())
}

async fn withdraw_release(
    pool: &PgPool,
    release: &EnvironmentTemplateRelease,
    actor_id: ActorId,
    now: UtcTimestamp,
) -> Result<(), Box<dyn std::error::Error>> {
    sqlx::query(
        "INSERT INTO control.release_withdrawals \
         (release_id,project_id,course_id,release_version,actor_id,reason_code,withdrawn_at,contract) \
         VALUES ($1,$2,$3,$4,$5,'ADMIN',$6,$7)",
    )
    .bind(release.id.as_uuid())
    .bind(release.project_id.as_uuid())
    .bind(release.course_id.map(CourseId::as_uuid))
    .bind(i64::try_from(release.version)?)
    .bind(actor_id.as_uuid())
    .bind(now.get())
    .bind(json!({"releaseId": release.id, "reasonCode": "ADMIN"}))
    .execute(pool)
    .await?;
    Ok(())
}

async fn upload_state(
    pool: &PgPool,
    upload_id: UploadSessionId,
) -> Result<(String, Option<PlatformImageId>, Option<String>), Box<dyn std::error::Error>> {
    let row = sqlx::query(
        "SELECT state,imported_catalog_id,terminal_diagnostic \
         FROM control.platform_image_upload_sessions WHERE upload_id=$1",
    )
    .bind(upload_id.as_uuid())
    .fetch_one(pool)
    .await?;
    let state: String = sqlx::Row::try_get(&row, "state")?;
    let imported: Option<uuid::Uuid> = sqlx::Row::try_get(&row, "imported_catalog_id")?;
    Ok((
        state,
        imported
            .map(|id| PlatformImageId::from_str(&id.to_string()))
            .transpose()
            .map_err(|error| std::io::Error::other(error.to_string()))?,
        sqlx::Row::try_get(&row, "terminal_diagnostic")?,
    ))
}

fn config() -> Result<ControlConfig, Box<dyn std::error::Error>> {
    Ok(ControlConfig {
        package_object_prefix: "problem-packages".to_owned(),
        upload_ttl_seconds: UPLOAD_TTL_SECONDS,
        completion_lease_seconds: 300,
        max_package_files: 100,
        max_package_bytes: 1_048_576,
        retention_policy_id: PolicyId::new(),
        retention_seconds: 86_400,
        sse_retention_seconds: 3_600,
        trust_revision: Revision::new(1)?,
        image_policy_id: PolicyId::new(),
        image_policy_revision: Revision::new(1)?,
        environment_schema_sha256: Sha256Digest::of_bytes(b"environment"),
        evaluation_schema_sha256: Sha256Digest::of_bytes(b"evaluation"),
        container_build: ContainerBuildPolicy {
            builder_binding: "buildkit-primary-v1".to_owned(),
            output_repository_prefix: "harbor.internal/labweaver-system".to_owned(),
            dockerfile_path: "Dockerfile".to_owned(),
            runner_dockerfile_path: "evaluation/Dockerfile".to_owned(),
            network: contracts::supply_chain::BuildNetworkPolicy::DenyAll,
            max_duration_milliseconds: 600_000,
            max_cpu_millicores: 2_000,
            max_memory_bytes: 2_147_483_648,
        },
        virtual_machine_bases: control_service::VirtualMachineBaseCatalog {
            provider_binding: "kubevirt-primary-v1".to_owned(),
            storage_class_binding: "vm-rwo-primary-v1".to_owned(),
            max_bases: 8,
            max_capacity_bytes: 137_438_953_472,
            bases: vec![control_service::VirtualMachineBasePolicy {
                artifact_id: ImageArtifactId::new(),
                base_disk: contracts::supply_chain::VirtualMachineBaseDisk {
                    binding: "ubuntu-24.04-v1".to_owned(),
                    source_registry_digest: concat!(
                        "docker://quay.io/containerdisks/ubuntu@",
                        "sha256:d28194a16351320fa9a093e18233033508a745566eb8ba3b309c32924bf155a5"
                    )
                    .to_owned(),
                    capacity_bytes: 10_737_418_240,
                },
                format: contracts::supply_chain::VirtualMachineDiskFormat::Qcow2,
            }],
        },
        evaluation_runtime: EvaluationRuntimePolicy {
            provider_binding: "evaluation-primary-v1".to_owned(),
            runner_image: format!("runner@sha256:{}", "a".repeat(64)),
        },
        llm_policy_options: contracts::authoring::ProjectLlmPolicyOptions {
            models: vec![contracts::authoring::ProjectLlmPolicyModelOption {
                model: "fixture-provider-v1".to_owned(),
                label: "Fixture model".to_owned(),
            }],
            default_model: "fixture-provider-v1".to_owned(),
            runtime_binding: "claude-code-test".to_owned(),
            claude_code_version: "2.1.207".to_owned(),
            max_in_flight_per_worker: 2,
        },
    })
}

fn service_config(base_url: &reqwest::Url, ca_path: &std::path::Path) -> ServiceHttpClientConfig {
    ServiceHttpClientConfig {
        base_url: base_url.clone(),
        ca_certificate_file: ca_path.to_string_lossy().into_owned(),
        timeout_milliseconds: 3_000,
    }
}

fn admin_request(
    uri: String,
    method: &str,
    actor_id: ActorId,
    session_id: BffSessionId,
    key: Option<&str>,
    body: Option<Vec<u8>>,
) -> Result<Request<Body>, axum::http::Error> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("x-labweaver-actor-id", actor_id.to_string())
        .header("x-labweaver-session-id", session_id.to_string());
    if let Some(key) = key {
        builder = builder.header("Idempotency-Key", key);
    }
    if body.is_some() {
        builder = builder.header("content-type", "application/json");
    }
    builder.body(Body::from(body.unwrap_or_default()))
}

fn received_body(
    received: &Arc<Mutex<Vec<(&'static str, Value)>>>,
    route: &str,
) -> Result<Value, Box<dyn std::error::Error>> {
    let recorded = received
        .lock()
        .map_err(|_| std::io::Error::other("recorded request lock poisoned"))?;
    recorded
        .iter()
        .find(|(name, _)| *name == route)
        .map(|(_, body)| body.clone())
        .ok_or_else(|| std::io::Error::other(format!("no request recorded for {route}")).into())
}

async fn problem_body(response: Response) -> Result<ProblemDetails, Box<dyn std::error::Error>> {
    Ok(serde_json::from_slice(
        &to_bytes(response.into_body(), usize::MAX).await?,
    )?)
}

fn problem(status: StatusCode, code: &str, retryable: bool) -> Response {
    let diagnostic = DiagnosticCode::parse(code)
        .unwrap_or_else(|error| unreachable!("test diagnostic must parse: {error}"));
    (
        status,
        Json(ProblemDetails {
            problem_type: format!("urn:labweaver:problem:{}", code.to_ascii_lowercase()),
            title: "Agent platform image request blocked".to_owned(),
            status: status.as_u16(),
            detail: "The Agent authority rejected the platform image request.".to_owned(),
            instance: "urn:labweaver:request:test".to_owned(),
            diagnostic_code: diagnostic,
            request_id: "test-request".to_owned(),
            trace_id: None,
            retryable,
            violations: Vec::new(),
        }),
    )
        .into_response()
}

async fn service_token_client(
    issuer: &str,
) -> Result<ServiceTokenClient, Box<dyn std::error::Error>> {
    let config = ServiceTokenClientConfig::new(
        issuer,
        "labweaver-control".to_owned(),
        "test-secret".to_owned(),
        "labweaver-control".to_owned(),
        BTreeSet::from(["access.control.forward".to_owned()]),
        30,
        TransportSecurityMode::InsecureTestOnly,
    )?;
    Ok(ServiceTokenClient::discover(
        config,
        no_redirect_http_client(None, TransportSecurityMode::InsecureTestOnly)?,
    )
    .await?)
}

async fn spawn_authority(jwk: Value) -> Result<AuthorityHandle, Box<dyn std::error::Error>> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let issuer = format!("http://localhost:{}/realms/test", address.port());
    let state = (issuer.clone(), jwk);
    let router = Router::new()
        .route(
            "/realms/test/.well-known/openid-configuration",
            get(authority_discovery),
        )
        .route("/realms/test/jwks", get(authority_jwks))
        .route("/realms/test/token", post(authority_token))
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
        shutdown: Some(shutdown_tx),
        task,
    })
}

async fn authority_discovery(State((issuer, _)): State<(String, Value)>) -> Json<Value> {
    Json(json!({
        "issuer": issuer,
        "authorization_endpoint": format!("{issuer}/authorize"),
        "token_endpoint": format!("{issuer}/token"),
        "jwks_uri": format!("{issuer}/jwks"),
        "response_types_supported": ["code"],
        "subject_types_supported": ["public"],
        "id_token_signing_alg_values_supported": ["ES256"],
        "grant_types_supported": ["client_credentials"]
    }))
}

async fn authority_jwks(State((_, jwk)): State<(String, Value)>) -> Json<Value> {
    Json(json!({"keys": [jwk]}))
}

async fn authority_token() -> Result<Json<Value>, StatusCode> {
    Ok(Json(json!({
        "access_token": SERVICE_TOKEN,
        "token_type": "Bearer",
        "expires_in": 300
    })))
}

async fn spawn_access_server(
    certificate_pem: &str,
    private_key_pem: &str,
    admin: Arc<AtomicBool>,
) -> Result<TlsServiceHandle, Box<dyn std::error::Error>> {
    spawn_tls_service(
        Router::new()
            .route("/internal/v1/auth/decision", post(access_decision))
            .with_state(AccessState { admin }),
        certificate_pem,
        private_key_pem,
    )
    .await
}

async fn access_decision(
    State(state): State<AccessState>,
    Json(request): Json<AuthorizationDecisionRequest>,
) -> Result<Json<AuthorizationDecision>, StatusCode> {
    let valid_until = "2099-01-01T00:00:00.000Z"
        .parse()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let roles = if state.admin.load(Ordering::SeqCst) {
        vec![PlatformRole::PlatformAdmin]
    } else {
        vec![PlatformRole::Teacher]
    };
    Ok(Json(AuthorizationDecision {
        actor: AuthenticatedActor {
            actor_id: request.actor_id,
            roles,
            expires_at: valid_until,
        },
        scope: request.scope,
        authorization_revision: Revision::new(1).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
        scope_revision: Revision::new(1).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
        valid_until,
        diagnostic_code: None,
    }))
}

async fn spawn_agent_server(
    certificate_pem: &str,
    private_key_pem: &str,
    state: AgentState,
) -> Result<TlsServiceHandle, Box<dyn std::error::Error>> {
    spawn_tls_service(
        Router::new()
            .route(
                "/internal/v1/platform-images",
                get(agent_list_images).post(agent_register_image),
            )
            .route(
                "/internal/v1/platform-images/{catalog_id}/repin",
                post(agent_repin_image),
            )
            .route(
                "/internal/v1/platform-images/{catalog_id}/disable",
                post(agent_disable_image),
            )
            .with_state(state),
        certificate_pem,
        private_key_pem,
    )
    .await
}

fn record_request(state: &AgentState, route: &'static str, body: Value) -> Result<(), StatusCode> {
    let mut received = state
        .received
        .lock()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    received.push((route, body));
    Ok(())
}

async fn agent_list_images(State(state): State<AgentState>) -> Json<PlatformImageCatalog> {
    Json(PlatformImageCatalog {
        entries: state.entries.clone(),
    })
}

async fn agent_register_image(
    State(state): State<AgentState>,
    Json(request): Json<InternalPlatformImageRegistrationRequest>,
) -> Response {
    if record_request(&state, "register", json!(request)).is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    if request.binding == CONFLICT_BINDING {
        return problem(
            StatusCode::CONFLICT,
            "LW_PLATFORM_IMAGE_STATE_CONFLICT",
            false,
        );
    }
    let mut entry = state.imported.clone();
    entry.binding = request.binding;
    (StatusCode::CREATED, Json(entry)).into_response()
}

async fn agent_repin_image(
    State(state): State<AgentState>,
    Path(catalog_id): Path<PlatformImageId>,
    Json(request): Json<InternalPlatformImageRepinRequest>,
) -> Response {
    if record_request(&state, "repin", json!(request)).is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    if request.expected_digest == UNRESOLVABLE_DIGEST {
        return problem(
            StatusCode::UNPROCESSABLE_ENTITY,
            "LW_PLATFORM_IMAGE_REFERENCE_INVALID",
            false,
        );
    }
    let mut entry = state.imported.clone();
    entry.catalog_id = catalog_id;
    (StatusCode::OK, Json(entry)).into_response()
}

async fn agent_disable_image(
    State(state): State<AgentState>,
    Path(catalog_id): Path<PlatformImageId>,
    Json(request): Json<InternalPlatformImageDisableRequest>,
) -> Response {
    if record_request(&state, "disable", json!(request)).is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    let mut entry = state.imported.clone();
    entry.catalog_id = catalog_id;
    entry.status = PlatformImageStatus::Disabled;
    (StatusCode::OK, Json(entry)).into_response()
}

async fn spawn_tls_service(
    router: Router,
    certificate_pem: &str,
    private_key_pem: &str,
) -> Result<TlsServiceHandle, Box<dyn std::error::Error>> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let config = tls_config(certificate_pem, private_key_pem)?;
    let acceptor = TlsAcceptor::from(config);
    let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
    let task = tokio::spawn(async move {
        loop {
            let accepted = tokio::select! {
                result = listener.accept() => result,
                _ = &mut shutdown_rx => return,
            };
            let Ok((stream, _)) = accepted else {
                return;
            };
            let acceptor = acceptor.clone();
            let router = router.clone();
            tokio::spawn(async move {
                let Ok(stream) = acceptor.accept(stream).await else {
                    return;
                };
                let service = TowerToHyperService::new(router);
                let connection = Builder::new(TokioExecutor::new())
                    .serve_connection_with_upgrades(TokioIo::new(stream), service)
                    .into_owned();
                let _ = connection.await;
            });
        }
    });
    Ok(TlsServiceHandle {
        base_url: format!("https://localhost:{}/", address.port()).parse()?,
        shutdown: Some(shutdown_tx),
        task,
    })
}

fn tls_config(
    certificate_pem: &str,
    private_key_pem: &str,
) -> Result<Arc<ServerConfig>, Box<dyn std::error::Error>> {
    let certificates = rustls_pemfile::certs(&mut Cursor::new(certificate_pem.as_bytes()))
        .collect::<Result<Vec<_>, _>>()?;
    let key: PrivateKeyDer<'static> =
        rustls_pemfile::private_key(&mut Cursor::new(private_key_pem.as_bytes()))?
            .ok_or("private key missing")?;
    let mut config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certificates, key)?;
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(Arc::new(config))
}

fn tls_material() -> Result<(String, String, String, Value), Box<dyn std::error::Error>> {
    let ca_key = KeyPair::generate()?;
    let mut ca_parameters = CertificateParams::new(Vec::<String>::new())?;
    ca_parameters.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_parameters.key_usages = vec![
        KeyUsagePurpose::DigitalSignature,
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::CrlSign,
    ];
    let ca = CertifiedIssuer::self_signed(ca_parameters, ca_key)?;
    let mut leaf_parameters = CertificateParams::new(vec!["localhost".to_owned()])?;
    leaf_parameters.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    leaf_parameters.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    let leaf_key = KeyPair::generate()?;
    let leaf = leaf_parameters.signed_by(&leaf_key, &ca)?;
    let public = leaf_key.public_key_raw();
    let jwk = json!({
        "kty": "EC",
        "crv": "P-256",
        "x": URL_SAFE_NO_PAD.encode(&public[1..33]),
        "y": URL_SAFE_NO_PAD.encode(&public[33..65]),
        "use": "sig",
        "alg": "ES256",
        "kid": "test"
    });
    Ok((ca.pem(), leaf.pem(), leaf_key.serialize_pem(), jwk))
}

fn import_now() -> Result<UtcTimestamp, Box<dyn std::error::Error>> {
    let value = time::OffsetDateTime::now_utc();
    Ok(UtcTimestamp::from_utc(value.replace_nanosecond(
        value.nanosecond() / 1_000_000 * 1_000_000,
    )?)?)
}

async fn import_database()
-> Result<(PgPool, testcontainers::ContainerAsync<Postgres>), Box<dyn std::error::Error>> {
    let postgres = Postgres::default().with_tag("17.5-alpine").start().await?;
    let pool = PgPoolOptions::new()
        .max_connections(8)
        .connect(&format!(
            "postgres://postgres:postgres@127.0.0.1:{}/postgres",
            postgres.get_host_port_ipv4(5432).await?
        ))
        .await?;
    support::apply_domain_migrations(&pool, Domain::Control).await?;
    Ok((pool, postgres))
}

#[derive(Clone)]
struct VmCatalogState {
    pool: PgPool,
    entry: Arc<Mutex<PlatformImageEntry>>,
    unavailable: Arc<AtomicBool>,
    requests: Arc<AtomicUsize>,
    candidate_update: Arc<Mutex<Option<EnvironmentCandidate>>>,
}

async fn vm_catalog_response(
    State(state): State<VmCatalogState>,
    headers: axum::http::HeaderMap,
) -> Response {
    state.requests.fetch_add(1, Ordering::SeqCst);
    if headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        != Some(format!("Bearer {SERVICE_TOKEN}").as_str())
    {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if state.unavailable.load(Ordering::SeqCst) {
        return problem(
            StatusCode::SERVICE_UNAVAILABLE,
            "LW_SERVICE_UNAVAILABLE",
            true,
        );
    }
    let updated = match state.candidate_update.lock() {
        Ok(mut update) => update.take(),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    // Updating the same row inside the external HTTP call also checks that Control has not
    // locked it before asking the Agent for a catalog snapshot.
    if let Some(candidate) = updated {
        let Ok(revision) = i64::try_from(candidate.revision.get()) else {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        };
        let Ok(contract) = serde_json::to_value(&candidate) else {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        };
        if sqlx::query(
            "UPDATE control.candidates SET revision=$2,contract=$3 WHERE candidate_id=$1",
        )
        .bind(candidate.id.as_uuid())
        .bind(revision)
        .bind(contract)
        .execute(&state.pool)
        .await
        .is_err()
        {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    }
    let entry = match state.entry.lock() {
        Ok(entry) => entry.clone(),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    Json(PlatformImageCatalog {
        entries: vec![entry],
    })
    .into_response()
}

async fn vm_work_access_decision(
    State(owner): State<ActorId>,
    Json(request): Json<AuthorizationDecisionRequest>,
) -> Result<Json<AuthorizationDecision>, StatusCode> {
    if request.actor_id != owner {
        return Err(StatusCode::FORBIDDEN);
    }
    let mut decision = access_decision(
        State(AccessState {
            admin: Arc::new(AtomicBool::new(false)),
        }),
        Json(request),
    )
    .await?;
    decision.0.actor.roles = vec![PlatformRole::Student];
    Ok(decision)
}

fn fresh_vm_work_candidate(
    project: &Project,
    entry: &PlatformImageEntry,
) -> Result<EnvironmentCandidate, Box<dyn std::error::Error>> {
    let capacity = entry.capacity_bytes.ok_or("missing reviewed VM capacity")?;
    let candidate = EnvironmentCandidate {
        id: CandidateId::new(),
        run_id: AgentRunId::new(),
        project_id: project.id,
        course_id: project.course_id,
        revision: Revision::new(1)?,
        spec: serde_json::from_value(json!({
            "apiVersion":"environment.labweaver.io/v1", "kind":"EnvironmentSpec",
            "name":"fresh-vm-work", "class":"work",
            "resources":{"cpuMillicores":1000,"memoryBytes":2_147_483_648_u64,"storageBytes":capacity},
            "network":{"mode":"deny_all"},
            "entries":[{"name":"ssh","protocol":"ssh","servicePort":22}],
            "security":{
                "userPolicy":"non_root_required", "rootFilesystemPolicy":"mutable_required",
                "privilegeEscalationPolicy":"deny", "publicExposurePolicy":"deny",
                "securityProfileBinding":"restricted-v1"
            },
            "runtime":{
                "kind":"virtual_machine", "provider_binding":"kubevirt-primary-v1",
                "storage_class_binding":"vm-rwo-primary-v1", "ssh_port":22,
                "base_disk":{
                    "binding":entry.binding,
                    "sourceRegistryDigest":format!("docker://harbor.lab.lan/labweaver-system/{}@{}",entry.binding,entry.resolved_digest),
                    "capacityBytes":capacity
                }
            },
            "retention":{
                "policyId":PolicyId::new(), "policyRevision":1, "class":"run_evidence",
                "retainUntil":"2099-01-01T00:00:00.000Z", "disposition":"delete"
            }
        }))?,
        policy_revision: Revision::new(1)?,
        model: "fixture-provider-v1".to_owned(),
        created_at: project.created_at,
    };
    candidate.validate()?;
    Ok(candidate)
}

async fn insert_vm_work_candidate(
    pool: &PgPool,
    candidate: &EnvironmentCandidate,
    schema: Sha256Digest,
) -> Result<(), Box<dyn std::error::Error>> {
    sqlx::query(
        "INSERT INTO control.candidates
         (candidate_id,candidate_kind,project_id,course_id,run_id,revision,state,content_sha256,
          contract,policy_revision,schema_sha256,projected_event_id)
         VALUES($1,'environment',$2,$3,$4,1,'validated',$5,$6,1,$7,$8)",
    )
    .bind(candidate.id.as_uuid())
    .bind(candidate.project_id.as_uuid())
    .bind(candidate.course_id.map(CourseId::as_uuid))
    .bind(candidate.run_id.as_uuid())
    .bind(Sha256Digest::of_canonical(&candidate.spec)?.to_string())
    .bind(serde_json::to_value(candidate)?)
    .bind(schema.to_string())
    .bind(contracts::EventId::new().as_uuid())
    .execute(pool)
    .await?;
    Ok(())
}

fn vm_work_approval_request(
    candidate: &EnvironmentCandidate,
    actor: ActorId,
    session: BffSessionId,
    key: &str,
) -> Result<Request<Body>, Box<dyn std::error::Error>> {
    let mut request = admin_request(
        format!(
            "/api/v1/projects/{}/environment-candidates/{}/decisions",
            candidate.project_id, candidate.id
        ),
        "POST",
        actor,
        session,
        Some(key),
        Some(serde_json::to_vec(&CandidateDecisionRequest {
            candidate_revision: candidate.revision,
            policy_revision: candidate.policy_revision,
            trust_revision: Revision::new(1)?,
            decision: CandidateDecision::Approved,
            reason: "reviewed the imported VM base".to_owned(),
        })?),
    )?;
    request.headers_mut().insert(
        "If-Match",
        HeaderValue::from_str(&StrongEtag::from_revision(candidate.revision).header_value())?,
    );
    Ok(request)
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one Work journey exercises the PG approval fences and live HTTPS catalog authority"
)]
async fn fresh_vm_catalog_base_is_visible_approved_and_published_through_normal_work_routes()
-> Result<(), Box<dyn std::error::Error>> {
    let (pool, _postgres) = import_database().await?;
    sqlx::query(
        "DO $$ BEGIN
             IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='lw_control_runtime') THEN
                 CREATE ROLE lw_control_runtime NOLOGIN;
             END IF;
         END $$",
    )
    .execute(&pool)
    .await?;
    support::apply_domain_migrations(&pool, Domain::Access).await?;
    let now = import_now()?;
    let actor = ActorId::new();
    let session = BffSessionId::new();
    let project = Project {
        id: ProjectId::new(),
        owner_actor_id: actor,
        name: "fresh VM Work".to_owned(),
        description: None,
        course_id: Some(CourseId::new()),
        state: ProjectState::Active,
        revision: Revision::new(1)?,
        created_at: now,
        updated_at: now,
    };
    sqlx::query(
        "INSERT INTO control.projects
         (project_id,owner_actor_id,name,course_id,state,revision,created_at,updated_at,contract)
         VALUES($1,$2,$3,$4,'active',1,$5,$5,$6)",
    )
    .bind(project.id.as_uuid())
    .bind(actor.as_uuid())
    .bind(&project.name)
    .bind(project.course_id.map(CourseId::as_uuid))
    .bind(now.get())
    .bind(serde_json::to_value(&project)?)
    .execute(&pool)
    .await?;
    let control_config = config()?;
    let service = ControlService::new(
        pool.clone(),
        Arc::new(PlatformImageObjects::new(Vec::new())),
        control_config.clone(),
    )?;
    let policy: ProjectLlmEgressPolicy = serde_json::from_value(json!({
        "id":PolicyId::new(), "projectId":project.id, "courseId":project.course_id, "revision":1,
        "binding":{"runtimeBinding":"claude-code-test","model":"fixture-provider-v1","claudeCodeVersion":"2.1.207","maxInFlightPerWorker":2},
        "budget":{"maxInputTokens":100_000,"maxOutputTokens":16_000,"maxRequests":8,"maxCostMicrousd":2_000_000,"timeoutMilliseconds":120_000,"maxTransientRetries":2,"maxSchemaRepairs":2},
        "deniedDataClasses":["secret","token","private_key","personally_identifiable_information","unallowlisted_student_submission"],
        "studentContentMode":"manifest_allowlist_only", "activatedAt":now
    }))?;
    service
        .activate_project_policy(
            project.id,
            policy,
            &IdempotencyKey::parse("fresh-vm-policy")?,
            None,
        )
        .await?;
    let mut entry = platform_image_entry(
        "fresh-import-vm",
        PlatformImageKind::VirtualMachine,
        &format!("sha256:{}", Sha256Digest::of_bytes(b"fresh VM disk")),
        now,
    );
    entry.capacity_bytes = Some(21_474_836_480);
    entry.disk_sha256 = Some(Sha256Digest::of_bytes(b"qcow bytes").to_string());
    entry.format = Some(VirtualMachineDiskFormat::Qcow2);
    let mut candidate = fresh_vm_work_candidate(&project, &entry)?;
    insert_vm_work_candidate(&pool, &candidate, control_config.environment_schema_sha256).await?;
    let state = VmCatalogState {
        pool: pool.clone(),
        entry: Arc::new(Mutex::new(entry.clone())),
        unavailable: Arc::new(AtomicBool::new(false)),
        requests: Arc::new(AtomicUsize::new(0)),
        candidate_update: Arc::new(Mutex::new(None)),
    };
    let (ca, leaf, private_key, jwk) = tls_material()?;
    let temp = tempfile::tempdir()?;
    let ca_path = temp.path().join("vm-catalog-ca.pem");
    std::fs::write(&ca_path, ca)?;
    let authority = spawn_authority(jwk).await?;
    let access_server = spawn_tls_service(
        Router::new()
            .route("/internal/v1/auth/decision", post(vm_work_access_decision))
            .with_state(actor),
        &leaf,
        &private_key,
    )
    .await?;
    let agent_server = spawn_tls_service(
        Router::new()
            .route("/internal/v1/platform-images", get(vm_catalog_response))
            .with_state(state.clone()),
        &leaf,
        &private_key,
    )
    .await?;
    let tokens = Arc::new(service_token_client(&authority.issuer).await?);
    let verifier = ServiceTokenVerifier::discover(
        ServiceAuthConfig::new(
            &authority.issuer,
            "labweaver-agent".to_owned(),
            BTreeSet::from(["control-service".to_owned()]),
            BTreeSet::new(),
            BTreeSet::from(["ES256".to_owned()]),
            3_600,
            1,
            TransportSecurityMode::InsecureTestOnly,
        )?,
        no_redirect_http_client(None, TransportSecurityMode::InsecureTestOnly)?,
    )
    .await?;
    let app = router(Arc::new(ApiState {
        control: service.clone(),
        access: control_service::clients::AccessClient::new_authenticated(
            service_config(&access_server.base_url, &ca_path),
            Arc::clone(&tokens),
        )?,
        agent: control_service::clients::AgentClient::new_authenticated(
            service_config(&agent_server.base_url, &ca_path),
            Arc::clone(&tokens),
        )?,
        environment: control_service::clients::EnvironmentClient::new_authenticated(
            service_config(&agent_server.base_url, &ca_path),
            Arc::clone(&tokens),
        )?,
        evaluation: control_service::clients::EvaluationClient::new_authenticated(
            service_config(&agent_server.base_url, &ca_path),
            tokens,
        )?,
        service_token_verifier: Arc::new(verifier),
    }))
    .layer(axum::Extension(GatewayPrincipal {
        client_id: "access-gateway".to_owned(),
    }));
    let candidate_uri = format!(
        "/api/v1/projects/{}/environment-candidates/{}",
        project.id, candidate.id
    );

    let denied = app
        .clone()
        .oneshot(admin_request(
            candidate_uri.clone(),
            "GET",
            ActorId::new(),
            session,
            None,
            None,
        )?)
        .await?;
    assert_eq!(denied.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        state.requests.load(Ordering::SeqCst),
        0,
        "unauthorized reads must not request the Agent catalog"
    );
    let response = app
        .clone()
        .oneshot(admin_request(
            candidate_uri.clone(),
            "GET",
            actor,
            session,
            None,
            None,
        )?)
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let view: EnvironmentCandidateView =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await?)?;
    let contracts::authoring::EnvironmentRuntimeSpec::VirtualMachine { base_disk, .. } =
        &candidate.spec.runtime
    else {
        return Err("fixture must be a VM".into());
    };
    let expected_artifact = ImageArtifact::VirtualMachine {
        id: entry.catalog_id.to_string().parse()?,
        base_disk: base_disk.clone(),
        format: VirtualMachineDiskFormat::Qcow2,
    };
    assert_eq!(view.image_artifact, Some(expected_artifact.clone()));
    assert!(view.build.is_none(), "VM approval needs no container build");
    assert_eq!(
        service
            .environment_candidate_view(
                project.course_id.ok_or("missing course")?,
                candidate.id,
                std::slice::from_ref(&entry)
            )
            .await?
            .image_artifact,
        Some(expected_artifact.clone())
    );

    let mut disabled = entry.clone();
    disabled.status = PlatformImageStatus::Disabled;
    let mut stale_trust = entry.clone();
    stale_trust.trust_revision += 1;
    let mut repository_drift = entry.clone();
    repository_drift.source_reference =
        "harbor.lab.lan/other-project/fresh-import-vm:v1".to_owned();
    let mut digest_drift = entry.clone();
    digest_drift.resolved_digest =
        format!("sha256:{}", Sha256Digest::of_bytes(b"different manifest"));
    let mut capacity_drift = entry.clone();
    capacity_drift.capacity_bytes = Some(21_474_836_481);
    let mut format_missing = entry.clone();
    format_missing.format = None;
    let mut inventory_only = entry.clone();
    inventory_only.disk_sha256 = None;
    for (index, drifted) in [
        disabled,
        stale_trust,
        repository_drift,
        digest_drift,
        capacity_drift,
        format_missing,
        inventory_only,
    ]
    .into_iter()
    .enumerate()
    {
        *state.entry.lock().map_err(|_| "catalog lock poisoned")? = drifted;
        let response = app
            .clone()
            .oneshot(admin_request(
                candidate_uri.clone(),
                "GET",
                actor,
                session,
                None,
                None,
            )?)
            .await?;
        assert_eq!(
            response.status(),
            StatusCode::CONFLICT,
            "invalid catalog descriptor {index}"
        );
        assert_eq!(
            problem_body(response).await?.diagnostic_code.as_str(),
            "LW_RELEASE_ARTIFACT_MISMATCH"
        );
        let response = app
            .clone()
            .oneshot(vm_work_approval_request(
                &candidate,
                actor,
                session,
                &format!("reject-vm-{index}"),
            )?)
            .await?;
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert_eq!(
            problem_body(response).await?.diagnostic_code.as_str(),
            "LW_RELEASE_ARTIFACT_MISMATCH"
        );
    }
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM control.candidate_approvals")
            .fetch_one(&pool)
            .await?,
        0
    );
    *state.entry.lock().map_err(|_| "catalog lock poisoned")? = entry.clone();
    state.unavailable.store(true, Ordering::SeqCst);
    let response = app
        .clone()
        .oneshot(admin_request(
            candidate_uri.clone(),
            "GET",
            actor,
            session,
            None,
            None,
        )?)
        .await?;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(problem_body(response).await?.retryable);
    state.unavailable.store(false, Ordering::SeqCst);

    let stale_request = vm_work_approval_request(&candidate, actor, session, "vm-revision-race")?;
    candidate.revision = Revision::new(2)?;
    *state
        .candidate_update
        .lock()
        .map_err(|_| "candidate update lock poisoned")? = Some(candidate.clone());
    let response = app.clone().oneshot(stale_request).await?;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(
        problem_body(response).await?.diagnostic_code.as_str(),
        "LW_REVISION_CONFLICT"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM control.candidate_approvals")
            .fetch_one(&pool)
            .await?,
        0
    );
    let response = app
        .clone()
        .oneshot(vm_work_approval_request(
            &candidate,
            actor,
            session,
            "approve-fresh-vm",
        )?)
        .await?;
    assert_eq!(response.status(), StatusCode::CREATED);
    let approval: CandidateApproval =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await?)?;
    assert_eq!(approval.candidate_revision, candidate.revision);
    let publish = CreateEnvironmentTemplateReleaseRequest {
        project_id: project.id,
        course_id: project.course_id,
        candidate_id: candidate.id,
        candidate_revision: candidate.revision,
        runtime_kind: RuntimeKind::VirtualMachine,
        approval_id: approval.id,
    };
    let publish_uri = format!(
        "/api/v1/projects/{}/environment-template-releases",
        project.id
    );
    state
        .entry
        .lock()
        .map_err(|_| "catalog lock poisoned")?
        .status = PlatformImageStatus::Disabled;
    let response = app
        .clone()
        .oneshot(admin_request(
            publish_uri.clone(),
            "POST",
            actor,
            session,
            Some("publish-disabled-vm"),
            Some(serde_json::to_vec(&publish)?),
        )?)
        .await?;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(
        problem_body(response).await?.diagnostic_code.as_str(),
        "LW_RELEASE_ARTIFACT_MISMATCH"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM control.environment_template_releases")
            .fetch_one(&pool)
            .await?,
        0
    );
    *state.entry.lock().map_err(|_| "catalog lock poisoned")? = entry;
    let response = app
        .clone()
        .oneshot(admin_request(
            publish_uri,
            "POST",
            actor,
            session,
            Some("publish-fresh-vm"),
            Some(serde_json::to_vec(&publish)?),
        )?)
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let accepted: OperationAccepted =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await?)?;
    let response = app
        .oneshot(admin_request(
            accepted.status_url,
            "GET",
            actor,
            session,
            None,
            None,
        )?)
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let release_view: EnvironmentTemplateReleaseView =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await?)?;
    let release = release_view.release;
    assert_eq!(release.artifact, expected_artifact);
    assert_eq!(release.candidate_revision, candidate.revision);
    release.validate()?;
    Ok(())
}

fn upload_request(binding: &str) -> CreatePlatformImageUploadRequest {
    CreatePlatformImageUploadRequest {
        kind: PlatformImageKind::Container,
        binding: binding.to_owned(),
        target_reference: format!("harbor.lab.lan/labweaver-system/{binding}:v1"),
        archive_bytes: 5_000_000_000,
        archive_media_type: PLATFORM_IMAGE_ARCHIVE_MEDIA_TYPE.to_owned(),
        disk_format: None,
        disk_path: None,
        capacity_bytes: None,
        trust_revision: 1,
        reason: "reviewed large archive".to_owned(),
    }
}

fn completion_request(session: &PlatformImageUploadSession) -> CompletePlatformImageUploadRequest {
    CompletePlatformImageUploadRequest {
        parts: session
            .upload_target
            .parts
            .iter()
            .map(|part| CompletePlatformImageUploadPart {
                part_number: part.part_number,
                etag: format!("etag-{}", part.part_number),
            })
            .collect(),
    }
}

#[tokio::test]
async fn platform_image_status_recovers_lost_multipart_creation_response()
-> Result<(), Box<dyn std::error::Error>> {
    let (pool, _postgres) = import_database().await?;
    let objects = Arc::new(PlatformImageObjects::new(Vec::new()));
    let control = ControlService::new(pool.clone(), objects.clone(), config()?)?;
    let actor = ActorId::new();
    let request = upload_request("recover-lost-create");
    let create_key = IdempotencyKey::parse("create-recover-lost")?;
    let session = control
        .create_platform_image_upload(actor, &request, &create_key, import_now()?)
        .await?;
    let object_key = format!(
        "problem-packages/platform-image-uploads/{}",
        session.upload_id
    );
    let active_upload = objects
        .find_platform_image_multipart_uploads(&object_key)
        .await?
        .into_iter()
        .next()
        .ok_or("fake multipart upload missing")?;
    sqlx::query(
        "UPDATE control.platform_image_upload_sessions
            SET multipart_upload_id=NULL,multipart_part_size_bytes=NULL,
                multipart_part_count=NULL,multipart_creation_lease_token=NULL,
                multipart_creation_lease_expires_at=NULL
          WHERE upload_id=$1",
    )
    .bind(session.upload_id.as_uuid())
    .execute(&pool)
    .await?;
    sqlx::query(
        "UPDATE control.idempotency_ledger
            SET state='in_progress',result=NULL,completed_at=NULL
          WHERE operation='control_create_platform_image_upload_v1'
            AND idempotency_key=$1",
    )
    .bind(create_key.as_str())
    .execute(&pool)
    .await?;

    let recovered = control
        .platform_image_upload_status(session.upload_id)
        .await?;
    assert_eq!(recovered.state, PlatformImageUploadState::Pending);
    let target = recovered
        .upload_target
        .as_ref()
        .ok_or("recovered upload target missing")?;
    assert_eq!(
        target.part_size_bytes,
        PLATFORM_IMAGE_UPLOAD_PART_SIZE_BYTES
    );
    assert_eq!(
        recovered.uploaded_parts.len(),
        session.upload_target.parts.len()
    );
    assert_eq!(
        sqlx::query_scalar::<_, Option<String>>(
            "SELECT multipart_upload_id FROM control.platform_image_upload_sessions WHERE upload_id=$1",
        )
        .bind(session.upload_id.as_uuid())
        .fetch_one(&pool)
        .await?,
        Some(active_upload)
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            "SELECT state FROM control.idempotency_ledger
              WHERE operation='control_create_platform_image_upload_v1' AND idempotency_key=$1",
        )
        .bind(create_key.as_str())
        .fetch_one(&pool)
        .await?,
        "completed"
    );
    Ok(())
}

#[tokio::test]
async fn expired_lost_multipart_creation_is_aborted_without_idempotency_recreation()
-> Result<(), Box<dyn std::error::Error>> {
    let (pool, _postgres) = import_database().await?;
    let objects = Arc::new(PlatformImageObjects::new(Vec::new()));
    let control = ControlService::new(pool.clone(), objects.clone(), config()?)?;
    let transport = ImportTransportState {
        accepted: Arc::new(Mutex::new(None)),
        failures: Arc::new(AtomicUsize::new(0)),
        cancelled: Arc::new(AtomicBool::new(false)),
    };
    let (agent, _server, _authority, _temp) = worker_transport(transport).await?;
    let worker = control_service::platform_image_jobs::PlatformImageImportWorker {
        control: control.clone(),
        agent,
        poll_interval: std::time::Duration::from_millis(10),
    };
    let actor = ActorId::new();
    let request = upload_request("expire-lost-create");
    let create_key = IdempotencyKey::parse("create-expire-lost")?;
    let session = control
        .create_platform_image_upload(actor, &request, &create_key, import_now()?)
        .await?;
    let object_key = format!(
        "problem-packages/platform-image-uploads/{}",
        session.upload_id
    );
    let _active_upload = objects
        .find_platform_image_multipart_uploads(&object_key)
        .await?
        .into_iter()
        .next()
        .ok_or("fake multipart upload missing")?;
    sqlx::query(
        "UPDATE control.platform_image_upload_sessions
            SET multipart_upload_id=NULL,multipart_part_size_bytes=NULL,
                multipart_part_count=NULL,multipart_creation_lease_token=NULL,
                multipart_creation_lease_expires_at=NULL,
                expires_at=date_trunc('milliseconds',clock_timestamp())-interval '1 second'
          WHERE upload_id=$1",
    )
    .bind(session.upload_id.as_uuid())
    .execute(&pool)
    .await?;
    sqlx::query(
        "UPDATE control.idempotency_ledger
            SET state='in_progress',result=NULL,completed_at=NULL
          WHERE operation='control_create_platform_image_upload_v1'
            AND idempotency_key=$1",
    )
    .bind(create_key.as_str())
    .execute(&pool)
    .await?;

    worker.tick().await?;
    let failed = control
        .platform_image_upload_status(session.upload_id)
        .await?;
    assert_eq!(failed.state, PlatformImageUploadState::Failed);
    assert_eq!(
        failed.diagnostic.as_deref(),
        Some("LW_PLATFORM_IMAGE_UPLOAD_EXPIRED")
    );
    assert!(
        objects
            .find_platform_image_multipart_uploads(&object_key)
            .await?
            .is_empty()
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            "SELECT state FROM control.idempotency_ledger
              WHERE operation='control_create_platform_image_upload_v1' AND idempotency_key=$1",
        )
        .bind(create_key.as_str())
        .fetch_one(&pool)
        .await?,
        "in_progress"
    );
    assert!(matches!(
        control
            .create_platform_image_upload(actor, &request, &create_key, import_now()?)
            .await,
        Err(control_service::ControlError::PlatformImageUploadExpired)
    ));
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM control.platform_image_upload_sessions
              WHERE create_idempotency_key=$1",
        )
        .bind(create_key.as_str())
        .fetch_one(&pool)
        .await?,
        1
    );
    Ok(())
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one persisted upload exercises stale-owner fencing, restart and late cancellation in order"
)]
async fn control_import_restart_fences_old_workers_and_freezes_a_cancelled_late_upload()
-> Result<(), Box<dyn std::error::Error>> {
    let (pool, _postgres) = import_database().await?;
    let objects = Arc::new(PlatformImageObjects::new(Vec::new()));
    let control = ControlService::new(pool.clone(), objects.clone(), config()?)?;
    let actor = ActorId::new();
    let request = upload_request("late-upload");
    let session = control
        .create_platform_image_upload(
            actor,
            &request,
            &IdempotencyKey::parse("create-late")?,
            import_now()?,
        )
        .await?;
    assert_eq!(session.archive_bytes, 5_000_000_000);
    let queued = control
        .queue_platform_image_completion(
            actor,
            session.upload_id,
            &completion_request(&session),
            &IdempotencyKey::parse("complete-late")?,
            import_now()?,
        )
        .await?;
    assert_eq!(
        control
            .queue_platform_image_completion(
                actor,
                session.upload_id,
                &completion_request(&session),
                &IdempotencyKey::parse("complete-late")?,
                import_now()?
            )
            .await?,
        queued
    );
    let cancel = control
        .cancel_platform_image_upload(
            actor,
            session.upload_id,
            &CancelPlatformImageUploadRequest {
                expected_revision: queued.revision,
            },
            &IdempotencyKey::parse("cancel-late")?,
            import_now()?,
        )
        .await?;
    assert_eq!(cancel.state, PlatformImageUploadState::Cancelling);
    assert_eq!(
        control
            .platform_image_upload_status(session.upload_id)
            .await?
            .state,
        PlatformImageUploadState::Cancelling
    );
    let first = control
        .claim_platform_image_import(import_now()?)
        .await?
        .ok_or("cancelled upload not claimed")?;
    assert!(first.archive.is_none());
    assert!(first.cancel_requested);
    let reference = objects
        .freeze_current_reference(
            &first.archive_object_key,
            first.archive_size,
            &first.archive_media_type,
        )
        .await?;
    control
        .record_platform_image_import_reference(&first, &reference, import_now()?)
        .await?;
    control
        .renew_platform_image_import(&first, import_now()?)
        .await?;
    sqlx::query("UPDATE control.platform_image_upload_sessions SET completion_lease_expires_at=clock_timestamp()-interval '1 second' WHERE upload_id=$1").bind(session.upload_id.as_uuid()).execute(&pool).await?;
    assert!(matches!(
        control
            .cancel_platform_image_import_fenced(&first, import_now()?)
            .await,
        Err(control_service::ControlError::OperationLeaseLost)
    ));
    assert!(matches!(
        control
            .renew_platform_image_import(&first, import_now()?)
            .await,
        Err(control_service::ControlError::OperationLeaseLost)
    ));
    let restarted = ControlService::new(pool.clone(), objects, config()?)?;
    let current = restarted
        .claim_platform_image_import(import_now()?)
        .await?
        .ok_or("expired lease not recovered")?;
    assert_ne!(first.lease_token, current.lease_token);
    assert_eq!(current.archive, Some(reference.clone()));
    assert!(matches!(
        control
            .finish_platform_image_import_fenced(&first, PlatformImageId::new(), import_now()?)
            .await,
        Err(control_service::ControlError::OperationLeaseLost)
    ));
    restarted
        .cancel_platform_image_import_fenced(&current, import_now()?)
        .await?;
    let terminal = restarted
        .platform_image_upload_status(session.upload_id)
        .await?;
    assert_eq!(terminal.state, PlatformImageUploadState::Cancelled);
    let cleanup = sqlx::query("SELECT object_key,object_version,next_attempt_at FROM control.object_cleanup_ledger WHERE upload_id=$1").bind(session.upload_id.as_uuid()).fetch_one(&pool).await?;
    assert_eq!(
        sqlx::Row::try_get::<String, _>(&cleanup, "object_key")?,
        current.archive_object_key
    );
    assert_eq!(
        sqlx::Row::try_get::<String, _>(&cleanup, "object_version")?,
        reference.object_version
    );
    assert!(
        sqlx::Row::try_get::<time::OffsetDateTime, _>(&cleanup, "next_attempt_at")?
            >= session.expires_at.get()
    );
    assert!(matches!(
        restarted.cleanup_one_object(import_now()?).await?,
        control_service::CleanupOutcome::Idle
    ));
    Ok(())
}

#[tokio::test]
async fn import_migration_preserves_pending_running_and_terminal_rows()
-> Result<(), Box<dyn std::error::Error>> {
    let postgres = Postgres::default().with_tag("17.5-alpine").start().await?;
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&format!(
            "postgres://postgres:postgres@127.0.0.1:{}/postgres",
            postgres.get_host_port_ipv4(5432).await?
        ))
        .await?;
    sqlx::query("CREATE SCHEMA control").execute(&pool).await?;
    let mut connection = pool.acquire().await?;
    sqlx::query("SET search_path=control,pg_catalog")
        .execute(&mut *connection)
        .await?;
    let migration_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../migrations");
    let catalog = persistence_sqlx::MigrationCatalog::load(&migration_root.join("catalog.yaml"))?;
    let migrations = &catalog
        .domains
        .iter()
        .find(|domain| domain.name == Domain::Control)
        .ok_or("control migrations absent")?
        .migrations;
    for migration in migrations.iter().filter(|migration| migration.id < 11) {
        sqlx::raw_sql(&persistence_sqlx::MigrationCatalog::read_verified_sql(
            &migration_root,
            migration,
        )?)
        .execute(&mut *connection)
        .await?;
    }
    for state in ["pending", "importing", "imported", "failed"] {
        let upload_id = UploadSessionId::new();
        let lease = (state == "importing").then(uuid::Uuid::now_v7);
        sqlx::query("INSERT INTO control.platform_image_upload_sessions (upload_id,created_by,kind,binding,target_reference,trust_revision,reason,archive_bytes,archive_media_type,object_key,state,expires_at,completion_lease_token,completion_lease_expires_at,imported_catalog_id) VALUES ($1,$2,'container',$3,'registry.test/platform/base:v1',1,'reviewed',10,'application/vnd.oci.image.layout.v1.tar',$4,$5,now()+interval '15 minutes',$6,CASE WHEN $6::uuid IS NOT NULL THEN now()-interval '1 second' END,$7)")
            .bind(upload_id.as_uuid()).bind(ActorId::new().as_uuid()).bind(format!("migration-{state}")).bind(format!("archive/{upload_id}")).bind(state).bind(lease).bind((state == "imported").then(|| PlatformImageId::new().as_uuid()))
            .execute(&mut *connection).await?;
    }
    let migration = migrations
        .iter()
        .find(|migration| migration.id == 11)
        .ok_or("new import migration absent")?;
    sqlx::raw_sql(&persistence_sqlx::MigrationCatalog::read_verified_sql(
        &migration_root,
        migration,
    )?)
    .execute(&mut *connection)
    .await?;
    let states = sqlx::query_as::<_, (String, i64, bool)>("SELECT state,revision,cancel_requested FROM control.platform_image_upload_sessions ORDER BY state").fetch_all(&mut *connection).await?;
    assert_eq!(
        states,
        vec![
            ("failed".to_owned(), 1, false),
            ("imported".to_owned(), 1, false),
            ("importing".to_owned(), 1, false),
            ("pending".to_owned(), 1, false)
        ]
    );
    Ok(())
}

#[derive(Clone)]
struct ImportTransportState {
    accepted: Arc<Mutex<Option<InternalPlatformImageImportEnqueueRequest>>>,
    failures: Arc<AtomicUsize>,
    cancelled: Arc<AtomicBool>,
}

async fn transport_enqueue(
    State(state): State<ImportTransportState>,
    Json(request): Json<InternalPlatformImageImportEnqueueRequest>,
) -> Result<Response, StatusCode> {
    {
        let mut accepted = state
            .accepted
            .lock()
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        if accepted
            .as_ref()
            .is_some_and(|previous| previous != &request)
        {
            return Err(StatusCode::CONFLICT);
        }
        *accepted = Some(request.clone());
    }
    if state
        .failures
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
        .is_ok()
    {
        return Err(StatusCode::BAD_GATEWAY);
    }
    Ok((
        StatusCode::ACCEPTED,
        Json(InternalPlatformImageImportJobStatus {
            upload_id: request.upload_id,
            state: PlatformImageImportJobState::Running,
            revision: Revision::new(2).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
            diagnostic: None,
            catalog_id: None,
        }),
    )
        .into_response())
}

async fn transport_cancel(
    State(state): State<ImportTransportState>,
    Path(upload_id): Path<UploadSessionId>,
) -> Result<Response, StatusCode> {
    let accepted = state
        .accepted
        .lock()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if accepted
        .as_ref()
        .is_none_or(|request| request.upload_id != upload_id)
    {
        return Err(StatusCode::NOT_FOUND);
    }
    state.cancelled.store(true, Ordering::SeqCst);
    Ok((
        StatusCode::ACCEPTED,
        Json(InternalPlatformImageImportJobStatus {
            upload_id,
            state: PlatformImageImportJobState::Cancelled,
            revision: Revision::new(3).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
            diagnostic: Some("LW_PLATFORM_IMAGE_IMPORT_CANCELLED".to_owned()),
            catalog_id: None,
        }),
    )
        .into_response())
}

async fn worker_transport(
    state: ImportTransportState,
) -> Result<
    (
        control_service::clients::AgentClient,
        TlsServiceHandle,
        AuthorityHandle,
        tempfile::TempDir,
    ),
    Box<dyn std::error::Error>,
> {
    let (ca, certificate, key, jwk) = tls_material()?;
    let temp = tempfile::tempdir()?;
    let ca_path = temp.path().join("agent-ca.pem");
    std::fs::write(&ca_path, ca)?;
    let authority = spawn_authority(jwk).await?;
    let server = spawn_tls_service(
        Router::new()
            .route(
                "/internal/v1/platform-images/import-jobs",
                post(transport_enqueue),
            )
            .route(
                "/internal/v1/platform-images/import-jobs/{upload_id}/cancel",
                post(transport_cancel),
            )
            .with_state(state),
        &certificate,
        &key,
    )
    .await?;
    let token = Arc::new(service_token_client(&authority.issuer).await?);
    let client = control_service::clients::AgentClient::new_authenticated(
        service_config(&server.base_url, &ca_path),
        token,
    )?;
    Ok((client, server, authority, temp))
}

#[tokio::test]
async fn unknown_agent_enqueue_response_recovers_the_same_job_before_cancelling_and_cleaning()
-> Result<(), Box<dyn std::error::Error>> {
    let (pool, _postgres) = import_database().await?;
    let objects = Arc::new(PlatformImageObjects::new(Vec::new()));
    let control = ControlService::new(pool.clone(), objects.clone(), config()?)?;
    let transport = ImportTransportState {
        accepted: Arc::new(Mutex::new(None)),
        failures: Arc::new(AtomicUsize::new(3)),
        cancelled: Arc::new(AtomicBool::new(false)),
    };
    let (agent, _server, _authority, _temp) = worker_transport(transport.clone()).await?;
    let worker = control_service::platform_image_jobs::PlatformImageImportWorker {
        control: control.clone(),
        agent: agent.clone(),
        poll_interval: std::time::Duration::from_millis(10),
    };
    let actor = ActorId::new();
    let session = control
        .create_platform_image_upload(
            actor,
            &upload_request("lost-response"),
            &IdempotencyKey::parse("create-lost")?,
            import_now()?,
        )
        .await?;
    control
        .queue_platform_image_completion(
            actor,
            session.upload_id,
            &completion_request(&session),
            &IdempotencyKey::parse("complete-lost")?,
            import_now()?,
        )
        .await?;
    tokio::time::timeout(std::time::Duration::from_secs(5), worker.tick()).await??;
    let uncertain = control
        .platform_image_upload_status(session.upload_id)
        .await?;
    assert_eq!(uncertain.state, PlatformImageUploadState::Importing);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM control.object_cleanup_ledger WHERE upload_id=$1"
        )
        .bind(session.upload_id.as_uuid())
        .fetch_one(&pool)
        .await?,
        0
    );
    let original = transport
        .accepted
        .lock()
        .map_err(|_| "fixture lock poisoned")?
        .clone()
        .ok_or("Agent did not receive the job")?;
    sqlx::query("UPDATE control.platform_image_upload_sessions SET completion_lease_expires_at=clock_timestamp()-interval '1 second' WHERE upload_id=$1").bind(session.upload_id.as_uuid()).execute(&pool).await?;
    control
        .cancel_platform_image_upload(
            actor,
            session.upload_id,
            &CancelPlatformImageUploadRequest {
                expected_revision: uncertain.revision,
            },
            &IdempotencyKey::parse("cancel-lost")?,
            import_now()?,
        )
        .await?;
    let recovered = control_service::platform_image_jobs::PlatformImageImportWorker {
        control: ControlService::new(pool.clone(), objects, config()?)?,
        agent,
        poll_interval: std::time::Duration::from_millis(10),
    };
    tokio::time::timeout(std::time::Duration::from_secs(5), recovered.tick()).await??;
    assert_eq!(
        *transport
            .accepted
            .lock()
            .map_err(|_| "fixture lock poisoned")?,
        Some(original)
    );
    assert!(transport.cancelled.load(Ordering::SeqCst));
    assert_eq!(
        control
            .platform_image_upload_status(session.upload_id)
            .await?
            .state,
        PlatformImageUploadState::Cancelled
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM control.object_cleanup_ledger WHERE upload_id=$1"
        )
        .bind(session.upload_id.as_uuid())
        .fetch_one(&pool)
        .await?,
        1
    );
    Ok(())
}

#[tokio::test]
async fn pending_cancel_aborts_multipart_without_creating_an_agent_job()
-> Result<(), Box<dyn std::error::Error>> {
    let (pool, _postgres) = import_database().await?;
    let objects = Arc::new(PlatformImageObjects::new(Vec::new()));
    let control = ControlService::new(pool.clone(), objects.clone(), config()?)?;
    let transport = ImportTransportState {
        accepted: Arc::new(Mutex::new(None)),
        failures: Arc::new(AtomicUsize::new(0)),
        cancelled: Arc::new(AtomicBool::new(false)),
    };
    let (agent, _server, _authority, _temp) = worker_transport(transport.clone()).await?;
    let worker = control_service::platform_image_jobs::PlatformImageImportWorker {
        control: control.clone(),
        agent,
        poll_interval: std::time::Duration::from_millis(10),
    };
    let actor = ActorId::new();
    let session = control
        .create_platform_image_upload(
            actor,
            &upload_request("pending-cancel"),
            &IdempotencyKey::parse("create-pending")?,
            import_now()?,
        )
        .await?;
    control
        .cancel_platform_image_upload(
            actor,
            session.upload_id,
            &CancelPlatformImageUploadRequest {
                expected_revision: session.revision,
            },
            &IdempotencyKey::parse("cancel-pending")?,
            import_now()?,
        )
        .await?;
    worker.tick().await?;
    assert_eq!(
        control
            .platform_image_upload_status(session.upload_id)
            .await?
            .state,
        PlatformImageUploadState::Cancelled
    );
    assert!(
        transport
            .accepted
            .lock()
            .map_err(|_| "fixture lock poisoned")?
            .is_none()
    );
    let versions: Vec<String> = sqlx::query_scalar(
        "SELECT object_version FROM control.object_cleanup_ledger WHERE upload_id=$1",
    )
    .bind(session.upload_id.as_uuid())
    .fetch_all(&pool)
    .await?;
    assert!(versions.is_empty());
    Ok(())
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one expired upload keeps abort and exact late-version cleanup ordering together"
)]
async fn expired_upload_aborts_before_terminal_cleanup_and_does_not_create_an_agent_job()
-> Result<(), Box<dyn std::error::Error>> {
    let (pool, _postgres) = import_database().await?;
    let objects = Arc::new(PlatformImageObjects::new(Vec::new()));
    objects.read_failure.store(true, Ordering::SeqCst);
    objects
        .versions
        .lock()
        .map_err(|_| "fixture lock poisoned")?
        .clear();
    let control = ControlService::new(pool.clone(), objects.clone(), config()?)?;
    let transport = ImportTransportState {
        accepted: Arc::new(Mutex::new(None)),
        failures: Arc::new(AtomicUsize::new(0)),
        cancelled: Arc::new(AtomicBool::new(false)),
    };
    let (agent, _server, _authority, _temp) = worker_transport(transport.clone()).await?;
    let worker = control_service::platform_image_jobs::PlatformImageImportWorker {
        control: control.clone(),
        agent,
        poll_interval: std::time::Duration::from_millis(10),
    };
    let actor = ActorId::new();
    let session = control
        .create_platform_image_upload(
            actor,
            &upload_request("expired-pending-cancel"),
            &IdempotencyKey::parse("create-expired-pending")?,
            import_now()?,
        )
        .await?;
    control
        .cancel_platform_image_upload(
            actor,
            session.upload_id,
            &CancelPlatformImageUploadRequest {
                expected_revision: session.revision,
            },
            &IdempotencyKey::parse("cancel-expired-pending")?,
            import_now()?,
        )
        .await?;
    sqlx::query("UPDATE control.platform_image_upload_sessions SET expires_at=date_trunc('milliseconds',clock_timestamp())-interval '1 second',created_at=date_trunc('milliseconds',clock_timestamp())-interval '2 seconds' WHERE upload_id=$1")
        .bind(session.upload_id.as_uuid()).execute(&pool).await?;
    worker.tick().await?;
    assert_eq!(
        control
            .platform_image_upload_status(session.upload_id)
            .await?
            .state,
        PlatformImageUploadState::Cancelled
    );
    objects.read_failure.store(false, Ordering::SeqCst);
    objects.read_failure.store(true, Ordering::SeqCst);
    assert!(worker.tick().await.is_err());
    assert!(sqlx::query_scalar::<_, bool>("SELECT cleanup_versions_next_attempt_at>clock_timestamp() FROM control.platform_image_upload_sessions WHERE upload_id=$1")
        .bind(session.upload_id.as_uuid()).fetch_one(&pool).await?);
    objects.read_failure.store(false, Ordering::SeqCst);
    sqlx::query("UPDATE control.platform_image_upload_sessions SET cleanup_versions_next_attempt_at=clock_timestamp()-interval '1 second' WHERE upload_id=$1")
        .bind(session.upload_id.as_uuid()).execute(&pool).await?;
    worker.tick().await?;
    assert!(sqlx::query_scalar::<_, bool>("SELECT cleanup_versions_next_attempt_at>clock_timestamp() FROM control.platform_image_upload_sessions WHERE upload_id=$1")
        .bind(session.upload_id.as_uuid()).fetch_one(&pool).await?);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM control.object_cleanup_ledger WHERE upload_id=$1"
        )
        .bind(session.upload_id.as_uuid())
        .fetch_one(&pool)
        .await?,
        0
    );
    *objects
        .versions
        .lock()
        .map_err(|_| "fixture lock poisoned")? = vec!["put-committed-after-empty-scan".to_owned()];
    sqlx::query("UPDATE control.platform_image_upload_sessions SET cleanup_versions_next_attempt_at=clock_timestamp()-interval '1 second' WHERE upload_id=$1")
        .bind(session.upload_id.as_uuid()).execute(&pool).await?;
    worker.tick().await?;
    let versions: Vec<String> = sqlx::query_scalar(
        "SELECT object_version FROM control.object_cleanup_ledger WHERE upload_id=$1",
    )
    .bind(session.upload_id.as_uuid())
    .fetch_all(&pool)
    .await?;
    assert_eq!(versions, vec!["put-committed-after-empty-scan".to_owned()]);
    assert!(
        transport
            .accepted
            .lock()
            .map_err(|_| "fixture lock poisoned")?
            .is_none()
    );
    Ok(())
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one worker exercises multipart expiry and an accepted completion after the deadline"
)]
async fn abandoned_multipart_uploads_abort_and_accepted_completion_survives_expiry()
-> Result<(), Box<dyn std::error::Error>> {
    let (pool, _postgres) = import_database().await?;
    let objects = Arc::new(PlatformImageObjects::new(Vec::new()));
    let control = ControlService::new(pool.clone(), objects.clone(), config()?)?;
    let transport = ImportTransportState {
        accepted: Arc::new(Mutex::new(None)),
        failures: Arc::new(AtomicUsize::new(0)),
        cancelled: Arc::new(AtomicBool::new(false)),
    };
    let (agent, _server, _authority, _temp) = worker_transport(transport.clone()).await?;
    let worker = control_service::platform_image_jobs::PlatformImageImportWorker {
        control: control.clone(),
        agent,
        poll_interval: std::time::Duration::from_millis(10),
    };
    let actor = ActorId::new();
    for binding in ["orphan-put", "orphan-empty"] {
        let session = control
            .create_platform_image_upload(
                actor,
                &upload_request(binding),
                &IdempotencyKey::parse(&format!("create-{binding}"))?,
                import_now()?,
            )
            .await?;
        sqlx::query("UPDATE control.platform_image_upload_sessions SET expires_at=date_trunc('milliseconds',clock_timestamp())-interval '1 second',created_at=date_trunc('milliseconds',clock_timestamp())-interval '2 seconds' WHERE upload_id=$1")
            .bind(session.upload_id.as_uuid()).execute(&pool).await?;
        assert!(matches!(
            control
                .queue_platform_image_completion(
                    actor,
                    session.upload_id,
                    &completion_request(&session),
                    &IdempotencyKey::parse(&format!("expired-{binding}"))?,
                    import_now()?,
                )
                .await,
            Err(control_service::ControlError::PlatformImageUploadExpired)
        ));
        worker.tick().await?;
        let status = control
            .platform_image_upload_status(session.upload_id)
            .await?;
        assert_eq!(status.state, PlatformImageUploadState::Cancelled);
        assert_eq!(status.revision.get(), session.revision.get() + 3);
        let versions: Vec<String> = sqlx::query_scalar(
            "SELECT object_version FROM control.object_cleanup_ledger WHERE upload_id=$1",
        )
        .bind(session.upload_id.as_uuid())
        .fetch_all(&pool)
        .await?;
        assert!(versions.is_empty());
        assert!(
            transport
                .accepted
                .lock()
                .map_err(|_| "fixture lock poisoned")?
                .is_none()
        );
        worker.tick().await?;
        assert_eq!(
            control
                .platform_image_upload_status(session.upload_id)
                .await?,
            status
        );
        assert_eq!(
            control
                .platform_image_upload_status(session.upload_id)
                .await?,
            status
        );
    }
    // Accepted completion owns the job independently of the PUT deadline.
    let session = control
        .create_platform_image_upload(
            actor,
            &upload_request("accepted-before-expiry"),
            &IdempotencyKey::parse("create-accepted-expiry")?,
            import_now()?,
        )
        .await?;
    let key = IdempotencyKey::parse("complete-accepted-expiry")?;
    let queued = control
        .queue_platform_image_completion(
            actor,
            session.upload_id,
            &completion_request(&session),
            &key,
            import_now()?,
        )
        .await?;
    sqlx::query("UPDATE control.platform_image_upload_sessions SET expires_at=date_trunc('milliseconds',clock_timestamp())-interval '1 second',created_at=date_trunc('milliseconds',clock_timestamp())-interval '2 seconds' WHERE upload_id=$1")
        .bind(session.upload_id.as_uuid()).execute(&pool).await?;
    assert_eq!(
        control
            .queue_platform_image_completion(
                actor,
                session.upload_id,
                &completion_request(&session),
                &key,
                import_now()?,
            )
            .await?,
        queued
    );
    objects.available.store(true, Ordering::SeqCst);
    worker.tick().await?;
    assert_eq!(
        control
            .platform_image_upload_status(session.upload_id)
            .await?
            .state,
        PlatformImageUploadState::Importing
    );
    assert_eq!(
        transport
            .accepted
            .lock()
            .map_err(|_| "fixture lock poisoned")?
            .as_ref()
            .map(|request| request.upload_id),
        Some(session.upload_id)
    );
    Ok(())
}
