//! Browser-bound, one-time OIDC login transactions.

use std::sync::Arc;

use auth::{
    OidcTransaction, consume_oidc_transaction, create_bff_session, extract_platform_roles,
    upsert_actor_with_metadata,
};
use axum::{
    extract::{Query, State},
    http::{HeaderMap, HeaderValue, header},
    response::{IntoResponse, Redirect, Response},
};
use persistence_sqlx::Sha256Digest;
use serde::Deserialize;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{ApiError, AppState, deployment_duration, identity_metadata, session_cookie};

#[derive(Deserialize)]
pub(super) struct LoginQuery {
    return_to: Option<String>,
}

pub(super) async fn login(
    State(state): State<Arc<AppState>>,
    Query(query): Query<LoginQuery>,
) -> Response {
    sensitive_response(begin_login(&state, query).await.into_response())
}

async fn begin_login(state: &AppState, query: LoginQuery) -> Result<Response, ApiError> {
    state
        .config
        .login_return_url(query.return_to.as_deref())
        .map_err(|_| ApiError::bad_request("LW_AUTH_OIDC_RETURN_TO_REJECTED"))?;
    let mut transaction = OidcTransaction::generate()
        .map_err(|_| ApiError::unavailable("LW_AUTH_OIDC_RANDOMNESS_UNAVAILABLE"))?;
    transaction.return_to = query.return_to;
    let url = state
        .provider
        .authorization_url(&transaction)
        .map_err(ApiError::from)?;
    let cookie_name = transaction_cookie_name(&transaction.state)?;
    let cookie = transaction_cookie(
        &cookie_name,
        &transaction.browser_binding,
        state.deployment.browser.oidc_transaction_ttl_seconds,
    )?;
    let transaction_id = Uuid::now_v7();
    let encrypted = state
        .key_ring
        .encrypt(
            &serde_json::to_vec(&transaction)
                .map_err(|_| ApiError::internal("LW_AUTH_OIDC_STATE_REJECTED"))?,
            transaction_id.as_bytes(),
        )
        .map_err(|_| ApiError::internal("LW_AUTH_KEYRING_ENCRYPTION_FAILED"))?;
    let state_hash = Sha256Digest::of_bytes(transaction.state.as_bytes()).to_string();
    sqlx::query("INSERT INTO access.oidc_transactions (transaction_id, state_sha256, encrypted_payload, encryption_key_id, expires_at) VALUES ($1,$2,$3,$4,$5)")
        .bind(transaction_id).bind(state_hash).bind(encrypted.payload).bind(encrypted.key_id)
        .bind(OffsetDateTime::now_utc()
            + deployment_duration(state.deployment.browser.oidc_transaction_ttl_seconds)?)
        .execute(&state.pool).await
        .map_err(|_| ApiError::unavailable("LW_AUTH_MEMBERSHIP_UNAVAILABLE"))?;
    let mut response = Redirect::temporary(url.as_str()).into_response();
    response.headers_mut().append(header::SET_COOKIE, cookie);
    metrics::counter!("labweaver_auth_oidc_transactions", "result" => "created").increment(1);
    Ok(response)
}

#[derive(Deserialize)]
pub(super) struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

pub(super) async fn callback(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<CallbackQuery>,
) -> Response {
    let Some(returned_state) = query.state.as_deref() else {
        return sensitive_response(
            ApiError::unauthorized("LW_AUTH_OIDC_STATE_REQUIRED").into_response(),
        );
    };
    let cookie_name = match transaction_cookie_name(returned_state) {
        Ok(name) => name,
        Err(error) => return sensitive_response(error.into_response()),
    };
    let mut response = complete_login(&state, &headers, &query, &cookie_name)
        .await
        .into_response();
    // Remove only this transaction, including provider refusal and exchange failures.
    // A different browser never consumes server state, and other tabs keep their cookies.
    match transaction_cookie(&cookie_name, "", 0) {
        Ok(cookie) => {
            response.headers_mut().append(header::SET_COOKIE, cookie);
        }
        Err(error) => return sensitive_response(error.into_response()),
    }
    sensitive_response(response)
}

async fn complete_login(
    state: &AppState,
    headers: &HeaderMap,
    query: &CallbackQuery,
    cookie_name: &str,
) -> Result<Response, ApiError> {
    let browser_binding = browser_proof(headers, cookie_name)?;
    let returned_state = query
        .state
        .as_deref()
        .ok_or_else(|| ApiError::unauthorized("LW_AUTH_OIDC_STATE_REQUIRED"))?;
    let now = OffsetDateTime::now_utc();
    let transaction = consume_oidc_transaction(
        &state.pool,
        &state.key_ring,
        returned_state,
        browser_binding,
        now,
    )
    .await
    .map_err(ApiError::from)?;
    if query.error.is_some() {
        return Err(ApiError::unauthorized("LW_AUTH_TOKEN_INVALID"));
    }
    let code = query
        .code
        .as_ref()
        .filter(|code| !code.is_empty())
        .ok_or_else(|| ApiError::unauthorized("LW_AUTH_OIDC_STATE_REQUIRED"))?;
    let return_url = state
        .config
        .login_return_url(transaction.return_to.as_deref())
        .map_err(|_| ApiError::unauthorized("LW_AUTH_OIDC_RETURN_TO_REJECTED"))?;
    let identity = state
        .provider
        .exchange_code(code.clone(), &transaction, &state.oidc_http)
        .await
        .map_err(ApiError::from)?;
    identity.validate_expiry_at(now).map_err(ApiError::from)?;
    let roles = extract_platform_roles(
        &identity.claims,
        &state.deployment.oidc.role_claim_path,
        &state.role_mappings,
    )
    .map_err(ApiError::from)?
    .into_iter()
    .collect::<Vec<_>>();
    let metadata = identity_metadata(&identity.claims);
    let actor = upsert_actor_with_metadata(
        &state.pool,
        state.config.issuer.as_str(),
        &identity.subject,
        metadata.username.as_deref(),
        metadata.display_name.as_deref(),
    )
    .await
    .map_err(ApiError::from)?;
    auth::update_notification_email(&state.pool, actor.actor_id, &identity.claims)
        .await
        .map_err(ApiError::from)?;
    let expires_at = auth::configured_session_expiry(
        now,
        deployment_duration(state.config.session_ttl_seconds)?,
    )
    .map_err(ApiError::from)?;
    let session = create_bff_session(
        &state.pool,
        &state.key_ring,
        auth::CreateBffSession {
            actor_id: actor.actor_id,
            roles,
            authorization_revision: 1,
            expires_at,
            idle_ttl: deployment_duration(state.deployment.browser.session_idle_ttl_seconds)?,
            oidc_sid: identity.sid,
            logout_hint: identity.logout_hint,
        },
        now,
    )
    .await
    .map_err(ApiError::from)?;
    let mut response = Redirect::temporary(return_url.as_str()).into_response();
    response.headers_mut().append(
        header::SET_COOKIE,
        session_cookie(state, session.session_id, session.expires_at)?,
    );
    metrics::counter!("labweaver_auth_callbacks", "result" => "success").increment(1);
    Ok(response)
}

fn transaction_cookie_name(state: &str) -> Result<String, ApiError> {
    if !is_transaction_value(state) {
        return Err(ApiError::unauthorized("LW_AUTH_OIDC_STATE_REJECTED"));
    }
    Ok(format!(
        "__Host-labweaver_oidc_{}",
        Sha256Digest::of_bytes(state.as_bytes())
    ))
}

fn is_transaction_value(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn browser_proof<'a>(headers: &'a HeaderMap, cookie_name: &str) -> Result<&'a str, ApiError> {
    let rejected = || ApiError::unauthorized("LW_AUTH_OIDC_BROWSER_REJECTED");
    let mut proof = None;
    for cookie in headers.get_all(header::COOKIE) {
        for part in cookie.to_str().map_err(|_| rejected())?.split(';') {
            let Some((name, value)) = part.trim().split_once('=') else {
                continue;
            };
            if name == cookie_name {
                if proof.is_some() || !is_transaction_value(value) {
                    return Err(rejected());
                }
                proof = Some(value);
            }
        }
    }
    proof.ok_or_else(rejected)
}

fn transaction_cookie(name: &str, value: &str, ttl: u64) -> Result<HeaderValue, ApiError> {
    HeaderValue::from_str(&format!(
        "{name}={value}; Path=/; Secure; HttpOnly; SameSite=Lax; Max-Age={ttl}"
    ))
    .map_err(|_| ApiError::internal("LW_AUTH_OIDC_STATE_REJECTED"))
}

fn sensitive_response(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
        .headers_mut()
        .insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_proof_requires_exactly_one_transaction_cookie()
    -> Result<(), Box<dyn std::error::Error>> {
        let transaction = OidcTransaction::generate()?;
        let other = OidcTransaction::generate()?;
        let name = transaction_cookie_name(&transaction.state).map_err(|_| "invalid state")?;
        let other_name = transaction_cookie_name(&other.state).map_err(|_| "invalid state")?;
        let mut headers = HeaderMap::new();
        assert!(browser_proof(&headers, &name).is_err());
        headers.append(
            header::COOKIE,
            HeaderValue::from_str(&format!(
                "{other_name}={}; {name}={}",
                other.browser_binding, transaction.browser_binding
            ))?,
        );
        assert_eq!(
            browser_proof(&headers, &name).map_err(|_| "cookie rejected")?,
            transaction.browser_binding
        );
        // Duplicate values are rejected both within a field and across fields.
        let same = HeaderValue::from_str(&format!("{name}={}", transaction.browser_binding))?;
        headers.append(header::COOKIE, same.clone());
        assert!(browser_proof(&headers, &name).is_err());
        headers.clear();
        headers.append(
            header::COOKIE,
            HeaderValue::from_str(&format!("{}; {}", same.to_str()?, same.to_str()?))?,
        );
        assert!(browser_proof(&headers, &name).is_err());
        Ok(())
    }
}
