//! Immutable S3-compatible object storage used for versioned platform artifacts.

use std::collections::BTreeMap;
use std::time::Duration;

use async_trait::async_trait;
use aws_config::BehaviorVersion;
use aws_credential_types::Credentials;
use aws_sdk_s3::Client;
use aws_sdk_s3::config::{Builder as S3ConfigBuilder, Region};
use aws_sdk_s3::error::ProvideErrorMetadata;
use aws_sdk_s3::presigning::PresigningConfig;
use aws_sdk_s3::primitives::{ByteStream, DateTime};
use aws_sdk_s3::types::ObjectLockMode;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use contracts::{ArtifactId, ArtifactRef, Sha256Digest, UtcTimestamp};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use url::Url;

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
            || self.upload_ttl_seconds > 3_600
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
    /// Signs one conditional immutable upload.
    async fn presign_upload(
        &self,
        key: &str,
        size_bytes: u64,
        sha256: Sha256Digest,
        media_type: &str,
        now: UtcTimestamp,
    ) -> Result<PresignedUpload, ObjectStoreError>;

    /// Downloads one exact object version and verifies raw bytes.
    async fn read_verified(
        &self,
        key: &str,
        version: &str,
        expected_size: u64,
        expected_sha256: Sha256Digest,
        media_type: &str,
    ) -> Result<VerifiedObject, ObjectStoreError>;

    /// Resolves the current upload version once, then verifies and freezes that exact version.
    async fn freeze_current(
        &self,
        key: &str,
        expected_size: u64,
        expected_sha256: Sha256Digest,
        media_type: &str,
    ) -> Result<VerifiedObject, ObjectStoreError>;

    /// Writes bytes once under Governance Object Lock and verifies the exact retained version.
    async fn put_governance_locked(
        &self,
        _key: &str,
        _bytes: &[u8],
        _sha256: Sha256Digest,
        _media_type: &str,
        _now: UtcTimestamp,
        _retain_until: UtcTimestamp,
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
        let shared = aws_config::defaults(BehaviorVersion::latest())
            .region(Region::new(config.region.clone()))
            .credentials_provider(credentials)
            .endpoint_url(config.endpoint.as_str())
            .load()
            .await;
        let service = S3ConfigBuilder::from(&shared)
            .force_path_style(config.force_path_style)
            .build();
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
        sha256: Sha256Digest,
        media_type: &str,
    ) -> Result<VerifiedObject, ObjectStoreError> {
        self.validate_key(key)?;
        let size_bytes =
            u64::try_from(bytes.len()).map_err(|_| ObjectStoreError::ObjectTooLarge)?;
        if bytes.is_empty()
            || size_bytes > self.config.max_object_bytes
            || Sha256Digest::of_bytes(bytes) != sha256
            || media_type.trim().is_empty()
        {
            return Err(ObjectStoreError::ObjectIdentityInvalid);
        }
        let checksum = STANDARD
            .encode(hex_bytes(&sha256.to_string()).ok_or(ObjectStoreError::ObjectIdentityInvalid)?);
        let response = self
            .client
            .put_object()
            .bucket(&self.config.bucket)
            .key(key)
            .content_length(
                i64::try_from(size_bytes).map_err(|_| ObjectStoreError::ObjectTooLarge)?,
            )
            .content_type(media_type)
            .checksum_sha256(checksum)
            .if_none_match("*")
            .metadata("sha256", sha256.to_string())
            .body(ByteStream::from(bytes.to_vec()))
            .send()
            .await
            .map_err(|_| ObjectStoreError::UploadFailed)?;
        let version = response
            .version_id()
            .filter(|value| !value.is_empty() && *value != "null")
            .ok_or(ObjectStoreError::VersioningRequired)?
            .to_owned();
        self.read_verified(key, &version, size_bytes, sha256, media_type)
            .await
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

#[async_trait]
impl ImmutableObjectStore for S3ImmutableObjectStore {
    async fn presign_upload(
        &self,
        key: &str,
        size_bytes: u64,
        sha256: Sha256Digest,
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
        let checksum = STANDARD
            .encode(hex_bytes(&sha256.to_string()).ok_or(ObjectStoreError::ObjectIdentityInvalid)?);
        let expires = Duration::from_secs(self.config.upload_ttl_seconds);
        let request = self
            .client
            .put_object()
            .bucket(&self.config.bucket)
            .key(key)
            .content_length(
                i64::try_from(size_bytes).map_err(|_| ObjectStoreError::ObjectTooLarge)?,
            )
            .content_type(media_type)
            .checksum_sha256(checksum)
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
        version: &str,
        expected_size: u64,
        expected_sha256: Sha256Digest,
        media_type: &str,
    ) -> Result<VerifiedObject, ObjectStoreError> {
        self.validate_key(key)?;
        if version.trim().is_empty() || expected_size == 0 || media_type.trim().is_empty() {
            return Err(ObjectStoreError::ObjectIdentityInvalid);
        }
        let response = match self
            .client
            .get_object()
            .bucket(&self.config.bucket)
            .key(key)
            .version_id(version)
            .send()
            .await
        {
            Ok(response) => response,
            Err(error) => {
                let rendered_error = error.to_string();
                let error_class = rendered_error.split(':').next().unwrap_or("unknown").trim();
                tracing::warn!(
                    event = "artifact_store.get_object_failed",
                    endpoint = %self.config.endpoint,
                    bucket = %self.config.bucket,
                    object_key = %key,
                    object_version = %version,
                    error_class = %error_class,
                    error_source = ?std::error::Error::source(&error).map(ToString::to_string),
                    service_error_code = ?error.as_service_error().and_then(|service| service.code()),
                );
                return Err(ObjectStoreError::ObjectUnavailable);
            }
        };
        let observed_size = response
            .content_length()
            .and_then(|observed| u64::try_from(observed).ok());
        if response
            .version_id()
            .is_none_or(|observed| observed != version)
            || observed_size != Some(expected_size)
            || response
                .content_type()
                .is_none_or(|observed| observed != media_type)
        {
            return Err(ObjectStoreError::ObjectIdentityMismatch);
        }
        let body = match response.body.collect().await {
            Ok(body) => body.into_bytes().to_vec(),
            Err(_) => {
                tracing::warn!(
                    event = "artifact_store.get_object_body_failed",
                    endpoint = %self.config.endpoint,
                    bucket = %self.config.bucket,
                    object_key = %key,
                    object_version = %version,
                );
                return Err(ObjectStoreError::ObjectUnavailable);
            }
        };
        if u64::try_from(body.len()).ok() != Some(expected_size)
            || Sha256Digest::of_bytes(&body) != expected_sha256
        {
            return Err(ObjectStoreError::ObjectIdentityMismatch);
        }
        Ok(VerifiedObject {
            reference: ArtifactRef {
                artifact_id: ArtifactId::new(),
                store_binding: self.config.binding.clone(),
                object_version: version.to_owned(),
                sha256: expected_sha256,
                size_bytes: expected_size,
                media_type: media_type.to_owned(),
            },
            bytes: body,
        })
    }

    async fn freeze_current(
        &self,
        key: &str,
        expected_size: u64,
        expected_sha256: Sha256Digest,
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
        self.read_verified(key, &version, expected_size, expected_sha256, media_type)
            .await
    }

    async fn put_governance_locked(
        &self,
        key: &str,
        bytes: &[u8],
        sha256: Sha256Digest,
        media_type: &str,
        now: UtcTimestamp,
        retain_until: UtcTimestamp,
    ) -> Result<VerifiedObject, ObjectStoreError> {
        self.validate_key(key)?;
        let size_bytes =
            u64::try_from(bytes.len()).map_err(|_| ObjectStoreError::ObjectTooLarge)?;
        if bytes.is_empty()
            || size_bytes > self.config.max_object_bytes
            || Sha256Digest::of_bytes(bytes) != sha256
            || media_type.trim().is_empty()
            || retain_until.get() <= now.get()
        {
            return Err(ObjectStoreError::ObjectIdentityInvalid);
        }
        let checksum = STANDARD
            .encode(hex_bytes(&sha256.to_string()).ok_or(ObjectStoreError::ObjectIdentityInvalid)?);
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
            .checksum_sha256(checksum)
            .if_none_match("*")
            .metadata("sha256", sha256.to_string())
            .object_lock_mode(ObjectLockMode::Governance)
            .object_lock_retain_until_date(retention)
            .body(ByteStream::from(bytes.to_vec()))
            .send()
            .await
            .map_err(|_| ObjectStoreError::UploadFailed)?;
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
            || head
                .metadata()
                .and_then(|metadata| metadata.get("sha256"))
                .is_none_or(|observed| observed != &sha256.to_string())
        {
            return Err(ObjectStoreError::ObjectLockIdentityMismatch);
        }
        self.read_verified(key, &version, size_bytes, sha256, media_type)
            .await
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

fn hex_bytes(value: &str) -> Option<Vec<u8>> {
    if value.len() % 2 != 0 {
        return None;
    }
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let text = std::str::from_utf8(pair).ok()?;
            u8::from_str_radix(text, 16).ok()
        })
        .collect()
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
            Self::ObjectIdentityMismatch => "LW_OBJECT_IDENTITY_MISMATCH",
            Self::DeleteFailed => "LW_OBJECT_CLEANUP_FAILED",
            Self::VersioningRequired => "LW_OBJECT_VERSIONING_REQUIRED",
            Self::ObjectLockRequired => "LW_OBJECT_LOCK_REQUIRED",
            Self::ObjectLockIdentityMismatch => "LW_OBJECT_LOCK_IDENTITY_MISMATCH",
        }
    }
}

#[cfg(test)]
mod tests {
    use aws_sdk_s3::types::{BucketVersioningStatus, VersioningConfiguration};
    use contracts::{Sha256Digest, UtcTimestamp};
    use testcontainers::core::{IntoContainerPort, WaitFor};
    use testcontainers::{GenericImage, ImageExt, runners::AsyncRunner};

    use super::{
        BehaviorVersion, Credentials, ImmutableObjectStore, Region, S3ConfigBuilder,
        S3ImmutableObjectStore, S3StoreConfig, hex_bytes,
    };

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
        };
        config.validate()?;
        config.endpoint = "http://minio.internal.example".parse()?;
        assert!(config.validate().is_err());
        Ok(())
    }

    #[test]
    fn hex_decoder_is_strict() {
        assert_eq!(hex_bytes("00ff"), Some(vec![0, 255]));
        assert_eq!(hex_bytes("0"), None);
        assert_eq!(hex_bytes("zz"), None);
    }

    #[tokio::test]
    #[allow(
        clippy::too_many_lines,
        reason = "one MinIO lifecycle preserves a single exact bucket and object-version identity"
    )]
    async fn minio_versioning_object_lock_and_cleanup_are_fail_closed()
    -> Result<(), Box<dyn std::error::Error>> {
        let minio = GenericImage::new("minio/minio", "RELEASE.2025-04-22T22-12-26Z")
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
            .endpoint_url(endpoint)
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
        let bytes = b"immutable teacher material";
        let digest = Sha256Digest::of_bytes(bytes);
        let key = "problem-packages/course/upload/object";
        let presigned = store
            .presign_upload(
                key,
                u64::try_from(bytes.len())?,
                digest,
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
        let mut overwrite = http.put(&presigned.url);
        for (name, value) in &presigned.required_headers {
            overwrite = overwrite.header(name, value);
        }
        assert_eq!(
            overwrite.body(bytes.to_vec()).send().await?.status(),
            reqwest::StatusCode::PRECONDITION_FAILED
        );

        let frozen = store
            .freeze_current(key, u64::try_from(bytes.len())?, digest, "text/plain")
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
                .freeze_current(key, u64::try_from(bytes.len())?, digest, "text/plain",)
                .await
                .is_err()
        );
        assert_eq!(
            store
                .read_verified(
                    key,
                    &version,
                    u64::try_from(bytes.len())?,
                    digest,
                    "text/plain",
                )
                .await?
                .bytes,
            bytes
        );
        store.delete_orphan(key, &version).await?;
        assert!(
            store
                .read_verified(
                    key,
                    &version,
                    u64::try_from(bytes.len())?,
                    digest,
                    "text/plain",
                )
                .await
                .is_err()
        );
        let observed_now = time::OffsetDateTime::now_utc();
        let observed_now =
            observed_now.replace_nanosecond(observed_now.nanosecond() / 1_000_000 * 1_000_000)?;
        let now = UtcTimestamp::from_utc(observed_now)?;
        let retain_until = UtcTimestamp::from_utc(observed_now + time::Duration::seconds(60))?;
        let locked_bytes = b"immutable frozen submission";
        let locked_digest = Sha256Digest::of_bytes(locked_bytes);
        let locked_key = "problem-packages/course/submissions/frozen";
        let locked = store
            .put_governance_locked(
                locked_key,
                locked_bytes,
                locked_digest,
                "application/vnd.labweaver.frozen-submission.v1+json",
                now,
                retain_until,
            )
            .await?;
        assert_eq!(locked.bytes, locked_bytes);
        assert_eq!(locked.reference.sha256, locked_digest);
        assert!(!locked.reference.object_version.is_empty());
        assert!(
            store
                .put_governance_locked(
                    locked_key,
                    locked_bytes,
                    locked_digest,
                    "application/vnd.labweaver.frozen-submission.v1+json",
                    now,
                    retain_until,
                )
                .await
                .is_err()
        );
        Ok(())
    }
}
