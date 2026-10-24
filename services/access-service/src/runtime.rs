//! Host-isolated runtime admission. Opaque browser secrets are persisted only as hashes.
use std::sync::Arc;

use axum::{
    body::Bytes,
    extract::{Path, Query, Request, State},
    http::{HeaderMap, Method, Uri, header},
    middleware::Next,
    response::Response,
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use contracts::{EndpointGrantId, object_authorization::ObjectAction};
use persistence_sqlx::Sha256Digest;
use rand::RngCore;
use serde::Deserialize;
use sqlx::Row;
use time::{Duration, OffsetDateTime};

use super::{ApiError, AppState, authenticated_session, proxy};

const BINDING_COOKIE: &str = "__Host-labweaver_runtime_binding";
const CAPABILITY_COOKIE: &str = "__Secure-labweaver_runtime";
const HANDOFF_SECONDS: i64 = 30;
const CAPABILITY_SECONDS: i64 = 300;

fn runtime_host(state: &AppState, uri: &Uri, headers: &HeaderMap) -> bool {
    // Configuration validation requires an exact canonical origin with no path.
    let Some((_, authority)) = state.deployment.browser.runtime_origin.split_once("://") else {
        return false;
    };
    // HTTP/2 carries :authority in the URI rather than a Host header.
    let host = headers.get(header::HOST).and_then(|h| h.to_str().ok());
    uri.authority()
        .map(axum::http::uri::Authority::as_str)
        .or(host)
        == Some(authority)
        && host.is_none_or(|host| host == authority)
}

/// Enforce the origin boundary even when an ingress is bypassed or misconfigured.
pub(super) async fn isolate_origin(
    State(state): State<Arc<AppState>>,
    request: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let runtime = runtime_host(&state, request.uri(), request.headers());
    let path = request.uri().path();
    let admission =
        path.starts_with("/runtime-bootstrap/") || path.starts_with("/runtime-handoff/");
    if (runtime
        && !(admission
            || (path.starts_with("/connect/") && !path.starts_with("/connect/console/"))))
        || (!runtime && admission)
        || (!runtime
            && (path.starts_with("/api/") || path.starts_with("/auth/"))
            && request
                .headers()
                .get(header::ORIGIN)
                .and_then(|v| v.to_str().ok())
                == Some(state.deployment.browser.runtime_origin.as_str()))
    {
        return Err(ApiError::forbidden("LW_ACCESS_RUNTIME_ORIGIN_REJECTED"));
    }
    if runtime
        && !matches!(
            *request.method(),
            Method::GET | Method::HEAD | Method::OPTIONS
        )
        && request
            .headers()
            .get(header::ORIGIN)
            .and_then(|v| v.to_str().ok())
            != Some(state.deployment.browser.runtime_origin.as_str())
    {
        return Err(ApiError::forbidden("LW_ACCESS_RUNTIME_ORIGIN_REJECTED"));
    }
    Ok(next.run(request).await)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct HandoffQuery {
    binding: Option<String>,
}

/// Portal GETs never forward untrusted content. They only start a browser-bound handoff.
pub(super) async fn connect(
    State(state): State<Arc<AppState>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    if runtime_host(&state, &uri, &headers) {
        return proxy::forward_runtime(State(state), method, uri, headers, body).await;
    }
    if method != Method::GET {
        return Err(ApiError::forbidden("LW_ACCESS_RUNTIME_ORIGIN_REJECTED"));
    }
    let (grant, path) = proxy::parse_runtime_path(uri.path())?;
    if path != "/" {
        return Err(ApiError::forbidden("LW_ACCESS_RUNTIME_ORIGIN_REJECTED"));
    }
    let session = authenticated_session(&state, &headers).await?;
    proxy::authorize_runtime(&state, session.actor_id, grant, ObjectAction::Browse).await?;
    let Query(query) = Query::<HandoffQuery>::try_from_uri(&uri).map_err(|_| denied())?;
    let Some(binding) = query.binding else {
        return redirect(
            &format!(
                "{}/runtime-bootstrap/{grant}",
                state.deployment.browser.runtime_origin
            ),
            None,
        );
    };
    if !valid_secret(&binding) {
        return Err(denied());
    }
    let token = secret();
    let now = OffsetDateTime::now_utc();
    sqlx::query(
        "INSERT INTO access.runtime_capabilities \
         (handoff_sha256,binding_sha256,endpoint_grant_id,actor_id,bff_session_id,handoff_expires_at,expires_at) \
         VALUES ($1,$2,$3,$4,$5,$6,$7)",
    )
    .bind(hash(&token)).bind(hash(&binding)).bind(grant.as_uuid())
    .bind(session.actor_id).bind(session.session_id)
    .bind(now + Duration::seconds(HANDOFF_SECONDS))
    .bind(std::cmp::min(now + Duration::seconds(CAPABILITY_SECONDS), session.expires_at))
    .execute(&state.pool).await.map_err(|_| store_error())?;
    redirect(
        &format!(
            "{}/runtime-handoff/{grant}/{token}",
            state.deployment.browser.runtime_origin
        ),
        None,
    )
}

/// The nonce cookie never leaves the runtime host. A stolen handoff URL is insufficient.
pub(super) async fn bootstrap(
    State(state): State<Arc<AppState>>,
    Path(grant): Path<EndpointGrantId>,
) -> Result<Response, ApiError> {
    let portal = state.config.allowed_origins.first().ok_or_else(denied)?;
    let binding = secret();
    redirect(
        &format!("{portal}/connect/{grant}/?binding={binding}"),
        Some(format!(
            "{BINDING_COOKIE}={binding}; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age={HANDOFF_SECONDS}"
        )),
    )
}

pub(super) async fn consume(
    State(state): State<Arc<AppState>>,
    Path((grant, token)): Path<(EndpointGrantId, String)>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let binding = cookie(&headers, BINDING_COOKIE)
        .filter(|v| valid_secret(v))
        .ok_or_else(denied)?;
    if !valid_secret(&token) {
        return Err(denied());
    }
    // Atomically claim the handoff; concurrent/replayed requests cannot create a second capability.
    let capability = secret();
    let row = sqlx::query(
        "UPDATE access.runtime_capabilities c SET consumed_at=now(),capability_sha256=$4 \
         FROM access.bff_sessions s WHERE c.handoff_sha256=$1 AND c.binding_sha256=$2 \
         AND c.endpoint_grant_id=$3 AND c.consumed_at IS NULL AND c.handoff_expires_at>now() \
         AND c.expires_at>now() AND s.session_id=c.bff_session_id AND s.actor_id=c.actor_id \
         AND s.revoked_at IS NULL AND s.expires_at>now() AND s.idle_expires_at>now() \
         RETURNING c.actor_id",
    )
    .bind(hash(&token))
    .bind(hash(binding))
    .bind(grant.as_uuid())
    .bind(hash(&capability))
    .fetch_optional(&state.pool)
    .await
    .map_err(|_| store_error())?
    .ok_or_else(denied)?;
    proxy::authorize_runtime(&state, row.get("actor_id"), grant, ObjectAction::Browse).await?;
    redirect(
        &format!("/connect/{grant}/"),
        Some(format!(
            "{CAPABILITY_COOKIE}={capability}; Path=/connect/{grant}/; HttpOnly; Secure; SameSite=Lax; Max-Age={CAPABILITY_SECONDS}"
        )),
    )
}

pub(super) async fn authenticated_capability(
    state: &AppState,
    uri: &Uri,
    headers: &HeaderMap,
    grant: EndpointGrantId,
) -> Result<auth::BffSession, ApiError> {
    if !runtime_host(state, uri, headers) {
        return Err(denied());
    }
    let capability = cookie(headers, CAPABILITY_COOKIE)
        .filter(|v| valid_secret(v))
        .ok_or_else(denied)?;
    let row = sqlx::query(
        "SELECT c.bff_session_id,c.actor_id FROM access.runtime_capabilities c \
         WHERE capability_sha256=$1 AND endpoint_grant_id=$2 AND consumed_at IS NOT NULL AND expires_at>now()",
    ).bind(hash(capability)).bind(grant.as_uuid()).fetch_optional(&state.pool)
        .await.map_err(|_| store_error())?.ok_or_else(denied)?;
    let session = auth::load_bff_session(
        &state.pool,
        &state.key_ring,
        row.get("bff_session_id"),
        super::deployment_duration(state.deployment.browser.session_idle_ttl_seconds)?,
        OffsetDateTime::now_utc(),
    )
    .await
    .map_err(ApiError::from)?;
    if session.actor_id != row.get::<uuid::Uuid, _>("actor_id") {
        return Err(denied());
    }
    Ok(session)
}

fn cookie<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    let mut values = headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|h| h.to_str().ok())
        .flat_map(|h| h.split(';'))
        .filter_map(|v| v.trim().split_once('='))
        .filter(|(key, _)| *key == name)
        .map(|(_, value)| value);
    let value = values.next()?;
    if values.next().is_some() {
        None
    } else {
        Some(value)
    }
}

fn secret() -> String {
    let mut bytes = [0_u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

fn valid_secret(value: &str) -> bool {
    value.len() == 43
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

fn hash(value: &str) -> String {
    Sha256Digest::of_bytes(value.as_bytes()).to_string()
}

fn denied() -> ApiError {
    ApiError::forbidden("LW_ACCESS_RUNTIME_CAPABILITY_REJECTED")
}
fn store_error() -> ApiError {
    ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE")
}

fn redirect(location: &str, cookie: Option<String>) -> Result<Response, ApiError> {
    let mut response = Response::builder()
        .status(303)
        .header(header::LOCATION, location)
        .header(header::CACHE_CONTROL, "no-store")
        .header("referrer-policy", "no-referrer")
        .header(
            "content-security-policy",
            "default-src 'none'; frame-ancestors 'none'",
        )
        .header("cross-origin-opener-policy", "same-origin");
    if let Some(cookie) = cookie {
        response = response.header(header::SET_COOKIE, cookie);
    }
    response
        .body(axum::body::Body::empty())
        .map_err(|_| store_error())
}
