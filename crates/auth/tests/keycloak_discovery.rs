//! Explicit real-Keycloak Authorization Code + PKCE verification.
//!
//! The test is ignored by default because it requires a caller-owned temporary
//! Keycloak container and its controlled CA file. It intentionally has no
//! fallback endpoint or fixture-only success path.

use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
};

use auth::config::OidcFileConfig;
use auth::{AuthConfig, OidcProvider, OidcTransaction, build_bearer_authorizer};
use reqwest::{Certificate, StatusCode, redirect::Policy};
use scraper::{Html, Selector};
use serde::Deserialize;
use url::Url;

#[tokio::test]
#[ignore = "requires LABWEAVER_KEYCLOAK_TEST_ISSUER and LABWEAVER_KEYCLOAK_TEST_CA_FILE"]
async fn configured_keycloak_completes_pkce_exchange_and_provider_logout()
-> Result<(), Box<dyn std::error::Error>> {
    let issuer = env::var("LABWEAVER_KEYCLOAK_TEST_ISSUER")?;
    let ca_file = env::var("LABWEAVER_KEYCLOAK_TEST_CA_FILE")?;
    let config = AuthConfig::new(
        &issuer,
        "labweaver-web".to_owned(),
        "http://127.0.0.1:38080/auth/callback",
        "http://127.0.0.1:38080/",
        "labweaver-api".to_owned(),
        BTreeSet::from(["https://portal.example.test".to_owned()]),
        900,
    )?;
    let ca = Certificate::from_pem(&fs::read(ca_file)?)?;
    let http = reqwest::Client::builder()
        .add_root_certificate(ca)
        .cookie_store(true)
        .redirect(Policy::none())
        .build()?;
    let document = http
        .get(format!("{issuer}/.well-known/openid-configuration"))
        .send()
        .await?
        .error_for_status()?;
    let metadata: serde_json::Value = document.json().await?;
    assert_eq!(
        metadata.get("issuer").and_then(serde_json::Value::as_str),
        Some(issuer.as_str())
    );
    let provider = OidcProvider::discover_with_http_client(&config, http.clone()).await?;
    let (transaction, code) = authorization_code(&http, &provider, &config).await?;
    let identity = provider.exchange_code(code, &transaction, &http).await?;
    assert!(!identity.subject.is_empty());
    assert!(identity.expires_at > time::OffsetDateTime::now_utc());
    assert!(
        identity
            .claims
            .pointer("/realm_access/roles")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|roles| roles.iter().any(|role| role == "teacher"))
    );

    let logout_url = provider.logout_url(
        &identity.logout_hint,
        config.post_logout_redirect_uri.as_str(),
    )?;
    assert!(
        logout_url
            .path()
            .ends_with("/protocol/openid-connect/logout")
    );
    let logout = http.get(logout_url).send().await?;
    assert!(matches!(
        logout.status(),
        StatusCode::FOUND | StatusCode::SEE_OTHER
    ));
    let post_logout = follow_provider_redirects(
        &http,
        logout,
        config.post_logout_redirect_uri.as_str(),
        config.issuer.origin(),
    )
    .await?;
    assert_eq!(
        post_logout.as_str(),
        config.post_logout_redirect_uri.as_str()
    );

    let oidc = oidc_file(&issuer);
    let verifier = build_bearer_authorizer(&config, &oidc, http.clone()).await?;
    let first_token = issue_access_token(&http, &provider, &config, &issuer).await?;
    verifier.check_auth(&first_token).await?;
    tokio::time::sleep(std::time::Duration::from_millis(1_100)).await;

    let admin_username = env::var("LABWEAVER_KEYCLOAK_TEST_ADMIN_USERNAME")?;
    let admin_password = env::var("LABWEAVER_KEYCLOAK_TEST_ADMIN_PASSWORD")?;
    let admin_token = admin_token(&http, &issuer, &admin_username, &admin_password).await?;
    rotate_signing_key(&http, &issuer, &admin_token, 200).await?;
    let rotated_token = issue_access_token(&http, &provider, &config, &issuer).await?;
    assert_ne!(token_kid(&first_token)?, token_kid(&rotated_token)?);
    verifier.check_auth(&rotated_token).await?;

    let stale_verifier = build_bearer_authorizer(&config, &oidc, http.clone()).await?;
    stale_verifier.check_auth(&rotated_token).await?;
    tokio::time::sleep(std::time::Duration::from_millis(1_100)).await;
    rotate_signing_key(&http, &issuer, &admin_token, 300).await?;
    let outage_token = issue_access_token(&http, &provider, &config, &issuer).await?;
    assert_ne!(token_kid(&rotated_token)?, token_kid(&outage_token)?);
    remove_all_key_providers(&http, &issuer, &admin_token).await?;
    let outage_result = stale_verifier.check_auth(&outage_token).await;
    assert!(outage_result.is_err());
    Ok(())
}

async fn authorization_code(
    http: &reqwest::Client,
    provider: &OidcProvider,
    config: &AuthConfig,
) -> Result<(OidcTransaction, String), Box<dyn std::error::Error>> {
    let transaction = OidcTransaction::generate()?;
    let url = provider.authorization_url(&transaction)?;
    assert_eq!(url.scheme(), "https");
    let query = url.query_pairs().collect::<BTreeMap<_, _>>();
    assert_eq!(query.get("response_type"), Some(&"code".into()));
    assert_eq!(query.get("code_challenge_method"), Some(&"S256".into()));
    assert_eq!(
        query.get("state").map(std::borrow::Cow::as_ref),
        Some(transaction.state.as_str())
    );
    assert_eq!(
        query.get("nonce").map(std::borrow::Cow::as_ref),
        Some(transaction.nonce.as_str())
    );

    let authorization = http.get(url).send().await?;
    let callback = if authorization.status().is_redirection() {
        follow_provider_redirects(
            http,
            authorization,
            config.redirect_uri.as_str(),
            config.issuer.origin(),
        )
        .await?
    } else {
        let login_page = authorization.error_for_status()?.text().await?;
        let document = Html::parse_document(&login_page);
        let selector = Selector::parse("form#kc-form-login")
            .map_err(|_| std::io::Error::other("Keycloak login form selector is invalid"))?;
        let action = document
            .select(&selector)
            .next()
            .and_then(|form| form.value().attr("action"))
            .ok_or_else(|| std::io::Error::other("Keycloak login form action is missing"))?;
        let login = http
            .post(action)
            .form(&[
                ("username", "teacher"),
                ("password", "test-only-password"),
                ("credentialId", ""),
            ])
            .send()
            .await?;
        follow_provider_redirects(
            http,
            login,
            config.redirect_uri.as_str(),
            config.issuer.origin(),
        )
        .await?
    };
    let callback_query = callback.query_pairs().collect::<BTreeMap<_, _>>();
    assert_eq!(
        callback_query.get("state").map(std::borrow::Cow::as_ref),
        Some(transaction.state.as_str())
    );
    let code = callback_query
        .get("code")
        .ok_or_else(|| std::io::Error::other("Keycloak authorization code is missing"))?
        .to_string();
    Ok((transaction, code))
}

#[derive(Deserialize)]
struct AccessTokenResponse {
    access_token: String,
}

async fn issue_access_token(
    http: &reqwest::Client,
    provider: &OidcProvider,
    config: &AuthConfig,
    issuer: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let (transaction, code) = authorization_code(http, provider, config).await?;
    let response = http
        .post(format!("{issuer}/protocol/openid-connect/token"))
        .form(&[
            ("grant_type", "authorization_code"),
            ("client_id", config.client_id.as_str()),
            ("code", code.as_str()),
            ("redirect_uri", config.redirect_uri.as_str()),
            ("code_verifier", transaction.pkce_verifier.as_str()),
        ])
        .send()
        .await?
        .error_for_status()?
        .json::<AccessTokenResponse>()
        .await?;
    Ok(response.access_token)
}

fn oidc_file(issuer: &str) -> OidcFileConfig {
    OidcFileConfig {
        issuer: issuer.to_owned(),
        client_id: "labweaver-web".to_owned(),
        audience: "labweaver-api".to_owned(),
        redirect_uri: "http://127.0.0.1:38080/auth/callback".to_owned(),
        post_logout_redirect_uri: "http://127.0.0.1:38080/".to_owned(),
        trusted_ca_file: String::new(),
        role_claim_path: vec!["realm_access".to_owned(), "roles".to_owned()],
        role_mappings: BTreeMap::from([("teacher".to_owned(), "teacher".to_owned())]),
        jwt_algorithms: BTreeSet::from(["RS256".to_owned()]),
        jwks_refresh_seconds: 3_600,
        jwks_retry_seconds: 1,
    }
}

async fn admin_token(
    http: &reqwest::Client,
    issuer: &str,
    username: &str,
    password: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let base = issuer
        .strip_suffix("/realms/labweaver-test")
        .ok_or_else(|| std::io::Error::other("unexpected test issuer"))?;
    Ok(http
        .post(format!(
            "{base}/realms/master/protocol/openid-connect/token"
        ))
        .form(&[
            ("grant_type", "password"),
            ("client_id", "admin-cli"),
            ("username", username),
            ("password", password),
        ])
        .send()
        .await?
        .error_for_status()?
        .json::<AccessTokenResponse>()
        .await?
        .access_token)
}

async fn rotate_signing_key(
    http: &reqwest::Client,
    issuer: &str,
    admin_token: &str,
    priority: u16,
) -> Result<(), Box<dyn std::error::Error>> {
    let admin_realm = admin_realm_uri(issuer)?;
    let realm = http
        .get(&admin_realm)
        .bearer_auth(admin_token)
        .send()
        .await?
        .error_for_status()?
        .json::<serde_json::Value>()
        .await?;
    let realm_id = realm
        .get("id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| std::io::Error::other("Keycloak realm id is missing"))?;
    http.post(format!("{admin_realm}/components"))
        .bearer_auth(admin_token)
        .json(&serde_json::json!({
            "name": format!("issue-47-rsa-{priority}"),
            "providerId": "rsa-generated",
            "providerType": "org.keycloak.keys.KeyProvider",
            "parentId": realm_id,
            "config": {
                "priority": [priority.to_string()],
                "enabled": ["true"],
                "active": ["true"],
                "algorithm": ["RS256"],
                "keySize": ["2048"]
            }
        }))
        .send()
        .await?
        .error_for_status()?;
    Ok(())
}

async fn remove_all_key_providers(
    http: &reqwest::Client,
    issuer: &str,
    admin_token: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let admin_realm = admin_realm_uri(issuer)?;
    let realm = http
        .get(&admin_realm)
        .bearer_auth(admin_token)
        .send()
        .await?
        .error_for_status()?
        .json::<serde_json::Value>()
        .await?;
    let realm_id = realm
        .get("id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| std::io::Error::other("Keycloak realm id is missing"))?;
    let components = http
        .get(format!("{admin_realm}/components"))
        .bearer_auth(admin_token)
        .query(&[
            ("parent", realm_id),
            ("type", "org.keycloak.keys.KeyProvider"),
        ])
        .send()
        .await?
        .error_for_status()?
        .json::<Vec<serde_json::Value>>()
        .await?;
    for component in components {
        let component_id = component
            .get("id")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| std::io::Error::other("Keycloak component id is missing"))?;
        http.delete(format!("{admin_realm}/components/{component_id}"))
            .bearer_auth(admin_token)
            .send()
            .await?
            .error_for_status()?;
    }
    Ok(())
}

fn admin_realm_uri(issuer: &str) -> Result<String, Box<dyn std::error::Error>> {
    let base = issuer
        .strip_suffix("/realms/labweaver-test")
        .ok_or_else(|| std::io::Error::other("unexpected test issuer"))?;
    Ok(format!("{base}/admin/realms/labweaver-test"))
}

fn token_kid(token: &str) -> Result<String, Box<dyn std::error::Error>> {
    jsonwebtoken::decode_header(token)?
        .kid
        .ok_or_else(|| std::io::Error::other("Keycloak token kid is missing").into())
}

async fn follow_provider_redirects(
    http: &reqwest::Client,
    mut response: reqwest::Response,
    target: &str,
    issuer_origin: url::Origin,
) -> Result<Url, Box<dyn std::error::Error>> {
    for _ in 0..8 {
        let location = response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .ok_or_else(|| std::io::Error::other("Keycloak redirect location is missing"))?;
        let location = Url::parse(location)?;
        if location.as_str().starts_with(target) {
            return Ok(location);
        }
        if location.origin() != issuer_origin {
            return Err(
                std::io::Error::other("Keycloak redirect left the controlled issuer").into(),
            );
        }
        response = http.get(location).send().await?;
        if !response.status().is_redirection() {
            return Err(std::io::Error::other(format!(
                "Keycloak redirect chain stopped with status {} at path {}",
                response.status(),
                response.url().path()
            ))
            .into());
        }
    }
    Err(std::io::Error::other("Keycloak redirect chain exceeded its bound").into())
}
