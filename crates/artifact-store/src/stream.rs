//! Bounded file backed reads for large immutable objects.
//!
//! The ordinary object-store API intentionally returns bytes for small control-plane objects.
//! Large VM image imports use this module instead: the S3 body is copied to a temporary file with
//! a fixed-size buffer, while the immutable version, media type, exact length, and observed
//! SHA-256 are checked before the file is handed to the importer.  The temporary path owns its
//! cleanup, including when the async operation is cancelled or fails part way through.

use std::{io::Write, path::Path};

use aws_sdk_s3::operation::get_object::GetObjectOutput;
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::{ArtifactRef, ObjectStoreError};

const COPY_BUFFER_BYTES: usize = 1024 * 1024;

/// One immutable object copied to a temporary file without retaining its payload in memory.
pub struct VerifiedObjectFile {
    reference: ArtifactRef,
    path: tempfile::TempPath,
    sha256: String,
}

impl std::fmt::Debug for VerifiedObjectFile {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("VerifiedObjectFile")
            .field("reference", &self.reference)
            .field("path", &self.path)
            .field("sha256", &self.sha256)
            .finish()
    }
}

impl VerifiedObjectFile {
    /// Creates a file-backed verified object for a byte-oriented adapter.
    ///
    /// Production S3 reads use [`response_to_tempfile`]. This constructor keeps small test and
    /// local adapters on the same cleanup and identity contract without exposing the file fields.
    ///
    /// # Errors
    ///
    /// Returns an object identity error when the byte length differs from the declared reference,
    /// or an availability error when the private temporary file cannot be created or written.
    pub fn from_bytes(reference: ArtifactRef, bytes: &[u8]) -> Result<Self, ObjectStoreError> {
        if u64::try_from(bytes.len()).ok() != Some(reference.size_bytes) {
            return Err(ObjectStoreError::ObjectIdentityMismatch);
        }
        let temporary = NamedTempFile::new().map_err(|_| ObjectStoreError::ObjectUnavailable)?;
        let mut output = temporary
            .reopen()
            .map_err(|_| ObjectStoreError::ObjectUnavailable)?;
        output
            .write_all(bytes)
            .map_err(|_| ObjectStoreError::ObjectUnavailable)?;
        output
            .sync_all()
            .map_err(|_| ObjectStoreError::ObjectUnavailable)?;
        let sha256 = format!("{:x}", Sha256::digest(bytes));
        Ok(Self {
            reference,
            path: temporary.into_temp_path(),
            sha256,
        })
    }

    /// Exact immutable identity returned by the object store.
    #[must_use]
    pub const fn reference(&self) -> &ArtifactRef {
        &self.reference
    }

    /// Temporary file containing exactly the declared object size.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Lowercase SHA-256 observed while copying the object.
    #[must_use]
    pub fn sha256(&self) -> &str {
        &self.sha256
    }

    /// Moves the owned temporary path out for a caller that needs to retain it separately.
    #[must_use]
    pub fn into_path(self) -> tempfile::TempPath {
        self.path
    }
}

/// Streams one already-validated S3 response into a private temporary file.
pub(crate) async fn response_to_tempfile(
    response: GetObjectOutput,
    expected: ArtifactRef,
    max_object_bytes: u64,
) -> Result<VerifiedObjectFile, ObjectStoreError> {
    if expected.size_bytes == 0 || expected.size_bytes > max_object_bytes {
        return Err(ObjectStoreError::ObjectTooLarge);
    }
    if response.version_id() != Some(expected.object_version.as_str())
        || response
            .content_length()
            .and_then(|value| u64::try_from(value).ok())
            != Some(expected.size_bytes)
        || response
            .content_type()
            .is_none_or(|value| value != expected.media_type)
    {
        return Err(ObjectStoreError::ObjectIdentityMismatch);
    }

    let temporary = NamedTempFile::new().map_err(|_| ObjectStoreError::ObjectUnavailable)?;
    let path = temporary.into_temp_path();
    let mut output = tokio::fs::File::create(&path)
        .await
        .map_err(|_| ObjectStoreError::ObjectUnavailable)?;
    let mut input = response.body.into_async_read();
    let mut buffer = vec![0_u8; COPY_BUFFER_BYTES];
    let mut hasher = Sha256::new();
    let mut copied = 0_u64;
    loop {
        let read = input
            .read(&mut buffer)
            .await
            .map_err(|_| ObjectStoreError::ObjectUnavailable)?;
        if read == 0 {
            break;
        }
        copied = copied
            .checked_add(u64::try_from(read).map_err(|_| ObjectStoreError::ObjectTooLarge)?)
            .ok_or(ObjectStoreError::ObjectTooLarge)?;
        if copied > expected.size_bytes {
            return Err(ObjectStoreError::ObjectIdentityMismatch);
        }
        output
            .write_all(&buffer[..read])
            .await
            .map_err(|_| ObjectStoreError::ObjectUnavailable)?;
        hasher.update(&buffer[..read]);
    }
    output
        .flush()
        .await
        .map_err(|_| ObjectStoreError::ObjectUnavailable)?;
    output
        .sync_all()
        .await
        .map_err(|_| ObjectStoreError::ObjectUnavailable)?;
    drop(output);
    if copied != expected.size_bytes {
        return Err(ObjectStoreError::ObjectIdentityMismatch);
    }
    let sha256 = format!("{:x}", hasher.finalize());
    Ok(VerifiedObjectFile {
        reference: expected,
        path,
        sha256,
    })
}

#[cfg(test)]
mod tests {
    use aws_sdk_s3::operation::get_object::GetObjectOutput;
    use aws_sdk_s3::primitives::ByteStream;
    use contracts::{ArtifactId, ArtifactRef};

    use super::response_to_tempfile;

    #[tokio::test]
    async fn streams_exact_object_and_removes_file_when_guard_drops()
    -> Result<(), Box<dyn std::error::Error>> {
        let bytes = b"large object test";
        let expected = ArtifactRef {
            artifact_id: ArtifactId::new(),
            store_binding: "minio-primary".to_owned(),
            object_version: "version-1".to_owned(),
            size_bytes: u64::try_from(bytes.len())?,
            media_type: "application/octet-stream".to_owned(),
        };
        let response = GetObjectOutput::builder()
            .version_id("version-1")
            .content_length(i64::try_from(bytes.len())?)
            .content_type("application/octet-stream")
            .body(ByteStream::from_static(bytes))
            .build();
        let file = response_to_tempfile(response, expected.clone(), 1024).await?;
        let path = file.path().to_owned();
        assert_eq!(std::fs::read(&path)?, bytes);
        assert_eq!(file.reference(), &expected);
        assert_eq!(file.sha256().len(), 64);
        drop(file);
        assert!(!path.exists());
        Ok(())
    }

    #[tokio::test]
    async fn rejects_missing_or_mismatched_version_before_creating_file()
    -> Result<(), Box<dyn std::error::Error>> {
        let expected = ArtifactRef {
            artifact_id: ArtifactId::new(),
            store_binding: "minio-primary".to_owned(),
            object_version: "version-1".to_owned(),
            size_bytes: 1,
            media_type: "text/plain".to_owned(),
        };
        for version in [None, Some("version-2")] {
            let mut builder = GetObjectOutput::builder()
                .content_length(1)
                .content_type("text/plain")
                .body(ByteStream::from_static(b"x"));
            if let Some(version) = version {
                builder = builder.version_id(version);
            }
            let error = response_to_tempfile(builder.build(), expected.clone(), 1024)
                .await
                .err()
                .ok_or("mismatched version was accepted")?;
            assert_eq!(error, crate::ObjectStoreError::ObjectIdentityMismatch);
        }
        Ok(())
    }
}
