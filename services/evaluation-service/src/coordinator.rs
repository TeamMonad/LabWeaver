//! Fixed-operation Kubernetes coordinator for immutable submission freeze Jobs.
#![allow(
    missing_docs,
    clippy::missing_errors_doc,
    reason = "the reviewed configuration and stable diagnostics define this internal boundary"
)]

use std::{collections::BTreeMap, fs, path::PathBuf, time::Duration};

use base64::{Engine, engine::general_purpose::STANDARD};
use contracts::{
    PolicyId, RetentionClass, RetentionDisposition, RetentionSnapshot, Revision, UtcTimestamp,
    submission::{
        EnvironmentFreezeBinding, EnvironmentFreezeBindingRequest, EnvironmentFreezeSourceBinding,
    },
};
use rand::random;
use reqwest::{Certificate, Client, Identity, Method, StatusCode, Url};
use russh::keys::ssh_key::{LineEnding, PrivateKey, private::Ed25519Keypair};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    FreezeCommandStoreError, FreezeRequest, PgFreezeCommandStore, SubmissionFreezeCommand,
};

const WORKER_IMAGE_PULL_SECRET_NAME: &str = "harbor-labweaver-system-pull";

const FIELD_MANAGER: &str = "labweaver-freeze-coordinator";
const MAX_BOUND_FILE_BYTES: u64 = 1024 * 1024;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FreezeCoordinatorConfiguration {
    pub kubernetes_api_server: Url,
    pub kubernetes_bearer_token_file: PathBuf,
    pub kubernetes_ca_file: PathBuf,
    pub environment_service_base_uri: Url,
    pub environment_ca_file: PathBuf,
    pub environment_client_certificate_file: PathBuf,
    pub environment_client_private_key_file: PathBuf,
    pub allowed_environment_server_sans: Vec<String>,
    pub worker_image: String,
    pub worker_service_account_name: String,
    pub vm_job_namespace: String,
    pub worker_configuration_file: PathBuf,
    pub worker_secret_files: BTreeMap<String, PathBuf>,
    pub worker_tls_ca_file: PathBuf,
    pub infrastructure_namespace_labels: BTreeMap<String, String>,
    pub dns_namespace_labels: BTreeMap<String, String>,
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
    kubernetes_token: String,
    worker_configuration: String,
    worker_secrets: BTreeMap<String, Vec<u8>>,
}

impl FreezeCoordinator {
    pub fn new(
        configuration: FreezeCoordinatorConfiguration,
        store: PgFreezeCommandStore,
    ) -> Result<Self, FreezeCoordinatorError> {
        validate_configuration(&configuration)?;
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
        let mut identity = read_bound_file(&configuration.environment_client_certificate_file)?;
        identity.push(b'\n');
        identity.extend(read_bound_file(
            &configuration.environment_client_private_key_file,
        )?);
        let mut environment_builder = Client::builder()
            .https_only(true)
            .tls_built_in_root_certs(false)
            .redirect(reqwest::redirect::Policy::none())
            .identity(Identity::from_pem(&identity)?)
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
        let worker_secrets = configuration
            .worker_secret_files
            .iter()
            .map(|(name, path)| {
                validate_key(name)?;
                Ok((name.clone(), read_bound_file(path)?))
            })
            .collect::<Result<_, FreezeCoordinatorError>>()?;
        Ok(Self {
            configuration,
            store,
            kubernetes,
            environment,
            kubernetes_token,
            worker_configuration,
            worker_secrets,
        })
    }

    /// Claims at most one command and reconciles every bounded in-flight Job.
    pub async fn reconcile_once(&self) -> Result<(), FreezeCoordinatorError> {
        let _ = self.store.claim_next().await?;
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
                    diagnostic = error.diagnostic_code(),
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
                            diagnostic = cleanup_error.diagnostic_code(),
                            retry = true,
                        );
                    }
                }
            }
        }
        Ok(())
    }

    async fn fail_command_after_cleanup(
        &self,
        command: &SubmissionFreezeCommand,
        diagnostic: &'static str,
    ) -> Result<(), FreezeCoordinatorError> {
        let job_name = job_name(command);
        let container_namespace = format!("lw-env-{}", command.environment_id);
        for namespace in [&container_namespace, &self.configuration.vm_job_namespace] {
            if !self.cleanup(namespace, &job_name).await? {
                tracing::warn!(
                    event = "evaluation.freeze.cleanup.pending",
                    frozen_submission_id = %command.frozen_submission_id,
                    environment_id = %command.environment_id,
                    namespace,
                );
                return Ok(());
            }
        }
        self.store
            .mark_failed(command.frozen_submission_id, diagnostic)
            .await?;
        tracing::error!(
            event = "evaluation.freeze.failed",
            frozen_submission_id = %command.frozen_submission_id,
            environment_id = %command.environment_id,
            diagnostic,
            cleanup_verified = true,
        );
        Ok(())
    }

    async fn reconcile(
        &self,
        command: &SubmissionFreezeCommand,
    ) -> Result<(), FreezeCoordinatorError> {
        let job_name = job_name(command);
        let namespace = self.job_namespace(command).await?;
        let job = self.get(&namespace, "batch/v1", "jobs", &job_name).await?;
        if let Some(job) = job {
            let succeeded = job.pointer("/status/succeeded").and_then(Value::as_u64) == Some(1);
            let failed = job
                .pointer("/status/failed")
                .and_then(Value::as_u64)
                .unwrap_or(0)
                > 0;
            if (succeeded || failed) && self.cleanup(&namespace, &job_name).await? {
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
                    Err(FreezeCommandStoreError::ResultMissing) if failed => {
                        self.store
                            .mark_failed(command.frozen_submission_id, "LW_COLLECT_JOB_FAILED")
                            .await?;
                    }
                    Err(error) => return Err(error.into()),
                }
            }
            return Ok(());
        }
        self.create_resources(command, &namespace, &job_name).await
    }

    async fn job_namespace(
        &self,
        command: &SubmissionFreezeCommand,
    ) -> Result<String, FreezeCoordinatorError> {
        let probe_key = PrivateKey::from(Ed25519Keypair::from_seed(&random::<[u8; 32]>()));
        let binding = self
            .binding(command, Some(probe_key.public_key().to_openssh()?))
            .await?;
        Ok(match binding.source {
            EnvironmentFreezeSourceBinding::Container { namespace, .. } => namespace,
            EnvironmentFreezeSourceBinding::VirtualMachine { .. } => {
                self.configuration.vm_job_namespace.clone()
            }
        })
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
        let existing_secret = self.get(namespace, "v1", "secrets", job_name).await?;
        let existing_bundle = match (existing_config.as_ref(), existing_secret.as_ref()) {
            (Some(config), Some(secret)) => {
                verify_owned(config, command)?;
                verify_owned(secret, command)?;
                true
            }
            (None, None) => false,
            _ => {
                let _ = self.cleanup(namespace, job_name).await?;
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
        let now = self.store.authority_now().await?;
        let request = FreezeRequest {
            frozen_submission_id: command.frozen_submission_id,
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
                retain_until: UtcTimestamp::from_utc(
                    now.get() + time::Duration::days(self.configuration.retention_days),
                )
                .map_err(|_| FreezeCoordinatorError::BindingInvalid)?,
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
                        "sourceIdentity": contracts::Sha256Digest::of_canonical(&binding.environment)
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
        let job = json!({
            "apiVersion":"batch/v1","kind":"Job","metadata":{"name":job_name,"namespace":namespace,"labels":labels},
            "spec":{"backoffLimit":0,"activeDeadlineSeconds":self.configuration.job_active_deadline_seconds,
                "template":{"metadata":{"labels":labels},"spec":{"restartPolicy":"Never","serviceAccountName":self.configuration.worker_service_account_name,
                    "automountServiceAccountToken":false,"securityContext":{"runAsNonRoot":true,"runAsUser":65532,"runAsGroup":65532,"fsGroup":65532,"seccompProfile":{"type":"RuntimeDefault"}},
                    "containers":[{"name":"freeze","image":self.configuration.worker_image,"imagePullPolicy":"IfNotPresent",
                        "args":["--mode","freeze-worker"],"env":[
                            {"name":"LABWEAVER_EVALUATION_CONFIG_FILE","value":"/etc/labweaver/worker/worker.yaml"},
                            {"name":"LABWEAVER_FREEZE_COMMAND_FILE","value":"/etc/labweaver/worker/command.json"},
                            {"name":"SSL_CERT_FILE","value":self.configuration.worker_tls_ca_file}],
                        "securityContext":{"allowPrivilegeEscalation":false,"readOnlyRootFilesystem":true,"capabilities":{"drop":["ALL"]}},
                        "resources":{"requests":{"cpu":"100m","memory":"128Mi"},"limits":{"cpu":"1","memory":"1Gi"}},"volumeMounts":mounts}],"volumes":volumes}}}
        });
        self.apply(namespace, "batch/v1", "jobs", job_name, job)
            .await
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
            .json(&EnvironmentFreezeBindingRequest {
                course_id: command.course_id,
                actor_id: command.actor_id,
                expected_revision: command.environment_revision,
                collector_public_key_openssh,
            })
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(FreezeCoordinatorError::BindingUnavailable);
        }
        let bytes = response.bytes().await?;
        contracts::parse_strict_json(&bytes).map_err(|_| FreezeCoordinatorError::BindingInvalid)
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

    async fn cleanup(&self, namespace: &str, name: &str) -> Result<bool, FreezeCoordinatorError> {
        for (api_version, plural) in [
            ("batch/v1", "jobs"),
            ("v1", "configmaps"),
            ("v1", "secrets"),
            ("networking.k8s.io/v1", "networkpolicies"),
        ] {
            let response = self
                .authorized(self.kubernetes.delete(self.resource_url(
                    namespace,
                    api_version,
                    plural,
                    name,
                )?))
                .json(&json!({"propagationPolicy":"Foreground"}))
                .send()
                .await?;
            if !(response.status().is_success() || response.status() == StatusCode::NOT_FOUND) {
                return Err(FreezeCoordinatorError::KubernetesRejected);
            }
        }
        for (api_version, plural) in [
            ("batch/v1", "jobs"),
            ("v1", "configmaps"),
            ("v1", "secrets"),
            ("networking.k8s.io/v1", "networkpolicies"),
        ] {
            if let Some(resource) = self.get(namespace, api_version, plural, name).await? {
                // A foreground Job deletion remains observable until its Pods have terminated.
                // Once deletionTimestamp is set, the API server has accepted the destructive
                // request and Kubernetes owns the remaining garbage collection. Treating that
                // state as pending caused the coordinator to recreate the Job after it vanished,
                // leaving a terminal command in an endless create/delete loop.
                let deleting = api_version == "batch/v1"
                    && plural == "jobs"
                    && resource
                        .pointer("/metadata/deletionTimestamp")
                        .and_then(Value::as_str)
                        .is_some();
                if !deleting {
                    return Ok(false);
                }
            }
        }
        Ok(true)
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
}

fn validate_configuration(
    configuration: &FreezeCoordinatorConfiguration,
) -> Result<(), FreezeCoordinatorError> {
    let environment_host = configuration.environment_service_base_uri.host_str();
    if configuration.kubernetes_api_server.scheme() != "https"
        || configuration.environment_service_base_uri.scheme() != "https"
        || configuration.environment_service_base_uri.path() != "/"
        || environment_host.is_none()
        || !configuration
            .allowed_environment_server_sans
            .iter()
            .any(|value| Some(value.as_str()) == environment_host)
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
        || configuration.worker_secret_files.is_empty()
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
    if value.is_empty()
        || value.len() > 63
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'.')
        })
        || value.starts_with(['-', '.'])
        || value.ends_with(['-', '.'])
    {
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

#[derive(Debug, thiserror::Error)]
pub enum FreezeCoordinatorError {
    #[error("LW_COLLECT_COORDINATOR_CONFIG_INVALID")]
    ConfigurationInvalid,
    #[error("LW_COLLECT_COORDINATOR_CERTIFICATE_INVALID")]
    CertificateInvalid,
    #[error("LW_COLLECT_BINDING_INVALID")]
    BindingInvalid,
    #[error("LW_COLLECT_BINDING_UNAVAILABLE")]
    BindingUnavailable,
    #[error("LW_COLLECT_KUBERNETES_REJECTED")]
    KubernetesRejected,
    #[error("LW_COLLECT_IO_FAILED")]
    Io(#[from] std::io::Error),
    #[error("LW_COLLECT_HTTP_FAILED")]
    Http(#[from] reqwest::Error),
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
        matches!(self, Self::BindingInvalid | Self::Json(_) | Self::Ssh(_))
    }

    const fn diagnostic_code(&self) -> &'static str {
        match self {
            Self::ConfigurationInvalid => "LW_COLLECT_COORDINATOR_CONFIG_INVALID",
            Self::CertificateInvalid => "LW_COLLECT_COORDINATOR_CERTIFICATE_INVALID",
            Self::BindingInvalid => "LW_COLLECT_BINDING_INVALID",
            Self::BindingUnavailable => "LW_COLLECT_BINDING_UNAVAILABLE",
            Self::KubernetesRejected => "LW_COLLECT_KUBERNETES_REJECTED",
            Self::Io(_) => "LW_COLLECT_IO_FAILED",
            Self::Http(_) => "LW_COLLECT_HTTP_FAILED",
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

#[cfg(test)]
mod tests {
    use super::{FreezeCoordinatorError, command_deadline_exceeded};
    use contracts::UtcTimestamp;

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
    fn invalid_binding_is_a_terminal_command_error() {
        let error = FreezeCoordinatorError::BindingInvalid;
        assert!(!error.is_systemic());
        assert!(error.is_terminal_command_error());
        assert_eq!(error.diagnostic_code(), "LW_COLLECT_BINDING_INVALID");
    }
}
