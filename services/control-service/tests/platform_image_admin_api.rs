//! Administrator platform-image gateway: the Agent stays the catalog authority, Control owns the
//! release impact hint, the staging session and the completion fence.
//!
//! Every upstream failure is expected to survive to the browser with the Agent's own diagnostic
//! and status, and every staged archive is expected to leave exactly one cleanup ledger row.

use std::{
    collections::{BTreeMap, BTreeSet},
    io::Cursor,
    str::FromStr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use artifact_store::{ImmutableObjectStore, ObjectStoreError, PresignedUpload, VerifiedObject};
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
use contracts::authoring::{CandidateApproval, CandidateDecision, RuntimeKind};
use contracts::http::{
    CreatePlatformImageUploadRequest, DisablePlatformImageRequest,
    InternalPlatformImageDisableRequest, InternalPlatformImageImportRequest,
    InternalPlatformImageRegistrationRequest, InternalPlatformImageRepinRequest,
    PLATFORM_IMAGE_ARCHIVE_MEDIA_TYPE, PlatformImageCatalog, PlatformImageCatalogView,
    PlatformImageEntry, PlatformImageEntryView, PlatformImageKind, PlatformImageStatus,
    PlatformImageUploadSession, RegisterPlatformImageRequest, RepinPlatformImageRequest,
};
use contracts::supply_chain::{EnvironmentTemplateRelease, ImageArtifact};
use contracts::{
    ActorId, ApprovalId, ArtifactId, ArtifactRef, AuthenticatedActor, AuthorizationDecision,
    AuthorizationDecisionRequest, BffSessionId, BuildRequestId, CandidateId, CourseId,
    DiagnosticCode, ImageArtifactId, PlatformImageId, PlatformRole, PolicyId, ProblemDetails,
    ProjectId, ReleaseId, Revision, UploadSessionId, UtcTimestamp,
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

    let now = "2026-07-16T08:00:00.000Z".parse::<UtcTimestamp>()?;
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
        control: service,
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
        session.upload_target.upload_url,
        "https://objects.example.invalid/platform-image-archive"
    );
    assert_eq!(
        upload_state(&pool, session.upload_id).await?,
        ("pending".to_owned(), None, None)
    );

    // Completion freezes the exact staged version, imports it through the Agent, and records the
    // staging archive for bounded cleanup.
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
            Some(b"{}".to_vec()),
        )?)
        .await?;
    let complete_status = complete_response.status();
    let complete_bytes = to_bytes(complete_response.into_body(), usize::MAX).await?;
    assert_eq!(
        complete_status,
        StatusCode::CREATED,
        "{}",
        String::from_utf8_lossy(&complete_bytes)
    );
    let imported: PlatformImageEntryView = serde_json::from_slice(&complete_bytes)?;
    assert_eq!(imported.entry.catalog_id, imported_entry.catalog_id);
    assert_eq!(imported.release_reference_count, 1);

    let import_body = received_body(&received, "import")?;
    let (frozen_artifact_id, frozen_object_version) = frozen_archive(&pool, session.upload_id)
        .await?
        .ok_or("the staged archive must be frozen before the Agent import")?;
    assert_eq!(frozen_object_version, FROZEN_OBJECT_VERSION);
    assert_eq!(
        import_body
            .get("archive")
            .and_then(|archive| archive.get("objectVersion"))
            .and_then(Value::as_str),
        Some(frozen_object_version.as_str())
    );
    assert_eq!(
        import_body
            .get("archive")
            .and_then(|archive| archive.get("artifactId"))
            .and_then(Value::as_str),
        Some(frozen_artifact_id.to_string().as_str())
    );
    assert_eq!(
        import_body.get("archiveObjectKey").and_then(Value::as_str),
        Some(
            format!(
                "problem-packages/platform-image-uploads/{}",
                session.upload_id
            )
            .as_str()
        )
    );
    assert_eq!(
        upload_state(&pool, session.upload_id).await?,
        ("imported".to_owned(), Some(imported_entry.catalog_id), None)
    );
    assert_eq!(
        cleanup_ledger_key(&pool, session.upload_id).await?,
        Some(format!(
            "problem-packages/platform-image-uploads/{}",
            session.upload_id
        ))
    );

    // An Agent rejection closes the staging session as failed, keeps the upstream diagnostic and
    // still schedules the frozen archive for deletion.
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
            Some(b"{}".to_vec()),
        )?)
        .await?;
    let rejected_status = rejected_complete.status();
    let rejected_bytes = to_bytes(rejected_complete.into_body(), usize::MAX).await?;
    assert_eq!(
        rejected_status,
        StatusCode::CONFLICT,
        "{}",
        String::from_utf8_lossy(&rejected_bytes)
    );
    let rejected_problem: ProblemDetails = serde_json::from_slice(&rejected_bytes)?;
    assert_eq!(
        rejected_problem.diagnostic_code.as_str(),
        "LW_PLATFORM_IMAGE_STATE_CONFLICT"
    );
    assert_eq!(
        upload_state(&pool, rejected_session.upload_id).await?,
        (
            "failed".to_owned(),
            None,
            Some("LW_PLATFORM_IMAGE_STATE_CONFLICT".to_owned())
        )
    );
    assert_eq!(
        cleanup_ledger_key(&pool, rejected_session.upload_id).await?,
        Some(format!(
            "problem-packages/platform-image-uploads/{}",
            rejected_session.upload_id
        ))
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
}

impl PlatformImageObjects {
    fn new(bytes: Vec<u8>) -> Self {
        Self {
            bytes,
            frozen: Mutex::new(BTreeMap::new()),
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
        Ok(VerifiedObject {
            reference,
            bytes: self.bytes.clone(),
        })
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

async fn cleanup_ledger_key(
    pool: &PgPool,
    upload_id: UploadSessionId,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    Ok(sqlx::query_scalar(
        "SELECT object_key FROM control.object_cleanup_ledger WHERE upload_id=$1",
    )
    .bind(upload_id.as_uuid())
    .fetch_optional(pool)
    .await?)
}

async fn frozen_archive(
    pool: &PgPool,
    upload_id: UploadSessionId,
) -> Result<Option<(uuid::Uuid, String)>, Box<dyn std::error::Error>> {
    let row = sqlx::query(
        "SELECT artifact_id,object_version FROM control.platform_image_upload_sessions \
         WHERE upload_id=$1",
    )
    .bind(upload_id.as_uuid())
    .fetch_one(pool)
    .await?;
    let artifact_id: Option<uuid::Uuid> = sqlx::Row::try_get(&row, "artifact_id")?;
    let object_version: Option<String> = sqlx::Row::try_get(&row, "object_version")?;
    Ok(artifact_id.zip(object_version))
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
            .route(
                "/internal/v1/platform-images/imports",
                post(agent_import_image),
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

async fn agent_import_image(
    State(state): State<AgentState>,
    Json(request): Json<InternalPlatformImageImportRequest>,
) -> Response {
    if record_request(&state, "import", json!(request)).is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    if request.binding == CONFLICT_BINDING {
        return problem(
            StatusCode::CONFLICT,
            "LW_PLATFORM_IMAGE_STATE_CONFLICT",
            false,
        );
    }
    (StatusCode::CREATED, Json(state.imported.clone())).into_response()
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
