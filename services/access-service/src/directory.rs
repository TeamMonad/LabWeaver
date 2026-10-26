use std::sync::Arc;

use auth::ServiceTokenClient;
use contracts::auth::{OrganizationUser, OrganizationUserPage};
use serde::Deserialize;
use thiserror::Error;
use url::Url;

/// The issuer-backed organization directory used for membership selection.
///
/// This is deliberately a small Keycloak Admin REST client.  Access remains
/// the only component that resolves provider identities into local actors.
#[derive(Clone)]
pub(crate) struct KeycloakDirectory {
    client: reqwest::Client,
    users_endpoint: Url,
    service_token_client: Arc<ServiceTokenClient>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct DirectoryFailure {
    pub(crate) stage: &'static str,
    pub(crate) error_kind: &'static str,
    pub(crate) http_status: Option<u16>,
    pub(crate) retryable: bool,
}

impl DirectoryFailure {
    fn token(error: &auth::ServiceTokenClientError) -> Self {
        if let auth::ServiceTokenClientError::TokenExchange(failure) = error {
            return Self {
                stage: "directory.token",
                error_kind: failure.kind.as_str(),
                http_status: failure.http_status,
                retryable: failure.kind.retryable(),
            };
        }
        let (error_kind, retryable) = match error {
            auth::ServiceTokenClientError::Discovery => ("token_discovery", true),
            auth::ServiceTokenClientError::HttpClient => ("token_client", false),
            _ => ("token_rejected", false),
        };
        Self {
            stage: "directory.token",
            error_kind,
            http_status: None,
            retryable,
        }
    }

    fn request(error: &reqwest::Error) -> Self {
        let error_kind = if error.is_timeout() {
            "timeout"
        } else if is_tls_error(error) {
            "tls"
        } else {
            "transport"
        };
        Self {
            stage: "directory.request",
            error_kind,
            http_status: None,
            retryable: error_kind != "tls" && (error.is_timeout() || error.is_connect()),
        }
    }

    fn response(status: reqwest::StatusCode) -> Self {
        let (error_kind, retryable) = match status {
            reqwest::StatusCode::UNAUTHORIZED => ("http_unauthorized", false),
            reqwest::StatusCode::FORBIDDEN => ("http_forbidden", false),
            reqwest::StatusCode::NOT_FOUND => ("http_not_found", false),
            reqwest::StatusCode::TOO_MANY_REQUESTS => ("http_rate_limited", true),
            status if status.is_server_error() => ("http_server_error", true),
            _ => ("http_unexpected", false),
        };
        Self {
            stage: "directory.response",
            error_kind,
            http_status: Some(status.as_u16()),
            retryable,
        }
    }

    fn decode(error: &reqwest::Error) -> Self {
        Self {
            stage: "directory.decode",
            error_kind: if error.is_timeout() {
                "timeout"
            } else {
                "decode"
            },
            http_status: Some(reqwest::StatusCode::OK.as_u16()),
            retryable: error.is_timeout(),
        }
    }
}

fn is_tls_error(error: &reqwest::Error) -> bool {
    let mut source = std::error::Error::source(error);
    while let Some(cause) = source {
        if cause.is::<rustls::Error>() {
            return true;
        }
        source = cause
            .downcast_ref::<std::io::Error>()
            .and_then(std::io::Error::get_ref)
            .map(|inner| inner as &(dyn std::error::Error + 'static))
            .or_else(|| cause.source());
    }
    false
}

#[derive(Debug, Error)]
pub(crate) enum DirectoryError {
    #[error("directory endpoint is unavailable")]
    Unavailable,
    #[error("directory endpoint is unavailable")]
    Upstream(DirectoryFailure),
    #[error("directory user was not found")]
    UserNotFound,
    #[error("directory user is disabled")]
    UserDisabled,
}

#[derive(Debug, Deserialize)]
struct KeycloakUser {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    username: Option<String>,
    #[serde(default, rename = "firstName")]
    first_name: Option<String>,
    #[serde(default, rename = "lastName")]
    last_name: Option<String>,
    #[serde(default)]
    enabled: Option<bool>,
}

#[derive(Clone, Debug)]
pub(crate) struct ResolvedDirectoryUser {
    pub(crate) subject: String,
    pub(crate) username: String,
    pub(crate) display_name: String,
}

impl KeycloakDirectory {
    pub(crate) fn new(
        issuer: &str,
        client: reqwest::Client,
        service_token_client: Arc<ServiceTokenClient>,
    ) -> Result<Self, DirectoryError> {
        let issuer = Url::parse(issuer).map_err(|_| DirectoryError::Unavailable)?;
        let prefix = issuer
            .path_segments()
            .ok_or(DirectoryError::Unavailable)?
            .filter(|segment| !segment.is_empty())
            .collect::<Vec<_>>();
        if prefix.len() < 2 || prefix[prefix.len() - 2] != "realms" {
            return Err(DirectoryError::Unavailable);
        }
        let realm = prefix[prefix.len() - 1];
        if realm.is_empty() {
            return Err(DirectoryError::Unavailable);
        }
        let base = &prefix[..prefix.len() - 2];
        let path = if base.is_empty() {
            format!("/admin/realms/{realm}/users")
        } else {
            format!("/{}/admin/realms/{realm}/users", base.join("/"))
        };
        let mut users_endpoint = issuer;
        users_endpoint.set_path(&path);
        users_endpoint.set_query(None);
        users_endpoint.set_fragment(None);
        Ok(Self {
            client,
            users_endpoint,
            service_token_client,
        })
    }

    pub(crate) async fn search(
        &self,
        query: &str,
        page: u32,
        page_size: u16,
        first: u32,
    ) -> Result<OrganizationUserPage, DirectoryError> {
        let users = self
            .request_users(&[
                ("search", query.to_owned()),
                ("first", first.to_string()),
                ("max", (u32::from(page_size) + 1_u32).to_string()),
            ])
            .await?;
        let has_more = users.len() > usize::from(page_size);
        let items = users
            .into_iter()
            .take(usize::from(page_size))
            .filter_map(|user| {
                let username = user.username.as_deref()?.trim().to_owned();
                if username.is_empty() {
                    return None;
                }
                let display_name = display_name(&user, &username);
                Some(OrganizationUser {
                    username,
                    display_name,
                    enabled: user.enabled.unwrap_or(false),
                })
            })
            .collect();
        Ok(OrganizationUserPage {
            items,
            page,
            page_size,
            has_more,
        })
    }

    pub(crate) async fn resolve_username(
        &self,
        username: &str,
    ) -> Result<ResolvedDirectoryUser, DirectoryError> {
        let username = username.trim();
        if username.is_empty() {
            return Err(DirectoryError::UserNotFound);
        }
        let users = self
            .request_users(&[
                ("username", username.to_owned()),
                ("exact", "true".to_owned()),
                ("first", "0".to_owned()),
                ("max", "2".to_owned()),
            ])
            .await?;
        let user = select_exact_user(users, username)?;
        if !user.enabled.unwrap_or(false) {
            return Err(DirectoryError::UserDisabled);
        }
        let subject = user
            .id
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .ok_or(DirectoryError::Unavailable)?;
        let subject = subject.to_owned();
        let username = user
            .username
            .as_ref()
            .filter(|value| !value.trim().is_empty())
            .ok_or(DirectoryError::Unavailable)?
            .trim()
            .to_owned();
        let display_name = display_name(&user, &username);
        Ok(ResolvedDirectoryUser {
            subject,
            username,
            display_name,
        })
    }

    async fn request_users(
        &self,
        params: &[(&str, String)],
    ) -> Result<Vec<KeycloakUser>, DirectoryError> {
        let token = self
            .service_token_client
            .access_token()
            .await
            .map_err(|error| DirectoryError::Upstream(DirectoryFailure::token(&error)))?;
        let response = self
            .client
            .get(self.users_endpoint.clone())
            .query(params)
            .bearer_auth(token)
            .send()
            .await
            .map_err(|error| DirectoryError::Upstream(DirectoryFailure::request(&error)))?;
        read_users(response).await
    }
}

async fn read_users(response: reqwest::Response) -> Result<Vec<KeycloakUser>, DirectoryError> {
    match response.status() {
        reqwest::StatusCode::OK => response
            .json::<Vec<KeycloakUser>>()
            .await
            .map_err(|error| DirectoryError::Upstream(DirectoryFailure::decode(&error))),
        status => Err(DirectoryError::Upstream(DirectoryFailure::response(status))),
    }
}

fn display_name(user: &KeycloakUser, username: &str) -> String {
    let given = user.first_name.as_deref().unwrap_or("").trim();
    let family = user.last_name.as_deref().unwrap_or("").trim();
    match (given.is_empty(), family.is_empty()) {
        (false, false) => format!("{given} {family}"),
        (false, true) => given.to_owned(),
        (true, false) => family.to_owned(),
        (true, true) => username.to_owned(),
    }
}

fn select_exact_user(
    users: Vec<KeycloakUser>,
    username: &str,
) -> Result<KeycloakUser, DirectoryError> {
    let user = match users.as_slice() {
        [] => return Err(DirectoryError::UserNotFound),
        [_] => users
            .into_iter()
            .next()
            .ok_or(DirectoryError::Unavailable)?,
        _ => return Err(DirectoryError::Unavailable),
    };
    if !user
        .username
        .as_deref()
        .is_some_and(|value| value.eq_ignore_ascii_case(username))
    {
        return Err(DirectoryError::UserNotFound);
    }
    Ok(user)
}

#[cfg(test)]
mod tests {
    use std::{
        io::Write,
        sync::{Arc, Mutex},
    };

    use super::{
        DirectoryError, DirectoryFailure, KeycloakDirectory, KeycloakUser, select_exact_user,
    };
    use axum::{
        Json, Router,
        extract::State,
        http::{HeaderMap, StatusCode},
        response::IntoResponse,
        routing::{get, post},
    };

    #[derive(Clone)]
    struct SharedWriter(Arc<Mutex<Vec<u8>>>);

    struct BufferWriter(Arc<Mutex<Vec<u8>>>);

    impl Write for BufferWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0
                .lock()
                .map_err(|_| std::io::Error::other("log buffer poisoned"))?
                .extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'writer> tracing_subscriber::fmt::MakeWriter<'writer> for SharedWriter {
        type Writer = BufferWriter;

        fn make_writer(&'writer self) -> Self::Writer {
            BufferWriter(Arc::clone(&self.0))
        }
    }

    const CLIENT_SECRET: &str = "client-secret-sentinel";
    const QUERY_USERNAME: &str = "query-user-secret-sentinel";
    const RESPONSE_BODY: &str = "response-body-secret-sentinel";

    #[derive(Clone, Copy)]
    enum FailureCase {
        TokenRejected,
        TokenUnavailable,
        TokenTimeout,
        TokenMalformed,
        DirectoryForbidden,
        DirectoryMalformed,
    }

    impl FailureCase {
        fn expected(self) -> (&'static str, &'static str, Option<u16>, bool) {
            match self {
                Self::TokenRejected => ("directory.token", "rejected", Some(401), false),
                Self::TokenUnavailable => ("directory.token", "upstream_5xx", Some(503), true),
                Self::TokenTimeout => ("directory.token", "timeout", None, true),
                Self::TokenMalformed => ("directory.token", "decode", Some(200), false),
                Self::DirectoryForbidden => {
                    ("directory.response", "http_forbidden", Some(403), false)
                }
                Self::DirectoryMalformed => ("directory.decode", "decode", Some(200), false),
            }
        }

        fn reaches_directory(self) -> bool {
            matches!(self, Self::DirectoryForbidden | Self::DirectoryMalformed)
        }
    }

    #[derive(Clone)]
    struct AuthorityState {
        issuer: String,
        token: String,
        case: FailureCase,
        calls: Arc<[std::sync::atomic::AtomicUsize; 2]>,
    }

    struct DirectoryFixture {
        directory: KeycloakDirectory,
        state: AuthorityState,
        server: tokio::task::JoinHandle<std::io::Result<()>>,
    }

    impl Drop for DirectoryFixture {
        fn drop(&mut self) {
            self.server.abort();
        }
    }

    async fn token_endpoint(
        State(state): State<AuthorityState>,
        headers: HeaderMap,
    ) -> axum::response::Response {
        use base64::Engine as _;
        state.calls[0].fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let authorization = format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD
                .encode(format!("directory-client:{CLIENT_SECRET}"))
        );
        assert!(
            headers
                .get(axum::http::header::AUTHORIZATION)
                .is_some_and(|value| value == authorization.as_str())
        );
        match state.case {
            FailureCase::TokenRejected | FailureCase::TokenUnavailable => {
                let status = if matches!(state.case, FailureCase::TokenRejected) { StatusCode::UNAUTHORIZED } else { StatusCode::SERVICE_UNAVAILABLE };
                (status, Json(serde_json::json!({"error":"invalid_client", "error_description": RESPONSE_BODY}))).into_response()
            }
            FailureCase::TokenTimeout => {
                tokio::time::sleep(std::time::Duration::from_secs(10)).await;
                Json(serde_json::json!({"error": RESPONSE_BODY})).into_response()
            }
            FailureCase::TokenMalformed => Json(serde_json::json!({"not_a_token": RESPONSE_BODY})).into_response(),
            _ => Json(serde_json::json!({"access_token":state.token, "token_type":"Bearer", "expires_in":300})).into_response(),
        }
    }

    async fn directory_endpoint(
        State(state): State<AuthorityState>,
        headers: HeaderMap,
    ) -> axum::response::Response {
        state.calls[1].fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        assert!(
            headers
                .get(axum::http::header::AUTHORIZATION)
                .is_some_and(|value| value == format!("Bearer {}", state.token).as_str())
        );
        let status = if matches!(state.case, FailureCase::DirectoryForbidden) {
            StatusCode::FORBIDDEN
        } else {
            StatusCode::OK
        };
        (status, Json(serde_json::json!({"error": RESPONSE_BODY}))).into_response()
    }

    async fn directory_fixture(
        case: FailureCase,
    ) -> Result<DirectoryFixture, Box<dyn std::error::Error>> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let issuer = format!("http://{}/realms/test", listener.local_addr()?);
        let state = AuthorityState {
            issuer: issuer.clone(),
            token: jsonwebtoken::encode(
                &jsonwebtoken::Header::default(),
                &serde_json::json!({"aud":"labweaver-access", "sub":"external-authority-fixture", "exp":time::OffsetDateTime::now_utc().unix_timestamp()+300}),
                &jsonwebtoken::EncodingKey::from_secret(b"signing-secret-sentinel"),
            )?,
            case,
            calls: Arc::new([
                std::sync::atomic::AtomicUsize::new(0),
                std::sync::atomic::AtomicUsize::new(0),
            ]),
        };
        let router = Router::new()
            .route("/realms/test/.well-known/openid-configuration", get(|State(state): State<AuthorityState>| async move {
                Json(serde_json::json!({"issuer":state.issuer, "authorization_endpoint":format!("{}/authorize", state.issuer), "token_endpoint":format!("{}/token", state.issuer), "jwks_uri":format!("{}/jwks", state.issuer), "response_types_supported":["code"], "subject_types_supported":["public"], "id_token_signing_alg_values_supported":["RS256"]}))
            }))
            .route("/realms/test/jwks", get(|| async { Json(serde_json::json!({"keys": []})) }))
            .route("/realms/test/token", post(token_endpoint))
            .route("/admin/realms/test/users", get(directory_endpoint))
            .with_state(state.clone());
        let server = tokio::spawn(async move { axum::serve(listener, router).await });
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(1))
            .build()?;
        let config = auth::ServiceTokenClientConfig::new(
            &issuer,
            "directory-client".to_owned(),
            CLIENT_SECRET.to_owned(),
            "labweaver-access".to_owned(),
            std::collections::BTreeSet::new(),
            30,
            auth::TransportSecurityMode::InsecureTestOnly,
        )?;
        let token_client =
            Arc::new(auth::ServiceTokenClient::discover(config, http.clone()).await?);
        let directory = KeycloakDirectory::new(&issuer, http, token_client)?;
        Ok(DirectoryFixture {
            directory,
            state,
            server,
        })
    }

    async fn assert_private_failure(case: FailureCase) -> Result<(), Box<dyn std::error::Error>> {
        use base64::Engine as _;
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::fmt()
            .json()
            .flatten_event(true)
            .with_max_level(tracing::Level::TRACE)
            .with_writer(SharedWriter(Arc::clone(&bytes)))
            .finish();
        let guard = tracing::subscriber::set_default(subscriber);
        let fixture = directory_fixture(case).await?;
        let error = fixture
            .directory
            .search(QUERY_USERNAME, 1, 20, 0)
            .await
            .err()
            .ok_or("expected upstream rejection")?;
        let response = crate::ApiError::from(error).into_response();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = axum::body::to_bytes(response.into_body(), 4096).await?;
        let problem: serde_json::Value = serde_json::from_slice(&body)?;
        let (stage, class, status, retryable) = case.expected();
        assert_eq!(problem["diagnosticCode"], "LW_ACCESS_DIRECTORY_UNAVAILABLE");
        assert_eq!(problem["retryable"], retryable);
        assert_eq!(
            fixture.state.calls[0].load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        assert_eq!(
            fixture.state.calls[1].load(std::sync::atomic::Ordering::SeqCst),
            usize::from(case.reaches_directory())
        );
        drop(guard);
        let output = String::from_utf8(
            bytes
                .lock()
                .map_err(|_| std::io::Error::other("log buffer poisoned"))?
                .clone(),
        )?;
        let events = output
            .lines()
            .map(serde_json::from_str::<serde_json::Value>)
            .collect::<Result<Vec<_>, _>>()?;
        let event = events
            .iter()
            .find(|event| event["event"] == "auth.request.rejected")
            .ok_or("missing request rejection log")?;
        assert_eq!(event["failure_stage"], stage);
        assert_eq!(event["error_kind"], class);
        assert_eq!(
            event.get("http_status").and_then(serde_json::Value::as_u64),
            status.map(u64::from)
        );
        assert_eq!(event["retryable"], retryable);
        if !case.reaches_directory() {
            let token_event = events
                .iter()
                .find(|event| event["event"] == "auth.service_token.failed")
                .ok_or("missing token exchange log")?;
            assert_eq!(token_event["error_kind"], class);
            assert_eq!(
                token_event
                    .get("http_status")
                    .and_then(serde_json::Value::as_u64),
                status.map(u64::from)
            );
            assert_eq!(token_event["retryable"], retryable);
        }
        let public = std::str::from_utf8(&body)?;
        let basic = base64::engine::general_purpose::STANDARD
            .encode(format!("directory-client:{CLIENT_SECRET}"));
        for secret in [
            fixture.state.token.as_str(),
            CLIENT_SECRET,
            QUERY_USERNAME,
            RESPONSE_BODY,
            basic.as_str(),
        ] {
            assert!(!public.contains(secret));
            for line in output.lines() {
                assert!(!line.contains(secret));
            }
        }
        Ok(())
    }

    #[tokio::test(flavor = "current_thread")]
    async fn directory_token_and_response_failures_are_classified_without_leaks_at_any_log_level()
    -> Result<(), Box<dyn std::error::Error>> {
        for case in [
            FailureCase::TokenRejected,
            FailureCase::TokenUnavailable,
            FailureCase::TokenTimeout,
            FailureCase::TokenMalformed,
            FailureCase::DirectoryForbidden,
            FailureCase::DirectoryMalformed,
        ] {
            assert_private_failure(case).await?;
        }
        Ok(())
    }

    #[test]
    fn directory_failure_stages_keep_permanent_and_retryable_causes_distinct() {
        for status in [
            StatusCode::UNAUTHORIZED,
            StatusCode::FORBIDDEN,
            StatusCode::NOT_FOUND,
        ] {
            let failure = DirectoryFailure::response(status);
            assert_eq!(failure.http_status, Some(status.as_u16()));
            assert_eq!(failure.stage, "directory.response");
            assert!(!failure.retryable);
        }
        for status in [
            StatusCode::TOO_MANY_REQUESTS,
            StatusCode::SERVICE_UNAVAILABLE,
        ] {
            assert!(DirectoryFailure::response(status).retryable);
        }
        let failure =
            DirectoryFailure::token(&auth::ServiceTokenClientError::TokenAudienceRejected);
        assert_eq!(failure.stage, "directory.token");
        assert_eq!(failure.error_kind, "token_rejected");
        assert_eq!(failure.http_status, None);
        assert!(!failure.retryable);
        let exchange =
            auth::ServiceTokenClientError::TokenExchange(auth::ServiceTokenExchangeFailure {
                kind: auth::ServiceTokenFailureKind::Upstream5xx,
                http_status: Some(503),
            });
        assert!(DirectoryFailure::token(&exchange).retryable);
    }

    fn user(username: &str) -> KeycloakUser {
        KeycloakUser {
            id: Some("subject".to_owned()),
            username: Some(username.to_owned()),
            first_name: None,
            last_name: None,
            enabled: Some(true),
        }
    }

    #[test]
    fn exact_username_resolution_rejects_ambiguous_provider_results() {
        let result = select_exact_user(vec![user("alice"), user("alice")], "alice");
        assert!(matches!(result, Err(DirectoryError::Unavailable)));
    }

    #[test]
    fn exact_username_resolution_accepts_case_insensitive_single_result()
    -> Result<(), DirectoryError> {
        let result = select_exact_user(vec![user("Alice")], "alice")?;
        assert_eq!(result.username.as_deref(), Some("Alice"));
        Ok(())
    }
}
