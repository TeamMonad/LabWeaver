//! Bounded init-container materialization for immutable evaluation archives.
//!
//! The scheduler gives an attempt a short-lived, version-pinned object URL.  The
//! URL and its request headers are kept in a Secret mounted only into this
//! process.  This worker verifies the complete response before decoding the
//! canonical frozen archive into an `EmptyDir`.  The evaluation worker receives
//! only the resulting read-only directory and never sees the signed URL.
#![allow(
    missing_docs,
    clippy::too_many_lines,
    reason = "the init-container boundary is intentionally explicit for review"
)]

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write as _,
    path::{Path, PathBuf},
    time::Duration,
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use contracts::{parse_strict_json, validate_relative_path};
use persistence_sqlx::Sha256Digest;
use reqwest::{
    Certificate, Client, StatusCode, Url,
    header::{CONTENT_LENGTH, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue},
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Schema identity for the init-container command.
pub const ARTIFACT_MATERIALIZER_SCHEMA_VERSION: &str =
    "evaluation.labweaver.io/artifact-materializer/v1";
/// Environment variable containing the mounted command path.
pub const MATERIALIZER_COMMAND_PATH_ENV: &str = "LABWEAVER_ARTIFACT_MATERIALIZER_COMMAND_FILE";
/// Environment variable containing the mounted object-store CA bundle path.
pub const MATERIALIZER_CA_FILE_ENV: &str = "LABWEAVER_ARTIFACT_MATERIALIZER_CA_FILE";
/// Default Secret mount path used by the generated Jobs.
pub const DEFAULT_MATERIALIZER_COMMAND_PATH: &str = "/run/secrets/materializer/command.json";
/// Fixed path for the optional object-store CA bundle in the materializer Secret.
pub const DEFAULT_MATERIALIZER_CA_FILE: &str = "/run/secrets/materializer/ca.crt";
/// Canonical frozen archive media type.
pub const FROZEN_ARCHIVE_MEDIA_TYPE: &str = "application/vnd.labweaver.frozen-submission.v1+json";

const MAX_COMMAND_BYTES: u64 = 1024 * 1024;
const MAX_ARCHIVE_BYTES: u64 = 96 * 1024 * 1024;
const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_TOTAL_FILE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_ARTIFACTS: usize = 512;
const MAX_FILES_PER_ARCHIVE: usize = 10_000;
const MAX_CA_BYTES: u64 = 1024 * 1024;
const DOWNLOAD_TIMEOUT: Duration = Duration::from_mins(2);

/// Which read-only input root receives one archive.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MaterializeDestination {
    Submission,
    Evaluator,
}

impl MaterializeDestination {
    fn root(self) -> &'static Path {
        match self {
            Self::Submission => Path::new("/input/submission"),
            Self::Evaluator => Path::new("/input/evaluator"),
        }
    }
}

/// The explicitly selected representation of one immutable object.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum MaterializeContent {
    /// Canonical `FrozenSubmission` archive containing its own file list.
    FrozenArchive,
    /// One approved package file written at this exact relative path.
    RawFile { path: String },
}

/// One exact immutable object download.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MaterializeArtifact {
    /// Short-lived HTTPS URL for an exact object version.
    pub url: String,
    /// Additional signed request headers.
    #[serde(default)]
    pub required_headers: BTreeMap<String, String>,
    /// SHA-256 digest of the complete archive response.
    pub expected_sha256: Sha256Digest,
    /// Exact response byte length.
    pub expected_size_bytes: u64,
    /// Exact object media type.
    pub media_type: String,
    /// `EmptyDir` root receiving decoded files.
    pub destination: MaterializeDestination,
    /// Explicit payload representation. Archive versus raw file is never
    /// inferred from media type or object name. Keeping this as a nested
    /// object makes the internal Secret format unambiguous and lets both
    /// structs enforce unknown-field rejection through their derives.
    pub content: MaterializeContent,
}

/// Secret-only command consumed by the artifact materializer init container.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MaterializeCommand {
    pub schema_version: String,
    pub artifacts: Vec<MaterializeArtifact>,
}

impl MaterializeCommand {
    /// Validates the complete command before opening a network connection.
    ///
    /// # Errors
    ///
    /// Returns [`MaterializerError::CommandInvalid`] when the schema, URLs,
    /// headers, sizes, destinations, or content paths are invalid.
    pub fn validate(&self) -> Result<(), MaterializerError> {
        if self.schema_version != ARTIFACT_MATERIALIZER_SCHEMA_VERSION
            || self.artifacts.is_empty()
            || self.artifacts.len() > MAX_ARTIFACTS
        {
            return Err(MaterializerError::CommandInvalid);
        }
        let mut destinations = BTreeSet::new();
        let mut paths = BTreeSet::new();
        for artifact in &self.artifacts {
            let url = Url::parse(&artifact.url).map_err(|_| MaterializerError::CommandInvalid)?;
            if url.scheme() != "https"
                || url.host_str().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
                || url.fragment().is_some()
                || artifact.expected_size_bytes == 0
                || artifact.expected_size_bytes > MAX_ARCHIVE_BYTES
                || artifact.media_type.trim().is_empty()
            {
                return Err(MaterializerError::CommandInvalid);
            }
            match &artifact.content {
                MaterializeContent::FrozenArchive
                    if artifact.media_type != FROZEN_ARCHIVE_MEDIA_TYPE =>
                {
                    return Err(MaterializerError::CommandInvalid);
                }
                MaterializeContent::RawFile { path } => {
                    validate_relative_path(path).map_err(|_| MaterializerError::CommandInvalid)?;
                    if !paths.insert((artifact.destination as u8, path.as_str())) {
                        return Err(MaterializerError::CommandInvalid);
                    }
                }
                MaterializeContent::FrozenArchive => {}
            }
            for (name, value) in &artifact.required_headers {
                HeaderName::from_bytes(name.as_bytes())
                    .map_err(|_| MaterializerError::CommandInvalid)?;
                HeaderValue::from_str(value).map_err(|_| MaterializerError::CommandInvalid)?;
            }
            if matches!(artifact.content, MaterializeContent::FrozenArchive)
                && !destinations.insert(artifact.destination as u8)
            {
                return Err(MaterializerError::CommandInvalid);
            }
        }
        Ok(())
    }
}

/// Runs the init-container download and archive decoder.
///
/// # Errors
///
/// Returns a materializer error when the mounted command, object-store
/// response, archive, or destination filesystem fails validation or access.
pub async fn run_artifact_materializer() -> Result<(), MaterializerError> {
    let path = std::env::var_os(MATERIALIZER_COMMAND_PATH_ENV).map_or_else(
        || PathBuf::from(DEFAULT_MATERIALIZER_COMMAND_PATH),
        PathBuf::from,
    );
    let command = read_command(&path)?;
    command.validate()?;
    let ca_path = std::env::var_os(MATERIALIZER_CA_FILE_ENV).map(PathBuf::from);
    let client = build_client(ca_path.as_deref())?;
    for artifact in &command.artifacts {
        let body = download(&client, artifact).await?;
        match &artifact.content {
            MaterializeContent::RawFile { path } => {
                materialize_file(artifact.destination.root(), path, &body)?;
            }
            MaterializeContent::FrozenArchive => {
                let archive = decode_archive(&body, artifact)?;
                materialize_archive(artifact.destination.root(), archive)?;
            }
        }
    }
    Ok(())
}

fn build_client(ca_path: Option<&Path>) -> Result<Client, MaterializerError> {
    let mut builder = Client::builder()
        .no_proxy()
        .https_only(true)
        .redirect(reqwest::redirect::Policy::none())
        .timeout(DOWNLOAD_TIMEOUT);
    if let Some(ca_path) = ca_path {
        if !ca_path.is_absolute() {
            return Err(MaterializerError::ConfigurationInvalid);
        }
        let ca_bundle = read_bounded_file(ca_path, MAX_CA_BYTES)?;
        let mut pem = &ca_bundle[..];
        let certificates = rustls_pemfile::certs(&mut pem)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| MaterializerError::ConfigurationInvalid)?;
        if certificates.is_empty() || !pem.is_empty() {
            return Err(MaterializerError::ConfigurationInvalid);
        }
        builder = builder.tls_built_in_root_certs(false);
        for certificate in certificates {
            let certificate = Certificate::from_der(certificate.as_ref())
                .map_err(|_| MaterializerError::ConfigurationInvalid)?;
            builder = builder.add_root_certificate(certificate);
        }
    }
    builder
        .build()
        .map_err(|_| MaterializerError::ConfigurationInvalid)
}

fn read_bounded_file(path: &Path, max_bytes: u64) -> Result<Vec<u8>, MaterializerError> {
    let metadata = fs::metadata(path).map_err(|_| MaterializerError::ConfigurationInvalid)?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > max_bytes {
        return Err(MaterializerError::ConfigurationInvalid);
    }
    let bytes = fs::read(path).map_err(|_| MaterializerError::ConfigurationInvalid)?;
    if u64::try_from(bytes.len()).ok() != Some(metadata.len()) {
        return Err(MaterializerError::ConfigurationInvalid);
    }
    Ok(bytes)
}

fn read_command(path: &Path) -> Result<MaterializeCommand, MaterializerError> {
    let metadata = fs::metadata(path).map_err(|_| MaterializerError::CommandUnavailable)?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_COMMAND_BYTES {
        return Err(MaterializerError::CommandInvalid);
    }
    let bytes = fs::read(path).map_err(|_| MaterializerError::CommandUnavailable)?;
    if bytes.is_empty() || u64::try_from(bytes.len()).ok() != Some(metadata.len()) {
        return Err(MaterializerError::CommandInvalid);
    }
    parse_strict_json(&bytes).map_err(|_| MaterializerError::CommandInvalid)
}

async fn download(
    client: &Client,
    artifact: &MaterializeArtifact,
) -> Result<Vec<u8>, MaterializerError> {
    let url = Url::parse(&artifact.url).map_err(|_| MaterializerError::CommandInvalid)?;
    let mut headers = HeaderMap::new();
    for (name, value) in &artifact.required_headers {
        let name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| MaterializerError::CommandInvalid)?;
        let value = HeaderValue::from_str(value).map_err(|_| MaterializerError::CommandInvalid)?;
        headers.insert(name, value);
    }
    let response = client
        .get(url)
        .headers(headers)
        .send()
        .await
        .map_err(|_| MaterializerError::DownloadUnavailable)?;
    if response.status() != StatusCode::OK {
        return Err(MaterializerError::DownloadRejected);
    }
    if response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        != Some(artifact.media_type.as_str())
    {
        return Err(MaterializerError::DownloadIdentityMismatch);
    }
    if response
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .is_some_and(|length| length != artifact.expected_size_bytes)
    {
        return Err(MaterializerError::DownloadIdentityMismatch);
    }
    let mut stream = response.bytes_stream();
    let mut body = Vec::with_capacity(
        usize::try_from(artifact.expected_size_bytes)
            .map_err(|_| MaterializerError::ArchiveTooLarge)?,
    );
    while let Some(chunk) = futures_util::StreamExt::next(&mut stream).await {
        let chunk = chunk.map_err(|_| MaterializerError::DownloadUnavailable)?;
        let next = body
            .len()
            .checked_add(chunk.len())
            .ok_or(MaterializerError::ArchiveTooLarge)?;
        if u64::try_from(next).map_or(true, |size| size > artifact.expected_size_bytes) {
            return Err(MaterializerError::DownloadIdentityMismatch);
        }
        body.extend_from_slice(&chunk);
    }
    if u64::try_from(body.len()).ok() != Some(artifact.expected_size_bytes)
        || Sha256Digest::of_bytes(&body) != artifact.expected_sha256
    {
        return Err(MaterializerError::DownloadIdentityMismatch);
    }
    Ok(body)
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FrozenArchive {
    #[serde(rename = "apiVersion")]
    api_version: String,
    files: Vec<FrozenArchiveFile>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FrozenArchiveFile {
    path: String,
    #[serde(rename = "contentBase64")]
    content_base64: String,
}

fn decode_archive(
    body: &[u8],
    artifact: &MaterializeArtifact,
) -> Result<FrozenArchive, MaterializerError> {
    if u64::try_from(body.len()).map_or(true, |size| size > MAX_ARCHIVE_BYTES)
        || body.len() as u64 != artifact.expected_size_bytes
    {
        return Err(MaterializerError::ArchiveTooLarge);
    }
    let archive: FrozenArchive =
        parse_strict_json(body).map_err(|_| MaterializerError::ArchiveInvalid)?;
    if archive.api_version != "evaluation.labweaver.io/frozen-submission-archive/v1"
        || archive.files.is_empty()
        || archive.files.len() > MAX_FILES_PER_ARCHIVE
        || serde_jcs::to_vec(&archive).map_err(|_| MaterializerError::ArchiveInvalid)? != body
    {
        return Err(MaterializerError::ArchiveInvalid);
    }
    let mut previous: Option<&str> = None;
    let mut total = 0_u64;
    for file in &archive.files {
        validate_relative_path(&file.path).map_err(|_| MaterializerError::ArchiveInvalid)?;
        if previous.is_some_and(|path| path >= file.path.as_str()) {
            return Err(MaterializerError::ArchiveInvalid);
        }
        previous = Some(&file.path);
        let bytes = STANDARD
            .decode(&file.content_base64)
            .map_err(|_| MaterializerError::ArchiveInvalid)?;
        let size = u64::try_from(bytes.len()).map_err(|_| MaterializerError::ArchiveTooLarge)?;
        if size > MAX_FILE_BYTES {
            return Err(MaterializerError::ArchiveTooLarge);
        }
        total = total
            .checked_add(size)
            .filter(|size| *size <= MAX_TOTAL_FILE_BYTES)
            .ok_or(MaterializerError::ArchiveTooLarge)?;
    }
    Ok(archive)
}

fn materialize_archive(root: &Path, archive: FrozenArchive) -> Result<(), MaterializerError> {
    ensure_directory(root)?;
    for file in archive.files {
        let target = root.join(&file.path);
        ensure_parent_directories(
            root,
            target.parent().ok_or(MaterializerError::WorkspaceInvalid)?,
        )?;
        let bytes = STANDARD
            .decode(&file.content_base64)
            .map_err(|_| MaterializerError::ArchiveInvalid)?;
        let mut output = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)
            .map_err(|_| MaterializerError::WorkspaceInvalid)?;
        output
            .write_all(&bytes)
            .map_err(|_| MaterializerError::WorkspaceInvalid)?;
        output
            .sync_all()
            .map_err(|_| MaterializerError::WorkspaceInvalid)?;
    }
    Ok(())
}

fn materialize_file(root: &Path, path: &str, bytes: &[u8]) -> Result<(), MaterializerError> {
    validate_relative_path(path).map_err(|_| MaterializerError::CommandInvalid)?;
    ensure_directory(root)?;
    let target = root.join(path);
    ensure_parent_directories(
        root,
        target.parent().ok_or(MaterializerError::WorkspaceInvalid)?,
    )?;
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&target)
        .map_err(|_| MaterializerError::WorkspaceInvalid)?;
    output
        .write_all(bytes)
        .map_err(|_| MaterializerError::WorkspaceInvalid)?;
    output
        .sync_all()
        .map_err(|_| MaterializerError::WorkspaceInvalid)
}

fn ensure_directory(path: &Path) -> Result<(), MaterializerError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(path).map_err(|_| MaterializerError::WorkspaceInvalid)
        }
        Ok(_) | Err(_) => Err(MaterializerError::WorkspaceInvalid),
    }
}

fn ensure_parent_directories(root: &Path, parent: &Path) -> Result<(), MaterializerError> {
    let relative = parent
        .strip_prefix(root)
        .map_err(|_| MaterializerError::WorkspaceInvalid)?;
    let mut current = root.to_owned();
    for component in relative.components() {
        let std::path::Component::Normal(component) = component else {
            return Err(MaterializerError::WorkspaceInvalid);
        };
        current.push(component);
        ensure_directory(&current)?;
    }
    Ok(())
}

/// Stable init-container failures.  None of these messages include a signed URL.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum MaterializerError {
    #[error("artifact materializer configuration is invalid")]
    ConfigurationInvalid,
    #[error("artifact materializer command is unavailable")]
    CommandUnavailable,
    #[error("artifact materializer command is invalid")]
    CommandInvalid,
    #[error("artifact materializer download is unavailable")]
    DownloadUnavailable,
    #[error("artifact materializer download was rejected")]
    DownloadRejected,
    #[error("artifact materializer object identity mismatched")]
    DownloadIdentityMismatch,
    #[error("artifact materializer archive is invalid")]
    ArchiveInvalid,
    #[error("artifact materializer archive exceeds limits")]
    ArchiveTooLarge,
    #[error("artifact materializer workspace is invalid")]
    WorkspaceInvalid,
}

impl MaterializerError {
    #[must_use]
    pub const fn diagnostic_code(&self) -> &'static str {
        match self {
            Self::ConfigurationInvalid => "LW_ARTIFACT_MATERIALIZER_CONFIG_INVALID",
            Self::CommandUnavailable => "LW_ARTIFACT_MATERIALIZER_COMMAND_UNAVAILABLE",
            Self::CommandInvalid => "LW_ARTIFACT_MATERIALIZER_COMMAND_INVALID",
            Self::DownloadUnavailable => "LW_ARTIFACT_MATERIALIZER_DOWNLOAD_UNAVAILABLE",
            Self::DownloadRejected => "LW_ARTIFACT_MATERIALIZER_DOWNLOAD_REJECTED",
            Self::DownloadIdentityMismatch => "LW_ARTIFACT_MATERIALIZER_DOWNLOAD_IDENTITY_MISMATCH",
            Self::ArchiveInvalid => "LW_ARTIFACT_MATERIALIZER_ARCHIVE_INVALID",
            Self::ArchiveTooLarge => "LW_ARTIFACT_MATERIALIZER_ARCHIVE_TOO_LARGE",
            Self::WorkspaceInvalid => "LW_ARTIFACT_MATERIALIZER_WORKSPACE_INVALID",
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        error::Error,
        io::{Cursor, Error as IoError, Write as _},
        sync::Arc,
    };

    use rcgen::{
        BasicConstraints, CertificateParams, CertifiedIssuer, ExtendedKeyUsagePurpose, IsCa,
        KeyPair, KeyUsagePurpose,
    };
    use rustls::{ServerConfig, pki_types::PrivateKeyDer};
    use tempfile::NamedTempFile;
    use tokio::{
        io::{AsyncReadExt as _, AsyncWriteExt as _},
        net::TcpListener,
    };
    use tokio_rustls::TlsAcceptor;

    use super::*;

    #[allow(
        clippy::expect_used,
        reason = "the in-memory test fixture must fail immediately if its canonical archive cannot be encoded"
    )]
    fn archive() -> Vec<u8> {
        serde_jcs::to_vec(&FrozenArchive {
            api_version: "evaluation.labweaver.io/frozen-submission-archive/v1".to_owned(),
            files: vec![FrozenArchiveFile {
                path: "src/main.cpp".to_owned(),
                content_base64: STANDARD.encode(b"int main() {}\n"),
            }],
        })
        .expect("archive serializes")
    }

    fn artifact(body: &[u8], destination: MaterializeDestination) -> MaterializeArtifact {
        MaterializeArtifact {
            url: "https://objects.example.test/bucket/archive?X-Amz-Signature=signed".to_owned(),
            required_headers: BTreeMap::new(),
            expected_sha256: Sha256Digest::of_bytes(body),
            expected_size_bytes: body.len() as u64,
            media_type: FROZEN_ARCHIVE_MEDIA_TYPE.to_owned(),
            destination,
            content: MaterializeContent::FrozenArchive,
        }
    }

    #[test]
    #[allow(
        clippy::expect_used,
        reason = "serialization failures invalidate this in-memory contract fixture"
    )]
    fn artifact_roundtrips_with_nested_content_and_rejects_unknown_fields() {
        let body = archive();
        let artifact = artifact(&body, MaterializeDestination::Submission);
        let encoded = serde_json::to_value(&artifact).expect("artifact serializes");
        assert_eq!(
            encoded.get("content").and_then(|value| value.get("kind")),
            Some(&serde_json::json!("frozen_archive"))
        );
        assert!(encoded.get("kind").is_none());
        assert_eq!(
            serde_json::from_value::<MaterializeArtifact>(encoded.clone())
                .expect("artifact deserializes"),
            artifact
        );

        let mut flattened = encoded.clone();
        flattened["kind"] = serde_json::json!("frozen_archive");
        assert!(serde_json::from_value::<MaterializeArtifact>(flattened).is_err());

        let mut raw_artifact = artifact;
        raw_artifact.media_type = "application/json".to_owned();
        raw_artifact.content = MaterializeContent::RawFile {
            path: "profile.json".to_owned(),
        };
        let mut nested_unknown =
            serde_json::to_value(raw_artifact).expect("raw artifact serializes");
        nested_unknown["content"]["unexpected"] = serde_json::json!(true);
        assert!(serde_json::from_value::<MaterializeArtifact>(nested_unknown).is_err());
    }

    #[test]
    fn command_rejects_plaintext_urls_duplicate_destinations_and_wrong_media_type() {
        let body = archive();
        let mut command = MaterializeCommand {
            schema_version: ARTIFACT_MATERIALIZER_SCHEMA_VERSION.to_owned(),
            artifacts: vec![artifact(&body, MaterializeDestination::Submission)],
        };
        assert!(command.validate().is_ok());
        command.artifacts[0].url = "http://objects.example.test/archive".to_owned();
        assert_eq!(command.validate(), Err(MaterializerError::CommandInvalid));
        command.artifacts[0].url =
            "https://objects.example.test/bucket/archive?X-Amz-Signature=signed".to_owned();
        command
            .artifacts
            .push(artifact(&body, MaterializeDestination::Submission));
        assert_eq!(command.validate(), Err(MaterializerError::CommandInvalid));
        command.artifacts.pop();
        command.artifacts[0].media_type = "application/octet-stream".to_owned();
        assert_eq!(command.validate(), Err(MaterializerError::CommandInvalid));
    }

    #[test]
    #[allow(
        clippy::expect_used,
        reason = "serialization failures invalidate this in-memory archive fixture"
    )]
    fn canonical_archive_rejects_path_traversal_duplicates_and_noncanonical_json() {
        let body = archive();
        let mut item = artifact(&body, MaterializeDestination::Submission);
        assert!(decode_archive(&body, &item).is_ok());
        let mut value: serde_json::Value = serde_json::from_slice(&body).expect("valid json");
        value["files"][0]["path"] = serde_json::json!("../escape");
        let body = serde_jcs::to_vec(&value).expect("canonical json");
        item.expected_size_bytes = body.len() as u64;
        item.expected_sha256 = Sha256Digest::of_bytes(&body);
        assert!(matches!(
            decode_archive(&body, &item),
            Err(MaterializerError::ArchiveInvalid)
        ));
        let mut noncanonical = archive();
        noncanonical.push(b' ');
        item.expected_size_bytes = noncanonical.len() as u64;
        item.expected_sha256 = Sha256Digest::of_bytes(&noncanonical);
        assert!(matches!(
            decode_archive(&noncanonical, &item),
            Err(MaterializerError::ArchiveInvalid)
        ));
    }

    #[tokio::test]
    async fn configured_ca_trusts_the_object_store_and_rejects_wrong_ca_or_hostname()
    -> Result<(), Box<dyn Error>> {
        let ca = test_ca()?;
        let (certificate_pem, private_key_pem) = leaf_certificate(&ca)?;
        let trusted_ca_file = ca_file(ca.pem().as_bytes())?;
        let client = build_client(Some(trusted_ca_file.path()))?;

        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let address = listener.local_addr()?;
        let server = tokio::spawn(serve_once(
            listener,
            tls_config(&certificate_pem, &private_key_pem)?,
        ));
        let response = tokio::time::timeout(
            Duration::from_secs(5),
            client
                .get(format!("https://localhost:{}", address.port()))
                .send(),
        )
        .await??;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.text().await?, "ok");
        let _ = tokio::time::timeout(Duration::from_secs(5), server).await??;

        let wrong_ca = test_ca()?;
        let wrong_ca_file = ca_file(wrong_ca.pem().as_bytes())?;
        let wrong_client = build_client(Some(wrong_ca_file.path()))?;
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let address = listener.local_addr()?;
        let server = tokio::spawn(serve_once(
            listener,
            tls_config(&certificate_pem, &private_key_pem)?,
        ));
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            wrong_client
                .get(format!("https://localhost:{}", address.port()))
                .send(),
        )
        .await?;
        assert!(
            result.is_err(),
            "an unrelated CA must not authenticate the endpoint"
        );
        let _ = tokio::time::timeout(Duration::from_secs(5), server).await?;

        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let address = listener.local_addr()?;
        let server = tokio::spawn(serve_once(
            listener,
            tls_config(&certificate_pem, &private_key_pem)?,
        ));
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            client
                .get(format!("https://127.0.0.1:{}", address.port()))
                .send(),
        )
        .await?;
        assert!(
            result.is_err(),
            "a hostname mismatch must reject the endpoint"
        );
        let _ = tokio::time::timeout(Duration::from_secs(5), server).await?;
        Ok(())
    }

    fn ca_file(pem: &[u8]) -> Result<NamedTempFile, std::io::Error> {
        let mut file = NamedTempFile::new()?;
        file.write_all(pem)?;
        file.flush()?;
        Ok(file)
    }

    async fn serve_once(
        listener: TcpListener,
        config: Arc<ServerConfig>,
    ) -> Result<(), std::io::Error> {
        let acceptor = TlsAcceptor::from(config);
        let (stream, _) = listener.accept().await?;
        let mut stream = acceptor
            .accept(stream)
            .await
            .map_err(|error| IoError::other(error.to_string()))?;
        let mut request = [0_u8; 4096];
        let _ = stream.read(&mut request).await?;
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
            )
            .await?;
        Ok(())
    }

    fn tls_config(
        certificate_pem: &str,
        private_key_pem: &str,
    ) -> Result<Arc<ServerConfig>, Box<dyn Error>> {
        let certificates = rustls_pemfile::certs(&mut Cursor::new(certificate_pem.as_bytes()))
            .collect::<Result<Vec<_>, _>>()?;
        let key: PrivateKeyDer<'static> =
            rustls_pemfile::private_key(&mut Cursor::new(private_key_pem.as_bytes()))?
                .ok_or("private key missing")?;
        let mut config = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certificates, key)?;
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        Ok(Arc::new(config))
    }

    fn test_ca() -> Result<CertifiedIssuer<'static, KeyPair>, rcgen::Error> {
        let mut parameters = CertificateParams::new(Vec::<String>::new())?;
        parameters.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        parameters.key_usages = vec![
            KeyUsagePurpose::DigitalSignature,
            KeyUsagePurpose::KeyCertSign,
            KeyUsagePurpose::CrlSign,
        ];
        CertifiedIssuer::self_signed(parameters, KeyPair::generate()?)
    }

    fn leaf_certificate(
        ca: &CertifiedIssuer<'static, KeyPair>,
    ) -> Result<(String, String), rcgen::Error> {
        let mut parameters = CertificateParams::new(vec!["localhost".to_owned()])?;
        parameters.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        parameters.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        let key = KeyPair::generate()?;
        let certificate = parameters.signed_by(&key, ca)?;
        Ok((certificate.pem(), key.serialize_pem()))
    }
}
