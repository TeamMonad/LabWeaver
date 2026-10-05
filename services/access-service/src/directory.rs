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

#[derive(Debug, Error)]
pub(crate) enum DirectoryError {
    #[error("directory endpoint is unavailable")]
    Unavailable,
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
            .map_err(|_| DirectoryError::Unavailable)?;
        let response = self
            .client
            .get(self.users_endpoint.clone())
            .query(params)
            .bearer_auth(token)
            .send()
            .await
            .map_err(|_| DirectoryError::Unavailable)?;
        match response.status() {
            reqwest::StatusCode::OK => response
                .json::<Vec<KeycloakUser>>()
                .await
                .map_err(|_| DirectoryError::Unavailable),
            reqwest::StatusCode::NOT_FOUND => Err(DirectoryError::Unavailable),
            reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN => {
                Err(DirectoryError::Unavailable)
            }
            status if status.is_server_error() => Err(DirectoryError::Unavailable),
            _ => Err(DirectoryError::Unavailable),
        }
    }
}

fn display_name(user: &KeycloakUser, username: &str) -> String {
    let given = user.first_name.as_deref().unwrap_or("").trim();
    let family = user.last_name.as_deref().unwrap_or("").trim();
    let display_name = match (given.is_empty(), family.is_empty()) {
        (false, false) => format!("{given} {family}"),
        (false, true) => given.to_owned(),
        (true, false) => family.to_owned(),
        (true, true) => username.to_owned(),
    };
    display_name
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
    use super::{DirectoryError, KeycloakUser, select_exact_user};

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
    fn exact_username_resolution_accepts_case_insensitive_single_result() {
        let result = select_exact_user(vec![user("Alice")], "alice")
            .expect("one exact provider result should resolve");
        assert_eq!(result.username.as_deref(), Some("Alice"));
    }
}
