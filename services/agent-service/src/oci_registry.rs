//! Digest-preserving publication of a verified OCI image to the platform registry.
//!
//! The build executor remains the only authority that may push a built image. After
//! [`crate::oci_import::parse_oci_layout`] has verified the sandbox export, this module uploads the
//! exact blobs and manifest by their content digests and reads the manifest back from the registry
//! so a tag or a mutable reference can never become the runtime identity.

use std::{sync::Arc, time::Instant};

use http_auth::parser::ChallengeParser;
use reqwest::{
    Body, Client, Method, StatusCode, Url,
    header::{ACCEPT, CONTENT_TYPE, HeaderName, HeaderValue, LOCATION, WWW_AUTHENTICATE},
};
use serde::Deserialize;
use sha2::Digest;
use tempfile::TempPath;
use thiserror::Error;
use tokio::io::AsyncReadExt;
use tokio_util::io::ReaderStream;

use persistence_sqlx::Sha256Digest;

use crate::oci_import::{MANIFEST_MEDIA_TYPES, OciBlob, OciImage};

/// One verified OCI blob stored in a temporary file instead of a process-memory buffer.
pub struct OciFileBlob {
    pub digest: String,
    pub media_type: String,
    pub size_bytes: u64,
    pub path: TempPath,
}

impl OciFileBlob {
    fn path(&self) -> &std::path::Path {
        self.path.as_ref()
    }
}

impl std::fmt::Debug for OciFileBlob {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OciFileBlob")
            .field("digest", &self.digest)
            .field("media_type", &self.media_type)
            .field("size_bytes", &self.size_bytes)
            .field("path", &self.path)
            .finish()
    }
}

/// Verified OCI image whose large blobs are retained in temporary files.
pub struct OciFileImage {
    pub manifest_digest: String,
    pub manifest_media_type: String,
    pub manifest_bytes: Vec<u8>,
    pub blobs: Vec<OciFileBlob>,
}

impl std::fmt::Debug for OciFileImage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OciFileImage")
            .field("manifest_digest", &self.manifest_digest)
            .field("manifest_media_type", &self.manifest_media_type)
            .field("manifest_bytes", &self.manifest_bytes.len())
            .field("blobs", &self.blobs)
            .finish()
    }
}

const BLOB_MEDIA_TYPE: &str = "application/octet-stream";
// A file upload can span several GiB. Metadata retains the client's short timeout;
// this one streaming request has a bounded budget and remains cancellable by future drop.
const FILE_BLOB_UPLOAD_TIMEOUT: std::time::Duration = std::time::Duration::from_mins(15);
const MAX_TOKEN_BYTES: usize = 64 * 1024;
const ACCEPTED_MANIFEST_TYPES: &str = "application/vnd.oci.image.index.v1+json, application/vnd.docker.distribution.manifest.list.v2+json, application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.v2+json";

/// Multi-platform indexes the registry may return for a tag.
///
/// A reviewed seed names one platform image, so an index is followed to its
/// `linux/amd64` entry instead of being rejected: Harbor answers a tag whose
/// accept header omits these types with `MANIFEST_UNKNOWN`.
const INDEX_MEDIA_TYPES: [&str; 2] = [
    "application/vnd.oci.image.index.v1+json",
    "application/vnd.docker.distribution.manifest.list.v2+json",
];

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
    bearer: Arc<tokio::sync::Mutex<Option<BearerAuthorization>>>,
}

#[derive(Clone)]
struct BearerChallenge {
    realm: Url,
    service: Option<String>,
}

// Authorization material stays within this repository's publisher and is never Debug/logged.
#[derive(Clone)]
struct BearerAuthorization {
    challenge: BearerChallenge,
    token: String,
    expires_at: Instant,
    push: bool,
}

#[derive(Deserialize)]
struct TokenResponse {
    token: Option<String>,
    access_token: Option<String>,
    expires_in: Option<u64>,
    issued_at: Option<String>,
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
            bearer: Arc::new(tokio::sync::Mutex::new(None)),
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
            bearer: Arc::new(tokio::sync::Mutex::new(None)),
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
        self.read_manifest_digest(&image.manifest_digest, &image.manifest_digest)
            .await
    }

    /// Uploads a verified file-backed OCI image without buffering its blobs in memory.
    pub async fn publish_file(&self, image: &OciFileImage) -> Result<String, OciRegistryError> {
        for blob in &image.blobs {
            self.ensure_file_blob(blob).await?;
        }
        self.put_manifest(
            &image.manifest_digest,
            &image.manifest_media_type,
            &image.manifest_bytes,
        )
        .await?;
        self.read_manifest_digest(&image.manifest_digest, &image.manifest_digest)
            .await
    }

    /// Publishes the manifest under one mutable tag so the reviewed reference stays resolvable.
    ///
    /// The immutable identity is still the digest: the tag is written last and only after the
    /// registry has confirmed the exact manifest the caller verified.
    ///
    /// # Errors
    ///
    /// Returns [`OciRegistryError::Configuration`] for an unusable tag and the mapped registry
    /// failure otherwise.
    pub async fn tag(&self, tag: &str, image: &OciImage) -> Result<String, OciRegistryError> {
        if !valid_tag(tag) {
            return Err(OciRegistryError::Configuration);
        }
        self.put_manifest(tag, &image.manifest_media_type, &image.manifest_bytes)
            .await?;
        self.read_manifest_digest(tag, &image.manifest_digest).await
    }

    /// Publishes a file-backed image under one mutable tag after digest publication succeeds.
    pub async fn tag_file(
        &self,
        tag: &str,
        image: &OciFileImage,
    ) -> Result<String, OciRegistryError> {
        if !valid_tag(tag) {
            return Err(OciRegistryError::Configuration);
        }
        self.put_manifest(tag, &image.manifest_media_type, &image.manifest_bytes)
            .await?;
        self.read_manifest_digest(tag, &image.manifest_digest).await
    }

    async fn ensure_blob(&self, blob: &OciBlob) -> Result<(), OciRegistryError> {
        let path = format!("v2/{}/blobs/{}", self.repository, blob.digest);
        let response = self.send(Method::HEAD, &path, &[], None, true).await?;
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
            .send(Method::POST, &start_path, &[], None, true)
            .await?;
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
        if !self.same_origin(&location) {
            return Err(OciRegistryError::Rejected);
        }
        let separator = if location.query().is_some() { '&' } else { '?' };
        let upload = format!("{location}{separator}digest={}", blob.digest);
        let response = self
            .request(Method::PUT, upload.as_str(), true)
            .await?
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

    async fn ensure_file_blob(&self, blob: &OciFileBlob) -> Result<(), OciRegistryError> {
        verify_file_blob(blob).await?;
        let path = format!("v2/{}/blobs/{}", self.repository, blob.digest);
        let response = self.send(Method::HEAD, &path, &[], None, true).await?;
        match response.status() {
            StatusCode::OK => Ok(()),
            StatusCode::NOT_FOUND => self.upload_file_blob(blob).await,
            status if denied(status) => Err(OciRegistryError::Denied),
            _ => Err(OciRegistryError::Rejected),
        }
    }

    async fn upload_file_blob(&self, blob: &OciFileBlob) -> Result<(), OciRegistryError> {
        let start_path = format!("v2/{}/blobs/uploads/", self.repository);
        let response = self
            .send(Method::POST, &start_path, &[], None, true)
            .await?;
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
        if !self.same_origin(&location) {
            return Err(OciRegistryError::Rejected);
        }
        let separator = if location.query().is_some() { '&' } else { '?' };
        let upload = format!("{location}{separator}digest={}", blob.digest);
        // Authenticate before opening the stream. A rejected PUT is terminal: a consumed
        // multi-gigabyte request body is never cloned, buffered or transparently replayed.
        let request = self.request(Method::PUT, upload.as_str(), true).await?;
        let file = tokio::fs::File::open(blob.path())
            .await
            .map_err(|_| OciRegistryError::Unavailable)?;
        let content_length = HeaderValue::from_str(&blob.size_bytes.to_string())
            .map_err(|_| OciRegistryError::Configuration)?;
        let response = request
            .timeout(FILE_BLOB_UPLOAD_TIMEOUT)
            .header(CONTENT_TYPE, HeaderValue::from_static(BLOB_MEDIA_TYPE))
            .header(reqwest::header::CONTENT_LENGTH, content_length)
            .body(Body::wrap_stream(ReaderStream::new(file)))
            .send()
            .await
            .map_err(|_| OciRegistryError::Unavailable)?;
        let status = response.status();
        trace_registry_response("blob_upload_commit", status);
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
            .send(
                Method::PUT,
                &path,
                &[(CONTENT_TYPE, content_type)],
                Some(bytes),
                true,
            )
            .await?;
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
        // A tag may resolve to a multi-platform index; follow it to the
        // `linux/amd64` entry once, then read that single-platform manifest.
        let mut target = tag.to_owned();
        for _ in 0..2 {
            let path = format!("v2/{}/manifests/{target}", self.repository);
            let response = self
                .send(
                    Method::GET,
                    &path,
                    &[(ACCEPT, HeaderValue::from_static(ACCEPTED_MANIFEST_TYPES))],
                    None,
                    false,
                )
                .await?;
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
            let manifest: serde_json::Value =
                serde_json::from_slice(&body).map_err(|_| OciRegistryError::Rejected)?;
            if INDEX_MEDIA_TYPES.contains(&media_type.as_str()) {
                target = index_platform_digest(&manifest)?;
                continue;
            }
            if !MANIFEST_MEDIA_TYPES.contains(&media_type.as_str()) {
                return Err(OciRegistryError::Rejected);
            }
            if declared_digest.is_some_and(|declared| declared != observed_digest) {
                return Err(OciRegistryError::DigestMismatch);
            }
            return finish_manifest(&manifest, &media_type, observed_digest);
        }
        Err(OciRegistryError::Rejected)
    }

    /// Reads one manifest back at `reference` and requires the registry to confirm `expected`.
    async fn read_manifest_digest(
        &self,
        reference: &str,
        expected: &str,
    ) -> Result<String, OciRegistryError> {
        let path = format!("v2/{}/manifests/{reference}", self.repository);
        let response = self
            .send(
                Method::GET,
                &path,
                &[(ACCEPT, HeaderValue::from_static(ACCEPTED_MANIFEST_TYPES))],
                None,
                true,
            )
            .await?;
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
        if observed != expected {
            return Err(OciRegistryError::DigestMismatch);
        }
        Ok(observed.to_owned())
    }

    async fn request(
        &self,
        method: Method,
        target: &str,
        push: bool,
    ) -> Result<reqwest::RequestBuilder, OciRegistryError> {
        let url = self
            .base
            .join(target)
            .map_err(|_| OciRegistryError::Configuration)?;
        if !self.same_origin(&url) {
            return Err(OciRegistryError::Rejected);
        }
        let request = self.client.request(method, url);
        let cached = self.bearer.lock().await.clone();
        if let Some(auth) = cached {
            let token = if auth.expires_at > Instant::now() && (!push || auth.push) {
                auth.token
            } else {
                self.fetch_bearer(&auth.challenge, push).await?
            };
            Ok(request.bearer_auth(token))
        } else {
            Ok(request.basic_auth(&self.credentials.username, Some(&self.credentials.password)))
        }
    }

    /// Only replayable metadata requests participate in a single challenge retry.
    async fn send(
        &self,
        method: Method,
        target: &str,
        headers: &[(HeaderName, HeaderValue)],
        body: Option<&[u8]>,
        push: bool,
    ) -> Result<reqwest::Response, OciRegistryError> {
        for attempt in 0..2 {
            let mut request = self.request(method.clone(), target, push).await?;
            for (name, value) in headers {
                request = request.header(name, value);
            }
            if let Some(body) = body {
                request = request.body(body.to_vec());
            }
            let response = request
                .send()
                .await
                .map_err(|_| OciRegistryError::Unavailable)?;
            if response.status() == StatusCode::UNAUTHORIZED && attempt == 0 {
                let Some(challenge) = self.bearer_challenge(response.headers())? else {
                    trace_registry_response(registry_stage(&method, target), response.status());
                    return Ok(response);
                };
                self.fetch_bearer(&challenge, push).await?;
                continue;
            }
            trace_registry_response(registry_stage(&method, target), response.status());
            return Ok(response);
        }
        Err(OciRegistryError::Denied)
    }

    fn same_origin(&self, url: &Url) -> bool {
        url.origin() == self.base.origin()
            && url.username().is_empty()
            && url.password().is_none()
            && url.fragment().is_none()
    }

    fn bearer_challenge(
        &self,
        headers: &reqwest::header::HeaderMap,
    ) -> Result<Option<BearerChallenge>, OciRegistryError> {
        let mut selected = None;
        for header in headers.get_all(WWW_AUTHENTICATE) {
            let value = header.to_str().map_err(|_| OciRegistryError::Rejected)?;
            for challenge in ChallengeParser::new(value) {
                let challenge = challenge.map_err(|_| OciRegistryError::Rejected)?;
                if !challenge.scheme.eq_ignore_ascii_case("bearer") {
                    continue;
                }
                if selected.is_some() {
                    return Err(OciRegistryError::Rejected);
                }
                let mut params = std::collections::BTreeMap::new();
                for (key, value) in challenge.params {
                    if params
                        .insert(key.to_ascii_lowercase(), value.to_unescaped())
                        .is_some()
                    {
                        return Err(OciRegistryError::Rejected);
                    }
                }
                let realm = Url::parse(params.get("realm").ok_or(OciRegistryError::Rejected)?)
                    .map_err(|_| OciRegistryError::Rejected)?;
                if realm.scheme() != "https" || !self.same_origin(&realm) || realm.query().is_some()
                {
                    return Err(OciRegistryError::Rejected);
                }
                if let Some(scope) = params.get("scope") {
                    for scope in scope.split_whitespace() {
                        let mut parts = scope.splitn(3, ':');
                        if parts.next() != Some("repository")
                            || parts.next() != Some(self.repository.as_str())
                            || parts.next().is_none_or(|actions| {
                                actions.is_empty()
                                    || actions
                                        .split(',')
                                        .any(|action| !matches!(action, "pull" | "push"))
                            })
                        {
                            return Err(OciRegistryError::Rejected);
                        }
                    }
                }
                let service = params.remove("service");
                if service.as_ref().is_some_and(|value| {
                    value.is_empty() || value.len() > 256 || value.chars().any(char::is_whitespace)
                }) {
                    return Err(OciRegistryError::Rejected);
                }
                selected = Some(BearerChallenge { realm, service });
            }
        }
        Ok(selected)
    }

    async fn fetch_bearer(
        &self,
        challenge: &BearerChallenge,
        push: bool,
    ) -> Result<String, OciRegistryError> {
        let mut realm = challenge.realm.clone();
        let actions = if push { "pull,push" } else { "pull" };
        {
            let mut query = realm.query_pairs_mut();
            if let Some(service) = &challenge.service {
                query.append_pair("service", service);
            }
            query.append_pair(
                "scope",
                &format!("repository:{}:{actions}", self.repository),
            );
        }
        let mut response = self
            .client
            .get(realm.clone())
            .basic_auth(&self.credentials.username, Some(&self.credentials.password))
            .send()
            .await
            .map_err(|_| OciRegistryError::Unavailable)?;
        trace_registry_response("token_exchange", response.status());
        if denied(response.status()) {
            return Err(OciRegistryError::Denied);
        }
        if response.status() != StatusCode::OK || response.url() != &realm {
            return Err(OciRegistryError::Rejected);
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_TOKEN_BYTES as u64)
        {
            return Err(OciRegistryError::Rejected);
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| OciRegistryError::Unavailable)?
        {
            if bytes.len().saturating_add(chunk.len()) > MAX_TOKEN_BYTES {
                return Err(OciRegistryError::Rejected);
            }
            bytes.extend_from_slice(&chunk);
        }
        let response: TokenResponse =
            serde_json::from_slice(&bytes).map_err(|_| OciRegistryError::Rejected)?;
        if let (Some(token), Some(access_token)) = (&response.token, &response.access_token)
            && token != access_token
        {
            return Err(OciRegistryError::Rejected);
        }
        let token = response
            .token
            .filter(|value| !value.is_empty())
            .or(response.access_token.filter(|value| !value.is_empty()))
            .ok_or(OciRegistryError::Denied)?;
        let now = time::OffsetDateTime::now_utc();
        let issued = response.issued_at.map_or(Ok(now), |value| {
            time::OffsetDateTime::parse(&value, &time::format_description::well_known::Rfc3339)
                .map_err(|_| OciRegistryError::Rejected)
        })?;
        if issued > now + time::Duration::seconds(30) {
            return Err(OciRegistryError::Rejected);
        }
        let lifetime = i64::try_from(response.expires_in.unwrap_or(60))
            .map_err(|_| OciRegistryError::Rejected)?;
        let expiry = issued
            .checked_add(time::Duration::seconds(lifetime))
            .ok_or(OciRegistryError::Rejected)?;
        if expiry <= now {
            return Err(OciRegistryError::Denied);
        }
        let remaining =
            std::time::Duration::try_from(expiry - now).map_err(|_| OciRegistryError::Rejected)?;
        let expires_at = Instant::now()
            .checked_add(remaining)
            .ok_or(OciRegistryError::Rejected)?;
        *self.bearer.lock().await = Some(BearerAuthorization {
            challenge: challenge.clone(),
            token: token.clone(),
            expires_at,
            push,
        });
        Ok(token)
    }
}

fn registry_stage(method: &Method, target: &str) -> &'static str {
    if target.contains("/blobs/uploads/") {
        "blob_upload_start"
    } else if method == Method::HEAD {
        "blob_exists"
    } else if method == Method::PUT {
        "manifest_put"
    } else {
        "manifest_get"
    }
}

fn trace_registry_response(stage: &'static str, status: StatusCode) {
    if !status.is_success() && status != StatusCode::NOT_FOUND {
        tracing::warn!(
            event = "agent.oci_registry.request_rejected",
            failure_stage = stage,
            http_status = status.as_u16()
        );
    }
}

/// Picks the `linux/amd64` manifest digest out of a multi-platform index.
fn index_platform_digest(index: &serde_json::Value) -> Result<String, OciRegistryError> {
    let entries = index
        .get("manifests")
        .and_then(serde_json::Value::as_array)
        .ok_or(OciRegistryError::Rejected)?;
    for entry in entries {
        let platform = entry.get("platform");
        let is_amd64_linux = platform
            .and_then(|value| value.get("os"))
            .and_then(serde_json::Value::as_str)
            == Some("linux")
            && platform
                .and_then(|value| value.get("architecture"))
                .and_then(serde_json::Value::as_str)
                == Some("amd64");
        if !is_amd64_linux {
            continue;
        }
        if let Some(digest) = entry.get("digest").and_then(serde_json::Value::as_str)
            && validate_declared_digest(digest).is_ok()
        {
            return Ok(digest.to_owned());
        }
    }
    Err(OciRegistryError::Rejected)
}

async fn verify_file_blob(blob: &OciFileBlob) -> Result<(), OciRegistryError> {
    let metadata = tokio::fs::metadata(blob.path())
        .await
        .map_err(|_| OciRegistryError::Unavailable)?;
    if metadata.len() != blob.size_bytes {
        return Err(OciRegistryError::DigestMismatch);
    }
    let mut file = tokio::fs::File::open(blob.path())
        .await
        .map_err(|_| OciRegistryError::Unavailable)?;
    let mut hasher = sha2::Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    let mut size = 0_u64;
    loop {
        let read = file
            .read(&mut buffer)
            .await
            .map_err(|_| OciRegistryError::Unavailable)?;
        if read == 0 {
            break;
        }
        size = size
            .checked_add(u64::try_from(read).map_err(|_| OciRegistryError::DigestMismatch)?)
            .ok_or(OciRegistryError::DigestMismatch)?;
        hasher.update(&buffer[..read]);
    }
    if size != blob.size_bytes {
        return Err(OciRegistryError::DigestMismatch);
    }
    let digest = format!("sha256:{:x}", hasher.finalize());
    if digest != blob.digest {
        return Err(OciRegistryError::DigestMismatch);
    }
    Ok(())
}

/// Reads one already fetched single-platform manifest into its resolved identity.
fn finish_manifest(
    manifest: &serde_json::Value,
    media_type: &str,
    observed_digest: String,
) -> Result<ResolvedRegistryImage, OciRegistryError> {
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
        media_type: media_type.to_owned(),
        size_bytes,
    })
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
#[path = "oci_registry_auth_tests.rs"]
#[allow(
    clippy::expect_used,
    reason = "fixture-only URLs and immutable protocol values are validated at construction"
)]
mod auth_tests;

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

    use super::{
        OciRegistryError, OciRegistryPublisher, RegistryCredentials, index_platform_digest,
    };

    #[test]
    fn index_resolution_picks_the_linux_amd64_entry() {
        let index = serde_json::json!({
            "manifests": [
                {
                    "digest": format!("sha256:{}", "a".repeat(64)),
                    "platform": {"os": "linux", "architecture": "arm64"},
                },
                {
                    "digest": format!("sha256:{}", "b".repeat(64)),
                    "platform": {"os": "linux", "architecture": "amd64"},
                },
            ],
        });
        assert_eq!(
            index_platform_digest(&index).expect("amd64 entry"),
            format!("sha256:{}", "b".repeat(64))
        );
    }

    #[test]
    fn index_resolution_rejects_an_index_without_linux_amd64() {
        let index = serde_json::json!({
            "manifests": [
                {
                    "digest": format!("sha256:{}", "c".repeat(64)),
                    "platform": {"os": "windows", "architecture": "amd64"},
                },
            ],
        });
        assert!(matches!(
            index_platform_digest(&index),
            Err(OciRegistryError::Rejected)
        ));
    }

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
            let computed = format!(
                "sha256:{}",
                persistence_sqlx::Sha256Digest::of_bytes(&bytes)
            );
            let digest = state.digest_header.clone().unwrap_or(computed);
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

    pub(super) fn image() -> OciImage {
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
    async fn tag_writes_the_reviewed_reference_and_confirms_the_digest()
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
        assert_eq!(publisher.tag("24.04", &image).await?, image.manifest_digest);
        assert_eq!(
            state
                .lock()
                .expect("state lock")
                .manifest
                .as_ref()
                .map(|(reference, _)| reference.clone()),
            Some("24.04".to_owned())
        );

        {
            let mut state = state.lock().expect("state lock");
            state.digest_header = Some(format!("sha256:{}", "0".repeat(64)));
        }
        assert_eq!(
            publisher.tag("24.04", &image).await.err(),
            Some(OciRegistryError::DigestMismatch)
        );
        assert_eq!(
            publisher.tag("not a tag", &image).await.err(),
            Some(OciRegistryError::Configuration)
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
