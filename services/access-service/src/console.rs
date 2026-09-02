//! One-time browser console admission and metadata-only WebSocket proxying.

use std::{collections::HashMap, io::Cursor, str::FromStr, sync::Arc};

use auth::{BffSession, EncryptedValue};
use axum::{
    Json,
    body::Bytes,
    extract::{
        Path, State,
        ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade},
    },
    http::{self, HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use contracts::{
    AccessGrantId, ActorId, ConsoleCapabilityId, ConsoleSessionId, CourseId, EnvironmentId,
    Revision,
    access::{ConsoleCapability, ConsoleCapabilityAvailability, ConsoleKind, ConsoleLeaseFence},
    environment::{EnvironmentAccessSubjectKind, EnvironmentConsoleEligibilityRequest},
    http::{IssueConsoleCapabilityRequest, StrongEtag},
};
use futures_util::{SinkExt, StreamExt};
use persistence_sqlx::Sha256Digest;
use rand::RngCore;
use rustls::{ClientConfig, RootCertStore};
use sha2::{Digest, Sha256};
use sqlx::{Postgres, Row, Transaction};
use subtle::ConstantTimeEq;
use time::{Duration, OffsetDateTime};
use tokio::sync::Mutex;
use tokio_tungstenite::{
    Connector, connect_async_tls_with_config,
    tungstenite::{client::IntoClientRequest, protocol::Message as UpstreamMessage},
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::{
    ApiError, AppState, authenticated_session, grants, require_browser_origin, utc_timestamp,
};

const HANDOFF_COOKIE: &str = "labweaver_console_handoff";
const MAX_FRAME_BYTES: usize = 64 * 1024;

#[derive(Clone, Default)]
pub(super) struct ConsoleRegistry {
    sessions: Arc<Mutex<HashMap<Uuid, RegisteredSession>>>,
}

struct RegisteredSession {
    session_id: Uuid,
    grant_id: Uuid,
    environment_id: Uuid,
    bff_session_id: Uuid,
    cancellation: CancellationToken,
}

impl ConsoleRegistry {
    async fn register(&self, session: &ConsumedCapability, cancellation: CancellationToken) {
        self.sessions.lock().await.insert(
            session.session_id.as_uuid(),
            RegisteredSession {
                session_id: session.session_id.as_uuid(),
                grant_id: session.access_grant_id.as_uuid(),
                environment_id: session.environment_id.as_uuid(),
                bff_session_id: session.bff_session_id,
                cancellation,
            },
        );
    }
    async fn remove(&self, session_id: ConsoleSessionId) {
        self.sessions.lock().await.remove(&session_id.as_uuid());
    }
    pub(super) async fn cancel_grant(&self, grant_id: AccessGrantId) {
        self.cancel_matching(|s| s.grant_id == grant_id.as_uuid())
            .await;
    }
    pub(super) async fn cancel_environment(&self, environment_id: EnvironmentId) {
        self.cancel_matching(|s| s.environment_id == environment_id.as_uuid())
            .await;
    }
    pub(super) async fn cancel_bff(&self, bff_session_id: Uuid) {
        self.cancel_matching(|s| s.bff_session_id == bff_session_id)
            .await;
    }
    pub(super) async fn cancel_session(&self, session_id: ConsoleSessionId) {
        self.cancel_matching(|s| s.session_id == session_id.as_uuid())
            .await;
    }
    pub(super) async fn cancel_all(&self) {
        self.cancel_matching(|_| true).await;
    }
    async fn cancel_matching(&self, predicate: impl Fn(&RegisteredSession) -> bool) {
        for session in self.sessions.lock().await.values().filter(|s| predicate(s)) {
            session.cancellation.cancel();
        }
    }
}

#[derive(Clone)]
pub(super) struct ConsoleGateway {
    base_uri: url::Url,
    connector: Connector,
}

impl ConsoleGateway {
    pub(super) fn new(
        base_uri: &str,
        ca_pem: &[u8],
        certificate_pem: &[u8],
        key_pem: &[u8],
    ) -> Result<Self, ApiError> {
        let base_uri = url::Url::parse(base_uri)
            .map_err(|_| ApiError::internal("LW_ACCESS_CONSOLE_CONFIG_INVALID"))?;
        let mut roots = RootCertStore::empty();
        for certificate in rustls_pemfile::certs(&mut Cursor::new(ca_pem)) {
            roots
                .add(
                    certificate
                        .map_err(|_| ApiError::internal("LW_ACCESS_CONSOLE_CERTIFICATE_INVALID"))?,
                )
                .map_err(|_| ApiError::internal("LW_ACCESS_CONSOLE_CERTIFICATE_INVALID"))?;
        }
        if roots.is_empty() {
            return Err(ApiError::internal("LW_ACCESS_CONSOLE_CERTIFICATE_INVALID"));
        }
        let certificates = rustls_pemfile::certs(&mut Cursor::new(certificate_pem))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| ApiError::internal("LW_ACCESS_CONSOLE_CERTIFICATE_INVALID"))?;
        let key = rustls_pemfile::private_key(&mut Cursor::new(key_pem))
            .map_err(|_| ApiError::internal("LW_ACCESS_CONSOLE_CERTIFICATE_INVALID"))?
            .ok_or_else(|| ApiError::internal("LW_ACCESS_CONSOLE_CERTIFICATE_INVALID"))?;
        let tls = ClientConfig::builder()
            .with_root_certificates(roots)
            .with_client_auth_cert(certificates, key)
            .map_err(|_| ApiError::internal("LW_ACCESS_CONSOLE_CERTIFICATE_INVALID"))?;
        Ok(Self {
            base_uri,
            connector: Connector::Rustls(Arc::new(tls)),
        })
    }

    fn request(&self, session: &ConsumedCapability) -> Result<http::Request<()>, ApiError> {
        let mut url = self.base_uri.clone();
        url.set_scheme("wss")
            .map_err(|()| ApiError::internal("LW_ACCESS_CONSOLE_CONFIG_INVALID"))?;
        url.set_path(&format!(
            "/internal/v1/environments/{}/console:connect",
            session.environment_id
        ));
        let mut request = url
            .as_str()
            .into_client_request()
            .map_err(|_| ApiError::internal("LW_ACCESS_CONSOLE_CONFIG_INVALID"))?;
        let headers = request.headers_mut();
        headers.insert(
            header::SEC_WEBSOCKET_PROTOCOL,
            session
                .kind
                .websocket_subprotocol()
                .parse()
                .map_err(|_| ApiError::internal("LW_ACCESS_CONSOLE_CONFIG_INVALID"))?,
        );
        for (name, value) in [
            (
                "x-labweaver-console-session-id",
                session.session_id.to_string(),
            ),
            (
                "x-labweaver-access-grant-id",
                session.access_grant_id.to_string(),
            ),
            (
                "x-labweaver-access-grant-revision",
                session.access_grant_revision.get().to_string(),
            ),
            (
                "x-labweaver-environment-revision",
                session.environment_revision.get().to_string(),
            ),
        ] {
            headers.insert(
                http::HeaderName::from_bytes(name.as_bytes())
                    .map_err(|_| ApiError::internal("LW_ACCESS_CONSOLE_CONFIG_INVALID"))?,
                value
                    .parse()
                    .map_err(|_| ApiError::internal("LW_ACCESS_CONSOLE_CONFIG_INVALID"))?,
            );
        }
        if let Some(fence) = &session.lease_fence {
            headers.insert(
                "x-labweaver-lease-id",
                HeaderValue::from_str(&fence.lease_id.to_string())
                    .map_err(|_| ApiError::internal("LW_ACCESS_CONSOLE_CONFIG_INVALID"))?,
            );
            headers.insert(
                "x-labweaver-lease-revision",
                HeaderValue::from_str(&fence.lease_revision.get().to_string())
                    .map_err(|_| ApiError::internal("LW_ACCESS_CONSOLE_CONFIG_INVALID"))?,
            );
        }
        Ok(request)
    }
}

pub(super) async fn list_capabilities(
    State(state): State<Arc<AppState>>,
    Path(grant_id): Path<AccessGrantId>,
    headers: HeaderMap,
) -> Result<Json<ConsoleCapabilityAvailability>, ApiError> {
    let session = authenticated_session(&state, &headers).await?;
    Ok(Json(
        resolve_availability(&state, grant_id, &session).await?,
    ))
}

pub(super) async fn issue_capability(
    State(state): State<Arc<AppState>>,
    Path(grant_id): Path<AccessGrantId>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    require_browser_origin(&state, &headers)?;
    let session = authenticated_session(&state, &headers).await?;
    let supplied = headers
        .get(state.deployment.browser.csrf_header_name.as_str())
        .and_then(|value| value.to_str().ok());
    auth::verify_csrf_token(&session.csrf_token, supplied).map_err(ApiError::from)?;
    let request = contracts::parse_strict_json::<IssueConsoleCapabilityRequest>(&body)
        .map_err(|_| ApiError::bad_request("LW_CONTRACT_DOCUMENT_INVALID"))?;
    let etag = StrongEtag::from_revision(grants::if_match(&headers)?);
    request
        .validate_if_match(&etag)
        .map_err(|_| ApiError::precondition("LW_REVISION_CONFLICT"))?;
    let availability = resolve_availability(&state, grant_id, &session).await?;
    request
        .validate_against(&availability)
        .map_err(|_| ApiError::precondition("LW_CONSOLE_AUTHORITY_DRIFT"))?;
    let key = grants::idempotency_key(&headers)?;
    let request_sha = grants::request_hash(&request)?;
    let scope = grants::actor_idempotency_scope(
        ActorId::from_str(&session.actor_id.to_string())
            .map_err(|_| ApiError::internal("LW_ACCESS_ID_INVALID"))?,
    );
    let now = OffsetDateTime::now_utc();
    let expires_at = now + Duration::seconds(30);
    if expires_at > availability.expires_at.get() {
        return Err(ApiError::forbidden("LW_CONSOLE_CAPABILITY_EXPIRED"));
    }
    let mut tx = state
        .pool
        .begin()
        .await
        .map_err(|_| ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))?;
    if let Some(value) = grants::reserve_idempotency(
        &mut tx,
        "issue_console_capability",
        &scope,
        &key,
        &request_sha,
    )
    .await?
    {
        let capability: ConsoleCapability = serde_json::from_value(value)
            .map_err(|_| ApiError::internal("LW_ACCESS_CONSOLE_RECORD_INVALID"))?;
        let cookie = capability_cookie_from_record(&state, &mut tx, &capability).await?;
        tx.commit()
            .await
            .map_err(|_| ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))?;
        return Ok(capability_response(StatusCode::OK, capability, cookie));
    }
    let capability_id = ConsoleCapabilityId::new();
    let locator_token = random_token();
    let locator = format!("/connect/console/{locator_token}");
    let secret = random_secret();
    let encrypted = state
        .key_ring
        .encrypt(&secret, capability_id.as_uuid().as_bytes())
        .map_err(|_| ApiError::internal("LW_ACCESS_CONSOLE_ENCRYPTION_FAILED"))?;
    let capability = ConsoleCapability {
        id: capability_id,
        kind: request.kind,
        access_grant_id: grant_id,
        access_grant_revision: availability.access_grant_revision,
        environment_id: availability.environment_id,
        environment_class: availability.environment_class,
        environment_revision: availability.environment_revision,
        lease_fence: availability.lease_fence.clone(),
        issued_at: utc_timestamp(now)?,
        expires_at: utc_timestamp(expires_at)?,
        connection_locator: locator.clone(),
        websocket_subprotocol: request.kind.websocket_subprotocol().to_owned(),
    };
    capability
        .validate_against(&availability)
        .map_err(|_| ApiError::internal("LW_ACCESS_CONSOLE_RECORD_INVALID"))?;
    insert_capability(
        &mut tx,
        &session,
        &availability,
        &capability,
        &encrypted,
        &secret,
        (&scope, key.as_str()),
    )
    .await?;
    let value = serde_json::to_value(&capability)
        .map_err(|_| ApiError::internal("LW_ACCESS_CONSOLE_RECORD_INVALID"))?;
    grants::complete_idempotency(&mut tx, "issue_console_capability", &scope, &key, &value).await?;
    tx.commit()
        .await
        .map_err(|_| ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))?;
    Ok(capability_response(
        StatusCode::CREATED,
        capability,
        handoff_cookie(&locator, &secret)?,
    ))
}

pub(super) async fn connect(
    State(state): State<Arc<AppState>>,
    Path(opaque): Path<String>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Result<Response, ApiError> {
    require_browser_origin(&state, &headers)?;
    let requested_kind = requested_console_kind(&headers)?;
    let session = authenticated_session(&state, &headers).await?;
    let secret = handoff_secret(&headers)?;
    let consumed = consume_capability(&state, &opaque, &secret, &session, requested_kind).await?;
    Ok(upgrade
        .protocols([requested_kind.websocket_subprotocol()])
        .max_frame_size(MAX_FRAME_BYTES)
        .on_upgrade(move |socket| bridge(state, consumed, socket)))
}

async fn resolve_availability(
    state: &AppState,
    grant_id: AccessGrantId,
    session: &BffSession,
) -> Result<ConsoleCapabilityAvailability, ApiError> {
    let now = OffsetDateTime::now_utc();
    let row = sqlx::query(
        "SELECT actor_id,course_id,environment_id,environment_revision,revision,expires_at \
         FROM access.access_grants WHERE grant_id=$1 AND state='active' AND expires_at>$2",
    )
    .bind(grant_id.as_uuid())
    .bind(now)
    .fetch_optional(&state.pool)
    .await
    .map_err(|_| ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))?
    .ok_or_else(|| ApiError::forbidden("LW_CONSOLE_CAPABILITY_DENIED"))?;
    let actor_id: Uuid = row.get("actor_id");
    if actor_id != session.actor_id {
        return Err(ApiError::forbidden("LW_AUTH_SCOPE_DENIED"));
    }
    let course_id = CourseId::from_str(&row.get::<Uuid, _>("course_id").to_string())
        .map_err(|_| ApiError::internal("LW_ACCESS_ID_INVALID"))?;
    let environment_id = EnvironmentId::from_str(&row.get::<Uuid, _>("environment_id").to_string())
        .map_err(|_| ApiError::internal("LW_ACCESS_ID_INVALID"))?;
    let environment_revision = Revision::try_from(row.get::<i64, _>("environment_revision"))
        .map_err(|_| ApiError::internal("LW_ACCESS_CONSOLE_RECORD_INVALID"))?;
    let access_grant_revision = Revision::try_from(row.get::<i64, _>("revision"))
        .map_err(|_| ApiError::internal("LW_ACCESS_CONSOLE_RECORD_INVALID"))?;
    let membership =
        grants::active_membership(&state.pool, course_id, actor_id, &session.roles).await?;
    let actor = ActorId::from_str(&actor_id.to_string())
        .map_err(|_| ApiError::internal("LW_ACCESS_ID_INVALID"))?;
    let request = EnvironmentConsoleEligibilityRequest {
        environment_id,
        course_id,
        actor_id: actor,
        subject_kind: if matches!(
            membership.role,
            contracts::PlatformRole::Teacher | contracts::PlatformRole::PlatformAdmin
        ) {
            EnvironmentAccessSubjectKind::CourseTeacher
        } else {
            EnvironmentAccessSubjectKind::Owner
        },
        expected_revision: environment_revision,
    };
    let eligibility = state
        .owner_resolver
        .resolve_console_eligibility(&request, utc_timestamp(now)?)
        .await
        .map_err(|_| ApiError::unavailable("LW_CONSOLE_AUTHORITY_UNAVAILABLE"))?;
    let grant_expiry: OffsetDateTime = row.get("expires_at");
    let mut deadline = [
        grant_expiry,
        session.expires_at,
        session.idle_expires_at,
        eligibility.eligibility_expires_at.get(),
    ]
    .into_iter()
    .min()
    .ok_or_else(|| ApiError::internal("LW_ACCESS_CONSOLE_RECORD_INVALID"))?;
    if let Some(expiry) = membership.expires_at {
        deadline = deadline.min(expiry);
    }
    if let Some(fence) = &eligibility.lease_fence {
        deadline = deadline.min(fence.expires_at.get());
    }
    if deadline <= now + Duration::seconds(30) {
        return Err(ApiError::forbidden("LW_CONSOLE_CAPABILITY_EXPIRED"));
    }
    let availability = ConsoleCapabilityAvailability {
        access_grant_id: grant_id,
        access_grant_revision,
        environment_id,
        environment_class: eligibility.environment_class,
        environment_revision,
        expires_at: utc_timestamp(deadline)?,
        lease_fence: eligibility.lease_fence,
        kinds: vec![eligibility.binding.kind()],
    };
    availability
        .validate()
        .map_err(|_| ApiError::internal("LW_ACCESS_CONSOLE_RECORD_INVALID"))?;
    Ok(availability)
}

async fn insert_capability(
    tx: &mut Transaction<'_, Postgres>,
    session: &BffSession,
    availability: &ConsoleCapabilityAvailability,
    capability: &ConsoleCapability,
    encrypted: &EncryptedValue,
    secret: &[u8; 32],
    idempotency: (&str, &str),
) -> Result<(), ApiError> {
    let lease = capability.lease_fence.as_ref();
    let inserted = sqlx::query(
        "INSERT INTO access.console_capabilities \
         (capability_id,kind,access_grant_id,access_grant_revision,bff_session_id,actor_id,course_id,environment_id,environment_class,environment_revision,lease_id,lease_revision,lease_expires_at,issued_at,expires_at,authorization_expires_at,locator_sha256,handoff_secret_sha256,encrypted_handoff_secret,encryption_key_id,idempotency_scope,idempotency_key_sha256) \
         SELECT $1,$2,$3,$4,$5,$6,g.course_id,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21
         FROM access.access_grants g
         JOIN access.bff_sessions s ON s.session_id=$5 AND s.actor_id=$6
         JOIN access.course_memberships cm ON cm.course_id=g.course_id AND cm.actor_id=g.actor_id
         WHERE g.grant_id=$3 AND g.actor_id=$6 AND g.state='active' AND g.revision=$4 AND g.environment_revision=$9
           AND g.expires_at>clock_timestamp() AND s.revoked_at IS NULL AND s.expires_at>clock_timestamp()
           AND s.idle_expires_at>clock_timestamp() AND cm.state='active' AND cm.role=ANY(s.platform_roles)
           AND cm.role=CASE g.contract->>'subjectKind' WHEN 'owner' THEN 'student' WHEN 'course_teacher' THEN 'teacher' ELSE '' END
           AND (cm.expires_at IS NULL OR cm.expires_at>clock_timestamp())",
    )
    .bind(capability.id.as_uuid()).bind(console_kind_db(capability.kind)).bind(capability.access_grant_id.as_uuid())
    .bind(capability.access_grant_revision.to_i64().ok_or(ApiError::internal("LW_ACCESS_CONSOLE_RECORD_INVALID"))?).bind(session.session_id).bind(session.actor_id)
    .bind(capability.environment_id.as_uuid()).bind(match capability.environment_class { contracts::authoring::EnvironmentClass::Experiment => "experiment", contracts::authoring::EnvironmentClass::Work => "work" })
    .bind(capability.environment_revision.to_i64().ok_or(ApiError::internal("LW_ACCESS_CONSOLE_RECORD_INVALID"))?).bind(lease.map(|f| f.lease_id.as_uuid()))
    .bind(lease.and_then(|f| f.lease_revision.to_i64()).ok_or(ApiError::internal("LW_ACCESS_CONSOLE_RECORD_INVALID"))?).bind(lease.map(|f| f.expires_at.get()))
    .bind(capability.issued_at.get()).bind(capability.expires_at.get()).bind(availability.expires_at.get())
    .bind(Sha256Digest::of_bytes(capability.connection_locator.as_bytes()).to_string())
    .bind(hex_sha(secret)).bind(&encrypted.payload).bind(&encrypted.key_id)
    .bind(idempotency.0).bind(Sha256Digest::of_bytes(idempotency.1.as_bytes()).to_string())
    .execute(&mut **tx).await.map_err(|_| ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))?;
    if inserted.rows_affected() != 1 {
        return Err(ApiError::precondition("LW_CONSOLE_AUTHORITY_DRIFT"));
    }
    Ok(())
}

async fn capability_cookie_from_record(
    state: &AppState,
    tx: &mut Transaction<'_, Postgres>,
    capability: &ConsoleCapability,
) -> Result<HeaderValue, ApiError> {
    let row = sqlx::query("SELECT encrypted_handoff_secret,encryption_key_id,consumed_at FROM access.console_capabilities WHERE capability_id=$1 FOR UPDATE")
        .bind(capability.id.as_uuid()).fetch_one(&mut **tx).await.map_err(|_| ApiError::conflict("LW_IDEMPOTENCY_CONFLICT"))?;
    if row
        .get::<Option<OffsetDateTime>, _>("consumed_at")
        .is_some()
    {
        return Err(ApiError::conflict("LW_CONSOLE_CAPABILITY_CONSUMED"));
    }
    if capability.expires_at.get() <= OffsetDateTime::now_utc() {
        return Err(ApiError::conflict("LW_CONSOLE_CAPABILITY_EXPIRED"));
    }
    let encrypted = EncryptedValue {
        payload: row.get("encrypted_handoff_secret"),
        key_id: row.get("encryption_key_id"),
    };
    let secret = state
        .key_ring
        .decrypt(&encrypted, capability.id.as_uuid().as_bytes())
        .map_err(|_| ApiError::internal("LW_ACCESS_CONSOLE_ENCRYPTION_FAILED"))?;
    handoff_cookie(&capability.connection_locator, &secret)
}

fn capability_response(
    status: StatusCode,
    capability: ConsoleCapability,
    cookie: HeaderValue,
) -> Response {
    let mut response = (status, Json(capability)).into_response();
    response.headers_mut().insert(header::SET_COOKIE, cookie);
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

fn handoff_cookie(locator: &str, secret: &[u8]) -> Result<HeaderValue, ApiError> {
    HeaderValue::from_str(&format!(
        "{HANDOFF_COOKIE}={}; Path={locator}; Max-Age=30; Secure; HttpOnly; SameSite=Strict",
        URL_SAFE_NO_PAD.encode(secret)
    ))
    .map_err(|_| ApiError::internal("LW_ACCESS_CONSOLE_RECORD_INVALID"))
}

fn handoff_secret(headers: &HeaderMap) -> Result<Vec<u8>, ApiError> {
    let cookies = headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| ApiError::unauthorized("LW_CONSOLE_HANDOFF_REQUIRED"))?;
    let encoded = cookies
        .split(';')
        .map(str::trim)
        .find_map(|item| item.strip_prefix(&format!("{HANDOFF_COOKIE}=")))
        .ok_or_else(|| ApiError::unauthorized("LW_CONSOLE_HANDOFF_REQUIRED"))?;
    URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| ApiError::unauthorized("LW_CONSOLE_HANDOFF_INVALID"))
}

struct ConsumedCapability {
    session_id: ConsoleSessionId,
    kind: ConsoleKind,
    access_grant_id: AccessGrantId,
    access_grant_revision: Revision,
    environment_id: EnvironmentId,
    environment_revision: Revision,
    lease_fence: Option<ConsoleLeaseFence>,
    authorization_expires_at: OffsetDateTime,
    bff_session_id: Uuid,
}

#[allow(clippy::too_many_lines)] // This is one auditable consume-and-open transaction boundary.
async fn consume_capability(
    state: &AppState,
    opaque: &str,
    secret: &[u8],
    session: &BffSession,
    requested_kind: ConsoleKind,
) -> Result<ConsumedCapability, ApiError> {
    if opaque.is_empty()
        || opaque.len() > 128
        || !opaque
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
    {
        return Err(ApiError::bad_request("LW_CONSOLE_LOCATOR_INVALID"));
    }
    let locator = format!("/connect/console/{opaque}");
    let mut tx = state
        .pool
        .begin()
        .await
        .map_err(|_| ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('labweaver:console-session-capacity'))")
        .execute(&mut *tx)
        .await
        .map_err(|_| ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))?;
    let active: i64 = sqlx::query_scalar("SELECT count(*) FROM access.console_sessions WHERE state IN ('opening','active','terminating','termination_overdue')")
        .fetch_one(&mut *tx).await.map_err(|_| ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))?;
    if active >= i64::from(state.deployment.grants.max_console_sessions) {
        return Err(ApiError::unavailable("LW_CONSOLE_CAPACITY_EXHAUSTED"));
    }
    let row = sqlx::query(
        "SELECT c.capability_id,c.kind,c.access_grant_id,c.access_grant_revision,c.actor_id,c.course_id,c.environment_id,c.environment_revision,c.lease_id,c.lease_revision,c.lease_expires_at,c.handoff_secret_sha256,c.expires_at,c.authorization_expires_at,c.consumed_at,g.state,g.revision AS current_grant_revision,g.environment_revision AS current_environment_revision,g.expires_at AS grant_expires_at,s.revoked_at,s.expires_at AS bff_expires_at,s.idle_expires_at,EXISTS (SELECT 1 FROM access.course_memberships cm WHERE cm.course_id=c.course_id AND cm.actor_id=c.actor_id AND cm.state='active' AND cm.role=ANY(s.platform_roles) AND cm.role=CASE g.contract->>'subjectKind' WHEN 'owner' THEN 'student' WHEN 'course_teacher' THEN 'teacher' ELSE '' END AND (cm.expires_at IS NULL OR cm.expires_at > clock_timestamp())) AS membership_active \
         FROM access.console_capabilities c JOIN access.access_grants g ON g.grant_id=c.access_grant_id JOIN access.bff_sessions s ON s.session_id=c.bff_session_id \
         WHERE c.locator_sha256=$1 FOR UPDATE OF c",
    ).bind(Sha256Digest::of_bytes(locator.as_bytes()).to_string()).fetch_optional(&mut *tx).await
      .map_err(|_| ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))?.ok_or_else(|| ApiError::forbidden("LW_CONSOLE_CAPABILITY_DENIED"))?;
    let now = OffsetDateTime::now_utc();
    if row
        .get::<Option<OffsetDateTime>, _>("consumed_at")
        .is_some()
    {
        return Err(ApiError::conflict("LW_CONSOLE_CAPABILITY_CONSUMED"));
    }
    if row.get::<OffsetDateTime, _>("expires_at") <= now {
        return Err(ApiError::forbidden("LW_CONSOLE_CAPABILITY_EXPIRED"));
    }
    let kind = parse_console_kind(&row.get::<String, _>("kind"))?;
    if kind != requested_kind {
        return Err(ApiError::bad_request("LW_CONSOLE_SUBPROTOCOL_MISMATCH"));
    }
    if row.get::<Uuid, _>("actor_id") != session.actor_id
        || row.get::<Option<OffsetDateTime>, _>("revoked_at").is_some()
        || row.get::<OffsetDateTime, _>("grant_expires_at") <= now
        || row.get::<OffsetDateTime, _>("bff_expires_at") <= now
        || row.get::<OffsetDateTime, _>("idle_expires_at") <= now
        || !row.get::<bool, _>("membership_active")
        || row.get::<String, _>("state") != "active"
        || row.get::<i64, _>("access_grant_revision") != row.get::<i64, _>("current_grant_revision")
        || row.get::<i64, _>("environment_revision")
            != row.get::<i64, _>("current_environment_revision")
        || row
            .get::<String, _>("handoff_secret_sha256")
            .as_bytes()
            .ct_eq(hex_sha(secret).as_bytes())
            .unwrap_u8()
            != 1
    {
        return Err(ApiError::precondition("LW_CONSOLE_REVISION_CONFLICT"));
    }
    let capability_id: Uuid = row.get("capability_id");
    let session_id = ConsoleSessionId::new();
    let authorization_expires_at: OffsetDateTime = row.get("authorization_expires_at");
    let lease_fence = match (
        row.get::<Option<Uuid>, _>("lease_id"),
        row.get::<Option<i64>, _>("lease_revision"),
        row.get::<Option<OffsetDateTime>, _>("lease_expires_at"),
    ) {
        (Some(id), Some(rev), Some(expires)) => Some(ConsoleLeaseFence {
            lease_id: FromStr::from_str(&id.to_string())
                .map_err(|_| ApiError::internal("LW_ACCESS_ID_INVALID"))?,
            lease_revision: Revision::try_from(rev)
                .map_err(|_| ApiError::internal("LW_ACCESS_CONSOLE_RECORD_INVALID"))?,
            expires_at: utc_timestamp(expires)?,
        }),
        (None, None, None) => None,
        _ => return Err(ApiError::internal("LW_ACCESS_CONSOLE_RECORD_INVALID")),
    };
    sqlx::query("INSERT INTO access.console_sessions (session_id,capability_id,kind,bff_session_id,access_grant_id,access_grant_revision,actor_id,course_id,environment_id,environment_revision,lease_id,lease_revision,lease_expires_at,proxy_owner,revision,state,opened_at,authorization_expires_at) SELECT $1,c.capability_id,c.kind,c.bff_session_id,c.access_grant_id,c.access_grant_revision,c.actor_id,c.course_id,c.environment_id,c.environment_revision,c.lease_id,c.lease_revision,c.lease_expires_at,$2,1,'opening',$3,c.authorization_expires_at FROM access.console_capabilities c WHERE c.capability_id=$4")
        .bind(session_id.as_uuid()).bind(state.console_proxy_owner.as_str()).bind(now).bind(capability_id).execute(&mut *tx).await.map_err(|_| ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))?;
    let course_id = CourseId::from_str(&row.get::<Uuid, _>("course_id").to_string())
        .map_err(|_| ApiError::internal("LW_ACCESS_ID_INVALID"))?;
    let grant_id = AccessGrantId::from_str(&row.get::<Uuid, _>("access_grant_id").to_string())
        .map_err(|_| ApiError::internal("LW_ACCESS_ID_INVALID"))?;
    let environment_id = EnvironmentId::from_str(&row.get::<Uuid, _>("environment_id").to_string())
        .map_err(|_| ApiError::internal("LW_ACCESS_ID_INVALID"))?;
    let grant_revision = Revision::try_from(row.get::<i64, _>("access_grant_revision"))
        .map_err(|_| ApiError::internal("LW_ACCESS_CONSOLE_RECORD_INVALID"))?;
    let environment_revision = Revision::try_from(row.get::<i64, _>("environment_revision"))
        .map_err(|_| ApiError::internal("LW_ACCESS_CONSOLE_RECORD_INVALID"))?;
    grants::enqueue_event_value(
        &mut tx,
        contracts::events::subjects::ACCESS_CONSOLE_SESSION_STATE_CHANGED,
        course_id,
        session_id.as_uuid(),
        Revision::new(1).map_err(|_| ApiError::internal("LW_ACCESS_CONSOLE_RECORD_INVALID"))?,
        serde_json::to_value(contracts::events::ConsoleSessionChanged {
            console_session_id: session_id,
            access_grant_id: grant_id,
            access_grant_revision: grant_revision,
            environment_id,
            environment_revision,
            state: contracts::access::ConsoleSessionState::Opening,
            effective_at: utc_timestamp(now)?,
            terminate_by: None,
            diagnostic_code: None,
        })
        .map_err(|_| ApiError::internal("LW_ACCESS_EVENT_INVALID"))?,
    )
    .await?;
    let updated = sqlx::query("UPDATE access.console_capabilities SET consumed_at=$2,session_id=$3,secret_scrubbed_at=$2,encrypted_handoff_secret='\\x'::bytea WHERE capability_id=$1 AND consumed_at IS NULL")
        .bind(capability_id).bind(now).bind(session_id.as_uuid()).execute(&mut *tx).await.map_err(|_| ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))?.rows_affected();
    if updated != 1 {
        return Err(ApiError::conflict("LW_CONSOLE_CAPABILITY_CONSUMED"));
    }
    tx.commit()
        .await
        .map_err(|_| ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))?;
    Ok(ConsumedCapability {
        session_id,
        kind,
        access_grant_id: grant_id,
        access_grant_revision: grant_revision,
        environment_id,
        environment_revision,
        lease_fence,
        authorization_expires_at,
        bff_session_id: session.session_id,
    })
}

async fn bridge(state: Arc<AppState>, session: ConsumedCapability, browser: WebSocket) {
    let cancellation = CancellationToken::new();
    state
        .console_registry
        .register(&session, cancellation.clone())
        .await;
    let diagnostic = match bridge_inner(&state, &session, browser, cancellation).await {
        Ok(()) => "LW_CONSOLE_CLOSED",
        Err(code) => code,
    };
    state.console_registry.remove(session.session_id).await;
    let now = OffsetDateTime::now_utc();
    if close_session(&state, session.session_id, diagnostic, now)
        .await
        .is_err()
    {
        tracing::error!(event="access.console.session_close_persist_failed", console_session_id=%session.session_id, diagnostic="LW_ACCESS_STORE_UNAVAILABLE");
    }
}

pub(super) async fn terminate_bff_sessions(
    state: &AppState,
    bff_session_id: Uuid,
    diagnostic: &'static str,
) -> Result<(), ApiError> {
    let now = OffsetDateTime::now_utc();
    let terminate_by = now + Duration::seconds(60);
    let mut tx = state
        .pool
        .begin()
        .await
        .map_err(|_| ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))?;
    let rows = sqlx::query("UPDATE access.console_sessions SET state='terminating',termination_requested_at=$2,terminate_by=$3,revision=revision+1,diagnostic_code=$4 WHERE bff_session_id=$1 AND state IN ('opening','active') RETURNING session_id")
        .bind(bff_session_id).bind(now).bind(terminate_by).bind(diagnostic)
        .fetch_all(&mut *tx).await.map_err(|_| ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))?;
    enqueue_terminations(&mut tx, rows, diagnostic, now).await?;
    tx.commit()
        .await
        .map_err(|_| ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))?;
    state.console_registry.cancel_bff(bff_session_id).await;
    Ok(())
}

pub(super) async fn reconcile_ineligible_bff_sessions(state: &AppState) -> Result<(), ApiError> {
    let now = OffsetDateTime::now_utc();
    let terminate_by = now + Duration::seconds(60);
    let mut tx = state
        .pool
        .begin()
        .await
        .map_err(|_| ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))?;
    let rows = sqlx::query("UPDATE access.console_sessions c SET state='terminating',termination_requested_at=$1,terminate_by=$2,revision=c.revision+1,diagnostic_code='LW_AUTH_SESSION_REVOKED' FROM access.bff_sessions b WHERE c.bff_session_id=b.session_id AND c.state IN ('opening','active') AND (b.revoked_at IS NOT NULL OR b.expires_at<=$1 OR b.idle_expires_at<=$1) RETURNING c.session_id")
        .bind(now).bind(terminate_by).fetch_all(&mut *tx).await
        .map_err(|_| ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))?;
    enqueue_terminations(&mut tx, rows, "LW_AUTH_SESSION_REVOKED", now).await?;
    tx.commit()
        .await
        .map_err(|_| ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))
}

async fn enqueue_terminations(
    tx: &mut Transaction<'_, Postgres>,
    rows: Vec<sqlx::postgres::PgRow>,
    diagnostic: &'static str,
    now: OffsetDateTime,
) -> Result<(), ApiError> {
    for row in rows {
        let session_id = ConsoleSessionId::from_str(&row.get::<Uuid, _>("session_id").to_string())
            .map_err(|_| ApiError::internal("LW_ACCESS_ID_INVALID"))?;
        enqueue_console_event(
            tx,
            session_id,
            contracts::access::ConsoleSessionState::Terminating,
            Some(diagnostic),
            now,
        )
        .await?;
    }
    Ok(())
}

async fn close_session(
    state: &AppState,
    session_id: ConsoleSessionId,
    diagnostic: &str,
    now: OffsetDateTime,
) -> Result<(), ApiError> {
    let mut tx = state
        .pool
        .begin()
        .await
        .map_err(|_| ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))?;
    let rows = sqlx::query("UPDATE access.console_sessions SET state='closed',revision=revision+1,closed_at=$2,diagnostic_code=$3 WHERE session_id=$1 AND state IN ('opening','active','terminating','termination_overdue')")
        .bind(session_id.as_uuid()).bind(now).bind(diagnostic).execute(&mut *tx).await.map_err(|_| ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))?.rows_affected();
    if rows == 1 {
        enqueue_console_event(
            &mut tx,
            session_id,
            contracts::access::ConsoleSessionState::Closed,
            Some(diagnostic),
            now,
        )
        .await?;
    }
    tx.commit()
        .await
        .map_err(|_| ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))
}

pub(super) async fn enqueue_console_event(
    tx: &mut Transaction<'_, Postgres>,
    session_id: ConsoleSessionId,
    state: contracts::access::ConsoleSessionState,
    diagnostic: Option<&str>,
    now: OffsetDateTime,
) -> Result<(), ApiError> {
    let row = sqlx::query("SELECT course_id,access_grant_id,access_grant_revision,environment_id,environment_revision,revision,terminate_by FROM access.console_sessions WHERE session_id=$1")
        .bind(session_id.as_uuid()).fetch_one(&mut **tx).await.map_err(|_| ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))?;
    let course_id = CourseId::from_str(&row.get::<Uuid, _>("course_id").to_string())
        .map_err(|_| ApiError::internal("LW_ACCESS_ID_INVALID"))?;
    let revision = Revision::try_from(row.get::<i64, _>("revision"))
        .map_err(|_| ApiError::internal("LW_ACCESS_CONSOLE_RECORD_INVALID"))?;
    grants::enqueue_event_value(
        tx,
        contracts::events::subjects::ACCESS_CONSOLE_SESSION_STATE_CHANGED,
        course_id,
        session_id.as_uuid(),
        revision,
        serde_json::to_value(contracts::events::ConsoleSessionChanged {
            console_session_id: session_id,
            access_grant_id: AccessGrantId::from_str(
                &row.get::<Uuid, _>("access_grant_id").to_string(),
            )
            .map_err(|_| ApiError::internal("LW_ACCESS_ID_INVALID"))?,
            access_grant_revision: revision_from_row(&row, "access_grant_revision")?,
            environment_id: EnvironmentId::from_str(
                &row.get::<Uuid, _>("environment_id").to_string(),
            )
            .map_err(|_| ApiError::internal("LW_ACCESS_ID_INVALID"))?,
            environment_revision: revision_from_row(&row, "environment_revision")?,
            state,
            effective_at: utc_timestamp(now)?,
            terminate_by: row
                .get::<Option<OffsetDateTime>, _>("terminate_by")
                .map(utc_timestamp)
                .transpose()?,
            diagnostic_code: diagnostic.map(str::to_owned),
        })
        .map_err(|_| ApiError::internal("LW_ACCESS_EVENT_INVALID"))?,
    )
    .await
}

async fn bridge_inner(
    state: &AppState,
    session: &ConsumedCapability,
    browser: WebSocket,
    cancellation: CancellationToken,
) -> Result<(), &'static str> {
    let request = state
        .console_gateway
        .request(session)
        .map_err(|_| "LW_CONSOLE_UPSTREAM_INVALID")?;
    let (upstream, response) = connect_async_tls_with_config(
        request,
        None,
        true,
        Some(state.console_gateway.connector.clone()),
    )
    .await
    .map_err(|_| "LW_CONSOLE_UPSTREAM_UNAVAILABLE")?;
    if response
        .headers()
        .get(header::SEC_WEBSOCKET_PROTOCOL)
        .and_then(|v| v.to_str().ok())
        != Some(session.kind.websocket_subprotocol())
    {
        return Err("LW_CONSOLE_SUBPROTOCOL_MISMATCH");
    }
    let now = OffsetDateTime::now_utc();
    let mut tx = state
        .pool
        .begin()
        .await
        .map_err(|_| "LW_ACCESS_STORE_UNAVAILABLE")?;
    let activated = sqlx::query("UPDATE access.console_sessions SET state='active',revision=revision+1,activated_at=$2 WHERE session_id=$1 AND state='opening'").bind(session.session_id.as_uuid()).bind(now).execute(&mut *tx).await.map_err(|_| "LW_ACCESS_STORE_UNAVAILABLE")?.rows_affected();
    if activated != 1 {
        return Err("LW_CONSOLE_SESSION_STATE_INVALID");
    }
    enqueue_console_event(
        &mut tx,
        session.session_id,
        contracts::access::ConsoleSessionState::Active,
        None,
        now,
    )
    .await
    .map_err(|_| "LW_ACCESS_EVENT_INVALID")?;
    tx.commit()
        .await
        .map_err(|_| "LW_ACCESS_STORE_UNAVAILABLE")?;
    let (mut browser_tx, mut browser_rx) = browser.split();
    let (mut upstream_tx, mut upstream_rx) = upstream.split();
    let deadline = tokio::time::sleep(
        std::time::Duration::try_from(session.authorization_expires_at - OffsetDateTime::now_utc())
            .unwrap_or_default(),
    );
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            () = cancellation.cancelled() => { let _ = browser_tx.send(Message::Close(Some(CloseFrame { code: 1008, reason: "authorization revoked".into() }))).await; return Err("LW_CONSOLE_AUTHORIZATION_ENDED"); }
            () = &mut deadline => { let _ = browser_tx.send(Message::Close(Some(CloseFrame { code: 1008, reason: "authorization expired".into() }))).await; return Err("LW_CONSOLE_AUTHORIZATION_ENDED"); }
            message = browser_rx.next() => {
                let Some(message) = message else { return Ok(()); };
                let message = message.map_err(|_| "LW_CONSOLE_BROWSER_IO_FAILED")?;
                let forwarded = match message {
                    Message::Binary(value) if value.len() <= MAX_FRAME_BYTES => UpstreamMessage::Binary(value),
                    Message::Text(value) if session.kind == ConsoleKind::Xterm && value.len() <= 1024 => { let control: contracts::access::ConsoleClientControl = contracts::parse_strict_json(value.as_bytes()).map_err(|_| "LW_CONSOLE_CONTROL_INVALID")?; control.validate().map_err(|_| "LW_CONSOLE_CONTROL_INVALID")?; UpstreamMessage::Text(value.to_string().into()) },
                    Message::Ping(value) => UpstreamMessage::Ping(value), Message::Pong(value) => UpstreamMessage::Pong(value), Message::Close(_) => return Ok(()), _ => return Err("LW_CONSOLE_FRAME_INVALID"),
                }; upstream_tx.send(forwarded).await.map_err(|_| "LW_CONSOLE_UPSTREAM_IO_FAILED")?;
            }
            message = upstream_rx.next() => {
                let Some(message) = message else { return Ok(()); };
                let message = message.map_err(|_| "LW_CONSOLE_UPSTREAM_IO_FAILED")?;
                let forwarded = match message { UpstreamMessage::Binary(value) if value.len() <= MAX_FRAME_BYTES => Message::Binary(value), UpstreamMessage::Ping(value) => Message::Ping(value), UpstreamMessage::Pong(value) => Message::Pong(value), UpstreamMessage::Close(_) => return Ok(()), UpstreamMessage::Text(_) | UpstreamMessage::Frame(_) | UpstreamMessage::Binary(_) => return Err("LW_CONSOLE_UPSTREAM_FRAME_INVALID") };
                browser_tx.send(forwarded).await.map_err(|_| "LW_CONSOLE_BROWSER_IO_FAILED")?;
            }
        }
    }
}

fn random_secret() -> [u8; 32] {
    let mut value = [0_u8; 32];
    rand::rng().fill_bytes(&mut value);
    value
}
fn random_token() -> String {
    URL_SAFE_NO_PAD.encode(random_secret())
}
fn hex_sha(value: &[u8]) -> String {
    format!("{:x}", Sha256::digest(value))
}
fn revision_from_row(row: &sqlx::postgres::PgRow, name: &str) -> Result<Revision, ApiError> {
    Revision::try_from(row.get::<i64, _>(name))
        .map_err(|_| ApiError::internal("LW_ACCESS_CONSOLE_RECORD_INVALID"))
}
const fn console_kind_db(kind: ConsoleKind) -> &'static str {
    match kind {
        ConsoleKind::Xterm => "xterm",
        ConsoleKind::Novnc => "novnc",
    }
}

fn parse_console_kind(value: &str) -> Result<ConsoleKind, ApiError> {
    match value {
        "xterm" => Ok(ConsoleKind::Xterm),
        "novnc" => Ok(ConsoleKind::Novnc),
        _ => Err(ApiError::internal("LW_ACCESS_CONSOLE_RECORD_INVALID")),
    }
}

fn requested_console_kind(headers: &HeaderMap) -> Result<ConsoleKind, ApiError> {
    let protocols = headers
        .get(header::SEC_WEBSOCKET_PROTOCOL)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| ApiError::bad_request("LW_CONSOLE_SUBPROTOCOL_MISMATCH"))?;
    let mut selected = None;
    for protocol in protocols.split(',').map(str::trim) {
        let kind = match protocol {
            value if value == ConsoleKind::Xterm.websocket_subprotocol() => ConsoleKind::Xterm,
            value if value == ConsoleKind::Novnc.websocket_subprotocol() => ConsoleKind::Novnc,
            _ => continue,
        };
        if selected
            .replace(kind)
            .is_some_and(|previous| previous != kind)
        {
            return Err(ApiError::bad_request("LW_CONSOLE_SUBPROTOCOL_MISMATCH"));
        }
    }
    selected.ok_or_else(|| ApiError::bad_request("LW_CONSOLE_SUBPROTOCOL_MISMATCH"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handoff_cookie_is_path_scoped_strict_and_never_domain_scoped()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(cookie) = handoff_cookie("/connect/console/opaque", &[7_u8; 32]) else {
            return Err("valid handoff cookie was rejected".into());
        };
        let value = cookie.to_str()?;
        assert!(value.starts_with("labweaver_console_handoff="));
        assert!(value.contains("; Path=/connect/console/opaque; Max-Age=30"));
        assert!(value.ends_with("; Secure; HttpOnly; SameSite=Strict"));
        assert!(!value.contains("Domain="));
        Ok(())
    }

    #[test]
    fn console_protocol_selection_rejects_missing_unknown_and_ambiguous_offers() {
        let mut headers = HeaderMap::new();
        assert!(requested_console_kind(&headers).is_err());
        headers.insert(
            header::SEC_WEBSOCKET_PROTOCOL,
            HeaderValue::from_static("unknown.v1"),
        );
        assert!(requested_console_kind(&headers).is_err());
        headers.insert(
            header::SEC_WEBSOCKET_PROTOCOL,
            HeaderValue::from_static("labweaver.console.novnc.v1"),
        );
        assert_eq!(
            requested_console_kind(&headers).ok(),
            Some(ConsoleKind::Novnc)
        );
        headers.insert(
            header::SEC_WEBSOCKET_PROTOCOL,
            HeaderValue::from_static("labweaver.console.xterm.v1, labweaver.console.novnc.v1"),
        );
        assert!(requested_console_kind(&headers).is_err());
    }
}
