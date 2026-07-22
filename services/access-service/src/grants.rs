use std::{collections::BTreeSet, path::PathBuf, str::FromStr, sync::Arc, time::Duration};

use async_nats::jetstream::message::PublishMessage;
use auth::NatsFileConfig;
use axum::{
    Json,
    body::Bytes,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode, header},
};
use contracts::{
    AccessGrantId, ActorId, CourseId, EndpointGrantId, EndpointId, EnvironmentId, EventId,
    GatewaySessionId, PlatformRole, Revision, Sequence, Sha256Digest, SshPublicKeyId,
    StreamSequence, UtcTimestamp,
    access::{
        AccessGrant, AccessGrantSnapshot, AccessGrantState, AuthorizationDecision,
        AuthorizationDecisionSummary, CloseGatewaySessionRequest, CreateGatewaySessionRequest,
        EndpointAction, EndpointGrant, EndpointGrantSnapshot, EndpointGrantSnapshotState,
        GatewaySession, GatewaySessionState, HeartbeatGatewaySessionRequest, SshAuthorization,
        SshAuthorizationRequest, SshKeyAlgorithm, SshPublicKey, validate_ssh_public_key,
    },
    environment::{
        EndpointHealth, EndpointProtocol, EnvironmentAccessSubjectKind,
        EnvironmentEndpointEligibilityRequest,
    },
    events::{
        AccessGrantChanged, CloudEvent, EventContract, GatewaySessionChanged, SshPublicKeyRevoked,
        subjects,
    },
    http::{
        CreateAccessGrantRequest, CreateSshPublicKeyRequest, EnvironmentAccessGrantListQuery,
        IdempotencyKey, RenewAccessGrantRequest, RevokeAccessGrantRequest, StrongEtag,
    },
};
use futures_util::StreamExt;
use rand::RngCore;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Postgres, Row, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

use super::{
    ApiError, AppState, MtlsPrincipal, authenticated_identity, authenticated_session,
    cookie_session_id, require_browser_origin, utc_timestamp,
};

const TERMINATION_SECONDS: i64 = 60;
// This request/reply subject deliberately sits outside the persisted
// `labweaver.access.>` event stream. Otherwise JetStream's PubAck races the
// authoritative Access response and the caller can observe the wrong payload.
const ACCESS_REVOCATION_SUBJECT: &str = "labweaver.service.access.revoke.v1";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EnvironmentAccessRevocationRequest {
    version: u8,
    environment_id: EnvironmentId,
    environment_revision: Revision,
    reason: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EnvironmentAccessRevocationResponse {
    version: u8,
    environment_id: EnvironmentId,
    environment_revision: Revision,
    access_revocation_revision: Revision,
}

fn parse_body<T: DeserializeOwned>(body: &Bytes) -> Result<T, ApiError> {
    contracts::parse_strict_json(body)
        .map_err(|_| ApiError::bad_request("LW_CONTRACT_DOCUMENT_INVALID"))
}

pub async fn connect_nats(
    config: &NatsFileConfig,
) -> Result<async_nats::Client, GrantRuntimeError> {
    let options = async_nats::ConnectOptions::new()
        .require_tls(true)
        .add_root_certificates(PathBuf::from(&config.ca_certificate_file))
        .add_client_certificate(
            PathBuf::from(&config.client_certificate_file),
            PathBuf::from(&config.client_private_key_file),
        )
        .credentials_file(PathBuf::from(&config.credentials_file))
        .await
        .map_err(|_| GrantRuntimeError::NatsCredentials)?;
    options
        .connect(&config.server)
        .await
        .map_err(|_| GrantRuntimeError::NatsConnect)
}

fn actor_uuid(actor_id: ActorId) -> Result<Uuid, ApiError> {
    Uuid::parse_str(&actor_id.to_string()).map_err(|_| ApiError::internal("LW_ACCESS_ID_INVALID"))
}

async fn require_mutation_auth(state: &AppState, headers: &HeaderMap) -> Result<(), ApiError> {
    if cookie_session_id(state, headers).is_some() {
        require_browser_origin(state, headers)?;
        let session = authenticated_session(state, headers).await?;
        let supplied = headers
            .get(state.deployment.browser.csrf_header_name.as_str())
            .and_then(|value| value.to_str().ok());
        auth::verify_csrf_token(&session.csrf_token, supplied).map_err(ApiError::from)?;
    }
    Ok(())
}

fn idempotency_key(headers: &HeaderMap) -> Result<IdempotencyKey, ApiError> {
    let value = headers
        .get("Idempotency-Key")
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| ApiError::bad_request("LW_IDEMPOTENCY_REQUIRED"))?;
    IdempotencyKey::parse(value).map_err(|_| ApiError::bad_request("LW_IDEMPOTENCY_INVALID"))
}

fn if_match(headers: &HeaderMap) -> Result<Revision, ApiError> {
    let value = headers
        .get(header::IF_MATCH)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| ApiError::precondition("LW_REVISION_REQUIRED"))?;
    StrongEtag::parse(value)
        .map(|etag| etag.revision())
        .map_err(|_| ApiError::precondition("LW_REVISION_CONFLICT"))
}

fn request_hash<T: Serialize>(request: &T) -> Result<String, ApiError> {
    let bytes = serde_jcs::to_vec(request)
        .map_err(|_| ApiError::bad_request("LW_CONTRACT_DOCUMENT_INVALID"))?;
    Ok(Sha256Digest::of_bytes(&bytes).to_string())
}

async fn reserve_idempotency(
    tx: &mut Transaction<'_, Postgres>,
    operation: &str,
    scope_id: &str,
    key: &IdempotencyKey,
    hash: &str,
) -> Result<Option<Value>, ApiError> {
    let inserted = sqlx::query(
        "INSERT INTO access.idempotency_ledger (operation,scope_id,idempotency_key,request_sha256,state) \
         VALUES ($1,$2,$3,$4,'in_progress') ON CONFLICT DO NOTHING",
    )
    .bind(operation)
    .bind(scope_id)
    .bind(key.as_str())
    .bind(hash)
    .execute(&mut **tx)
    .await
    .map_err(|_| ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))?
    .rows_affected();
    if inserted == 1 {
        return Ok(None);
    }
    let row = sqlx::query(
        "SELECT request_sha256,state,result FROM access.idempotency_ledger \
         WHERE operation=$1 AND scope_id=$2 AND idempotency_key=$3 FOR UPDATE",
    )
    .bind(operation)
    .bind(scope_id)
    .bind(key.as_str())
    .fetch_one(&mut **tx)
    .await
    .map_err(|_| ApiError::conflict("LW_IDEMPOTENCY_CONFLICT"))?;
    let existing_hash: String = row.get("request_sha256");
    let state: String = row.get("state");
    if existing_hash != hash || state != "completed" {
        return Err(ApiError::conflict("LW_IDEMPOTENCY_CONFLICT"));
    }
    Ok(Some(row.get("result")))
}

async fn complete_idempotency(
    tx: &mut Transaction<'_, Postgres>,
    operation: &str,
    scope_id: &str,
    key: &IdempotencyKey,
    value: &Value,
) -> Result<(), ApiError> {
    let rows = sqlx::query(
        "UPDATE access.idempotency_ledger SET state='completed',result=$4,completed_at=now() \
         WHERE operation=$1 AND scope_id=$2 AND idempotency_key=$3 AND state='in_progress'",
    )
    .bind(operation)
    .bind(scope_id)
    .bind(key.as_str())
    .bind(value)
    .execute(&mut **tx)
    .await
    .map_err(|_| ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))?
    .rows_affected();
    if rows != 1 {
        return Err(ApiError::conflict("LW_IDEMPOTENCY_CONFLICT"));
    }
    Ok(())
}

fn actor_idempotency_scope(actor_id: ActorId) -> String {
    format!("actor:{actor_id}")
}

fn service_idempotency_scope(principal: &MtlsPrincipal) -> String {
    format!("service:{}", principal.san_uri)
}

pub async fn create_ssh_key(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<(StatusCode, Json<SshPublicKey>), ApiError> {
    let request: CreateSshPublicKeyRequest = parse_body(&body)?;
    require_mutation_auth(&state, &headers).await?;
    let identity = authenticated_identity(&state, &headers).await?;
    let actor = actor_uuid(identity.actor.actor_id)?;
    let idem_scope = actor_idempotency_scope(identity.actor.actor_id);
    let validated = validate_ssh_public_key(&request.public_key_openssh)
        .map_err(|_| ApiError::unprocessable("LW_ACCESS_SSH_KEY_REJECTED"))?;
    let key = idempotency_key(&headers)?;
    let hash = request_hash(&request)?;
    let mut tx = state
        .pool
        .begin()
        .await
        .map_err(|_| ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))?;
    if let Some(value) =
        reserve_idempotency(&mut tx, "create_ssh_key", &idem_scope, &key, &hash).await?
    {
        let existing = serde_json::from_value(value)
            .map_err(|_| ApiError::internal("LW_ACCESS_STORE_CORRUPT"))?;
        tx.commit()
            .await
            .map_err(|_| ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))?;
        return Ok((StatusCode::OK, Json(existing)));
    }
    let actor_exists: Option<Uuid> =
        sqlx::query_scalar("SELECT actor_id FROM access.actors WHERE actor_id=$1 FOR UPDATE")
            .bind(actor)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|_| ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))?;
    if actor_exists.is_none() {
        return Err(ApiError::forbidden("LW_AUTH_SCOPE_DENIED"));
    }
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM access.ssh_public_keys WHERE actor_id=$1 AND revoked_at IS NULL",
    )
    .bind(actor)
    .fetch_one(&mut *tx)
    .await
    .map_err(|_| ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))?;
    if count >= i64::from(state.deployment.grants.max_keys_per_actor) {
        return Err(ApiError::conflict("LW_ACCESS_SSH_KEY_LIMIT"));
    }
    let key_id = SshPublicKeyId::new();
    let now = OffsetDateTime::now_utc();
    let algorithm = match validated.algorithm {
        SshKeyAlgorithm::Ed25519 => "ed25519",
        SshKeyAlgorithm::SecurityKeyEd25519 => "security_key_ed25519",
        SshKeyAlgorithm::RsaSha2 => "rsa_sha2",
    };
    sqlx::query(
        "INSERT INTO access.ssh_public_keys \
         (key_id,actor_id,fingerprint_sha256,algorithm,rsa_bits,normalized_openssh,revision,created_at) \
         VALUES ($1,$2,$3,$4,$5,$6,1,$7)",
    )
    .bind(key_id.as_uuid())
    .bind(actor)
    .bind(&validated.fingerprint_sha256)
    .bind(algorithm)
    .bind(validated.rsa_bits.map(i64::from))
    .bind(&validated.normalized_openssh)
    .bind(now)
    .execute(&mut *tx)
    .await
    .map_err(|error| {
        if error
            .as_database_error()
            .is_some_and(sqlx::error::DatabaseError::is_unique_violation)
        {
            ApiError::conflict("LW_ACCESS_SSH_KEY_DUPLICATE")
        } else {
            ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE")
        }
    })?;
    let result = SshPublicKey {
        id: key_id,
        actor_id: identity.actor.actor_id,
        fingerprint_sha256: validated.fingerprint_sha256,
        algorithm: validated.algorithm,
        rsa_bits: validated.rsa_bits,
        revision: Revision::new(1).map_err(|_| ApiError::internal("LW_ACCESS_STORE_CORRUPT"))?,
        created_at: utc_timestamp(now)?,
    };
    complete_idempotency(
        &mut tx,
        "create_ssh_key",
        &idem_scope,
        &key,
        &serde_json::to_value(&result)
            .map_err(|_| ApiError::internal("LW_ACCESS_STORE_CORRUPT"))?,
    )
    .await?;
    tx.commit()
        .await
        .map_err(|_| ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))?;
    Ok((StatusCode::CREATED, Json(result)))
}

#[derive(Deserialize)]
pub struct KeyListQuery {
    limit: Option<u16>,
}

pub async fn list_ssh_keys(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<KeyListQuery>,
) -> Result<Json<Value>, ApiError> {
    let identity = authenticated_identity(&state, &headers).await?;
    let actor = actor_uuid(identity.actor.actor_id)?;
    let limit = i64::from(query.limit.unwrap_or(50).clamp(1, 100));
    let rows = sqlx::query(
        "SELECT key_id,fingerprint_sha256,algorithm,rsa_bits,revision,created_at \
         FROM access.ssh_public_keys WHERE actor_id=$1 AND revoked_at IS NULL \
         ORDER BY created_at,key_id LIMIT $2",
    )
    .bind(actor)
    .bind(limit)
    .fetch_all(&state.pool)
    .await
    .map_err(|_| ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))?;
    let mut items = Vec::with_capacity(rows.len());
    for row in rows {
        items.push(ssh_key_from_row(&row, identity.actor.actor_id)?);
    }
    Ok(Json(json!({"items": items, "nextCursor": null})))
}

pub async fn delete_ssh_key(
    State(state): State<Arc<AppState>>,
    Path(key_id): Path<SshPublicKeyId>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    require_mutation_auth(&state, &headers).await?;
    let identity = authenticated_identity(&state, &headers).await?;
    let actor = actor_uuid(identity.actor.actor_id)?;
    let idem_scope = actor_idempotency_scope(identity.actor.actor_id);
    let expected = if_match(&headers)?;
    let idem = idempotency_key(&headers)?;
    let hash = request_hash(&json!({"keyId": key_id, "revision": expected}))?;
    let now = OffsetDateTime::now_utc();
    let terminate_by = now + time::Duration::seconds(TERMINATION_SECONDS);
    let mut tx = state
        .pool
        .begin()
        .await
        .map_err(|_| ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))?;
    if reserve_idempotency(&mut tx, "delete_ssh_key", &idem_scope, &idem, &hash)
        .await?
        .is_some()
    {
        tx.commit()
            .await
            .map_err(|_| ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))?;
        return Ok(StatusCode::NO_CONTENT);
    }
    let rows = sqlx::query(
        "UPDATE access.ssh_public_keys SET revoked_at=$4,revoke_reason_code='LW_ACCESS_SSH_KEY_REVOKED',revision=revision+1 \
         WHERE key_id=$1 AND actor_id=$2 AND revision=$3 AND revoked_at IS NULL",
    )
    .bind(key_id.as_uuid())
    .bind(actor)
    .bind(i64::try_from(expected.get()).map_err(|_| ApiError::precondition("LW_REVISION_CONFLICT"))?)
    .bind(now)
    .execute(&mut *tx).await.map_err(|_| ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))?.rows_affected();
    if rows != 1 {
        return Err(ApiError::precondition("LW_REVISION_CONFLICT"));
    }
    terminate_sessions_for_key(&mut tx, key_id, now, terminate_by).await?;
    enqueue_key_event(&mut tx, key_id, identity.actor.actor_id, now).await?;
    complete_idempotency(
        &mut tx,
        "delete_ssh_key",
        &idem_scope,
        &idem,
        &json!({"deleted": true}),
    )
    .await?;
    tx.commit()
        .await
        .map_err(|_| ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))?;
    Ok(StatusCode::NO_CONTENT)
}

#[allow(
    clippy::too_many_lines,
    reason = "the handler keeps one auditable transaction boundary for idempotency and grant creation"
)]
pub async fn create_access_grant(
    State(state): State<Arc<AppState>>,
    Path(environment_id): Path<contracts::EnvironmentId>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<(StatusCode, Json<AccessGrant>), ApiError> {
    let mut request: CreateAccessGrantRequest = parse_body(&body)?;
    require_mutation_auth(&state, &headers).await?;
    if request.environment_id != environment_id
        || request.endpoint_ids.is_empty()
        || request.endpoint_ids.len() > usize::from(state.deployment.grants.max_endpoints_per_grant)
        || request
            .endpoint_ids
            .iter()
            .copied()
            .collect::<BTreeSet<_>>()
            .len()
            != request.endpoint_ids.len()
    {
        return Err(ApiError::bad_request("LW_CONTRACT_DOCUMENT_INVALID"));
    }
    let identity = authenticated_identity(&state, &headers).await?;
    let actor = actor_uuid(identity.actor.actor_id)?;
    let idem_scope = actor_idempotency_scope(identity.actor.actor_id);
    let membership =
        active_membership(&state.pool, request.course_id, actor, &identity.actor.roles).await?;
    if membership.role == PlatformRole::PlatformAdmin {
        return Err(ApiError::forbidden("LW_ACCESS_ADMIN_GRANT_FORBIDDEN"));
    }
    let now = OffsetDateTime::now_utc();
    let default_expiry = now
        + time::Duration::seconds(
            i64::try_from(state.deployment.grants.default_ttl_seconds)
                .map_err(|_| ApiError::internal("LW_ACCESS_CONFIG_INVALID"))?,
        );
    let maximum_expiry = now
        + time::Duration::seconds(
            i64::try_from(state.deployment.grants.max_ttl_seconds)
                .map_err(|_| ApiError::internal("LW_ACCESS_CONFIG_INVALID"))?,
        );
    let requested_expiry = request.expires_at.map_or(default_expiry, UtcTimestamp::get);
    let effective_expiry = [
        requested_expiry,
        maximum_expiry,
        identity.expires_at.get(),
        membership.expires_at.unwrap_or(maximum_expiry),
    ]
    .into_iter()
    .min()
    .ok_or_else(|| ApiError::unprocessable("LW_ACCESS_GRANT_TTL_INVALID"))?;
    if effective_expiry <= now {
        return Err(ApiError::unprocessable("LW_ACCESS_GRANT_TTL_INVALID"));
    }
    request.expires_at = Some(utc_timestamp(effective_expiry)?);
    let idem = idempotency_key(&headers)?;
    let hash = request_hash(&request)?;
    let grant_id = AccessGrantId::new();
    let subject_kind = if membership.role == PlatformRole::Student {
        EnvironmentAccessSubjectKind::Owner
    } else {
        EnvironmentAccessSubjectKind::CourseTeacher
    };
    let contract = json!({"request": request, "subjectKind": subject_kind});
    let mut tx = state
        .pool
        .begin()
        .await
        .map_err(|_| ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))?;
    if let Some(value) =
        reserve_idempotency(&mut tx, "create_access_grant", &idem_scope, &idem, &hash).await?
    {
        let existing: AccessGrant = serde_json::from_value(value)
            .map_err(|_| ApiError::internal("LW_ACCESS_STORE_CORRUPT"))?;
        tx.commit()
            .await
            .map_err(|_| ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))?;
        return Ok((StatusCode::OK, Json(existing)));
    }
    sqlx::query(
        "INSERT INTO access.access_grants \
         (grant_id,actor_id,course_id,environment_id,environment_revision,revision,state,not_before,expires_at,contract,created_at,updated_at) \
         VALUES ($1,$2,$3,$4,$5,1,'requested',$6,$7,$8,$6,$6)",
    )
    .bind(grant_id.as_uuid()).bind(actor).bind(request.course_id.as_uuid()).bind(environment_id.as_uuid())
    .bind(i64::try_from(request.environment_revision.get()).map_err(|_| ApiError::bad_request("LW_CONTRACT_DOCUMENT_INVALID"))?)
    .bind(now).bind(effective_expiry).bind(&contract)
    .execute(&mut *tx).await.map_err(|error| if error.as_database_error().is_some_and(sqlx::error::DatabaseError::is_unique_violation) {
        ApiError::conflict("LW_ACCESS_GRANT_ALREADY_ACTIVE")
    } else { ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE") })?;
    sqlx::query(
        "INSERT INTO access.access_grant_activation_jobs (grant_id,state) VALUES ($1,'pending')",
    )
    .bind(grant_id.as_uuid())
    .execute(&mut *tx)
    .await
    .map_err(|_| ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))?;
    let result = AccessGrant {
        id: grant_id,
        actor_id: identity.actor.actor_id,
        course_id: request.course_id,
        environment_id,
        environment_revision: request.environment_revision,
        state: AccessGrantState::Requested,
        revision: Revision::new(1).map_err(|_| ApiError::internal("LW_ACCESS_STORE_CORRUPT"))?,
        endpoint_grants: Vec::new(),
        issued_at: utc_timestamp(now)?,
        expires_at: utc_timestamp(effective_expiry)?,
        revoked_at: None,
        reason_code: None,
    };
    enqueue_grant_event(&mut tx, &result, subjects::ACCESS_GRANT_CREATED, now).await?;
    complete_idempotency(
        &mut tx,
        "create_access_grant",
        &idem_scope,
        &idem,
        &serde_json::to_value(&result)
            .map_err(|_| ApiError::internal("LW_ACCESS_STORE_CORRUPT"))?,
    )
    .await?;
    tx.commit()
        .await
        .map_err(|_| ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))?;
    Ok((StatusCode::CREATED, Json(result)))
}

pub async fn get_access_grant(
    State(state): State<Arc<AppState>>,
    Path(grant_id): Path<AccessGrantId>,
    headers: HeaderMap,
) -> Result<Json<AccessGrant>, ApiError> {
    let identity = authenticated_identity(&state, &headers).await?;
    Ok(Json(
        load_grant(&state.pool, grant_id, Some(identity.actor.actor_id)).await?,
    ))
}

pub async fn list_access_grants(
    State(state): State<Arc<AppState>>,
    Path(environment_id): Path<contracts::EnvironmentId>,
    headers: HeaderMap,
    Query(query): Query<EnvironmentAccessGrantListQuery>,
) -> Result<Json<Value>, ApiError> {
    query
        .validate()
        .map_err(|_| ApiError::bad_request("LW_CONTRACT_DOCUMENT_INVALID"))?;
    let identity = authenticated_identity(&state, &headers).await?;
    let rows = sqlx::query(
        "SELECT grant_id FROM access.access_grants WHERE actor_id=$1 AND environment_id=$2 \
         AND ($3::text IS NULL OR state=$3) AND ($4 OR state IN ('requested','active')) \
         ORDER BY created_at DESC LIMIT $5",
    )
    .bind(actor_uuid(identity.actor.actor_id)?)
    .bind(environment_id.as_uuid())
    .bind(query.state.map(grant_state_str))
    .bind(query.include_terminal)
    .bind(i64::from(query.limit.unwrap_or(50).clamp(1, 100)))
    .fetch_all(&state.pool)
    .await
    .map_err(|_| ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))?;
    let mut items = Vec::with_capacity(rows.len());
    for row in rows {
        let id = typed_id::<AccessGrantId>(row.get::<Uuid, _>("grant_id"))?;
        let snapshot = load_grant_snapshot(&state.pool, id, identity.actor.actor_id).await?;
        if query.endpoint_id.is_none_or(|endpoint_id| {
            snapshot
                .endpoint_grants
                .iter()
                .any(|endpoint| endpoint.endpoint_id == endpoint_id)
        }) {
            items.push(snapshot);
        }
    }
    Ok(Json(json!({"items": items, "nextCursor": null})))
}

pub async fn renew_access_grant(
    State(state): State<Arc<AppState>>,
    Path(grant_id): Path<AccessGrantId>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<AccessGrant>, ApiError> {
    let request: RenewAccessGrantRequest = parse_body(&body)?;
    require_mutation_auth(&state, &headers).await?;
    if request.grant_id != grant_id {
        return Err(ApiError::bad_request("LW_CONTRACT_DOCUMENT_INVALID"));
    }
    let identity = authenticated_identity(&state, &headers).await?;
    let idem_scope = actor_idempotency_scope(identity.actor.actor_id);
    let expected = if_match(&headers)?;
    let idem = idempotency_key(&headers)?;
    let hash = request_hash(&request)?;
    let now = OffsetDateTime::now_utc();
    let max = now
        + time::Duration::seconds(
            i64::try_from(state.deployment.grants.max_ttl_seconds)
                .map_err(|_| ApiError::internal("LW_ACCESS_CONFIG_INVALID"))?,
        );
    if request.expires_at.get() <= now
        || request.expires_at.get() > max
        || request.expires_at > identity.expires_at
    {
        return Err(ApiError::unprocessable("LW_ACCESS_GRANT_TTL_INVALID"));
    }
    let mut tx = state
        .pool
        .begin()
        .await
        .map_err(|_| ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))?;
    if let Some(value) =
        reserve_idempotency(&mut tx, "renew_access_grant", &idem_scope, &idem, &hash).await?
    {
        let grant = serde_json::from_value(value)
            .map_err(|_| ApiError::internal("LW_ACCESS_STORE_CORRUPT"))?;
        tx.commit()
            .await
            .map_err(|_| ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))?;
        return Ok(Json(grant));
    }
    let rows = sqlx::query(
        "UPDATE access.access_grants SET expires_at=$4,revision=revision+1,updated_at=$5 \
         WHERE grant_id=$1 AND actor_id=$2 AND revision=$3 AND state='active' AND expires_at < $4",
    )
    .bind(grant_id.as_uuid())
    .bind(actor_uuid(identity.actor.actor_id)?)
    .bind(
        i64::try_from(expected.get())
            .map_err(|_| ApiError::precondition("LW_REVISION_CONFLICT"))?,
    )
    .bind(request.expires_at.get())
    .bind(now)
    .execute(&mut *tx)
    .await
    .map_err(|_| ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))?
    .rows_affected();
    if rows != 1 {
        return Err(ApiError::precondition("LW_REVISION_CONFLICT"));
    }
    sqlx::query("UPDATE access.endpoint_grants SET expires_at=$2 WHERE grant_id=$1")
        .bind(grant_id.as_uuid())
        .bind(request.expires_at.get())
        .execute(&mut *tx)
        .await
        .map_err(|_| ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))?;
    let grant = load_grant_tx(&mut tx, grant_id, Some(identity.actor.actor_id)).await?;
    complete_idempotency(
        &mut tx,
        "renew_access_grant",
        &idem_scope,
        &idem,
        &serde_json::to_value(&grant).map_err(|_| ApiError::internal("LW_ACCESS_STORE_CORRUPT"))?,
    )
    .await?;
    tx.commit()
        .await
        .map_err(|_| ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))?;
    Ok(Json(grant))
}

pub async fn revoke_access_grant(
    State(state): State<Arc<AppState>>,
    Path(grant_id): Path<AccessGrantId>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<(StatusCode, Json<AccessGrant>), ApiError> {
    let request: RevokeAccessGrantRequest = parse_body(&body)?;
    require_mutation_auth(&state, &headers).await?;
    if request.grant_id != grant_id || request.reason_code.trim().is_empty() {
        return Err(ApiError::bad_request("LW_CONTRACT_DOCUMENT_INVALID"));
    }
    let identity = authenticated_identity(&state, &headers).await?;
    let idem_scope = actor_idempotency_scope(identity.actor.actor_id);
    let expected = if_match(&headers)?;
    let idem = idempotency_key(&headers)?;
    let hash = request_hash(&request)?;
    let now = OffsetDateTime::now_utc();
    let terminate_by = now + time::Duration::seconds(TERMINATION_SECONDS);
    let mut tx = state
        .pool
        .begin()
        .await
        .map_err(|_| ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))?;
    if let Some(value) =
        reserve_idempotency(&mut tx, "revoke_access_grant", &idem_scope, &idem, &hash).await?
    {
        let grant = serde_json::from_value(value)
            .map_err(|_| ApiError::internal("LW_ACCESS_STORE_CORRUPT"))?;
        tx.commit()
            .await
            .map_err(|_| ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))?;
        return Ok((StatusCode::OK, Json(grant)));
    }
    let scope =
        sqlx::query("SELECT actor_id,course_id FROM access.access_grants WHERE grant_id=$1")
            .bind(grant_id.as_uuid())
            .fetch_optional(&mut *tx)
            .await
            .map_err(|_| ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))?
            .ok_or_else(|| ApiError::not_found("LW_ACCESS_GRANT_NOT_FOUND"))?;
    let owner: Uuid = scope.get("actor_id");
    if owner != actor_uuid(identity.actor.actor_id)? {
        if identity.actor.roles.contains(&PlatformRole::PlatformAdmin) {
            // Platform administrators may perform an audited emergency revocation.
        } else {
            let course_id = typed_id::<CourseId>(scope.get("course_id"))?;
            let membership = active_membership(
                &state.pool,
                course_id,
                actor_uuid(identity.actor.actor_id)?,
                &identity.actor.roles,
            )
            .await?;
            if membership.role != PlatformRole::Teacher {
                return Err(ApiError::forbidden("LW_ACCESS_DENIED"));
            }
        }
    }
    let rows = sqlx::query(
        "UPDATE access.access_grants SET state='revoked',revision=revision+1,revoked_at=$3,reason_code=$4,updated_at=$3 \
         WHERE grant_id=$1 AND revision=$2 AND state IN ('requested','active')",
    ).bind(grant_id.as_uuid())
      .bind(i64::try_from(expected.get()).map_err(|_| ApiError::precondition("LW_REVISION_CONFLICT"))?)
      .bind(now).bind(&request.reason_code)
      .execute(&mut *tx).await.map_err(|_| ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))?.rows_affected();
    if rows != 1 {
        return Err(ApiError::precondition("LW_REVISION_CONFLICT"));
    }
    terminate_sessions_for_grant(&mut tx, grant_id, now, terminate_by).await?;
    let grant = load_grant_tx(&mut tx, grant_id, None).await?;
    enqueue_grant_event(&mut tx, &grant, subjects::ACCESS_GRANT_REVOKED, now).await?;
    complete_idempotency(
        &mut tx,
        "revoke_access_grant",
        &idem_scope,
        &idem,
        &serde_json::to_value(&grant).map_err(|_| ApiError::internal("LW_ACCESS_STORE_CORRUPT"))?,
    )
    .await?;
    tx.commit()
        .await
        .map_err(|_| ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))?;
    Ok((StatusCode::ACCEPTED, Json(grant)))
}

#[allow(
    clippy::too_many_lines,
    reason = "the handler keeps the preflight authority check and transactional revalidation together for auditability"
)]
pub async fn authorize_ssh(
    State(state): State<Arc<AppState>>,
    axum::extract::Extension(principal): axum::extract::Extension<MtlsPrincipal>,
    body: Bytes,
) -> Result<Json<SshAuthorization>, ApiError> {
    let request: SshAuthorizationRequest = parse_body(&body)?;
    ensure_gateway_request(&state, &principal, &request.gateway_identity)?;
    if !valid_fingerprint(&request.presented_key_fingerprint_sha256)
        || request.connection_id.trim().is_empty()
        || !valid_sha256_hex(&request.source_address_hash)
    {
        return Err(ApiError::bad_request("LW_CONTRACT_DOCUMENT_INVALID"));
    }
    let now = OffsetDateTime::now_utc();
    if (request.requested_at.get() - now)
        .whole_seconds()
        .unsigned_abs()
        > 30
    {
        return Err(ApiError::forbidden("LW_ACCESS_AUTHORIZATION_STALE"));
    }
    // OpenSSH resolves the local account before AuthorizedKeysCommand. Therefore this
    // phase authenticates only the fixed `gateway` account and key. The exact endpoint
    // alias is selected and re-authorized when the forced command is redeemed.
    let candidate = sqlx::query(
        "SELECT k.key_id,k.actor_id,k.normalized_openssh FROM access.ssh_public_keys k \
         JOIN access.actors a ON a.actor_id=k.actor_id \
         WHERE k.fingerprint_sha256=$1 AND k.revoked_at IS NULL AND a.disabled_at IS NULL \
           AND EXISTS (SELECT 1 FROM access.access_grants g \
             JOIN access.endpoint_grants eg ON eg.grant_id=g.grant_id \
             JOIN access.course_memberships cm ON cm.course_id=g.course_id AND cm.actor_id=g.actor_id \
             WHERE g.actor_id=k.actor_id AND g.state='active' AND g.not_before<=$2 AND g.expires_at>$2 \
               AND eg.protocol='ssh' AND eg.health='healthy' AND eg.expires_at>$2 \
               AND cm.state='active' AND (cm.expires_at IS NULL OR cm.expires_at>$2) \
               AND cm.role=CASE g.contract->>'subjectKind' WHEN 'owner' THEN 'student' WHEN 'course_teacher' THEN 'teacher' ELSE '' END)",
    )
    .bind(&request.presented_key_fingerprint_sha256)
    .bind(now)
    .fetch_optional(&state.pool)
    .await
    .map_err(|_| ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))?
    .ok_or_else(|| ApiError::forbidden("LW_ACCESS_SSH_DENIED"))?;
    let actor_id = typed_id::<ActorId>(candidate.get("actor_id"))?;
    let key_id = typed_id::<SshPublicKeyId>(candidate.get("key_id"))?;
    let authorized_at = OffsetDateTime::now_utc();
    let authorization_id = Uuid::now_v7();
    let token = random_token();
    let token_hash = sha256_hex(token.as_bytes());
    let configured_until = authorized_at
        + time::Duration::seconds(
            i64::try_from(state.deployment.grants.authorization_token_ttl_seconds)
                .map_err(|_| ApiError::internal("LW_ACCESS_CONFIG_INVALID"))?,
        );
    let valid_until = configured_until;
    sqlx::query(
        "INSERT INTO access.ssh_authorizations \
         (authorization_id,token_sha256,actor_id,key_id,gateway_identity,connection_id,source_address_sha256,issued_at,expires_at) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)",
    ).bind(authorization_id).bind(token_hash).bind(actor_id.as_uuid()).bind(key_id.as_uuid())
      .bind(&principal.san_uri).bind(&request.connection_id).bind(&request.source_address_hash).bind(authorized_at).bind(valid_until)
      .execute(&state.pool).await.map_err(|_| ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))?;
    Ok(Json(SshAuthorization {
        authorization_id: authorization_id.to_string(),
        ssh_public_key_id: key_id,
        normalized_authorized_key: candidate.get("normalized_openssh"),
        force_command_token: token,
        valid_until: utc_timestamp(valid_until)?,
    }))
}

#[allow(
    clippy::too_many_lines,
    reason = "one transaction binds the one-time key authorization to the exact endpoint eligibility decision"
)]
pub async fn create_gateway_session(
    State(state): State<Arc<AppState>>,
    axum::extract::Extension(principal): axum::extract::Extension<MtlsPrincipal>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<(StatusCode, Json<GatewaySession>), ApiError> {
    let request: CreateGatewaySessionRequest = parse_body(&body)?;
    ensure_gateway_request(&state, &principal, &request.gateway_identity)?;
    validate_alias(&request.alias)?;
    let idem = idempotency_key(&headers)?;
    let idem_scope = service_idempotency_scope(&principal);
    let hash = request_hash(&request)?;
    let authorization_id = Uuid::parse_str(&request.authorization_id)
        .map_err(|_| ApiError::bad_request("LW_CONTRACT_DOCUMENT_INVALID"))?;
    let now = OffsetDateTime::now_utc();
    let session_id = GatewaySessionId::new();
    let mut tx = state
        .pool
        .begin()
        .await
        .map_err(|_| ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))?;
    if let Some(value) =
        reserve_idempotency(&mut tx, "create_gateway_session", &idem_scope, &idem, &hash).await?
    {
        let session = serde_json::from_value(value)
            .map_err(|_| ApiError::internal("LW_ACCESS_STORE_CORRUPT"))?;
        tx.commit()
            .await
            .map_err(|_| ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))?;
        return Ok((StatusCode::OK, Json(session)));
    }
    let auth = sqlx::query(
        "SELECT a.actor_id,a.key_id,a.expires_at \
         FROM access.ssh_authorizations a \
         JOIN access.ssh_public_keys k ON k.key_id=a.key_id \
         WHERE a.authorization_id=$1 AND a.token_sha256=$2 AND a.gateway_identity=$3 AND a.connection_id=$4 \
           AND a.consumed_at IS NULL AND a.expires_at>$5 AND k.actor_id=a.actor_id \
           AND k.revoked_at IS NULL FOR UPDATE OF a",
    ).bind(authorization_id).bind(sha256_hex(request.force_command_token.as_bytes()))
      .bind(&principal.san_uri).bind(&request.connection_id).bind(now)
      .fetch_optional(&mut *tx).await.map_err(|_| ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))?
      .ok_or_else(|| ApiError::forbidden("LW_ACCESS_FORCE_TOKEN_REJECTED"))?;
    let candidate = sqlx::query(
        "SELECT g.grant_id,g.revision AS grant_revision,g.actor_id,g.course_id,g.environment_id, \
                g.environment_revision,g.contract,g.expires_at,eg.endpoint_grant_id,eg.endpoint_id, \
                eg.endpoint_revision,eg.expires_at AS endpoint_expires_at,cm.expires_at AS membership_expires_at \
         FROM access.endpoint_grants eg JOIN access.access_grants g ON g.grant_id=eg.grant_id \
         JOIN access.course_memberships cm ON cm.course_id=g.course_id AND cm.actor_id=g.actor_id \
         JOIN access.ssh_public_keys k ON k.key_id=$2 AND k.actor_id=g.actor_id \
         WHERE eg.alias=$1 AND eg.protocol='ssh' AND eg.health='healthy' AND g.actor_id=$3 \
           AND g.state='active' AND g.not_before<=$4 AND g.expires_at>$4 AND eg.expires_at>$4 \
           AND k.revoked_at IS NULL AND cm.state='active' AND (cm.expires_at IS NULL OR cm.expires_at>$4) \
           AND cm.role=CASE g.contract->>'subjectKind' WHEN 'owner' THEN 'student' WHEN 'course_teacher' THEN 'teacher' ELSE '' END \
         FOR SHARE OF g,eg,k,cm",
    )
    .bind(&request.alias)
    .bind(auth.get::<Uuid, _>("key_id"))
    .bind(auth.get::<Uuid, _>("actor_id"))
    .bind(now)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|_| ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))?
    .ok_or_else(|| ApiError::forbidden("LW_ACCESS_SSH_DENIED"))?;
    let actor_id = typed_id::<ActorId>(candidate.get("actor_id"))?;
    let endpoint_id = typed_id::<EndpointId>(candidate.get("endpoint_id"))?;
    let endpoint_revision = revision(candidate.get("endpoint_revision"))?;
    let subject_kind: EnvironmentAccessSubjectKind = serde_json::from_value(
        candidate
            .get::<Value, _>("contract")
            .get("subjectKind")
            .cloned()
            .ok_or_else(|| ApiError::internal("LW_ACCESS_STORE_CORRUPT"))?,
    )
    .map_err(|_| ApiError::internal("LW_ACCESS_STORE_CORRUPT"))?;
    let eligibility = state
        .owner_resolver
        .resolve_endpoint_eligibility(
            &EnvironmentEndpointEligibilityRequest {
                environment_id: typed_id(candidate.get("environment_id"))?,
                course_id: typed_id(candidate.get("course_id"))?,
                actor_id,
                subject_kind,
                expected_revision: revision(candidate.get("environment_revision"))?,
                endpoint_ids: vec![endpoint_id],
            },
            utc_timestamp(now)?,
        )
        .await
        .map_err(|error| match error {
            auth::OwnerResolverClientError::ScopeDenied
            | auth::OwnerResolverClientError::ResponseInvalid => {
                ApiError::forbidden("LW_ACCESS_SSH_DENIED")
            }
            _ => ApiError::unavailable("LW_ACCESS_SSH_AUTHORITY_UNAVAILABLE"),
        })?;
    let resolved = eligibility
        .endpoints
        .first()
        .ok_or_else(|| ApiError::forbidden("LW_ACCESS_SSH_DENIED"))?;
    if resolved.protocol != EndpointProtocol::Ssh
        || resolved.health != EndpointHealth::Healthy
        || resolved.revision != endpoint_revision
        || eligibility.eligibility_expires_at.get() <= now
    {
        return Err(ApiError::forbidden("LW_ACCESS_SSH_DENIED"));
    }
    let target_ssh_host_key_identity_sha256 = resolved
        .ssh_host_key_identity_sha256
        .ok_or_else(|| ApiError::forbidden("LW_ACCESS_SSH_DENIED"))?;
    let expires_at = [
        auth.get::<OffsetDateTime, _>("expires_at"),
        candidate.get::<OffsetDateTime, _>("expires_at"),
        candidate.get::<OffsetDateTime, _>("endpoint_expires_at"),
        candidate
            .get::<Option<OffsetDateTime>, _>("membership_expires_at")
            .unwrap_or(eligibility.eligibility_expires_at.get()),
        eligibility.eligibility_expires_at.get(),
    ]
    .into_iter()
    .min()
    .ok_or_else(|| ApiError::internal("LW_ACCESS_STORE_CORRUPT"))?;
    sqlx::query(
        "INSERT INTO access.gateway_sessions \
         (session_id,grant_id,grant_revision,actor_id,endpoint_id,endpoint_grant_id,key_id,state,started_at,expires_at,contract,gateway_identity,connection_id,revision,last_heartbeat_at) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,'active',$8,$9,$10,$11,$12,1,$8)",
    ).bind(session_id.as_uuid()).bind(candidate.get::<Uuid,_>("grant_id")).bind(candidate.get::<i64,_>("grant_revision"))
      .bind(auth.get::<Uuid,_>("actor_id")).bind(candidate.get::<Uuid,_>("endpoint_id"))
      .bind(candidate.get::<Uuid,_>("endpoint_grant_id")).bind(auth.get::<Uuid,_>("key_id"))
      .bind(now).bind(expires_at).bind(json!({
          "authorizationId": authorization_id,
          "alias": request.alias,
          "sshHostKeyIdentitySha256": target_ssh_host_key_identity_sha256,
      }))
      .bind(&principal.san_uri).bind(&request.connection_id)
      .execute(&mut *tx).await.map_err(|_| ApiError::conflict("LW_ACCESS_SESSION_CONFLICT"))?;
    sqlx::query("UPDATE access.ssh_authorizations SET consumed_at=$2,session_id=$3 WHERE authorization_id=$1 AND consumed_at IS NULL")
      .bind(authorization_id).bind(now).bind(session_id.as_uuid()).execute(&mut *tx).await
      .map_err(|_| ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))?;
    let session = load_session_tx(&mut tx, session_id).await?;
    complete_idempotency(
        &mut tx,
        "create_gateway_session",
        &idem_scope,
        &idem,
        &serde_json::to_value(&session)
            .map_err(|_| ApiError::internal("LW_ACCESS_STORE_CORRUPT"))?,
    )
    .await?;
    tx.commit()
        .await
        .map_err(|_| ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))?;
    Ok((StatusCode::CREATED, Json(session)))
}

pub async fn heartbeat_gateway_session(
    State(state): State<Arc<AppState>>,
    axum::extract::Extension(principal): axum::extract::Extension<MtlsPrincipal>,
    Path(session_id): Path<GatewaySessionId>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<GatewaySession>, ApiError> {
    let request: HeartbeatGatewaySessionRequest = parse_body(&body)?;
    ensure_gateway_request(&state, &principal, &request.gateway_identity)?;
    let expected = if_match(&headers)?;
    if expected != request.expected_revision {
        return Err(ApiError::precondition("LW_REVISION_CONFLICT"));
    }
    let now = OffsetDateTime::now_utc();
    let rows = sqlx::query(
        "UPDATE access.gateway_sessions SET last_heartbeat_at=$4,revision=revision+1 \
         WHERE session_id=$1 AND gateway_identity=$2 AND connection_id=$3 AND revision=$5 AND state IN ('active','terminating')",
    ).bind(session_id.as_uuid()).bind(&principal.san_uri).bind(&request.connection_id).bind(now)
      .bind(i64::try_from(expected.get()).map_err(|_| ApiError::precondition("LW_REVISION_CONFLICT"))?)
      .execute(&state.pool).await.map_err(|_| ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))?.rows_affected();
    if rows != 1 {
        return Err(ApiError::precondition("LW_REVISION_CONFLICT"));
    }
    Ok(Json(load_session(&state.pool, session_id).await?))
}

pub async fn close_gateway_session(
    State(state): State<Arc<AppState>>,
    axum::extract::Extension(principal): axum::extract::Extension<MtlsPrincipal>,
    Path(session_id): Path<GatewaySessionId>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<GatewaySession>, ApiError> {
    let request: CloseGatewaySessionRequest = parse_body(&body)?;
    ensure_gateway_request(&state, &principal, &request.gateway_identity)?;
    let expected = if_match(&headers)?;
    if expected != request.expected_revision || request.reason_code.trim().is_empty() {
        return Err(ApiError::precondition("LW_REVISION_CONFLICT"));
    }
    let now = OffsetDateTime::now_utc();
    let mut tx = state
        .pool
        .begin()
        .await
        .map_err(|_| ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))?;
    let rows = sqlx::query(
        "UPDATE access.gateway_sessions SET state='closed',terminated_at=$4,close_reason_code=$5,revision=revision+1 \
         WHERE session_id=$1 AND gateway_identity=$2 AND connection_id=$3 AND revision=$6 AND state IN ('active','terminating','termination_overdue')",
    ).bind(session_id.as_uuid()).bind(&principal.san_uri).bind(&request.connection_id).bind(now).bind(&request.reason_code)
      .bind(i64::try_from(expected.get()).map_err(|_| ApiError::precondition("LW_REVISION_CONFLICT"))?)
      .execute(&mut *tx).await.map_err(|_| ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))?.rows_affected();
    if rows != 1 {
        return Err(ApiError::precondition("LW_REVISION_CONFLICT"));
    }
    let session = load_session_tx(&mut tx, session_id).await?;
    enqueue_session_event(
        &mut tx,
        &session,
        subjects::ACCESS_SESSION_CLOSED,
        now,
        &request.reason_code,
    )
    .await?;
    tx.commit()
        .await
        .map_err(|_| ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))?;
    Ok(Json(session))
}

pub async fn activation_loop(state: Arc<AppState>) -> Result<(), GrantRuntimeError> {
    let interval = Duration::from_secs(state.deployment.grants.activation_poll_seconds);
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        if let Err(error) = activate_one(&state).await {
            tracing::error!(event="access.grant.activation_failed", diagnostic=%error);
        }
    }
}

async fn activate_one(state: &AppState) -> Result<(), GrantRuntimeError> {
    let owner = format!("access-{}", Uuid::now_v7());
    let token = Uuid::now_v7();
    let lease_seconds = i64::try_from(state.deployment.grants.worker_lease_seconds)
        .map_err(|_| GrantRuntimeError::Config)?;
    let row = sqlx::query(
        "WITH candidate AS (SELECT grant_id FROM access.access_grant_activation_jobs \
         WHERE (state IN ('pending','retry') AND next_attempt_at<=now()) \
            OR (state='leased' AND lease_expires_at<=now()) ORDER BY next_attempt_at,grant_id \
         FOR UPDATE SKIP LOCKED LIMIT 1) UPDATE access.access_grant_activation_jobs j \
         SET state='leased',lease_owner=$1,lease_token=$2,lease_expires_at=now()+($3*interval '1 second'),updated_at=now() \
         FROM candidate WHERE j.grant_id=candidate.grant_id RETURNING j.grant_id",
    ).bind(&owner).bind(token).bind(lease_seconds).fetch_optional(&state.pool).await?;
    let Some(row) = row else {
        return Ok(());
    };
    let grant_uuid: Uuid = row.get("grant_id");
    let grant_row = sqlx::query("SELECT actor_id,course_id,environment_id,environment_revision,expires_at,contract FROM access.access_grants WHERE grant_id=$1 AND state='requested'")
        .bind(grant_uuid).fetch_optional(&state.pool).await?;
    let Some(grant_row) = grant_row else {
        let rows = sqlx::query("UPDATE access.access_grant_activation_jobs SET state='completed',lease_owner=NULL,lease_token=NULL,lease_expires_at=NULL,updated_at=now() WHERE grant_id=$1 AND state='leased' AND lease_token=$2 AND lease_expires_at>now()")
            .bind(grant_uuid).bind(token).execute(&state.pool).await?.rows_affected();
        if rows == 0 {
            log_activation_lease_lost(grant_uuid, "complete_inactive_grant");
        }
        return Ok(());
    };
    let contract: Value = grant_row.get("contract");
    let request: CreateAccessGrantRequest = serde_json::from_value(
        contract
            .get("request")
            .cloned()
            .ok_or(GrantRuntimeError::Contract)?,
    )
    .map_err(|_| GrantRuntimeError::Contract)?;
    let subject_kind: EnvironmentAccessSubjectKind = serde_json::from_value(
        contract
            .get("subjectKind")
            .cloned()
            .ok_or(GrantRuntimeError::Contract)?,
    )
    .map_err(|_| GrantRuntimeError::Contract)?;
    let actor_id =
        typed_id::<ActorId>(grant_row.get("actor_id")).map_err(|_| GrantRuntimeError::Contract)?;
    let eligibility_request = EnvironmentEndpointEligibilityRequest {
        environment_id: request.environment_id,
        course_id: request.course_id,
        actor_id,
        subject_kind,
        expected_revision: request.environment_revision,
        endpoint_ids: request.endpoint_ids.clone(),
    };
    let now = utc_timestamp(OffsetDateTime::now_utc()).map_err(|_| GrantRuntimeError::Contract)?;
    match state
        .owner_resolver
        .resolve_endpoint_eligibility(&eligibility_request, now)
        .await
    {
        Ok(eligibility) => activate_grant(state, grant_uuid, token, eligibility).await?,
        Err(
            auth::OwnerResolverClientError::ScopeDenied
            | auth::OwnerResolverClientError::ResponseInvalid,
        ) => {
            deny_grant(
                state,
                grant_uuid,
                token,
                "LW_ACCESS_ENDPOINT_ELIGIBILITY_DENIED",
            )
            .await?;
        }
        Err(_) => {
            retry_activation(
                state,
                grant_uuid,
                token,
                "LW_ACCESS_ENDPOINT_RESOLVER_UNAVAILABLE",
            )
            .await?;
        }
    }
    Ok(())
}

async fn activate_grant(
    state: &AppState,
    grant_id: Uuid,
    lease_token: Uuid,
    eligibility: contracts::environment::EnvironmentEndpointEligibility,
) -> Result<(), GrantRuntimeError> {
    let now = OffsetDateTime::now_utc();
    let mut tx = state.pool.begin().await?;
    if !lock_activation_lease(&mut tx, grant_id, lease_token).await? {
        log_activation_lease_lost(grant_id, "activate");
        tx.rollback().await?;
        return Ok(());
    }
    let grant = sqlx::query("SELECT expires_at,revision FROM access.access_grants WHERE grant_id=$1 AND state='requested' FOR UPDATE")
        .bind(grant_id).fetch_optional(&mut *tx).await?;
    let Some(grant) = grant else {
        complete_activation_job(&mut tx, grant_id, lease_token, now).await?;
        tx.commit().await?;
        return Ok(());
    };
    let expires_at = std::cmp::min(
        grant.get::<OffsetDateTime, _>("expires_at"),
        eligibility.eligibility_expires_at.get(),
    );
    if expires_at <= now {
        deny_grant_tx(
            &mut tx,
            grant_id,
            lease_token,
            "LW_ACCESS_GRANT_EXPIRED_BEFORE_ACTIVATION",
            now,
        )
        .await?;
        tx.commit().await?;
        return Ok(());
    }
    for endpoint in eligibility.endpoints {
        let endpoint_grant_id = EndpointGrantId::new();
        let alias =
            (endpoint.protocol == EndpointProtocol::Ssh).then(|| ssh_alias(endpoint_grant_id));
        let ssh_gateway_hostname = (endpoint.protocol == EndpointProtocol::Ssh)
            .then(|| state.deployment.grants.public_ssh_gateway_hostname.clone());
        let ssh_gateway_port = (endpoint.protocol == EndpointProtocol::Ssh)
            .then_some(state.deployment.grants.public_ssh_gateway_port);
        let ssh_gateway_host_key_fingerprint =
            (endpoint.protocol == EndpointProtocol::Ssh).then(|| {
                state
                    .deployment
                    .grants
                    .public_ssh_gateway_host_key_fingerprint
                    .clone()
            });
        let contract = json!({
            "endpointId": endpoint.id,
            "endpointRevision": endpoint.revision,
            "protocol": endpoint.protocol,
            "health": endpoint.health,
            "sshGatewayHostname": ssh_gateway_hostname,
            "sshGatewayPort": ssh_gateway_port,
            "sshGatewayHostKeyFingerprint": ssh_gateway_host_key_fingerprint,
        });
        sqlx::query("INSERT INTO access.endpoint_grants (endpoint_grant_id,grant_id,endpoint_id,endpoint_revision,protocol,health,alias,expires_at,contract) VALUES ($1,$2,$3,$4,$5,'healthy',$6,$7,$8)")
            .bind(endpoint_grant_id.as_uuid()).bind(grant_id).bind(endpoint.id.as_uuid())
            .bind(i64::try_from(endpoint.revision.get()).map_err(|_| GrantRuntimeError::Contract)?)
            .bind(protocol_str(endpoint.protocol)).bind(alias).bind(expires_at).bind(contract)
            .execute(&mut *tx).await?;
    }
    let rows = sqlx::query("UPDATE access.access_grants SET state='active',revision=revision+1,expires_at=$2,updated_at=$3,last_activation_diagnostic=NULL WHERE grant_id=$1 AND state='requested'")
        .bind(grant_id).bind(expires_at).bind(now).execute(&mut *tx).await?.rows_affected();
    if rows != 1 {
        return Err(GrantRuntimeError::ActivationLeaseLost);
    }
    complete_activation_job(&mut tx, grant_id, lease_token, now).await?;
    let typed = typed_id::<AccessGrantId>(grant_id).map_err(|_| GrantRuntimeError::Contract)?;
    let value = load_grant_tx_runtime(&mut tx, typed).await?;
    enqueue_grant_event_runtime(&mut tx, &value, subjects::ACCESS_GRANT_ACTIVATED, now).await?;
    tx.commit().await?;
    Ok(())
}

async fn deny_grant(
    state: &AppState,
    grant_id: Uuid,
    token: Uuid,
    diagnostic: &str,
) -> Result<(), GrantRuntimeError> {
    let now = OffsetDateTime::now_utc();
    let mut tx = state.pool.begin().await?;
    if !lock_activation_lease(&mut tx, grant_id, token).await? {
        log_activation_lease_lost(grant_id, "deny");
        tx.rollback().await?;
        return Ok(());
    }
    deny_grant_tx(&mut tx, grant_id, token, diagnostic, now).await?;
    tx.commit().await?;
    Ok(())
}

async fn deny_grant_tx(
    tx: &mut Transaction<'_, Postgres>,
    grant_id: Uuid,
    token: Uuid,
    diagnostic: &str,
    now: OffsetDateTime,
) -> Result<(), GrantRuntimeError> {
    let rows = sqlx::query("UPDATE access.access_grants SET state='denied',revision=revision+1,reason_code=$2,last_activation_diagnostic=$2,updated_at=$3 WHERE grant_id=$1 AND state='requested'")
        .bind(grant_id).bind(diagnostic).bind(now).execute(&mut **tx).await?.rows_affected();
    if rows != 1 {
        return Err(GrantRuntimeError::ActivationLeaseLost);
    }
    let rows = sqlx::query("UPDATE access.access_grant_activation_jobs SET state='failed',last_diagnostic=$3,lease_owner=NULL,lease_token=NULL,lease_expires_at=NULL,updated_at=$4 WHERE grant_id=$1 AND state='leased' AND lease_token=$2 AND lease_expires_at>now()")
        .bind(grant_id).bind(token).bind(diagnostic).bind(now).execute(&mut **tx).await?.rows_affected();
    if rows != 1 {
        return Err(GrantRuntimeError::ActivationLeaseLost);
    }
    let typed = typed_id::<AccessGrantId>(grant_id).map_err(|_| GrantRuntimeError::Contract)?;
    let value = load_grant_tx_runtime(tx, typed).await?;
    enqueue_grant_event_runtime(tx, &value, subjects::ACCESS_GRANT_DENIED, now).await?;
    Ok(())
}

async fn retry_activation(
    state: &AppState,
    grant_id: Uuid,
    token: Uuid,
    diagnostic: &str,
) -> Result<(), GrantRuntimeError> {
    let retry = i64::try_from(state.deployment.grants.activation_retry_seconds)
        .map_err(|_| GrantRuntimeError::Config)?;
    let mut tx = state.pool.begin().await?;
    let Some(attempts) = sqlx::query_scalar::<_, i32>(
        "SELECT attempts FROM access.access_grant_activation_jobs WHERE grant_id=$1 AND state='leased' AND lease_token=$2 AND lease_expires_at>now() FOR UPDATE",
    )
    .bind(grant_id)
    .bind(token)
    .fetch_optional(&mut *tx)
    .await?
    else {
        log_activation_lease_lost(grant_id, "retry");
        tx.rollback().await?;
        return Ok(());
    };
    let exhausted =
        attempts.saturating_add(1) >= i32::from(state.deployment.grants.activation_max_attempts);
    let effective_diagnostic = if exhausted {
        "LW_ACCESS_ENDPOINT_RESOLVER_RETRY_EXHAUSTED"
    } else {
        diagnostic
    };
    let next_state = if exhausted { "failed" } else { "retry" };
    let rows = sqlx::query("UPDATE access.access_grant_activation_jobs SET state=$3,attempts=attempts+1,next_attempt_at=now()+($4*interval '1 second'),last_diagnostic=$5,lease_owner=NULL,lease_token=NULL,lease_expires_at=NULL,updated_at=now() WHERE grant_id=$1 AND state='leased' AND lease_token=$2 AND lease_expires_at>now()")
        .bind(grant_id).bind(token).bind(next_state).bind(retry).bind(effective_diagnostic).execute(&mut *tx).await?.rows_affected();
    if rows != 1 {
        return Err(GrantRuntimeError::ActivationLeaseLost);
    }
    let rows = sqlx::query("UPDATE access.access_grants SET activation_attempts=activation_attempts+1,last_activation_diagnostic=$2,updated_at=now() WHERE grant_id=$1 AND state='requested'")
        .bind(grant_id).bind(effective_diagnostic).execute(&mut *tx).await?.rows_affected();
    if rows != 1 {
        return Err(GrantRuntimeError::ActivationLeaseLost);
    }
    tx.commit().await?;
    Ok(())
}

async fn lock_activation_lease(
    tx: &mut Transaction<'_, Postgres>,
    grant_id: Uuid,
    token: Uuid,
) -> Result<bool, sqlx::Error> {
    let held = sqlx::query_scalar::<_, bool>(
        "SELECT true FROM access.access_grant_activation_jobs WHERE grant_id=$1 AND state='leased' AND lease_token=$2 AND lease_expires_at>now() FOR UPDATE",
    )
    .bind(grant_id)
    .bind(token)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(held.unwrap_or(false))
}

async fn complete_activation_job(
    tx: &mut Transaction<'_, Postgres>,
    grant_id: Uuid,
    token: Uuid,
    now: OffsetDateTime,
) -> Result<(), GrantRuntimeError> {
    let rows = sqlx::query("UPDATE access.access_grant_activation_jobs SET state='completed',lease_owner=NULL,lease_token=NULL,lease_expires_at=NULL,updated_at=$3 WHERE grant_id=$1 AND state='leased' AND lease_token=$2 AND lease_expires_at>now()")
        .bind(grant_id).bind(token).bind(now).execute(&mut **tx).await?.rows_affected();
    if rows != 1 {
        return Err(GrantRuntimeError::ActivationLeaseLost);
    }
    Ok(())
}

fn log_activation_lease_lost(grant_id: Uuid, action: &'static str) {
    tracing::warn!(
        event = "access.grant.activation_lease_lost",
        diagnostic = "LW_ACCESS_ACTIVATION_LEASE_LOST",
        %grant_id,
        action,
        "stale activation worker was fenced"
    );
}

/// Handles the synchronous Environment owner request/reply used to revoke all
/// live grants before stop, restart, delete, cancellation, or expiry advances.
pub async fn environment_revocation_loop(state: Arc<AppState>) -> Result<(), GrantRuntimeError> {
    let mut subscriber = state
        .nats
        .subscribe(ACCESS_REVOCATION_SUBJECT)
        .await
        .map_err(|_| GrantRuntimeError::NatsSubscribe)?;
    while let Some(message) = subscriber.next().await {
        let Some(reply) = message.reply else {
            tracing::warn!(
                event = "access.environment_revocation.request_rejected",
                diagnostic = "LW_ACCESS_REVOCATION_REPLY_MISSING"
            );
            continue;
        };
        let request = match contracts::parse_strict_json::<EnvironmentAccessRevocationRequest>(
            &message.payload,
        ) {
            Ok(request) if valid_environment_revocation_request(&request) => request,
            _ => {
                tracing::warn!(
                    event = "access.environment_revocation.request_rejected",
                    diagnostic = "LW_ACCESS_REVOCATION_REQUEST_INVALID"
                );
                continue;
            }
        };
        let access_revocation_revision = revoke_environment_grants(&state.pool, &request).await?;
        let response = EnvironmentAccessRevocationResponse {
            version: 1,
            environment_id: request.environment_id,
            environment_revision: request.environment_revision,
            access_revocation_revision,
        };
        let payload = serde_json::to_vec(&response).map_err(|_| GrantRuntimeError::Contract)?;
        state
            .nats
            .publish(reply, payload.into())
            .await
            .map_err(|_| GrantRuntimeError::NatsPublish)?;
        tracing::info!(
            event = "access.environment_revocation.completed",
            environment_id = %request.environment_id,
            environment_revision = request.environment_revision.get(),
            access_revocation_revision = access_revocation_revision.get(),
            reason = request.reason
        );
    }
    Err(GrantRuntimeError::NatsSubscribe)
}

fn valid_environment_revocation_request(request: &EnvironmentAccessRevocationRequest) -> bool {
    request.version == 1
        && matches!(
            request.reason.as_str(),
            "environment_stopped"
                | "environment_restarted"
                | "environment_deleted"
                | "environment_cancelled"
                | "environment_expired"
        )
}

async fn revoke_environment_grants(
    pool: &PgPool,
    request: &EnvironmentAccessRevocationRequest,
) -> Result<Revision, GrantRuntimeError> {
    let now = OffsetDateTime::now_utc();
    let terminate_by = now + time::Duration::seconds(TERMINATION_SECONDS);
    let mut transaction = pool.begin().await?;
    let rows = sqlx::query(
        "UPDATE access.access_grants \
         SET state='revoked',revision=revision+1,revoked_at=$2,reason_code=$3,updated_at=$2 \
         WHERE environment_id=$1 AND state IN ('requested','active') RETURNING grant_id",
    )
    .bind(request.environment_id.as_uuid())
    .bind(now)
    .bind(&request.reason)
    .fetch_all(&mut *transaction)
    .await?;
    for row in rows {
        let grant_id = typed_id::<AccessGrantId>(row.get("grant_id"))
            .map_err(|error| log_revocation_mutation_error(&error))?;
        terminate_sessions_for_grant(&mut transaction, grant_id, now, terminate_by)
            .await
            .map_err(|error| log_revocation_mutation_error(&error))?;
        let grant = load_grant_tx(&mut transaction, grant_id, None)
            .await
            .map_err(|error| log_revocation_mutation_error(&error))?;
        enqueue_grant_event(
            &mut transaction,
            &grant,
            subjects::ACCESS_GRANT_REVOKED,
            now,
        )
        .await
        .map_err(|error| log_revocation_mutation_error(&error))?;
    }
    let maximum_revision: i64 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(revision),1) FROM access.access_grants WHERE environment_id=$1",
    )
    .bind(request.environment_id.as_uuid())
    .fetch_one(&mut *transaction)
    .await?;
    transaction.commit().await?;
    revision(maximum_revision).map_err(|error| log_revocation_mutation_error(&error))
}

fn log_revocation_mutation_error(error: &ApiError) -> GrantRuntimeError {
    tracing::error!(
        event = "access.environment_revocation.failed",
        diagnostic = error.diagnostic,
        status = error.status.as_u16()
    );
    GrantRuntimeError::AccessMutation
}

pub async fn maintenance_loop(state: Arc<AppState>) -> Result<(), GrantRuntimeError> {
    let interval = Duration::from_secs(state.deployment.grants.expiry_poll_seconds);
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        expire_grants(&state.pool).await?;
        mark_overdue(&state.pool).await?;
        cleanup_authorizations(&state.pool).await?;
    }
}

pub async fn outbox_loop(state: Arc<AppState>) -> Result<(), GrantRuntimeError> {
    let context = async_nats::jetstream::new(state.nats.clone());
    let mut ticker = tokio::time::interval(Duration::from_millis(250));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        let rows = sqlx::query("SELECT event_id,subject,payload FROM access.outbox_events WHERE published_at IS NULL ORDER BY created_at,event_id LIMIT 32")
            .fetch_all(&state.pool).await?;
        for row in rows {
            let event_id: Uuid = row.get("event_id");
            let subject: String = row.get("subject");
            let payload: Value = row.get("payload");
            let bytes = serde_json::to_vec(&payload).map_err(|_| GrantRuntimeError::Contract)?;
            let ack = context
                .send_publish(
                    subject,
                    PublishMessage::build()
                        .payload(bytes.into())
                        .message_id(event_id.to_string()),
                )
                .await
                .map_err(|_| GrantRuntimeError::NatsPublish)?;
            ack.await.map_err(|_| GrantRuntimeError::NatsPublish)?;
            sqlx::query("UPDATE access.outbox_events SET published_at=now() WHERE event_id=$1 AND published_at IS NULL")
                .bind(event_id).execute(&state.pool).await?;
        }
    }
}

async fn active_membership(
    pool: &PgPool,
    course_id: CourseId,
    actor_id: Uuid,
    roles: &[PlatformRole],
) -> Result<Membership, ApiError> {
    let now = OffsetDateTime::now_utc();
    let rows = sqlx::query("SELECT role,expires_at FROM access.course_memberships WHERE course_id=$1 AND actor_id=$2 AND state='active' AND (expires_at IS NULL OR expires_at>$3) ORDER BY CASE role WHEN 'platform_admin' THEN 0 WHEN 'teacher' THEN 1 ELSE 2 END")
        .bind(course_id.as_uuid()).bind(actor_id).bind(now).fetch_all(pool).await
        .map_err(|_| ApiError::unavailable("LW_AUTH_MEMBERSHIP_UNAVAILABLE"))?;
    for row in rows {
        let role = match row.get::<String, _>("role").as_str() {
            "student" => PlatformRole::Student,
            "teacher" => PlatformRole::Teacher,
            "platform_admin" => PlatformRole::PlatformAdmin,
            _ => continue,
        };
        if roles.contains(&role) {
            return Ok(Membership {
                role,
                expires_at: row.get("expires_at"),
            });
        }
    }
    Err(ApiError::forbidden("LW_AUTH_SCOPE_DENIED"))
}

struct Membership {
    role: PlatformRole,
    expires_at: Option<OffsetDateTime>,
}

async fn load_grant(
    pool: &PgPool,
    grant_id: AccessGrantId,
    actor: Option<ActorId>,
) -> Result<AccessGrant, ApiError> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|_| ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))?;
    let grant = load_grant_tx(&mut tx, grant_id, actor).await?;
    tx.commit()
        .await
        .map_err(|_| ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))?;
    Ok(grant)
}

async fn load_grant_snapshot(
    pool: &PgPool,
    grant_id: AccessGrantId,
    actor_id: ActorId,
) -> Result<AccessGrantSnapshot, ApiError> {
    let grant = load_grant(pool, grant_id, Some(actor_id)).await?;
    let stream: i64 = sqlx::query_scalar(
        "SELECT last_stream_sequence FROM access.access_grants WHERE grant_id=$1",
    )
    .bind(grant_id.as_uuid())
    .fetch_one(pool)
    .await
    .map_err(|_| ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))?;
    let endpoint_grants = grant
        .endpoint_grants
        .iter()
        .map(|endpoint| EndpointGrantSnapshot {
            id: endpoint.id,
            endpoint_id: endpoint.endpoint_id,
            endpoint_revision: endpoint.endpoint_revision,
            protocol: endpoint.protocol,
            alias: endpoint.alias.clone(),
            state: match grant.state {
                AccessGrantState::Expired => EndpointGrantSnapshotState::Expired,
                AccessGrantState::Revoked | AccessGrantState::Denied => {
                    EndpointGrantSnapshotState::Revoked
                }
                _ if endpoint.health != EndpointHealth::Healthy => {
                    EndpointGrantSnapshotState::Unhealthy
                }
                _ => EndpointGrantSnapshotState::Active,
            },
            expires_at: endpoint.expires_at,
        })
        .collect();
    let terminal = matches!(
        grant.state,
        AccessGrantState::Denied | AccessGrantState::Expired | AccessGrantState::Revoked
    );
    let snapshot = AccessGrantSnapshot {
        id: grant.id,
        environment_id: grant.environment_id,
        environment_revision: grant.environment_revision,
        state: grant.state,
        revision: grant.revision,
        endpoint_grants,
        issued_at: grant.issued_at,
        expires_at: grant.expires_at,
        revoked_at: grant.revoked_at,
        reason_code: grant.reason_code.clone(),
        decision: AuthorizationDecisionSummary {
            decision: if terminal {
                AuthorizationDecision::Terminal
            } else if grant.state == AccessGrantState::Active {
                AuthorizationDecision::Allow
            } else {
                AuthorizationDecision::Deny
            },
            reason_code: grant
                .reason_code
                .unwrap_or_else(|| grant_state_str(grant.state).to_owned()),
            evaluated_at: utc_timestamp(OffsetDateTime::now_utc())?,
        },
        last_changed_stream_sequence: StreamSequence(
            u64::try_from(stream).map_err(|_| ApiError::internal("LW_ACCESS_STORE_CORRUPT"))?,
        ),
    };
    snapshot
        .validate()
        .map_err(|_| ApiError::internal("LW_ACCESS_STORE_CORRUPT"))?;
    Ok(snapshot)
}

async fn load_grant_tx(
    tx: &mut Transaction<'_, Postgres>,
    grant_id: AccessGrantId,
    actor: Option<ActorId>,
) -> Result<AccessGrant, ApiError> {
    let row = sqlx::query("SELECT actor_id,course_id,environment_id,environment_revision,revision,state,not_before,expires_at,revoked_at,reason_code FROM access.access_grants WHERE grant_id=$1")
        .bind(grant_id.as_uuid()).fetch_optional(&mut **tx).await.map_err(|_| ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))?
        .ok_or_else(|| ApiError::not_found("LW_ACCESS_GRANT_NOT_FOUND"))?;
    let actor_id = typed_id::<ActorId>(row.get("actor_id"))?;
    if actor.is_some_and(|expected| expected != actor_id) {
        return Err(ApiError::forbidden("LW_ACCESS_DENIED"));
    }
    let endpoints = endpoint_grants(tx, grant_id).await?;
    let value = AccessGrant {
        id: grant_id,
        actor_id,
        course_id: typed_id(row.get("course_id"))?,
        environment_id: typed_id(row.get("environment_id"))?,
        environment_revision: revision(row.get("environment_revision"))?,
        state: parse_grant_state(&row.get::<String, _>("state"))?,
        revision: revision(row.get("revision"))?,
        endpoint_grants: endpoints,
        issued_at: utc_timestamp(row.get("not_before"))?,
        expires_at: utc_timestamp(row.get("expires_at"))?,
        revoked_at: row
            .get::<Option<OffsetDateTime>, _>("revoked_at")
            .map(utc_timestamp)
            .transpose()?,
        reason_code: row.get("reason_code"),
    };
    value
        .validate()
        .map_err(|_| ApiError::internal("LW_ACCESS_STORE_CORRUPT"))?;
    Ok(value)
}

async fn load_grant_tx_runtime(
    tx: &mut Transaction<'_, Postgres>,
    grant_id: AccessGrantId,
) -> Result<AccessGrant, GrantRuntimeError> {
    load_grant_tx(tx, grant_id, None)
        .await
        .map_err(|_| GrantRuntimeError::Contract)
}

async fn endpoint_grants(
    tx: &mut Transaction<'_, Postgres>,
    grant_id: AccessGrantId,
) -> Result<Vec<EndpointGrant>, ApiError> {
    let rows = sqlx::query("SELECT endpoint_grant_id,endpoint_id,endpoint_revision,protocol,health,alias,expires_at,contract FROM access.endpoint_grants WHERE grant_id=$1 ORDER BY endpoint_grant_id")
        .bind(grant_id.as_uuid()).fetch_all(&mut **tx).await.map_err(|_| ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))?;
    rows.into_iter()
        .map(|row| {
            let id = typed_id(row.get("endpoint_grant_id"))?;
            let protocol = parse_protocol(&row.get::<String, _>("protocol"))?;
            let contract: Value = row.get("contract");
            let ssh_gateway_hostname = optional_contract_string(&contract, "sshGatewayHostname")?;
            let ssh_gateway_port = optional_contract_u16(&contract, "sshGatewayPort")?;
            let ssh_gateway_host_key_fingerprint =
                optional_contract_string(&contract, "sshGatewayHostKeyFingerprint")?;
            Ok(EndpointGrant {
                id,
                access_grant_id: grant_id,
                endpoint_id: typed_id(row.get("endpoint_id"))?,
                endpoint_revision: revision(row.get("endpoint_revision"))?,
                protocol,
                action: EndpointAction::Connect,
                health: parse_health(&row.get::<String, _>("health"))?,
                alias: row.get("alias"),
                connect_url: matches!(protocol, EndpointProtocol::Http | EndpointProtocol::Https)
                    .then(|| format!("/connect/{id}/")),
                ssh_gateway_hostname,
                ssh_gateway_port,
                ssh_gateway_host_key_fingerprint,
                expires_at: utc_timestamp(row.get("expires_at"))?,
            })
        })
        .collect()
}

fn ssh_key_from_row(
    row: &sqlx::postgres::PgRow,
    actor_id: ActorId,
) -> Result<SshPublicKey, ApiError> {
    Ok(SshPublicKey {
        id: typed_id(row.get("key_id"))?,
        actor_id,
        fingerprint_sha256: row.get("fingerprint_sha256"),
        algorithm: match row.get::<String, _>("algorithm").as_str() {
            "ed25519" => SshKeyAlgorithm::Ed25519,
            "security_key_ed25519" => SshKeyAlgorithm::SecurityKeyEd25519,
            "rsa_sha2" => SshKeyAlgorithm::RsaSha2,
            _ => return Err(ApiError::internal("LW_ACCESS_STORE_CORRUPT")),
        },
        rsa_bits: row
            .get::<Option<i64>, _>("rsa_bits")
            .map(|value| {
                u32::try_from(value).map_err(|_| ApiError::internal("LW_ACCESS_STORE_CORRUPT"))
            })
            .transpose()?,
        revision: revision(row.get("revision"))?,
        created_at: utc_timestamp(row.get("created_at"))?,
    })
}

async fn load_session(pool: &PgPool, id: GatewaySessionId) -> Result<GatewaySession, ApiError> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|_| ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))?;
    let session = load_session_tx(&mut tx, id).await?;
    tx.commit()
        .await
        .map_err(|_| ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))?;
    Ok(session)
}

async fn load_session_tx(
    tx: &mut Transaction<'_, Postgres>,
    id: GatewaySessionId,
) -> Result<GatewaySession, ApiError> {
    let row = sqlx::query("SELECT grant_id,grant_revision,endpoint_grant_id,key_id,contract,gateway_identity,connection_id,revision,state,started_at,last_heartbeat_at,termination_requested_at,terminate_by,terminated_at,close_reason_code FROM access.gateway_sessions WHERE session_id=$1")
        .bind(id.as_uuid()).fetch_optional(&mut **tx).await.map_err(|_| ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))?
        .ok_or_else(|| ApiError::not_found("LW_ACCESS_SESSION_NOT_FOUND"))?;
    let contract = row.get::<Value, _>("contract");
    let target_alias = contract
        .get("alias")
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::internal("LW_ACCESS_STORE_CORRUPT"))?
        .to_owned();
    let target_ssh_host_key_identity_sha256 = contract
        .get("sshHostKeyIdentitySha256")
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::internal("LW_ACCESS_STORE_CORRUPT"))?
        .parse::<Sha256Digest>()
        .map_err(|_| ApiError::internal("LW_ACCESS_STORE_CORRUPT"))?;
    let session = GatewaySession {
        id,
        access_grant_id: typed_id(row.get("grant_id"))?,
        access_grant_revision: revision(row.get("grant_revision"))?,
        endpoint_grant_id: typed_id(row.get("endpoint_grant_id"))?,
        ssh_public_key_id: typed_id(row.get("key_id"))?,
        target_alias,
        target_ssh_host_key_identity_sha256,
        gateway_identity: row.get("gateway_identity"),
        connection_id: row.get("connection_id"),
        revision: revision(row.get("revision"))?,
        state: parse_session_state(&row.get::<String, _>("state"))?,
        opened_at: utc_timestamp(row.get("started_at"))?,
        last_heartbeat_at: utc_timestamp(row.get("last_heartbeat_at"))?,
        termination_requested_at: row
            .get::<Option<OffsetDateTime>, _>("termination_requested_at")
            .map(utc_timestamp)
            .transpose()?,
        terminate_by: row
            .get::<Option<OffsetDateTime>, _>("terminate_by")
            .map(utc_timestamp)
            .transpose()?,
        closed_at: row
            .get::<Option<OffsetDateTime>, _>("terminated_at")
            .map(utc_timestamp)
            .transpose()?,
        close_reason_code: row.get("close_reason_code"),
    };
    session
        .validate()
        .map_err(|_| ApiError::internal("LW_ACCESS_STORE_CORRUPT"))?;
    Ok(session)
}

async fn terminate_sessions_for_key(
    tx: &mut Transaction<'_, Postgres>,
    key_id: SshPublicKeyId,
    now: OffsetDateTime,
    terminate_by: OffsetDateTime,
) -> Result<(), ApiError> {
    let rows = sqlx::query("UPDATE access.gateway_sessions SET state='terminating',termination_requested_at=$2,terminate_by=$3,revision=revision+1 WHERE key_id=$1 AND state='active' RETURNING session_id")
        .bind(key_id.as_uuid()).bind(now).bind(terminate_by).fetch_all(&mut **tx).await.map_err(|_| ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))?;
    for row in rows {
        let id = typed_id(row.get("session_id"))?;
        let session = load_session_tx(tx, id).await?;
        enqueue_session_event(
            tx,
            &session,
            subjects::ACCESS_SESSION_TERMINATION_REQUESTED,
            now,
            "LW_ACCESS_SSH_KEY_REVOKED",
        )
        .await?;
    }
    Ok(())
}

async fn terminate_sessions_for_grant(
    tx: &mut Transaction<'_, Postgres>,
    grant_id: AccessGrantId,
    now: OffsetDateTime,
    terminate_by: OffsetDateTime,
) -> Result<(), ApiError> {
    let rows = sqlx::query("UPDATE access.gateway_sessions SET state='terminating',termination_requested_at=$2,terminate_by=$3,revision=revision+1 WHERE grant_id=$1 AND state='active' RETURNING session_id")
        .bind(grant_id.as_uuid()).bind(now).bind(terminate_by).fetch_all(&mut **tx).await.map_err(|_| ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))?;
    for row in rows {
        let id = typed_id(row.get("session_id"))?;
        let session = load_session_tx(tx, id).await?;
        enqueue_session_event(
            tx,
            &session,
            subjects::ACCESS_SESSION_TERMINATION_REQUESTED,
            now,
            "LW_ACCESS_GRANT_REVOKED",
        )
        .await?;
    }
    Ok(())
}

async fn expire_grants(pool: &PgPool) -> Result<(), GrantRuntimeError> {
    loop {
        let now = OffsetDateTime::now_utc();
        let terminate_by = now + time::Duration::seconds(TERMINATION_SECONDS);
        let mut tx = pool.begin().await?;
        let row=sqlx::query("SELECT grant_id FROM access.access_grants WHERE state='active' AND expires_at<= $1 ORDER BY expires_at,grant_id FOR UPDATE SKIP LOCKED LIMIT 1")
          .bind(now).fetch_optional(&mut *tx).await?;
        let Some(row) = row else {
            tx.rollback().await?;
            break;
        };
        let grant_id = typed_id::<AccessGrantId>(row.get("grant_id"))
            .map_err(|_| GrantRuntimeError::Contract)?;
        sqlx::query("UPDATE access.access_grants SET state='expired',revision=revision+1,reason_code='expired',updated_at=$2 WHERE grant_id=$1")
          .bind(grant_id.as_uuid()).bind(now).execute(&mut *tx).await?;
        terminate_sessions_for_grant(&mut tx, grant_id, now, terminate_by)
            .await
            .map_err(|_| GrantRuntimeError::Contract)?;
        let grant = load_grant_tx_runtime(&mut tx, grant_id).await?;
        enqueue_grant_event_runtime(&mut tx, &grant, subjects::ACCESS_GRANT_EXPIRED, now).await?;
        tx.commit().await?;
    }
    Ok(())
}

async fn mark_overdue(pool: &PgPool) -> Result<(), GrantRuntimeError> {
    let now = OffsetDateTime::now_utc();
    let mut tx = pool.begin().await?;
    let rows=sqlx::query("UPDATE access.gateway_sessions SET state='termination_overdue',close_reason_code='termination_overdue',revision=revision+1 WHERE state='terminating' AND terminate_by<$1 RETURNING session_id")
      .bind(now).fetch_all(&mut *tx).await?;
    let count = rows.len();
    for row in &rows {
        let id = typed_id::<GatewaySessionId>(row.get("session_id"))
            .map_err(|_| GrantRuntimeError::Contract)?;
        let session = load_session_tx(&mut tx, id)
            .await
            .map_err(|_| GrantRuntimeError::Contract)?;
        enqueue_session_event_runtime(
            &mut tx,
            &session,
            subjects::ACCESS_SESSION_TERMINATION_OVERDUE,
            now,
            "termination_overdue",
        )
        .await?;
    }
    tx.commit().await?;
    if count > 0 {
        metrics::counter!("labweaver_access_session_termination_overdue")
            .increment(u64::try_from(count).unwrap_or(u64::MAX));
    }
    Ok(())
}

async fn cleanup_authorizations(pool: &PgPool) -> Result<(), GrantRuntimeError> {
    sqlx::query("DELETE FROM access.ssh_authorizations WHERE expires_at<now()-interval '1 hour'")
        .execute(pool)
        .await?;
    Ok(())
}

async fn enqueue_grant_event(
    tx: &mut Transaction<'_, Postgres>,
    grant: &AccessGrant,
    subject: &str,
    now: OffsetDateTime,
) -> Result<(), ApiError> {
    let stream: i64 = sqlx::query_scalar("SELECT nextval('access.access_stream_sequence')")
        .fetch_one(&mut **tx)
        .await
        .map_err(|_| ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))?;
    sqlx::query("UPDATE access.access_grants SET last_stream_sequence=$2 WHERE grant_id=$1")
        .bind(grant.id.as_uuid())
        .bind(stream)
        .execute(&mut **tx)
        .await
        .map_err(|_| ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))?;
    enqueue_event_value(
        tx,
        subject,
        grant.course_id,
        grant.id.as_uuid(),
        grant.revision,
        serde_json::to_value(AccessGrantChanged {
            access_grant_id: grant.id,
            revision: grant.revision,
            state: grant_state_str(grant.state).to_owned(),
            effective_at: utc_timestamp(now)?,
        })
        .map_err(|_| ApiError::internal("LW_ACCESS_EVENT_INVALID"))?,
    )
    .await
}
async fn enqueue_grant_event_runtime(
    tx: &mut Transaction<'_, Postgres>,
    grant: &AccessGrant,
    subject: &str,
    now: OffsetDateTime,
) -> Result<(), GrantRuntimeError> {
    enqueue_grant_event(tx, grant, subject, now)
        .await
        .map_err(|_| GrantRuntimeError::Contract)
}
async fn enqueue_key_event(
    tx: &mut Transaction<'_, Postgres>,
    key_id: SshPublicKeyId,
    actor_id: ActorId,
    now: OffsetDateTime,
) -> Result<(), ApiError> {
    let courses=sqlx::query("SELECT DISTINCT g.course_id FROM access.gateway_sessions s JOIN access.access_grants g ON g.grant_id=s.grant_id WHERE s.key_id=$1").bind(key_id.as_uuid()).fetch_all(&mut **tx).await.map_err(|_|ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))?;
    for row in courses {
        let course_id = typed_id(row.get("course_id"))?;
        enqueue_event_value(
            tx,
            subjects::ACCESS_SSH_KEY_REVOKED,
            course_id,
            Uuid::now_v7(),
            Revision::new(1).map_err(|_| ApiError::internal("LW_ACCESS_EVENT_INVALID"))?,
            serde_json::to_value(SshPublicKeyRevoked {
                ssh_public_key_id: key_id,
                actor_id,
                effective_at: utc_timestamp(now)?,
            })
            .map_err(|_| ApiError::internal("LW_ACCESS_EVENT_INVALID"))?,
        )
        .await?;
    }
    Ok(())
}
async fn enqueue_session_event(
    tx: &mut Transaction<'_, Postgres>,
    session: &GatewaySession,
    subject: &str,
    now: OffsetDateTime,
    reason: &str,
) -> Result<(), ApiError> {
    let course = sqlx::query_scalar::<_, Uuid>(
        "SELECT course_id FROM access.access_grants WHERE grant_id=$1",
    )
    .bind(session.access_grant_id.as_uuid())
    .fetch_one(&mut **tx)
    .await
    .map_err(|_| ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))?;
    enqueue_event_value(
        tx,
        subject,
        typed_id(course)?,
        session.id.as_uuid(),
        session.revision,
        serde_json::to_value(GatewaySessionChanged {
            gateway_session_id: session.id,
            access_grant_id: session.access_grant_id,
            access_grant_revision: session.access_grant_revision,
            state: session_state_str(session.state).to_owned(),
            effective_at: utc_timestamp(now)?,
            terminate_by: session.terminate_by,
            reason_code: reason.to_owned(),
        })
        .map_err(|_| ApiError::internal("LW_ACCESS_EVENT_INVALID"))?,
    )
    .await
}
async fn enqueue_session_event_runtime(
    tx: &mut Transaction<'_, Postgres>,
    session: &GatewaySession,
    subject: &str,
    now: OffsetDateTime,
    reason: &str,
) -> Result<(), GrantRuntimeError> {
    enqueue_session_event(tx, session, subject, now, reason)
        .await
        .map_err(|_| GrantRuntimeError::Contract)
}

async fn enqueue_event_value(
    tx: &mut Transaction<'_, Postgres>,
    subject: &str,
    course_id: CourseId,
    aggregate: Uuid,
    revision: Revision,
    data: Value,
) -> Result<(), ApiError> {
    let contract = event_contract(subject)?;
    let event_id = EventId::new();
    let now = utc_timestamp(OffsetDateTime::now_utc())?;
    let event = CloudEvent {
        specversion: contracts::events::SPEC_VERSION.to_owned(),
        id: event_id,
        source: contract.source().to_owned(),
        event_type: subject.to_owned(),
        subject: subject.to_owned(),
        time: now,
        datacontenttype: "application/json".to_owned(),
        dataschema: contract.data_schema(),
        course_id,
        aggregate_revision: revision,
        aggregate_sequence: Sequence(revision.get()),
        trace_id: event_id.to_string(),
        data,
    };
    event
        .validate(contract)
        .map_err(|_| ApiError::internal("LW_ACCESS_EVENT_INVALID"))?;
    let payload =
        serde_json::to_value(&event).map_err(|_| ApiError::internal("LW_ACCESS_EVENT_INVALID"))?;
    let bytes =
        serde_json::to_vec(&payload).map_err(|_| ApiError::internal("LW_ACCESS_EVENT_INVALID"))?;
    sqlx::query("INSERT INTO access.outbox_events (event_id,subject,event_type,aggregate_id,aggregate_sequence,payload,payload_sha256) VALUES ($1,$2,$2,$3,$4,$5,$6)")
      .bind(event_id.as_uuid()).bind(subject).bind(aggregate).bind(i64::try_from(revision.get()).map_err(|_|ApiError::internal("LW_ACCESS_EVENT_INVALID"))?).bind(payload).bind(Sha256Digest::of_bytes(&bytes).to_string())
      .execute(&mut **tx).await.map_err(|_|ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))?;
    Ok(())
}

fn event_contract(subject: &str) -> Result<EventContract, ApiError> {
    contracts::events::EVENT_CONTRACTS
        .iter()
        .copied()
        .find(|c| c.subject == subject)
        .ok_or_else(|| ApiError::internal("LW_ACCESS_EVENT_INVALID"))
}
fn ensure_gateway_request(
    state: &AppState,
    principal: &MtlsPrincipal,
    claimed: &str,
) -> Result<(), ApiError> {
    if state
        .deployment
        .grants
        .gateway_san_uris
        .contains(&principal.san_uri)
        && claimed == principal.san_uri
    {
        Ok(())
    } else {
        Err(ApiError::forbidden("LW_ACCESS_GATEWAY_IDENTITY_DENIED"))
    }
}
fn validate_alias(alias: &str) -> Result<(), ApiError> {
    if alias.len() == 23
        && alias.starts_with("lw-")
        && alias[3..]
            .bytes()
            .all(|b| b.is_ascii_lowercase() || (b'2'..=b'7').contains(&b))
    {
        Ok(())
    } else {
        Err(ApiError::forbidden("LW_ACCESS_ALIAS_INVALID"))
    }
}
fn valid_fingerprint(value: &str) -> bool {
    value.starts_with("SHA256:")
        && value.len() <= 96
        && value[7..]
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'='))
}
fn valid_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}
fn random_token() -> String {
    let mut bytes = [0_u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    hex(&bytes)
}
fn sha256_hex(value: &[u8]) -> String {
    hex(&Sha256::digest(value))
}
fn hex(value: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(value.len() * 2);
    for byte in value {
        output.push(char::from(DIGITS[usize::from(byte >> 4)]));
        output.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    output
}
fn ssh_alias(id: EndpointGrantId) -> String {
    let alphabet = b"abcdefghijklmnopqrstuvwxyz234567";
    let uuid = id.as_uuid();
    let bytes = uuid.as_bytes();
    let mut out = String::from("lw-");
    let mut acc = 0_u32;
    let mut bits = 0_u8;
    for byte in bytes {
        acc = (acc << 8) | u32::from(*byte);
        bits += 8;
        while bits >= 5 && out.len() < 23 {
            bits -= 5;
            out.push(alphabet[((acc >> bits) & 31) as usize] as char);
        }
    }
    while out.len() < 23 {
        out.push('a');
    }
    out
}
fn protocol_str(p: EndpointProtocol) -> &'static str {
    match p {
        EndpointProtocol::Http => "http",
        EndpointProtocol::Https => "https",
        EndpointProtocol::Ssh => "ssh",
    }
}
fn parse_protocol(v: &str) -> Result<EndpointProtocol, ApiError> {
    match v {
        "http" => Ok(EndpointProtocol::Http),
        "https" => Ok(EndpointProtocol::Https),
        "ssh" => Ok(EndpointProtocol::Ssh),
        _ => Err(ApiError::internal("LW_ACCESS_STORE_CORRUPT")),
    }
}
fn parse_health(v: &str) -> Result<EndpointHealth, ApiError> {
    match v {
        "healthy" => Ok(EndpointHealth::Healthy),
        "unhealthy" => Ok(EndpointHealth::Unhealthy),
        "removed" => Ok(EndpointHealth::Removed),
        _ => Err(ApiError::internal("LW_ACCESS_STORE_CORRUPT")),
    }
}

fn optional_contract_string(contract: &Value, field: &str) -> Result<Option<String>, ApiError> {
    match contract.get(field) {
        Some(Value::String(value)) if !value.is_empty() => Ok(Some(value.clone())),
        Some(Value::Null) | None => Ok(None),
        _ => Err(ApiError::internal("LW_ACCESS_STORE_CORRUPT")),
    }
}

fn optional_contract_u16(contract: &Value, field: &str) -> Result<Option<u16>, ApiError> {
    match contract.get(field) {
        Some(Value::Number(value)) => value
            .as_u64()
            .and_then(|value| u16::try_from(value).ok())
            .map(Some)
            .ok_or_else(|| ApiError::internal("LW_ACCESS_STORE_CORRUPT")),
        Some(Value::Null) | None => Ok(None),
        _ => Err(ApiError::internal("LW_ACCESS_STORE_CORRUPT")),
    }
}
fn grant_state_str(s: AccessGrantState) -> &'static str {
    match s {
        AccessGrantState::Requested => "requested",
        AccessGrantState::Active => "active",
        AccessGrantState::Denied => "denied",
        AccessGrantState::Expired => "expired",
        AccessGrantState::Revoked => "revoked",
    }
}
fn parse_grant_state(v: &str) -> Result<AccessGrantState, ApiError> {
    match v {
        "requested" => Ok(AccessGrantState::Requested),
        "active" => Ok(AccessGrantState::Active),
        "denied" => Ok(AccessGrantState::Denied),
        "expired" => Ok(AccessGrantState::Expired),
        "revoked" => Ok(AccessGrantState::Revoked),
        _ => Err(ApiError::internal("LW_ACCESS_STORE_CORRUPT")),
    }
}
fn session_state_str(s: GatewaySessionState) -> &'static str {
    match s {
        GatewaySessionState::Active => "active",
        GatewaySessionState::Terminating => "terminating",
        GatewaySessionState::TerminationOverdue => "termination_overdue",
        GatewaySessionState::Closed => "closed",
    }
}
fn parse_session_state(v: &str) -> Result<GatewaySessionState, ApiError> {
    match v {
        "active" => Ok(GatewaySessionState::Active),
        "terminating" => Ok(GatewaySessionState::Terminating),
        "termination_overdue" => Ok(GatewaySessionState::TerminationOverdue),
        "closed" => Ok(GatewaySessionState::Closed),
        _ => Err(ApiError::internal("LW_ACCESS_STORE_CORRUPT")),
    }
}
fn revision(v: i64) -> Result<Revision, ApiError> {
    Revision::new(u64::try_from(v).map_err(|_| ApiError::internal("LW_ACCESS_STORE_CORRUPT"))?)
        .map_err(|_| ApiError::internal("LW_ACCESS_STORE_CORRUPT"))
}
fn typed_id<T: FromStr>(v: Uuid) -> Result<T, ApiError> {
    v.to_string()
        .parse()
        .map_err(|_| ApiError::internal("LW_ACCESS_STORE_CORRUPT"))
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn environment_revocation_request_is_strict_and_reason_bounded() {
        let valid: EnvironmentAccessRevocationRequest = contracts::parse_strict_json(
            br#"{"version":1,"environmentId":"00000000-0000-7000-8000-000000000101","environmentRevision":4,"reason":"environment_deleted"}"#,
        )
        .expect("valid revocation request");
        assert!(valid_environment_revocation_request(&valid));

        let unsupported: EnvironmentAccessRevocationRequest = contracts::parse_strict_json(
            br#"{"version":1,"environmentId":"00000000-0000-7000-8000-000000000101","environmentRevision":4,"reason":"arbitrary"}"#,
        )
        .expect("structurally valid request");
        assert!(!valid_environment_revocation_request(&unsupported));

        assert!(
            contracts::parse_strict_json::<EnvironmentAccessRevocationRequest>(
                br#"{"version":1,"environmentId":"00000000-0000-7000-8000-000000000101","environmentRevision":4,"reason":"environment_deleted","extra":true}"#,
            )
            .is_err()
        );
    }
}

#[derive(Debug, thiserror::Error)]
pub enum GrantRuntimeError {
    #[error("LW_ACCESS_CONFIG_INVALID")]
    Config,
    #[error("LW_ACCESS_NATS_CREDENTIALS_INVALID")]
    NatsCredentials,
    #[error("LW_ACCESS_NATS_UNAVAILABLE")]
    NatsConnect,
    #[error("LW_ACCESS_NATS_PUBLISH_FAILED")]
    NatsPublish,
    #[error("LW_ACCESS_NATS_SUBSCRIBE_FAILED")]
    NatsSubscribe,
    #[error("LW_ACCESS_CONTRACT_INVALID")]
    Contract,
    #[error("LW_ACCESS_ACTIVATION_LEASE_LOST")]
    ActivationLeaseLost,
    #[error("LW_ACCESS_REVOCATION_FAILED")]
    AccessMutation,
    #[error("LW_ACCESS_STORE_UNAVAILABLE")]
    Database(#[from] sqlx::Error),
}
