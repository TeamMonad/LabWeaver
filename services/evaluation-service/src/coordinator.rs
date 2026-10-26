//! Fixed-operation Kubernetes coordinator for immutable submission freeze Jobs.
#![allow(
    missing_docs,
    clippy::missing_errors_doc,
    reason = "the reviewed configuration and stable diagnostics define this internal boundary"
)]

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::PathBuf,
    sync::Arc,
    time::Duration,
}; // internal persistence hash, not contract hash

use auth::{ServiceTokenClient, ServiceTokenClientError};
use base64::{Engine, engine::general_purpose::STANDARD};
use contracts::{
    DiagnosticCode, PolicyId, RetentionClass, RetentionDisposition, RetentionSnapshot, Revision,
    UtcTimestamp,
    submission::{
        EnvironmentFreezeBinding, EnvironmentFreezeBindingRequest, EnvironmentFreezeSourceBinding,
    },
};
use rand::random;
use reqwest::{
    Certificate, Client, Method, StatusCode, Url,
    header::{self, HeaderMap},
};
use russh::keys::ssh_key::{LineEnding, PrivateKey, private::Ed25519Keypair};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::authoring_client::{AuthoringAdmissionClient, AuthoringAdmissionClientError};
use crate::{
    EvaluationControlStoreError, FreezeCommandDurableOutcome, FreezeRequest, FreezeWorkerDispatch,
    FreezeWorkerProfile, PgEvaluationControlStore, PgFreezeCommandStore, SubmissionFreezeCommand,
};

const WORKER_IMAGE_PULL_SECRET_NAME: &str = "harbor-labweaver-system-pull";

const FIELD_MANAGER: &str = "labweaver-freeze-coordinator";
const MAX_BOUND_FILE_BYTES: u64 = 1024 * 1024;
const ENVIRONMENT_FREEZE_SCOPE: &str = "evaluation.environment.freeze";

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FreezeCoordinatorConfiguration {
    #[serde(default)]
    pub worker_profile: FreezeWorkerProfile,
    pub kubernetes_api_server: Url,
    pub kubernetes_bearer_token_file: PathBuf,
    pub kubernetes_ca_file: PathBuf,
    pub environment_service_base_uri: Url,
    pub environment_ca_file: PathBuf,
    pub environment_audience: String,
    pub worker_image: String,
    #[serde(default)]
    pub worker_service_account_name: String,
    #[serde(default)]
    pub vm_job_namespace: String,
    pub worker_configuration_file: PathBuf,
    #[serde(default)]
    pub worker_secret_files: BTreeMap<String, PathBuf>,
    #[serde(default)]
    pub worker_registry_pull_config_file: PathBuf,
    pub worker_tls_ca_file: PathBuf,
    #[serde(default)]
    pub infrastructure_namespace_labels: BTreeMap<String, String>,
    #[serde(default)]
    pub dns_namespace_labels: BTreeMap<String, String>,
    #[serde(default)]
    pub dns_pod_labels: BTreeMap<String, String>,
    pub retention_policy_id: PolicyId,
    pub retention_policy_revision: Revision,
    pub retention_days: i64,
    pub job_active_deadline_seconds: u64,
    pub request_timeout_milliseconds: u64,
}

#[derive(Clone)]
pub struct FreezeCoordinator {
    configuration: FreezeCoordinatorConfiguration,
    store: PgFreezeCommandStore,
    kubernetes: Client,
    environment: Client,
    environment_token_client: Arc<ServiceTokenClient>,
    environment_token_scopes: BTreeSet<String>,
    authoring_admission: AuthoringAdmissionClient,
    evaluation: PgEvaluationControlStore,
    kubernetes_token: String,
    worker_configuration: String,
    worker_secrets: BTreeMap<String, Vec<u8>>,
    worker_registry_pull_config: Vec<u8>,
}

impl FreezeCoordinator {
    pub fn new(
        configuration: FreezeCoordinatorConfiguration,
        store: PgFreezeCommandStore,
        environment_token_client: Arc<ServiceTokenClient>,
        available_service_scopes: &BTreeSet<String>,
        authoring_admission: AuthoringAdmissionClient,
        evaluation: PgEvaluationControlStore,
    ) -> Result<Self, FreezeCoordinatorError> {
        validate_configuration(&configuration)?;
        if !available_service_scopes.contains(ENVIRONMENT_FREEZE_SCOPE) {
            return Err(FreezeCoordinatorError::ConfigurationInvalid);
        }
        let environment_token_scopes = BTreeSet::from([ENVIRONMENT_FREEZE_SCOPE.to_owned()]);
        let kubernetes_token = read_bound_text(&configuration.kubernetes_bearer_token_file)?;
        let kubernetes_ca =
            Certificate::from_pem(&read_bound_file(&configuration.kubernetes_ca_file)?)?;
        let kubernetes = Client::builder()
            .https_only(true)
            .tls_built_in_root_certs(false)
            .add_root_certificate(kubernetes_ca)
            .timeout(Duration::from_millis(
                configuration.request_timeout_milliseconds,
            ))
            .build()?;

        let environment_ca =
            Certificate::from_pem_bundle(&read_bound_file(&configuration.environment_ca_file)?)
                .map_err(|_| FreezeCoordinatorError::CertificateInvalid)?;
        if environment_ca.is_empty() {
            return Err(FreezeCoordinatorError::CertificateInvalid);
        }
        let mut environment_builder = Client::builder()
            .https_only(true)
            .tls_built_in_root_certs(false)
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_millis(
                configuration.request_timeout_milliseconds,
            ));
        for ca in environment_ca {
            environment_builder = environment_builder.add_root_certificate(ca);
        }
        let environment = environment_builder.build()?;
        let worker_configuration = read_bound_text(&configuration.worker_configuration_file)?;
        serde_yaml::from_str::<Value>(&worker_configuration)
            .map_err(|_| FreezeCoordinatorError::ConfigurationInvalid)?;
        let (worker_secrets, worker_registry_pull_config) = match &configuration.worker_profile {
            FreezeWorkerProfile::Managed => (
                configuration
                    .worker_secret_files
                    .iter()
                    .map(|(name, path)| {
                        validate_key(name)?;
                        Ok((name.clone(), read_bound_file(path)?))
                    })
                    .collect::<Result<_, FreezeCoordinatorError>>()?,
                read_registry_pull_config(&configuration.worker_registry_pull_config_file)?,
            ),
            FreezeWorkerProfile::Existing { .. } => (BTreeMap::new(), Vec::new()),
        };
        Ok(Self {
            configuration,
            store,
            kubernetes,
            environment,
            environment_token_client,
            environment_token_scopes,
            authoring_admission,
            evaluation,
            kubernetes_token,
            worker_configuration,
            worker_secrets,
            worker_registry_pull_config,
        })
    }

    /// Claims at most one command and reconciles every bounded in-flight Job.
    pub async fn reconcile_once(&self) -> Result<(), FreezeCoordinatorError> {
        let _ = self.store.claim_next().await?;
        for command in self.store.cleanup_pending(32).await? {
            if self.recover_completed_command(&command).await? {
                continue;
            }
            self.cleanup_failed_command(&command).await?;
        }
        let authority_now = self.store.authority_now().await?;
        for command in self.store.running(32).await? {
            if let Err(error) = self.reconcile(&command).await {
                if error.is_systemic() {
                    return Err(error);
                }
                let deadline_exceeded = command_deadline_exceeded(
                    command.requested_at,
                    authority_now,
                    self.configuration.job_active_deadline_seconds,
                );
                let terminal = error.is_terminal_command_error() || deadline_exceeded;
                tracing::warn!(
                    event = "evaluation.freeze.reconcile.failed",
                    frozen_submission_id = %command.frozen_submission_id,
                    environment_id = %command.environment_id,
                    diagnostic_code = error.diagnostic_code(),
                    deadline_exceeded,
                    retry = !terminal,
                );
                if terminal {
                    let cleanup = self
                        .fail_command_after_cleanup(
                            &command,
                            if deadline_exceeded {
                                "LW_COLLECT_DEADLINE_EXCEEDED"
                            } else {
                                error.diagnostic_code()
                            },
                        )
                        .await;
                    if let Err(cleanup_error) = cleanup {
                        if cleanup_error.is_systemic() {
                            return Err(cleanup_error);
                        }
                        tracing::warn!(
                            event = "evaluation.freeze.cleanup.failed",
                            frozen_submission_id = %command.frozen_submission_id,
                            environment_id = %command.environment_id,
                            diagnostic_code = cleanup_error.diagnostic_code(),
                            retry = true,
                        );
                    }
                }
            }
        }
        Ok(())
    }

    async fn cleanup_failed_command(
        &self,
        command: &SubmissionFreezeCommand,
    ) -> Result<(), FreezeCoordinatorError> {
        if self.recover_completed_command(command).await? {
            return Ok(());
        }
        if !self.cleanup_command_resources(command).await? {
            return Ok(());
        }
        self.store
            .mark_cleanup_verified(command.frozen_submission_id)
            .await?;
        if self.recover_completed_command(command).await? {
            return Ok(());
        }
        tracing::info!(
            event = "evaluation.freeze.cleanup.verified",
            frozen_submission_id = %command.frozen_submission_id,
            environment_id = %command.environment_id,
        );
        Ok(())
    }

    async fn fail_command_after_cleanup(
        &self,
        command: &SubmissionFreezeCommand,
        diagnostic: &str,
    ) -> Result<(), FreezeCoordinatorError> {
        if !self.cleanup_command_resources(command).await? {
            return Ok(());
        }
        self.store
            .mark_failed(command.frozen_submission_id, diagnostic)
            .await?;
        if self.recover_completed_command(command).await? {
            return Ok(());
        }
        tracing::error!(
            event = "evaluation.freeze.failed",
            frozen_submission_id = %command.frozen_submission_id,
            environment_id = %command.environment_id,
            diagnostic_code = diagnostic,
            cleanup_verified = true,
        );
        Ok(())
    }

    async fn complete_command_after_cleanup(
        &self,
        command: &SubmissionFreezeCommand,
    ) -> Result<(), FreezeCoordinatorError> {
        if !self.cleanup_command_resources(command).await? {
            return Ok(());
        }
        self.store
            .mark_completed(command.frozen_submission_id)
            .await?;
        tracing::info!(
            event = "evaluation.freeze.completed",
            frozen_submission_id = %command.frozen_submission_id,
            environment_id = %command.environment_id,
            cleanup_verified = true,
        );
        Ok(())
    }

    async fn cleanup_command_resources(
        &self,
        command: &SubmissionFreezeCommand,
    ) -> Result<bool, FreezeCoordinatorError> {
        let refreshed = self.store.refresh_worker_dispatch(command).await?;
        let command = &refreshed;
        let bound = match &command.worker_dispatch {
            FreezeWorkerDispatch::Pending => return Ok(true),
            FreezeWorkerDispatch::Bound { .. } => command.clone(),
            FreezeWorkerDispatch::Untracked => {
                // Legacy residue requires a real binding. Never guess its former namespace.
                let namespace = self.job_namespace(command).await?;
                self.store
                    .bind_worker_dispatch(
                        command,
                        &FreezeWorkerDispatch::Bound {
                            namespace,
                            profile: FreezeWorkerProfile::Managed,
                        },
                    )
                    .await?
            }
        };
        let FreezeWorkerDispatch::Bound { namespace, profile } = &bound.worker_dispatch else {
            return Err(FreezeCoordinatorError::BindingInvalid);
        };
        self.cleanup(namespace, &job_name(command), &bound, profile)
            .await
    }

    async fn recover_completed_command(
        &self,
        command: &SubmissionFreezeCommand,
    ) -> Result<bool, FreezeCoordinatorError> {
        if !matches!(
            self.store
                .durable_outcome(command.frozen_submission_id)
                .await?,
            Some(FreezeCommandDurableOutcome::Completed)
        ) {
            return Ok(false);
        }
        self.complete_command_after_cleanup(command).await?;
        Ok(true)
    }

    async fn reconcile(
        &self,
        command: &SubmissionFreezeCommand,
    ) -> Result<(), FreezeCoordinatorError> {
        if self.recover_completed_command(command).await? {
            return Ok(());
        }
        if let Some(FreezeCommandDurableOutcome::Failed(diagnostic)) = self
            .store
            .durable_outcome(command.frozen_submission_id)
            .await?
        {
            self.fail_command_after_cleanup(command, diagnostic.as_str())
                .await?;
            return Ok(());
        }
        let bound = match &command.worker_dispatch {
            FreezeWorkerDispatch::Bound { .. } => command.clone(),
            FreezeWorkerDispatch::Pending | FreezeWorkerDispatch::Untracked => {
                let namespace = self.job_namespace(command).await?;
                self.store
                    .bind_worker_dispatch(
                        command,
                        &FreezeWorkerDispatch::Bound {
                            namespace,
                            profile: self.configuration.worker_profile.clone(),
                        },
                    )
                    .await?
            }
        };
        let command = &bound;
        let FreezeWorkerDispatch::Bound { namespace, profile } = &command.worker_dispatch else {
            return Err(FreezeCoordinatorError::BindingInvalid);
        };
        let job_name = job_name(command);
        let job = self.get(namespace, "batch/v1", "jobs", &job_name).await?;
        if let Some(job) = job {
            verify_owned(&job, command)?;
            let succeeded = job.pointer("/status/succeeded").and_then(Value::as_u64) == Some(1);
            let failed = job
                .pointer("/status/failed")
                .and_then(Value::as_u64)
                .unwrap_or(0)
                > 0;
            let worker_diagnostic = if failed {
                self.worker_failure_diagnostic(namespace, command).await?
            } else {
                None
            };
            if failed {
                if self.recover_completed_command(command).await? {
                    return Ok(());
                }
                let diagnostic = worker_diagnostic
                    .as_ref()
                    .map_or("LW_COLLECT_JOB_FAILED", DiagnosticCode::as_str);
                self.store
                    .mark_failed_pending_cleanup(command.frozen_submission_id, diagnostic)
                    .await?;
                if self.recover_completed_command(command).await? {
                    return Ok(());
                }
                self.cleanup_failed_command(command).await?;
                return Ok(());
            }
            if succeeded && self.cleanup(namespace, &job_name, command, profile).await? {
                // The worker persists the immutable result before exiting. A
                // Job can still be observed as failed after that durable write
                // (for example when the kubelet reports a terminal transition
                // during cleanup). Never overwrite a completed submission with
                // a job-level failure; use the database result as the authority.
                match self
                    .store
                    .mark_completed(command.frozen_submission_id)
                    .await
                {
                    Ok(()) => {}
                    Err(error) => return Err(error.into()),
                }
            }
            return Ok(());
        }
        self.create_resources(command, namespace, &job_name).await
    }

    async fn worker_failure_diagnostic(
        &self,
        namespace: &str,
        command: &SubmissionFreezeCommand,
    ) -> Result<Option<DiagnosticCode>, FreezeCoordinatorError> {
        let response = self
            .authorized(
                self.kubernetes
                    .get(self.collection_url(namespace, "v1", "pods")?),
            )
            .query(&[(
                "labelSelector",
                format!(
                    "labweaver.io/frozen-submission-id={}",
                    command.frozen_submission_id
                ),
            )])
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(FreezeCoordinatorError::KubernetesRejected);
        }
        let pods: Value = response.json().await?;
        Ok(pod_failure_diagnostic(&pods))
    }

    async fn job_namespace(
        &self,
        command: &SubmissionFreezeCommand,
    ) -> Result<String, FreezeCoordinatorError> {
        let probe_key = PrivateKey::from(Ed25519Keypair::from_seed(&random::<[u8; 32]>()));
        let binding = self
            .binding(command, Some(probe_key.public_key().to_openssh()?))
            .await?;
        if binding.environment.environment_id != command.environment_id
            || binding.environment.environment_revision != command.environment_revision
        {
            return Err(FreezeCoordinatorError::BindingInvalid);
        }
        let namespace = match binding.source {
            EnvironmentFreezeSourceBinding::Container { namespace, .. } => namespace,
            EnvironmentFreezeSourceBinding::VirtualMachine { .. } => {
                if !matches!(
                    self.configuration.worker_profile,
                    FreezeWorkerProfile::Managed
                ) {
                    return Err(FreezeCoordinatorError::BindingInvalid);
                }
                self.configuration.vm_job_namespace.clone()
            }
        };
        if !self
            .configuration
            .worker_profile
            .accepts_namespace(&namespace)
        {
            return Err(FreezeCoordinatorError::BindingInvalid);
        }
        Ok(namespace)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the linear resource construction keeps secret, source, and Job bindings reviewable together"
    )]
    async fn create_resources(
        &self,
        command: &SubmissionFreezeCommand,
        namespace: &str,
        job_name: &str,
    ) -> Result<(), FreezeCoordinatorError> {
        let existing_config = self.get(namespace, "v1", "configmaps", job_name).await?;
        let profile = match &command.worker_dispatch {
            FreezeWorkerDispatch::Bound {
                namespace: bound_namespace,
                profile,
            } if bound_namespace == namespace => profile,
            _ => &self.configuration.worker_profile,
        };
        let existing_secret = match profile {
            FreezeWorkerProfile::Managed => self.get(namespace, "v1", "secrets", job_name).await?,
            FreezeWorkerProfile::Existing { .. } => None,
        };
        let existing_bundle = match (existing_config.as_ref(), existing_secret.as_ref()) {
            (Some(config), None) if matches!(profile, FreezeWorkerProfile::Existing { .. }) => {
                verify_owned(config, command)?;
                true
            }
            (Some(config), Some(secret)) => {
                verify_owned(config, command)?;
                verify_owned(secret, command)?;
                true
            }
            (None, None) => false,
            _ => {
                let _ = self.cleanup(namespace, job_name, command, profile).await?;
                return Ok(());
            }
        };
        let key = PrivateKey::from(Ed25519Keypair::from_seed(&random::<[u8; 32]>()));
        let public_key = key.public_key().to_openssh()?;
        let binding = self.binding(command, Some(public_key)).await?;
        if binding.environment.environment_id != command.environment_id
            || binding.environment.environment_revision != command.environment_revision
        {
            return Err(FreezeCoordinatorError::BindingInvalid);
        }
        match self
            .authoring_admission
            .resolve_environment(
                binding.environment.release_id,
                &contracts::http::EnvironmentPublicationAdmissionQuery {
                    project_id: command.project_id,
                    course_id: command.course_id,
                    environment_release_version: binding.environment.release_version,
                },
            )
            .await
        {
            Ok(Some(admission)) => {
                let release = self
                    .evaluation
                    .load_release(admission.evaluation_release_id)
                    .await?;
                if release.state != contracts::evaluation::EvaluationReleaseState::Active
                    || release.revision != admission.evaluation_release_revision
                    || release.project_id != command.project_id
                    || release.course_id != command.course_id
                {
                    return Err(FreezeCoordinatorError::BindingInvalid);
                }
                let expected_manifest =
                    contracts::submission::SubmissionManifest::from_evaluation_spec(
                        &release.evaluation_spec,
                    )
                    .map_err(|_| FreezeCoordinatorError::BindingInvalid)?;
                if expected_manifest != command.manifest {
                    return Err(FreezeCoordinatorError::BindingInvalid);
                }
            }
            Ok(None) => {
                // Control has authoritatively identified a Work release.  It retains bounded
                // snapshot behavior without entering the Evaluation run path.
            }
            Err(error) => return Err(FreezeCoordinatorError::AuthoringAdmission(error)),
        }
        let now = self.store.authority_now().await?;
        let request = FreezeRequest {
            frozen_submission_id: command.frozen_submission_id,
            project_id: command.project_id,
            course_id: command.course_id,
            actor_id: command.actor_id,
            agent_run_id: binding.agent_run_id,
            manifest_revision: command.manifest_revision,
            manifest: command.manifest.clone(),
            environment: binding.environment.clone(),
            retention: RetentionSnapshot {
                policy_id: self.configuration.retention_policy_id,
                policy_revision: self.configuration.retention_policy_revision,
                class: RetentionClass::StudentSubmission,
                retain_until: Some(
                    UtcTimestamp::from_utc(
                        now.get() + time::Duration::days(self.configuration.retention_days),
                    )
                    .map_err(|_| FreezeCoordinatorError::BindingInvalid)?,
                ),
                disposition: RetentionDisposition::Delete,
            },
            idempotency_key: command.idempotency_key.clone(),
            trace_id: command.trace_id.clone(),
        };
        let collector_certificate = match &binding.source {
            EnvironmentFreezeSourceBinding::VirtualMachine {
                collector_certificate_openssh,
                ..
            } => Some(collector_certificate_openssh.clone()),
            EnvironmentFreezeSourceBinding::Container { .. } => None,
        };
        let (source, volume, vm_egress) = match binding.source {
            EnvironmentFreezeSourceBinding::Container {
                namespace: source_namespace,
                persistent_volume_claim,
                storage_class_name: _,
            } => {
                if source_namespace != namespace {
                    return Err(FreezeCoordinatorError::BindingInvalid);
                }
                (
                    json!({
                        "kind":"pvc",
                        "workspaceRoot":"/workspace",
                        "sourceIdentity": persistence_sqlx::Sha256Digest::of_canonical(&binding.environment)
                            .map_err(|_| FreezeCoordinatorError::BindingInvalid)?
                    }),
                    Some(
                        json!({"name":"workspace","persistentVolumeClaim":{"claimName":persistent_volume_claim,"readOnly":true}}),
                    ),
                    None,
                )
            }
            EnvironmentFreezeSourceBinding::VirtualMachine {
                namespace: source_namespace,
                host,
                port,
                username,
                workspace_root,
                expected_host_key_sha256,
                source_identity,
                collector_certificate_openssh: _,
                expires_at,
            } => {
                let _: std::net::IpAddr = host
                    .parse()
                    .map_err(|_| FreezeCoordinatorError::BindingInvalid)?;
                if source_namespace != format!("lw-env-{}", command.environment_id) {
                    return Err(FreezeCoordinatorError::BindingInvalid);
                }
                (
                    json!({
                    "kind":"ssh","host":host,"port":port,"username":username,
                    "workspaceRoot":workspace_root,
                    "privateKeyPath":"/run/secrets/collector/key",
                    "certificatePath":"/run/secrets/collector/key-cert.pub",
                    "expectedHostKeySha256":expected_host_key_sha256,
                    "sourceIdentity":source_identity,"expiresAt":expires_at,
                    "connectTimeoutMilliseconds":5000,"operationTimeoutMilliseconds":30000
                    }),
                    None,
                    Some((
                        json!({
                            "namespaceSelector":{"matchLabels":{
                                "kubernetes.io/metadata.name":source_namespace
                            }},
                            "podSelector":{"matchLabels":{
                                "labweaver.io/environment-id":command.environment_id.to_string()
                            }}
                        }),
                        port,
                    )),
                )
            }
        };
        let command_document = serde_json::to_string(&json!({"request":request,"source":source}))?;
        let labels = resource_labels(command);
        if !matches!(&command.worker_dispatch, FreezeWorkerDispatch::Bound {
            namespace: bound_namespace, profile: bound_profile
        } if bound_namespace == namespace && bound_profile == profile)
        {
            return Err(FreezeCoordinatorError::BindingInvalid);
        }
        if matches!(profile, FreezeWorkerProfile::Existing { .. }) {
            let volume = volume.ok_or(FreezeCoordinatorError::BindingInvalid)?;
            return self
                .create_existing_resources(
                    command,
                    namespace,
                    job_name,
                    &command_document,
                    volume,
                    profile,
                )
                .await;
        }
        if !existing_bundle {
            self.apply(
                namespace,
                "v1",
                "configmaps",
                job_name,
                json!({"apiVersion":"v1","kind":"ConfigMap","metadata":{"name":job_name,"namespace":namespace,"labels":labels},"immutable":true,
                    "data":{"worker.yaml":self.worker_configuration,"command.json":command_document}}),
            ).await?;
        }
        let mut secret_data = self
            .worker_secrets
            .iter()
            .map(|(name, value)| (name.clone(), Value::String(STANDARD.encode(value))))
            .collect::<serde_json::Map<_, _>>();
        let has_collector_certificate = collector_certificate.is_some();
        if let Some(collector_certificate_openssh) = collector_certificate {
            secret_data.insert(
                "collector-key".to_owned(),
                Value::String(STANDARD.encode(key.to_openssh(LineEnding::LF)?.as_bytes())),
            );
            secret_data.insert(
                "collector-key-cert.pub".to_owned(),
                Value::String(STANDARD.encode(collector_certificate_openssh.as_bytes())),
            );
        }
        if !existing_bundle {
            self.apply(namespace, "v1", "secrets", job_name, json!({"apiVersion":"v1","kind":"Secret","metadata":{"name":job_name,"namespace":namespace,"labels":labels},"immutable":true,"type":"Opaque","data":secret_data})).await?;
        }
        self.apply(
            namespace,
            "v1",
            "secrets",
            WORKER_IMAGE_PULL_SECRET_NAME,
            json!({
                "apiVersion":"v1",
                "kind":"Secret",
                "metadata":{
                    "name":WORKER_IMAGE_PULL_SECRET_NAME,
                    "namespace":namespace,
                    "labels":{
                        "app.kubernetes.io/managed-by":FIELD_MANAGER,
                        "app.kubernetes.io/name":"evaluation-freeze-worker"
                    }
                },
                "type":"kubernetes.io/dockerconfigjson",
                "data":{
                    ".dockerconfigjson":STANDARD.encode(&self.worker_registry_pull_config)
                }
            }),
        )
        .await?;
        self.apply(
            namespace,
            "v1",
            "serviceaccounts",
            &self.configuration.worker_service_account_name,
            json!({"apiVersion":"v1","kind":"ServiceAccount","metadata":{"name":self.configuration.worker_service_account_name,
                "namespace":namespace,"labels":{"app.kubernetes.io/managed-by":FIELD_MANAGER,"app.kubernetes.io/name":"evaluation-freeze-worker"}},
                "automountServiceAccountToken":false,
                "imagePullSecrets":[{"name":WORKER_IMAGE_PULL_SECRET_NAME}]}),
        )
        .await?;
        let mut egress = vec![
            json!({"to":[{"namespaceSelector":{"matchLabels":self.configuration.infrastructure_namespace_labels}}]}),
            json!({"to":[{"namespaceSelector":{"matchLabels":self.configuration.dns_namespace_labels},
                "podSelector":{"matchLabels":self.configuration.dns_pod_labels}}],
                "ports":[{"protocol":"UDP","port":53},{"protocol":"TCP","port":53}]}),
        ];
        if let Some((peer, port)) = vm_egress {
            egress.push(json!({"to":[peer],"ports":[{"protocol":"TCP","port":port}]}));
        }
        self.apply(
            namespace,
            "networking.k8s.io/v1",
            "networkpolicies",
            job_name,
            json!({"apiVersion":"networking.k8s.io/v1","kind":"NetworkPolicy","metadata":{"name":job_name,"namespace":namespace,"labels":labels},
                "spec":{"podSelector":{"matchLabels":{"labweaver.io/frozen-submission-id":command.frozen_submission_id.to_string()}},
                "policyTypes":["Ingress","Egress"],"ingress":[],"egress":egress}}),
        )
        .await?;
        let mut volumes = vec![
            json!({"name":"command","configMap":{"name":job_name}}),
            json!({"name":"secrets","secret":{"secretName":job_name,"defaultMode":292}}),
        ];
        let mut mounts = vec![
            json!({"name":"command","mountPath":"/etc/labweaver/worker","readOnly":true}),
            json!({"name":"secrets","mountPath":"/etc/labweaver/secrets","readOnly":true}),
        ];
        if has_collector_certificate {
            volumes.push(json!({"name":"collector","secret":{"secretName":job_name,"defaultMode":256,"items":[
                {"key":"collector-key","path":"key"},{"key":"collector-key-cert.pub","path":"key-cert.pub"}]}}));
            mounts.push(
                json!({"name":"collector","mountPath":"/run/secrets/collector","readOnly":true}),
            );
        }
        if let Some(volume) = volume {
            volumes.push(volume);
            mounts.push(json!({"name":"workspace","mountPath":"/workspace","readOnly":true}));
        }
        let job = self.worker_job(command, namespace, job_name, &volumes, &mounts, profile);
        self.apply(namespace, "batch/v1", "jobs", job_name, job)
            .await
    }

    fn worker_job(
        &self,
        command: &SubmissionFreezeCommand,
        namespace: &str,
        job_name: &str,
        volumes: &[Value],
        mounts: &[Value],
        profile: &FreezeWorkerProfile,
    ) -> Value {
        let labels = resource_labels(command);
        let (service_account, pull_policy) = match profile {
            FreezeWorkerProfile::Managed => (
                self.configuration.worker_service_account_name.as_str(),
                "IfNotPresent",
            ),
            FreezeWorkerProfile::Existing {
                service_account_name,
                ..
            } => (service_account_name.as_str(), "Never"),
        };
        let mut job = json!({
            "apiVersion":"batch/v1","kind":"Job","metadata":{"name":job_name,"namespace":namespace,"labels":labels},
            "spec":{"backoffLimit":0,"activeDeadlineSeconds":self.configuration.job_active_deadline_seconds,
                "template":{"metadata":{"labels":labels},"spec":{"restartPolicy":"Never","serviceAccountName":service_account,
                    "automountServiceAccountToken":false,"securityContext":{"runAsNonRoot":true,"runAsUser":65532,"runAsGroup":65532,"fsGroup":65532,"seccompProfile":{"type":"RuntimeDefault"}},
                    "containers":[{"name":"freeze","image":self.configuration.worker_image,"imagePullPolicy":pull_policy,
                        "args":["--mode","freeze-worker"],"env":[
                            {"name":"LABWEAVER_EVALUATION_CONFIG_FILE","value":"/etc/labweaver/worker/worker.yaml"},
                            {"name":"LABWEAVER_FREEZE_COMMAND_FILE","value":"/etc/labweaver/worker/command.json"},
                            {"name":"SSL_CERT_FILE","value":self.configuration.worker_tls_ca_file}],
                        "terminationMessagePath":"/dev/termination-log","terminationMessagePolicy":"File",
                        "securityContext":{"allowPrivilegeEscalation":false,"readOnlyRootFilesystem":true,"capabilities":{"drop":["ALL"]}},
                        "resources":profile.resources().document(),"volumeMounts":mounts}],"volumes":volumes}}}
        });
        if let FreezeWorkerProfile::Existing {
            node_name: Some(node),
            ..
        } = profile
        {
            job["spec"]["template"]["spec"]["nodeSelector"] =
                json!({"kubernetes.io/hostname":node});
        }
        job
    }

    async fn create_existing_resources(
        &self,
        command: &SubmissionFreezeCommand,
        namespace: &str,
        job_name: &str,
        command_document: &str,
        workspace_volume: Value,
        profile: &FreezeWorkerProfile,
    ) -> Result<(), FreezeCoordinatorError> {
        self.verify_existing_resources(command, namespace, profile)
            .await?;
        let FreezeWorkerProfile::Existing { secret_name, .. } = profile else {
            return Err(FreezeCoordinatorError::ConfigurationInvalid);
        };
        let labels = resource_labels(command);
        let config = json!({"apiVersion":"v1","kind":"ConfigMap",
            "metadata":{"name":job_name,"namespace":namespace,"labels":labels},"immutable":true,
            "data":{"worker.yaml":self.worker_configuration,"command.json":command_document}});
        self.create_owned(namespace, "v1", "configmaps", job_name, config, command)
            .await?;
        let volumes = vec![
            json!({"name":"command","configMap":{"name":job_name}}),
            json!({"name":"secrets","secret":{"secretName":secret_name,"defaultMode":292}}),
            workspace_volume,
        ];
        let mounts = vec![
            json!({"name":"command","mountPath":"/etc/labweaver/worker","readOnly":true}),
            json!({"name":"secrets","mountPath":"/etc/labweaver/secrets","readOnly":true}),
            json!({"name":"workspace","mountPath":"/workspace","readOnly":true}),
        ];
        let job = self.worker_job(command, namespace, job_name, &volumes, &mounts, profile);
        self.create_owned(namespace, "batch/v1", "jobs", job_name, job, command)
            .await
    }

    async fn verify_existing_resources(
        &self,
        command: &SubmissionFreezeCommand,
        namespace: &str,
        profile: &FreezeWorkerProfile,
    ) -> Result<(), FreezeCoordinatorError> {
        let FreezeWorkerProfile::Existing {
            secret_name,
            service_account_name,
            network_policy_names,
            resource_quota_names,
            resources,
            ..
        } = profile
        else {
            return Err(FreezeCoordinatorError::ConfigurationInvalid);
        };
        if !profile.accepts_namespace(namespace) {
            return Err(FreezeCoordinatorError::BindingInvalid);
        }
        // Request only Secret metadata; never negotiate a fallback to its data representation.
        let response = self
            .authorized(self.kubernetes.get(self.resource_url(
                namespace,
                "v1",
                "secrets",
                secret_name,
            )?))
            .header(
                header::ACCEPT,
                "application/json;as=PartialObjectMetadata;g=meta.k8s.io;v=v1",
            )
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(FreezeCoordinatorError::ProfilePrerequisiteUnavailable);
        }
        let secret: Value = response.json().await?;
        if secret.get("kind").and_then(Value::as_str) != Some("PartialObjectMetadata")
            || secret.get("data").is_some()
            || secret.get("stringData").is_some()
        {
            return Err(FreezeCoordinatorError::ProfilePrerequisiteUnavailable);
        }
        verify_existing_reference(&secret, namespace, secret_name)?;
        let account = self
            .get(namespace, "v1", "serviceaccounts", service_account_name)
            .await?
            .ok_or(FreezeCoordinatorError::ProfilePrerequisiteUnavailable)?;
        verify_existing_reference(&account, namespace, service_account_name)?;
        let labels = resource_labels(command);
        let mut isolated_ingress = false;
        let mut isolated_egress = false;
        for name in network_policy_names {
            let policy = self
                .get(namespace, "networking.k8s.io/v1", "networkpolicies", name)
                .await?
                .ok_or(FreezeCoordinatorError::ProfilePrerequisiteUnavailable)?;
            verify_existing_reference(&policy, namespace, name)?;
            let (ingress, egress) = crate::freeze_profile::selected_policy_types(&policy, &labels)
                .ok_or(FreezeCoordinatorError::ProfilePrerequisiteUnavailable)?;
            if ingress
                && policy
                    .pointer("/spec/ingress")
                    .is_some_and(|rules| rules.as_array().is_none_or(|rules| !rules.is_empty()))
            {
                return Err(FreezeCoordinatorError::ProfilePrerequisiteUnavailable);
            }
            isolated_ingress |= ingress;
            isolated_egress |= egress;
        }
        if !isolated_ingress || !isolated_egress {
            return Err(FreezeCoordinatorError::ProfilePrerequisiteUnavailable);
        }
        for name in resource_quota_names {
            let quota = self
                .get(namespace, "v1", "resourcequotas", name)
                .await?
                .ok_or(FreezeCoordinatorError::QuotaUnavailable)?;
            verify_existing_reference(&quota, namespace, name)?;
            match crate::freeze_profile::check_quota(&quota, resources) {
                crate::freeze_profile::QuotaCheck::Fits => {}
                crate::freeze_profile::QuotaCheck::Full => {
                    return Err(FreezeCoordinatorError::QuotaExceeded);
                }
                crate::freeze_profile::QuotaCheck::Unavailable => {
                    return Err(FreezeCoordinatorError::QuotaUnavailable);
                }
            }
        }
        Ok(())
    }

    async fn create_owned(
        &self,
        namespace: &str,
        api_version: &str,
        plural: &str,
        name: &str,
        document: Value,
        command: &SubmissionFreezeCommand,
    ) -> Result<(), FreezeCoordinatorError> {
        let response = self
            .authorized(self.kubernetes.post(self.collection_url(
                namespace,
                api_version,
                plural,
            )?))
            .json(&document)
            .send()
            .await?;
        if response.status() == StatusCode::CONFLICT {
            let existing = self
                .get(namespace, api_version, plural, name)
                .await?
                .ok_or(FreezeCoordinatorError::KubernetesRejected)?;
            return verify_owned(&existing, command);
        }
        if !response.status().is_success() {
            return Err(FreezeCoordinatorError::KubernetesRejected);
        }
        Ok(())
    }

    async fn binding(
        &self,
        command: &SubmissionFreezeCommand,
        collector_public_key_openssh: Option<String>,
    ) -> Result<EnvironmentFreezeBinding, FreezeCoordinatorError> {
        let mut uri = self.configuration.environment_service_base_uri.clone();
        uri.set_path(&format!(
            "/internal/v1/environments/{}/freeze-binding",
            command.environment_id
        ));
        let response = self
            .environment
            .post(uri)
            .headers(self.environment_headers().await?)
            .json(&EnvironmentFreezeBindingRequest {
                project_id: command.project_id,
                course_id: command.course_id,
                actor_id: command.actor_id,
                expected_revision: command.environment_revision,
                collector_public_key_openssh,
            })
            .send()
            .await?;
        if !response.status().is_success() {
            let status = response.status();
            if status == StatusCode::UNPROCESSABLE_ENTITY {
                let body = response.bytes().await?;
                return Err(classify_binding_rejection(status, &body));
            }
            return Err(classify_binding_rejection(status, &[]));
        }
        let bytes = response.bytes().await?;
        contracts::parse_strict_json(&bytes).map_err(|_| FreezeCoordinatorError::BindingInvalid)
    }

    async fn environment_headers(&self) -> Result<HeaderMap, FreezeCoordinatorError> {
        let mut headers = HeaderMap::new();
        self.environment_token_client
            .bearer_auth_for(
                &mut headers,
                &self.configuration.environment_audience,
                &self.environment_token_scopes,
            )
            .await
            .map_err(FreezeCoordinatorError::ServiceToken)?;
        Ok(headers)
    }

    async fn apply(
        &self,
        namespace: &str,
        api_version: &str,
        plural: &str,
        name: &str,
        document: Value,
    ) -> Result<(), FreezeCoordinatorError> {
        let response = self
            .authorized(self.kubernetes.request(
                Method::PATCH,
                self.resource_url(namespace, api_version, plural, name)?,
            ))
            .query(&[("fieldManager", FIELD_MANAGER), ("force", "true")])
            .header("content-type", "application/apply-patch+yaml")
            .body(serde_json::to_vec(&document)?)
            .send()
            .await?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(FreezeCoordinatorError::KubernetesRejected)
        }
    }

    async fn get(
        &self,
        namespace: &str,
        api_version: &str,
        plural: &str,
        name: &str,
    ) -> Result<Option<Value>, FreezeCoordinatorError> {
        let response = self
            .authorized(self.kubernetes.get(self.resource_url(
                namespace,
                api_version,
                plural,
                name,
            )?))
            .send()
            .await?;
        if response.status() == StatusCode::NOT_FOUND {
            Ok(None)
        } else if response.status().is_success() {
            Ok(Some(response.json().await?))
        } else {
            Err(FreezeCoordinatorError::KubernetesRejected)
        }
    }

    async fn cleanup(
        &self,
        namespace: &str,
        name: &str,
        command: &SubmissionFreezeCommand,
        profile: &FreezeWorkerProfile,
    ) -> Result<bool, FreezeCoordinatorError> {
        let owned_kinds = [
            ("batch/v1", "jobs"),
            ("v1", "configmaps"),
            ("v1", "secrets"),
            ("networking.k8s.io/v1", "networkpolicies"),
        ];
        let owned_kinds = match profile {
            FreezeWorkerProfile::Managed => owned_kinds.as_slice(),
            FreezeWorkerProfile::Existing { .. } => &owned_kinds[..2],
        };
        let mut pending = false;
        for &(api_version, plural) in owned_kinds {
            let Some(resource) = self.get(namespace, api_version, plural, name).await? else {
                continue;
            };
            verify_owned(&resource, command)?;
            verify_resource_identity(&resource, namespace, name)?;
            if resource.pointer("/metadata/deletionTimestamp").is_some() {
                pending = true;
                continue;
            }
            let uid = resource
                .pointer("/metadata/uid")
                .and_then(Value::as_str)
                .ok_or(FreezeCoordinatorError::KubernetesRejected)?;
            let response = self
                .authorized(self.kubernetes.delete(self.resource_url(
                    namespace,
                    api_version,
                    plural,
                    name,
                )?))
                .json(&json!({"propagationPolicy":"Foreground","preconditions":{"uid":uid}}))
                .send()
                .await?;
            if response.status() == StatusCode::CONFLICT {
                pending = true;
                continue;
            }
            if !(response.status().is_success() || response.status() == StatusCode::NOT_FOUND) {
                return Err(FreezeCoordinatorError::KubernetesRejected);
            }
        }
        for &(api_version, plural) in owned_kinds {
            if self
                .get(namespace, api_version, plural, name)
                .await?
                .is_some()
            {
                // Foreground deletion remains observable until the API server has removed the
                // object and all of its dependants.  Keep cleanup pending for every observation;
                // terminal command state is written only after a subsequent absence check.
                return Ok(false);
            }
        }
        Ok(!pending)
    }

    fn authorized(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        request.bearer_auth(&self.kubernetes_token)
    }

    fn resource_url(
        &self,
        namespace: &str,
        api_version: &str,
        plural: &str,
        name: &str,
    ) -> Result<Url, FreezeCoordinatorError> {
        validate_key(namespace)?;
        validate_key(name)?;
        let prefix = if api_version == "v1" {
            "/api/v1".to_owned()
        } else {
            format!("/apis/{api_version}")
        };
        self.configuration
            .kubernetes_api_server
            .join(&format!("{prefix}/namespaces/{namespace}/{plural}/{name}"))
            .map_err(|_| FreezeCoordinatorError::ConfigurationInvalid)
    }

    fn collection_url(
        &self,
        namespace: &str,
        api_version: &str,
        plural: &str,
    ) -> Result<Url, FreezeCoordinatorError> {
        validate_key(namespace)?;
        validate_key(plural)?;
        let prefix = if api_version == "v1" {
            "/api/v1".to_owned()
        } else {
            format!("/apis/{api_version}")
        };
        self.configuration
            .kubernetes_api_server
            .join(&format!("{prefix}/namespaces/{namespace}/{plural}"))
            .map_err(|_| FreezeCoordinatorError::ConfigurationInvalid)
    }
}

fn pod_failure_diagnostic(pods: &Value) -> Option<DiagnosticCode> {
    pods.pointer("/items")?
        .as_array()?
        .iter()
        .flat_map(|pod| {
            pod.pointer("/status/containerStatuses")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
        })
        .filter_map(|status| {
            status
                .pointer("/state/terminated/message")
                .and_then(Value::as_str)
        })
        .find_map(|message| DiagnosticCode::parse(message.trim()).ok())
}

fn validate_configuration(
    configuration: &FreezeCoordinatorConfiguration,
) -> Result<(), FreezeCoordinatorError> {
    let environment_host = configuration.environment_service_base_uri.host_str();
    if configuration.kubernetes_api_server.scheme() != "https"
        || configuration.environment_service_base_uri.scheme() != "https"
        || configuration.environment_service_base_uri.path() != "/"
        || environment_host.is_none()
        || configuration.environment_audience.trim().is_empty()
        || configuration
            .environment_audience
            .chars()
            .any(char::is_control)
        || !configuration.worker_image.contains("@sha256:")
        || configuration
            .worker_image
            .rsplit_once("@sha256:")
            .is_none_or(|(_, digest)| {
                digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
        || !(60..=3600).contains(&configuration.job_active_deadline_seconds)
        || !(100..=30_000).contains(&configuration.request_timeout_milliseconds)
        || !(1..=3650).contains(&configuration.retention_days)
        || !configuration.worker_profile.valid()
        || !configuration.worker_tls_ca_file.is_absolute()
    {
        return Err(FreezeCoordinatorError::ConfigurationInvalid);
    }
    if matches!(
        configuration.worker_profile,
        FreezeWorkerProfile::Existing { .. }
    ) {
        if !configuration.worker_secret_files.is_empty() {
            return Err(FreezeCoordinatorError::ConfigurationInvalid);
        }
        return Ok(());
    }
    if configuration.worker_secret_files.is_empty()
        || !configuration.worker_registry_pull_config_file.is_absolute()
        || !configuration.worker_tls_ca_file.is_absolute()
        || !configuration
            .worker_secret_files
            .values()
            .any(|path| path == &configuration.worker_tls_ca_file)
        || configuration.infrastructure_namespace_labels.is_empty()
        || configuration.dns_namespace_labels.is_empty()
        || configuration.dns_pod_labels.is_empty()
    {
        return Err(FreezeCoordinatorError::ConfigurationInvalid);
    }
    for value in [
        &configuration.worker_service_account_name,
        &configuration.vm_job_namespace,
    ] {
        validate_key(value)?;
    }
    Ok(())
}

fn validate_key(value: &str) -> Result<(), FreezeCoordinatorError> {
    if !crate::freeze_profile::valid_name(value) {
        return Err(FreezeCoordinatorError::ConfigurationInvalid);
    }
    Ok(())
}

fn read_bound_file(path: &PathBuf) -> Result<Vec<u8>, FreezeCoordinatorError> {
    if !path.is_absolute() {
        return Err(FreezeCoordinatorError::ConfigurationInvalid);
    }
    let parent = path
        .parent()
        .ok_or(FreezeCoordinatorError::ConfigurationInvalid)?;
    let canonical_parent = fs::canonicalize(parent)?;
    let canonical = fs::canonicalize(path)?;
    let metadata = fs::metadata(&canonical)?;
    if !canonical.starts_with(canonical_parent)
        || !metadata.is_file()
        || metadata.len() == 0
        || metadata.len() > MAX_BOUND_FILE_BYTES
    {
        return Err(FreezeCoordinatorError::ConfigurationInvalid);
    }
    Ok(fs::read(canonical)?)
}

fn read_bound_text(path: &PathBuf) -> Result<String, FreezeCoordinatorError> {
    String::from_utf8(read_bound_file(path)?)
        .map_err(|_| FreezeCoordinatorError::ConfigurationInvalid)
}

fn read_registry_pull_config(path: &PathBuf) -> Result<Vec<u8>, FreezeCoordinatorError> {
    let payload = read_bound_file(path)?;
    let value: Value = serde_json::from_slice(&payload)
        .map_err(|_| FreezeCoordinatorError::ConfigurationInvalid)?;
    let auths = value
        .as_object()
        .filter(|root| root.len() == 1)
        .and_then(|root| root.get("auths"))
        .and_then(Value::as_object)
        .filter(|auths| !auths.is_empty());
    if auths.is_none_or(|auths| {
        auths.iter().any(|(registry, credentials)| {
            registry.is_empty()
                || credentials.as_object().is_none_or(|credentials| {
                    credentials
                        .get("auth")
                        .and_then(Value::as_str)
                        .is_none_or(str::is_empty)
                })
        })
    }) {
        return Err(FreezeCoordinatorError::ConfigurationInvalid);
    }
    Ok(payload)
}

fn job_name(command: &SubmissionFreezeCommand) -> String {
    format!(
        "lw-freeze-{}",
        &command.frozen_submission_id.as_uuid().simple().to_string()[..20]
    )
}

fn resource_labels(command: &SubmissionFreezeCommand) -> Value {
    json!({
        "app.kubernetes.io/managed-by":FIELD_MANAGER,
        "app.kubernetes.io/name":"evaluation-freeze-worker",
        "labweaver.io/frozen-submission-id":command.frozen_submission_id.to_string(),
        "labweaver.io/environment-id":command.environment_id.to_string()
    })
}

fn verify_owned(
    resource: &Value,
    command: &SubmissionFreezeCommand,
) -> Result<(), FreezeCoordinatorError> {
    let expected_submission = command.frozen_submission_id.to_string();
    let expected_environment = command.environment_id.to_string();
    if resource
        .pointer("/metadata/labels/app.kubernetes.io~1managed-by")
        .and_then(Value::as_str)
        != Some(FIELD_MANAGER)
        || resource
            .pointer("/metadata/labels/labweaver.io~1frozen-submission-id")
            .and_then(Value::as_str)
            != Some(expected_submission.as_str())
        || resource
            .pointer("/metadata/labels/labweaver.io~1environment-id")
            .and_then(Value::as_str)
            != Some(expected_environment.as_str())
    {
        return Err(FreezeCoordinatorError::KubernetesRejected);
    }
    Ok(())
}

fn verify_existing_reference(
    resource: &Value,
    namespace: &str,
    name: &str,
) -> Result<(), FreezeCoordinatorError> {
    verify_resource_identity(resource, namespace, name)?;
    if resource.pointer("/metadata/deletionTimestamp").is_some() {
        return Err(FreezeCoordinatorError::ProfilePrerequisiteUnavailable);
    }
    Ok(())
}

fn verify_resource_identity(
    resource: &Value,
    namespace: &str,
    name: &str,
) -> Result<(), FreezeCoordinatorError> {
    if resource.pointer("/metadata/name").and_then(Value::as_str) != Some(name)
        || resource
            .pointer("/metadata/namespace")
            .and_then(Value::as_str)
            != Some(namespace)
        || resource
            .pointer("/metadata/uid")
            .and_then(Value::as_str)
            .is_none_or(str::is_empty)
    {
        return Err(FreezeCoordinatorError::ProfilePrerequisiteUnavailable);
    }
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum FreezeCoordinatorError {
    #[error("LW_COLLECT_COORDINATOR_CONFIG_INVALID")]
    ConfigurationInvalid,
    #[error("LW_COLLECT_COORDINATOR_CERTIFICATE_INVALID")]
    CertificateInvalid,
    #[error("LW_COLLECT_BINDING_INVALID")]
    BindingInvalid,
    #[error("LW_ENVIRONMENT_FREEZE_NOT_ELIGIBLE")]
    BindingNotEligible,
    #[error("LW_COLLECT_BINDING_UNAVAILABLE")]
    BindingUnavailable,
    #[error("LW_COLLECT_KUBERNETES_REJECTED")]
    KubernetesRejected,
    #[error("LW_COLLECT_PROFILE_RESOURCE_UNAVAILABLE")]
    ProfilePrerequisiteUnavailable,
    #[error("LW_COLLECT_QUOTA_UNAVAILABLE")]
    QuotaUnavailable,
    #[error("LW_COLLECT_QUOTA_EXCEEDED")]
    QuotaExceeded,
    #[error("LW_COLLECT_IO_FAILED")]
    Io(#[from] std::io::Error),
    #[error("LW_COLLECT_HTTP_FAILED")]
    Http(#[from] reqwest::Error),
    #[error("LW_COLLECT_SERVICE_TOKEN_FAILED")]
    ServiceToken(#[from] ServiceTokenClientError),
    #[error("LW_COLLECT_AUTHORING_ADMISSION_FAILED")]
    AuthoringAdmission(#[from] AuthoringAdmissionClientError),
    #[error("LW_COLLECT_EVALUATION_CONTROL_FAILED")]
    Evaluation(#[from] EvaluationControlStoreError),
    #[error("LW_COLLECT_JSON_FAILED")]
    Json(#[from] serde_json::Error),
    #[error("LW_COLLECT_IDENTITY_INVALID")]
    Ssh(#[from] russh::keys::ssh_key::Error),
    #[error(transparent)]
    Store(#[from] crate::FreezeCommandStoreError),
}

impl FreezeCoordinatorError {
    const fn is_systemic(&self) -> bool {
        matches!(
            self,
            Self::ConfigurationInvalid | Self::CertificateInvalid | Self::Io(_) | Self::Store(_)
        )
    }

    const fn is_terminal_command_error(&self) -> bool {
        matches!(
            self,
            Self::BindingInvalid | Self::BindingNotEligible | Self::Json(_) | Self::Ssh(_)
        )
    }

    const fn diagnostic_code(&self) -> &'static str {
        match self {
            Self::ConfigurationInvalid => "LW_COLLECT_COORDINATOR_CONFIG_INVALID",
            Self::CertificateInvalid => "LW_COLLECT_COORDINATOR_CERTIFICATE_INVALID",
            Self::BindingInvalid => "LW_COLLECT_BINDING_INVALID",
            Self::BindingNotEligible => "LW_ENVIRONMENT_FREEZE_NOT_ELIGIBLE",
            Self::BindingUnavailable => "LW_COLLECT_BINDING_UNAVAILABLE",
            Self::KubernetesRejected => "LW_COLLECT_KUBERNETES_REJECTED",
            Self::ProfilePrerequisiteUnavailable => "LW_COLLECT_PROFILE_RESOURCE_UNAVAILABLE",
            Self::QuotaUnavailable => "LW_COLLECT_QUOTA_UNAVAILABLE",
            Self::QuotaExceeded => "LW_COLLECT_QUOTA_EXCEEDED",
            Self::Io(_) => "LW_COLLECT_IO_FAILED",
            Self::Http(_) => "LW_COLLECT_HTTP_FAILED",
            Self::ServiceToken(_) => "LW_COLLECT_SERVICE_TOKEN_FAILED",
            Self::AuthoringAdmission(_) => "LW_COLLECT_AUTHORING_ADMISSION_FAILED",
            Self::Evaluation(_) => "LW_COLLECT_EVALUATION_CONTROL_FAILED",
            Self::Json(_) => "LW_COLLECT_JSON_FAILED",
            Self::Ssh(_) => "LW_COLLECT_IDENTITY_INVALID",
            Self::Store(_) => "LW_COLLECT_STORE_FAILED",
        }
    }
}

fn command_deadline_exceeded(
    requested_at: UtcTimestamp,
    authority_now: UtcTimestamp,
    deadline_seconds: u64,
) -> bool {
    let Ok(deadline_seconds) = i64::try_from(deadline_seconds) else {
        return true;
    };
    authority_now.get() >= requested_at.get() + time::Duration::seconds(deadline_seconds)
}

fn classify_binding_rejection(status: StatusCode, body: &[u8]) -> FreezeCoordinatorError {
    let not_eligible = serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|value| {
            value
                .get("diagnosticCode")
                .and_then(Value::as_str)
                .map(|code| code == "LW_ENVIRONMENT_FREEZE_NOT_ELIGIBLE")
        })
        .unwrap_or(false);
    if status == StatusCode::UNPROCESSABLE_ENTITY && not_eligible {
        FreezeCoordinatorError::BindingNotEligible
    } else {
        FreezeCoordinatorError::BindingUnavailable
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ENVIRONMENT_FREEZE_SCOPE, FreezeCoordinator, FreezeCoordinatorConfiguration,
        FreezeCoordinatorError, classify_binding_rejection, command_deadline_exceeded,
        pod_failure_diagnostic, read_registry_pull_config,
    };
    use crate::authoring_client::{
        AuthoringAdmissionClient, AuthoringAdmissionClientConfiguration,
        AuthoringAdmissionClientError,
    };
    use crate::{PgEvaluationControlStore, PgFreezeCommandStore, SubmissionFreezeCommand};
    use auth::{
        ServiceTokenClient, ServiceTokenClientConfig, ServiceTokenClientError,
        TransportSecurityMode,
    };
    use axum::{
        Json, Router,
        body::Body,
        extract::State,
        http::{Method, Request, StatusCode},
        response::{IntoResponse, Response},
        routing::{any, get, post},
    };
    use contracts::authoring::{PackageFile, RuntimeKind};
    use contracts::evaluation::{
        EvaluationExecutionBinding, EvaluationRuntimeIdentity, EvaluationSpec,
    };
    use contracts::http::{
        AuthoringPublicationAdmissionBinding, IdempotencyKey,
        InternalPublishEvaluationReleaseRequest,
    };
    use contracts::submission::{
        EnvironmentFreezeBinding, EnvironmentFreezeSourceBinding, FrozenEnvironmentIdentity,
        SubmissionManifest,
    };
    use contracts::{
        ActorId, AgentRunId, ApprovalId, ArtifactId, ArtifactRef, CandidateId, CourseId,
        EnvironmentId, FrozenSubmissionId, PolicyId, ProjectId, ReleaseId, RetentionClass,
        RetentionDisposition, RetentionSnapshot, Revision, UtcTimestamp,
    };
    use hyper_util::{
        rt::{TokioExecutor, TokioIo},
        server::conn::auto::Builder,
        service::TowerToHyperService,
    };
    use persistence_sqlx::{Domain, MigrationCatalog, Sha256Digest};
    use rcgen::{
        BasicConstraints, CertificateParams, CertifiedIssuer, ExtendedKeyUsagePurpose, IsCa,
        KeyPair, KeyUsagePurpose,
    };
    use reqwest::{Certificate, Client, Url};
    use rustls::{ServerConfig, pki_types::PrivateKeyDer};
    use serde_json::{Value, json};
    use sqlx::postgres::PgPoolOptions;
    use std::{
        collections::{BTreeMap, BTreeSet},
        error::Error,
        fs,
        io::Cursor,
        path::Path,
        sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        },
    };
    use tempfile::TempDir;
    use testcontainers::{ImageExt, runners::AsyncRunner};
    use testcontainers_modules::postgres::Postgres;
    use tokio::net::TcpListener;
    use tokio_rustls::TlsAcceptor;

    #[test]
    fn existing_profile_example_requires_no_worker_credential_files()
    -> Result<(), Box<dyn std::error::Error>> {
        let base: Value = serde_yaml::from_str(include_str!(
            "../../../deploy/config/evaluation-service.yaml.example"
        ))?;
        let mut configuration: FreezeCoordinatorConfiguration =
            serde_json::from_value(base["coordinator"].clone())?;
        assert!(super::validate_configuration(&configuration).is_ok());
        let fragment: Value = serde_yaml::from_str(include_str!(
            "../../../deploy/config/evaluation-freeze-existing-profile.yaml.example"
        ))?;
        configuration.worker_profile =
            serde_json::from_value(fragment["coordinator"]["workerProfile"].clone())?;
        assert!(
            super::validate_configuration(&configuration).is_err(),
            "existing mode rejects credential-copy configuration"
        );
        configuration.worker_secret_files.clear();
        configuration.worker_registry_pull_config_file =
            Path::new("not-a-credential-file").to_owned();
        assert!(super::validate_configuration(&configuration).is_ok());
        assert!(
            configuration
                .worker_profile
                .accepts_namespace("lw-bundle-synthetic-example")
        );
        assert!(
            !configuration
                .worker_profile
                .accepts_namespace("other-namespace")
        );
        Ok(())
    }

    #[test]
    fn unavailable_binding_is_retried_until_the_command_deadline()
    -> Result<(), Box<dyn std::error::Error>> {
        let error = FreezeCoordinatorError::BindingUnavailable;
        assert!(!error.is_systemic());
        assert!(!error.is_terminal_command_error());
        assert_eq!(error.diagnostic_code(), "LW_COLLECT_BINDING_UNAVAILABLE");
        let requested = "2026-07-22T00:00:00.000Z".parse::<UtcTimestamp>()?;
        let before = "2026-07-22T00:04:58.000Z".parse::<UtcTimestamp>()?;
        let deadline = "2026-07-22T00:04:59.000Z".parse::<UtcTimestamp>()?;
        assert!(!command_deadline_exceeded(requested, before, 299));
        assert!(command_deadline_exceeded(requested, deadline, 299));
        Ok(())
    }

    #[test]
    fn confirmed_environment_freeze_rejection_is_terminal_but_service_unavailable_retries() {
        let not_eligible = classify_binding_rejection(
            StatusCode::UNPROCESSABLE_ENTITY,
            br#"{"diagnosticCode":"LW_ENVIRONMENT_FREEZE_NOT_ELIGIBLE"}"#,
        );
        assert!(not_eligible.is_terminal_command_error());
        assert_eq!(
            not_eligible.diagnostic_code(),
            "LW_ENVIRONMENT_FREEZE_NOT_ELIGIBLE"
        );

        let unavailable = classify_binding_rejection(StatusCode::SERVICE_UNAVAILABLE, &[]);
        assert!(!unavailable.is_terminal_command_error());
        assert_eq!(
            unavailable.diagnostic_code(),
            "LW_COLLECT_BINDING_UNAVAILABLE"
        );
    }

    #[test]
    fn temporary_authoring_admission_failures_are_retried() {
        for error in [
            FreezeCoordinatorError::AuthoringAdmission(AuthoringAdmissionClientError::Unavailable),
            FreezeCoordinatorError::AuthoringAdmission(AuthoringAdmissionClientError::Transport),
            FreezeCoordinatorError::AuthoringAdmission(AuthoringAdmissionClientError::Token(
                ServiceTokenClientError::TokenExchange(auth::ServiceTokenExchangeFailure {
                    kind: auth::ServiceTokenFailureKind::Rejected,
                    http_status: Some(401),
                }),
            )),
        ] {
            assert!(!error.is_systemic());
            assert!(!error.is_terminal_command_error());
        }
    }

    #[test]
    fn invalid_binding_is_a_terminal_command_error() {
        let error = FreezeCoordinatorError::BindingInvalid;
        assert!(!error.is_systemic());
        assert!(error.is_terminal_command_error());
        assert_eq!(error.diagnostic_code(), "LW_COLLECT_BINDING_INVALID");
    }

    #[test]
    fn worker_termination_message_accepts_only_bounded_diagnostics() {
        let pods = json!({
            "items": [{
                "status": {
                    "containerStatuses": [{
                        "state": {
                            "terminated": {
                                "message": "LW_COLLECT_SSH_CREDENTIAL_INVALID\n"
                            }
                        }
                    }]
                }
            }]
        });
        assert_eq!(
            pod_failure_diagnostic(&pods)
                .as_ref()
                .map(contracts::DiagnosticCode::as_str),
            Some("LW_COLLECT_SSH_CREDENTIAL_INVALID")
        );

        let unsafe_message = json!({
            "items": [{
                "status": {
                    "containerStatuses": [{
                        "state": {
                            "terminated": {
                                "message": "database failed: password=secret"
                            }
                        }
                    }]
                }
            }]
        });
        assert!(pod_failure_diagnostic(&unsafe_message).is_none());
    }

    #[test]
    fn registry_pull_config_requires_one_non_empty_auth_map()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("registry.json");
        fs::write(
            &path,
            br#"{"auths":{"harbor.lab.lan":{"auth":"cm9ib3Q6c2VjcmV0"}}}"#,
        )?;
        assert!(read_registry_pull_config(&path).is_ok());
        fs::write(&path, br#"{"auths":{"harbor.lab.lan":{}}}"#)?;
        assert!(read_registry_pull_config(&path).is_err());
        fs::write(&path, br#"{"auths":{}}"#)?;
        assert!(read_registry_pull_config(&path).is_err());
        Ok(())
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the integration fixture keeps the complete admission and Kubernetes boundary in one scenario"
    )]
    #[tokio::test]
    async fn coordinator_profiles_preserve_admission_and_recover_dispatch_without_binding()
    -> Result<(), Box<dyn std::error::Error>> {
        let database = Postgres::default().with_tag("17.5-alpine").start().await?;
        let database_url = format!(
            "postgres://postgres:postgres@127.0.0.1:{}/postgres",
            database.get_host_port_ipv4(5432).await?
        );
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .connect(&database_url)
            .await?;
        apply_evaluation_migrations(&pool).await?;

        let project_id = ProjectId::new();
        let course_id = CourseId::new();
        let actor_id = ActorId::new();
        let environment_id = EnvironmentId::new();
        let environment_release_id = ReleaseId::new();
        let environment_revision = Revision::new(1)?;
        let evaluation_spec = EvaluationSpec::from_yaml(
            r#"apiVersion: evaluation.labweaver.io/v1
kind: EvaluationSpec
metadata:
  name: coordinator-manifest-v1
  version: "1.0.0"
spec:
  submission:
    collector:
      kind: workspace_snapshot
      include: [answer.txt]
      maxBytes: 1024
    llmReadable: [answer.txt]
  steps:
    - role: score
      id: score-answer
      runner:
        kind: file_assertion
        requiredFiles: [answer.txt]
      checker:
        kind: exit_code
        expected: 0
      score:
        max: 1
      failurePolicy: stop
  aggregation:
    kind: deterministic_sum
    maxScore: 1
    gates: []
  review:
    teacherApprovalRequiredForRelease: true
    forceManualWhen: []
"#,
        )?;
        let expected_manifest = SubmissionManifest::from_evaluation_spec(&evaluation_spec)?;
        let mut requested_manifest = expected_manifest.clone();
        requested_manifest.include[0] = contracts::PathRule::ExactFile {
            path: "tampered.txt".to_owned(),
        };

        let evaluation = PgEvaluationControlStore::new(pool.clone());
        let now = evaluation.authority_now().await?;
        let runtime_identity = EvaluationRuntimeIdentity {
            provider_binding: "kubernetes/test".to_owned(),
            runner_image: format!(
                "registry.example/labweaver/evaluation-worker@sha256:{}",
                Sha256Digest::of_bytes(b"coordinator-test-runner")
            ),
        };
        let package_artifact_id = ArtifactId::new();
        let package = contracts::authoring::ProblemPackage {
            id: contracts::ProblemPackageId::new(),
            project_id,
            course_id: Some(course_id),
            revision: Revision::new(1)?,
            files: vec![PackageFile {
                sha256: "0".repeat(64),
                path: "program.json".to_owned(),
                object: ArtifactRef {
                    artifact_id: package_artifact_id,
                    store_binding: "test-store".to_owned(),
                    object_version: "v1".to_owned(),
                    size_bytes: 1,
                    media_type: "application/json".to_owned(),
                },
            }],
            retention: RetentionSnapshot {
                policy_id: PolicyId::new(),
                policy_revision: Revision::new(1)?,
                class: RetentionClass::CourseMaterial,
                retain_until: Some("2027-01-01T00:00:00.000Z".parse()?),
                disposition: RetentionDisposition::Delete,
            },
            completed_at: "2026-01-01T00:00:00.000Z".parse()?,
        };
        let execution_binding = EvaluationExecutionBinding {
            package,
            object_locators: BTreeMap::from([(
                package_artifact_id,
                "packages/program.json".to_owned(),
            )]),
        };
        let publish_request = InternalPublishEvaluationReleaseRequest {
            project_id,
            course_id: Some(course_id),
            candidate_id: CandidateId::new(),
            candidate_revision: Revision::new(1)?,
            approval_id: ApprovalId::new(),
            approval_revision: Revision::new(1)?,
            evaluation_spec,
            execution_binding,
            runtime_identity,
            published_by: actor_id,
        };
        let release = match evaluation
            .publish_release(
                &publish_request,
                &IdempotencyKey::parse("coordinator-manifest-release")?,
                now,
                "coordinator-manifest-release",
            )
            .await?
        {
            crate::EvaluationReleaseReservation::Created(value)
            | crate::EvaluationReleaseReservation::Replayed(value) => value,
        };

        let environment_binding = EnvironmentFreezeBinding {
            environment: FrozenEnvironmentIdentity {
                environment_id,
                environment_revision,
                release_id: environment_release_id,
                release_version: 1,
                runtime_kind: RuntimeKind::Container,
                build_request_id: None,
            },
            agent_run_id: AgentRunId::new(),
            source: EnvironmentFreezeSourceBinding::Container {
                namespace: "student".to_owned(),
                persistent_volume_claim: "student-workspace".to_owned(),
                storage_class_name: "standard".to_owned(),
            },
        };
        let environment_server = EnvironmentServer::start(environment_binding).await?;
        let token_client = Arc::new(
            ServiceTokenClient::discover(
                ServiceTokenClientConfig::new(
                    &environment_server.issuer,
                    "evaluation-coordinator-test".to_owned(),
                    "test-secret".to_owned(),
                    "environment".to_owned(),
                    BTreeSet::from([ENVIRONMENT_FREEZE_SCOPE.to_owned()]),
                    1,
                    TransportSecurityMode::InsecureTestOnly,
                )?,
                Client::builder().no_proxy().build()?,
            )
            .await?,
        );
        let admission_server = AdmissionServer::start().await?;
        let temporary = TempDir::new()?;
        let authoring_admission = build_authoring_client(
            &admission_server,
            token_client.clone(),
            &temporary,
            AuthoringPublicationAdmissionBinding {
                approval_id: release.approval_id,
                approval_revision: release.approval_revision,
                project_id,
                course_id: Some(course_id),
                environment_release_id,
                environment_release_version: 1,
                evaluation_release_id: release.id,
                evaluation_release_revision: release.revision,
            },
        )?;
        let kubernetes_server = KubernetesServer::start().await?;
        let mut coordinator = FreezeCoordinator {
            configuration: FreezeCoordinatorConfiguration {
                worker_profile: crate::FreezeWorkerProfile::Managed,
                kubernetes_api_server: kubernetes_server.base_uri.clone(),
                kubernetes_bearer_token_file: Path::new("unused").to_owned(),
                kubernetes_ca_file: Path::new("unused").to_owned(),
                environment_service_base_uri: environment_server.base_uri.clone(),
                environment_ca_file: Path::new("unused").to_owned(),
                environment_audience: "environment".to_owned(),
                worker_image: format!(
                    "registry.example/labweaver/freeze-worker@sha256:{}",
                    Sha256Digest::of_bytes(b"freeze-worker")
                ),
                worker_service_account_name: "freeze-worker".to_owned(),
                vm_job_namespace: "vm-jobs".to_owned(),
                worker_configuration_file: Path::new("unused").to_owned(),
                worker_secret_files: BTreeMap::new(),
                worker_registry_pull_config_file: Path::new("unused").to_owned(),
                worker_tls_ca_file: Path::new("unused").to_owned(),
                infrastructure_namespace_labels: BTreeMap::from([(
                    "managed".to_owned(),
                    "true".to_owned(),
                )]),
                dns_namespace_labels: BTreeMap::from([("managed".to_owned(), "true".to_owned())]),
                dns_pod_labels: BTreeMap::from([("managed".to_owned(), "true".to_owned())]),
                retention_policy_id: PolicyId::new(),
                retention_policy_revision: Revision::new(1)?,
                retention_days: 1,
                job_active_deadline_seconds: 60,
                request_timeout_milliseconds: 2_000,
            },
            store: PgFreezeCommandStore::new(pool.clone()),
            kubernetes: Client::builder().no_proxy().build()?,
            environment: Client::builder().no_proxy().build()?,
            environment_token_client: token_client,
            environment_token_scopes: BTreeSet::from([ENVIRONMENT_FREEZE_SCOPE.to_owned()]),
            authoring_admission,
            evaluation,
            kubernetes_token: "test-kubernetes-token".to_owned(),
            worker_configuration: "{}".to_owned(),
            worker_secrets: BTreeMap::new(),
            worker_registry_pull_config: Vec::new(),
        };
        let mut command = SubmissionFreezeCommand {
            input_authorization_context: None,
            worker_dispatch: crate::FreezeWorkerDispatch::Pending,
            frozen_submission_id: FrozenSubmissionId::new(),
            operation_id: contracts::OperationId::new(),
            project_id,
            course_id: Some(course_id),
            environment_id,
            actor_id,
            environment_revision,
            manifest_revision: Revision::new(1)?,
            manifest: requested_manifest,
            idempotency_key: "coordinator-manifest-mismatch".to_owned(),
            trace_id: "coordinator-manifest-mismatch".to_owned(),
            requested_at: now,
        };

        let result = coordinator
            .create_resources(&command, "student", "lw-freeze-test")
            .await;
        assert!(matches!(
            result,
            Err(FreezeCoordinatorError::BindingInvalid)
        ));
        assert_eq!(
            kubernetes_server.patch_count.load(Ordering::Acquire),
            0,
            "manifest mismatch must reject before ConfigMap, Secret, or Job apply"
        );
        kubernetes_server.objects.lock().map_err(|_| "Kubernetes fixture lock")?.insert(
            "/apis/batch/v1/namespaces/student/jobs/lw-freeze-test".to_owned(),
            json!({"metadata":{"name":"lw-freeze-test","namespace":"student",
                "uid":FrozenSubmissionId::new().to_string(),"labels":super::resource_labels(&command),
                "deletionTimestamp":"2026-09-12T00:00:00Z"}}),
        );
        assert!(
            !coordinator
                .cleanup(
                    "student",
                    "lw-freeze-test",
                    &command,
                    &crate::FreezeWorkerProfile::Managed
                )
                .await?,
            "a foreground Job that is still observed must keep cleanup pending"
        );
        assert!(
            coordinator
                .cleanup(
                    "student",
                    "lw-freeze-test",
                    &command,
                    &crate::FreezeWorkerProfile::Managed
                )
                .await?,
            "cleanup completes only after the Job and all owned objects disappear"
        );

        // The same authenticated producer and real PostgreSQL queue now exercise the existing profile.
        let profile = existing_profile();
        coordinator.configuration.worker_profile = profile.clone();
        command.manifest = expected_manifest;
        seed_existing_resources(&kubernetes_server, "student")?;
        coordinator.store.accept(&command).await?;
        kubernetes_server
            .lose_next_job_response
            .store(1, Ordering::Release);
        coordinator.reconcile_once().await?;
        let first_requests = kubernetes_server
            .requests
            .lock()
            .map_err(|_| "request log lock")?
            .clone();
        let writes: Vec<_> = first_requests
            .iter()
            .filter(|(method, _, _)| method == Method::POST.as_str())
            .collect();
        assert_eq!(
            writes.len(),
            2,
            "only ConfigMap and Job are created despite the lost response"
        );
        assert!(
            writes
                .iter()
                .all(|(_, path, _)| path.ends_with("/configmaps") || path.ends_with("/jobs"))
        );
        assert_eq!(kubernetes_server.patch_count.load(Ordering::Acquire), 0);
        let bound = coordinator.store.refresh_worker_dispatch(&command).await?;
        assert_eq!(
            bound.worker_dispatch,
            crate::FreezeWorkerDispatch::Bound {
                namespace: "student".to_owned(),
                profile: profile.clone()
            }
        );
        let job_path = format!(
            "/apis/batch/v1/namespaces/student/jobs/{}",
            super::job_name(&command)
        );
        let config_path = format!(
            "/api/v1/namespaces/student/configmaps/{}",
            super::job_name(&command)
        );
        let objects = kubernetes_server
            .objects
            .lock()
            .map_err(|_| "object lock")?
            .clone();
        let job = objects.get(&job_path).ok_or("worker Job missing")?;
        assert_eq!(
            job.pointer("/spec/template/spec/serviceAccountName"),
            Some(&json!("synthetic-freeze-account"))
        );
        assert_eq!(
            job.pointer("/spec/template/spec/containers/0/imagePullPolicy"),
            Some(&json!("Never"))
        );
        assert_eq!(
            job.pointer("/spec/template/spec/containers/0/resources/limits/memory"),
            Some(&json!("268435456"))
        );
        assert_eq!(
            job.pointer("/spec/template/spec/volumes/1/secret/secretName"),
            Some(&json!("synthetic-freeze-secret"))
        );
        assert_eq!(
            job.pointer("/spec/template/spec/volumes/2/persistentVolumeClaim/claimName"),
            Some(&json!("student-workspace"))
        );
        assert_eq!(
            job.pointer("/spec/template/spec/volumes/2/persistentVolumeClaim/readOnly"),
            Some(&json!(true))
        );
        let original_config = objects
            .get(&config_path)
            .cloned()
            .ok_or("ConfigMap missing")?;
        assert_eq!(
            job.pointer("/spec/template/spec/nodeSelector/kubernetes.io~1hostname"),
            Some(&json!("synthetic-node"))
        );
        let account_path = "/api/v1/namespaces/student/serviceaccounts/synthetic-freeze-account";
        let account = kubernetes_server
            .objects
            .lock()
            .map_err(|_| "object lock")?
            .remove(account_path)
            .ok_or("account missing")?;
        let command_json = original_config
            .pointer("/data/command.json")
            .and_then(Value::as_str)
            .ok_or("command missing")?;
        let workspace = job
            .pointer("/spec/template/spec/volumes/2")
            .cloned()
            .ok_or("workspace volume missing")?;
        assert!(matches!(
            coordinator
                .create_existing_resources(
                    &bound,
                    "student",
                    &super::job_name(&command),
                    command_json,
                    workspace.clone(),
                    &profile
                )
                .await,
            Err(FreezeCoordinatorError::ProfilePrerequisiteUnavailable)
        ));
        kubernetes_server
            .objects
            .lock()
            .map_err(|_| "object lock")?
            .insert(account_path.to_owned(), account);
        let secret_path = "/api/v1/namespaces/student/secrets/synthetic-freeze-secret";
        {
            let mut objects = kubernetes_server
                .objects
                .lock()
                .map_err(|_| "object lock")?;
            objects
                .get_mut(secret_path)
                .ok_or("secret metadata missing")?["kind"] = json!("Secret");
        }
        assert!(
            matches!(
                coordinator
                    .create_existing_resources(
                        &bound,
                        "student",
                        &super::job_name(&command),
                        command_json,
                        workspace,
                        &profile
                    )
                    .await,
                Err(FreezeCoordinatorError::ProfilePrerequisiteUnavailable)
            ),
            "full Secret representation is not a metadata fallback"
        );
        kubernetes_server
            .objects
            .lock()
            .map_err(|_| "object lock")?
            .get_mut(secret_path)
            .ok_or("secret metadata missing")?["kind"] = json!("PartialObjectMetadata");

        // The quota already accounts for the Work and other Pods. A full namespace cannot create another worker.
        let mut blocked = command.clone();
        blocked.frozen_submission_id = FrozenSubmissionId::new();
        blocked.operation_id = contracts::OperationId::new();
        blocked.idempotency_key = "coordinator-quota-full".to_owned();
        let quota_path = "/api/v1/namespaces/student/resourcequotas/synthetic-budget";
        {
            let mut objects = kubernetes_server
                .objects
                .lock()
                .map_err(|_| "object lock")?;
            objects.get_mut(quota_path).ok_or("quota missing")?["status"]["used"]["limits.cpu"] =
                json!("900m");
        }
        coordinator.store.accept(&blocked).await?;
        coordinator.reconcile_once().await?;
        assert_eq!(
            kubernetes_server
                .requests
                .lock()
                .map_err(|_| "request log lock")?
                .iter()
                .filter(|(method, _, _)| method == Method::POST.as_str())
                .count(),
            2,
            "quota rejection must occur before any ConfigMap or Job creation"
        );

        // No fresh binding is available after this point. Observation and cleanup must use the ledger.
        environment_server.stop().await;
        if let crate::FreezeWorkerProfile::Existing { namespace, .. } =
            &mut coordinator.configuration.worker_profile
        {
            *namespace = "another-synthetic-namespace".to_owned();
        }
        coordinator.reconcile_once().await?;
        let requests = kubernetes_server
            .requests
            .lock()
            .map_err(|_| "request log lock")?
            .clone();
        assert_eq!(
            requests
                .iter()
                .filter(|(method, _, _)| method == Method::POST.as_str())
                .count(),
            2
        );
        assert_eq!(
            kubernetes_server
                .objects
                .lock()
                .map_err(|_| "object lock")?
                .get(&config_path),
            Some(&original_config)
        );
        {
            let mut objects = kubernetes_server
                .objects
                .lock()
                .map_err(|_| "object lock")?;
            objects.get_mut(&job_path).ok_or("Job missing")?["metadata"]["labels"]["labweaver.io/environment-id"] =
                json!(EnvironmentId::new().to_string());
        }
        assert!(
            matches!(
                coordinator.cleanup_command_resources(&command).await,
                Err(FreezeCoordinatorError::KubernetesRejected)
            ),
            "foreign ownership must block cleanup"
        );
        assert!(
            !kubernetes_server
                .requests
                .lock()
                .map_err(|_| "request log lock")?
                .iter()
                .any(|(method, _, _)| method == Method::DELETE.as_str())
        );
        kubernetes_server
            .objects
            .lock()
            .map_err(|_| "object lock")?
            .insert(job_path.clone(), job.clone());
        assert!(
            coordinator.cleanup_command_resources(&command).await?,
            "refresh Pending and recover the real dispatch after binding expiry"
        );
        {
            let final_objects = kubernetes_server
                .objects
                .lock()
                .map_err(|_| "object lock")?;
            assert!(
                !final_objects.contains_key(&job_path) && !final_objects.contains_key(&config_path)
            );
            assert!(
                final_objects
                    .keys()
                    .any(|path| path.ends_with("/secrets/synthetic-freeze-secret"))
            );
            assert!(
                final_objects
                    .keys()
                    .any(|path| path.ends_with("/serviceaccounts/synthetic-freeze-account"))
            );
            assert!(
                final_objects
                    .keys()
                    .any(|path| path.ends_with("/networkpolicies/synthetic-isolation"))
            );
        }
        let requests = kubernetes_server
            .requests
            .lock()
            .map_err(|_| "request log lock")?
            .clone();
        let deletes: Vec<_> = requests
            .iter()
            .filter(|(method, _, _)| method == Method::DELETE.as_str())
            .collect();
        assert_eq!(deletes.len(), 2);
        assert!(deletes.iter().all(
            |(_, path, body)| (path == &job_path || path == &config_path)
                && body.pointer("/preconditions/uid").is_some()
        ));

        kubernetes_server.stop().await;
        admission_server.stop().await;
        drop(database);
        Ok(())
    }

    async fn apply_evaluation_migrations(pool: &sqlx::PgPool) -> Result<(), Box<dyn Error>> {
        sqlx::query("CREATE SCHEMA evaluation")
            .execute(pool)
            .await?;
        let mut connection = pool.acquire().await?;
        sqlx::query("SET search_path = evaluation, pg_catalog")
            .execute(&mut *connection)
            .await?;
        let migration_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../migrations");
        let catalog = MigrationCatalog::load(&migration_root.join("catalog.yaml"))?;
        let domain = catalog
            .domains
            .iter()
            .find(|domain| domain.name == Domain::Evaluation)
            .ok_or("evaluation migration domain missing")?;
        for migration in &domain.migrations {
            let sql = MigrationCatalog::read_verified_sql(&migration_root, migration)?;
            sqlx::raw_sql(sqlx::AssertSqlSafe(&*sql))
                .execute(&mut *connection)
                .await?;
        }
        Ok(())
    }

    struct EnvironmentServer {
        base_uri: Url,
        issuer: String,
        task: tokio::task::JoinHandle<()>,
    }

    impl EnvironmentServer {
        async fn start(binding: EnvironmentFreezeBinding) -> Result<Self, Box<dyn Error>> {
            let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
            let port = listener.local_addr()?.port();
            let base_uri = Url::parse(&format!("http://127.0.0.1:{port}/"))?;
            let issuer = format!("http://localhost:{port}/realms/test");
            let discovery_issuer = issuer.clone();
            let router = Router::new()
                .route(
                    "/realms/test/.well-known/openid-configuration",
                    get(move || {
                        let issuer = discovery_issuer.clone();
                        async move {
                            Json(json!({
                                "issuer": issuer,
                                "authorization_endpoint": format!("{issuer}/authorize"),
                                "token_endpoint": format!("{issuer}/token"),
                                "jwks_uri": format!("{issuer}/jwks"),
                                "response_types_supported": ["code"],
                                "subject_types_supported": ["public"],
                                "id_token_signing_alg_values_supported": ["ES256"],
                                "grant_types_supported": ["authorization_code", "client_credentials"]
                            }))
                        }
                    }),
                )
                .route(
                    "/realms/test/token",
                    post(|| async {
                        Json(json!({
                            "access_token": "eyJhbGciOiJub25lIn0.eyJhdWQiOlsiZW52aXJvbm1lbnQiLCJjb250cm9sIl19.sig",
                            "token_type": "Bearer",
                            "expires_in": 300
                        }))
                    }),
                )
                .route(
                    "/realms/test/jwks",
                    get(|| async { Json(json!({ "keys": [] })) }),
                )
                .fallback(any(environment_request))
                .with_state(binding);
            let task = tokio::spawn(async move {
                let _ = axum::serve(listener, router).await;
            });
            Ok(Self {
                base_uri,
                issuer,
                task,
            })
        }

        async fn stop(self) {
            self.task.abort();
            let _ = self.task.await;
        }
    }

    async fn environment_request(
        State(binding): State<EnvironmentFreezeBinding>,
        request: Request<Body>,
    ) -> Response {
        if request.method() == Method::POST
            && request
                .uri()
                .path()
                .starts_with("/internal/v1/environments/")
        {
            (StatusCode::OK, Json(binding)).into_response()
        } else {
            StatusCode::NOT_FOUND.into_response()
        }
    }

    type KubernetesRequestLog = Vec<(String, String, Value)>;

    struct KubernetesServer {
        base_uri: Url,
        patch_count: Arc<AtomicUsize>,
        objects: Arc<Mutex<BTreeMap<String, Value>>>,
        requests: Arc<Mutex<KubernetesRequestLog>>,
        lose_next_job_response: Arc<AtomicUsize>,
        task: tokio::task::JoinHandle<()>,
    }

    impl KubernetesServer {
        async fn start() -> Result<Self, Box<dyn Error>> {
            let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
            let port = listener.local_addr()?.port();
            let base_uri = Url::parse(&format!("http://127.0.0.1:{port}/"))?;
            let patch_count = Arc::new(AtomicUsize::new(0));
            let deleting_job_reads = Arc::new(AtomicUsize::new(1));
            let objects = Arc::new(Mutex::new(BTreeMap::new()));
            let requests = Arc::new(Mutex::new(Vec::new()));
            let lose_next_job_response = Arc::new(AtomicUsize::new(0));
            let router =
                Router::new()
                    .fallback(any(kubernetes_request))
                    .with_state(KubernetesState {
                        patch_count: patch_count.clone(),
                        deleting_job_reads: deleting_job_reads.clone(),
                        objects: objects.clone(),
                        requests: requests.clone(),
                        lose_next_job_response: lose_next_job_response.clone(),
                    });
            let task = tokio::spawn(async move {
                let _ = axum::serve(listener, router).await;
            });
            Ok(Self {
                base_uri,
                patch_count,
                objects,
                requests,
                lose_next_job_response,
                task,
            })
        }

        async fn stop(self) {
            self.task.abort();
            let _ = self.task.await;
        }
    }

    #[derive(Clone)]
    struct KubernetesState {
        patch_count: Arc<AtomicUsize>,
        deleting_job_reads: Arc<AtomicUsize>,
        objects: Arc<Mutex<BTreeMap<String, Value>>>,
        requests: Arc<Mutex<KubernetesRequestLog>>,
        lose_next_job_response: Arc<AtomicUsize>,
    }

    async fn kubernetes_request(
        State(state): State<KubernetesState>,
        request: Request<Body>,
    ) -> Response {
        let method = request.method().clone();
        let path = request.uri().path().to_owned();
        let accept = request
            .headers()
            .get("accept")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let Ok(bytes) = axum::body::to_bytes(request.into_body(), 1024 * 1024).await else {
            return StatusCode::BAD_REQUEST.into_response();
        };
        let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        if let Ok(mut requests) = state.requests.lock() {
            requests.push((method.to_string(), path.clone(), body.clone()));
        } else {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
        if method == Method::PATCH {
            state.patch_count.fetch_add(1, Ordering::AcqRel);
            StatusCode::OK.into_response()
        } else if method == Method::DELETE {
            let Ok(mut objects) = state.objects.lock() else {
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            };
            if let Some(object) = objects.get(&path) {
                if body.pointer("/preconditions/uid") != object.pointer("/metadata/uid") {
                    return StatusCode::CONFLICT.into_response();
                }
                objects.remove(&path);
            }
            StatusCode::OK.into_response()
        } else if method == Method::POST {
            let Some(name) = body.pointer("/metadata/name").and_then(Value::as_str) else {
                return StatusCode::BAD_REQUEST.into_response();
            };
            let object_path = format!("{path}/{name}");
            let Ok(mut objects) = state.objects.lock() else {
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            };
            if objects.contains_key(&object_path) {
                return StatusCode::CONFLICT.into_response();
            }
            let mut object = body;
            object["metadata"]["uid"] = json!(FrozenSubmissionId::new().to_string());
            objects.insert(object_path, object.clone());
            if path.ends_with("/jobs")
                && state
                    .lose_next_job_response
                    .fetch_update(Ordering::AcqRel, Ordering::Acquire, |v| v.checked_sub(1))
                    .is_ok()
            {
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }
            (StatusCode::CREATED, Json(object)).into_response()
        } else if method == Method::GET {
            if path.ends_with("/secrets/synthetic-freeze-secret")
                && accept.as_deref()
                    != Some("application/json;as=PartialObjectMetadata;g=meta.k8s.io;v=v1")
            {
                return StatusCode::NOT_ACCEPTABLE.into_response();
            }
            let Ok(mut objects) = state.objects.lock() else {
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            };
            let Some(object) = objects.get(&path).cloned() else {
                return StatusCode::NOT_FOUND.into_response();
            };
            if object.pointer("/metadata/deletionTimestamp").is_some()
                && state
                    .deleting_job_reads
                    .fetch_update(Ordering::AcqRel, Ordering::Acquire, |v| v.checked_sub(1))
                    .is_err()
            {
                objects.remove(&path);
                return StatusCode::NOT_FOUND.into_response();
            }
            (StatusCode::OK, Json(object)).into_response()
        } else {
            StatusCode::NOT_FOUND.into_response()
        }
    }

    fn existing_profile() -> crate::FreezeWorkerProfile {
        crate::FreezeWorkerProfile::Existing {
            namespace: "student".to_owned(),
            secret_name: "synthetic-freeze-secret".to_owned(),
            service_account_name: "synthetic-freeze-account".to_owned(),
            network_policy_names: vec!["synthetic-isolation".to_owned()],
            resource_quota_names: vec!["synthetic-budget".to_owned()],
            node_name: Some("synthetic-node".to_owned()),
            resources: crate::FreezeWorkerResources {
                request_cpu_millicores: 100,
                limit_cpu_millicores: 500,
                request_memory_bytes: 64 * 1024 * 1024,
                limit_memory_bytes: 256 * 1024 * 1024,
            },
        }
    }

    fn seed_existing_resources(
        server: &KubernetesServer,
        namespace: &str,
    ) -> Result<(), Box<dyn Error>> {
        let mut objects = server.objects.lock().map_err(|_| "object lock")?;
        let metadata = |name| json!({"name":name,"namespace":namespace,"uid":FrozenSubmissionId::new().to_string()});
        objects.insert(format!("/api/v1/namespaces/{namespace}/secrets/synthetic-freeze-secret"),
            json!({"apiVersion":"meta.k8s.io/v1","kind":"PartialObjectMetadata","metadata":metadata("synthetic-freeze-secret")}));
        objects.insert(
            format!("/api/v1/namespaces/{namespace}/serviceaccounts/synthetic-freeze-account"),
            json!({"metadata":metadata("synthetic-freeze-account")}),
        );
        objects.insert(format!("/apis/networking.k8s.io/v1/namespaces/{namespace}/networkpolicies/synthetic-isolation"),
            json!({"metadata":metadata("synthetic-isolation"),"spec":{"podSelector":{},"policyTypes":["Ingress","Egress"],"ingress":[],"egress":[]}}));
        let hard = json!({"requests.cpu":"1","limits.cpu":"1","requests.memory":"512Mi","limits.memory":"512Mi"});
        let used = json!({"requests.cpu":"100m","limits.cpu":"500m","requests.memory":"64Mi","limits.memory":"256Mi"});
        objects.insert(format!("/api/v1/namespaces/{namespace}/resourcequotas/synthetic-budget"),
            json!({"metadata":metadata("synthetic-budget"),"spec":{"hard":hard},"status":{"hard":hard,"used":used}}));
        Ok(())
    }

    struct AdmissionServer {
        base_uri: Url,
        ca_pem: String,
        binding: Arc<Mutex<Option<AuthoringPublicationAdmissionBinding>>>,
        task: tokio::task::JoinHandle<()>,
    }

    impl AdmissionServer {
        async fn start() -> Result<Self, Box<dyn Error>> {
            let ca = test_ca()?;
            let (certificate_pem, private_key_pem) = leaf_certificate(&ca)?;
            let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
            let port = listener.local_addr()?.port();
            let base_uri = Url::parse(&format!("https://localhost:{port}/"))?;
            let binding = Arc::new(Mutex::new(None));
            let router = Router::new()
                .fallback(any(admission_request))
                .with_state(binding.clone());
            let tls = tls_config(&certificate_pem, &private_key_pem)?;
            let task = tokio::spawn(serve_tls(listener, router, tls));
            Ok(Self {
                base_uri,
                ca_pem: ca.pem(),
                binding,
                task,
            })
        }

        async fn stop(self) {
            self.task.abort();
            let _ = self.task.await;
        }
    }

    async fn admission_request(
        State(state): State<Arc<Mutex<Option<AuthoringPublicationAdmissionBinding>>>>,
        request: Request<Body>,
    ) -> Response {
        if request.method() == Method::GET
            && request
                .uri()
                .path()
                .starts_with("/internal/v1/environment-releases/")
            && let Ok(guard) = state.lock()
            && let Some(binding) = guard.clone()
        {
            return (StatusCode::OK, Json(Some(binding))).into_response();
        }
        StatusCode::NOT_FOUND.into_response()
    }

    fn build_authoring_client(
        server: &AdmissionServer,
        token_client: Arc<ServiceTokenClient>,
        temporary: &TempDir,
        binding: AuthoringPublicationAdmissionBinding,
    ) -> Result<AuthoringAdmissionClient, Box<dyn Error>> {
        *server
            .binding
            .lock()
            .map_err(|_| std::io::Error::other("admission state lock poisoned"))? = Some(binding);
        let ca = Certificate::from_pem(server.ca_pem.as_bytes())?;
        let client = Client::builder()
            .no_proxy()
            .add_root_certificate(ca)
            .build()?;
        let ca_file = temporary.path().join("admission-ca.pem");
        fs::write(&ca_file, &server.ca_pem)?;
        let client = AuthoringAdmissionClient::new(
            AuthoringAdmissionClientConfiguration {
                base_uri: server.base_uri.clone(),
                ca_file,
                timeout_milliseconds: 2_000,
                max_request_bytes: 64 * 1024,
                max_response_bytes: 64 * 1024,
                audience: "control".to_owned(),
            },
            client,
            token_client,
            BTreeSet::from([
                "control.authoring.read".to_owned(),
                "control.llm_policy.read".to_owned(),
            ]),
        )?;
        Ok(client)
    }

    async fn serve_tls(listener: TcpListener, router: Router, config: Arc<ServerConfig>) {
        let acceptor = TlsAcceptor::from(config);
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let acceptor = acceptor.clone();
            let router = router.clone();
            tokio::spawn(async move {
                let Ok(stream) = acceptor.accept(stream).await else {
                    return;
                };
                let service = TowerToHyperService::new(router);
                let connection = Builder::new(TokioExecutor::new())
                    .serve_connection_with_upgrades(TokioIo::new(stream), service)
                    .into_owned();
                let _ = connection.await;
            });
        }
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

    fn tls_config(
        certificate_pem: &str,
        private_key_pem: &str,
    ) -> Result<Arc<ServerConfig>, Box<dyn Error>> {
        let certificates = rustls_pemfile::certs(&mut Cursor::new(certificate_pem.as_bytes()))
            .collect::<Result<Vec<_>, _>>()?;
        let key: PrivateKeyDer<'static> =
            rustls_pemfile::private_key(&mut Cursor::new(private_key_pem.as_bytes()))?
                .ok_or("private key missing")?;
        let config = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certificates, key)?;
        Ok(Arc::new(config))
    }
}
