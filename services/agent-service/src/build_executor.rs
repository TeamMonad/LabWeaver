//! Fixed-command `BuildKit`, Harbor, and Trivy executor backend.
#![allow(
    missing_docs,
    reason = "the deployment schema and stable failure codes document internal executor bindings"
)]

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use artifact_store::{ImmutableObjectStore, S3ImmutableObjectStore};
use async_trait::async_trait;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use contracts::events::AgentBuildRequested;
use contracts::supply_chain::VulnerabilitySummary;
use contracts::{BuildRequestId, Sha256Digest};
use flate2::read::GzDecoder;
use reqwest::{Certificate, Client, StatusCode, Url};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::{PgPool, Row};
use tempfile::TempDir;
use tokio::process::Command;

use crate::build_pipeline::{
    BuildIdentity, BuildProviderFailure, BuildProviderFailureCode, BuildProviderRequestContext,
    BuiltCandidate, PrivateRegistryProject, PublishedImage, ScanEvidence,
};
use crate::build_provider::{BuildExecutorBackend, BuildExecutorRequest, BuildExecutorResponse};

const MAX_DOCKERFILE_BYTES: u64 = 256 * 1024;
const MAX_CONTEXT_ENTRIES: usize = 10_000;
const MAX_HARBOR_RESPONSE_BYTES: usize = 1024 * 1024;

/// Deployment-owned, non-secret executor bindings.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProductionBuildExecutorConfig {
    pub buildctl_path: PathBuf,
    pub buildkit_address: String,
    pub buildkit_ca_file: PathBuf,
    pub buildkit_client_certificate_file: PathBuf,
    pub buildkit_client_private_key_file: PathBuf,
    pub trivy_path: PathBuf,
    pub trivy_cache_directory: PathBuf,
    pub docker_config_directory: PathBuf,
    pub work_directory: PathBuf,
    pub max_unpacked_context_bytes: u64,
    pub harbor_api: Url,
    pub harbor_registry: String,
    pub harbor_ca_file: PathBuf,
    pub harbor_username_file: PathBuf,
    pub harbor_password_file: PathBuf,
    pub project_storage_quota_bytes: u64,
    pub robot_subject: String,
    pub scanner_name: String,
    pub scanner_version: String,
    pub scanner_database_repository: String,
    pub scanner_database_sha256: Sha256Digest,
}

impl ProductionBuildExecutorConfig {
    fn validate(&self) -> Result<(), BuildProviderFailure> {
        if !self.buildctl_path.is_absolute()
            || !self.trivy_path.is_absolute()
            || !self.buildkit_ca_file.is_absolute()
            || !self.buildkit_client_certificate_file.is_absolute()
            || !self.buildkit_client_private_key_file.is_absolute()
            || !self.trivy_cache_directory.is_absolute()
            || !self.docker_config_directory.is_absolute()
            || !self.work_directory.is_absolute()
            || self.max_unpacked_context_bytes == 0
            || !self.buildkit_address.starts_with("tcp://")
            || self
                .buildkit_address
                .bytes()
                .any(|byte| byte.is_ascii_whitespace())
            || self.harbor_api.scheme() != "https"
            || self.harbor_api.host_str().is_none()
            || !self.harbor_ca_file.is_absolute()
            || self.harbor_registry.contains('/')
            || self.harbor_registry.contains("//")
            || self.project_storage_quota_bytes == 0
            || self.robot_subject.trim().is_empty()
            || self.scanner_name != "trivy"
            || self.scanner_version.trim().is_empty()
            || !valid_database_reference(&self.scanner_database_repository, &self.harbor_registry)
            || self.scanner_database_sha256 == Sha256Digest::of_bytes(&[])
        {
            return Err(rejected());
        }
        Ok(())
    }
}

/// Production backend. It never accepts a command string or invokes a shell.
pub struct ProductionBuildExecutor {
    config: ProductionBuildExecutorConfig,
    pool: PgPool,
    objects: Arc<S3ImmutableObjectStore>,
    client: Client,
    harbor_username: String,
    harbor_password: String,
}

impl ProductionBuildExecutor {
    pub fn new(
        config: ProductionBuildExecutorConfig,
        pool: PgPool,
        objects: Arc<S3ImmutableObjectStore>,
    ) -> Result<Self, BuildProviderFailure> {
        config.validate()?;
        for path in [
            &config.buildkit_ca_file,
            &config.buildkit_client_certificate_file,
            &config.buildkit_client_private_key_file,
        ] {
            read_secret(path)?;
        }
        let harbor_username = read_secret(&config.harbor_username_file)?;
        let harbor_password = read_secret(&config.harbor_password_file)?;
        prepare_private_directory(&config.work_directory)?;
        prepare_private_directory(&config.trivy_cache_directory)?;
        prepare_docker_config(
            &config.docker_config_directory,
            &config.harbor_registry,
            &harbor_username,
            &harbor_password,
        )?;
        let harbor_ca = std::fs::read(&config.harbor_ca_file).map_err(|_| rejected())?;
        let harbor_ca = Certificate::from_pem(&harbor_ca).map_err(|_| rejected())?;
        let client = Client::builder()
            .https_only(true)
            .timeout(std::time::Duration::from_secs(30))
            .add_root_certificate(harbor_ca)
            .build()
            .map_err(|_| rejected())?;
        Ok(Self {
            config,
            pool,
            objects,
            client,
            harbor_username,
            harbor_password,
        })
    }

    async fn execute_inner(
        &self,
        context: &BuildProviderRequestContext,
        request: &BuildExecutorRequest,
    ) -> Result<BuildExecutorResponse, BuildProviderFailure> {
        match request {
            BuildExecutorRequest::EnsurePrivateProject { command, identity } => self
                .ensure_private_project(command, *identity)
                .await
                .map(|project| BuildExecutorResponse::PrivateProjectReady { project }),
            BuildExecutorRequest::Build { command, identity } => self
                .build(context, command, *identity)
                .await
                .map(|candidate| BuildExecutorResponse::Built { candidate }),
            BuildExecutorRequest::Scan { candidate } => self
                .scan(candidate)
                .await
                .map(|evidence| BuildExecutorResponse::Scanned { evidence }),
            BuildExecutorRequest::Publish { candidate } => self
                .publish(candidate)
                .await
                .map(|image| BuildExecutorResponse::Published { image }),
            BuildExecutorRequest::Cleanup {
                build_request_id,
                identity,
            } => {
                self.cleanup(*build_request_id, *identity).await?;
                Ok(BuildExecutorResponse::Cleaned {
                    build_request_id: *build_request_id,
                    build_identity: *identity,
                })
            }
        }
    }

    async fn ensure_private_project(
        &self,
        command: &AgentBuildRequested,
        identity: BuildIdentity,
    ) -> Result<PrivateRegistryProject, BuildProviderFailure> {
        let repository = RepositoryIdentity::parse(
            &command.request.output_repository,
            &self.config.harbor_registry,
        )?;
        // Deployment reconciles project privacy, quota, and robot membership with
        // Harbor administrator credentials. This runtime deliberately receives only
        // the scoped robot credential, for which Harbor's project API returns 403.
        // Verify the adopted endpoint here; BuildKit's push is the authoritative
        // scoped-credential and repository permission check in the following stage.
        self.client
            .get(self.harbor_url(&["health"])?)
            .send()
            .await
            .map_err(network)?
            .error_for_status()
            .map_err(network)?;
        Ok(PrivateRegistryProject {
            build_request_id: command.request.id,
            build_identity: identity,
            repository_prefix: format!("{}/{}", self.config.harbor_registry, repository.project),
            private: true,
            storage_quota_bytes: self.config.project_storage_quota_bytes,
            robot_subject: self.config.robot_subject.clone(),
        })
    }

    async fn build(
        &self,
        context: &BuildProviderRequestContext,
        command: &AgentBuildRequested,
        identity: BuildIdentity,
    ) -> Result<BuiltCandidate, BuildProviderFailure> {
        let repository = RepositoryIdentity::parse(
            &command.request.output_repository,
            &self.config.harbor_registry,
        )?;
        let object = match self
            .objects
            .read_verified(
                &command.request.context_object_key,
                &command.request.context.object_version,
                command.request.context.size_bytes,
                command.request.context.sha256,
                &command.request.context.media_type,
            )
            .await
        {
            Ok(object) => object,
            Err(error) => {
                tracing::warn!(
                    event = "agent.build_executor.context_rejected",
                    build_request_id = %context.build_request_id,
                    object_key = %command.request.context_object_key,
                    object_version = %command.request.context.object_version,
                    expected_size_bytes = command.request.context.size_bytes,
                    expected_media_type = %command.request.context.media_type,
                    diagnostic_code = error.diagnostic_code(),
                );
                return Err(rejected());
            }
        };
        let workspace = TempDir::new_in(&self.config.work_directory).map_err(|_| unavailable())?;
        if let Err(failure) = unpack_context(
            &object.bytes,
            &command.request.context.media_type,
            workspace.path(),
            self.config.max_unpacked_context_bytes,
        ) {
            tracing::warn!(
                event = "agent.build_executor.context_unpack_rejected",
                build_request_id = %context.build_request_id,
                code = ?failure.code,
            );
            return Err(failure);
        }
        if let Err(failure) = validate_dockerfile(
            workspace.path(),
            &command.request.dockerfile_path,
            &command.request.base_image_digest,
        ) {
            tracing::warn!(
                event = "agent.build_executor.dockerfile_rejected",
                build_request_id = %context.build_request_id,
                dockerfile_path = %command.request.dockerfile_path,
                code = ?failure.code,
            );
            return Err(failure);
        }
        let tag = candidate_tag(identity);
        let tagged = format!("{}:{tag}", command.request.output_repository);
        let metadata = workspace.path().join("build-metadata.json");
        self.run_buildctl(
            workspace.path(),
            &command.request.dockerfile_path,
            &tagged,
            &metadata,
        )
        .await?;
        let metadata: Value = serde_json::from_slice(
            &tokio::fs::read(&metadata)
                .await
                .map_err(|_| output_invalid())?,
        )
        .map_err(|_| output_invalid())?;
        let digest = metadata
            .get("containerimage.digest")
            .and_then(Value::as_str)
            .filter(|value| valid_digest(value))
            .ok_or_else(output_invalid)?
            .to_owned();
        sqlx::query(
            "INSERT INTO agent.build_executor_artifacts \
             (build_request_id,build_identity,repository,project_name,repository_name,candidate_tag,digest) \
             VALUES ($1,$2,$3,$4,$5,$6,$7) \
             ON CONFLICT (build_request_id) DO UPDATE SET \
               build_identity=EXCLUDED.build_identity,repository=EXCLUDED.repository, \
               project_name=EXCLUDED.project_name,repository_name=EXCLUDED.repository_name, \
               candidate_tag=EXCLUDED.candidate_tag,digest=EXCLUDED.digest,cleaned_at=NULL, \
               updated_at=clock_timestamp()",
        )
        .bind(context.build_request_id.as_uuid())
        .bind(identity.0.to_string())
        .bind(&command.request.output_repository)
        .bind(&repository.project)
        .bind(&repository.repository)
        .bind(&tag)
        .bind(&digest)
        .execute(&self.pool)
        .await
        .map_err(|_| unavailable())?;
        Ok(BuiltCandidate {
            build_request_id: command.request.id,
            build_identity: identity,
            repository: command.request.output_repository.clone(),
            digest,
        })
    }

    async fn run_buildctl(
        &self,
        workspace: &Path,
        dockerfile_path: &str,
        tagged: &str,
        metadata: &Path,
    ) -> Result<(), BuildProviderFailure> {
        let status = Command::new(&self.config.buildctl_path)
            .args([
                "--addr",
                &self.config.buildkit_address,
                "--tlscacert",
                self.config.buildkit_ca_file.to_str().ok_or_else(rejected)?,
                "--tlscert",
                self.config
                    .buildkit_client_certificate_file
                    .to_str()
                    .ok_or_else(rejected)?,
                "--tlskey",
                self.config
                    .buildkit_client_private_key_file
                    .to_str()
                    .ok_or_else(rejected)?,
                "build",
                "--frontend",
                "dockerfile.v0",
                "--local",
            ])
            .arg(format!("context={}", workspace.display()))
            .arg("--local")
            .arg(format!("dockerfile={}", workspace.display()))
            .arg("--opt")
            .arg(format!("filename={dockerfile_path}"))
            .args(["--opt", "platform=linux/amd64", "--output"])
            .arg(format!("type=image,name={tagged},push=true"))
            .args(["--metadata-file"])
            .arg(metadata)
            .env("DOCKER_CONFIG", &self.config.docker_config_directory)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .map_err(network)?;
        if !status.success() {
            return Err(unavailable());
        }
        Ok(())
    }

    async fn scan(&self, candidate: &BuiltCandidate) -> Result<ScanEvidence, BuildProviderFailure> {
        let report = tempfile::NamedTempFile::new_in(&self.config.work_directory)
            .map_err(|_| unavailable())?;
        let status = Command::new(&self.config.trivy_path)
            .args([
                "image",
                "--format",
                "json",
                "--scanners",
                "vuln,secret",
                "--severity",
                "UNKNOWN,LOW,MEDIUM,HIGH,CRITICAL",
                "--exit-code",
                "0",
                "--no-progress",
                "--cache-dir",
            ])
            .arg(&self.config.trivy_cache_directory)
            .args(["--db-repository", &self.config.scanner_database_repository])
            .args([
                "--skip-java-db-update",
                "--skip-check-update",
                "--skip-vex-repo-update",
                "--skip-version-check",
            ])
            .arg("--output")
            .arg(report.path())
            .arg(format!("{}@{}", candidate.repository, candidate.digest))
            .env("TRIVY_USERNAME", &self.harbor_username)
            .env("TRIVY_PASSWORD", &self.harbor_password)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .map_err(network)?;
        if !status.success() {
            return Err(unavailable());
        }
        let report = tokio::fs::read(report.path())
            .await
            .map_err(|_| output_invalid())?;
        let vulnerabilities = parse_trivy_report(&report)?;
        Ok(ScanEvidence {
            build_identity: candidate.build_identity,
            digest: candidate.digest.clone(),
            scanner_name: self.config.scanner_name.clone(),
            scanner_version: self.config.scanner_version.clone(),
            scanner_database_sha256: self.config.scanner_database_sha256,
            vulnerabilities,
        })
    }

    async fn publish(
        &self,
        candidate: &BuiltCandidate,
    ) -> Result<PublishedImage, BuildProviderFailure> {
        let repository =
            RepositoryIdentity::parse(&candidate.repository, &self.config.harbor_registry)?;
        let url = self.harbor_url(&[
            "projects",
            &repository.project,
            "repositories",
            &repository.repository,
            "artifacts",
            &candidate.digest,
        ])?;
        let response = self
            .authorized(self.client.get(url.clone()))
            .send()
            .await
            .map_err(|error| {
                tracing::error!(
                    event = "agent.build_executor.harbor_publish_request_failed",
                    endpoint = %url,
                    error = %error,
                );
                unavailable()
            })?;
        let status = response.status();
        let body = response.text().await.map_err(|error| {
            tracing::error!(
                event = "agent.build_executor.harbor_publish_response_read_failed",
                endpoint = %url,
                status = %status,
                error = %error,
            );
            unavailable()
        })?;
        if !status.is_success() {
            tracing::error!(
                event = "agent.build_executor.harbor_publish_rejected",
                endpoint = %url,
                status = %status,
                body = %sanitize_diagnostic(&body),
            );
            return Err(unavailable());
        }
        let artifact: Value = serde_json::from_str(&body).map_err(|_| output_invalid())?;
        if artifact.get("digest").and_then(Value::as_str) != Some(candidate.digest.as_str()) {
            return Err(output_invalid());
        }
        Ok(PublishedImage {
            build_identity: candidate.build_identity,
            digest: candidate.digest.clone(),
        })
    }

    async fn cleanup(
        &self,
        build_request_id: BuildRequestId,
        identity: BuildIdentity,
    ) -> Result<(), BuildProviderFailure> {
        let row = sqlx::query(
            "SELECT project_name,repository_name,candidate_tag,digest,build_identity,cleaned_at \
             FROM agent.build_executor_artifacts WHERE build_request_id=$1",
        )
        .bind(build_request_id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| unavailable())?;
        let Some(row) = row else {
            return Ok(());
        };
        let stored_identity: String = row
            .try_get("build_identity")
            .map_err(|_| output_invalid())?;
        if stored_identity != identity.0.to_string() {
            return Err(identity_mismatch());
        }
        if row
            .try_get::<Option<time::OffsetDateTime>, _>("cleaned_at")
            .map_err(|_| output_invalid())?
            .is_some()
        {
            return Ok(());
        }
        let project: String = row.try_get("project_name").map_err(|_| output_invalid())?;
        let repository: String = row
            .try_get("repository_name")
            .map_err(|_| output_invalid())?;
        let tag: String = row.try_get("candidate_tag").map_err(|_| output_invalid())?;
        let digest: String = row.try_get("digest").map_err(|_| output_invalid())?;
        let url = self.harbor_url(&[
            "projects",
            &project,
            "repositories",
            &repository,
            "artifacts",
            &digest,
            "tags",
            &tag,
        ])?;
        let delete = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            self.authorized(self.client.delete(url)).send(),
        )
        .await;
        match delete {
            Ok(Ok(response))
                if matches!(
                    response.status(),
                    StatusCode::OK
                        | StatusCode::ACCEPTED
                        | StatusCode::NO_CONTENT
                        | StatusCode::NOT_FOUND
                ) => {}
            Ok(Ok(_)) => return Err(unavailable()),
            Ok(Err(_)) | Err(_) => tracing::warn!(
                event = "agent.build_executor.harbor_tag_delete_indeterminate",
                build_request_id = %build_request_id,
                diagnostic = "LW_AGENT_BUILD_CLEANUP_DELETE_INDETERMINATE"
            ),
        }
        // Harbor can complete the delete while its Core API response remains
        // pending. Registry tag listing is a separate, read-only and
        // authoritative absence check; an indeterminate delete never becomes
        // success unless this exact repository proves the tag is absent.
        if !self
            .registry_tag_absent(&project, &repository, &tag)
            .await?
        {
            return Err(unavailable());
        }
        sqlx::query(
            "UPDATE agent.build_executor_artifacts SET cleaned_at=clock_timestamp(), \
             updated_at=clock_timestamp() WHERE build_request_id=$1 AND build_identity=$2",
        )
        .bind(build_request_id.as_uuid())
        .bind(identity.0.to_string())
        .execute(&self.pool)
        .await
        .map_err(|_| unavailable())?;
        Ok(())
    }

    async fn registry_tag_absent(
        &self,
        project: &str,
        repository: &str,
        tag: &str,
    ) -> Result<bool, BuildProviderFailure> {
        let scope = format!("repository:{project}/{repository}:pull");
        let mut token_url = self.config.harbor_api.clone();
        token_url.set_path("/service/token");
        token_url
            .query_pairs_mut()
            .clear()
            .append_pair("service", "harbor-registry")
            .append_pair("scope", &scope);
        let token_response = self
            .authorized(self.client.get(token_url))
            .send()
            .await
            .map_err(network)?;
        if token_response.status() != StatusCode::OK {
            return Err(unavailable());
        }
        let token_bytes = token_response.bytes().await.map_err(network)?;
        if token_bytes.len() > MAX_HARBOR_RESPONSE_BYTES {
            return Err(output_invalid());
        }
        let token: HarborTokenResponse =
            serde_json::from_slice(&token_bytes).map_err(|_| output_invalid())?;
        if token.token.is_empty() {
            return Err(output_invalid());
        }

        let mut tags_url = Url::parse(&format!("https://{}/", self.config.harbor_registry))
            .map_err(|_| rejected())?;
        tags_url
            .path_segments_mut()
            .map_err(|()| rejected())?
            .extend(["v2", project, repository, "tags", "list"]);
        let tags_response = self
            .client
            .get(tags_url)
            .bearer_auth(token.token)
            .send()
            .await
            .map_err(network)?;
        if tags_response.status() != StatusCode::OK {
            return Err(unavailable());
        }
        let tags_bytes = tags_response.bytes().await.map_err(network)?;
        if tags_bytes.len() > MAX_HARBOR_RESPONSE_BYTES {
            return Err(output_invalid());
        }
        let listing: HarborTagList =
            serde_json::from_slice(&tags_bytes).map_err(|_| output_invalid())?;
        Ok(!listing
            .tags
            .unwrap_or_default()
            .iter()
            .any(|item| item == tag))
    }

    fn harbor_url(&self, segments: &[&str]) -> Result<Url, BuildProviderFailure> {
        let mut url = self.config.harbor_api.clone();
        {
            let mut path = url.path_segments_mut().map_err(|()| rejected())?;
            path.pop_if_empty();
            path.extend(["api", "v2.0"]);
            path.extend(segments.iter().copied());
        }
        Ok(url)
    }

    fn authorized(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        request.basic_auth(&self.harbor_username, Some(&self.harbor_password))
    }
}

#[derive(Deserialize)]
struct HarborTokenResponse {
    token: String,
}

#[derive(Deserialize)]
struct HarborTagList {
    tags: Option<Vec<String>>,
}

#[async_trait]
impl BuildExecutorBackend for ProductionBuildExecutor {
    async fn execute(
        &self,
        context: &BuildProviderRequestContext,
        request: &BuildExecutorRequest,
    ) -> BuildExecutorResponse {
        match self.execute_inner(context, request).await {
            Ok(response) => response,
            Err(failure) => {
                tracing::error!(
                    event = "agent.build_executor.stage_failed",
                    build_request_id = %context.build_request_id,
                    generation = context.fence_generation,
                    stage = ?context.stage,
                    code = ?failure.code,
                );
                BuildExecutorResponse::Failed { failure }
            }
        }
    }
}

struct RepositoryIdentity {
    project: String,
    repository: String,
}

impl RepositoryIdentity {
    fn parse(value: &str, registry: &str) -> Result<Self, BuildProviderFailure> {
        let suffix = value
            .strip_prefix(registry)
            .and_then(|value| value.strip_prefix('/'));
        let mut segments = suffix.ok_or_else(rejected)?.split('/');
        let project = segments.next().ok_or_else(rejected)?;
        let repository = segments.next().ok_or_else(rejected)?;
        if segments.next().is_some() || !portable_name(project) || !portable_name(repository) {
            return Err(rejected());
        }
        Ok(Self {
            project: project.to_owned(),
            repository: repository.to_owned(),
        })
    }
}

fn unpack_context(
    bytes: &[u8],
    media_type: &str,
    destination: &Path,
    maximum_bytes: u64,
) -> Result<(), BuildProviderFailure> {
    let reader: Box<dyn Read> = match media_type {
        "application/vnd.oci.image.layer.v1.tar+gzip" | "application/gzip" => {
            Box::new(GzDecoder::new(bytes))
        }
        "application/vnd.oci.image.layer.v1.tar" | "application/x-tar" => Box::new(bytes),
        _ => return Err(rejected()),
    };
    let mut archive = tar::Archive::new(reader);
    let entries = archive.entries().map_err(|_| rejected())?;
    let mut total_bytes = 0_u64;
    for (index, entry) in entries.enumerate() {
        if index >= MAX_CONTEXT_ENTRIES {
            return Err(rejected());
        }
        let mut entry = entry.map_err(|_| rejected())?;
        total_bytes = total_bytes
            .checked_add(entry.size())
            .filter(|total| *total <= maximum_bytes)
            .ok_or_else(rejected)?;
        let kind = entry.header().entry_type();
        if !(kind.is_file() || kind.is_dir()) {
            return Err(rejected());
        }
        let path = entry.path().map_err(|_| rejected())?;
        if path.is_absolute()
            || path.components().any(|component| {
                matches!(
                    component,
                    std::path::Component::ParentDir | std::path::Component::Prefix(_)
                )
            })
            || !entry.unpack_in(destination).map_err(|_| rejected())?
        {
            return Err(rejected());
        }
    }
    Ok(())
}

fn validate_dockerfile(
    root: &Path,
    relative: &str,
    base_digest: &str,
) -> Result<(), BuildProviderFailure> {
    let path = root.join(relative);
    let metadata = std::fs::symlink_metadata(&path).map_err(|_| rejected())?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() > MAX_DOCKERFILE_BYTES
    {
        return Err(rejected());
    }
    let text = std::fs::read_to_string(path).map_err(|_| rejected())?;
    let mut from = text.lines().filter_map(|line| {
        let line = line.trim();
        let mut tokens = line.split_ascii_whitespace();
        tokens
            .next()
            .filter(|token| token.eq_ignore_ascii_case("FROM"))
            .and_then(|_| tokens.next())
            .filter(|image| !image.starts_with("--"))
    });
    let first = from.next().ok_or_else(rejected)?;
    if !first.ends_with(&format!("@{base_digest}"))
        || !valid_digest(base_digest)
        || from.any(|image| !image.contains("@sha256:"))
        || text.lines().any(|line| {
            let normalized = line.trim().to_ascii_lowercase();
            normalized.starts_with("add http://")
                || normalized.starts_with("add https://")
                || normalized.contains("--network=host")
                || normalized.contains("--security=insecure")
        })
    {
        return Err(rejected());
    }
    Ok(())
}

fn parse_trivy_report(bytes: &[u8]) -> Result<VulnerabilitySummary, BuildProviderFailure> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| output_invalid())?;
    let mut summary = VulnerabilitySummary {
        unknown: 0,
        low: 0,
        medium: 0,
        high: 0,
        critical: 0,
    };
    for result in value
        .get("Results")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if result
            .get("Secrets")
            .and_then(Value::as_array)
            .is_some_and(|secrets| !secrets.is_empty())
        {
            return Err(rejected());
        }
        for vulnerability in result
            .get("Vulnerabilities")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let counter = match vulnerability.get("Severity").and_then(Value::as_str) {
                Some("CRITICAL") => &mut summary.critical,
                Some("HIGH") => &mut summary.high,
                Some("MEDIUM") => &mut summary.medium,
                Some("LOW") => &mut summary.low,
                _ => &mut summary.unknown,
            };
            *counter = counter.checked_add(1).ok_or_else(output_invalid)?;
        }
    }
    Ok(summary)
}

fn candidate_tag(identity: BuildIdentity) -> String {
    format!("candidate-{}", &identity.0.to_string()[..24])
}

fn valid_digest(value: &str) -> bool {
    value.len() == 71
        && value.starts_with("sha256:")
        && value[7..]
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn valid_database_reference(value: &str, registry: &str) -> bool {
    let Some((repository, digest)) = value.rsplit_once('@') else {
        return false;
    };
    repository.starts_with(&format!("{registry}/"))
        && !repository.contains("..")
        && valid_digest(digest)
}

fn portable_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"-_.".contains(&byte)
        })
}

fn read_secret(path: &Path) -> Result<String, BuildProviderFailure> {
    let value = std::fs::read_to_string(path).map_err(|_| rejected())?;
    let value = value.trim();
    if value.is_empty() {
        return Err(rejected());
    }
    Ok(value.to_owned())
}

fn prepare_docker_config(
    directory: &Path,
    registry: &str,
    username: &str,
    password: &str,
) -> Result<(), BuildProviderFailure> {
    prepare_private_directory(directory)?;
    let auth = BASE64_STANDARD.encode(format!("{username}:{password}"));
    let bytes = serde_json::to_vec(&json!({"auths": {registry: {"auth": auth}}}))
        .map_err(|_| rejected())?;
    let path = directory.join("config.json");
    #[cfg(unix)]
    {
        use std::io::Write as _;
        use std::os::unix::fs::OpenOptionsExt as _;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(path)
            .map_err(|_| rejected())?;
        file.write_all(&bytes).map_err(|_| rejected())?;
    }
    #[cfg(not(unix))]
    std::fs::write(path, bytes).map_err(|_| rejected())?;
    Ok(())
}

fn prepare_private_directory(directory: &Path) -> Result<(), BuildProviderFailure> {
    std::fs::create_dir_all(directory).map_err(|_| rejected())?;
    let metadata = std::fs::symlink_metadata(directory).map_err(|_| rejected())?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(rejected());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))
            .map_err(|_| rejected())?;
    }
    Ok(())
}

fn network<T>(_error: T) -> BuildProviderFailure {
    unavailable()
}

fn sanitize_diagnostic(value: &str) -> String {
    value
        .chars()
        .filter(|character| !character.is_control() || matches!(character, '\n' | '\t'))
        .take(512)
        .collect()
}

const fn rejected() -> BuildProviderFailure {
    BuildProviderFailure {
        code: BuildProviderFailureCode::Rejected,
        retryable: false,
    }
}

const fn unavailable() -> BuildProviderFailure {
    BuildProviderFailure {
        code: BuildProviderFailureCode::Unavailable,
        retryable: true,
    }
}

const fn identity_mismatch() -> BuildProviderFailure {
    BuildProviderFailure {
        code: BuildProviderFailureCode::IdentityMismatch,
        retryable: false,
    }
}

const fn output_invalid() -> BuildProviderFailure {
    BuildProviderFailure {
        code: BuildProviderFailureCode::OutputInvalid,
        retryable: false,
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::io::Write as _;

    use super::*;

    #[test]
    fn repository_identity_is_exactly_harbor_project_and_repository() {
        let parsed = RepositoryIdentity::parse(
            "harbor.internal/labweaver-system/course-123-candidate-456",
            "harbor.internal",
        )
        .expect("valid repository");
        assert_eq!(parsed.project, "labweaver-system");
        assert_eq!(parsed.repository, "course-123-candidate-456");
        for invalid in [
            "other.internal/labweaver-system/course-123-candidate-456",
            "harbor.internal/labweaver-system/nested/candidate-456",
            "https://harbor.internal/labweaver-system/course-123-candidate-456",
        ] {
            assert!(RepositoryIdentity::parse(invalid, "harbor.internal").is_err());
        }
    }

    #[test]
    fn trivy_database_is_one_digest_bound_internal_repository() {
        let digest = format!("sha256:{}", "a".repeat(64));
        assert!(valid_database_reference(
            &format!("harbor.internal/labweaver-system/trivy-db@{digest}"),
            "harbor.internal",
        ));
        for invalid in [
            "ghcr.io/aquasecurity/trivy-db:2",
            "harbor.internal/labweaver-system/trivy-db:2",
            "harbor.internal/../cache/trivy-db@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ] {
            assert!(!valid_database_reference(invalid, "harbor.internal"));
        }
    }

    #[test]
    fn docker_config_is_written_for_the_exact_registry() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let target = directory.path().join("docker");
        prepare_docker_config(&target, "harbor.internal", "robot$user", "secret")
            .expect("docker config");
        let value: Value = serde_json::from_slice(
            &std::fs::read(target.join("config.json")).expect("config bytes"),
        )
        .expect("config json");
        assert_eq!(
            value.pointer("/auths/harbor.internal/auth"),
            Some(&Value::String(BASE64_STANDARD.encode("robot$user:secret")))
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                std::fs::metadata(target.join("config.json"))
                    .expect("config metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn private_executor_directories_are_created_before_first_build() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let work = directory.path().join("work");
        let trivy = directory.path().join("trivy");
        prepare_private_directory(&work).expect("work directory");
        prepare_private_directory(&trivy).expect("trivy directory");
        assert!(work.is_dir());
        assert!(trivy.is_dir());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                std::fs::metadata(work)
                    .expect("work metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
    }

    #[test]
    fn dockerfile_requires_digest_bound_base_and_rejects_unsafe_entitlements() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let digest = format!("sha256:{}", "a".repeat(64));
        std::fs::write(
            directory.path().join("Dockerfile"),
            format!("FROM harbor.internal/base@{digest}\nCOPY . /workspace\n"),
        )
        .expect("write Dockerfile");
        assert!(validate_dockerfile(directory.path(), "Dockerfile", &digest).is_ok());

        std::fs::write(
            directory.path().join("Dockerfile"),
            format!("FROM harbor.internal/base@{digest}\nRUN --network=host true\n"),
        )
        .expect("write unsafe Dockerfile");
        assert!(validate_dockerfile(directory.path(), "Dockerfile", &digest).is_err());
    }

    #[test]
    fn context_unpack_rejects_links_and_traversal() {
        let mut archive_bytes = Vec::new();
        {
            let mut archive = tar::Builder::new(&mut archive_bytes);
            let bytes = b"FROM scratch\n";
            let mut header = tar::Header::new_gnu();
            header.set_size(u64::try_from(bytes.len()).expect("bounded"));
            header.set_mode(0o644);
            header.set_cksum();
            archive
                .append_data(&mut header, "Dockerfile", &bytes[..])
                .expect("append file");
            archive.finish().expect("finish archive");
        }
        let directory = tempfile::tempdir().expect("temporary directory");
        assert!(
            unpack_context(&archive_bytes, "application/x-tar", directory.path(), 1024,).is_ok()
        );

        let mut link_archive = Vec::new();
        {
            let mut archive = tar::Builder::new(&mut link_archive);
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_size(0);
            header.set_link_name("/etc/passwd").expect("link name");
            header.set_cksum();
            archive
                .append_data(&mut header, "escape", std::io::empty())
                .expect("append link");
            archive.finish().expect("finish archive");
        }
        assert!(
            unpack_context(&link_archive, "application/x-tar", directory.path(), 1024,).is_err()
        );
    }

    #[test]
    fn trivy_report_is_bounded_to_counts_and_rejects_secrets() {
        let report = serde_json::to_vec(&json!({
            "Results": [{
                "Vulnerabilities": [
                    {"Severity": "HIGH"},
                    {"Severity": "CRITICAL"},
                    {"Severity": "unexpected"}
                ]
            }]
        }))
        .expect("serialize report");
        let summary = parse_trivy_report(&report).expect("valid report");
        assert_eq!(summary.high, 1);
        assert_eq!(summary.critical, 1);
        assert_eq!(summary.unknown, 1);

        let secret = serde_json::to_vec(&json!({"Results": [{"Secrets": [{}]}]}))
            .expect("serialize secret report");
        assert!(parse_trivy_report(&secret).is_err());
    }

    #[test]
    fn gzip_context_is_supported_without_external_commands() {
        let mut archive_bytes = Vec::new();
        {
            let encoder =
                flate2::write::GzEncoder::new(&mut archive_bytes, flate2::Compression::default());
            let mut archive = tar::Builder::new(encoder);
            let bytes = b"FROM scratch\n";
            let mut header = tar::Header::new_gnu();
            header.set_size(u64::try_from(bytes.len()).expect("bounded"));
            header.set_mode(0o644);
            header.set_cksum();
            archive
                .append_data(&mut header, "Dockerfile", &bytes[..])
                .expect("append file");
            let encoder = archive.into_inner().expect("finish tar");
            encoder.finish().expect("finish gzip");
        }
        let directory = tempfile::tempdir().expect("temporary directory");
        assert!(
            unpack_context(
                &archive_bytes,
                "application/vnd.oci.image.layer.v1.tar+gzip",
                directory.path(),
                1024,
            )
            .is_ok()
        );
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(directory.path().join("Dockerfile"))
            .expect("open unpacked file");
        file.write_all(b"# verified\n")
            .expect("write unpacked file");
    }
}
