//! Restricted Kubernetes API backend for the deployment-owned runtime executor.

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use artifact_store::{ImmutableObjectStore, S3ImmutableObjectStore};
use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use contracts::{ArtifactRef, Revision, Sha256Digest, UtcTimestamp};
use reqwest::{Certificate, Client, Method, StatusCode, Url};
use serde::Deserialize;
use serde_json::{Value, json};
use time::OffsetDateTime;
use tokio::sync::Mutex;

use crate::{
    ContainerApplyObservation, ContainerBackendFence, ContainerExecutorBackend,
    ContainerExecutorRequest, ContainerExecutorResponse, ContainerResource, ContainerResourcePlan,
    KubeVirtBackendFence, KubeVirtCleanupPlan, KubeVirtExecutorBackend, KubeVirtExecutorRequest,
    KubeVirtExecutorResponse, KubeVirtResource, KubeVirtResourcePlan, KubeVirtRunningObservation,
    KubeVirtStoppedObservation, ProviderFailure, ProviderFailureCode,
};

const FIELD_MANAGER: &str = "labweaver-runtime-executor";
const CLEANUP_MEDIA_TYPE: &str = "application/vnd.labweaver.environment-cleanup+json";

fn valid_dns_label(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 63
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && value
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && value
            .as_bytes()
            .last()
            .is_some_and(u8::is_ascii_alphanumeric)
}

/// Reviewed, non-secret Kubernetes executor configuration.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeExecutorConfiguration {
    pub api_server: Url,
    pub bearer_token_file: PathBuf,
    pub cluster_ca_file: PathBuf,
    pub request_timeout_milliseconds: u64,
    pub cleanup_poll_milliseconds: u64,
    pub cleanup_retention_seconds: u64,
    pub ssh_handshake_timeout_milliseconds: u64,
    pub registry_pull_secret_file: PathBuf,
    pub registry_pull_secret_name: String,
}

impl RuntimeExecutorConfiguration {
    fn validate(&self) -> Result<(), ProviderFailure> {
        if self.api_server.scheme() != "https"
            || self.api_server.host_str().is_none()
            || !self.bearer_token_file.is_absolute()
            || !self.cluster_ca_file.is_absolute()
            || self.request_timeout_milliseconds == 0
            || self.request_timeout_milliseconds > 30_000
            || self.cleanup_poll_milliseconds == 0
            || self.cleanup_poll_milliseconds > 5_000
            || self.cleanup_retention_seconds < 3_600
            || self.cleanup_retention_seconds > 31_536_000
            || self.ssh_handshake_timeout_milliseconds == 0
            || self.ssh_handshake_timeout_milliseconds > 10_000
            || !self.registry_pull_secret_file.is_absolute()
            || !valid_dns_label(&self.registry_pull_secret_name)
        {
            return Err(rejected());
        }
        Ok(())
    }
}

/// Fixed-operation Kubernetes backend. No command string or `kubectl` process is accepted.
#[derive(Clone)]
pub struct KubernetesContainerExecutor {
    configuration: RuntimeExecutorConfiguration,
    client: Client,
    token: String,
    objects: Arc<S3ImmutableObjectStore>,
}

impl KubernetesContainerExecutor {
    pub fn new(
        configuration: RuntimeExecutorConfiguration,
        objects: Arc<S3ImmutableObjectStore>,
    ) -> Result<Self, ProviderFailure> {
        configuration.validate()?;
        let token = read_secret(&configuration.bearer_token_file)?;
        let ca = Certificate::from_pem(
            &std::fs::read(&configuration.cluster_ca_file).map_err(|_| rejected())?,
        )
        .map_err(|_| rejected())?;
        let client = Client::builder()
            .https_only(true)
            .add_root_certificate(ca)
            .timeout(Duration::from_millis(
                configuration.request_timeout_milliseconds,
            ))
            .build()
            .map_err(|_| rejected())?;
        Ok(Self {
            configuration,
            client,
            token,
            objects,
        })
    }

    async fn apply_plan(
        &self,
        plan: &ContainerResourcePlan,
    ) -> Result<ContainerApplyObservation, ProviderFailure> {
        validate_plan(plan)?;
        let namespace = plan
            .resources
            .iter()
            .find(|resource| resource.kind == "Namespace")
            .ok_or_else(rejected)?;
        self.apply_resource(plan, namespace).await?;
        self.ensure_registry_pull_secret(plan).await?;
        for resource in plan
            .resources
            .iter()
            .filter(|resource| resource.kind != "Namespace")
        {
            self.apply_resource(plan, resource).await?;
        }
        self.observe_plan(plan).await
    }

    async fn ensure_registry_pull_secret(
        &self,
        plan: &ContainerResourcePlan,
    ) -> Result<(), ProviderFailure> {
        let docker_config =
            validated_registry_pull_config(&self.configuration.registry_pull_secret_file)?;
        let secret = ContainerResource {
            kind: "Secret".to_owned(),
            namespace: Some(plan.namespace.clone()),
            name: self.configuration.registry_pull_secret_name.clone(),
            document: json!({
                "apiVersion": "v1",
                "kind": "Secret",
                "metadata": {
                    "name": self.configuration.registry_pull_secret_name,
                    "namespace": plan.namespace,
                    "labels": {
                        "app.kubernetes.io/name": "labweaver-environment",
                        "labweaver.io/environment-id": plan.environment_id.to_string(),
                        "labweaver.io/managed": "true"
                    }
                },
                "type": "kubernetes.io/dockerconfigjson",
                "data": {".dockerconfigjson": BASE64_STANDARD.encode(docker_config)}
            }),
        };
        self.apply_resource(plan, &secret).await
    }

    async fn apply_resource(
        &self,
        plan: &ContainerResourcePlan,
        resource: &ContainerResource,
    ) -> Result<(), ProviderFailure> {
        validate_resource(plan, resource)?;
        let url = self.resource_url(resource)?;
        let response = self
            .authorized(
                self.client
                    .request(Method::PATCH, url)
                    .query(&[("fieldManager", FIELD_MANAGER), ("force", "false")])
                    .header("content-type", "application/apply-patch+yaml")
                    .body(serde_json::to_vec(&resource.document).map_err(|_| rejected())?),
            )
            .send()
            .await
            .map_err(|_| unavailable())?;
        accept_mutation(response.status())
    }

    async fn observe_plan(
        &self,
        plan: &ContainerResourcePlan,
    ) -> Result<ContainerApplyObservation, ProviderFailure> {
        validate_plan(plan)?;
        let deployment = plan
            .resources
            .iter()
            .find(|resource| resource.kind == "Deployment")
            .ok_or_else(rejected)?;
        let response = self
            .authorized(self.client.get(self.resource_url(deployment)?))
            .send()
            .await
            .map_err(|_| unavailable())?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(ContainerApplyObservation {
                ready: false,
                observed_at: timestamp()?,
            });
        }
        if !response.status().is_success() {
            return Err(status_failure(response.status()));
        }
        let value: Value = response.json().await.map_err(|_| invalid_observation())?;
        let generation = pointer_u64(&value, "/metadata/generation")?;
        let observed_generation = pointer_u64(&value, "/status/observedGeneration")?;
        let desired = pointer_u64(&value, "/spec/replicas")?;
        let available = pointer_u64_or_zero(&value, "/status/availableReplicas")?;
        let unavailable = pointer_u64_or_zero(&value, "/status/unavailableReplicas")?;
        Ok(ContainerApplyObservation {
            ready: observed_generation >= generation && available == desired && unavailable == 0,
            observed_at: timestamp()?,
        })
    }

    async fn scale(
        &self,
        fence: &ContainerBackendFence,
        plan: &ContainerResourcePlan,
        replicas: u32,
    ) -> Result<ContainerApplyObservation, ProviderFailure> {
        let mut deployment = plan
            .resources
            .iter()
            .find(|resource| resource.kind == "Deployment")
            .cloned()
            .ok_or_else(rejected)?;
        deployment.document["spec"]["replicas"] = json!(replicas);
        self.apply_resource(plan, &deployment).await?;
        loop {
            let observation = self.observe_plan(plan).await?;
            if observation.ready {
                return Ok(observation);
            }
            if timestamp()?.get() >= fence.deadline_at.get() {
                return Err(unavailable());
            }
            tokio::time::sleep(Duration::from_millis(
                self.configuration.cleanup_poll_milliseconds,
            ))
            .await;
        }
    }

    async fn restart(
        &self,
        plan: &ContainerResourcePlan,
        operation_revision: Revision,
    ) -> Result<ContainerApplyObservation, ProviderFailure> {
        let mut deployment = plan
            .resources
            .iter()
            .find(|resource| resource.kind == "Deployment")
            .cloned()
            .ok_or_else(rejected)?;
        let annotations = deployment
            .document
            .pointer_mut("/spec/template/metadata")
            .and_then(Value::as_object_mut)
            .ok_or_else(rejected)?
            .entry("annotations")
            .or_insert_with(|| json!({}));
        annotations.as_object_mut().ok_or_else(rejected)?.insert(
            "labweaver.io/restart-revision".to_owned(),
            json!(operation_revision.get().to_string()),
        );
        self.apply_resource(plan, &deployment).await?;
        self.observe_plan(plan).await
    }

    async fn delete_namespace(
        &self,
        fence: &ContainerBackendFence,
        plan: &ContainerResourcePlan,
    ) -> Result<ArtifactRef, ProviderFailure> {
        validate_plan(plan)?;
        let namespace_url =
            self.namespaced_url(&format!("/api/v1/namespaces/{}", plan.namespace))?;
        let observed = self
            .authorized(self.client.get(namespace_url.clone()))
            .send()
            .await
            .map_err(|_| unavailable())?;
        if observed.status() != StatusCode::NOT_FOUND {
            if !observed.status().is_success() {
                return Err(status_failure(observed.status()));
            }
            let namespace: Value = observed.json().await.map_err(|_| invalid_observation())?;
            verify_namespace_identity(&namespace, &plan.namespace, plan.environment_id)?;
            let patch = self
                .authorized(
                    self.client
                        .patch(namespace_url.clone())
                        .header("content-type", "application/merge-patch+json")
                        .json(&json!({"metadata":{"finalizers":[]}})),
                )
                .send()
                .await
                .map_err(|_| unavailable())?;
            accept_mutation(patch.status())?;
            let deletion = self
                .authorized(self.client.delete(namespace_url.clone()))
                .send()
                .await
                .map_err(|_| unavailable())?;
            if deletion.status() != StatusCode::NOT_FOUND {
                accept_mutation(deletion.status())?;
            }
        }
        loop {
            if timestamp()?.get() >= fence.deadline_at.get() {
                return Err(unavailable());
            }
            let readback = self
                .authorized(self.client.get(namespace_url.clone()))
                .send()
                .await
                .map_err(|_| unavailable())?;
            if readback.status() == StatusCode::NOT_FOUND {
                break;
            }
            if !readback.status().is_success() {
                return Err(status_failure(readback.status()));
            }
            tokio::time::sleep(Duration::from_millis(
                self.configuration.cleanup_poll_milliseconds,
            ))
            .await;
        }
        self.write_cleanup_evidence(fence, plan).await
    }

    async fn write_cleanup_evidence(
        &self,
        fence: &ContainerBackendFence,
        plan: &ContainerResourcePlan,
    ) -> Result<ArtifactRef, ProviderFailure> {
        let now = timestamp()?;
        let document = json!({
            "schemaVersion":"environment-cleanup.v1",
            "environmentId":plan.environment_id,
            "namespace":plan.namespace,
            "operationId":fence.operation_id,
            "operationGeneration":fence.operation_generation,
            "requestId":fence.request_id,
            "planSha256":plan.plan_sha256,
            "namespaceAbsent":true,
            "observedAt":now,
        });
        self.store_cleanup_document(plan.environment_id, fence.request_id, now, document)
            .await
    }

    async fn apply_kubevirt_plan(
        &self,
        plan: &KubeVirtResourcePlan,
    ) -> Result<(), ProviderFailure> {
        validate_kubevirt_plan(plan)?;
        for resource in &plan.resources {
            validate_kubevirt_resource(plan, resource)?;
            let url = self.kubevirt_resource_url(resource)?;
            let response = self
                .authorized(
                    self.client
                        .request(Method::PATCH, url)
                        .query(&[("fieldManager", FIELD_MANAGER), ("force", "false")])
                        .header("content-type", "application/apply-patch+yaml")
                        .body(serde_json::to_vec(&resource.document).map_err(|_| rejected())?),
                )
                .send()
                .await
                .map_err(|_| unavailable())?;
            accept_mutation(response.status())?;
        }
        Ok(())
    }

    async fn observe_kubevirt_running(
        &self,
        fence: &KubeVirtBackendFence,
        plan: &KubeVirtResourcePlan,
    ) -> Result<KubeVirtRunningObservation, ProviderFailure> {
        validate_kubevirt_plan(plan)?;
        let vm = self
            .get_json(
                "VirtualMachine",
                &plan.namespace,
                &plan.virtual_machine_name,
            )
            .await?
            .ok_or_else(unavailable)?;
        let vmi = self
            .get_json(
                "VirtualMachineInstance",
                &plan.namespace,
                &plan.virtual_machine_name,
            )
            .await?
            .ok_or_else(unavailable)?;
        let pvc = self
            .get_json(
                "PersistentVolumeClaim",
                &plan.namespace,
                &plan.data_volume_name,
            )
            .await?
            .ok_or_else(unavailable)?;
        let service = self
            .get_json("Service", &plan.namespace, "ssh")
            .await?
            .ok_or_else(unavailable)?;
        if vmi.pointer("/status/phase").and_then(Value::as_str) != Some("Running") {
            return Err(unavailable());
        }
        let guest_ip = vmi
            .pointer("/status/interfaces/0/ipAddress")
            .and_then(Value::as_str)
            .ok_or_else(unavailable)?
            .parse::<IpAddr>()
            .map_err(|_| invalid_observation())?;
        let service_cluster_ip = service
            .pointer("/spec/clusterIP")
            .and_then(Value::as_str)
            .ok_or_else(unavailable)?
            .parse::<IpAddr>()
            .map_err(|_| invalid_observation())?;
        let conditions = vmi
            .pointer("/status/conditions")
            .and_then(Value::as_array)
            .ok_or_else(unavailable)?;
        let condition_true = |kind: &str| {
            conditions.iter().any(|condition| {
                condition.get("type").and_then(Value::as_str) == Some(kind)
                    && condition.get("status").and_then(Value::as_str) == Some("True")
            })
        };
        if !condition_true("Ready") || !condition_true("AgentConnected") {
            return Err(unavailable());
        }
        let ssh_host_key_sha256 = self.probe_ssh_host_key(service_cluster_ip).await?;
        Ok(KubeVirtRunningObservation {
            observed_environment_generation: fence.environment_generation,
            vm_resource_generation: pointer_u64(&vm, "/metadata/generation")?,
            observed_vm_resource_generation: pointer_u64(&vm, "/status/observedGeneration")?,
            vm_uid: pointer_uuid(&vm, "/metadata/uid")?,
            vmi_uid: pointer_uuid(&vmi, "/metadata/uid")?,
            root_disk_uid: pointer_uuid(&pvc, "/metadata/uid")?,
            guest_ip,
            service_cluster_ip,
            ssh_host_key_sha256,
            guest_agent_connected: true,
            ssh_ready: true,
            observed_at: timestamp()?,
        })
    }

    async fn observe_kubevirt_stopped(
        &self,
        fence: &KubeVirtBackendFence,
        plan: &KubeVirtResourcePlan,
    ) -> Result<KubeVirtStoppedObservation, ProviderFailure> {
        let vm = self
            .get_json(
                "VirtualMachine",
                &plan.namespace,
                &plan.virtual_machine_name,
            )
            .await?
            .ok_or_else(unavailable)?;
        let pvc = self
            .get_json(
                "PersistentVolumeClaim",
                &plan.namespace,
                &plan.data_volume_name,
            )
            .await?
            .ok_or_else(unavailable)?;
        let vmi_absent = self
            .get_json(
                "VirtualMachineInstance",
                &plan.namespace,
                &plan.virtual_machine_name,
            )
            .await?
            .is_none();
        if !vmi_absent {
            return Err(unavailable());
        }
        Ok(KubeVirtStoppedObservation {
            observed_environment_generation: fence.environment_generation,
            vm_uid: pointer_uuid(&vm, "/metadata/uid")?,
            root_disk_uid: pointer_uuid(&pvc, "/metadata/uid")?,
            vmi_absent,
            observed_at: timestamp()?,
        })
    }

    async fn kubevirt_subresource(
        &self,
        fence: &KubeVirtBackendFence,
        plan: &KubeVirtResourcePlan,
        action: &str,
    ) -> Result<(), ProviderFailure> {
        if !matches!(action, "start" | "stop" | "restart") {
            return Err(rejected());
        }
        validate_kubevirt_plan(plan)?;
        let url = self.namespaced_url(&format!(
            "/apis/subresources.kubevirt.io/v1/namespaces/{}/virtualmachines/{}/{}",
            plan.namespace, plan.virtual_machine_name, action
        ))?;
        let response = self
            .authorized(
                self.client
                    .put(url)
                    .header("content-type", "application/json")
                    .json(&json!({"gracePeriod":0})),
            )
            .send()
            .await
            .map_err(|_| unavailable())?;
        if response.status() == StatusCode::CONFLICT && action == "start" {
            return Ok(());
        }
        accept_mutation(response.status())?;
        if timestamp()?.get() >= fence.deadline_at.get() {
            return Err(unavailable());
        }
        Ok(())
    }

    async fn delete_kubevirt_namespace(
        &self,
        fence: &KubeVirtBackendFence,
        plan: &KubeVirtCleanupPlan,
    ) -> Result<ArtifactRef, ProviderFailure> {
        let expected_namespace = format!("lw-env-{}", plan.environment_id);
        if plan.namespace != expected_namespace || plan.virtual_machine_name != "runtime" {
            return Err(rejected());
        }
        let namespace_url =
            self.namespaced_url(&format!("/api/v1/namespaces/{}", plan.namespace))?;
        let observed = self
            .authorized(self.client.get(namespace_url.clone()))
            .send()
            .await
            .map_err(|_| unavailable())?;
        if observed.status() != StatusCode::NOT_FOUND {
            if !observed.status().is_success() {
                return Err(status_failure(observed.status()));
            }
            let namespace: Value = observed.json().await.map_err(|_| invalid_observation())?;
            verify_namespace_identity(&namespace, &plan.namespace, plan.environment_id)?;
            let patch = self
                .authorized(
                    self.client
                        .patch(namespace_url.clone())
                        .header("content-type", "application/merge-patch+json")
                        .json(&json!({"metadata":{"finalizers":[]}})),
                )
                .send()
                .await
                .map_err(|_| unavailable())?;
            accept_mutation(patch.status())?;
            let deletion = self
                .authorized(self.client.delete(namespace_url.clone()))
                .send()
                .await
                .map_err(|_| unavailable())?;
            if deletion.status() != StatusCode::NOT_FOUND {
                accept_mutation(deletion.status())?;
            }
        }
        loop {
            if timestamp()?.get() >= fence.deadline_at.get() {
                return Err(unavailable());
            }
            let readback = self
                .authorized(self.client.get(namespace_url.clone()))
                .send()
                .await
                .map_err(|_| unavailable())?;
            if readback.status() == StatusCode::NOT_FOUND {
                break;
            }
            if !readback.status().is_success() {
                return Err(status_failure(readback.status()));
            }
            tokio::time::sleep(Duration::from_millis(
                self.configuration.cleanup_poll_milliseconds,
            ))
            .await;
        }
        let now = timestamp()?;
        let document = json!({
            "schemaVersion":"environment-cleanup.v1",
            "runtimeKind":"virtual_machine",
            "environmentId":plan.environment_id,
            "namespace":plan.namespace,
            "operationId":fence.operation_id,
            "environmentGeneration":fence.environment_generation,
            "requestId":fence.request_id,
            "planSha256":plan.plan_sha256,
            "namespaceAbsent":true,
            "observedAt":now,
        });
        self.store_cleanup_document(plan.environment_id, fence.request_id, now, document)
            .await
    }

    async fn store_cleanup_document(
        &self,
        environment_id: contracts::EnvironmentId,
        request_id: Sha256Digest,
        now: UtcTimestamp,
        document: Value,
    ) -> Result<ArtifactRef, ProviderFailure> {
        let bytes = serde_json::to_vec(&document).map_err(|_| invalid_observation())?;
        let sha256 = Sha256Digest::of_bytes(&bytes);
        let retain_until = UtcTimestamp::from_utc(
            now.get()
                + time::Duration::seconds(
                    i64::try_from(self.configuration.cleanup_retention_seconds)
                        .map_err(|_| rejected())?,
                ),
        )
        .map_err(|_| rejected())?;
        self.objects
            .put_governance_locked(
                &format!("cleanup/{environment_id}/{request_id}.json"),
                &bytes,
                sha256,
                CLEANUP_MEDIA_TYPE,
                now,
                retain_until,
            )
            .await
            .map(|object| object.reference)
            .map_err(|_| ProviderFailure {
                code: ProviderFailureCode::CleanupFailed,
                retryable: true,
            })
    }

    async fn get_json(
        &self,
        kind: &str,
        namespace: &str,
        name: &str,
    ) -> Result<Option<Value>, ProviderFailure> {
        let (prefix, plural, namespaced) = resource_path(kind)?;
        if !namespaced {
            return Err(rejected());
        }
        let url =
            self.namespaced_url(&format!("{prefix}/namespaces/{namespace}/{plural}/{name}"))?;
        let response = self
            .authorized(self.client.get(url))
            .send()
            .await
            .map_err(|_| unavailable())?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !response.status().is_success() {
            return Err(status_failure(response.status()));
        }
        response
            .json()
            .await
            .map(Some)
            .map_err(|_| invalid_observation())
    }

    async fn probe_ssh_host_key(&self, address: IpAddr) -> Result<Sha256Digest, ProviderFailure> {
        let observed = Arc::new(Mutex::new(None));
        let handler = HostKeyProbe {
            observed: Arc::clone(&observed),
        };
        let configuration = Arc::new(russh::client::Config::default());
        let connection = tokio::time::timeout(
            Duration::from_millis(self.configuration.ssh_handshake_timeout_milliseconds),
            russh::client::connect(configuration, SocketAddr::new(address, 22), handler),
        )
        .await
        .map_err(|_| unavailable())?
        .map_err(|_| unavailable())?;
        connection
            .disconnect(russh::Disconnect::ByApplication, "probe-complete", "en")
            .await
            .map_err(|_| unavailable())?;
        observed.lock().await.ok_or_else(unavailable)
    }

    fn kubevirt_resource_url(&self, resource: &KubeVirtResource) -> Result<Url, ProviderFailure> {
        let (prefix, plural, namespaced) = resource_path(&resource.kind)?;
        let path = if namespaced {
            let namespace = resource.namespace.as_deref().ok_or_else(rejected)?;
            format!("{prefix}/namespaces/{namespace}/{plural}/{}", resource.name)
        } else {
            if resource.namespace.is_some() {
                return Err(rejected());
            }
            format!("{prefix}/{plural}/{}", resource.name)
        };
        self.namespaced_url(&path)
    }

    fn resource_url(&self, resource: &ContainerResource) -> Result<Url, ProviderFailure> {
        let (prefix, plural, namespaced) = resource_path(&resource.kind)?;
        let path = if namespaced {
            let namespace = resource.namespace.as_deref().ok_or_else(rejected)?;
            format!("{prefix}/namespaces/{namespace}/{plural}/{}", resource.name)
        } else {
            if resource.namespace.is_some() {
                return Err(rejected());
            }
            format!("{prefix}/{plural}/{}", resource.name)
        };
        self.namespaced_url(&path)
    }

    fn namespaced_url(&self, path: &str) -> Result<Url, ProviderFailure> {
        self.configuration
            .api_server
            .join(path.trim_start_matches('/'))
            .map_err(|_| rejected())
    }

    fn authorized(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        request.bearer_auth(&self.token)
    }
}

#[async_trait]
impl ContainerExecutorBackend for KubernetesContainerExecutor {
    async fn execute(
        &self,
        fence: &ContainerBackendFence,
        request: &ContainerExecutorRequest,
    ) -> ContainerExecutorResponse {
        let result = match request {
            ContainerExecutorRequest::Apply { plan } => {
                self.apply_plan(plan)
                    .await
                    .map(|observation| ContainerExecutorResponse::Observed {
                        plan_sha256: plan.plan_sha256,
                        observation,
                    })
            }
            ContainerExecutorRequest::Observe { plan } => {
                self.observe_plan(plan).await.map(|observation| {
                    ContainerExecutorResponse::Observed {
                        plan_sha256: plan.plan_sha256,
                        observation,
                    }
                })
            }
            ContainerExecutorRequest::Scale { plan, replicas } => self
                .scale(fence, plan, *replicas)
                .await
                .map(|observation| ContainerExecutorResponse::Observed {
                    plan_sha256: plan.plan_sha256,
                    observation,
                }),
            ContainerExecutorRequest::Restart {
                plan,
                operation_revision,
            } => self
                .restart(plan, *operation_revision)
                .await
                .map(|observation| ContainerExecutorResponse::Observed {
                    plan_sha256: plan.plan_sha256,
                    observation,
                }),
            ContainerExecutorRequest::DeleteNamespace { plan } => self
                .delete_namespace(fence, plan)
                .await
                .map(|cleanup_evidence| ContainerExecutorResponse::Deleted {
                    plan_sha256: plan.plan_sha256,
                    cleanup_evidence,
                }),
        };
        result.unwrap_or_else(|failure| ContainerExecutorResponse::Failed { failure })
    }
}

#[derive(Clone)]
struct HostKeyProbe {
    observed: Arc<Mutex<Option<Sha256Digest>>>,
}

impl russh::client::Handler for HostKeyProbe {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &russh::keys::PublicKey,
    ) -> Result<bool, Self::Error> {
        let identity = server_public_key
            .fingerprint(russh::keys::HashAlg::Sha256)
            .to_string();
        *self.observed.lock().await = Some(Sha256Digest::of_bytes(identity.as_bytes()));
        Ok(true)
    }
}

#[async_trait]
impl KubeVirtExecutorBackend for KubernetesContainerExecutor {
    async fn execute(
        &self,
        fence: &KubeVirtBackendFence,
        request: &KubeVirtExecutorRequest,
    ) -> KubeVirtExecutorResponse {
        let result = match request {
            KubeVirtExecutorRequest::Apply { plan } => async {
                self.apply_kubevirt_plan(plan).await?;
                self.observe_kubevirt_running(fence, plan).await
            }
            .await
            .map(|observation| KubeVirtExecutorResponse::Running {
                plan_sha256: plan.plan_sha256,
                observation,
            }),
            KubeVirtExecutorRequest::Observe { plan } => self
                .observe_kubevirt_running(fence, plan)
                .await
                .map(|observation| KubeVirtExecutorResponse::Running {
                    plan_sha256: plan.plan_sha256,
                    observation,
                }),
            KubeVirtExecutorRequest::Start { plan } => async {
                self.kubevirt_subresource(fence, plan, "start").await?;
                self.observe_kubevirt_running(fence, plan).await
            }
            .await
            .map(|observation| KubeVirtExecutorResponse::Running {
                plan_sha256: plan.plan_sha256,
                observation,
            }),
            KubeVirtExecutorRequest::Stop { plan } => async {
                self.kubevirt_subresource(fence, plan, "stop").await?;
                loop {
                    match self.observe_kubevirt_stopped(fence, plan).await {
                        Ok(observation) => break Ok(observation),
                        Err(error)
                            if error.retryable && timestamp()?.get() < fence.deadline_at.get() =>
                        {
                            tokio::time::sleep(Duration::from_millis(
                                self.configuration.cleanup_poll_milliseconds,
                            ))
                            .await;
                        }
                        Err(error) => break Err(error),
                    }
                }
            }
            .await
            .map(|observation| KubeVirtExecutorResponse::Stopped {
                plan_sha256: plan.plan_sha256,
                observation,
            }),
            KubeVirtExecutorRequest::Restart { plan } => async {
                self.kubevirt_subresource(fence, plan, "restart").await?;
                self.observe_kubevirt_running(fence, plan).await
            }
            .await
            .map(|observation| KubeVirtExecutorResponse::Running {
                plan_sha256: plan.plan_sha256,
                observation,
            }),
            KubeVirtExecutorRequest::DeleteNamespace { plan } => self
                .delete_kubevirt_namespace(fence, plan)
                .await
                .map(|cleanup_evidence| KubeVirtExecutorResponse::Deleted {
                    plan_sha256: plan.plan_sha256,
                    cleanup_evidence,
                }),
        };
        result.unwrap_or_else(|failure| KubeVirtExecutorResponse::Failed { failure })
    }
}

fn validate_plan(plan: &ContainerResourcePlan) -> Result<(), ProviderFailure> {
    let expected_namespace = format!("lw-env-{}", plan.environment_id);
    if plan.namespace != expected_namespace
        || plan.resources.is_empty()
        || plan.resources.len() > 32
        || Sha256Digest::of_canonical(&json!({
            "environmentId": plan.environment_id,
            "resources": plan.resources,
        }))
        .is_err()
    {
        return Err(rejected());
    }
    Ok(())
}

fn validate_kubevirt_plan(plan: &KubeVirtResourcePlan) -> Result<(), ProviderFailure> {
    if plan.namespace != format!("lw-env-{}", plan.environment_id)
        || plan.virtual_machine_name != "runtime"
        || plan.data_volume_name != "rootdisk"
        || plan.resources.is_empty()
        || plan.resources.len() > 32
    {
        return Err(rejected());
    }
    Ok(())
}

fn validate_kubevirt_resource(
    plan: &KubeVirtResourcePlan,
    resource: &KubeVirtResource,
) -> Result<(), ProviderFailure> {
    let metadata = resource
        .document
        .get("metadata")
        .and_then(Value::as_object)
        .ok_or_else(rejected)?;
    if resource.document.get("kind").and_then(Value::as_str) != Some(&resource.kind)
        || metadata.get("name").and_then(Value::as_str) != Some(&resource.name)
        || resource
            .namespace
            .as_deref()
            .is_some_and(|namespace| namespace != plan.namespace)
        || metadata.get("namespace").and_then(Value::as_str) != resource.namespace.as_deref()
        || metadata
            .get("labels")
            .and_then(|labels| labels.get("labweaver.io/environment-id"))
            .and_then(Value::as_str)
            != Some(&plan.environment_id.to_string())
    {
        return Err(rejected());
    }
    resource_path(&resource.kind).map(|_| ())
}

fn validate_resource(
    plan: &ContainerResourcePlan,
    resource: &ContainerResource,
) -> Result<(), ProviderFailure> {
    let metadata = resource
        .document
        .get("metadata")
        .and_then(Value::as_object)
        .ok_or_else(rejected)?;
    if resource.document.get("kind").and_then(Value::as_str) != Some(&resource.kind)
        || metadata.get("name").and_then(Value::as_str) != Some(&resource.name)
        || resource
            .namespace
            .as_deref()
            .is_some_and(|namespace| namespace != plan.namespace)
        || metadata.get("namespace").and_then(Value::as_str) != resource.namespace.as_deref()
        || metadata
            .get("labels")
            .and_then(|labels| labels.get("labweaver.io/environment-id"))
            .and_then(Value::as_str)
            != Some(&plan.environment_id.to_string())
    {
        return Err(rejected());
    }
    resource_path(&resource.kind).map(|_| ())
}

fn resource_path(kind: &str) -> Result<(&'static str, &'static str, bool), ProviderFailure> {
    match kind {
        "Namespace" => Ok(("/api/v1", "namespaces", false)),
        "ResourceQuota" => Ok(("/api/v1", "resourcequotas", true)),
        "LimitRange" => Ok(("/api/v1", "limitranges", true)),
        "ServiceAccount" => Ok(("/api/v1", "serviceaccounts", true)),
        "PersistentVolumeClaim" => Ok(("/api/v1", "persistentvolumeclaims", true)),
        "Service" => Ok(("/api/v1", "services", true)),
        "Secret" => Ok(("/api/v1", "secrets", true)),
        "Deployment" => Ok(("/apis/apps/v1", "deployments", true)),
        "NetworkPolicy" => Ok(("/apis/networking.k8s.io/v1", "networkpolicies", true)),
        "HTTPRoute" => Ok(("/apis/gateway.networking.k8s.io/v1", "httproutes", true)),
        "DataVolume" => Ok(("/apis/cdi.kubevirt.io/v1beta1", "datavolumes", true)),
        "VirtualMachine" => Ok(("/apis/kubevirt.io/v1", "virtualmachines", true)),
        "VirtualMachineInstance" => Ok(("/apis/kubevirt.io/v1", "virtualmachineinstances", true)),
        _ => Err(rejected()),
    }
}

fn accept_mutation(status: StatusCode) -> Result<(), ProviderFailure> {
    status
        .is_success()
        .then_some(())
        .ok_or_else(|| status_failure(status))
}

fn status_failure(status: StatusCode) -> ProviderFailure {
    if status == StatusCode::UNAUTHORIZED
        || status == StatusCode::FORBIDDEN
        || status == StatusCode::UNPROCESSABLE_ENTITY
        || status == StatusCode::CONFLICT
    {
        rejected()
    } else {
        unavailable()
    }
}

fn pointer_u64(value: &Value, pointer: &str) -> Result<u64, ProviderFailure> {
    value
        .pointer(pointer)
        .and_then(Value::as_u64)
        .ok_or_else(invalid_observation)
}

fn pointer_u64_or_zero(value: &Value, pointer: &str) -> Result<u64, ProviderFailure> {
    value.pointer(pointer).map_or(Ok(0), |value| {
        value.as_u64().ok_or_else(invalid_observation)
    })
}

fn pointer_uuid(value: &Value, pointer: &str) -> Result<uuid::Uuid, ProviderFailure> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .ok_or_else(invalid_observation)?
        .parse()
        .map_err(|_| invalid_observation())
}

fn verify_namespace_identity(
    namespace: &Value,
    expected_name: &str,
    environment_id: contracts::EnvironmentId,
) -> Result<(), ProviderFailure> {
    if namespace.pointer("/metadata/name").and_then(Value::as_str) != Some(expected_name)
        || namespace
            .pointer("/metadata/labels/labweaver.io~1environment-id")
            .and_then(Value::as_str)
            != Some(&environment_id.to_string())
    {
        return Err(rejected());
    }
    let finalizers = namespace
        .pointer("/metadata/finalizers")
        .and_then(Value::as_array)
        .ok_or_else(rejected)?;
    if finalizers.iter().any(|value| {
        value
            .as_str()
            .is_some_and(|name| name != "labweaver.io/environment-cleanup")
    }) {
        return Err(rejected());
    }
    Ok(())
}

fn read_secret(path: &PathBuf) -> Result<String, ProviderFailure> {
    let value = std::fs::read_to_string(path).map_err(|_| rejected())?;
    let value = value.trim();
    if value.is_empty() || value.bytes().any(|byte| byte.is_ascii_control()) {
        return Err(rejected());
    }
    Ok(value.to_owned())
}

fn validated_registry_pull_config(path: &PathBuf) -> Result<Vec<u8>, ProviderFailure> {
    let docker_config = std::fs::read(path).map_err(|_| rejected())?;
    if docker_config.is_empty() || docker_config.len() > 65_536 {
        return Err(rejected());
    }
    let parsed: Value = serde_json::from_slice(&docker_config).map_err(|_| rejected())?;
    let auths = parsed
        .get("auths")
        .and_then(Value::as_object)
        .filter(|auths| !auths.is_empty())
        .ok_or_else(rejected)?;
    if auths.keys().any(|registry| registry.trim().is_empty())
        || auths.values().any(|binding| !binding.is_object())
    {
        return Err(rejected());
    }
    Ok(docker_config)
}

fn timestamp() -> Result<UtcTimestamp, ProviderFailure> {
    UtcTimestamp::from_utc(OffsetDateTime::now_utc()).map_err(|_| unavailable())
}

const fn rejected() -> ProviderFailure {
    ProviderFailure {
        code: ProviderFailureCode::Rejected,
        retryable: false,
    }
}

const fn unavailable() -> ProviderFailure {
    ProviderFailure {
        code: ProviderFailureCode::Unavailable,
        retryable: true,
    }
}

const fn invalid_observation() -> ProviderFailure {
    ProviderFailure {
        code: ProviderFailureCode::ObservationInvalid,
        retryable: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_allowlist_has_no_dynamic_api_path() {
        for kind in ["Pod", "Role", "RoleBinding", "CustomResourceDefinition"] {
            assert!(resource_path(kind).is_err(), "{kind}");
        }
        assert!(matches!(
            resource_path("Deployment"),
            Ok(("/apis/apps/v1", "deployments", true))
        ));
        assert!(matches!(
            resource_path("Namespace"),
            Ok(("/api/v1", "namespaces", false))
        ));
        assert!(matches!(
            resource_path("VirtualMachine"),
            Ok(("/apis/kubevirt.io/v1", "virtualmachines", true))
        ));
    }

    #[test]
    fn registry_pull_config_is_bounded_and_structured() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("config.json");
        std::fs::write(&path, br#"{"auths":{"harbor.lab.lan":{"auth":"opaque"}}}"#)
            .expect("write valid config");
        assert!(validated_registry_pull_config(&path).is_ok());
        std::fs::write(&path, br#"{"auths":{}}"#).expect("write empty config");
        assert!(validated_registry_pull_config(&path).is_err());
        std::fs::write(&path, br#"{"auths":{"harbor.lab.lan":"opaque"}}"#)
            .expect("write invalid binding");
        assert!(validated_registry_pull_config(&path).is_err());
    }
}
