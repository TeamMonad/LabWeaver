//! Immutable S3-compatible object storage used for versioned platform artifacts.

use std::collections::BTreeMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use aws_config::BehaviorVersion;
use aws_credential_types::Credentials;
use aws_sdk_s3::Client;
use aws_sdk_s3::config::{Builder as S3ConfigBuilder, Region};
use aws_sdk_s3::error::{ProvideErrorMetadata, SdkError};
use aws_sdk_s3::presigning::PresigningConfig;
use aws_sdk_s3::primitives::{ByteStream, DateTime};
use aws_sdk_s3::types::{CompletedMultipartUpload, CompletedPart, ObjectLockMode};
use contracts::http::PLATFORM_IMAGE_UPLOAD_PART_SIZE_BYTES;
use contracts::{ArtifactId, ArtifactRef, UtcTimestamp};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use url::Url;

mod stream;
pub use stream::VerifiedObjectFile;

/// Non-secret S3 binding. Credentials are supplied separately from Secret locators.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct S3StoreConfig {
    /// Stable binding recorded in public artifact references.
    pub binding: String,
    /// Internal S3-compatible HTTPS endpoint.
    pub endpoint: Url,
    /// Bucket with versioning enabled.
    pub bucket: String,
    /// Signing region.
    pub region: String,
    /// Prefix reserved for one explicitly bound immutable artifact class.
    pub object_prefix: String,
    /// Maximum presigned upload lifetime.
    pub upload_ttl_seconds: u64,
    /// Maximum accepted object size.
    pub max_object_bytes: u64,
    /// `MinIO` and other S3-compatible deployments require path-style addressing.
    pub force_path_style: bool,
    /// Optional PEM trust-anchor file for the S3 endpoint's private CA.
    ///
    /// When set, the S3 client builds a TLS connector trusting exactly this
    /// file, bypassing platform native roots (which may not include the
    /// private CA on minimal runtime images).
    #[serde(default)]
    pub ca_bundle_file: Option<String>,
}

impl S3StoreConfig {
    /// Rejects incomplete, public, or unbounded storage configuration.
    ///
    /// # Errors
    ///
    /// Returns a stable configuration error for any unsafe binding.
    pub fn validate(&self) -> Result<(), ObjectStoreError> {
        if self.binding.trim().is_empty()
            || self.bucket.trim().is_empty()
            || self.region.trim().is_empty()
            || self.object_prefix.trim_matches('/').is_empty()
            || self.upload_ttl_seconds == 0
            || self.upload_ttl_seconds > 14_400
            || self.max_object_bytes == 0
            || self.endpoint.scheme() != "https"
            || self.endpoint.host_str().is_none()
        {
            return Err(ObjectStoreError::ConfigurationInvalid);
        }
        Ok(())
    }
}

/// Required deployment credential resolved outside checked-in configuration.
#[derive(Clone)]
pub struct S3Credential {
    /// Access key ID.
    pub access_key_id: String,
    /// Secret access key.
    pub secret_access_key: String,
    /// Optional short-lived session token.
    pub session_token: Option<String>,
}

impl std::fmt::Debug for S3Credential {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("S3Credential([REDACTED])")
    }
}

/// Immutable upload request signed for one exact object identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PresignedUpload {
    /// Signed URL.
    pub url: String,
    /// Headers that must be supplied byte-for-byte by the client.
    pub required_headers: BTreeMap<String, String>,
    /// Server-side expiry.
    pub expires_at: UtcTimestamp,
}

/// One presigned part upload for a platform image archive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PresignedPlatformImagePart {
    /// Consecutive multipart part number starting at one.
    pub part_number: u32,
    /// Signed URL for this exact part.
    pub url: String,
    /// Headers that must be supplied byte-for-byte by the client.
    pub required_headers: BTreeMap<String, String>,
    /// Server-side expiry shared with the upload session.
    pub expires_at: UtcTimestamp,
}

/// Server-side multipart upload authority for one platform image archive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlatformImageMultipartUpload {
    /// Opaque S3 multipart upload identity. It never crosses the HTTP contract.
    pub upload_id: String,
    /// Fixed part size used for every non-final part.
    pub part_size_bytes: u64,
    /// Number of parts required by the reviewed archive size.
    pub part_count: u32,
    /// Presigned part upload authorities.
    pub parts: Vec<PresignedPlatformImagePart>,
    /// Session expiry for all authorities.
    pub expires_at: UtcTimestamp,
}

/// A part reported by S3 for one active multipart upload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlatformImageMultipartPart {
    /// Consecutive multipart part number.
    pub part_number: u32,
    /// S3 `ETag` for the uploaded part.
    pub etag: String,
    /// Actual bytes stored by S3 for this part.
    pub size_bytes: u64,
}

/// Part identity supplied by Control when completing a platform image upload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlatformImageMultipartPartInput {
    /// Consecutive multipart part number.
    pub part_number: u32,
    /// `ETag` returned by the browser's successful part upload.
    pub etag: String,
}

/// Short-lived GET URL for one exact immutable object version.
///
/// The URL is intended for a per-attempt init container.  It contains the S3
/// authorization material, so callers must keep it in a Secret that is mounted
/// only by that init container and must never put it in a normal `ConfigMap` or
/// log record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PresignedDownload {
    /// Signed URL for the exact object version.
    pub url: String,
    /// Headers that must be supplied byte-for-byte by the client.
    pub required_headers: BTreeMap<String, String>,
    /// Server-side expiry.
    pub expires_at: UtcTimestamp,
}

/// Verified bytes and immutable S3 version identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedObject {
    /// Immutable public reference.
    pub reference: ArtifactRef,
    /// Bytes returned only to the deterministic LLM egress gate.
    pub bytes: Vec<u8>,
}

/// Storage boundary used by Control and Agent without exposing credentials.
#[async_trait]
pub trait ImmutableObjectStore: Send + Sync {
    /// Returns the deployment binding used in every public artifact reference.
    fn binding(&self) -> &str;

    /// Signs one conditional immutable upload.
    async fn presign_upload(
        &self,
        key: &str,
        size_bytes: u64,
        media_type: &str,
        now: UtcTimestamp,
    ) -> Result<PresignedUpload, ObjectStoreError>;

    /// Creates the fixed-size multipart upload used only for platform image archives.
    async fn create_platform_image_multipart_upload(
        &self,
        _key: &str,
        _size_bytes: u64,
        _media_type: &str,
        _now: UtcTimestamp,
    ) -> Result<PlatformImageMultipartUpload, ObjectStoreError> {
        Err(ObjectStoreError::StreamingUnsupported)
    }

    /// Reissues the remaining short-lived part authorities for an existing upload identity.
    async fn presign_platform_image_multipart_upload(
        &self,
        _key: &str,
        _upload_id: &str,
        _size_bytes: u64,
        _now: UtcTimestamp,
        _expires_at: UtcTimestamp,
    ) -> Result<PlatformImageMultipartUpload, ObjectStoreError> {
        Err(ObjectStoreError::StreamingUnsupported)
    }

    /// Finds active multipart uploads for one exact platform-image key during creation recovery.
    async fn find_platform_image_multipart_uploads(
        &self,
        _key: &str,
    ) -> Result<Vec<String>, ObjectStoreError> {
        Err(ObjectStoreError::StreamingUnsupported)
    }

    /// Lists the actual parts of one exact active multipart upload.
    async fn list_platform_image_multipart_parts(
        &self,
        _key: &str,
        _upload_id: &str,
    ) -> Result<Vec<PlatformImageMultipartPart>, ObjectStoreError> {
        Err(ObjectStoreError::StreamingUnsupported)
    }

    /// Verifies the exact S3 part manifest and completes one multipart upload.
    async fn complete_platform_image_multipart_upload(
        &self,
        _key: &str,
        _upload_id: &str,
        _size_bytes: u64,
        _parts: &[PlatformImageMultipartPartInput],
    ) -> Result<(), ObjectStoreError> {
        Err(ObjectStoreError::StreamingUnsupported)
    }

    /// Aborts one exact active multipart upload. Repeating the call is safe.
    async fn abort_platform_image_multipart_upload(
        &self,
        _key: &str,
        _upload_id: &str,
    ) -> Result<(), ObjectStoreError> {
        Err(ObjectStoreError::StreamingUnsupported)
    }

    /// Downloads one exact object version and verifies raw bytes.
    async fn read_verified(
        &self,
        key: &str,
        expected: &ArtifactRef,
    ) -> Result<VerifiedObject, ObjectStoreError>;

    /// Streams one large immutable object into a temporary file owned by the returned guard.
    ///
    /// Implementations that do not support file-backed reads retain the byte-oriented contract
    /// and return a stable unsupported error.
    async fn read_verified_file(
        &self,
        _key: &str,
        _expected: &ArtifactRef,
    ) -> Result<VerifiedObjectFile, ObjectStoreError> {
        Err(ObjectStoreError::StreamingUnsupported)
    }

    /// Resolves the current upload version once, then verifies and freezes that exact version.
    async fn freeze_current(
        &self,
        key: &str,
        expected_size: u64,
        media_type: &str,
    ) -> Result<VerifiedObject, ObjectStoreError>;

    /// Resolves and verifies the current object version without downloading its body.
    ///
    /// This is used by durable import workers that persist the immutable reference before doing
    /// the long-running Agent-side download. Implementations must verify size and media type from
    /// the versioned metadata before returning the reference.
    async fn freeze_current_reference(
        &self,
        _key: &str,
        _expected_size: u64,
        _media_type: &str,
    ) -> Result<ArtifactRef, ObjectStoreError> {
        Err(ObjectStoreError::StreamingUnsupported)
    }

    /// Resolves and streams one current version into a temporary file.
    async fn freeze_current_file(
        &self,
        _key: &str,
        _expected_size: u64,
        _media_type: &str,
    ) -> Result<VerifiedObjectFile, ObjectStoreError> {
        Err(ObjectStoreError::StreamingUnsupported)
    }

    /// Lists immutable versions and delete markers for one validated, exact object key.
    /// Implementations must filter returned keys for equality and follow all version pages.
    async fn list_key_versions(&self, _key: &str) -> Result<Vec<String>, ObjectStoreError> {
        Err(ObjectStoreError::StreamingUnsupported)
    }

    /// Writes bytes once under the validated retention decision and verifies the exact version.
    ///
    /// `Some(deadline)` uses Governance Object Lock. `None` is reserved for the already validated
    /// permanent `CourseMaterial` form and uses versioned conditional storage without Object Lock.
    async fn put_immutable(
        &self,
        _key: &str,
        _bytes: &[u8],
        _media_type: &str,
        _now: UtcTimestamp,
        _retain_until: Option<UtcTimestamp>,
    ) -> Result<VerifiedObject, ObjectStoreError> {
        Err(ObjectStoreError::ObjectLockRequired)
    }

    /// Deletes only a named orphan version; completed package versions are never passed here.
    async fn delete_orphan(&self, key: &str, version: &str) -> Result<(), ObjectStoreError>;
}

/// AWS SDK implementation configured for a private S3-compatible endpoint.
#[derive(Clone, Debug)]
pub struct S3ImmutableObjectStore {
    config: S3StoreConfig,
    client: Client,
}

impl S3ImmutableObjectStore {
    /// Builds the SDK client from validated configuration and externally resolved credentials.
    ///
    /// # Errors
    ///
    /// Returns a stable configuration error when the explicit binding is invalid.
    pub async fn new(
        config: S3StoreConfig,
        credential: S3Credential,
    ) -> Result<Self, ObjectStoreError> {
        config.validate()?;
        if credential.access_key_id.trim().is_empty() || credential.secret_access_key.is_empty() {
            return Err(ObjectStoreError::ConfigurationInvalid);
        }
        let credentials = Credentials::new(
            credential.access_key_id,
            credential.secret_access_key,
            credential.session_token,
            None,
            "labweaver-secret-locator",
        );
        let mut loader = aws_config::defaults(BehaviorVersion::latest())
            .region(Region::new(config.region.clone()))
            .credentials_provider(credentials)
            .endpoint_url(config.endpoint.as_str());
        if let Some(ca_file) = config.ca_bundle_file.as_deref() {
            let ca_pem =
                std::fs::read(ca_file).map_err(|_| ObjectStoreError::ConfigurationInvalid)?;
            let certs: Result<Vec<_>, _> = rustls_pemfile::certs(&mut &ca_pem[..]).collect();
            let certs = certs.map_err(|_| ObjectStoreError::ConfigurationInvalid)?;
            let mut roots = rustls::RootCertStore::empty();
            for cert in certs {
                let c = rustls::Certificate(cert.as_ref().to_vec());
                roots
                    .add(&c)
                    .map_err(|_| ObjectStoreError::ConfigurationInvalid)?;
            }
            let tls_config = rustls::ClientConfig::builder()
                .with_safe_defaults()
                .with_root_certificates(roots)
                .with_no_client_auth();
            let connector = hyper_rustls::HttpsConnectorBuilder::new()
                .with_tls_config(tls_config)
                .https_only()
                .enable_http1()
                .enable_http2()
                .build();
            let http_client =
                aws_smithy_runtime::client::http::hyper_014::HyperClientBuilder::new()
                    .build(connector);
            loader = loader.http_client(http_client);
        }
        let shared = loader.load().await;
        let service = S3ConfigBuilder::from(&shared)
            .force_path_style(config.force_path_style)
            .build();
        // Instrumentation: report the effective native trust store size so a
        // private CA mismatch is observable at startup instead of surfacing as
        // an opaque dispatch failure on the first GetObject.
        let native_roots_loaded = rustls_native_certs::load_native_certs()
            .map(|certs| certs.len())
            .map_err(|error| error.to_string());
        tracing::info!(
            event = "artifact_store.s3_client_ready",
            binding = %config.binding,
            endpoint = %config.endpoint,
            native_roots_loaded = ?native_roots_loaded,
        );
        Ok(Self {
            config,
            client: Client::from_conf(service),
        })
    }

    /// Returns the exact configured store binding.
    #[must_use]
    pub fn binding(&self) -> &str {
        &self.config.binding
    }

    /// Prefixes an application-relative key with the configured immutable
    /// object namespace and validates the resulting locator.
    ///
    /// # Errors
    ///
    /// Returns [`ObjectStoreError::ObjectIdentityInvalid`] when the scoped
    /// locator violates the configured object namespace rules.
    pub fn scoped_key(&self, suffix: &str) -> Result<String, ObjectStoreError> {
        let prefix = self.config.object_prefix.trim_matches('/');
        let suffix = suffix.trim_start_matches('/');
        let key = format!("{prefix}/{suffix}");
        self.validate_key(&key)?;
        Ok(key)
    }

    /// Signs a bounded GET for one exact immutable object version.
    ///
    /// The expected size and media type are checked before signing so a caller
    /// cannot accidentally hand an unbounded or underspecified object to a
    /// runner.  The init container still verifies the downloaded bytes against
    /// the caller's digest before publishing files into its `EmptyDir`.
    ///
    /// # Errors
    ///
    /// Returns an error when the object identity, configured size or media type
    /// is invalid, or when the object-store client cannot create a bounded
    /// presigned request.
    pub async fn presign_download(
        &self,
        key: &str,
        version: &str,
        expected_size: u64,
        media_type: &str,
        now: UtcTimestamp,
    ) -> Result<PresignedDownload, ObjectStoreError> {
        self.validate_key(key)?;
        if version.trim().is_empty()
            || expected_size == 0
            || expected_size > self.config.max_object_bytes
            || media_type.trim().is_empty()
            || media_type.bytes().any(|byte| byte.is_ascii_control())
        {
            return Err(ObjectStoreError::ObjectIdentityInvalid);
        }
        let expires = Duration::from_secs(self.config.upload_ttl_seconds);
        let request = self
            .client
            .get_object()
            .bucket(&self.config.bucket)
            .key(key)
            .version_id(version)
            .presigned(
                PresigningConfig::expires_in(expires)
                    .map_err(|_| ObjectStoreError::ConfigurationInvalid)?,
            )
            .await
            .map_err(|_| ObjectStoreError::SigningFailed)?;
        let required_headers = request
            .headers()
            .map(|(name, value)| (name.to_owned(), value.to_owned()))
            .collect();
        let ttl = time::Duration::seconds(
            i64::try_from(self.config.upload_ttl_seconds)
                .map_err(|_| ObjectStoreError::ConfigurationInvalid)?,
        );
        let expires_at = UtcTimestamp::from_utc(now.get() + ttl)
            .map_err(|_| ObjectStoreError::ConfigurationInvalid)?;
        Ok(PresignedDownload {
            url: request.uri().to_string(),
            required_headers,
            expires_at,
        })
    }

    /// Stores an immutable version in a versioned bucket without requiring
    /// S3 Object Lock. The conditional write, version id, and read-back hash
    /// still make the artifact identity explicit for clusters whose existing
    /// bucket was provisioned without Governance Lock support.
    ///
    /// # Errors
    ///
    /// Returns an object-store error when the key or payload identity is
    /// invalid, the bucket cannot establish a version, the upload fails, or
    /// the read-back identity does not match.
    pub async fn put_versioned_immutable(
        &self,
        key: &str,
        bytes: &[u8],
        media_type: &str,
    ) -> Result<VerifiedObject, ObjectStoreError> {
        self.validate_key(key)?;
        let size_bytes =
            u64::try_from(bytes.len()).map_err(|_| ObjectStoreError::ObjectTooLarge)?;
        if bytes.is_empty()
            || size_bytes > self.config.max_object_bytes
            || media_type.trim().is_empty()
        {
            return Err(ObjectStoreError::ObjectIdentityInvalid);
        }
        let response = self
            .client
            .put_object()
            .bucket(&self.config.bucket)
            .key(key)
            .content_length(
                i64::try_from(size_bytes).map_err(|_| ObjectStoreError::ObjectTooLarge)?,
            )
            .content_type(media_type)
            .if_none_match("*")
            .body(ByteStream::from(bytes.to_vec()))
            .send()
            .await
            .map_err(|error| {
                log_upload_failure(&self.config.binding, "artifact.write.versioned", &error);
                ObjectStoreError::UploadFailed
            })?;
        let version = response
            .version_id()
            .filter(|value| !value.is_empty() && *value != "null")
            .ok_or(ObjectStoreError::VersioningRequired)?
            .to_owned();
        let expected = ArtifactRef {
            artifact_id: ArtifactId::new(),
            store_binding: self.config.binding.clone(),
            object_version: version,
            size_bytes,
            media_type: media_type.to_owned(),
        };
        self.read_verified(key, &expected).await
    }

    async fn stream_read_verified_file(
        &self,
        key: &str,
        expected: &ArtifactRef,
    ) -> Result<VerifiedObjectFile, ObjectStoreError> {
        self.validate_key(key)?;
        if expected.store_binding != self.config.binding
            || expected.object_version.trim().is_empty()
            || expected.size_bytes == 0
            || expected.size_bytes > self.config.max_object_bytes
            || expected.media_type.trim().is_empty()
            || expected
                .media_type
                .bytes()
                .any(|byte| byte.is_ascii_control())
        {
            return Err(ObjectStoreError::ObjectIdentityInvalid);
        }
        let response = self
            .client
            .get_object()
            .bucket(&self.config.bucket)
            .key(key)
            .version_id(&expected.object_version)
            .send()
            .await
            .map_err(|_| ObjectStoreError::ObjectUnavailable)?;
        stream::response_to_tempfile(response, expected.clone(), self.config.max_object_bytes).await
    }

    async fn stream_freeze_current_file(
        &self,
        key: &str,
        expected_size: u64,
        media_type: &str,
    ) -> Result<VerifiedObjectFile, ObjectStoreError> {
        self.validate_key(key)?;
        if expected_size == 0
            || expected_size > self.config.max_object_bytes
            || media_type.trim().is_empty()
            || media_type.bytes().any(|byte| byte.is_ascii_control())
        {
            return Err(ObjectStoreError::ObjectIdentityInvalid);
        }
        let head = self
            .client
            .head_object()
            .bucket(&self.config.bucket)
            .key(key)
            .send()
            .await
            .map_err(|_| ObjectStoreError::ObjectUnavailable)?;
        let version = head
            .version_id()
            .filter(|value| !value.is_empty() && *value != "null")
            .ok_or(ObjectStoreError::VersioningRequired)?
            .to_owned();
        let expected = ArtifactRef {
            artifact_id: ArtifactId::new(),
            store_binding: self.config.binding.clone(),
            object_version: version,
            size_bytes: expected_size,
            media_type: media_type.to_owned(),
        };
        self.stream_read_verified_file(key, &expected).await
    }

    async fn presign_platform_image_multipart_parts(
        &self,
        key: &str,
        upload_id: &str,
        size_bytes: u64,
        now: UtcTimestamp,
        expires_at: UtcTimestamp,
    ) -> Result<PlatformImageMultipartUpload, ObjectStoreError> {
        self.validate_key(key)?;
        if upload_id.trim().is_empty()
            || size_bytes == 0
            || size_bytes > self.config.max_object_bytes
        {
            return Err(ObjectStoreError::ObjectIdentityInvalid);
        }
        let part_count = size_bytes
            .checked_add(PLATFORM_IMAGE_UPLOAD_PART_SIZE_BYTES - 1)
            .and_then(|value| value.checked_div(PLATFORM_IMAGE_UPLOAD_PART_SIZE_BYTES))
            .and_then(|value| u32::try_from(value).ok())
            .ok_or(ObjectStoreError::ObjectTooLarge)?;
        let remaining = expires_at.get() - now.get();
        let remaining_seconds = remaining.whole_seconds();
        if remaining_seconds <= 0 {
            return Err(ObjectStoreError::ObjectIdentityInvalid);
        }
        let expires = Duration::from_secs(
            u64::try_from(remaining_seconds).map_err(|_| ObjectStoreError::ConfigurationInvalid)?,
        );
        let start_time = system_time_from_utc(now)?;
        let mut parts = Vec::with_capacity(usize::try_from(part_count).unwrap_or_default());
        for part_number in 1..=part_count {
            let request = self
                .client
                .upload_part()
                .bucket(&self.config.bucket)
                .key(key)
                .upload_id(upload_id)
                .part_number(
                    i32::try_from(part_number)
                        .map_err(|_| ObjectStoreError::ObjectIdentityInvalid)?,
                )
                .presigned(
                    PresigningConfig::builder()
                        .start_time(start_time)
                        .expires_in(expires)
                        .build()
                        .map_err(|_| ObjectStoreError::ConfigurationInvalid)?,
                )
                .await
                .map_err(|error| {
                    log_upload_failure(
                        &self.config.binding,
                        "artifact.platform_image.multipart_presign_part",
                        &error,
                    );
                    ObjectStoreError::SigningFailed
                })?;
            parts.push(PresignedPlatformImagePart {
                part_number,
                url: request.uri().to_string(),
                required_headers: request
                    .headers()
                    .map(|(name, value)| (name.to_owned(), value.to_owned()))
                    .collect(),
                expires_at,
            });
        }
        Ok(PlatformImageMultipartUpload {
            upload_id: upload_id.to_owned(),
            part_size_bytes: PLATFORM_IMAGE_UPLOAD_PART_SIZE_BYTES,
            part_count,
            parts,
            expires_at,
        })
    }

    fn validate_key(&self, key: &str) -> Result<(), ObjectStoreError> {
        let prefix = self.config.object_prefix.trim_matches('/');
        if key.is_empty()
            || !key.starts_with(prefix)
            || key.contains("..")
            || key.bytes().any(|byte| byte.is_ascii_control())
        {
            return Err(ObjectStoreError::ObjectIdentityInvalid);
        }
        Ok(())
    }
}

fn system_time_from_utc(timestamp: UtcTimestamp) -> Result<SystemTime, ObjectStoreError> {
    let value = timestamp.get();
    let seconds = value.unix_timestamp();
    if seconds < 0 {
        return Err(ObjectStoreError::ConfigurationInvalid);
    }
    UNIX_EPOCH
        .checked_add(Duration::from_secs(
            u64::try_from(seconds).map_err(|_| ObjectStoreError::ConfigurationInvalid)?,
        ))
        .and_then(|time| time.checked_add(Duration::from_nanos(u64::from(value.nanosecond()))))
        .ok_or(ObjectStoreError::ConfigurationInvalid)
}

#[async_trait]
impl ImmutableObjectStore for S3ImmutableObjectStore {
    fn binding(&self) -> &str {
        S3ImmutableObjectStore::binding(self)
    }

    async fn create_platform_image_multipart_upload(
        &self,
        key: &str,
        size_bytes: u64,
        media_type: &str,
        now: UtcTimestamp,
    ) -> Result<PlatformImageMultipartUpload, ObjectStoreError> {
        self.validate_key(key)?;
        if size_bytes == 0
            || size_bytes > self.config.max_object_bytes
            || media_type.trim().is_empty()
            || media_type.bytes().any(|byte| byte.is_ascii_control())
        {
            return Err(ObjectStoreError::ObjectIdentityInvalid);
        }
        let upload = self
            .client
            .create_multipart_upload()
            .bucket(&self.config.bucket)
            .key(key)
            .content_type(media_type)
            .send()
            .await
            .map_err(|error| {
                log_upload_failure(
                    &self.config.binding,
                    "artifact.platform_image.multipart_create",
                    &error,
                );
                ObjectStoreError::UploadFailed
            })?;
        let upload_id = upload
            .upload_id()
            .filter(|value| !value.is_empty())
            .ok_or(ObjectStoreError::UploadFailed)?
            .to_owned();
        let expires_at = UtcTimestamp::from_utc(
            now.get()
                + time::Duration::seconds(
                    i64::try_from(self.config.upload_ttl_seconds)
                        .map_err(|_| ObjectStoreError::ConfigurationInvalid)?,
                ),
        )
        .map_err(|_| ObjectStoreError::ConfigurationInvalid)?;
        match self
            .presign_platform_image_multipart_parts(key, &upload_id, size_bytes, now, expires_at)
            .await
        {
            Ok(upload) => Ok(upload),
            Err(error) => {
                let _ = self
                    .client
                    .abort_multipart_upload()
                    .bucket(&self.config.bucket)
                    .key(key)
                    .upload_id(&upload_id)
                    .send()
                    .await;
                Err(error)
            }
        }
    }

    async fn presign_platform_image_multipart_upload(
        &self,
        key: &str,
        upload_id: &str,
        size_bytes: u64,
        now: UtcTimestamp,
        expires_at: UtcTimestamp,
    ) -> Result<PlatformImageMultipartUpload, ObjectStoreError> {
        self.presign_platform_image_multipart_parts(key, upload_id, size_bytes, now, expires_at)
            .await
    }

    async fn find_platform_image_multipart_uploads(
        &self,
        key: &str,
    ) -> Result<Vec<String>, ObjectStoreError> {
        self.validate_key(key)?;
        let mut key_marker = None;
        let mut upload_id_marker = None;
        let mut uploads = Vec::new();
        loop {
            let mut request = self
                .client
                .list_multipart_uploads()
                .bucket(&self.config.bucket)
                .prefix(key);
            if let Some(marker) = key_marker.as_deref() {
                request = request.key_marker(marker);
            }
            if let Some(marker) = upload_id_marker.as_deref() {
                request = request.upload_id_marker(marker);
            }
            let output = request
                .send()
                .await
                .map_err(|_| ObjectStoreError::ObjectUnavailable)?;
            uploads.extend(
                output
                    .uploads()
                    .iter()
                    .filter(|upload| upload.key() == Some(key))
                    .filter_map(|upload| upload.upload_id().map(ToOwned::to_owned)),
            );
            if !output.is_truncated().unwrap_or(false) {
                break;
            }
            key_marker = Some(
                output
                    .next_key_marker()
                    .filter(|value| !value.is_empty())
                    .ok_or(ObjectStoreError::ObjectUnavailable)?
                    .to_owned(),
            );
            upload_id_marker = Some(
                output
                    .next_upload_id_marker()
                    .filter(|value| !value.is_empty())
                    .ok_or(ObjectStoreError::ObjectUnavailable)?
                    .to_owned(),
            );
        }
        Ok(uploads)
    }

    async fn list_platform_image_multipart_parts(
        &self,
        key: &str,
        upload_id: &str,
    ) -> Result<Vec<PlatformImageMultipartPart>, ObjectStoreError> {
        self.validate_key(key)?;
        if upload_id.trim().is_empty() {
            return Err(ObjectStoreError::ObjectIdentityInvalid);
        }
        let mut marker = None;
        let mut result = Vec::new();
        loop {
            let mut request = self
                .client
                .list_parts()
                .bucket(&self.config.bucket)
                .key(key)
                .upload_id(upload_id);
            if let Some(marker) = marker.as_deref() {
                request = request.part_number_marker(marker);
            }
            let output = request
                .send()
                .await
                .map_err(|error| map_multipart_sdk_error(&error))?;
            for part in output.parts() {
                let part_number = part
                    .part_number()
                    .and_then(|value| u32::try_from(value).ok())
                    .ok_or(ObjectStoreError::ObjectIdentityMismatch)?;
                let etag = part
                    .e_tag()
                    .filter(|value| !value.is_empty())
                    .ok_or(ObjectStoreError::ObjectIdentityMismatch)?
                    .to_owned();
                let size_bytes = part
                    .size()
                    .and_then(|value| u64::try_from(value).ok())
                    .ok_or(ObjectStoreError::ObjectIdentityMismatch)?;
                result.push(PlatformImageMultipartPart {
                    part_number,
                    etag,
                    size_bytes,
                });
            }
            if !output.is_truncated().unwrap_or(false) {
                break;
            }
            marker = Some(
                output
                    .next_part_number_marker()
                    .filter(|value| !value.is_empty())
                    .ok_or(ObjectStoreError::ObjectUnavailable)?
                    .to_owned(),
            );
        }
        result.sort_by_key(|part| part.part_number);
        Ok(result)
    }

    async fn complete_platform_image_multipart_upload(
        &self,
        key: &str,
        upload_id: &str,
        size_bytes: u64,
        parts: &[PlatformImageMultipartPartInput],
    ) -> Result<(), ObjectStoreError> {
        self.validate_key(key)?;
        if upload_id.trim().is_empty() || size_bytes == 0 {
            return Err(ObjectStoreError::ObjectIdentityInvalid);
        }
        let expected_part_count = size_bytes
            .checked_add(PLATFORM_IMAGE_UPLOAD_PART_SIZE_BYTES - 1)
            .and_then(|value| value.checked_div(PLATFORM_IMAGE_UPLOAD_PART_SIZE_BYTES))
            .and_then(|value| usize::try_from(value).ok())
            .ok_or(ObjectStoreError::ObjectTooLarge)?;
        if parts.len() != expected_part_count
            || parts.iter().enumerate().any(|(index, part)| {
                part.part_number != u32::try_from(index + 1).unwrap_or_default()
                    || part.etag.trim().is_empty()
                    || part.etag.bytes().any(|byte| byte.is_ascii_control())
            })
        {
            return Err(ObjectStoreError::ObjectIdentityMismatch);
        }
        let observed = self
            .list_platform_image_multipart_parts(key, upload_id)
            .await?;
        if observed.len() != parts.len()
            || observed.iter().enumerate().any(|(index, observed)| {
                let expected_size = if index + 1 == expected_part_count {
                    size_bytes
                        - PLATFORM_IMAGE_UPLOAD_PART_SIZE_BYTES
                            * u64::try_from(expected_part_count - 1).unwrap_or_default()
                } else {
                    PLATFORM_IMAGE_UPLOAD_PART_SIZE_BYTES
                };
                observed.part_number != parts[index].part_number
                    || observed.etag != parts[index].etag
                    || observed.size_bytes != expected_size
            })
        {
            return Err(ObjectStoreError::ObjectIdentityMismatch);
        }
        let completed_parts = parts
            .iter()
            .map(|part| {
                Ok::<_, ObjectStoreError>(
                    CompletedPart::builder()
                        .part_number(
                            i32::try_from(part.part_number)
                                .map_err(|_| ObjectStoreError::ObjectIdentityMismatch)?,
                        )
                        .e_tag(&part.etag)
                        .build(),
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        self.client
            .complete_multipart_upload()
            .bucket(&self.config.bucket)
            .key(key)
            .upload_id(upload_id)
            .multipart_upload(
                CompletedMultipartUpload::builder()
                    .set_parts(Some(completed_parts))
                    .build(),
            )
            .send()
            .await
            .map_err(|error| map_multipart_sdk_error(&error))?;
        Ok(())
    }

    async fn abort_platform_image_multipart_upload(
        &self,
        key: &str,
        upload_id: &str,
    ) -> Result<(), ObjectStoreError> {
        self.validate_key(key)?;
        if upload_id.trim().is_empty() {
            return Err(ObjectStoreError::ObjectIdentityInvalid);
        }
        match self
            .client
            .abort_multipart_upload()
            .bucket(&self.config.bucket)
            .key(key)
            .upload_id(upload_id)
            .send()
            .await
        {
            Ok(_) => Ok(()),
            Err(error)
                if error.as_service_error().and_then(|service| service.code())
                    == Some("NoSuchUpload") =>
            {
                Ok(())
            }
            Err(error) => Err(map_multipart_sdk_error(&error)),
        }
    }

    /// Presigns one write once object.
    ///
    /// `size_bytes` is the reviewed upper bound of the write, not the size of the body a writer
    /// sends: the body of a sandbox attempt result only exists inside the attempt. The bound
    /// therefore stays a caller-side check — the writer refuses to upload beyond it and the reader
    /// verifies the exact size it was promised — while the signature must not pin `Content-Length`,
    /// because a pinned length makes every writer whose body is smaller than the bound fail.
    async fn presign_upload(
        &self,
        key: &str,
        size_bytes: u64,
        media_type: &str,
        now: UtcTimestamp,
    ) -> Result<PresignedUpload, ObjectStoreError> {
        self.validate_key(key)?;
        if size_bytes == 0
            || size_bytes > self.config.max_object_bytes
            || media_type.trim().is_empty()
            || media_type.bytes().any(|byte| byte.is_ascii_control())
        {
            return Err(ObjectStoreError::ObjectIdentityInvalid);
        }
        let expires = Duration::from_secs(self.config.upload_ttl_seconds);
        let request = self
            .client
            .put_object()
            .bucket(&self.config.bucket)
            .key(key)
            .content_type(media_type)
            .if_none_match("*")
            .presigned(
                PresigningConfig::expires_in(expires)
                    .map_err(|_| ObjectStoreError::ConfigurationInvalid)?,
            )
            .await
            .map_err(|_| ObjectStoreError::SigningFailed)?;
        let required_headers = request
            .headers()
            .map(|(name, value)| (name.to_owned(), value.to_owned()))
            .collect();
        let ttl = time::Duration::seconds(
            i64::try_from(self.config.upload_ttl_seconds)
                .map_err(|_| ObjectStoreError::ConfigurationInvalid)?,
        );
        let expires_at = UtcTimestamp::from_utc(now.get() + ttl)
            .map_err(|_| ObjectStoreError::ConfigurationInvalid)?;
        Ok(PresignedUpload {
            url: request.uri().to_string(),
            required_headers,
            expires_at,
        })
    }

    async fn read_verified(
        &self,
        key: &str,
        expected: &ArtifactRef,
    ) -> Result<VerifiedObject, ObjectStoreError> {
        self.validate_key(key)?;
        if expected.store_binding != self.config.binding
            || expected.object_version.trim().is_empty()
            || expected.size_bytes == 0
            || expected.size_bytes > self.config.max_object_bytes
            || expected.media_type.trim().is_empty()
            || expected
                .media_type
                .bytes()
                .any(|byte| byte.is_ascii_control())
        {
            return Err(ObjectStoreError::ObjectIdentityInvalid);
        }
        let response = match self
            .client
            .get_object()
            .bucket(&self.config.bucket)
            .key(key)
            .version_id(&expected.object_version)
            .send()
            .await
        {
            Ok(response) => response,
            Err(error) => {
                let rendered_error = error.to_string();
                let error_class = rendered_error.split(':').next().unwrap_or("unknown").trim();
                let mut chain: Vec<String> = Vec::new();
                let mut current: Option<&dyn std::error::Error> = Some(&error);
                while let Some(source) = current {
                    chain.push(source.to_string());
                    current = source.source();
                }
                tracing::warn!(
                    event = "artifact_store.get_object_failed",
                    component = "immutable-object-store",
                    operation = "artifact.read",
                    outcome = "failed",
                    duration_ms = 0_u64,
                    binding = self.config.binding,
                    endpoint = %self.config.endpoint,
                    object_key = %key,
                    object_version = %expected.object_version,
                    diagnostic_code = "LW_OBJECT_STORE_UNAVAILABLE",
                    error_class = %error_class,
                    error_chain = ?chain,
                    service_error_code = ?error.as_service_error().and_then(|service| service.code()),
                    error_kind = "object_read_failed",
                    failure_stage = "artifact.read.request",
                    retryable = true,
                    safe_detail = "object_read_failed",
                );
                return Err(ObjectStoreError::ObjectUnavailable);
            }
        };
        let observed_size = response
            .content_length()
            .and_then(|observed| u64::try_from(observed).ok());
        if response
            .version_id()
            .is_none_or(|observed| observed != expected.object_version)
            || observed_size != Some(expected.size_bytes)
            || response
                .content_type()
                .is_none_or(|observed| observed != expected.media_type)
        {
            return Err(ObjectStoreError::ObjectIdentityMismatch);
        }
        let Ok(body) = response.body.collect().await else {
            tracing::warn!(
                event = "artifact_store.get_object_body_failed",
                component = "immutable-object-store",
                operation = "artifact.read",
                outcome = "failed",
                duration_ms = 0_u64,
                binding = self.config.binding,
                diagnostic_code = "LW_OBJECT_STORE_UNAVAILABLE",
                error_kind = "object_body_read_failed",
                failure_stage = "artifact.read.body",
                retryable = true,
                safe_detail = "object_body_read_failed",
            );
            return Err(ObjectStoreError::ObjectUnavailable);
        };
        let body = body.into_bytes().to_vec();
        if u64::try_from(body.len()).ok() != Some(expected.size_bytes) {
            return Err(ObjectStoreError::ObjectIdentityMismatch);
        }
        Ok(VerifiedObject {
            reference: expected.clone(),
            bytes: body,
        })
    }

    async fn read_verified_file(
        &self,
        key: &str,
        expected: &ArtifactRef,
    ) -> Result<VerifiedObjectFile, ObjectStoreError> {
        self.stream_read_verified_file(key, expected).await
    }

    async fn freeze_current(
        &self,
        key: &str,
        expected_size: u64,
        media_type: &str,
    ) -> Result<VerifiedObject, ObjectStoreError> {
        self.validate_key(key)?;
        let head = self
            .client
            .head_object()
            .bucket(&self.config.bucket)
            .key(key)
            .send()
            .await
            .map_err(|_| ObjectStoreError::ObjectUnavailable)?;
        let version = head
            .version_id()
            .filter(|value| !value.is_empty() && *value != "null")
            .ok_or(ObjectStoreError::VersioningRequired)?
            .to_owned();
        let expected = ArtifactRef {
            artifact_id: ArtifactId::new(),
            store_binding: self.config.binding.clone(),
            object_version: version,
            size_bytes: expected_size,
            media_type: media_type.to_owned(),
        };
        self.read_verified(key, &expected).await
    }

    async fn freeze_current_reference(
        &self,
        key: &str,
        expected_size: u64,
        media_type: &str,
    ) -> Result<ArtifactRef, ObjectStoreError> {
        self.validate_key(key)?;
        if expected_size == 0
            || expected_size > self.config.max_object_bytes
            || media_type.trim().is_empty()
            || media_type.bytes().any(|byte| byte.is_ascii_control())
        {
            return Err(ObjectStoreError::ObjectIdentityInvalid);
        }
        let head = self
            .client
            .head_object()
            .bucket(&self.config.bucket)
            .key(key)
            .send()
            .await
            .map_err(|error| {
                if error
                    .raw_response()
                    .is_some_and(|response| response.status().as_u16() == 404)
                {
                    ObjectStoreError::ObjectNotFound
                } else {
                    ObjectStoreError::ObjectUnavailable
                }
            })?;
        let version = head
            .version_id()
            .filter(|value| !value.is_empty() && *value != "null")
            .ok_or(ObjectStoreError::VersioningRequired)?
            .to_owned();
        let observed_size = head
            .content_length()
            .and_then(|observed| u64::try_from(observed).ok());
        if head.version_id().is_none_or(|observed| observed != version)
            || observed_size != Some(expected_size)
            || head
                .content_type()
                .is_none_or(|observed| observed != media_type)
        {
            return Err(ObjectStoreError::ObjectIdentityMismatch);
        }
        Ok(ArtifactRef {
            artifact_id: ArtifactId::new(),
            store_binding: self.config.binding.clone(),
            object_version: version,
            size_bytes: expected_size,
            media_type: media_type.to_owned(),
        })
    }

    async fn list_key_versions(&self, key: &str) -> Result<Vec<String>, ObjectStoreError> {
        self.validate_key(key)?;
        let mut key_marker = None;
        let mut version_marker = None;
        let mut versions = std::collections::BTreeSet::new();
        loop {
            let page = self
                .client
                .list_object_versions()
                .bucket(&self.config.bucket)
                .prefix(key)
                .max_keys(1000)
                .set_key_marker(key_marker.clone())
                .set_version_id_marker(version_marker.clone())
                .send()
                .await
                .map_err(|_| ObjectStoreError::ObjectUnavailable)?;
            for version in page.versions() {
                if version.key() == Some(key) {
                    let id = version
                        .version_id()
                        .filter(|id| !id.is_empty() && *id != "null")
                        .ok_or(ObjectStoreError::VersioningRequired)?;
                    versions.insert(id.to_owned());
                }
            }
            for marker in page.delete_markers() {
                if marker.key() == Some(key) {
                    let id = marker
                        .version_id()
                        .filter(|id| !id.is_empty() && *id != "null")
                        .ok_or(ObjectStoreError::VersioningRequired)?;
                    versions.insert(id.to_owned());
                }
            }
            if page.is_truncated() != Some(true) {
                break;
            }
            let next_key = page
                .next_key_marker()
                .ok_or(ObjectStoreError::ObjectUnavailable)?;
            let next_version = page.next_version_id_marker().map(str::to_owned);
            if !next_key.starts_with(key)
                || (key_marker.as_deref() == Some(next_key) && version_marker == next_version)
            {
                return Err(ObjectStoreError::ObjectUnavailable);
            }
            key_marker = Some(next_key.to_owned());
            version_marker = next_version;
        }
        Ok(versions.into_iter().collect())
    }

    async fn freeze_current_file(
        &self,
        key: &str,
        expected_size: u64,
        media_type: &str,
    ) -> Result<VerifiedObjectFile, ObjectStoreError> {
        self.stream_freeze_current_file(key, expected_size, media_type)
            .await
    }

    async fn put_immutable(
        &self,
        key: &str,
        bytes: &[u8],
        media_type: &str,
        now: UtcTimestamp,
        retain_until: Option<UtcTimestamp>,
    ) -> Result<VerifiedObject, ObjectStoreError> {
        let Some(retain_until) = retain_until else {
            return self.put_versioned_immutable(key, bytes, media_type).await;
        };
        self.validate_key(key)?;
        let size_bytes =
            u64::try_from(bytes.len()).map_err(|_| ObjectStoreError::ObjectTooLarge)?;
        if bytes.is_empty()
            || size_bytes > self.config.max_object_bytes
            || media_type.trim().is_empty()
            || retain_until.get() <= now.get()
        {
            return Err(ObjectStoreError::ObjectIdentityInvalid);
        }
        let retention = DateTime::from_nanos(retain_until.get().unix_timestamp_nanos())
            .map_err(|_| ObjectStoreError::ObjectIdentityInvalid)?;
        let response = self
            .client
            .put_object()
            .bucket(&self.config.bucket)
            .key(key)
            .content_length(
                i64::try_from(size_bytes).map_err(|_| ObjectStoreError::ObjectTooLarge)?,
            )
            .content_type(media_type)
            .if_none_match("*")
            .object_lock_mode(ObjectLockMode::Governance)
            .object_lock_retain_until_date(retention)
            .body(ByteStream::from(bytes.to_vec()))
            .send()
            .await
            .map_err(|error| {
                log_upload_failure(
                    &self.config.binding,
                    "artifact.write.governance_locked",
                    &error,
                );
                ObjectStoreError::UploadFailed
            })?;
        let version = response
            .version_id()
            .filter(|value| !value.is_empty() && *value != "null")
            .ok_or(ObjectStoreError::VersioningRequired)?
            .to_owned();
        let head = self
            .client
            .head_object()
            .bucket(&self.config.bucket)
            .key(key)
            .version_id(&version)
            .send()
            .await
            .map_err(|_| ObjectStoreError::ObjectUnavailable)?;
        let observed_size = head
            .content_length()
            .and_then(|observed| u64::try_from(observed).ok());
        if head.version_id().is_none_or(|observed| observed != version)
            || observed_size != Some(size_bytes)
            || head
                .content_type()
                .is_none_or(|observed| observed != media_type)
            || head.object_lock_mode() != Some(&ObjectLockMode::Governance)
            || head
                .object_lock_retain_until_date()
                .is_none_or(|observed| observed.as_nanos() != retention.as_nanos())
        {
            return Err(ObjectStoreError::ObjectLockIdentityMismatch);
        }
        let expected = ArtifactRef {
            artifact_id: ArtifactId::new(),
            store_binding: self.config.binding.clone(),
            object_version: version,
            size_bytes,
            media_type: media_type.to_owned(),
        };
        self.read_verified(key, &expected).await
    }

    async fn delete_orphan(&self, key: &str, version: &str) -> Result<(), ObjectStoreError> {
        self.validate_key(key)?;
        if version.trim().is_empty() {
            return Err(ObjectStoreError::ObjectIdentityInvalid);
        }
        self.client
            .delete_object()
            .bucket(&self.config.bucket)
            .key(key)
            .version_id(version)
            .send()
            .await
            .map_err(|_| ObjectStoreError::DeleteFailed)?;
        Ok(())
    }
}

struct S3UploadDiagnostics<'a> {
    sdk_error_category: &'static str,
    service_code: &'a str,
    http_status: u16,
    request_id: &'a str,
}

fn s3_upload_diagnostics<E>(error: &SdkError<E>) -> S3UploadDiagnostics<'_>
where
    E: ProvideErrorMetadata,
{
    let response = error.raw_response();
    S3UploadDiagnostics {
        sdk_error_category: match error {
            SdkError::ConstructionFailure(_) => "construction",
            SdkError::TimeoutError(_) => "timeout",
            SdkError::DispatchFailure(_) => "dispatch",
            SdkError::ResponseError(_) => "response",
            SdkError::ServiceError(_) => "service",
            _ => "unknown",
        },
        service_code: error.code().unwrap_or("unknown"),
        http_status: response.map_or(0, |value| value.status().as_u16()),
        request_id: response
            .and_then(|value| value.headers().get("x-amz-request-id"))
            .unwrap_or("unknown"),
    }
}

fn map_multipart_sdk_error<E>(error: &SdkError<E>) -> ObjectStoreError
where
    E: ProvideErrorMetadata,
{
    if error.as_service_error().and_then(|service| service.code()) == Some("NoSuchUpload") {
        ObjectStoreError::ObjectNotFound
    } else {
        ObjectStoreError::ObjectUnavailable
    }
}

fn log_upload_failure<E>(binding: &str, operation: &'static str, error: &SdkError<E>)
where
    E: ProvideErrorMetadata,
{
    let diagnostics = s3_upload_diagnostics(error);
    tracing::error!(
        event = "artifact_store.put_object_failed",
        component = "immutable-object-store",
        operation,
        outcome = "failed",
        duration_ms = 0_u64,
        binding,
        diagnostic_code = "LW_OBJECT_UPLOAD_FAILED",
        error_kind = diagnostics.sdk_error_category,
        failure_stage = "artifact.write.request",
        retryable = false,
        http_status = diagnostics.http_status,
        s3_error_code = diagnostics.service_code,
        s3_request_id = diagnostics.request_id,
        safe_detail = "object_upload_failed",
    );
}

/// Fail-fast storage errors with stable diagnostics.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ObjectStoreError {
    /// Deployment configuration is incomplete or unsafe.
    #[error("LW_OBJECT_STORE_CONFIG_INVALID")]
    ConfigurationInvalid,
    /// Object identity is malformed or outside the configured prefix.
    #[error("LW_OBJECT_IDENTITY_INVALID")]
    ObjectIdentityInvalid,
    /// Object exceeds configured bounds.
    #[error("LW_OBJECT_TOO_LARGE")]
    ObjectTooLarge,
    /// Presigning failed.
    #[error("LW_OBJECT_UPLOAD_SIGNING_FAILED")]
    SigningFailed,
    /// Immutable upload failed before an object version was established.
    #[error("LW_OBJECT_UPLOAD_FAILED")]
    UploadFailed,
    /// Object could not be read.
    #[error("LW_OBJECT_UNAVAILABLE")]
    ObjectUnavailable,
    /// A metadata lookup confirmed that the exact key does not exist.
    #[error("LW_OBJECT_NOT_FOUND")]
    ObjectNotFound,
    /// Stored bytes or metadata differ from the immutable manifest.
    #[error("LW_OBJECT_IDENTITY_MISMATCH")]
    ObjectIdentityMismatch,
    /// Orphan cleanup failed and must be retried.
    #[error("LW_OBJECT_CLEANUP_FAILED")]
    DeleteFailed,
    /// Bucket versioning is disabled, so immutable package identity cannot be established.
    #[error("LW_OBJECT_VERSIONING_REQUIRED")]
    VersioningRequired,
    /// Governance Object Lock is unavailable or not implemented by the binding.
    #[error("LW_OBJECT_LOCK_REQUIRED")]
    ObjectLockRequired,
    /// Stored retention mode, deadline, metadata, or version differs from the request.
    #[error("LW_OBJECT_LOCK_IDENTITY_MISMATCH")]
    ObjectLockIdentityMismatch,
    /// This object-store binding exposes only the byte-oriented API.
    #[error("LW_OBJECT_STREAMING_UNSUPPORTED")]
    StreamingUnsupported,
}

impl ObjectStoreError {
    /// Returns a stable diagnostic without object keys, payloads, or credentials.
    #[must_use]
    pub const fn diagnostic_code(&self) -> &'static str {
        match self {
            Self::ConfigurationInvalid => "LW_OBJECT_STORE_CONFIG_INVALID",
            Self::ObjectIdentityInvalid => "LW_OBJECT_IDENTITY_INVALID",
            Self::ObjectTooLarge => "LW_OBJECT_TOO_LARGE",
            Self::SigningFailed => "LW_OBJECT_UPLOAD_SIGNING_FAILED",
            Self::UploadFailed => "LW_OBJECT_UPLOAD_FAILED",
            Self::ObjectUnavailable => "LW_OBJECT_UNAVAILABLE",
            Self::ObjectNotFound => "LW_OBJECT_NOT_FOUND",
            Self::ObjectIdentityMismatch => "LW_OBJECT_IDENTITY_MISMATCH",
            Self::DeleteFailed => "LW_OBJECT_CLEANUP_FAILED",
            Self::VersioningRequired => "LW_OBJECT_VERSIONING_REQUIRED",
            Self::ObjectLockRequired => "LW_OBJECT_LOCK_REQUIRED",
            Self::ObjectLockIdentityMismatch => "LW_OBJECT_LOCK_IDENTITY_MISMATCH",
            Self::StreamingUnsupported => "LW_OBJECT_STREAMING_UNSUPPORTED",
        }
    }
}

#[cfg(test)]
mod tests {
    use aws_sdk_s3::error::{ErrorMetadata, SdkError};
    use aws_sdk_s3::operation::put_object::PutObjectError;
    use aws_sdk_s3::types::{BucketVersioningStatus, VersioningConfiguration};
    use aws_smithy_runtime_api::http::{Response, StatusCode};
    use aws_smithy_types::body::SdkBody;
    use contracts::http::PLATFORM_IMAGE_UPLOAD_PART_SIZE_BYTES;
    use contracts::{ArtifactRef, UtcTimestamp};
    use sha2::{Digest, Sha256};
    use testcontainers::core::{IntoContainerPort, WaitFor};
    use testcontainers::{GenericImage, ImageExt, bollard::Docker, runners::AsyncRunner};

    use super::{
        BehaviorVersion, Credentials, ImmutableObjectStore, ObjectStoreError,
        PlatformImageMultipartPartInput, Region, S3ConfigBuilder, S3ImmutableObjectStore,
        S3StoreConfig, s3_upload_diagnostics,
    };

    async fn local_minio_image() -> Result<GenericImage, Box<dyn std::error::Error>> {
        let descriptor = std::env::var("LABWEAVER_TEST_MINIO_IMAGE")
            .map_err(|_| "LABWEAVER_TEST_MINIO_IMAGE must name a locally prepared image")?;
        let (name, tag) = descriptor
            .rsplit_once(':')
            .filter(|(name, tag)| !name.is_empty() && !tag.is_empty() && !tag.contains('/'))
            .ok_or("LABWEAVER_TEST_MINIO_IMAGE must be a local name:tag reference")?;
        let docker = Docker::connect_with_local_defaults()?;
        docker.inspect_image(&descriptor).await?;
        Ok(GenericImage::new(name.to_owned(), tag.to_owned()))
    }

    #[test]
    fn upload_diagnostics_extract_service_metadata() -> Result<(), Box<dyn std::error::Error>> {
        let mut response = Response::new(StatusCode::try_from(404_u16)?, SdkBody::empty());
        response
            .headers_mut()
            .insert("x-amz-request-id", "req-no-such-bucket");
        let error = SdkError::service_error(
            PutObjectError::generic(ErrorMetadata::builder().code("NoSuchBucket").build()),
            response,
        );
        let diagnostics = s3_upload_diagnostics(&error);
        assert_eq!(diagnostics.sdk_error_category, "service");
        assert_eq!(diagnostics.service_code, "NoSuchBucket");
        assert_eq!(diagnostics.http_status, 404);
        assert_eq!(diagnostics.request_id, "req-no-such-bucket");
        Ok(())
    }

    #[test]
    fn configuration_rejects_http_and_unbounded_uploads() -> Result<(), Box<dyn std::error::Error>>
    {
        let mut config = S3StoreConfig {
            binding: "minio-primary".to_owned(),
            endpoint: "https://minio.internal.example".parse()?,
            bucket: "labweaver-materials".to_owned(),
            region: "labweaver".to_owned(),
            object_prefix: "problem-packages".to_owned(),
            upload_ttl_seconds: 900,
            max_object_bytes: 64 * 1024 * 1024,
            force_path_style: true,
            ca_bundle_file: None,
        };
        config.validate()?;
        config.endpoint = "http://minio.internal.example".parse()?;
        assert!(config.validate().is_err());
        Ok(())
    }

    #[test]
    fn configuration_accepts_four_hour_ttl_and_rejects_out_of_range_values()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut config = S3StoreConfig {
            binding: "minio-primary".to_owned(),
            endpoint: "https://minio.internal.example".parse()?,
            bucket: "labweaver-materials".to_owned(),
            region: "labweaver".to_owned(),
            object_prefix: "problem-packages".to_owned(),
            upload_ttl_seconds: 14_400,
            max_object_bytes: 64 * 1024 * 1024,
            force_path_style: true,
            ca_bundle_file: None,
        };
        config.validate()?;

        config.upload_ttl_seconds = 14_401;
        assert!(config.validate().is_err());

        config.upload_ttl_seconds = 0;
        assert!(config.validate().is_err());
        Ok(())
    }

    #[tokio::test]
    #[allow(
        clippy::too_many_lines,
        reason = "one MinIO lifecycle preserves a single exact bucket and object-version identity"
    )]
    async fn minio_versioning_object_lock_and_cleanup_are_fail_closed()
    -> Result<(), Box<dyn std::error::Error>> {
        let minio = local_minio_image()
            .await?
            .with_exposed_port(9000.tcp())
            .with_wait_for(WaitFor::message_on_stderr("API:"))
            .with_env_var("MINIO_ROOT_USER", "labweaver-test")
            .with_env_var("MINIO_ROOT_PASSWORD", "labweaver-test-secret")
            .with_cmd(["server", "/data"])
            .start()
            .await?;
        let endpoint = format!("http://127.0.0.1:{}", minio.get_host_port_ipv4(9000).await?);
        let config = S3StoreConfig {
            binding: "minio-e2-v1".to_owned(),
            endpoint: endpoint.parse()?,
            bucket: "issue-48-materials".to_owned(),
            region: "labweaver-test-1".to_owned(),
            object_prefix: "problem-packages".to_owned(),
            upload_ttl_seconds: 60,
            max_object_bytes: 1_024,
            force_path_style: true,
            ca_bundle_file: None,
        };
        let credentials = Credentials::new(
            "labweaver-test",
            "labweaver-test-secret",
            None,
            None,
            "issue-48-test",
        );
        let shared = aws_config::defaults(BehaviorVersion::latest())
            .region(Region::new(config.region.clone()))
            .credentials_provider(credentials)
            .endpoint_url(&endpoint)
            .load()
            .await;
        let client = aws_sdk_s3::Client::from_conf(
            S3ConfigBuilder::from(&shared)
                .force_path_style(true)
                .build(),
        );
        client
            .create_bucket()
            .bucket(&config.bucket)
            .object_lock_enabled_for_bucket(true)
            .send()
            .await?;
        client
            .put_bucket_versioning()
            .bucket(&config.bucket)
            .versioning_configuration(
                VersioningConfiguration::builder()
                    .status(BucketVersioningStatus::Enabled)
                    .build(),
            )
            .send()
            .await?;
        let store = S3ImmutableObjectStore {
            config,
            client: client.clone(),
        };
        verify_exact_key_version_cleanup(&store).await?;
        let package_config = S3StoreConfig {
            binding: "minio-package-e2-v1".to_owned(),
            endpoint: endpoint.parse()?,
            bucket: "issue-48-packages".to_owned(),
            region: "labweaver-test-1".to_owned(),
            object_prefix: "problem-packages".to_owned(),
            upload_ttl_seconds: 60,
            max_object_bytes: 1_024,
            force_path_style: true,
            ca_bundle_file: None,
        };
        client
            .create_bucket()
            .bucket(&package_config.bucket)
            .send()
            .await?;
        client
            .put_bucket_versioning()
            .bucket(&package_config.bucket)
            .versioning_configuration(
                VersioningConfiguration::builder()
                    .status(BucketVersioningStatus::Enabled)
                    .build(),
            )
            .send()
            .await?;
        let package_store = S3ImmutableObjectStore {
            config: package_config,
            client: client.clone(),
        };
        let bytes = b"immutable teacher material";
        let key = "problem-packages/course/upload/object";
        let presigned = store
            .presign_upload(
                key,
                u64::try_from(bytes.len())?,
                "text/plain",
                "2026-07-15T08:00:00.000Z".parse::<UtcTimestamp>()?,
            )
            .await?;
        let http = reqwest::Client::builder().no_proxy().build()?;
        let mut request = http.put(&presigned.url);
        for (name, value) in &presigned.required_headers {
            request = request.header(name, value);
        }
        let response = request.body(bytes.to_vec()).send().await?;
        assert!(response.status().is_success());
        // A writer that uploads less than the reviewed bound must still succeed: the size bound is
        // a caller-side check, so the signature never pins a body length.
        let bounded = store
            .presign_upload(
                "problem-packages/course/upload/bounded-object",
                store.config.max_object_bytes,
                "text/plain",
                "2026-07-15T08:00:00.000Z".parse::<UtcTimestamp>()?,
            )
            .await?;
        let mut bounded_request = http.put(&bounded.url);
        for (name, value) in &bounded.required_headers {
            bounded_request = bounded_request.header(name, value);
        }
        assert!(
            bounded_request
                .body(bytes.to_vec())
                .send()
                .await?
                .status()
                .is_success()
        );
        let mut overwrite = http.put(&presigned.url);
        for (name, value) in &presigned.required_headers {
            overwrite = overwrite.header(name, value);
        }
        assert_eq!(
            overwrite.body(bytes.to_vec()).send().await?.status(),
            reqwest::StatusCode::PRECONDITION_FAILED
        );

        let frozen = store
            .freeze_current(key, u64::try_from(bytes.len())?, "text/plain")
            .await?;
        assert_eq!(frozen.bytes, bytes);
        let version = frozen.reference.object_version.clone();
        client
            .put_object()
            .bucket(&store.config.bucket)
            .key(key)
            .content_type("text/plain")
            .body(aws_sdk_s3::primitives::ByteStream::from_static(
                b"replacement",
            ))
            .send()
            .await?;
        assert!(
            store
                .freeze_current(key, u64::try_from(bytes.len())?, "text/plain",)
                .await
                .is_err()
        );
        assert_eq!(
            store
                .read_verified(
                    key,
                    &ArtifactRef {
                        artifact_id: frozen.reference.artifact_id,
                        store_binding: frozen.reference.store_binding.clone(),
                        object_version: version.clone(),
                        size_bytes: u64::try_from(bytes.len())?,
                        media_type: "text/plain".to_owned(),
                    },
                )
                .await?
                .bytes,
            bytes
        );
        let reread = store.read_verified(key, &frozen.reference).await?;
        assert_eq!(reread.reference, frozen.reference);
        assert_eq!(reread.bytes, bytes);
        let streamed = store.read_verified_file(key, &frozen.reference).await?;
        let streamed_path = streamed.path().to_owned();
        assert_eq!(streamed.reference(), &frozen.reference);
        assert_eq!(std::fs::read(&streamed_path)?, bytes);
        assert_eq!(streamed.sha256(), format!("{:x}", Sha256::digest(bytes)));
        drop(streamed);
        assert!(!streamed_path.exists());

        // Platform image archives use the real S3 multipart path.  Keep one non-final part at the
        // production 64 MiB boundary so MinIO enforces the same minimum-part behavior as S3.
        let mut image_config = store.config.clone();
        image_config.upload_ttl_seconds = 600;
        image_config.max_object_bytes = PLATFORM_IMAGE_UPLOAD_PART_SIZE_BYTES * 2;
        let image_store = S3ImmutableObjectStore {
            config: image_config,
            client: client.clone(),
        };
        let image_key = "problem-packages/platform-image-uploads/multipart-e2e.tar";
        let image_part_size = PLATFORM_IMAGE_UPLOAD_PART_SIZE_BYTES;
        let image_tail = b"platform-image-tail".to_vec();
        let image_tail_size = u64::try_from(image_tail.len())?;
        let image_size = image_part_size + image_tail_size;
        let observed_now = time::OffsetDateTime::now_utc();
        let observed_now =
            observed_now.replace_nanosecond(observed_now.nanosecond() / 1_000_000 * 1_000_000)?;
        let image_now = UtcTimestamp::from_utc(observed_now)?;
        let image_upload = image_store
            .create_platform_image_multipart_upload(
                image_key,
                image_size,
                "application/x-tar",
                image_now,
            )
            .await?;
        assert_eq!(image_upload.part_count, 2);
        assert_eq!(image_upload.parts.len(), 2);
        let http = reqwest::Client::builder().no_proxy().build()?;
        let image_head = vec![0xA5; usize::try_from(image_part_size)?];
        let mut part_inputs = Vec::with_capacity(image_upload.parts.len());
        for (part, bytes) in image_upload.parts.iter().zip([image_head, image_tail]) {
            let mut request = http.put(&part.url);
            for (name, value) in &part.required_headers {
                request = request.header(name, value);
            }
            let response = request.body(bytes).send().await?;
            if !response.status().is_success() {
                return Err(format!("multipart part upload returned {}", response.status()).into());
            }
            let etag = response
                .headers()
                .get(reqwest::header::ETAG)
                .ok_or("multipart part response omitted ETag")?
                .to_str()?
                .to_owned();
            part_inputs.push(PlatformImageMultipartPartInput {
                part_number: part.part_number,
                etag,
            });
        }
        let observed_parts = image_store
            .list_platform_image_multipart_parts(image_key, &image_upload.upload_id)
            .await?;
        assert_eq!(observed_parts.len(), 2);
        assert_eq!(observed_parts[0].size_bytes, image_part_size);
        assert_eq!(observed_parts[1].size_bytes, image_tail_size);

        let resumed = image_store
            .presign_platform_image_multipart_upload(
                image_key,
                &image_upload.upload_id,
                image_size,
                UtcTimestamp::from_utc(image_now.get() + time::Duration::seconds(1))?,
                image_upload.expires_at,
            )
            .await?;
        assert_eq!(resumed.expires_at, image_upload.expires_at);
        assert_eq!(resumed.part_count, image_upload.part_count);

        let mut wrong_parts = part_inputs.clone();
        wrong_parts[0].etag.push('x');
        assert_eq!(
            image_store
                .complete_platform_image_multipart_upload(
                    image_key,
                    &image_upload.upload_id,
                    image_size,
                    &wrong_parts,
                )
                .await,
            Err(ObjectStoreError::ObjectIdentityMismatch)
        );
        image_store
            .complete_platform_image_multipart_upload(
                image_key,
                &image_upload.upload_id,
                image_size,
                &part_inputs,
            )
            .await?;
        assert!(
            image_store
                .find_platform_image_multipart_uploads(image_key)
                .await?
                .is_empty()
        );
        let image_reference = image_store
            .freeze_current_reference(image_key, image_size, "application/x-tar")
            .await?;
        assert!(!image_reference.object_version.is_empty());

        let abort_key = "problem-packages/platform-image-uploads/multipart-abort-e2e.tar";
        let abort_upload = image_store
            .create_platform_image_multipart_upload(
                abort_key,
                image_part_size + 1,
                "application/x-tar",
                image_now,
            )
            .await?;
        image_store
            .abort_platform_image_multipart_upload(abort_key, &abort_upload.upload_id)
            .await?;
        image_store
            .abort_platform_image_multipart_upload(abort_key, &abort_upload.upload_id)
            .await?;
        assert!(
            image_store
                .find_platform_image_multipart_uploads(abort_key)
                .await?
                .is_empty()
        );
        assert!(image_store.list_key_versions(abort_key).await?.is_empty());
        image_store
            .delete_orphan(image_key, &image_reference.object_version)
            .await?;

        let package_bytes = b"approved package playbook";
        let package_key = "problem-packages/course/package/playbook";
        let package_upload = package_store
            .presign_upload(
                package_key,
                u64::try_from(package_bytes.len())?,
                "text/plain",
                "2026-07-15T08:00:00.000Z".parse::<UtcTimestamp>()?,
            )
            .await?;
        let mut package_request = http.put(&package_upload.url);
        for (name, value) in &package_upload.required_headers {
            package_request = package_request.header(name, value);
        }
        assert!(
            package_request
                .body(package_bytes.to_vec())
                .send()
                .await?
                .status()
                .is_success()
        );
        let package = package_store
            .freeze_current(
                package_key,
                u64::try_from(package_bytes.len())?,
                "text/plain",
            )
            .await?;
        assert_eq!(package.reference.store_binding, package_store.binding());
        assert_eq!(package.bytes, package_bytes);
        let package_reread = package_store
            .read_verified(package_key, &package.reference)
            .await?;
        assert_eq!(package_reread.bytes, package_bytes);
        let package_download = package_store
            .presign_download(
                package_key,
                &package.reference.object_version,
                package.reference.size_bytes,
                &package.reference.media_type,
                "2026-07-15T08:00:00.000Z".parse::<UtcTimestamp>()?,
            )
            .await?;
        assert!(package_download.url.contains("issue-48-packages"));
        let mut package_download_request = http.get(&package_download.url);
        for (name, value) in &package_download.required_headers {
            package_download_request = package_download_request.header(name, value);
        }
        assert_eq!(
            package_download_request
                .send()
                .await?
                .bytes()
                .await?
                .as_ref(),
            package_bytes
        );
        let mut wrong_binding = package.reference.clone();
        wrong_binding.store_binding = store.binding().to_owned();
        assert!(
            package_store
                .read_verified(package_key, &wrong_binding)
                .await
                .is_err()
        );
        store.delete_orphan(key, &version).await?;
        assert!(store.read_verified(key, &frozen.reference).await.is_err());
        let observed_now = time::OffsetDateTime::now_utc();
        let observed_now =
            observed_now.replace_nanosecond(observed_now.nanosecond() / 1_000_000 * 1_000_000)?;
        let now = UtcTimestamp::from_utc(observed_now)?;
        let retain_until = UtcTimestamp::from_utc(observed_now + time::Duration::seconds(60))?;
        let locked_bytes = b"immutable frozen submission";
        let locked_key = "problem-packages/course/submissions/frozen";
        let locked = store
            .put_immutable(
                locked_key,
                locked_bytes,
                "application/vnd.labweaver.frozen-submission.v1+json",
                now,
                Some(retain_until),
            )
            .await?;
        assert_eq!(locked.bytes, locked_bytes);
        assert!(!locked.reference.object_version.is_empty());
        assert!(
            store
                .put_immutable(
                    locked_key,
                    locked_bytes,
                    "application/vnd.labweaver.frozen-submission.v1+json",
                    now,
                    Some(retain_until),
                )
                .await
                .is_err()
        );
        Ok(())
    }

    async fn verify_exact_key_version_cleanup(
        store: &S3ImmutableObjectStore,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let key = "problem-packages/platform-image-uploads/paged-versions.tar";
        let neighbor = format!("{key}.another-upload");
        let unrelated = "problem-packages/another-upload/archive.tar";
        for other_key in [&neighbor, unrelated] {
            store
                .client
                .put_object()
                .bucket(&store.config.bucket)
                .key(other_key)
                .content_type("application/x-tar")
                .body(aws_sdk_s3::primitives::ByteStream::from_static(b"x"))
                .send()
                .await?;
        }
        let mut expected = std::collections::BTreeSet::new();
        for _ in 0..1001 {
            let response = store
                .client
                .put_object()
                .bucket(&store.config.bucket)
                .key(key)
                .content_type("application/x-tar")
                .body(aws_sdk_s3::primitives::ByteStream::from_static(b"x"))
                .send()
                .await?;
            expected.insert(response.version_id().ok_or("version missing")?.to_owned());
        }
        let reference = store
            .freeze_current_reference(key, 1, "application/x-tar")
            .await?;
        assert!(expected.contains(&reference.object_version));
        assert_eq!(
            store
                .freeze_current_reference(key, 2, "application/x-tar")
                .await,
            Err(super::ObjectStoreError::ObjectIdentityMismatch)
        );
        let marker = store
            .client
            .delete_object()
            .bucket(&store.config.bucket)
            .key(key)
            .send()
            .await?;
        expected.insert(
            marker
                .version_id()
                .ok_or("delete marker missing")?
                .to_owned(),
        );
        assert_eq!(
            store
                .freeze_current_reference(key, 1, "application/x-tar")
                .await,
            Err(super::ObjectStoreError::ObjectNotFound)
        );
        let versions = store.list_key_versions(key).await?;
        let observed = versions
            .iter()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(observed.len(), expected.len(), "version count differs");
        assert!(
            expected.difference(&observed).next().is_none(),
            "reviewed version missing from enumeration"
        );
        assert_eq!(
            store.list_key_versions("unowned-key").await,
            Err(super::ObjectStoreError::ObjectIdentityInvalid)
        );
        for version in versions {
            store.delete_orphan(key, &version).await?;
        }
        assert!(store.list_key_versions(key).await?.is_empty());
        for other_key in [&neighbor, unrelated] {
            assert_eq!(store.list_key_versions(other_key).await?.len(), 1);
        }
        Ok(())
    }
}
