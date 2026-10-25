//! Host-isolated runtime admission. Opaque browser secrets are persisted only as hashes.
use std::{
    borrow::Cow,
    collections::BTreeSet,
    net::IpAddr,
    sync::{Arc, LazyLock},
};

use axum::{
    body::Bytes,
    extract::{Form, Path, Query, Request, State},
    http::{HeaderMap, HeaderValue, Method, Uri, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use contracts::{EndpointGrantId, object_authorization::ObjectAction};
use persistence_sqlx::Sha256Digest;
use rand::RngCore;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use sqlx::Row;
use time::OffsetDateTime;

use super::{ApiError, AppState, authenticated_session, proxy};

const BINDING_COOKIE: &str = "__Host-labweaver_runtime_binding";
const CAPABILITY_COOKIE: &str = "__Secure-labweaver_runtime";
const HANDOFF_SECONDS: i64 = 30;
const CAPABILITY_SECONDS: i64 = 300;

#[derive(Debug, Eq, PartialEq)]
enum Host<'a> {
    Dns(Cow<'a, str>),
    Ip(IpAddr),
}

#[derive(Debug, Eq, PartialEq)]
struct Authority<'a> {
    host: Host<'a>,
    port: u16,
}

impl<'a> Authority<'a> {
    fn parse(raw: &'a str, default_port: u16) -> Result<Self, ApiError> {
        let (host, port) = if let Some(value) = raw.strip_prefix('[') {
            let (host, suffix) = value.split_once(']').ok_or_else(origin_denied)?;
            let port = if suffix.is_empty() {
                None
            } else {
                Some(suffix.strip_prefix(':').ok_or_else(origin_denied)?)
            };
            (host, port)
        } else {
            raw.rsplit_once(':')
                .map_or((raw, None), |(host, port)| (host, Some(port)))
        };
        let port = port.map_or(Ok(default_port), |port| {
            port.parse::<u16>().map_err(|_| origin_denied())
        })?;
        if port == 0 {
            return Err(origin_denied());
        }
        let host = host.strip_suffix('.').unwrap_or(host);
        let host = if let Ok(ip) = host.parse::<IpAddr>() {
            Host::Ip(ip)
        } else {
            if host.len() > 253
                || host.split('.').any(|label| {
                    label.is_empty()
                        || label.len() > 63
                        || !label
                            .as_bytes()
                            .first()
                            .is_some_and(u8::is_ascii_alphanumeric)
                        || !label
                            .as_bytes()
                            .last()
                            .is_some_and(u8::is_ascii_alphanumeric)
                        || !label
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                })
            {
                return Err(origin_denied());
            }
            Host::Dns(if host.bytes().any(|byte| byte.is_ascii_uppercase()) {
                Cow::Owned(host.to_ascii_lowercase())
            } else {
                Cow::Borrowed(host)
            })
        };
        Ok(Self { host, port })
    }

    fn origin(origin: &'a str) -> Result<(&'a str, Self), ApiError> {
        let (scheme, authority) = origin.split_once("://").ok_or_else(origin_denied)?;
        let port = match scheme {
            "https" => 443,
            "http" => 80,
            _ => return Err(origin_denied()),
        };
        Ok((scheme, Self::parse(authority, port)?))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BrowserHost {
    Portal,
    Runtime(EndpointGrantId),
}

fn classify_authority(
    portals: &BTreeSet<String>,
    runtime_base: &str,
    authority: &Authority<'_>,
) -> Result<BrowserHost, ApiError> {
    for origin in portals {
        if Authority::origin(origin)?.1 == *authority {
            return Ok(BrowserHost::Portal);
        }
    }
    let (_, base) = Authority::origin(runtime_base)?;
    let (Host::Dns(host), Host::Dns(base_host)) = (&authority.host, &base.host) else {
        return Err(origin_denied());
    };
    let label = host
        .strip_suffix(base_host.as_ref())
        .and_then(|label| label.strip_suffix('.'))
        .and_then(|label| label.strip_prefix("g-"))
        .ok_or_else(origin_denied)?;
    if authority.port != base.port
        || label.len() != 32
        || !label.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(origin_denied());
    }
    Ok(BrowserHost::Runtime(
        label.parse().map_err(|_| origin_denied())?,
    ))
}

fn request_authority<'a>(uri: &'a Uri, headers: &'a HeaderMap) -> Result<Authority<'a>, ApiError> {
    let default_port = if uri.scheme_str() == Some("http") {
        80
    } else {
        443
    };
    let mut hosts = headers.get_all(header::HOST).iter();
    let host = hosts
        .next()
        .map(|value| {
            value
                .to_str()
                .map_err(|_| origin_denied())
                .and_then(|value| Authority::parse(value, default_port))
        })
        .transpose()?;
    if hosts.next().is_some() {
        return Err(origin_denied());
    }
    let uri_authority = uri
        .authority()
        .map(|value| Authority::parse(value.as_str(), default_port))
        .transpose()?;
    if let (Some(host), Some(authority)) = (&host, &uri_authority)
        && host != authority
    {
        return Err(origin_denied());
    }
    uri_authority.or(host).ok_or_else(origin_denied)
}

fn browser_host(state: &AppState, uri: &Uri, headers: &HeaderMap) -> Result<BrowserHost, ApiError> {
    classify_authority(
        &state.config.allowed_origins,
        &state.deployment.browser.runtime_base_origin,
        &request_authority(uri, headers)?,
    )
}

pub(super) fn runtime_origin(state: &AppState, grant: EndpointGrantId) -> Result<String, ApiError> {
    let base = state
        .deployment
        .browser
        .runtime_base_origin
        .strip_prefix("https://")
        .ok_or_else(origin_denied)?;
    Ok(format!("https://g-{}.{base}", grant.as_uuid().simple()))
}

fn path_grant(path: &str) -> Option<EndpointGrantId> {
    path.strip_prefix("/connect/")
        .or_else(|| path.strip_prefix("/runtime-bootstrap/"))
        .or_else(|| path.strip_prefix("/runtime-handoff/"))?
        .split('/')
        .next()?
        .parse()
        .ok()
}

/// Unknown, missing, or conflicting authorities never fall back to the portal.
/// Forwarded-Host is deliberately not an authentication input.
pub(super) async fn isolate_origin(
    State(state): State<Arc<AppState>>,
    request: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let host = browser_host(&state, request.uri(), request.headers())?;
    let path = request.uri().path();
    let supplied_origin = request
        .headers()
        .get(header::ORIGIN)
        .map(|value| {
            value
                .to_str()
                .map_err(|_| origin_denied())
                .and_then(Authority::origin)
        })
        .transpose()?;
    match host {
        BrowserHost::Runtime(grant) => {
            if path_grant(path) != Some(grant) {
                return Err(origin_denied());
            }
            if let Some((scheme, origin)) = &supplied_origin {
                if *scheme != "https"
                    || classify_authority(
                        &state.config.allowed_origins,
                        &state.deployment.browser.runtime_base_origin,
                        origin,
                    )? != host
                {
                    return Err(origin_denied());
                }
            } else if !matches!(
                *request.method(),
                Method::GET | Method::HEAD | Method::OPTIONS
            ) {
                return Err(origin_denied());
            }
            // A same-site sibling still is a different security principal. Do not
            // let no-cors requests/embeds bypass the Origin fence by omitting Origin.
            if request
                .headers()
                .get("sec-fetch-site")
                .is_some_and(|site| site != "same-origin")
                && !(request.method() == Method::GET
                    && request
                        .headers()
                        .get("sec-fetch-mode")
                        .is_some_and(|mode| mode == "navigate")
                    && request
                        .headers()
                        .get("sec-fetch-dest")
                        .is_some_and(|dest| dest == "document"))
            {
                return Err(origin_denied());
            }
        }
        BrowserHost::Portal => {
            if path.starts_with("/runtime-bootstrap/") || path.starts_with("/runtime-handoff/") {
                return Err(origin_denied());
            }
            if (path.starts_with("/api/") || path.starts_with("/auth/"))
                && supplied_origin.as_ref().is_some_and(|(_, origin)| {
                    matches!(
                        classify_authority(
                            &state.config.allowed_origins,
                            &state.deployment.browser.runtime_base_origin,
                            origin
                        ),
                        Ok(BrowserHost::Runtime(_))
                    )
                })
            {
                return Err(origin_denied());
            }
        }
    }
    Ok(next.run(request).await)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct HandoffQuery {
    #[serde(default)]
    handoff: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HandoffRequest {
    binding: String,
}

/// Portal GETs never forward untrusted content. They only start a browser-bound handoff.
pub(super) async fn connect(
    State(state): State<Arc<AppState>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    let host = browser_host(&state, &uri, &headers)?;
    if matches!(host, BrowserHost::Runtime(_)) {
        return proxy::forward_runtime(State(state), method, uri, headers, body).await;
    }
    let (grant, path) = proxy::parse_runtime_path(uri.path())?;
    if path != "/" || !matches!(method, Method::GET | Method::POST) {
        return Err(origin_denied());
    }
    let session = authenticated_session(&state, &headers).await?;
    proxy::authorize_runtime(&state, session.actor_id, grant, ObjectAction::Browse).await?;
    if method == Method::GET {
        let Query(query) = Query::<HandoffQuery>::try_from_uri(&uri).map_err(|_| denied())?;
        if query.handoff {
            return bridge(&session);
        }
        return redirect(
            &format!(
                "{}/runtime-bootstrap/{grant}",
                runtime_origin(&state, grant)?
            ),
            None,
        );
    }
    super::require_browser_origin(&state, &headers)?;
    let supplied = headers
        .get(state.deployment.browser.csrf_header_name.as_str())
        .and_then(|value| value.to_str().ok());
    auth::verify_csrf_token(&session.csrf_token, supplied).map_err(ApiError::from)?;
    let request: HandoffRequest = contracts::parse_strict_json(&body).map_err(|_| denied())?;
    if !valid_secret(&request.binding) {
        return Err(denied());
    }
    let token = secret();
    let inserted = sqlx::query(
        "WITH authority AS MATERIALIZED (SELECT date_trunc('milliseconds',clock_timestamp()) AS issued_at) \
         INSERT INTO access.runtime_capabilities \
         (handoff_sha256,binding_sha256,endpoint_grant_id,actor_id,bff_session_id,issued_at,handoff_expires_at,expires_at) \
         SELECT $1,$2,$3,$4,$5,n.issued_at,LEAST(n.issued_at+interval '30 seconds',s.expires_at), \
                LEAST(n.issued_at+interval '300 seconds',s.expires_at) \
         FROM authority n JOIN access.bff_sessions s ON s.session_id=$5 AND s.actor_id=$4 \
         WHERE s.revoked_at IS NULL AND s.expires_at>n.issued_at AND s.idle_expires_at>n.issued_at",
    ).bind(hash(&token)).bind(hash(&request.binding)).bind(grant.as_uuid())
        .bind(session.actor_id).bind(session.session_id).execute(&state.pool).await.map_err(|_| store_error())?;
    if inserted.rows_affected() != 1 {
        return Err(denied());
    }
    let mut response = axum::Json(serde_json::json!({"handoffUrl": format!("{}/runtime-handoff/{grant}#token={token}", runtime_origin(&state, grant)?)})).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
        .headers_mut()
        .insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    Ok(response)
}

/// The nonce cookie never leaves the runtime host. A stolen handoff URL is insufficient.
pub(super) async fn bootstrap(
    State(state): State<Arc<AppState>>,
    Path(grant): Path<EndpointGrantId>,
) -> Result<Response, ApiError> {
    let portal = state.config.allowed_origins.first().ok_or_else(denied)?;
    let binding = secret();
    redirect(
        &format!("{portal}/connect/{grant}/?handoff=true#binding={binding}"),
        Some(format!(
            "{BINDING_COOKIE}={binding}; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age={HANDOFF_SECONDS}"
        )),
    )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RuntimeHandoff {
    token: String,
}

// The page is identical for every grant and contains no credential. Only this
// exact inline script is allowed by CSP. Secrets never enter a request target.
const HANDOFF_SCRIPT: &str = r#"(async () => {
  const token = new URLSearchParams(location.hash.slice(1)).get('token');
  history.replaceState(null, '', location.pathname);
  if (!token || !/^[A-Za-z0-9_-]{43}$/.test(token)) {
    document.body.textContent = '运行时交接无效，请从门户重新打开。';
    return;
  }
  // CORS-mode fetch preserves our Origin even under no-referrer; a navigation
  // form may instead send Origin:null. Manual redirect avoids fetching the app twice.
  const response = await fetch(location.pathname, {
    method: 'POST', mode: 'cors', credentials: 'same-origin', redirect: 'manual',
    body: new URLSearchParams({token})
  });
  if (response.type !== 'opaqueredirect') throw new Error('handoff denied');
  location.replace('/connect/' + location.pathname.split('/').pop() + '/');
})().catch(() => { document.body.textContent = '运行时交接失败，请从门户重新打开。'; });"#;

static HANDOFF_PAGE: LazyLock<String> = LazyLock::new(|| {
    format!(
        "<!doctype html><meta charset=\"utf-8\"><title>连接运行时</title><body><script>{HANDOFF_SCRIPT}</script>"
    )
});
static HANDOFF_CSP: LazyLock<Result<HeaderValue, axum::http::header::InvalidHeaderValue>> =
    LazyLock::new(|| {
        let digest = STANDARD.encode(Sha256::digest(HANDOFF_SCRIPT.as_bytes()));
        HeaderValue::from_str(&format!(
            "default-src 'none'; script-src 'sha256-{digest}'; connect-src 'self'; form-action 'none'; base-uri 'none'; frame-ancestors 'none'"
        ))
    });

pub(super) async fn handoff_page() -> Result<Response, ApiError> {
    let csp = HANDOFF_CSP.as_ref().map_err(|_| store_error())?;
    Response::builder()
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .header(header::CACHE_CONTROL, "no-store")
        .header("referrer-policy", "no-referrer")
        .header("cross-origin-opener-policy", "same-origin")
        .header("content-security-policy", csp.clone())
        .body(axum::body::Body::from(HANDOFF_PAGE.as_str()))
        .map_err(|_| store_error())
}

pub(super) async fn consume(
    State(state): State<Arc<AppState>>,
    Path(grant): Path<EndpointGrantId>,
    headers: HeaderMap,
    Form(handoff): Form<RuntimeHandoff>,
) -> Result<Response, ApiError> {
    let token = handoff.token;
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
    if browser_host(state, uri, headers)? != BrowserHost::Runtime(grant) {
        return Err(denied());
    }
    let capability = cookie(headers, CAPABILITY_COOKIE)
        .filter(|v| valid_secret(v))
        .ok_or_else(denied)?;
    let row = sqlx::query(
        "WITH authority AS MATERIALIZED (SELECT date_trunc('milliseconds',clock_timestamp()) AS now) \
         SELECT c.bff_session_id,c.actor_id,n.now AS authority_now FROM access.runtime_capabilities c CROSS JOIN authority n \
         WHERE capability_sha256=$1 AND endpoint_grant_id=$2 AND consumed_at IS NOT NULL AND expires_at>n.now",
    ).bind(hash(capability)).bind(grant.as_uuid()).fetch_optional(&state.pool)
        .await.map_err(|_| store_error())?.ok_or_else(denied)?;
    let session = auth::load_bff_session(
        &state.pool,
        &state.key_ring,
        row.get("bff_session_id"),
        super::deployment_duration(state.deployment.browser.session_idle_ttl_seconds)?,
        row.get::<OffsetDateTime, _>("authority_now"),
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

fn origin_denied() -> ApiError {
    ApiError::forbidden("LW_ACCESS_RUNTIME_ORIGIN_REJECTED")
}

// Only trusted portal HTML executes this bridge. The binding lives in a URL
// fragment until it is removed locally, then travels in an authenticated CSRF POST.
fn bridge(session: &auth::BffSession) -> Result<Response, ApiError> {
    let nonce = secret();
    let csrf = serde_json::to_string(session.csrf_token.expose()).map_err(|_| store_error())?;
    let html = format!(
        r#"<!doctype html><meta charset="utf-8"><title>连接运行时</title>
<script nonce="{nonce}">
(async () => {{
  const binding = new URLSearchParams(location.hash.slice(1)).get('binding');
  history.replaceState(null, '', location.pathname);
  if (!binding || !/^[A-Za-z0-9_-]{{43}}$/.test(binding)) throw new Error('invalid handoff');
  const response = await fetch(location.pathname, {{method:'POST',headers:{{'Content-Type':'application/json','X-CSRF-Token':{csrf}}},body:JSON.stringify({{binding}}),redirect:'error'}});
  if (!response.ok) throw new Error('handoff denied');
  location.replace((await response.json()).handoffUrl);
}})().catch(() => {{document.body.textContent='运行时交接失败，请从门户重新打开。';}});
</script>"#
    );
    Response::builder().header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .header(header::CACHE_CONTROL, "no-store").header("referrer-policy", "no-referrer")
        .header("cross-origin-opener-policy", "same-origin")
        .header("content-security-policy", format!("default-src 'none'; script-src 'nonce-{nonce}'; connect-src 'self'; base-uri 'none'; frame-ancestors 'none'"))
        .body(axum::body::Body::from(html)).map_err(|_| store_error())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classify(uri: &str, headers: &HeaderMap) -> Result<BrowserHost, ApiError> {
        let uri: Uri = uri.parse().map_err(|_| origin_denied())?;
        classify_authority(
            &BTreeSet::from(["https://portal.example.test".to_owned()]),
            "https://runtime.example.test",
            &request_authority(&uri, headers)?,
        )
    }

    #[test]
    fn canonical_authorities_cover_http_one_and_http_two() -> Result<(), Box<dyn std::error::Error>>
    {
        for host in [
            "PORTAL.EXAMPLE.TEST",
            "portal.example.test:443",
            "portal.example.test.:443",
        ] {
            let mut headers = HeaderMap::new();
            headers.insert(header::HOST, host.parse()?);
            assert_eq!(
                classify("/api/v1/auth/session", &headers).ok(),
                Some(BrowserHost::Portal)
            );
            assert_eq!(
                classify("https://portal.example.test/api/v1/auth/session", &headers).ok(),
                Some(BrowserHost::Portal)
            );
        }
        assert_eq!(
            classify(
                "https://PORTAL.EXAMPLE.TEST.:443/api/v1/auth/session",
                &HeaderMap::new()
            )
            .ok(),
            Some(BrowserHost::Portal)
        );
        let grant = EndpointGrantId::new();
        let uri = format!(
            "https://G-{}.RUNTIME.EXAMPLE.TEST.:443/connect/{grant}/",
            grant.as_uuid().simple()
        );
        assert_eq!(
            classify(&uri, &HeaderMap::new()).ok(),
            Some(BrowserHost::Runtime(grant))
        );
        Ok(())
    }

    #[test]
    fn missing_conflicting_unknown_and_forwarded_authorities_are_rejected()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = "/api/v1/auth/session";
        assert!(classify(path, &HeaderMap::new()).is_err());
        for host in [
            "unknown.example.test",
            "runtime.example.test",
            "g-invalid.runtime.example.test",
            "portal.example.test:8443",
            "user@portal.example.test",
        ] {
            let mut headers = HeaderMap::new();
            headers.insert(header::HOST, host.parse()?);
            headers.insert("x-forwarded-host", "portal.example.test".parse()?);
            assert!(
                classify(path, &headers).is_err(),
                "accepted unknown authority {host}"
            );
            assert!(
                classify("https://portal.example.test/api/v1/auth/session", &headers).is_err(),
                "accepted conflicting HTTP/2 authority {host}"
            );
        }
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-host", "portal.example.test".parse()?);
        assert!(classify(path, &headers).is_err());
        headers.insert(header::HOST, "portal.example.test".parse()?);
        headers.insert("x-forwarded-host", "unknown.example.test".parse()?);
        assert_eq!(classify(path, &headers).ok(), Some(BrowserHost::Portal));
        headers.append(header::HOST, "portal.example.test".parse()?);
        assert!(classify(path, &headers).is_err());
        Ok(())
    }
}
