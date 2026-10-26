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
        let (error_kind, retryable) = match error {
            auth::ServiceTokenClientError::Discovery
            | auth::ServiceTokenClientError::TokenExchange => ("token_exchange", true),
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
        source = cause.source();
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

    use super::{DirectoryError, DirectoryFailure, KeycloakUser, read_users, select_exact_user};
    use axum::{
        Json, Router,
        http::{HeaderMap, StatusCode},
        routing::get,
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

    #[tokio::test(flavor = "current_thread")]
    async fn forbidden_directory_logs_safe_context_without_credentials_or_payload()
    -> Result<(), Box<dyn std::error::Error>> {
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::fmt()
            .json()
            .flatten_event(true)
            .with_max_level(tracing::Level::WARN)
            .with_writer(SharedWriter(Arc::clone(&bytes)))
            .finish();
        let guard = tracing::subscriber::set_default(subscriber);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new().route(
                    "/users",
                    get(|headers: HeaderMap| async move {
                        assert_eq!(
                            headers
                                .get(axum::http::header::AUTHORIZATION)
                                .and_then(|value| value.to_str().ok()),
                            Some("Bearer token-secret-sentinel")
                        );
                        (
                            StatusCode::FORBIDDEN,
                            Json(serde_json::json!({"error": "body-secret-sentinel"})),
                        )
                    }),
                ),
            )
            .await
        });
        let response = reqwest::Client::new()
            .get(format!("http://{address}/users"))
            .query(&[("username", "query-user-secret-sentinel")])
            .bearer_auth("token-secret-sentinel")
            .send()
            .await?;
        let error = read_users(response)
            .await
            .err()
            .ok_or("403 must reject directory lookup")?;
        let api_error: crate::ApiError = error.into();
        assert_eq!(api_error.status, StatusCode::SERVICE_UNAVAILABLE);
        let response = axum::response::IntoResponse::into_response(api_error);
        let body = axum::body::to_bytes(response.into_body(), 4096).await?;
        let problem: serde_json::Value = serde_json::from_slice(&body)?;
        assert_eq!(problem["diagnosticCode"], "LW_ACCESS_DIRECTORY_UNAVAILABLE");
        assert_eq!(problem["retryable"], false);
        drop(guard);
        server.abort();

        let output = String::from_utf8(
            bytes
                .lock()
                .map_err(|_| std::io::Error::other("log buffer poisoned"))?
                .clone(),
        )?;
        let event: serde_json::Value = serde_json::from_str(output.trim())?;
        assert_eq!(event["diagnostic_code"], "LW_ACCESS_DIRECTORY_UNAVAILABLE");
        assert_eq!(event["http_status"], 403);
        assert_eq!(event["error_kind"], "http_forbidden");
        assert_eq!(event["failure_stage"], "directory.response");
        assert_eq!(event["retryable"], false);
        assert!(!output.contains("token-secret-sentinel"));
        assert!(!output.contains("query-user-secret-sentinel"));
        assert!(!output.contains("body-secret-sentinel"));
        assert!(!output.contains("Authorization"));
        assert!(!output.contains("Bearer"));
        assert!(!output.contains("username"));
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
        assert!(DirectoryFailure::token(&auth::ServiceTokenClientError::TokenExchange).retryable);
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
