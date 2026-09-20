//! Digest-preserving publication of a verified OCI image to the platform registry.
//!
//! The build executor remains the only authority that may push a built image. After
//! [`crate::oci_import::parse_oci_layout`] has verified the sandbox export, this module uploads the
//! exact blobs and manifest by their content digests and reads the manifest back from the registry
//! so a tag or a mutable reference can never become the runtime identity.

use reqwest::{
    Client, Method, StatusCode, Url,
    header::{ACCEPT, CONTENT_TYPE, HeaderValue, LOCATION},
};
use thiserror::Error;

use persistence_sqlx::Sha256Digest;

use crate::oci_import::{MANIFEST_MEDIA_TYPES, OciBlob, OciImage};

const BLOB_MEDIA_TYPE: &str = "application/octet-stream";
const ACCEPTED_MANIFEST_TYPES: &str = "application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.v2+json";

/// Registry credentials scoped to one project robot account.
#[derive(Clone)]
pub struct RegistryCredentials {
    pub username: String,
    pub password: String,
}

impl std::fmt::Debug for RegistryCredentials {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("RegistryCredentials([REDACTED])")
    }
}

/// Immutable identity resolved from one mutable tag reference.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedRegistryImage {
    /// Confirmed manifest digest; the only identity persisted downstream.
    pub digest: String,
    /// Manifest media type observed at resolution time.
    pub media_type: String,
    /// Sum of the config and layer sizes.
    pub size_bytes: u64,
}

/// Stable fail-closed publication errors.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum OciRegistryError {
    /// Endpoint, repository or credentials are invalid.
    #[error("LW_AGENT_OCI_REGISTRY_CONFIG_INVALID")]
    Configuration,
    /// The registry rejected the credentials or the request.
    #[error("LW_AGENT_OCI_REGISTRY_DENIED")]
    Denied,
    /// The registry returned an unexpected response or upload location.
    #[error("LW_AGENT_OCI_REGISTRY_REJECTED")]
    Rejected,
    /// The transport failed or the endpoint is unavailable.
    #[error("LW_AGENT_OCI_REGISTRY_UNAVAILABLE")]
    Unavailable,
    /// The published manifest readback does not match the verified digest.
    #[error("LW_AGENT_OCI_REGISTRY_DIGEST_MISMATCH")]
    DigestMismatch,
}

/// Digest-preserving publisher for one repository.
#[derive(Clone)]
pub struct OciRegistryPublisher {
    base: Url,
    repository: String,
    client: Client,
    credentials: RegistryCredentials,
}

impl std::fmt::Debug for OciRegistryPublisher {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OciRegistryPublisher")
            .field("base", &self.base)
            .field("repository", &self.repository)
            .finish_non_exhaustive()
    }
}

impl OciRegistryPublisher {
    /// Builds a strict HTTPS publisher for one repository.
    ///
    /// # Errors
    ///
    /// Returns [`OciRegistryError::Configuration`] for a non-HTTPS endpoint, an unsafe repository
    /// name or empty credentials.
    pub fn new(
        base: Url,
        repository: impl Into<String>,
        client: Client,
        credentials: RegistryCredentials,
    ) -> Result<Self, OciRegistryError> {
        let publisher = Self {
            base,
            repository: repository.into(),
            client,
            credentials,
        };
        publisher.validate()?;
        Ok(publisher)
    }

    /// Builds a publisher for local contract tests that does not require HTTPS.
    #[doc(hidden)]
    #[must_use]
    pub fn for_test(
        base: Url,
        repository: impl Into<String>,
        client: Client,
        credentials: RegistryCredentials,
    ) -> Self {
        Self {
            base,
            repository: repository.into(),
            client,
            credentials,
        }
    }

    fn validate(&self) -> Result<(), OciRegistryError> {
        if self.base.scheme() != "https"
            || self.base.host_str().is_none()
            || !self.base.username().is_empty()
            || self.base.password().is_some()
            || self.base.query().is_some()
            || self.base.fragment().is_some()
            || !valid_repository(&self.repository)
            || self.credentials.username.is_empty()
            || self.credentials.password.is_empty()
        {
            return Err(OciRegistryError::Configuration);
        }
        Ok(())
    }

    /// Uploads every blob and the manifest, then confirms the digest readback.
    ///
    /// # Errors
    ///
    /// Returns the stable registry failure; the image is never registered as published.
    pub async fn publish(&self, image: &OciImage) -> Result<String, OciRegistryError> {
        for blob in &image.blobs {
            self.ensure_blob(blob).await?;
        }
        self.put_manifest(
            &image.manifest_digest,
            &image.manifest_media_type,
            &image.manifest_bytes,
        )
        .await?;
        self.verify_manifest(&image.manifest_digest).await
    }

    async fn ensure_blob(&self, blob: &OciBlob) -> Result<(), OciRegistryError> {
        let path = format!("v2/{}/blobs/{}", self.repository, blob.digest);
        let response = self
            .request(Method::HEAD, &path)
            .send()
            .await
            .map_err(|_| OciRegistryError::Unavailable)?;
        match response.status() {
            StatusCode::OK => Ok(()),
            StatusCode::NOT_FOUND => self.upload_blob(blob).await,
            status if denied(status) => Err(OciRegistryError::Denied),
            _ => Err(OciRegistryError::Rejected),
        }
    }

    async fn upload_blob(&self, blob: &OciBlob) -> Result<(), OciRegistryError> {
        let start_path = format!("v2/{}/blobs/uploads/", self.repository);
        let response = self
            .request(Method::POST, &start_path)
            .send()
            .await
            .map_err(|_| OciRegistryError::Unavailable)?;
        let status = response.status();
        if denied(status) {
            return Err(OciRegistryError::Denied);
        }
        if status != StatusCode::ACCEPTED {
            return Err(OciRegistryError::Rejected);
        }
        let location = response
            .headers()
            .get(LOCATION)
            .and_then(|value| value.to_str().ok())
            .ok_or(OciRegistryError::Rejected)?;
        let location = self
            .base
            .join(location)
            .map_err(|_| OciRegistryError::Rejected)?;
        if location.scheme() != self.base.scheme() || location.host_str() != self.base.host_str() {
            return Err(OciRegistryError::Rejected);
        }
        let separator = if location.query().is_some() { '&' } else { '?' };
        let upload = format!("{location}{separator}digest={}", blob.digest);
        let response = self
            .request(Method::PUT, upload.as_str())
            .header(CONTENT_TYPE, HeaderValue::from_static(BLOB_MEDIA_TYPE))
            .body(blob.bytes.clone())
            .send()
            .await
            .map_err(|_| OciRegistryError::Unavailable)?;
        let status = response.status();
        if denied(status) {
            return Err(OciRegistryError::Denied);
        }
        if status != StatusCode::CREATED {
            return Err(OciRegistryError::Rejected);
        }
        Ok(())
    }

    async fn put_manifest(
        &self,
        digest: &str,
        media_type: &str,
        bytes: &[u8],
    ) -> Result<(), OciRegistryError> {
        let path = format!("v2/{}/manifests/{}", self.repository, digest);
        let content_type =
            HeaderValue::from_str(media_type).map_err(|_| OciRegistryError::Configuration)?;
        let response = self
            .request(Method::PUT, &path)
            .header(CONTENT_TYPE, content_type)
            .body(bytes.to_vec())
            .send()
            .await
            .map_err(|_| OciRegistryError::Unavailable)?;
        let status = response.status();
        if denied(status) {
            return Err(OciRegistryError::Denied);
        }
        if status != StatusCode::CREATED {
            return Err(OciRegistryError::Rejected);
        }
        Ok(())
    }

    /// Resolves one tag reference into the immutable manifest identity it currently points at.
    ///
    /// The administrator enters a tag; only the digest returned here is persisted and used
    /// downstream. Multi-architecture indexes are rejected so a pin is always one concrete
    /// image manifest.
    ///
    /// # Errors
    ///
    /// Returns the stable registry failure; an index, an unparsable manifest or a digest the
    /// registry does not confirm is [`OciRegistryError::Rejected`] or
    /// [`OciRegistryError::DigestMismatch`].
    pub async fn resolve_tag(
        &self,
        reference: &str,
    ) -> Result<ResolvedRegistryImage, OciRegistryError> {
        let (tag, declared_digest) = match reference.split_once('@') {
            Some((tag, digest)) => (tag, Some(validate_declared_digest(digest)?)),
            None => (reference, None),
        };
        if !valid_tag(tag) {
            return Err(OciRegistryError::Configuration);
        }
        let path = format!("v2/{}/manifests/{tag}", self.repository);
        let response = self
            .request(Method::GET, &path)
            .header(ACCEPT, HeaderValue::from_static(ACCEPTED_MANIFEST_TYPES))
            .send()
            .await
            .map_err(|_| OciRegistryError::Unavailable)?;
        let status = response.status();
        if denied(status) {
            return Err(OciRegistryError::Denied);
        }
        if status != StatusCode::OK {
            return Err(OciRegistryError::Rejected);
        }
        let media_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .ok_or(OciRegistryError::Rejected)?
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_owned();
        if !MANIFEST_MEDIA_TYPES.contains(&media_type.as_str()) {
            return Err(OciRegistryError::Rejected);
        }
        let observed_header = response
            .headers()
            .get("docker-content-digest")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let body = response
            .bytes()
            .await
            .map_err(|_| OciRegistryError::Unavailable)?;
        let observed_digest = format!("sha256:{}", Sha256Digest::of_bytes(&body));
        if observed_header.is_some_and(|header| header != observed_digest) {
            return Err(OciRegistryError::DigestMismatch);
        }
        if declared_digest.is_some_and(|declared| declared != observed_digest) {
            return Err(OciRegistryError::DigestMismatch);
        }
        let manifest: serde_json::Value =
            serde_json::from_slice(&body).map_err(|_| OciRegistryError::Rejected)?;
        if manifest.get("manifests").is_some() {
            return Err(OciRegistryError::Rejected);
        }
        let config_size = manifest
            .pointer("/config/size")
            .and_then(serde_json::Value::as_u64)
            .ok_or(OciRegistryError::Rejected)?;
        let layers = manifest
            .get("layers")
            .and_then(serde_json::Value::as_array)
            .ok_or(OciRegistryError::Rejected)?;
        let layer_bytes = layers.iter().try_fold(0_u64, |total, layer| {
            total.checked_add(layer.get("size").and_then(serde_json::Value::as_u64)?)
        });
        let size_bytes = config_size
            .checked_add(layer_bytes.ok_or(OciRegistryError::Rejected)?)
            .ok_or(OciRegistryError::Rejected)?;
        if size_bytes == 0 {
            return Err(OciRegistryError::Rejected);
        }
        Ok(ResolvedRegistryImage {
            digest: observed_digest,
            media_type,
            size_bytes,
        })
    }

    async fn verify_manifest(&self, digest: &str) -> Result<String, OciRegistryError> {
        let path = format!("v2/{}/manifests/{}", self.repository, digest);
        let response = self
            .request(Method::GET, &path)
            .header(ACCEPT, HeaderValue::from_static(ACCEPTED_MANIFEST_TYPES))
            .send()
            .await
            .map_err(|_| OciRegistryError::Unavailable)?;
        let status = response.status();
        if denied(status) {
            return Err(OciRegistryError::Denied);
        }
        if status != StatusCode::OK {
            return Err(OciRegistryError::Rejected);
        }
        let observed = response
            .headers()
            .get("docker-content-digest")
            .and_then(|value| value.to_str().ok())
            .ok_or(OciRegistryError::DigestMismatch)?;
        if observed != digest {
            return Err(OciRegistryError::DigestMismatch);
        }
        Ok(observed.to_owned())
    }

    fn request(&self, method: Method, target: &str) -> reqwest::RequestBuilder {
        let url = if target.starts_with("http") {
            target.to_owned()
        } else {
            self.base
                .join(target)
                .map_or_else(|_| target.to_owned(), |url| url.to_string())
        };
        self.client
            .request(method, url)
            .basic_auth(&self.credentials.username, Some(&self.credentials.password))
    }
}

fn denied(status: StatusCode) -> bool {
    status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN
}

fn valid_tag(value: &str) -> bool {
    let mut bytes = value.bytes();
    match bytes.next() {
        Some(byte) if byte.is_ascii_alphanumeric() || byte == b'_' => {}
        _ => return false,
    }
    value.len() <= 128
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn validate_declared_digest(value: &str) -> Result<String, OciRegistryError> {
    let Some(hex) = value.strip_prefix("sha256:") else {
        return Err(OciRegistryError::Configuration);
    };
    if hex.len() == 64
        && hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(value.to_owned())
    } else {
        Err(OciRegistryError::Configuration)
    }
}

fn valid_repository(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 255
        && value.split('/').all(|segment| {
            !segment.is_empty()
                && segment.len() <= 63
                && segment.bytes().all(|byte| {
                    byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || matches!(byte, b'-' | b'_' | b'.')
                })
                && !segment.starts_with(['.', '-', '_'])
                && !segment.ends_with(['.', '-', '_'])
        })
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use axum::{
        Router,
        body::Bytes,
        extract::{Query, State},
        http::{StatusCode, header},
        response::{IntoResponse, Response},
    };
    use reqwest::Client;

    use crate::oci_import::{OciBlob, OciImage};

    use super::{OciRegistryError, OciRegistryPublisher, RegistryCredentials};

    #[derive(Default)]
    struct RegistryState {
        blobs: HashMap<String, Vec<u8>>,
        manifest: Option<(String, Vec<u8>)>,
        uploads: u32,
        manifest_reads: u32,
        digest_header: Option<String>,
    }

    async fn handle(
        State(state): State<Arc<Mutex<RegistryState>>>,
        method: axum::http::Method,
        uri: axum::http::Uri,
        Query(params): Query<HashMap<String, String>>,
        body: Bytes,
    ) -> Response {
        let path = uri.path().to_owned();
        let method = method.as_str();
        if method == "HEAD"
            && path.contains("/blobs/")
            && !path.contains("/blobs/uploads/")
            && let Some(digest) = path.rsplit('/').next()
        {
            let state = state.lock().expect("state lock");
            return if state.blobs.contains_key(digest) {
                StatusCode::OK.into_response()
            } else {
                StatusCode::NOT_FOUND.into_response()
            };
        }
        if method == "POST" && path.ends_with("/blobs/uploads/") {
            let mut state = state.lock().expect("state lock");
            state.uploads += 1;
            let location = format!("{path}session-{}", state.uploads);
            return (StatusCode::ACCEPTED, [(header::LOCATION, location)]).into_response();
        }
        if method == "PUT" && path.contains("/blobs/uploads/") {
            let Some(declared) = params.get("digest") else {
                return StatusCode::BAD_REQUEST.into_response();
            };
            if body.is_empty() {
                return StatusCode::BAD_REQUEST.into_response();
            }
            let digest = format!("sha256:{}", persistence_sqlx::Sha256Digest::of_bytes(&body));
            if digest != *declared {
                return StatusCode::BAD_REQUEST.into_response();
            }
            let mut state = state.lock().expect("state lock");
            state.blobs.insert(digest, body.to_vec());
            return StatusCode::CREATED.into_response();
        }
        if method == "PUT" && path.contains("/manifests/") {
            let reference = path.rsplit('/').next().unwrap_or_default().to_owned();
            let mut state = state.lock().expect("state lock");
            state.manifest = Some((reference, body.to_vec()));
            return StatusCode::CREATED.into_response();
        }
        if method == "GET" && path.contains("/manifests/") {
            let reference = path.rsplit('/').next().unwrap_or_default().to_owned();
            let mut state = state.lock().expect("state lock");
            state.manifest_reads += 1;
            let Some((stored_reference, bytes)) = state.manifest.clone() else {
                return StatusCode::NOT_FOUND.into_response();
            };
            if stored_reference != reference {
                return StatusCode::NOT_FOUND.into_response();
            }
            let digest = state
                .digest_header
                .clone()
                .unwrap_or_else(|| stored_reference.clone());
            return (
                StatusCode::OK,
                [
                    (
                        header::HeaderName::from_static("docker-content-digest"),
                        digest,
                    ),
                    (
                        header::CONTENT_TYPE,
                        "application/vnd.oci.image.manifest.v1+json".to_owned(),
                    ),
                ],
                bytes,
            )
                .into_response();
        }
        StatusCode::NOT_FOUND.into_response()
    }

    async fn registry() -> (Arc<Mutex<RegistryState>>, UrlString) {
        let state = Arc::new(Mutex::new(RegistryState::default()));
        let router = Router::new()
            .route("/v2/{*path}", axum::routing::any(handle))
            .with_state(Arc::clone(&state));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fake registry");
        let address = listener.local_addr().expect("address");
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        (state, UrlString(format!("http://{address}/")))
    }

    struct UrlString(String);

    fn image() -> OciImage {
        let config = br#"{"architecture":"amd64"}"#.to_vec();
        let layer = b"layer".to_vec();
        let config_digest = format!(
            "sha256:{}",
            persistence_sqlx::Sha256Digest::of_bytes(&config)
        );
        let layer_digest = format!(
            "sha256:{}",
            persistence_sqlx::Sha256Digest::of_bytes(&layer)
        );
        let manifest = serde_json::json!({
            "schemaVersion": 2,
            "config": {
                "mediaType": "application/vnd.oci.image.config.v1+json",
                "digest": config_digest,
                "size": config.len(),
            },
            "layers": [{
                "mediaType": "application/vnd.oci.image.layer.v1.tar",
                "digest": layer_digest,
                "size": layer.len(),
            }],
        });
        let manifest_bytes = serde_json::to_vec(&manifest).expect("manifest");
        let manifest_digest = format!(
            "sha256:{}",
            persistence_sqlx::Sha256Digest::of_bytes(&manifest_bytes)
        );
        OciImage {
            manifest_digest,
            manifest_media_type: "application/vnd.oci.image.manifest.v1+json".to_owned(),
            manifest_bytes,
            config_digest,
            blobs: vec![
                OciBlob {
                    digest: format!(
                        "sha256:{}",
                        persistence_sqlx::Sha256Digest::of_bytes(&config)
                    ),
                    media_type: "application/vnd.oci.image.config.v1+json".to_owned(),
                    bytes: config,
                },
                OciBlob {
                    digest: layer_digest,
                    media_type: "application/vnd.oci.image.layer.v1.tar".to_owned(),
                    bytes: layer,
                },
            ],
        }
    }

    #[tokio::test]
    async fn resolve_tag_pins_the_current_manifest_digest() -> Result<(), Box<dyn std::error::Error>>
    {
        let (state, base) = registry().await;
        let image = image();
        let digest = {
            let mut state = state.lock().expect("state lock");
            state.manifest = Some(("24.04".to_owned(), image.manifest_bytes.clone()));
            state.digest_header = Some(image.manifest_digest.clone());
            image.manifest_digest.clone()
        };
        let publisher = OciRegistryPublisher::for_test(
            reqwest::Url::parse(&base.0)?,
            "labweaver-system/platform-build",
            Client::builder().no_proxy().build()?,
            RegistryCredentials {
                username: "robot$build".to_owned(),
                password: "secret".to_owned(),
            },
        );
        let resolved = publisher.resolve_tag("24.04").await?;
        assert_eq!(resolved.digest, digest);
        assert_eq!(
            resolved.media_type,
            "application/vnd.oci.image.manifest.v1+json"
        );
        assert_eq!(resolved.size_bytes, 29);
        assert_eq!(
            publisher
                .resolve_tag(&format!("24.04@{digest}"))
                .await?
                .digest,
            digest
        );
        assert!(matches!(
            publisher
                .resolve_tag(&format!("24.04@sha256:{}", "a".repeat(64)))
                .await,
            Err(OciRegistryError::DigestMismatch)
        ));
        assert!(matches!(
            publisher.resolve_tag("not a tag").await,
            Err(OciRegistryError::Configuration)
        ));
        Ok(())
    }

    #[tokio::test]
    async fn publish_uploads_blobs_then_manifest_and_verifies_readback()
    -> Result<(), Box<dyn std::error::Error>> {
        let (state, base) = registry().await;
        let publisher = OciRegistryPublisher::for_test(
            reqwest::Url::parse(&base.0)?,
            "labweaver-system/platform-build",
            Client::builder().no_proxy().build()?,
            RegistryCredentials {
                username: "robot$build".to_owned(),
                password: "secret".to_owned(),
            },
        );
        let image = image();
        let digest = publisher.publish(&image).await?;
        assert_eq!(digest, image.manifest_digest);
        let state = state.lock().expect("state lock");
        assert_eq!(state.uploads, 2);
        assert_eq!(state.blobs.len(), 2);
        assert_eq!(state.manifest_reads, 1);
        assert_eq!(
            state
                .manifest
                .as_ref()
                .map(|(reference, _)| reference.clone()),
            Some(image.manifest_digest)
        );
        Ok(())
    }

    #[tokio::test]
    async fn publish_skips_existing_blobs_and_rejects_digest_drift()
    -> Result<(), Box<dyn std::error::Error>> {
        let (state, base) = registry().await;
        let image = image();
        {
            let mut state = state.lock().expect("state lock");
            for blob in &image.blobs {
                state.blobs.insert(blob.digest.clone(), blob.bytes.clone());
            }
        }
        let publisher = OciRegistryPublisher::for_test(
            reqwest::Url::parse(&base.0)?,
            "labweaver-system/platform-build",
            Client::builder().no_proxy().build()?,
            RegistryCredentials {
                username: "robot$build".to_owned(),
                password: "secret".to_owned(),
            },
        );
        publisher.publish(&image).await?;
        assert_eq!(state.lock().expect("state lock").uploads, 0);

        {
            let mut state = state.lock().expect("state lock");
            state.digest_header = Some("sha256:".to_owned() + &"0".repeat(64));
        }
        assert_eq!(
            publisher.publish(&image).await.err(),
            Some(OciRegistryError::DigestMismatch)
        );
        Ok(())
    }
}
