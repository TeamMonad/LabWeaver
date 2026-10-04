//! Restricted Kubernetes API backend for the deployment-owned runtime executor.

use std::fmt::Write as _;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use artifact_store::S3ImmutableObjectStore;
use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use contracts::{ArtifactRef, Revision, UtcTimestamp};
use persistence_sqlx::Sha256Digest; // internal persistence hash, not contract hash
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
    KubeVirtSecretRef, KubeVirtStoppedObservation, KubeVirtVmVgpuLicenseMode,
    KubeVirtVmVgpuLicensingConfiguration, ProviderFailure, ProviderFailureCode,
    cdi_import::{
        BASE_DISK_IMPORT_TIMEOUT, CdiImportError, CdiStorageSizing, KubeVirtBaseDiskImport,
        KubernetesCdiImportClient, ensure_base_disk, storage_matches, validate_cdi_storage_profile,
    },
    kubevirt_launcher_sizing::{LauncherQuota, cpu_matches, launcher_quota},
};

const FIELD_MANAGER: &str = "labweaver-runtime-executor";
const CLEANUP_MEDIA_TYPE: &str = "application/vnd.labweaver.environment-cleanup+json";
const VM_VGPU_PRIVATE_CLOUD_INIT_SECRET: &str = "vm-vgpu-cloud-init";
const MAX_VGPU_BOOTSTRAP_SECRET_BYTES: usize = 64 * 1024;
const FASTAPI_DLS_GUEST_PATCHER_PATH: &str = "/usr/local/bin/gridd-unlock-patcher";

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
    cdi_import: Arc<KubernetesCdiImportClient>,
}

impl KubernetesContainerExecutor {
    /// Resolves the current process incarnation from Kubernetes, not from an RPC caller.
    pub async fn kubevirt_execution_instance(
        &self,
        namespace: &str,
        pod_name: &str,
        pod_uid: uuid::Uuid,
        container_name: &str,
        boot_token: uuid::Uuid,
    ) -> Result<crate::KubeVirtExecutionInstance, ProviderFailure> {
        let pod = self
            .executor_pod(namespace, pod_name)
            .await?
            .ok_or_else(rejected)?;
        if pointer_uuid(&pod, "/metadata/uid")? != pod_uid {
            return Err(rejected());
        }
        validate_executor_startup(
            &pod,
            namespace,
            pod_name,
            pod_uid,
            container_name,
            std::process::id(),
            boot_token,
        )
    }

    async fn executor_pod(
        &self,
        namespace: &str,
        name: &str,
    ) -> Result<Option<Value>, ProviderFailure> {
        if !valid_dns_label(namespace) || !valid_dns_label(name) {
            return Err(rejected());
        }
        let url = self.namespaced_url(&format!("/api/v1/namespaces/{namespace}/pods/{name}"))?;
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
            cdi_import: Arc::new(KubernetesCdiImportClient::new(
                client.clone(),
                configuration.api_server.clone(),
                token.clone(),
                Duration::from_millis(configuration.cleanup_poll_milliseconds),
                BASE_DISK_IMPORT_TIMEOUT,
            )),
            configuration,
            client,
            token,
            objects,
        })
    }

    async fn apply_plan(
        &self,
        fence: &ContainerBackendFence,
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
            .filter(|resource| resource.kind != "Namespace" && resource.kind != "Deployment")
        {
            self.apply_resource(plan, resource).await?;
        }
        let deployment = plan
            .resources
            .iter()
            .find(|resource| resource.kind == "Deployment")
            .ok_or_else(rejected)?;
        self.apply_resource(plan, deployment).await?;
        self.wait_for_workspace_claim(fence, plan).await?;
        self.observe_plan(plan).await
    }

    async fn wait_for_workspace_claim(
        &self,
        fence: &ContainerBackendFence,
        plan: &ContainerResourcePlan,
    ) -> Result<(), ProviderFailure> {
        let claim = plan
            .resources
            .iter()
            .find(|resource| resource.kind == "PersistentVolumeClaim")
            .ok_or_else(rejected)?;
        loop {
            if timestamp()?.get() >= fence.deadline_at.get() {
                return Err(unavailable());
            }
            let observed = self
                .get_json("PersistentVolumeClaim", &plan.namespace, &claim.name)
                .await?;
            if workspace_claim_is_bound(observed.as_ref())? {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(
                self.configuration.cleanup_poll_milliseconds,
            ))
            .await;
        }
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
            .map_err(|_error| {
                tracing::warn!(
                    event = "environment.container_executor.kubernetes_request_failed",
                    diagnostic_code = "LW_ENVIRONMENT_PROVIDER_UNAVAILABLE",
                    failure_stage = "apply",
                    environment_id = %plan.environment_id,
                    resource_kind = %resource.kind,
                    resource_name = %resource.name,
                    error_kind = "provider_transport",
                    retryable = true
                );
                unavailable()
            })?;
        let status = response.status();
        if !status.is_success() {
            let failure = status_failure(status);
            tracing::warn!(
                event = "environment.container_executor.kubernetes_response_rejected",
                diagnostic_code = failure.diagnostic_code(),
                failure_stage = "apply",
                environment_id = %plan.environment_id,
                resource_kind = %resource.kind,
                resource_name = %resource.name,
                status_code = status.as_u16(),
                error_kind = "provider_response",
                retryable = failure.retryable
            );
            return Err(failure);
        }
        Ok(())
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
            .map_err(|_error| {
                tracing::warn!(
                    event = "environment.container_executor.kubernetes_request_failed",
                    diagnostic_code = "LW_ENVIRONMENT_PROVIDER_UNAVAILABLE",
                    failure_stage = "observe",
                    environment_id = %plan.environment_id,
                    resource_kind = %deployment.kind,
                    resource_name = %deployment.name,
                    error_kind = "provider_transport",
                    retryable = true
                );
                unavailable()
            })?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(ContainerApplyObservation {
                ready: false,
                observed_at: timestamp()?,
            });
        }
        if !response.status().is_success() {
            let failure = status_failure(response.status());
            tracing::warn!(
                event = "environment.container_executor.kubernetes_response_rejected",
                diagnostic_code = failure.diagnostic_code(),
                failure_stage = "observe",
                environment_id = %plan.environment_id,
                resource_kind = %deployment.kind,
                resource_name = %deployment.name,
                status_code = response.status().as_u16(),
                error_kind = "provider_response",
                retryable = failure.retryable
            );
            return Err(failure);
        }
        let value: Value = response.json().await.map_err(|_| invalid_observation())?;
        let generation = pointer_u64(&value, "/metadata/generation")?;
        // Kubernetes omits `status.observedGeneration` until the deployment
        // controller has observed the resource for the first time. Absence is
        // therefore a valid pending observation; a present non-integer value
        // remains a contract violation.
        let observed_generation = pointer_u64_or_zero(&value, "/status/observedGeneration")?;
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
        if replicas == 0 {
            return self.stop_container(fence, plan).await;
        }
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

    async fn owned_namespace(
        &self,
        namespace: &str,
        environment_id: contracts::EnvironmentId,
        project_id: contracts::ProjectId,
    ) -> Result<Option<Value>, ProviderFailure> {
        if namespace != format!("lw-env-{environment_id}") {
            return Err(rejected());
        }
        let url = self.namespaced_url(&format!("/api/v1/namespaces/{namespace}"))?;
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
        let value: Value = response.json().await.map_err(|_| invalid_observation())?;
        verify_owned_identity(&value, namespace, None, environment_id, project_id)?;
        if value
            .pointer("/metadata/labels/labweaver.io~1managed")
            .and_then(Value::as_str)
            != Some("true")
        {
            return Err(rejected());
        }
        Ok(Some(value))
    }

    async fn stop_container(
        &self,
        fence: &ContainerBackendFence,
        plan: &ContainerResourcePlan,
    ) -> Result<ContainerApplyObservation, ProviderFailure> {
        validate_cleanup_plan(plan)?;
        self.owned_namespace(&plan.namespace, plan.environment_id, plan.project_id)
            .await?
            .ok_or_else(unavailable)?;
        let deployment = self
            .get_json("Deployment", &plan.namespace, "runtime")
            .await?
            .ok_or_else(unavailable)?;
        verify_owned_identity(
            &deployment,
            "runtime",
            Some(&plan.namespace),
            plan.environment_id,
            plan.project_id,
        )?;
        let uid = pointer_uuid(&deployment, "/metadata/uid")?;
        let resource_version = deployment
            .pointer("/metadata/resourceVersion")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(invalid_observation)?;
        let url = self.namespaced_url(&format!(
            "/apis/apps/v1/namespaces/{}/deployments/runtime",
            plan.namespace
        ))?;
        if timestamp()?.get() >= fence.deadline_at.get() {
            return Err(unavailable());
        }
        let response = self
            .authorized(
                self.client
                    .patch(url)
                    .header("content-type", "application/merge-patch+json")
                    .json(&json!({
                        "metadata":{"uid":uid, "resourceVersion":resource_version},
                        "spec":{"replicas":0}
                    })),
            )
            .send()
            .await
            .map_err(|_| unavailable())?;
        accept_mutation(response.status())?;
        loop {
            if timestamp()?.get() >= fence.deadline_at.get() {
                return Err(unavailable());
            }
            let deployment = self
                .get_json("Deployment", &plan.namespace, "runtime")
                .await?
                .ok_or_else(unavailable)?;
            verify_owned_identity(
                &deployment,
                "runtime",
                Some(&plan.namespace),
                plan.environment_id,
                plan.project_id,
            )?;
            if pointer_uuid(&deployment, "/metadata/uid")? != uid {
                return Err(rejected());
            }
            let url =
                self.namespaced_url(&format!("/api/v1/namespaces/{}/pods", plan.namespace))?;
            let response = self
                .authorized(self.client.get(url))
                .send()
                .await
                .map_err(|_| unavailable())?;
            if !response.status().is_success() {
                return Err(status_failure(response.status()));
            }
            let pods: Value = response.json().await.map_err(|_| invalid_observation())?;
            let pods_absent = pods
                .get("items")
                .and_then(Value::as_array)
                .ok_or_else(invalid_observation)?
                .is_empty();
            if pointer_u64(&deployment, "/spec/replicas")? == 0
                && pointer_u64_or_zero(&deployment, "/status/observedGeneration")?
                    >= pointer_u64(&deployment, "/metadata/generation")?
                && pointer_u64_or_zero(&deployment, "/status/replicas")? == 0
                && pods_absent
            {
                return Ok(ContainerApplyObservation {
                    ready: true,
                    observed_at: timestamp()?,
                });
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
        validate_cleanup_plan(plan)?;
        self.remove_owned_namespace(
            plan.environment_id,
            plan.project_id,
            &plan.namespace,
            fence.deadline_at,
            None,
        )
        .await?;
        self.write_cleanup_evidence(fence, plan).await
    }

    async fn remove_owned_namespace(
        &self,
        environment_id: contracts::EnvironmentId,
        project_id: contracts::ProjectId,
        namespace: &str,
        deadline_at: UtcTimestamp,
        kubevirt_permit: Option<&crate::KubeVirtExecutionPermit>,
    ) -> Result<(), ProviderFailure> {
        let url = self.namespaced_url(&format!("/api/v1/namespaces/{namespace}"))?;
        loop {
            if timestamp()?.get() >= deadline_at.get() {
                return Err(unavailable());
            }
            let Some(value) = self
                .owned_namespace(namespace, environment_id, project_id)
                .await?
            else {
                return Ok(());
            };
            verify_namespace_identity(&value, namespace, environment_id)?;
            let uid = pointer_uuid(&value, "/metadata/uid")?;
            let resource_version = value
                .pointer("/metadata/resourceVersion")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .ok_or_else(invalid_observation)?;
            if let Some(permit) = kubevirt_permit {
                check_kubevirt_permit(permit).await?;
            }
            let response = self
                .authorized(self.client.delete(url.clone()).json(&json!({
                    "apiVersion":"v1", "kind":"DeleteOptions",
                    "preconditions":{"uid":uid, "resourceVersion":resource_version}
                })))
                .send()
                .await
                .map_err(|_| unavailable())?;
            if response.status() == StatusCode::CONFLICT {
                continue;
            }
            if response.status() != StatusCode::NOT_FOUND && !response.status().is_success() {
                return Err(status_failure(response.status()));
            }
            // Re-read after deletion starts before clearing only the already allowed metadata finalizers.
            if let Some(value) = self
                .owned_namespace(namespace, environment_id, project_id)
                .await?
            {
                verify_namespace_identity(&value, namespace, environment_id)?;
                if pointer_uuid(&value, "/metadata/uid")? != uid {
                    return Err(rejected());
                }
                if value
                    .pointer("/metadata/deletionTimestamp")
                    .and_then(Value::as_str)
                    .is_none()
                {
                    return Err(unavailable());
                }
                let resource_version = value
                    .pointer("/metadata/resourceVersion")
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
                    .ok_or_else(invalid_observation)?;
                if let Some(permit) = kubevirt_permit {
                    check_kubevirt_permit(permit).await?;
                }
                let response = self.authorized(self.client.patch(url.clone()).header("content-type", "application/merge-patch+json").json(&json!({
                    "metadata":{"uid":uid, "resourceVersion":resource_version, "finalizers":[]}
                }))).send().await.map_err(|_| unavailable())?;
                if response.status() != StatusCode::NOT_FOUND
                    && response.status() != StatusCode::CONFLICT
                    && !response.status().is_success()
                {
                    return Err(status_failure(response.status()));
                }
            }
            tokio::time::sleep(Duration::from_millis(
                self.configuration.cleanup_poll_milliseconds,
            ))
            .await;
        }
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
        permit: &crate::KubeVirtExecutionPermit,
    ) -> Result<(), ProviderFailure> {
        validate_kubevirt_plan(plan)?;
        let vm_vgpu_licensing = vm_vgpu_licensing_configuration(plan)?;
        let namespace_resource = plan
            .resources
            .iter()
            .find(|resource| resource.kind == "Namespace")
            .ok_or_else(rejected)?;
        validate_kubevirt_resource(plan, namespace_resource)?;
        self.apply_kubevirt_resource(plan, namespace_resource, permit)
            .await?;
        self.ensure_base_disk(plan).await?;
        let physical_quota = self.kubevirt_quota(plan).await?;
        if let Some(licensing) = vm_vgpu_licensing.as_ref() {
            self.apply_vgpu_private_cloud_init(plan, licensing, permit)
                .await?;
        }
        for resource in &plan.resources {
            if resource.kind == "Namespace" {
                continue;
            }
            validate_kubevirt_resource(plan, resource)?;
            let mut resource = resource.clone();
            if resource.kind == "ResourceQuota" {
                resource = physical_quota.clone();
            }
            if vm_vgpu_licensing.is_some() && resource.kind == "VirtualMachine" {
                point_vm_to_private_cloud_init(&mut resource)?;
            }
            self.apply_kubevirt_resource(plan, &resource, permit)
                .await?;
        }
        Ok(())
    }

    /// Keep approval and billing logical; reserve the backend's actual concurrent workloads.
    async fn kubevirt_quota(
        &self,
        plan: &KubeVirtResourcePlan,
    ) -> Result<KubeVirtResource, ProviderFailure> {
        let (quota, disk, logical, scratch) = kubevirt_storage_intent(plan)?;
        let mut virtual_machines = plan.resources.iter().filter(|r| r.kind == "VirtualMachine");
        let vm = virtual_machines.next().ok_or_else(rejected)?;
        if virtual_machines.next().is_some() {
            return Err(rejected());
        }
        validate_kubevirt_resource(plan, vm)?;
        let kubevirts = self
            .get_api_json("/apis/kubevirt.io/v1/kubevirts?limit=2")
            .await?
            .ok_or_else(rejected)?;
        let launcher =
            launcher_quota(&vm.document, &quota.document, &kubevirts).map_err(|_| rejected())?;
        let config = self
            .get_api_json("/apis/cdi.kubevirt.io/v1beta1/cdiconfigs/config")
            .await?
            .ok_or_else(rejected)?;
        self.verify_cdi_storage_class(&plan.storage_class_name, true)
            .await?;
        let scratch_class = match config.pointer("/status/scratchSpaceStorageClass") {
            None | Some(Value::Null) => plan.storage_class_name.as_str(),
            Some(Value::String(value)) if value.is_empty() => plan.storage_class_name.as_str(),
            Some(Value::String(value)) => value,
            Some(_) => return Err(rejected()),
        };
        if scratch_class != plan.storage_class_name {
            self.verify_cdi_storage_class(scratch_class, false).await?;
        }
        let sizing = CdiStorageSizing::from_config(&config, &plan.storage_class_name)
            .map_err(|error| cdi_import_failure(&error))?;
        let physical_root = sizing
            .root_bytes(logical)
            .map_err(|error| cdi_import_failure(&error))?;
        let physical_scratch = sizing
            .scratch_bytes(scratch)
            .map_err(|error| cdi_import_failure(&error))?;
        let physical_total = physical_root
            .checked_add(physical_scratch)
            .ok_or_else(rejected)?;
        let pvc = self
            .get_json("PersistentVolumeClaim", &plan.namespace, &disk.name)
            .await?;
        let existing_quota = self
            .get_json("ResourceQuota", &plan.namespace, &quota.name)
            .await?;
        if let Some(pvc) = pvc {
            let existing_disk = self
                .get_json("DataVolume", &plan.namespace, &disk.name)
                .await?
                .ok_or_else(rejected)?;
            validate_existing_kubevirt_disk(plan, disk, &existing_disk, &pvc, physical_root)?;
            if existing_quota.is_none() {
                return Err(rejected());
            }
        }
        if let Some(existing_quota) = existing_quota {
            validate_existing_kubevirt_quota(plan, &existing_quota, physical_total, &launcher)?;
        }
        let mut quota = quota.clone();
        *quota
            .document
            .pointer_mut("/spec/hard/requests.storage")
            .ok_or_else(rejected)? = json!(physical_total.to_string());
        let hard = quota
            .document
            .pointer_mut("/spec/hard")
            .and_then(Value::as_object_mut)
            .ok_or_else(rejected)?;
        hard.insert(
            "requests.memory".to_owned(),
            json!(launcher.memory_request.to_string()),
        );
        hard.insert(
            "limits.memory".to_owned(),
            json!(launcher.memory_limit.to_string()),
        );
        hard.insert(
            "requests.cpu".to_owned(),
            json!(format!("{}m", launcher.cpu_request_millicores)),
        );
        hard.insert(
            "limits.cpu".to_owned(),
            json!(format!("{}m", launcher.cpu_limit_millicores)),
        );
        validate_kubevirt_resource(plan, &quota)?;
        Ok(quota)
    }

    async fn verify_cdi_storage_class(
        &self,
        name: &str,
        check_profile: bool,
    ) -> Result<(), ProviderFailure> {
        if !valid_dns_label(name) {
            return Err(rejected());
        }
        let storage_class = self
            .get_api_json(&format!("/apis/storage.k8s.io/v1/storageclasses/{name}"))
            .await?
            .ok_or_else(rejected)?;
        if storage_class
            .pointer("/metadata/name")
            .and_then(Value::as_str)
            != Some(name)
            || storage_class
                .get("provisioner")
                .and_then(Value::as_str)
                .is_none_or(str::is_empty)
        {
            return Err(rejected());
        }
        if check_profile {
            let profile = self
                .get_api_json(&format!(
                    "/apis/cdi.kubevirt.io/v1beta1/storageprofiles/{name}"
                ))
                .await?
                .ok_or_else(rejected)?;
            validate_cdi_storage_profile(Some(&profile), name)
                .map_err(|error| cdi_import_failure(&error))?;
        }
        Ok(())
    }

    async fn apply_kubevirt_resource(
        &self,
        plan: &KubeVirtResourcePlan,
        resource: &KubeVirtResource,
        permit: &crate::KubeVirtExecutionPermit,
    ) -> Result<(), ProviderFailure> {
        validate_kubevirt_resource(plan, resource)?;
        let url = self.kubevirt_resource_url(resource)?;
        check_kubevirt_permit(permit).await?;
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
            .map_err(|_error| {
                tracing::warn!(
                    event = "environment.kubevirt_executor.kubernetes_request_failed",
                    diagnostic_code = "LW_ENVIRONMENT_PROVIDER_UNAVAILABLE",
                    failure_stage = "apply",
                    environment_id = %plan.environment_id,
                    resource_kind = %resource.kind,
                    resource_name = %resource.name,
                    error_kind = "provider_transport",
                    retryable = true
                );
                unavailable()
            })?;
        let status = response.status();
        if !status.is_success() {
            let failure = status_failure(status);
            tracing::warn!(
                event = "environment.kubevirt_executor.kubernetes_response_rejected",
                diagnostic_code = failure.diagnostic_code(),
                failure_stage = "apply",
                environment_id = %plan.environment_id,
                resource_kind = %resource.kind,
                resource_name = %resource.name,
                status_code = status.as_u16(),
                error_kind = "provider_response",
                retryable = failure.retryable
            );
            return Err(failure);
        }
        Ok(())
    }

    async fn apply_vgpu_private_cloud_init(
        &self,
        plan: &KubeVirtResourcePlan,
        licensing: &KubeVirtVmVgpuLicensingConfiguration,
        permit: &crate::KubeVirtExecutionPermit,
    ) -> Result<(), ProviderFailure> {
        let base_secret = plan
            .resources
            .iter()
            .find(|resource| resource.kind == "Secret" && resource.name == "cloud-init")
            .ok_or_else(rejected)?;
        let base_userdata = secret_data_field(&base_secret.document, "userdata")?;
        let networkdata = secret_data_field(&base_secret.document, "networkdata")?;
        let token = self.read_secret_data(&licensing.token_secret_ref).await?;
        let tls_ca = self.read_secret_data(&licensing.tls_ca_secret_ref).await?;
        let signing_root = match licensing.mode {
            KubeVirtVmVgpuLicenseMode::NvidiaDls => None,
            KubeVirtVmVgpuLicenseMode::FastapiDls => Some(
                self.read_secret_data(
                    licensing
                        .fastapi_dls_signing_root_ca_secret_ref
                        .as_ref()
                        .ok_or_else(rejected)?,
                )
                .await?,
            ),
        };
        let userdata = render_vm_vgpu_cloud_init(
            &base_userdata,
            &token,
            &tls_ca,
            signing_root.as_deref(),
            licensing,
        )?;
        let mut document = base_secret.document.clone();
        document["metadata"]["name"] = json!(VM_VGPU_PRIVATE_CLOUD_INIT_SECRET);
        document["metadata"]["annotations"]["labweaver.io/private-bootstrap"] = json!("true");
        document["data"]["userdata"] = json!(BASE64_STANDARD.encode(userdata.as_bytes()));
        document["data"]["networkdata"] = json!(BASE64_STANDARD.encode(networkdata));
        let private_secret = KubeVirtResource {
            kind: "Secret".to_owned(),
            namespace: Some(plan.namespace.clone()),
            name: VM_VGPU_PRIVATE_CLOUD_INIT_SECRET.to_owned(),
            document,
        };
        self.apply_kubevirt_resource(plan, &private_secret, permit)
            .await
    }

    async fn read_secret_data(
        &self,
        reference: &KubeVirtSecretRef,
    ) -> Result<Vec<u8>, ProviderFailure> {
        let secret = self
            .get_json("Secret", &reference.namespace, &reference.name)
            .await?
            .ok_or_else(rejected)?;
        let encoded = secret
            .pointer(&format!("/data/{}", json_pointer_escape(&reference.key)))
            .and_then(Value::as_str)
            .ok_or_else(rejected)?;
        let data = BASE64_STANDARD.decode(encoded).map_err(|_| rejected())?;
        if data.is_empty() || data.len() > MAX_VGPU_BOOTSTRAP_SECRET_BYTES {
            return Err(rejected());
        }
        Ok(data)
    }

    /// Imports and identity-checks the base disk the plan's clone `DataVolume` sources from.
    ///
    /// Runs before any plan object is applied so the clone's `sourceRef` target always exists. A
    /// failed import fails closed and leaves the importer objects in place for diagnosis.
    async fn ensure_base_disk(&self, plan: &KubeVirtResourcePlan) -> Result<(), ProviderFailure> {
        let import = KubeVirtBaseDiskImport {
            data_source_namespace: plan.base_disk_data_source_namespace.clone(),
            data_source_name: plan.base_disk_data_source_name.clone(),
            source_registry_digest: plan.base_disk.source_registry_digest.clone(),
            disk_sha256: plan.base_disk_disk_sha256.clone(),
            identity: plan.base_disk_identity,
            storage_class_name: plan.storage_class_name.clone(),
            capacity_bytes: plan.base_disk.capacity_bytes,
        };
        ensure_base_disk(self.cdi_import.as_ref(), &import)
            .await
            .map_err(|error| {
                let failure = cdi_import_failure(&error);
                tracing::warn!(
                    event = "environment.kubevirt_executor.base_disk_import_failed",
                    diagnostic_code = %error,
                    failure_stage = "base_disk_import",
                    environment_id = %plan.environment_id,
                    error_kind = "vm_base_import",
                    retryable = failure.retryable
                );
                failure
            })
            .map(|_base_disk| ())
    }

    async fn observe_kubevirt_running(
        &self,
        fence: &KubeVirtBackendFence,
        plan: &KubeVirtResourcePlan,
    ) -> Result<KubeVirtRunningObservation, ProviderFailure> {
        let started = std::time::Instant::now();
        self.observe_kubevirt_running_gated(fence, plan)
            .await
            .map_err(|(failure, gate)| {
                log_kubevirt_readiness_failure(fence, started, failure, gate, true);
                failure
            })
    }

    async fn observe_kubevirt_running_gated(
        &self,
        fence: &KubeVirtBackendFence,
        plan: &KubeVirtResourcePlan,
    ) -> Result<KubeVirtRunningObservation, (ProviderFailure, &'static str)> {
        validate_kubevirt_plan(plan).map_err(|error| (error, "plan_invalid"))?;
        let vm = self
            .get_json(
                "VirtualMachine",
                &plan.namespace,
                &plan.virtual_machine_name,
            )
            .await
            .map_err(|error| (error, "vm_read"))?
            .ok_or_else(|| (unavailable(), "vm_missing"))?;
        let vmi = self
            .get_json(
                "VirtualMachineInstance",
                &plan.namespace,
                &plan.virtual_machine_name,
            )
            .await
            .map_err(|error| (error, "vmi_read"))?
            .ok_or_else(|| (unavailable(), "vmi_missing"))?;
        let pvc = self
            .get_json(
                "PersistentVolumeClaim",
                &plan.namespace,
                &plan.data_volume_name,
            )
            .await
            .map_err(|error| (error, "pvc_read"))?
            .ok_or_else(|| (unavailable(), "pvc_missing"))?;
        let service = self
            .get_json("Service", &plan.namespace, "ssh")
            .await
            .map_err(|error| (error, "service_read"))?
            .ok_or_else(|| (unavailable(), "service_missing"))?;
        if vmi.pointer("/status/phase").and_then(Value::as_str) != Some("Running") {
            return Err((unavailable(), "vmi_not_running"));
        }
        let guest_ip = vmi
            .pointer("/status/interfaces/0/ipAddress")
            .and_then(Value::as_str)
            .ok_or_else(|| (unavailable(), "guest_ip_missing"))?
            .parse::<IpAddr>()
            .map_err(|_| (invalid_observation(), "guest_ip_invalid"))?;
        let service_cluster_ip = service
            .pointer("/spec/clusterIP")
            .and_then(Value::as_str)
            .ok_or_else(|| (unavailable(), "service_ip_missing"))?
            .parse::<IpAddr>()
            .map_err(|_| (invalid_observation(), "service_ip_invalid"))?;
        let conditions = vmi
            .pointer("/status/conditions")
            .and_then(Value::as_array)
            .ok_or_else(|| (unavailable(), "vmi_conditions_missing"))?;
        let condition_true = |kind: &str| {
            conditions.iter().any(|condition| {
                condition.get("type").and_then(Value::as_str) == Some(kind)
                    && condition.get("status").and_then(Value::as_str) == Some("True")
            })
        };
        if !condition_true("Ready") {
            return Err((unavailable(), "vmi_not_ready"));
        }
        let ssh_host_key_sha256 = self.probe_ssh_host_key(service_cluster_ip).await?;
        Ok(KubeVirtRunningObservation {
            observed_environment_generation: fence.environment_generation,
            vm_resource_generation: pointer_u64(&vm, "/metadata/generation").map_err(|error| {
                (
                    error,
                    if vm.pointer("/metadata/generation").is_some() {
                        "vm_generation_invalid"
                    } else {
                        "vm_generation_missing"
                    },
                )
            })?,
            observed_vm_resource_generation: pointer_u64(&vm, "/status/observedGeneration")
                .map_err(|error| {
                    (
                        error,
                        if vm.pointer("/status/observedGeneration").is_some() {
                            "vm_observed_generation_invalid"
                        } else {
                            "vm_observed_generation_missing"
                        },
                    )
                })?,
            vm_uid: pointer_uuid(&vm, "/metadata/uid")
                .map_err(|error| (error, "vm_uid_invalid"))?,
            vmi_uid: pointer_uuid(&vmi, "/metadata/uid")
                .map_err(|error| (error, "vmi_uid_invalid"))?,
            root_disk_uid: pointer_uuid(&pvc, "/metadata/uid")
                .map_err(|error| (error, "pvc_uid_invalid"))?,
            guest_ip,
            service_cluster_ip,
            ssh_host_key_sha256,
            guest_agent_connected: condition_true("AgentConnected"),
            ssh_ready: true,
            observed_at: timestamp().map_err(|error| (error, "observation_time_invalid"))?,
        })
    }

    async fn wait_kubevirt_running(
        &self,
        fence: &KubeVirtBackendFence,
        plan: &KubeVirtResourcePlan,
    ) -> Result<KubeVirtRunningObservation, ProviderFailure> {
        let started = std::time::Instant::now();
        let mut previous_gate = None;
        loop {
            match self.observe_kubevirt_running_gated(fence, plan).await {
                Ok(observation) => return Ok(observation),
                Err((failure, gate)) => {
                    let retry = failure.code == ProviderFailureCode::Unavailable
                        && timestamp()?.get() < fence.deadline_at.get();
                    if previous_gate != Some(gate) || !retry {
                        log_kubevirt_readiness_failure(fence, started, failure, gate, !retry);
                        previous_gate = Some(gate);
                    }
                    if !retry {
                        return Err(failure);
                    }
                    tokio::time::sleep(Duration::from_millis(
                        self.configuration.cleanup_poll_milliseconds,
                    ))
                    .await;
                }
            }
        }
    }

    async fn kubevirt_stop_identity(
        &self,
        plan: &KubeVirtCleanupPlan,
    ) -> Result<(uuid::Uuid, uuid::Uuid), ProviderFailure> {
        if plan.namespace != format!("lw-env-{}", plan.environment_id)
            || plan.virtual_machine_name != "runtime"
        {
            return Err(rejected());
        }
        self.owned_namespace(&plan.namespace, plan.environment_id, plan.project_id)
            .await?
            .ok_or_else(unavailable)?;
        let vm = self
            .get_json(
                "VirtualMachine",
                &plan.namespace,
                &plan.virtual_machine_name,
            )
            .await?
            .ok_or_else(unavailable)?;
        let pvc = self
            .get_json("PersistentVolumeClaim", &plan.namespace, "rootdisk")
            .await?
            .ok_or_else(unavailable)?;
        verify_owned_identity(
            &vm,
            &plan.virtual_machine_name,
            Some(&plan.namespace),
            plan.environment_id,
            plan.project_id,
        )?;
        verify_owned_identity(
            &pvc,
            "rootdisk",
            Some(&plan.namespace),
            plan.environment_id,
            plan.project_id,
        )?;
        Ok((
            pointer_uuid(&vm, "/metadata/uid")?,
            pointer_uuid(&pvc, "/metadata/uid")?,
        ))
    }

    async fn observe_kubevirt_stopped(
        &self,
        fence: &KubeVirtBackendFence,
        plan: &KubeVirtCleanupPlan,
        identities: (uuid::Uuid, uuid::Uuid),
    ) -> Result<KubeVirtStoppedObservation, ProviderFailure> {
        if self.kubevirt_stop_identity(plan).await? != identities {
            return Err(rejected());
        }
        if let Some(vmi) = self
            .get_json(
                "VirtualMachineInstance",
                &plan.namespace,
                &plan.virtual_machine_name,
            )
            .await?
        {
            verify_owned_identity(
                &vmi,
                &plan.virtual_machine_name,
                Some(&plan.namespace),
                plan.environment_id,
                plan.project_id,
            )?;
            return Err(unavailable());
        }
        Ok(KubeVirtStoppedObservation {
            observed_environment_generation: fence.environment_generation,
            vm_uid: identities.0,
            root_disk_uid: identities.1,
            vmi_absent: true,
            observed_at: timestamp()?,
        })
    }

    async fn kubevirt_subresource(
        &self,
        fence: &KubeVirtBackendFence,
        plan: &KubeVirtResourcePlan,
        action: &str,
        permit: &crate::KubeVirtExecutionPermit,
    ) -> Result<(), ProviderFailure> {
        if !matches!(action, "start" | "stop" | "restart") {
            return Err(rejected());
        }
        validate_kubevirt_plan(plan)?;
        self.kubevirt_lifecycle_subresource(
            fence,
            &plan.namespace,
            &plan.virtual_machine_name,
            action,
            permit,
        )
        .await
    }

    async fn kubevirt_lifecycle_subresource(
        &self,
        fence: &KubeVirtBackendFence,
        namespace: &str,
        virtual_machine_name: &str,
        action: &str,
        permit: &crate::KubeVirtExecutionPermit,
    ) -> Result<(), ProviderFailure> {
        if timestamp()?.get() >= fence.deadline_at.get() {
            return Err(unavailable());
        }
        let url = self.namespaced_url(&format!(
            "/apis/subresources.kubevirt.io/v1/namespaces/{namespace}/virtualmachines/{virtual_machine_name}/{action}"
        ))?;
        let request_body = if action == "start" {
            json!({})
        } else {
            json!({"gracePeriod":30})
        };
        check_kubevirt_permit(permit).await?;
        let response = self
            .authorized(
                self.client
                    .put(url)
                    .header("content-type", "application/json")
                    .json(&request_body),
            )
            .send()
            .await
            .map_err(|_| unavailable())?;
        if response.status() == StatusCode::CONFLICT && matches!(action, "start" | "stop") {
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
        permit: &crate::KubeVirtExecutionPermit,
    ) -> Result<ArtifactRef, ProviderFailure> {
        let expected_namespace = format!("lw-env-{}", plan.environment_id);
        if plan.namespace != expected_namespace || plan.virtual_machine_name != "runtime" {
            return Err(rejected());
        }
        self.remove_owned_namespace(
            plan.environment_id,
            plan.project_id,
            &plan.namespace,
            fence.deadline_at,
            Some(permit),
        )
        .await?;
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
        _now: UtcTimestamp,
        document: Value,
    ) -> Result<ArtifactRef, ProviderFailure> {
        let bytes = serde_json::to_vec(&document).map_err(|_| invalid_observation())?;
        let key = self
            .objects
            .scoped_key(&format!("cleanup/{environment_id}/{request_id}.json"))
            .map_err(|_error| {
                tracing::warn!(
                    event = "environment.runtime_executor.cleanup_evidence_key_invalid",
                    diagnostic_code = "LW_ENVIRONMENT_PROVIDER_CLEANUP_FAILED",
                    environment_id = %environment_id,
                    request_id = %request_id,
                    error_kind = "artifact_identity",
                    failure_stage = "cleanup_evidence_key",
                    retryable = false
                );
                rejected()
            })?;
        self.objects
            .put_versioned_immutable(&key, &bytes, CLEANUP_MEDIA_TYPE)
            .await
            .map(|object| object.reference)
            .map_err(|_error| {
                tracing::warn!(
                    event = "environment.runtime_executor.cleanup_evidence_store_failed",
                    diagnostic_code = "LW_ENVIRONMENT_PROVIDER_CLEANUP_FAILED",
                    environment_id = %environment_id,
                    request_id = %request_id,
                    error_kind = "artifact_store",
                    failure_stage = "cleanup_evidence_store",
                    retryable = true
                );
                ProviderFailure {
                    code: ProviderFailureCode::CleanupFailed,
                    retryable: true,
                }
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
        self.get_api_json(&format!("{prefix}/namespaces/{namespace}/{plural}/{name}"))
            .await
    }

    async fn get_api_json(&self, path: &str) -> Result<Option<Value>, ProviderFailure> {
        let url = self.namespaced_url(path)?;
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

    async fn probe_ssh_host_key(
        &self,
        address: IpAddr,
    ) -> Result<Sha256Digest, (ProviderFailure, &'static str)> {
        let observed = Arc::new(Mutex::new(None));
        let handler = HostKeyProbe {
            observed: Arc::clone(&observed),
        };
        let configuration = Arc::new(russh::client::Config {
            // Keep Russh's complete safe preference set so the observed identity is compatible
            // with the collector's OpenSSH-certificate session. HostKeyProbe still records the
            // exact negotiated host-key SHA-256 identity.
            preferred: russh::Preferred::default(),
            ..russh::client::Config::default()
        });
        let connection = tokio::time::timeout(
            Duration::from_millis(self.configuration.ssh_handshake_timeout_milliseconds),
            russh::client::connect(configuration, SocketAddr::new(address, 22), handler),
        )
        .await
        .map_err(|_| (unavailable(), "ssh_connect_timeout"))?
        .map_err(|_| (unavailable(), "ssh_connect_error"))?;
        connection
            .disconnect(russh::Disconnect::ByApplication, "probe-complete", "en")
            .await
            .map_err(|_| (unavailable(), "ssh_disconnect_error"))?;
        observed
            .lock()
            .await
            .ok_or_else(|| (unavailable(), "ssh_host_key_missing"))
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

fn vm_vgpu_licensing_configuration(
    plan: &KubeVirtResourcePlan,
) -> Result<Option<&KubeVirtVmVgpuLicensingConfiguration>, ProviderFailure> {
    let vm = plan
        .resources
        .iter()
        .find(|resource| resource.kind == "VirtualMachine")
        .ok_or_else(rejected)?;
    let gpu_devices = vm
        .document
        .pointer("/spec/template/spec/domain/devices/gpus")
        .and_then(Value::as_array)
        .ok_or_else(rejected)?;
    let has_vgpu = !gpu_devices.is_empty();
    match (has_vgpu, plan.vm_vgpu_licensing.as_ref()) {
        (false, None) => Ok(None),
        (true, Some(configuration)) => {
            configuration.validate().map_err(|_| rejected())?;
            Ok(Some(configuration))
        }
        _ => Err(rejected()),
    }
}

fn kubevirt_storage_intent(
    plan: &KubeVirtResourcePlan,
) -> Result<(&KubeVirtResource, &KubeVirtResource, u64, u64), ProviderFailure> {
    let mut quotas = plan
        .resources
        .iter()
        .filter(|resource| resource.kind == "ResourceQuota");
    let quota = quotas.next().ok_or_else(rejected)?;
    let mut disks = plan
        .resources
        .iter()
        .filter(|resource| resource.kind == "DataVolume");
    let disk = disks.next().ok_or_else(rejected)?;
    if quotas.next().is_some()
        || disks.next().is_some()
        || quota.name != "runtime-quota"
        || disk.name != plan.data_volume_name
        || quota.namespace.as_deref() != Some(&plan.namespace)
        || disk.namespace.as_deref() != Some(&plan.namespace)
        || quota.document.get("apiVersion").and_then(Value::as_str) != Some("v1")
        || disk.document.get("apiVersion").and_then(Value::as_str)
            != Some("cdi.kubevirt.io/v1beta1")
        || disk
            .document
            .pointer("/spec/storage/storageClassName")
            .and_then(Value::as_str)
            != Some(&plan.storage_class_name)
        || [quota, disk].iter().any(|resource| {
            resource
                .document
                .pointer("/metadata/labels/labweaver.io~1managed")
                .and_then(Value::as_str)
                != Some("true")
        })
        || disk
            .document
            .pointer("/spec/storage/volumeMode")
            .and_then(Value::as_str)
            != Some("Filesystem")
        || disk.document.pointer("/spec/storage/accessModes") != Some(&json!(["ReadWriteOnce"]))
    {
        return Err(rejected());
    }
    validate_kubevirt_resource(plan, quota)?;
    validate_kubevirt_resource(plan, disk)?;
    let logical =
        positive_storage_bytes(&disk.document, "/spec/storage/resources/requests/storage")?;
    let scratch = positive_storage_bytes(
        &quota.document,
        "/metadata/annotations/labweaver.io~1cdi-scratch-storage-bytes",
    )?;
    let quota_logical = positive_storage_bytes(&quota.document, "/spec/hard/requests.storage")?;
    if logical < plan.base_disk.capacity_bytes
        || logical > scratch
        || logical.checked_add(scratch) != Some(quota_logical)
    {
        return Err(rejected());
    }
    Ok((quota, disk, logical, scratch))
}

fn positive_storage_bytes(document: &Value, pointer: &str) -> Result<u64, ProviderFailure> {
    document
        .pointer(pointer)
        .and_then(Value::as_str)
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .ok_or_else(rejected)
}

fn filesystem_volume_mode(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => true,
        Some(Value::String(value)) => value == "Filesystem",
        _ => false,
    }
}

fn validate_existing_kubevirt_disk(
    plan: &KubeVirtResourcePlan,
    disk: &KubeVirtResource,
    existing_disk: &Value,
    pvc: &Value,
    physical: u64,
) -> Result<(), ProviderFailure> {
    validate_kubevirt_resource(
        plan,
        &KubeVirtResource {
            document: existing_disk.clone(),
            ..disk.clone()
        },
    )?;
    let disk_uid = pointer_uuid(existing_disk, "/metadata/uid")?;
    let logical =
        positive_storage_bytes(&disk.document, "/spec/storage/resources/requests/storage")?;
    let controllers = pvc
        .pointer("/metadata/ownerReferences")
        .and_then(Value::as_array)
        .ok_or_else(rejected)?
        .iter()
        .filter(|owner| owner.get("controller") == Some(&Value::Bool(true)))
        .collect::<Vec<_>>();
    let capacity = pvc.pointer("/status/capacity/storage");
    let capacity_valid = match capacity {
        None | Some(Value::Null) => {
            pvc.pointer("/status/phase").and_then(Value::as_str) == Some("Pending")
        }
        Some(Value::String(value)) => storage_matches(value, physical),
        _ => false,
    };
    if pvc.pointer("/metadata/name").and_then(Value::as_str) != Some(&plan.data_volume_name)
        || pvc.pointer("/metadata/namespace").and_then(Value::as_str) != Some(&plan.namespace)
        || controllers.len() != 1
        || controllers[0].get("apiVersion").and_then(Value::as_str)
            != Some("cdi.kubevirt.io/v1beta1")
        || controllers[0].get("kind").and_then(Value::as_str) != Some("DataVolume")
        || controllers[0].get("name").and_then(Value::as_str) != Some(&disk.name)
        || controllers[0].get("uid").and_then(Value::as_str) != Some(&disk_uid.to_string())
        || existing_disk.pointer("/spec/storage/storageClassName")
            != disk.document.pointer("/spec/storage/storageClassName")
        || existing_disk.pointer("/spec/storage/accessModes")
            != disk.document.pointer("/spec/storage/accessModes")
        || existing_disk
            .pointer("/spec/storage/volumeMode")
            .and_then(Value::as_str)
            != Some("Filesystem")
        || !existing_disk
            .pointer("/spec/storage/resources/requests/storage")
            .and_then(Value::as_str)
            .is_some_and(|value| storage_matches(value, logical))
        || existing_disk.pointer("/spec/sourceRef") != disk.document.pointer("/spec/sourceRef")
        || existing_disk
            .pointer("/metadata/deletionTimestamp")
            .is_some_and(|value| !value.is_null())
        || pvc
            .pointer("/spec/storageClassName")
            .and_then(Value::as_str)
            != Some(&plan.storage_class_name)
        || !filesystem_volume_mode(pvc.pointer("/spec/volumeMode"))
        || pvc
            .pointer("/metadata/deletionTimestamp")
            .is_some_and(|value| !value.is_null())
        || !pvc
            .pointer("/spec/resources/requests/storage")
            .and_then(Value::as_str)
            .is_some_and(|value| storage_matches(value, physical))
        || !capacity_valid
    {
        return Err(rejected());
    }
    Ok(())
}

fn validate_existing_kubevirt_quota(
    plan: &KubeVirtResourcePlan,
    document: &Value,
    physical: u64,
    launcher: &LauncherQuota,
) -> Result<(), ProviderFailure> {
    let resource = KubeVirtResource {
        kind: "ResourceQuota".to_owned(),
        namespace: Some(plan.namespace.clone()),
        name: "runtime-quota".to_owned(),
        document: document.clone(),
    };
    validate_kubevirt_resource(plan, &resource)?;
    if document
        .pointer("/metadata/deletionTimestamp")
        .is_some_and(|value| !value.is_null())
        || !document
            .pointer("/spec/hard/requests.storage")
            .and_then(Value::as_str)
            .is_some_and(|value| storage_matches(value, physical))
        || [
            ("requests.memory", launcher.memory_request),
            ("limits.memory", launcher.memory_limit),
        ]
        .iter()
        .any(|(key, expected)| {
            !document
                .pointer(&format!("/spec/hard/{key}"))
                .and_then(Value::as_str)
                .is_some_and(|value| storage_matches(value, *expected))
        })
        || [
            ("requests.cpu", launcher.cpu_request_millicores),
            ("limits.cpu", launcher.cpu_limit_millicores),
        ]
        .iter()
        .any(|(key, expected)| {
            !document
                .pointer(&format!("/spec/hard/{key}"))
                .and_then(Value::as_str)
                .is_some_and(|value| cpu_matches(value, *expected))
        })
    {
        return Err(rejected());
    }
    Ok(())
}

fn secret_data_field(document: &Value, key: &str) -> Result<Vec<u8>, ProviderFailure> {
    let encoded = document
        .pointer(&format!("/data/{}", json_pointer_escape(key)))
        .and_then(Value::as_str)
        .ok_or_else(rejected)?;
    let data = BASE64_STANDARD.decode(encoded).map_err(|_| rejected())?;
    if data.is_empty() || data.len() > MAX_VGPU_BOOTSTRAP_SECRET_BYTES {
        return Err(rejected());
    }
    Ok(data)
}

fn json_pointer_escape(value: &str) -> String {
    value.replace('~', "~0").replace('/', "~1")
}

fn point_vm_to_private_cloud_init(resource: &mut KubeVirtResource) -> Result<(), ProviderFailure> {
    let volumes = resource
        .document
        .pointer_mut("/spec/template/spec/volumes")
        .and_then(Value::as_array_mut)
        .ok_or_else(rejected)?;
    let volume = volumes
        .iter_mut()
        .find(|volume| volume.get("name").and_then(Value::as_str) == Some("cloudinit"))
        .ok_or_else(rejected)?;
    let cloud_init = volume
        .get_mut("cloudInitNoCloud")
        .and_then(Value::as_object_mut)
        .ok_or_else(rejected)?;
    cloud_init["secretRef"]["name"] = json!(VM_VGPU_PRIVATE_CLOUD_INIT_SECRET);
    cloud_init["networkDataSecretRef"]["name"] = json!(VM_VGPU_PRIVATE_CLOUD_INIT_SECRET);
    Ok(())
}

fn render_vm_vgpu_cloud_init(
    base_userdata: &[u8],
    token: &[u8],
    tls_ca: &[u8],
    signing_root: Option<&[u8]>,
    licensing: &KubeVirtVmVgpuLicensingConfiguration,
) -> Result<String, ProviderFailure> {
    licensing.validate().map_err(|_| rejected())?;
    if token.is_empty() || token.len() > MAX_VGPU_BOOTSTRAP_SECRET_BYTES {
        return Err(rejected());
    }
    let base_userdata = std::str::from_utf8(base_userdata).map_err(|_| rejected())?;
    if !tls_ca
        .windows(b"-----BEGIN CERTIFICATE-----".len())
        .any(|window| window == b"-----BEGIN CERTIFICATE-----")
    {
        return Err(rejected());
    }
    if matches!(licensing.mode, KubeVirtVmVgpuLicenseMode::FastapiDls)
        && !signing_root.is_some_and(|value| {
            value
                .windows(b"-----BEGIN CERTIFICATE-----".len())
                .any(|window| window == b"-----BEGIN CERTIFICATE-----")
        })
    {
        return Err(rejected());
    }
    let token_b64 = BASE64_STANDARD.encode(token);
    let tls_ca_b64 = BASE64_STANDARD.encode(tls_ca);
    let gridd_config_b64 = BASE64_STANDARD.encode(format!(
        "FeatureType=1\nClientConfigTokenPath={}\n",
        KubeVirtVmVgpuLicensingConfiguration::token_directory_path()
    ));
    let mut write_files = format!(
        "  - path: /usr/local/share/ca-certificates/labweaver-vgpu-license.crt\n    owner: root:root\n    permissions: '0644'\n    encoding: b64\n    content: {tls_ca_b64}\n  - path: {gridd_config_path}\n    owner: root:root\n    permissions: '0644'\n    encoding: b64\n    content: {gridd_config_b64}\n  - path: {token_path}\n    owner: root:root\n    permissions: '0600'\n    encoding: b64\n    content: {token_b64}\n",
        gridd_config_path = KubeVirtVmVgpuLicensingConfiguration::gridd_config_path(),
        token_path = KubeVirtVmVgpuLicensingConfiguration::token_path(),
    );
    let patch_command = match licensing.mode {
        KubeVirtVmVgpuLicenseMode::NvidiaDls => String::new(),
        KubeVirtVmVgpuLicenseMode::FastapiDls => {
            let signing_root_b64 = BASE64_STANDARD.encode(signing_root.ok_or_else(rejected)?);
            let _ = write!(
                write_files,
                "  - path: {path}\n    owner: root:root\n    permissions: '0644'\n    encoding: b64\n    content: {signing_root_b64}\n",
                path = KubeVirtVmVgpuLicensingConfiguration::fastapi_signing_root_path(),
            );
            format!(
                "  - [{patcher}, -g, {gridd}, -c, {root}]\n",
                patcher = FASTAPI_DLS_GUEST_PATCHER_PATH,
                gridd = KubeVirtVmVgpuLicensingConfiguration::nvidia_gridd_path(),
                root = KubeVirtVmVgpuLicensingConfiguration::fastapi_signing_root_path(),
            )
        }
    };
    let marker = "\nruncmd:\n";
    let marker_index = base_userdata.find(marker).ok_or_else(rejected)?;
    let (before, after) = base_userdata.split_at(marker_index);
    let mut rendered = String::with_capacity(base_userdata.len() + write_files.len() + 256);
    rendered.push_str(before);
    rendered.push('\n');
    if !before.lines().any(|line| line.trim() == "write_files:") {
        rendered.push_str("write_files:\n");
    }
    rendered.push_str(&write_files);
    rendered.push_str(marker);
    rendered.push_str("  - [update-ca-certificates]\n");
    rendered.push_str(&patch_command);
    // The guest image may already have nvidia-gridd running before cloud-init
    // writes the deployment-owned token and trust roots.  Enabling alone does
    // not reload an active daemon, so always restart it after the private
    // bootstrap has been installed.
    rendered.push_str("  - [systemctl, enable, nvidia-gridd]\n");
    rendered.push_str("  - [systemctl, restart, nvidia-gridd]\n");
    rendered.push_str(&after[marker.len()..]);
    Ok(rendered)
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
                self.apply_plan(fence, plan).await.map(|observation| {
                    ContainerExecutorResponse::Observed {
                        plan_sha256: plan.plan_sha256,
                        observation,
                    }
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
        permit: &crate::KubeVirtExecutionPermit,
    ) -> KubeVirtExecutorResponse {
        let result = match request {
            KubeVirtExecutorRequest::Apply { plan } => async {
                self.apply_kubevirt_plan(plan, permit).await?;
                self.wait_kubevirt_running(fence, plan).await
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
                self.kubevirt_subresource(fence, plan, "start", permit)
                    .await?;
                self.wait_kubevirt_running(fence, plan).await
            }
            .await
            .map(|observation| KubeVirtExecutorResponse::Running {
                plan_sha256: plan.plan_sha256,
                observation,
            }),
            KubeVirtExecutorRequest::Stop { plan } => async {
                let identities = self.kubevirt_stop_identity(plan).await?;
                self.kubevirt_lifecycle_subresource(
                    fence,
                    &plan.namespace,
                    &plan.virtual_machine_name,
                    "stop",
                    permit,
                )
                .await?;
                loop {
                    match self.observe_kubevirt_stopped(fence, plan, identities).await {
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
                self.kubevirt_subresource(fence, plan, "restart", permit)
                    .await?;
                self.wait_kubevirt_running(fence, plan).await
            }
            .await
            .map(|observation| KubeVirtExecutorResponse::Running {
                plan_sha256: plan.plan_sha256,
                observation,
            }),
            KubeVirtExecutorRequest::DeleteNamespace { plan } => self
                .delete_kubevirt_namespace(fence, plan, permit)
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

fn validate_cleanup_plan(plan: &ContainerResourcePlan) -> Result<(), ProviderFailure> {
    if plan.namespace != format!("lw-env-{}", plan.environment_id)
        || !plan.image.is_empty()
        || !plan.resources.is_empty()
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
        || !valid_dns_label(&plan.base_disk_data_source_namespace)
        || !valid_dns_label(&plan.base_disk_data_source_name)
        || plan.base_disk_disk_sha256.len() != 64
        || !plan
            .base_disk_disk_sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        || plan.base_disk.validate().is_err()
        || plan.resources.iter().any(|resource| {
            resource.kind == "Secret" && resource.name == VM_VGPU_PRIVATE_CLOUD_INIT_SECRET
        })
    {
        return Err(rejected());
    }
    let virtual_machines = plan
        .resources
        .iter()
        .filter(|resource| resource.kind == "VirtualMachine")
        .collect::<Vec<_>>();
    if virtual_machines.len() != 1 || virtual_machines[0].name != plan.virtual_machine_name {
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

async fn check_kubevirt_permit(
    permit: &crate::KubeVirtExecutionPermit,
) -> Result<(), ProviderFailure> {
    permit
        .check()
        .await
        .map(|_| ())
        .map_err(|error| match error {
            crate::KubeVirtExecutorFenceError::DeadlineExceeded => ProviderFailure {
                code: ProviderFailureCode::Timeout,
                retryable: false,
            },
            crate::KubeVirtExecutorFenceError::Cancelled => ProviderFailure {
                code: ProviderFailureCode::Cancelled,
                retryable: false,
            },
            _ => unavailable(),
        })
}

fn validate_executor_startup(
    pod: &Value,
    namespace: &str,
    pod_name: &str,
    pod_uid: uuid::Uuid,
    container_name: &str,
    process_id: u32,
    boot_token: uuid::Uuid,
) -> Result<crate::KubeVirtExecutionInstance, ProviderFailure> {
    if process_id != 1
        || boot_token.is_nil()
        || !matches!(
            pod.pointer("/spec/hostPID"),
            None | Some(Value::Bool(false))
        )
        || !matches!(
            pod.pointer("/spec/shareProcessNamespace"),
            None | Some(Value::Bool(false))
        )
        || pointer_uuid(pod, "/metadata/uid")? != pod_uid
        || pod.pointer("/metadata/name").and_then(Value::as_str) != Some(pod_name)
        || pod.pointer("/metadata/namespace").and_then(Value::as_str) != Some(namespace)
    {
        return Err(rejected());
    }
    let containers = pod
        .pointer("/spec/containers")
        .and_then(Value::as_array)
        .ok_or_else(rejected)?;
    let container = containers
        .iter()
        .find(|container| container.get("name").and_then(Value::as_str) == Some(container_name))
        .ok_or_else(rejected)?;
    if container.get("args") != Some(&json!(["--mode", "kubevirt-executor"])) {
        return Err(rejected());
    }
    if container.get("command").is_some_and(|command| {
        command != &json!([]) && command != &json!(["/usr/local/bin/labweaver-service"])
    }) {
        return Err(rejected());
    }
    Ok(crate::KubeVirtExecutionInstance {
        namespace: namespace.to_owned(),
        pod_name: pod_name.to_owned(),
        pod_uid,
        container_name: container_name.to_owned(),
        boot_token,
    })
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
        "CiliumNetworkPolicy" => Ok(("/apis/cilium.io/v2", "ciliumnetworkpolicies", true)),
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

/// Maps one CDI import failure onto the closed Provider failure family. A drifted or
/// over-capacity base is permanent; an incomplete import is retryable within the reconcile budget.
fn cdi_import_failure(error: &CdiImportError) -> ProviderFailure {
    match error {
        CdiImportError::IdentityMismatch | CdiImportError::CapacityExceeded => rejected(),
        CdiImportError::ImportFailed => unavailable(),
    }
}

fn log_kubevirt_readiness_failure(
    fence: &KubeVirtBackendFence,
    started: std::time::Instant,
    failure: ProviderFailure,
    gate: &'static str,
    terminal: bool,
) {
    let remaining = (fence.deadline_at.get() - OffsetDateTime::now_utc())
        .whole_milliseconds()
        .max(0);
    tracing::warn!(
        event = "environment.kubevirt_executor.readiness_wait",
        environment_id = %fence.environment_id,
        operation_id = %fence.operation_id,
        provider_step = fence.provider_step,
        attempt = fence.attempt,
        request_id = %fence.request_id,
        generation = fence.environment_generation,
        diagnostic_code = failure.diagnostic_code(),
        readiness_gate = gate,
        retryable = failure.retryable,
        terminal,
        elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        remaining_ms = u64::try_from(remaining).unwrap_or(u64::MAX),
    );
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

fn workspace_claim_is_bound(claim: Option<&Value>) -> Result<bool, ProviderFailure> {
    let Some(claim) = claim else {
        return Ok(false);
    };
    let Some(phase) = claim.pointer("/status/phase") else {
        return Ok(false);
    };
    match phase.as_str().ok_or_else(invalid_observation)? {
        "Bound" => Ok(true),
        "Pending" => Ok(false),
        "Lost" => Err(rejected()),
        _ => Err(invalid_observation()),
    }
}

fn verify_owned_identity(
    value: &Value,
    name: &str,
    namespace: Option<&str>,
    environment_id: contracts::EnvironmentId,
    project_id: contracts::ProjectId,
) -> Result<(), ProviderFailure> {
    if value.pointer("/metadata/name").and_then(Value::as_str) != Some(name)
        || namespace.is_some_and(|namespace| {
            value.pointer("/metadata/namespace").and_then(Value::as_str) != Some(namespace)
        })
        || value
            .pointer("/metadata/labels/labweaver.io~1environment-id")
            .and_then(Value::as_str)
            != Some(&environment_id.to_string())
        || value
            .pointer("/metadata/labels/labweaver.io~1project-id")
            .and_then(Value::as_str)
            != Some(&project_id.to_string())
    {
        return Err(rejected());
    }
    Ok(())
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
    let deletion_started = namespace
        .pointer("/metadata/deletionTimestamp")
        .and_then(Value::as_str)
        .is_some();
    let Some(finalizers) = namespace
        .pointer("/metadata/finalizers")
        .and_then(Value::as_array)
    else {
        return if deletion_started {
            Ok(())
        } else {
            Err(rejected())
        };
    };
    if finalizers.iter().any(|value| {
        value.as_str().is_some_and(|name| {
            !matches!(
                name,
                "labweaver.io/environment-cleanup" | "finalizers.kubesphere.io/namespaces"
            )
        })
    }) {
        return Err(rejected());
    }
    Ok(())
}

#[cfg(test)]
mod namespace_identity_tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn namespace_identity_accepts_retained_kubesphere_finalizer() {
        let environment_id =
            contracts::EnvironmentId::from_str("019f8b1a-f95f-7551-a5ce-33f4c26466fd")
                .unwrap_or_else(|error| {
                    eprintln!("fixture environment id: {error}");
                    std::process::abort();
                });
        let namespace = json!({
            "metadata": {
                "name": format!("lw-env-{environment_id}"),
                "labels": {"labweaver.io/environment-id": environment_id.to_string()},
                "finalizers": [
                    "labweaver.io/environment-cleanup",
                    "finalizers.kubesphere.io/namespaces"
                ]
            }
        });
        assert!(
            verify_namespace_identity(
                &namespace,
                &format!("lw-env-{environment_id}"),
                environment_id
            )
            .is_ok()
        );
    }

    #[test]
    fn namespace_identity_rejects_unowned_finalizer() {
        let environment_id =
            contracts::EnvironmentId::from_str("019f8b1a-f95f-7551-a5ce-33f4c26466fd")
                .unwrap_or_else(|error| {
                    eprintln!("fixture environment id: {error}");
                    std::process::abort();
                });
        let namespace = json!({
            "metadata": {
                "name": format!("lw-env-{environment_id}"),
                "labels": {"labweaver.io/environment-id": environment_id.to_string()},
                "finalizers": ["unexpected.example/finalizer"]
            }
        });
        assert!(
            verify_namespace_identity(
                &namespace,
                &format!("lw-env-{environment_id}"),
                environment_id
            )
            .is_err()
        );
    }

    #[test]
    fn namespace_identity_accepts_removed_application_finalizer_during_deletion() {
        let environment_id =
            contracts::EnvironmentId::from_str("019f8b1a-f95f-7551-a5ce-33f4c26466fd")
                .unwrap_or_else(|error| {
                    eprintln!("fixture environment id: {error}");
                    std::process::abort();
                });
        let namespace = json!({
            "metadata": {
                "name": format!("lw-env-{environment_id}"),
                "labels": {"labweaver.io/environment-id": environment_id.to_string()},
                "deletionTimestamp": "2026-07-24T00:00:00Z"
            }
        });
        assert!(
            verify_namespace_identity(
                &namespace,
                &format!("lw-env-{environment_id}"),
                environment_id
            )
            .is_ok()
        );
    }
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
    let value = OffsetDateTime::now_utc();
    let value = value
        .replace_nanosecond((value.nanosecond() / 1_000_000) * 1_000_000)
        .map_err(|_| unavailable())?;
    UtcTimestamp::from_utc(value).map_err(|_| unavailable())
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
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use crate::{KubeVirtBaseDiskIdentity, ReconcileAction};
    use axum::{
        Router,
        body::{Body, to_bytes},
        extract::State,
        http::{Method, Request, StatusCode},
        response::{IntoResponse, Response},
    };
    use rcgen::generate_simple_self_signed;
    use tokio::net::TcpListener;

    #[test]
    fn executor_timestamp_is_normalized_to_contract_milliseconds() {
        let observed = timestamp().expect("current UTC time should normalize");
        assert_eq!(observed.get().nanosecond() % 1_000_000, 0);
    }

    #[test]
    fn missing_kubernetes_status_counter_is_pending_but_wrong_type_is_invalid() {
        let pending = json!({"metadata":{"generation":1},"spec":{"replicas":1}});
        assert_eq!(
            pointer_u64_or_zero(&pending, "/status/observedGeneration")
                .expect("missing status is a valid pending observation"),
            0
        );

        let invalid = json!({"status":{"observedGeneration":"one"}});
        assert!(pointer_u64_or_zero(&invalid, "/status/observedGeneration").is_err());
    }

    #[test]
    fn workspace_claim_status_is_only_bound_when_provider_reports_bound() {
        assert!(!workspace_claim_is_bound(None).expect("missing claim remains pending"));
        assert!(
            !workspace_claim_is_bound(Some(&json!({"metadata":{"name":"workspace"}})))
                .expect("missing phase remains pending")
        );
        assert!(
            !workspace_claim_is_bound(Some(&json!({"status":{"phase":"Pending"}})))
                .expect("pending phase remains pending")
        );
        assert!(
            workspace_claim_is_bound(Some(&json!({"status":{"phase":"Bound"}})))
                .expect("bound phase is accepted")
        );
        assert!(workspace_claim_is_bound(Some(&json!({"status":{"phase":"Lost"}}))).is_err());
        assert!(workspace_claim_is_bound(Some(&json!({"status":{"phase":1}}))).is_err());
    }

    fn vgpu_licensing(mode: KubeVirtVmVgpuLicenseMode) -> KubeVirtVmVgpuLicensingConfiguration {
        KubeVirtVmVgpuLicensingConfiguration {
            mode,
            license_url: "https://fastapi-dls.labweaver-gpu-license.svc.cluster.local/"
                .parse()
                .expect("fixture URL"),
            token_secret_ref: KubeVirtSecretRef {
                namespace: "labweaver-gpu-license".to_owned(),
                name: "fastapi-dls-client-token".to_owned(),
                key: "client-token".to_owned(),
            },
            tls_ca_secret_ref: KubeVirtSecretRef {
                namespace: "labweaver-gpu-license".to_owned(),
                name: "fastapi-dls-tls".to_owned(),
                key: "ca.crt".to_owned(),
            },
            fastapi_dls_signing_root_ca_secret_ref: (mode == KubeVirtVmVgpuLicenseMode::FastapiDls)
                .then(|| KubeVirtSecretRef {
                    namespace: "labweaver-gpu-license".to_owned(),
                    name: "fastapi-dls-signing-root".to_owned(),
                    key: "ca.crt".to_owned(),
                }),
        }
    }

    #[test]
    fn fastapi_vgpu_cloud_init_is_private_and_runs_the_fixed_guest_bootstrap() {
        let licensing = vgpu_licensing(KubeVirtVmVgpuLicenseMode::FastapiDls);
        let token = b"secret-client-token";
        let tls_ca = b"-----BEGIN CERTIFICATE-----\ntls\n-----END CERTIFICATE-----\n";
        let signing_root = b"-----BEGIN CERTIFICATE-----\nsigning\n-----END CERTIFICATE-----\n";
        let rendered = render_vm_vgpu_cloud_init(
            b"#cloud-config\nwrite_files:\n  - path: /etc/base\n    content: base\nruncmd:\n  - [true]\n",
            token,
            tls_ca,
            Some(signing_root),
            &licensing,
        )
        .expect("valid private bootstrap");

        assert!(rendered.contains("/usr/local/share/ca-certificates/labweaver-vgpu-license.crt"));
        assert!(rendered.contains(KubeVirtVmVgpuLicensingConfiguration::token_path()));
        assert!(rendered.contains(KubeVirtVmVgpuLicensingConfiguration::gridd_config_path()));
        assert!(rendered.contains(&BASE64_STANDARD.encode(format!(
            "FeatureType=1\nClientConfigTokenPath={}\n",
            KubeVirtVmVgpuLicensingConfiguration::token_directory_path()
        ))));
        assert!(
            rendered.contains(KubeVirtVmVgpuLicensingConfiguration::fastapi_signing_root_path())
        );
        assert!(rendered.contains("/usr/local/bin/gridd-unlock-patcher"));
        assert!(rendered.contains("/usr/bin/nvidia-gridd"));
        assert!(rendered.contains("[update-ca-certificates]"));
        assert!(rendered.contains("[systemctl, enable, nvidia-gridd]"));
        assert!(rendered.contains("[systemctl, restart, nvidia-gridd]"));
        assert!(rendered.contains(&BASE64_STANDARD.encode(token)));
        assert!(!rendered.contains(std::str::from_utf8(token).expect("fixture token")));
        assert!(rendered.contains("/etc/base"));
    }

    #[test]
    fn official_dls_cloud_init_does_not_run_fastapi_patcher() {
        let licensing = vgpu_licensing(KubeVirtVmVgpuLicenseMode::NvidiaDls);
        let rendered = render_vm_vgpu_cloud_init(
            b"#cloud-config\nwrite_files:\n  - path: /etc/base\n    content: base\nruncmd:\n",
            b"official-token",
            b"-----BEGIN CERTIFICATE-----\ntls\n-----END CERTIFICATE-----\n",
            None,
            &licensing,
        )
        .expect("valid official DLS bootstrap");

        assert!(rendered.contains(KubeVirtVmVgpuLicensingConfiguration::token_path()));
        assert!(!rendered.contains("gridd-unlock-patcher"));
        assert!(
            !rendered.contains(KubeVirtVmVgpuLicensingConfiguration::fastapi_signing_root_path())
        );
    }

    #[test]
    fn vgpu_private_cloud_init_rejects_missing_fastapi_signing_root() {
        let licensing = vgpu_licensing(KubeVirtVmVgpuLicenseMode::FastapiDls);
        assert!(
            render_vm_vgpu_cloud_init(
                b"#cloud-config\nwrite_files:\n  - path: /etc/base\n    content: base\nruncmd:\n",
                b"token",
                b"-----BEGIN CERTIFICATE-----\ntls\n-----END CERTIFICATE-----\n",
                None,
                &licensing,
            )
            .is_err()
        );
    }

    #[test]
    fn vm_vgpu_private_secret_repoints_both_cloud_init_documents() {
        let mut resource = KubeVirtResource {
            kind: "VirtualMachine".to_owned(),
            namespace: Some("lw-env-test".to_owned()),
            name: "runtime".to_owned(),
            document: json!({
                "spec": {"template": {"spec": {"volumes": [{
                    "name": "cloudinit",
                    "cloudInitNoCloud": {
                        "secretRef": {"name": "cloud-init"},
                        "networkDataSecretRef": {"name": "cloud-init"}
                    }
                }]}}}
            }),
        };

        point_vm_to_private_cloud_init(&mut resource).expect("cloud-init volume is valid");
        assert_eq!(
            resource
                .document
                .pointer("/spec/template/spec/volumes/0/cloudInitNoCloud/secretRef/name"),
            Some(&json!(VM_VGPU_PRIVATE_CLOUD_INIT_SECRET))
        );
        assert_eq!(
            resource.document.pointer(
                "/spec/template/spec/volumes/0/cloudInitNoCloud/networkDataSecretRef/name"
            ),
            Some(&json!(VM_VGPU_PRIVATE_CLOUD_INIT_SECRET))
        );
    }

    async fn test_kubevirt_executor(
        mock: &MockKubernetes,
    ) -> Result<(tempfile::TempDir, KubernetesContainerExecutor), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let token_file = directory.path().join("token");
        let ca_file = directory.path().join("ca.pem");
        let registry_file = directory.path().join("registry.json");
        std::fs::write(&token_file, "test-token\n")?;
        std::fs::write(&ca_file, mock.ca_pem.as_bytes())?;
        std::fs::write(
            &registry_file,
            br#"{"auths":{"registry.example":{"auth":"opaque"}}}"#,
        )?;
        let configuration = RuntimeExecutorConfiguration {
            api_server: mock.endpoint.clone(),
            bearer_token_file: token_file,
            cluster_ca_file: ca_file,
            request_timeout_milliseconds: 2_000,
            cleanup_poll_milliseconds: 1,
            cleanup_retention_seconds: 3_600,
            ssh_handshake_timeout_milliseconds: 1_000,
            registry_pull_secret_file: registry_file,
            registry_pull_secret_name: "registry-pull".to_owned(),
        };
        let objects = Arc::new(
            S3ImmutableObjectStore::new(
                artifact_store::S3StoreConfig {
                    binding: "test-store".to_owned(),
                    endpoint: "https://object-store.invalid".parse()?,
                    bucket: "test-bucket".to_owned(),
                    region: "test-region".to_owned(),
                    object_prefix: "test".to_owned(),
                    upload_ttl_seconds: 60,
                    max_object_bytes: 1_024,
                    force_path_style: true,
                    ca_bundle_file: None,
                },
                artifact_store::S3Credential {
                    access_key_id: "test-access".to_owned(),
                    secret_access_key: "test-secret".to_owned(),
                    session_token: None,
                },
            )
            .await?,
        );
        let executor = KubernetesContainerExecutor::new(configuration, objects)
            .map_err(|error| format!("executor configuration rejected: {error:?}"))?;
        Ok((directory, executor))
    }

    fn vm_vgpu_plan_for_test(
        environment_id: contracts::EnvironmentId,
        namespace: &str,
        licensing: KubeVirtVmVgpuLicensingConfiguration,
    ) -> KubeVirtResourcePlan {
        let labels = json!({"labweaver.io/environment-id": environment_id.to_string(), "labweaver.io/managed":"true"});
        let base_userdata =
            "#cloud-config\nwrite_files:\n  - path: /etc/base\n    content: base\nruncmd:\n";
        KubeVirtResourcePlan {
            environment_id,
            namespace: namespace.to_owned(),
            virtual_machine_name: "runtime".to_owned(),
            data_volume_name: "rootdisk".to_owned(),
            base_disk: contracts::supply_chain::VirtualMachineBaseDisk {
                binding: "ubuntu-vgpu".to_owned(),
                source_registry_digest:
                    "docker://registry.example/ubuntu@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                        .to_owned(),
                capacity_bytes: 1,
            },
            base_disk_format: contracts::supply_chain::VirtualMachineDiskFormat::Qcow2,
            base_disk_identity: KubeVirtBaseDiskIdentity::ReviewedDiskSha256,
            base_disk_data_source_namespace: "labweaver-system".to_owned(),
            base_disk_data_source_name: "ubuntu-vgpu".to_owned(),
            base_disk_disk_sha256:
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
            storage_class_name: "local-path".to_owned(),
            vm_vgpu_licensing: Some(licensing),
            // Deliberately put the namespace last.  The executor must establish
            // it before reading deployment Secrets or applying the VM's private
            // bootstrap, regardless of plan resource order.
            resources: vec![
                KubeVirtResource {
                    kind: "ResourceQuota".to_owned(), namespace: Some(namespace.to_owned()),
                    name: "runtime-quota".to_owned(),
                    document: json!({"apiVersion":"v1","kind":"ResourceQuota",
                        "metadata":{"name":"runtime-quota","namespace":namespace,"labels":labels,
                            "annotations":{"labweaver.io/cdi-scratch-storage-bytes":"17179869184",
                                "labweaver.io/vmi-memory-overhead-bytes":"536870912",
                                "labweaver.io/cdi-importer-memory-request-bytes":"262144000",
                                "labweaver.io/cdi-importer-memory-limit-bytes":"1073741824",
                                "labweaver.io/cdi-importer-cpu-request-millicores":"1000",
                                "labweaver.io/cdi-importer-cpu-limit-millicores":"4000"}},
                        "spec":{"hard":{"requests.storage":"34359738368","pods":"2",
                            "requests.memory":"2946498560","limits.memory":"3758096384",
                            "requests.cpu":"2","limits.cpu":"5"}}}),
                },
                KubeVirtResource {
                    kind: "DataVolume".to_owned(), namespace: Some(namespace.to_owned()),
                    name: "rootdisk".to_owned(),
                    document: json!({"apiVersion":"cdi.kubevirt.io/v1beta1","kind":"DataVolume",
                        "metadata":{"name":"rootdisk","namespace":namespace,"labels":labels},
                        "spec":{"sourceRef":{"kind":"DataSource","namespace":"labweaver-system","name":"ubuntu-vgpu"},
                            "storage":{"storageClassName":"local-path","volumeMode":"Filesystem","accessModes":["ReadWriteOnce"],
                                "resources":{"requests":{"storage":"17179869184"}}}}}),
                },
                KubeVirtResource {
                    kind: "Secret".to_owned(),
                    namespace: Some(namespace.to_owned()),
                    name: "cloud-init".to_owned(),
                    document: json!({
                        "apiVersion":"v1",
                        "kind":"Secret",
                        "metadata":{"name":"cloud-init","namespace":namespace,"labels":labels},
                        "data":{
                            "userdata":BASE64_STANDARD.encode(base_userdata),
                            "networkdata":BASE64_STANDARD.encode("version: 2\n")
                        }
                    }),
                },
                KubeVirtResource {
                    kind: "VirtualMachine".to_owned(),
                    namespace: Some(namespace.to_owned()),
                    name: "runtime".to_owned(),
                    document: json!({
                        "apiVersion":"kubevirt.io/v1",
                        "kind":"VirtualMachine",
                        "metadata":{"name":"runtime","namespace":namespace,"labels":labels},
                        "spec":{"template":{"spec":{
                            "architecture":"amd64",
                            "domain":{"resources":{"requests":{"memory":"2147483648","cpu":"1"},
                                "limits":{"memory":"2684354560","cpu":"1"}},
                                "devices":{"gpus":[{"name":"gpu","deviceName":"nvidia.com/GRID_V100DX-2Q"}],
                                    "interfaces":[{"name":"default","masquerade":{}}]}},
                            "volumes":[{"name":"cloudinit","cloudInitNoCloud":{
                                "secretRef":{"name":"cloud-init"},
                                "networkDataSecretRef":{"name":"cloud-init"}
                            }}]
                        }}}
                    }),
                },
                KubeVirtResource {
                    kind: "Namespace".to_owned(),
                    namespace: None,
                    name: namespace.to_owned(),
                    document: json!({
                        "apiVersion":"v1",
                        "kind":"Namespace",
                        "metadata":{"name":namespace,"labels":labels}
                    }),
                },
            ],
            plan_sha256: Sha256Digest::of_bytes(b"vm-vgpu-private-bootstrap"),
        }
    }

    #[tokio::test]
    async fn stale_kubevirt_permit_rejects_tenant_apply_before_any_http_mutation()
    -> Result<(), Box<dyn std::error::Error>> {
        let mock = spawn_mock_kubernetes().await?;
        let (_files, executor) = test_kubevirt_executor(&mock).await?;
        let environment_id = contracts::EnvironmentId::new();
        let plan = vm_vgpu_plan_for_test(
            environment_id,
            &format!("lw-env-{environment_id}"),
            vgpu_licensing(KubeVirtVmVgpuLicenseMode::FastapiDls),
        );
        let mut fence = stop_fence(environment_id);
        let (_database, permit) = crate::kubevirt_execution::test_permit(&mut fence).await?;
        sqlx::query("UPDATE environment.environment_instances SET contract=jsonb_set(contract,'{operation}','null'::jsonb) WHERE environment_id=$1").bind(environment_id.as_uuid()).execute(&permit.pool).await?;
        assert!(matches!(
            executor.apply_kubevirt_plan(&plan, &permit).await,
            Err(ProviderFailure {
                code: ProviderFailureCode::Cancelled,
                ..
            })
        ));
        assert!(mock.events.lock().await.is_empty());
        Ok(())
    }

    #[test]
    fn executor_startup_requires_actual_pid1_and_isolated_matching_pod() {
        let uid = uuid::Uuid::new_v4();
        let token = uuid::Uuid::new_v4();
        let base = json!({"metadata":{"namespace":"system","name":"executor","uid":uid},"spec":{"containers":[{"name":"kubevirt-executor","args":["--mode","kubevirt-executor"],"command":["/usr/local/bin/labweaver-service"]}]}});
        for command in [None, Some(json!([]))] {
            let mut pod = base.clone();
            if let Some(command) = command {
                pod["spec"]["containers"][0]["command"] = command;
            } else {
                pod["spec"]["containers"][0]
                    .as_object_mut()
                    .expect("container fixture")
                    .remove("command");
            }
            assert!(
                validate_executor_startup(
                    &pod,
                    "system",
                    "executor",
                    uid,
                    "kubevirt-executor",
                    1,
                    token
                )
                .is_ok()
            );
        }
        assert!(
            validate_executor_startup(
                &base,
                "system",
                "executor",
                uid,
                "kubevirt-executor",
                1,
                token
            )
            .is_ok()
        );
        assert!(
            validate_executor_startup(
                &base,
                "system",
                "executor",
                uid,
                "kubevirt-executor",
                2,
                token
            )
            .is_err()
        );
        for field in ["hostPID", "shareProcessNamespace"] {
            let mut pod = base.clone();
            pod["spec"][field] = json!(true);
            assert!(
                validate_executor_startup(
                    &pod,
                    "system",
                    "executor",
                    uid,
                    "kubevirt-executor",
                    1,
                    token
                )
                .is_err()
            );
        }
        let mut wrong = base.clone();
        wrong["spec"]["containers"][0]["args"] = json!(["--mode", "container-executor"]);
        assert!(
            validate_executor_startup(
                &wrong,
                "system",
                "executor",
                uid,
                "kubevirt-executor",
                1,
                token
            )
            .is_err()
        );
        assert!(
            validate_executor_startup(
                &base,
                "system",
                "executor",
                uuid::Uuid::new_v4(),
                "kubevirt-executor",
                1,
                token
            )
            .is_err()
        );
    }

    #[tokio::test]
    async fn kubevirt_quota_preserves_intent_and_accounts_launcher_and_storage()
    -> Result<(), Box<dyn std::error::Error>> {
        let mock = spawn_mock_kubernetes().await?;
        let (_files, executor) = test_kubevirt_executor(&mock).await?;
        let environment_id = contracts::EnvironmentId::new();
        let plan = vm_vgpu_plan_for_test(
            environment_id,
            &format!("lw-env-{environment_id}"),
            vgpu_licensing(KubeVirtVmVgpuLicenseMode::FastapiDls),
        );
        let original = serde_json::to_value(&plan)?;
        let quota = executor
            .kubevirt_quota(&plan)
            .await
            .map_err(|error| format!("storage quota rejected: {error:?}"))?;
        let mut expected = kubevirt_storage_intent(&plan)
            .map_err(|error| format!("intent: {error:?}"))?
            .0
            .document
            .clone();
        expected["spec"]["hard"]["requests.storage"] = json!("36422329304");
        expected["spec"]["hard"]["requests.memory"] = json!("3803582144");
        expected["spec"]["hard"]["limits.memory"] = json!("5177050880");
        expected["spec"]["hard"]["requests.cpu"] = json!("2005m");
        expected["spec"]["hard"]["limits.cpu"] = json!("5015m");
        assert_eq!(quota.document, expected);
        assert_eq!(
            serde_json::to_value(&plan)?,
            original,
            "logical intent and canonical hash must remain unchanged"
        );
        assert!(
            mock.events
                .lock()
                .await
                .iter()
                .all(|event| event.starts_with("GET "))
        );
        Ok(())
    }

    #[tokio::test]
    async fn kubevirt_storage_quota_uses_effective_scratch_class_and_alignment()
    -> Result<(), Box<dyn std::error::Error>> {
        let mock = spawn_mock_kubernetes().await?;
        let (_files, executor) = test_kubevirt_executor(&mock).await?;
        let id = contracts::EnvironmentId::new();
        let mut plan = vm_vgpu_plan_for_test(
            id,
            &format!("lw-env-{id}"),
            vgpu_licensing(KubeVirtVmVgpuLicenseMode::FastapiDls),
        );
        mock.get_responses.lock().await.extend([
            ("/apis/cdi.kubevirt.io/v1beta1/cdiconfigs/config".to_owned(), Some(json!({"status":{
                "scratchSpaceStorageClass":"scratch-class","filesystemOverhead":{"global":"0.06","storageClass":{"scratch-class":"0"}}}}))),
            ("/apis/storage.k8s.io/v1/storageclasses/scratch-class".to_owned(), Some(json!({"metadata":{"name":"scratch-class"},"provisioner":"example/scratch"}))),
        ]);
        let quota = executor
            .kubevirt_quota(&plan)
            .await
            .map_err(|error| format!("quota: {error:?}"))?;
        assert_eq!(
            quota.document.pointer("/spec/hard/requests.storage"),
            Some(&json!("35390530520"))
        );
        mock.get_responses.lock().await.clear();
        for resource in &mut plan.resources {
            if resource.kind == "DataVolume" {
                resource.document["spec"]["storage"]["resources"]["requests"]["storage"] =
                    json!("1048577");
            } else if resource.kind == "ResourceQuota" {
                resource.document["metadata"]["annotations"]["labweaver.io/cdi-scratch-storage-bytes"] =
                    json!("1048577");
                resource.document["spec"]["hard"]["requests.storage"] = json!("2097154");
            }
        }
        let quota = executor
            .kubevirt_quota(&plan)
            .await
            .map_err(|error| format!("aligned quota: {error:?}"))?;
        assert_eq!(
            quota.document.pointer("/spec/hard/requests.storage"),
            Some(&json!("5368710"))
        );
        Ok(())
    }

    #[tokio::test]
    async fn kubevirt_storage_quota_rejects_unknown_or_invalid_effective_configuration()
    -> Result<(), Box<dyn std::error::Error>> {
        let mock = spawn_mock_kubernetes().await?;
        let (_files, executor) = test_kubevirt_executor(&mock).await?;
        let id = contracts::EnvironmentId::new();
        let plan = vm_vgpu_plan_for_test(
            id,
            &format!("lw-env-{id}"),
            vgpu_licensing(KubeVirtVmVgpuLicenseMode::FastapiDls),
        );
        let config_path = "/apis/cdi.kubevirt.io/v1beta1/cdiconfigs/config";
        let profile_path = "/apis/cdi.kubevirt.io/v1beta1/storageprofiles/local-path";
        for (path, response) in [
            (config_path, None),
            (
                config_path,
                Some(json!({"status":{"filesystemOverhead":{"global":"NaN"}}})),
            ),
            (
                config_path,
                Some(json!({"status":{"filesystemOverhead":{"global":"-0.06"}}})),
            ),
            (
                config_path,
                Some(
                    json!({"status":{"scratchSpaceStorageClass":"missing","filesystemOverhead":{"global":"0.06"}}}),
                ),
            ),
            ("/apis/storage.k8s.io/v1/storageclasses/local-path", None),
            (profile_path, None),
            (
                profile_path,
                Some(
                    json!({"metadata":{"name":"local-path","annotations":{"cdi.kubevirt.io/minimumSupportedPvcSize":"1Gi"}},"status":{"claimPropertySets":[{"accessModes":["ReadWriteOnce"],"volumeMode":"Filesystem"}]}}),
                ),
            ),
            (
                profile_path,
                Some(json!({"metadata":{"name":"wrong-class"}})),
            ),
        ] {
            mock.get_responses.lock().await.clear();
            mock.get_responses
                .lock()
                .await
                .insert(path.to_owned(), response);
            assert!(
                executor.kubevirt_quota(&plan).await.is_err(),
                "must reject {path}"
            );
        }
        assert!(
            mock.events
                .lock()
                .await
                .iter()
                .all(|event| event.starts_with("GET "))
        );
        Ok(())
    }

    #[tokio::test]
    async fn kubevirt_storage_quota_rejects_invalid_logical_intent_before_api_reads()
    -> Result<(), Box<dyn std::error::Error>> {
        let mock = spawn_mock_kubernetes().await?;
        let (_files, executor) = test_kubevirt_executor(&mock).await?;
        let id = contracts::EnvironmentId::new();
        let original = vm_vgpu_plan_for_test(
            id,
            &format!("lw-env-{id}"),
            vgpu_licensing(KubeVirtVmVgpuLicenseMode::FastapiDls),
        );
        for (kind, pointer, value) in [
            (
                "DataVolume",
                "/spec/storage/resources/requests/storage",
                json!("0"),
            ),
            ("DataVolume", "/spec/storage/volumeMode", json!("Block")),
            (
                "ResourceQuota",
                "/metadata/annotations/labweaver.io~1cdi-scratch-storage-bytes",
                json!("0"),
            ),
            (
                "ResourceQuota",
                "/spec/hard/requests.storage",
                json!("18446744073709551615"),
            ),
        ] {
            let mut plan = original.clone();
            let resource = plan
                .resources
                .iter_mut()
                .find(|resource| resource.kind == kind)
                .ok_or("resource missing")?;
            if pointer.ends_with("volumeMode") {
                resource.document["spec"]["storage"]["volumeMode"] = value;
            } else {
                *resource
                    .document
                    .pointer_mut(pointer)
                    .ok_or("intent field missing")? = value;
            }
            assert!(executor.kubevirt_quota(&plan).await.is_err());
        }
        let mut overflow = original.clone();
        for resource in &mut overflow.resources {
            if resource.kind == "DataVolume" {
                resource.document["spec"]["storage"]["resources"]["requests"]["storage"] =
                    json!(u64::MAX.to_string());
            } else if resource.kind == "ResourceQuota" {
                resource.document["metadata"]["annotations"]["labweaver.io/cdi-scratch-storage-bytes"] =
                    json!(u64::MAX.to_string());
            }
        }
        assert!(executor.kubevirt_quota(&overflow).await.is_err());
        assert!(mock.events.lock().await.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn kubevirt_storage_quota_keeps_existing_disk_and_quota_exact()
    -> Result<(), Box<dyn std::error::Error>> {
        let mock = spawn_mock_kubernetes().await?;
        let (_files, executor) = test_kubevirt_executor(&mock).await?;
        let id = contracts::EnvironmentId::new();
        let namespace = format!("lw-env-{id}");
        let plan = vm_vgpu_plan_for_test(
            id,
            &namespace,
            vgpu_licensing(KubeVirtVmVgpuLicenseMode::FastapiDls),
        );
        let pvc = install_existing_kubevirt_storage(&mock, &plan).await?;
        let pvc_path = format!("/api/v1/namespaces/{namespace}/persistentvolumeclaims/rootdisk");
        assert!(
            executor.kubevirt_quota(&plan).await.is_ok(),
            "Pending exact-spec PVC is not a sizing failure"
        );
        for (pointer, value) in [
            ("/spec/resources/requests/storage", json!("17179869184")),
            (
                "/metadata/ownerReferences/0/uid",
                json!(uuid::Uuid::new_v4()),
            ),
            ("/status/phase", json!("Bound")),
        ] {
            let mut changed = pvc.clone();
            *changed.pointer_mut(pointer).ok_or("PVC field missing")? = value;
            mock.get_responses
                .lock()
                .await
                .insert(pvc_path.clone(), Some(changed));
            assert!(
                executor.kubevirt_quota(&plan).await.is_err(),
                "must reject {pointer}"
            );
        }
        let mut changed = pvc;
        changed["status"]["capacity"] = json!({"storage":"17179869184"});
        mock.get_responses
            .lock()
            .await
            .insert(pvc_path.clone(), Some(changed));
        assert!(
            executor.kubevirt_quota(&plan).await.is_err(),
            "mismatched observed capacity must not be ignored while Pending"
        );
        let mut bound = mock
            .get_responses
            .lock()
            .await
            .get(&pvc_path)
            .and_then(Clone::clone)
            .ok_or("PVC fixture missing")?;
        bound["status"]["phase"] = json!("Bound");
        bound["status"]["capacity"] = json!({"storage":"18210661336"});
        mock.get_responses
            .lock()
            .await
            .insert(pvc_path.clone(), Some(bound));
        assert!(
            executor.kubevirt_quota(&plan).await.is_ok(),
            "Bound exact capacity is reusable"
        );
        let quota_path = format!("/api/v1/namespaces/{namespace}/resourcequotas/runtime-quota");
        let mut changed_quota = mock
            .get_responses
            .lock()
            .await
            .get(&quota_path)
            .and_then(Clone::clone)
            .ok_or("quota fixture missing")?;
        changed_quota["spec"]["hard"]["requests.storage"] = json!("34359738368");
        mock.get_responses
            .lock()
            .await
            .insert(quota_path, Some(changed_quota));
        assert!(
            executor.kubevirt_quota(&plan).await.is_err(),
            "an existing quota must not be enlarged on replay"
        );
        Ok(())
    }

    #[tokio::test]
    async fn kubevirt_quota_rejects_unknown_authority_and_read_failures()
    -> Result<(), Box<dyn std::error::Error>> {
        let mock = spawn_mock_kubernetes().await?;
        let (_files, executor) = test_kubevirt_executor(&mock).await?;
        let id = contracts::EnvironmentId::new();
        let plan = vm_vgpu_plan_for_test(
            id,
            &format!("lw-env-{id}"),
            vgpu_licensing(KubeVirtVmVgpuLicenseMode::FastapiDls),
        );
        let instance = json!({"spec":{"configuration":{}},
            "status":{"observedKubeVirtVersion":"v1.8.4","targetKubeVirtVersion":"v1.8.4"}});
        for response in [
            None,
            Some(json!({"items":[]})),
            Some(json!({"items":[instance.clone(),instance.clone()]})),
            Some(json!({"items":[instance.clone()],"metadata":{"continue":"next"}})),
            Some(json!({"items":[{"spec":{"configuration":{}},
                "status":{"observedKubeVirtVersion":"v1.8.4","targetKubeVirtVersion":"v1.8.5"}}]})),
            Some(
                json!({"items":[{"status":{"observedKubeVirtVersion":"v1.8.4",
                "targetKubeVirtVersion":"v1.8.4"}}]}),
            ),
        ] {
            mock.events.lock().await.clear();
            mock.get_responses
                .lock()
                .await
                .insert("/apis/kubevirt.io/v1/kubevirts".to_owned(), response);
            assert!(executor.kubevirt_quota(&plan).await.is_err());
            assert_eq!(
                *mock.events.lock().await,
                ["GET /apis/kubevirt.io/v1/kubevirts?limit=2"]
            );
        }
        for (status, delay) in [
            (StatusCode::FORBIDDEN, Duration::ZERO),
            (StatusCode::OK, Duration::from_millis(2500)),
        ] {
            *mock.kubevirt_read.lock().await = Some((status, delay));
            mock.events.lock().await.clear();
            assert!(executor.kubevirt_quota(&plan).await.is_err());
            assert_eq!(
                *mock.events.lock().await,
                ["GET /apis/kubevirt.io/v1/kubevirts?limit=2"]
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn kubevirt_quota_rejects_each_existing_memory_and_cpu_drift()
    -> Result<(), Box<dyn std::error::Error>> {
        let mock = spawn_mock_kubernetes().await?;
        let (_files, executor) = test_kubevirt_executor(&mock).await?;
        let id = contracts::EnvironmentId::new();
        let namespace = format!("lw-env-{id}");
        let plan = vm_vgpu_plan_for_test(
            id,
            &namespace,
            vgpu_licensing(KubeVirtVmVgpuLicenseMode::FastapiDls),
        );
        install_existing_kubevirt_storage(&mock, &plan).await?;
        let path = format!("/api/v1/namespaces/{namespace}/resourcequotas/runtime-quota");
        let original = mock
            .get_responses
            .lock()
            .await
            .get(&path)
            .and_then(Clone::clone)
            .ok_or("quota fixture missing")?;
        for key in [
            "requests.memory",
            "limits.memory",
            "requests.cpu",
            "limits.cpu",
        ] {
            let mut changed = original.clone();
            changed["spec"]["hard"][key] = json!("1");
            mock.get_responses
                .lock()
                .await
                .insert(path.clone(), Some(changed));
            assert!(
                executor.kubevirt_quota(&plan).await.is_err(),
                "must reject {key} drift"
            );
        }
        mock.get_responses.lock().await.insert(path, Some(original));
        assert!(executor.kubevirt_quota(&plan).await.is_ok());
        assert!(
            mock.events
                .lock()
                .await
                .iter()
                .all(|event| event.starts_with("GET "))
        );
        Ok(())
    }

    async fn install_existing_kubevirt_storage(
        mock: &MockKubernetes,
        plan: &KubeVirtResourcePlan,
    ) -> Result<Value, Box<dyn std::error::Error>> {
        let namespace = &plan.namespace;
        let (quota, disk, _, _) =
            kubevirt_storage_intent(plan).map_err(|error| format!("intent: {error:?}"))?;
        let uid = uuid::Uuid::new_v4();
        let mut existing_disk = disk.document.clone();
        existing_disk["metadata"]["uid"] = json!(uid);
        existing_disk["spec"]["storage"]["resources"]["requests"]["storage"] = json!("16Gi");
        let pvc = json!({"metadata":{"name":"rootdisk","namespace":namespace,
            "ownerReferences":[{"controller":true,"apiVersion":"cdi.kubevirt.io/v1beta1","kind":"DataVolume","name":"rootdisk","uid":uid}]},
            "spec":{"storageClassName":"local-path","volumeMode":"Filesystem","resources":{"requests":{"storage":"18210661336"}}},
            "status":{"phase":"Pending"}});
        let mut existing_quota = quota.document.clone();
        existing_quota["spec"]["hard"]["requests.storage"] = json!("36422329304");
        existing_quota["spec"]["hard"]["requests.memory"] = json!("3803582144");
        existing_quota["spec"]["hard"]["limits.memory"] = json!("5177050880");
        existing_quota["spec"]["hard"]["requests.cpu"] = json!("2005m");
        existing_quota["spec"]["hard"]["limits.cpu"] = json!("5015m");
        let pvc_path = format!("/api/v1/namespaces/{namespace}/persistentvolumeclaims/rootdisk");
        mock.get_responses.lock().await.extend([
            (pvc_path, Some(pvc.clone())),
            (
                format!(
                    "/apis/cdi.kubevirt.io/v1beta1/namespaces/{namespace}/datavolumes/rootdisk"
                ),
                Some(existing_disk),
            ),
            (
                format!("/api/v1/namespaces/{namespace}/resourcequotas/runtime-quota"),
                Some(existing_quota),
            ),
        ]);
        Ok(pvc)
    }

    #[tokio::test]
    async fn vm_vgpu_apply_reads_named_secrets_and_only_applies_private_bootstrap()
    -> Result<(), Box<dyn std::error::Error>> {
        let mock = spawn_mock_kubernetes().await?;
        let (_files, executor) = test_kubevirt_executor(&mock).await?;
        let environment_id = contracts::EnvironmentId::new();
        let namespace = format!("lw-env-{environment_id}");
        let licensing = vgpu_licensing(KubeVirtVmVgpuLicenseMode::FastapiDls);
        let plan = vm_vgpu_plan_for_test(environment_id, &namespace, licensing);

        let mut fence = stop_fence(environment_id);
        let (_database, permit) = crate::kubevirt_execution::test_permit(&mut fence).await?;
        executor
            .apply_kubevirt_plan(&plan, &permit)
            .await
            .map_err(|error| format!("vGPU plan apply rejected: {error:?}"))?;
        let quota = mock
            .applied_quota
            .lock()
            .await
            .clone()
            .ok_or("quota was not applied")?;
        assert_eq!(
            quota.pointer("/spec/hard/requests.storage"),
            Some(&json!("36422329304"))
        );
        assert_eq!(
            kubevirt_storage_intent(&plan)
                .map_err(|error| format!("intent: {error:?}"))?
                .0
                .document
                .pointer("/spec/hard/requests.storage"),
            Some(&json!("34359738368"))
        );
        let private_secret = mock
            .applied_private_secret
            .lock()
            .await
            .clone()
            .ok_or("private bootstrap Secret was not applied")?;
        assert_eq!(
            private_secret.pointer("/metadata/name"),
            Some(&json!("vm-vgpu-cloud-init"))
        );
        let private_userdata = BASE64_STANDARD.decode(
            private_secret
                .pointer("/data/userdata")
                .and_then(Value::as_str)
                .ok_or("private userdata missing")?,
        )?;
        let private_userdata = String::from_utf8(private_userdata)?;
        assert!(private_userdata.contains(&BASE64_STANDARD.encode("secret-client-token")));
        assert!(!private_userdata.contains("secret-client-token"));
        assert!(!serde_json::to_string(&plan)?.contains("secret-client-token"));
        let events = mock.events.lock().await.clone();
        let namespace_apply = events
            .iter()
            .position(|event| event == &format!("PATCH /api/v1/namespaces/{namespace}"))
            .ok_or("environment Namespace was not applied")?;
        let private_secret_apply = events
            .iter()
            .position(|event| {
                event == &format!(
                    "PATCH /api/v1/namespaces/{namespace}/secrets/{VM_VGPU_PRIVATE_CLOUD_INIT_SECRET}"
                )
            })
            .ok_or("private bootstrap Secret was not applied")?;
        let vm_apply = events
            .iter()
            .position(|event| {
                event
                    == &format!(
                        "PATCH /apis/kubevirt.io/v1/namespaces/{namespace}/virtualmachines/runtime"
                    )
            })
            .ok_or("VirtualMachine was not applied")?;
        assert!(
            namespace_apply < private_secret_apply && private_secret_apply < vm_apply,
            "namespace/private Secret/VM apply ordering was not preserved: {events:?}"
        );
        assert!(
            events
                .iter()
                .any(|event| { event.ends_with("/secrets/fastapi-dls-client-token") })
        );
        assert!(
            events
                .iter()
                .any(|event| event.ends_with("/secrets/fastapi-dls-tls"))
        );
        assert!(
            events
                .iter()
                .any(|event| event.ends_with("/secrets/fastapi-dls-signing-root"))
        );
        Ok(())
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn apply_waits_for_workspace_claim_after_deployment_apply()
    -> Result<(), Box<dyn std::error::Error>> {
        let mock = spawn_mock_kubernetes().await?;
        let directory = tempfile::tempdir()?;
        let token_file = directory.path().join("token");
        let ca_file = directory.path().join("ca.pem");
        let registry_file = directory.path().join("registry.json");
        std::fs::write(&token_file, "test-token\n")?;
        std::fs::write(&ca_file, mock.ca_pem.as_bytes())?;
        std::fs::write(
            &registry_file,
            br#"{"auths":{"registry.example":{"auth":"opaque"}}}"#,
        )?;

        let configuration = RuntimeExecutorConfiguration {
            api_server: mock.endpoint.clone(),
            bearer_token_file: token_file,
            cluster_ca_file: ca_file,
            request_timeout_milliseconds: 2_000,
            cleanup_poll_milliseconds: 1,
            cleanup_retention_seconds: 3_600,
            ssh_handshake_timeout_milliseconds: 1_000,
            registry_pull_secret_file: registry_file,
            registry_pull_secret_name: "registry-pull".to_owned(),
        };
        let executor = KubernetesContainerExecutor::new(
            configuration.clone(),
            Arc::new(
                S3ImmutableObjectStore::new(
                    artifact_store::S3StoreConfig {
                        binding: "test-store".to_owned(),
                        endpoint: "https://object-store.invalid".parse()?,
                        bucket: "test-bucket".to_owned(),
                        region: "test-region".to_owned(),
                        object_prefix: "test".to_owned(),
                        upload_ttl_seconds: 60,
                        max_object_bytes: 1_024,
                        force_path_style: true,
                        ca_bundle_file: None,
                    },
                    artifact_store::S3Credential {
                        access_key_id: "test-access".to_owned(),
                        secret_access_key: "test-secret".to_owned(),
                        session_token: None,
                    },
                )
                .await?,
            ),
        )
        .map_err(|error| format!("executor configuration rejected: {error:?}"))?;

        let environment_id = contracts::EnvironmentId::new();
        let namespace = format!("lw-env-{environment_id}");
        let labels = json!({
            "labweaver.io/environment-id": environment_id.to_string()
        });
        let plan = ContainerResourcePlan {
            environment_id,
            project_id: contracts::ProjectId::new(),
            namespace: namespace.clone(),
            image: "registry.example/labweaver/test:latest".to_owned(),
            resources: vec![
                ContainerResource {
                    kind: "Namespace".to_owned(),
                    namespace: None,
                    name: namespace.clone(),
                    document: json!({
                        "apiVersion":"v1",
                        "kind":"Namespace",
                        "metadata":{"name":namespace,"labels":labels}
                    }),
                },
                ContainerResource {
                    kind: "PersistentVolumeClaim".to_owned(),
                    namespace: Some(namespace.clone()),
                    name: "workspace".to_owned(),
                    document: json!({
                        "apiVersion":"v1",
                        "kind":"PersistentVolumeClaim",
                        "metadata":{"name":"workspace","namespace":namespace,"labels":labels},
                        "spec":{"resources":{"requests":{"storage":"1Gi"}}}
                    }),
                },
                ContainerResource {
                    kind: "Deployment".to_owned(),
                    namespace: Some(namespace.clone()),
                    name: "runtime".to_owned(),
                    document: json!({
                        "apiVersion":"apps/v1",
                        "kind":"Deployment",
                        "metadata":{"name":"runtime","namespace":namespace,"labels":labels},
                        "spec":{"replicas":1}
                    }),
                },
            ],
            plan_sha256: Sha256Digest::of_bytes(b"runtime-plan"),
        };
        let fence = ContainerBackendFence {
            protocol_version: crate::CONTAINER_BACKEND_PROTOCOL_VERSION,
            environment_id,
            operation_id: contracts::OperationId::new(),
            provider_step: 1,
            operation_generation: 1,
            attempt: 1,
            action: ReconcileAction::Provision,
            request_id: Sha256Digest::of_bytes(b"runtime-request"),
            trace_id: "runtime-test".to_owned(),
            deadline_at: {
                let now = OffsetDateTime::now_utc();
                let now = now.replace_nanosecond((now.nanosecond() / 1_000_000) * 1_000_000)?;
                UtcTimestamp::from_utc(now + time::Duration::seconds(5))?
            },
        };

        let observation = executor.apply_plan(&fence, &plan).await;
        assert!(
            observation.is_ok(),
            "apply should complete: {observation:?}"
        );
        assert!(observation.expect("apply succeeded").ready);

        let events = mock.events.lock().await.clone();
        let deployment_path = format!("/apis/apps/v1/namespaces/{namespace}/deployments/runtime");
        let pvc_path = format!("/api/v1/namespaces/{namespace}/persistentvolumeclaims/workspace");
        let deployment_apply_event = events
            .iter()
            .position(|event| event == &format!("PATCH {deployment_path}"))
            .expect("deployment must be applied");
        let pvc_read = events
            .iter()
            .position(|event| event == &format!("GET {pvc_path} Bound"))
            .expect("PVC must be observed as bound");
        assert!(
            deployment_apply_event < pvc_read,
            "PVC observation must wait until deployment apply: {events:?}"
        );
        Ok(())
    }

    #[derive(Clone)]
    struct MockKubernetesState {
        deployment_applied: Arc<std::sync::atomic::AtomicBool>,
        events: Arc<Mutex<Vec<String>>>,
        applied_private_secret: Arc<Mutex<Option<Value>>>,
        applied_quota: Arc<Mutex<Option<Value>>>,
        get_responses: Arc<Mutex<std::collections::BTreeMap<String, Option<Value>>>>,
        lifecycle: Option<Arc<Mutex<LifecycleKubernetes>>>,
        kubevirt_read: Arc<Mutex<Option<(StatusCode, Duration)>>>,
    }

    struct MockKubernetes {
        endpoint: Url,
        ca_pem: String,
        events: Arc<Mutex<Vec<String>>>,
        applied_private_secret: Arc<Mutex<Option<Value>>>,
        applied_quota: Arc<Mutex<Option<Value>>>,
        get_responses: Arc<Mutex<std::collections::BTreeMap<String, Option<Value>>>>,
        task: tokio::task::JoinHandle<()>,
        kubevirt_read: Arc<Mutex<Option<(StatusCode, Duration)>>>,
    }

    impl Drop for MockKubernetes {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    async fn spawn_mock_kubernetes() -> Result<MockKubernetes, Box<dyn std::error::Error>> {
        spawn_lifecycle_kubernetes(None).await
    }

    async fn spawn_lifecycle_kubernetes(
        lifecycle: Option<LifecycleKubernetes>,
    ) -> Result<MockKubernetes, Box<dyn std::error::Error>> {
        let certificate = generate_simple_self_signed(vec!["localhost".to_owned()])?;
        let ca_pem = certificate.cert.pem();
        let private_key_pem = certificate.signing_key.serialize_pem();
        let tls =
            crate::http_transport::server_config(ca_pem.as_bytes(), private_key_pem.as_bytes())?;
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let endpoint: Url =
            format!("https://localhost:{}", listener.local_addr()?.port()).parse()?;
        let events = Arc::new(Mutex::new(Vec::new()));
        let applied_private_secret = Arc::new(Mutex::new(None));
        let applied_quota = Arc::new(Mutex::new(None));
        let get_responses = Arc::new(Mutex::new(std::collections::BTreeMap::new()));
        let kubevirt_read = Arc::new(Mutex::new(None));
        let state = MockKubernetesState {
            deployment_applied: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            events: Arc::clone(&events),
            applied_private_secret: Arc::clone(&applied_private_secret),
            applied_quota: Arc::clone(&applied_quota),
            get_responses: Arc::clone(&get_responses),
            lifecycle: lifecycle.map(|fixture| Arc::new(Mutex::new(fixture))),
            kubevirt_read: Arc::clone(&kubevirt_read),
        };
        let router = Router::new()
            .fallback(mock_kubernetes_handler)
            .with_state(state);
        let task = tokio::spawn(async move {
            let _ = crate::http_transport::serve_tls(listener, router, tls).await;
        });
        Ok(MockKubernetes {
            endpoint,
            ca_pem,
            events,
            applied_private_secret,
            applied_quota,
            get_responses,
            task,
            kubevirt_read,
        })
    }

    async fn mock_kubernetes_handler(
        State(state): State<MockKubernetesState>,
        request: Request<Body>,
    ) -> Response {
        let method = request.method().clone();
        let path = request.uri().path().to_owned();
        let is_deployment_patch = method == Method::PATCH && path.ends_with("/deployments/runtime");
        let is_private_secret_patch =
            method == Method::PATCH && path.ends_with("/secrets/vm-vgpu-cloud-init");
        let is_workspace_claim = path.ends_with("/persistentvolumeclaims/workspace");
        let event = if method == Method::GET && is_workspace_claim {
            let phase = if state
                .deployment_applied
                .load(std::sync::atomic::Ordering::Acquire)
            {
                "Bound"
            } else {
                "Pending"
            };
            format!("GET {path} {phase}")
        } else {
            format!(
                "{method} {path}{}",
                if path == "/apis/kubevirt.io/v1/kubevirts" {
                    "?limit=2"
                } else {
                    ""
                }
            )
        };
        state.events.lock().await.push(event);
        if let Some(fixture) = &state.lifecycle {
            return lifecycle_kubernetes_response(&mut *fixture.lock().await, request).await;
        }
        if method == Method::GET
            && let Some(response) =
                mock_storage_response(&state, &path, request.uri().query()).await
        {
            return response;
        }
        if is_deployment_patch {
            state
                .deployment_applied
                .store(true, std::sync::atomic::Ordering::Release);
        }
        if method == Method::GET
            && let Some(response) = mock_vgpu_secret_response(&path)
        {
            return response;
        }
        if method == Method::GET && path.ends_with("/datasources/ubuntu-vgpu") {
            return axum::Json(json!({
                "metadata": {"annotations": {
                    "labweaver.io/source-registry":
                        "docker://registry.example/ubuntu@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "labweaver.io/disk-sha256":
                        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "labweaver.io/base-disk-capacity-bytes": "1"
                }}
            }))
            .into_response();
        }
        if is_private_secret_patch
            || (method == Method::PATCH && path.ends_with("/resourcequotas/runtime-quota"))
        {
            let Ok(body) = to_bytes(request.into_body(), 1024 * 1024).await else {
                return StatusCode::BAD_REQUEST.into_response();
            };
            let Ok(document) = serde_json::from_slice::<Value>(&body) else {
                return StatusCode::BAD_REQUEST.into_response();
            };
            if is_private_secret_patch {
                *state.applied_private_secret.lock().await = Some(document);
            } else {
                *state.applied_quota.lock().await = Some(document);
            }
            return StatusCode::OK.into_response();
        }
        if method == Method::GET && is_workspace_claim {
            let phase = if state
                .deployment_applied
                .load(std::sync::atomic::Ordering::Acquire)
            {
                "Bound"
            } else {
                "Pending"
            };
            return axum::Json(json!({"status":{"phase":phase}})).into_response();
        }
        if method == Method::GET && path.ends_with("/deployments/runtime") {
            return axum::Json(json!({
                "metadata":{"generation":1},
                "spec":{"replicas":1},
                "status":{"observedGeneration":1,"availableReplicas":1,"unavailableReplicas":0}
            }))
            .into_response();
        }
        StatusCode::OK.into_response()
    }

    async fn mock_kubevirt_read_response(
        state: &MockKubernetesState,
        path: &str,
        query: Option<&str>,
    ) -> Option<Response> {
        if path != "/apis/kubevirt.io/v1/kubevirts" {
            return None;
        }
        if query != Some("limit=2") {
            return Some(StatusCode::BAD_REQUEST.into_response());
        }
        let response = *state.kubevirt_read.lock().await;
        if let Some((status, delay)) = response {
            tokio::time::sleep(delay).await;
            return Some(status.into_response());
        }
        None
    }

    async fn mock_storage_response(
        state: &MockKubernetesState,
        path: &str,
        query: Option<&str>,
    ) -> Option<Response> {
        if let Some(response) = mock_kubevirt_read_response(state, path, query).await {
            return Some(response);
        }
        if let Some(response) = state.get_responses.lock().await.get(path) {
            return Some(response.clone().map_or_else(
                || StatusCode::NOT_FOUND.into_response(),
                |value| axum::Json(value).into_response(),
            ));
        }
        if path.ends_with("/persistentvolumeclaims/rootdisk")
            || path.ends_with("/resourcequotas/runtime-quota")
        {
            return Some(StatusCode::NOT_FOUND.into_response());
        }
        let document = if path.ends_with("/cdiconfigs/config") {
            json!({"status":{"filesystemOverhead":{"global":"0.06"}}})
        } else if path.ends_with("/storageclasses/local-path") {
            json!({"metadata":{"name":"local-path"},"provisioner":"rancher.io/local-path"})
        } else if path.ends_with("/storageprofiles/local-path") {
            json!({"metadata":{"name":"local-path"},"status":{"claimPropertySets":null}})
        } else if path == "/apis/kubevirt.io/v1/kubevirts" {
            json!({"items":[{"spec":{"configuration":{}},"status":{
                "observedKubeVirtVersion":"v1.8.4","targetKubeVirtVersion":"v1.8.4"}}]})
        } else {
            return None;
        };
        Some(axum::Json(document).into_response())
    }

    fn mock_vgpu_secret_response(path: &str) -> Option<Response> {
        let (key, content) = if path.ends_with("/secrets/fastapi-dls-client-token") {
            ("client-token", "secret-client-token")
        } else if path.ends_with("/secrets/fastapi-dls-tls") {
            (
                "ca.crt",
                "-----BEGIN CERTIFICATE-----\ntls\n-----END CERTIFICATE-----\n",
            )
        } else if path.ends_with("/secrets/fastapi-dls-signing-root") {
            (
                "ca.crt",
                "-----BEGIN CERTIFICATE-----\nsigning\n-----END CERTIFICATE-----\n",
            )
        } else {
            return None;
        };
        Some(axum::Json(json!({"data":{(key):BASE64_STANDARD.encode(content)}})).into_response())
    }

    struct LifecycleKubernetes {
        objects: std::collections::BTreeMap<String, Value>,
        forbidden: bool,
        pods_remaining: bool,
        replace_disk_after_stop: bool,
    }

    impl LifecycleKubernetes {
        fn owned(
            environment_id: contracts::EnvironmentId,
            project_id: contracts::ProjectId,
        ) -> Self {
            let namespace = format!("lw-env-{environment_id}");
            let labels = json!({"labweaver.io/environment-id":environment_id, "labweaver.io/project-id":project_id, "labweaver.io/managed":"true"});
            let metadata = |name: &str, uid: u128| json!({"name":name,"namespace":namespace,"uid":uuid::Uuid::from_u128(uid),"resourceVersion":"1","generation":1,"labels":labels});
            Self {
                objects: std::collections::BTreeMap::from([
                    (
                        format!("/api/v1/namespaces/{namespace}"),
                        json!({"metadata":{"name":namespace,"uid":uuid::Uuid::from_u128(1),"resourceVersion":"1","labels":labels,"finalizers":["labweaver.io/environment-cleanup"]}}),
                    ),
                    (
                        format!("/apis/apps/v1/namespaces/{namespace}/deployments/runtime"),
                        json!({"metadata":metadata("runtime",2),"spec":{"replicas":1},"status":{"observedGeneration":1,"replicas":1}}),
                    ),
                    (
                        format!(
                            "/apis/kubevirt.io/v1/namespaces/{namespace}/virtualmachines/runtime"
                        ),
                        json!({"metadata":metadata("runtime",3)}),
                    ),
                    (
                        format!("/api/v1/namespaces/{namespace}/persistentvolumeclaims/rootdisk"),
                        json!({"metadata":metadata("rootdisk",4)}),
                    ),
                    (
                        format!(
                            "/apis/kubevirt.io/v1/namespaces/{namespace}/virtualmachineinstances/runtime"
                        ),
                        json!({"metadata":metadata("runtime",5)}),
                    ),
                ]),
                forbidden: false,
                pods_remaining: false,
                replace_disk_after_stop: false,
            }
        }
    }

    async fn lifecycle_kubernetes_response(
        fixture: &mut LifecycleKubernetes,
        request: Request<Body>,
    ) -> Response {
        let method = request.method().clone();
        let path = request.uri().path().to_owned();
        if fixture.forbidden {
            return StatusCode::FORBIDDEN.into_response();
        }
        if method == Method::PUT && path.ends_with("/virtualmachines/runtime/stop") {
            let vmi_path = path
                .replace("subresources.kubevirt.io", "kubevirt.io")
                .replace(
                    "virtualmachines/runtime/stop",
                    "virtualmachineinstances/runtime",
                );
            fixture.objects.remove(&vmi_path);
            if fixture.replace_disk_after_stop {
                for (path, object) in &mut fixture.objects {
                    if path.ends_with("/persistentvolumeclaims/rootdisk") {
                        object["metadata"]["uid"] = json!(uuid::Uuid::from_u128(40));
                    }
                }
            }
            return StatusCode::OK.into_response();
        }
        if method == Method::GET && path.ends_with("/pods") {
            return axum::Json(json!({"items":if fixture.pods_remaining {vec![json!({"metadata":{"name":"runtime-pod"}})]} else {Vec::<Value>::new()}})).into_response();
        }
        if method == Method::GET {
            return fixture.objects.get(&path).cloned().map_or_else(
                || StatusCode::NOT_FOUND.into_response(),
                |value| axum::Json(value).into_response(),
            );
        }
        let Ok(body) = to_bytes(request.into_body(), 1024 * 1024).await else {
            return StatusCode::BAD_REQUEST.into_response();
        };
        let Ok(body) = serde_json::from_slice::<Value>(&body) else {
            return StatusCode::BAD_REQUEST.into_response();
        };
        let Some(object) = fixture.objects.get_mut(&path) else {
            return StatusCode::NOT_FOUND.into_response();
        };
        if method == Method::DELETE {
            if body.pointer("/preconditions/uid") != object.pointer("/metadata/uid")
                || body.pointer("/preconditions/resourceVersion")
                    != object.pointer("/metadata/resourceVersion")
            {
                return StatusCode::CONFLICT.into_response();
            }
            object["metadata"]["deletionTimestamp"] = json!("2026-07-16T08:00:00Z");
            return StatusCode::OK.into_response();
        }
        if method == Method::PATCH {
            if body.pointer("/metadata/uid") != object.pointer("/metadata/uid")
                || body.pointer("/metadata/resourceVersion")
                    != object.pointer("/metadata/resourceVersion")
            {
                return StatusCode::CONFLICT.into_response();
            }
            if path.ends_with("/deployments/runtime") {
                object["spec"]["replicas"] = body["spec"]["replicas"].clone();
                object["status"]["replicas"] = json!(0);
            } else {
                fixture.objects.remove(&path);
            }
            return StatusCode::OK.into_response();
        }
        StatusCode::METHOD_NOT_ALLOWED.into_response()
    }

    #[tokio::test]
    async fn readiness_diagnostics_preserve_missing_and_invalid_ip_failures()
    -> Result<(), Box<dyn std::error::Error>> {
        let mock = spawn_mock_kubernetes().await?;
        let (_files, executor) = test_kubevirt_executor(&mock).await?;
        let environment_id = contracts::EnvironmentId::new();
        let namespace = format!("lw-env-{environment_id}");
        let plan = vm_vgpu_plan_for_test(
            environment_id,
            &namespace,
            vgpu_licensing(KubeVirtVmVgpuLicenseMode::FastapiDls),
        );
        let fence = stop_fence(environment_id);
        let vm_path =
            format!("/apis/kubevirt.io/v1/namespaces/{namespace}/virtualmachines/runtime");
        let vmi_path =
            format!("/apis/kubevirt.io/v1/namespaces/{namespace}/virtualmachineinstances/runtime");
        {
            let mut responses = mock.get_responses.lock().await;
            responses.insert(vm_path, Some(json!({"status":{"ready":true}})));
            responses.insert(
                format!("/api/v1/namespaces/{namespace}/persistentvolumeclaims/rootdisk"),
                Some(json!({})),
            );
            responses.insert(
                format!("/api/v1/namespaces/{namespace}/services/ssh"),
                Some(json!({"spec":{"clusterIP":"127.0.0.1"}})),
            );
        }
        for (interface, expected) in [
            (json!({}), ProviderFailureCode::Unavailable),
            (
                json!({"ipAddress":"invalid-address"}),
                ProviderFailureCode::ObservationInvalid,
            ),
        ] {
            mock.get_responses.lock().await.insert(vmi_path.clone(), Some(json!({"status":{"phase":"Running","conditions":[{"type":"Ready","status":"True"}],"interfaces":[interface]}})));
            let failure = executor
                .observe_kubevirt_running(&fence, &plan)
                .await
                .expect_err("Ready without a valid guest IP is insufficient");
            assert_eq!(failure.code, expected);
            assert!(failure.retryable);
        }
        assert!(
            mock.events
                .lock()
                .await
                .iter()
                .all(|event| event.starts_with("GET ")),
            "diagnostics do not mutate the runtime"
        );
        Ok(())
    }

    fn stop_fence(environment_id: contracts::EnvironmentId) -> KubeVirtBackendFence {
        KubeVirtBackendFence {
            protocol_version: crate::KUBEVIRT_BACKEND_PROTOCOL_VERSION,
            environment_id,
            operation_id: contracts::OperationId::new(),
            provider_step: 1,
            environment_generation: 2,
            attempt: 1,
            action: ReconcileAction::Stop,
            request_id: Sha256Digest::of_bytes(b"stop"),
            trace_id: "stop-test".to_owned(),
            deadline_at: UtcTimestamp::from_utc(
                timestamp().expect("timestamp").get() + time::Duration::seconds(1),
            )
            .expect("deadline"),
        }
    }

    #[tokio::test]
    async fn identity_only_stop_preserves_vm_and_disk_and_observes_container_pods()
    -> Result<(), Box<dyn std::error::Error>> {
        let environment_id = contracts::EnvironmentId::new();
        let project_id = contracts::ProjectId::new();
        let namespace = format!("lw-env-{environment_id}");
        let mock = spawn_lifecycle_kubernetes(Some(LifecycleKubernetes::owned(
            environment_id,
            project_id,
        )))
        .await?;
        let (_files, executor) = test_kubevirt_executor(&mock).await?;
        let mut fence = stop_fence(environment_id);
        let (_database, permit) = crate::kubevirt_execution::test_permit(&mut fence).await?;
        let plan = KubeVirtCleanupPlan {
            environment_id,
            project_id,
            namespace: namespace.clone(),
            virtual_machine_name: "runtime".to_owned(),
            plan_sha256: Sha256Digest::of_bytes(b"stop"),
        };
        let response = KubeVirtExecutorBackend::execute(
            &executor,
            &fence,
            &KubeVirtExecutorRequest::Stop { plan },
            &permit,
        )
        .await;
        let KubeVirtExecutorResponse::Stopped { observation, .. } = response else {
            return Err(format!("expected stopped, got {response:?}").into());
        };
        assert_eq!(observation.vm_uid, uuid::Uuid::from_u128(3));
        assert_eq!(observation.root_disk_uid, uuid::Uuid::from_u128(4));
        assert!(observation.vmi_absent);
        let container_plan = ContainerResourcePlan {
            environment_id,
            project_id,
            namespace,
            image: String::new(),
            resources: vec![],
            plan_sha256: Sha256Digest::of_bytes(b"stop-container"),
        };
        let container_fence = ContainerBackendFence {
            protocol_version: crate::CONTAINER_BACKEND_PROTOCOL_VERSION,
            environment_id,
            operation_id: fence.operation_id,
            provider_step: 1,
            operation_generation: 2,
            attempt: 1,
            action: ReconcileAction::Stop,
            request_id: fence.request_id,
            trace_id: fence.trace_id,
            deadline_at: fence.deadline_at,
        };
        assert!(
            executor
                .stop_container(&container_fence, &container_plan)
                .await
                .map_err(|failure| format!("{failure:?}"))?
                .ready
        );
        let events = mock.events.lock().await;
        assert!(events.iter().any(|event| event.ends_with("/pods")));
        assert!(!events.iter().any(|event| event.starts_with("DELETE")));
        Ok(())
    }

    #[tokio::test]
    async fn stop_rejects_foreign_project_missing_objects_and_forbidden_reads()
    -> Result<(), Box<dyn std::error::Error>> {
        let environment_id = contracts::EnvironmentId::new();
        let project_id = contracts::ProjectId::new();
        let namespace = format!("lw-env-{environment_id}");
        for scenario in [
            "vm-project",
            "disk-project",
            "deployment-project",
            "missing-vm",
            "forbidden",
        ] {
            let mut fixture = LifecycleKubernetes::owned(environment_id, project_id);
            let vm_path =
                format!("/apis/kubevirt.io/v1/namespaces/{namespace}/virtualmachines/runtime");
            let target = match scenario {
                "vm-project" => vm_path.clone(),
                "disk-project" => {
                    format!("/api/v1/namespaces/{namespace}/persistentvolumeclaims/rootdisk")
                }
                _ => format!("/apis/apps/v1/namespaces/{namespace}/deployments/runtime"),
            };
            match scenario {
                "missing-vm" => {
                    fixture.objects.remove(&vm_path);
                }
                "forbidden" => fixture.forbidden = true,
                _ => {
                    fixture.objects.get_mut(&target).expect("owned object")["metadata"]["labels"]
                        ["labweaver.io/project-id"] = json!(contracts::ProjectId::new());
                }
            }
            let mock = spawn_lifecycle_kubernetes(Some(fixture)).await?;
            let (_files, executor) = test_kubevirt_executor(&mock).await?;
            let fence = stop_fence(environment_id);
            if scenario == "deployment-project" {
                let plan = ContainerResourcePlan {
                    environment_id,
                    project_id,
                    namespace: namespace.clone(),
                    image: String::new(),
                    resources: vec![],
                    plan_sha256: fence.request_id,
                };
                let container_fence = ContainerBackendFence {
                    protocol_version: crate::CONTAINER_BACKEND_PROTOCOL_VERSION,
                    environment_id,
                    operation_id: fence.operation_id,
                    provider_step: 1,
                    operation_generation: 2,
                    attempt: 1,
                    action: ReconcileAction::Stop,
                    request_id: fence.request_id,
                    trace_id: fence.trace_id,
                    deadline_at: fence.deadline_at,
                };
                assert!(
                    executor
                        .stop_container(&container_fence, &plan)
                        .await
                        .is_err()
                );
            } else {
                let mut fence = fence;
                let (_database, permit) =
                    crate::kubevirt_execution::test_permit(&mut fence).await?;
                let plan = KubeVirtCleanupPlan {
                    environment_id,
                    project_id,
                    namespace: namespace.clone(),
                    virtual_machine_name: "runtime".to_owned(),
                    plan_sha256: fence.request_id,
                };
                assert!(matches!(
                    KubeVirtExecutorBackend::execute(
                        &executor,
                        &fence,
                        &KubeVirtExecutorRequest::Stop { plan },
                        &permit,
                    )
                    .await,
                    KubeVirtExecutorResponse::Failed { .. }
                ));
            }
            assert!(
                mock.events
                    .lock()
                    .await
                    .iter()
                    .all(|event| event.starts_with("GET")),
                "{scenario} mutated an unverified object"
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn stop_rejects_changed_disk_uid_and_waits_for_remaining_pods()
    -> Result<(), Box<dyn std::error::Error>> {
        let environment_id = contracts::EnvironmentId::new();
        let project_id = contracts::ProjectId::new();
        let namespace = format!("lw-env-{environment_id}");
        let mut fixture = LifecycleKubernetes::owned(environment_id, project_id);
        fixture.replace_disk_after_stop = true;
        let mock = spawn_lifecycle_kubernetes(Some(fixture)).await?;
        let (_files, executor) = test_kubevirt_executor(&mock).await?;
        let mut fence = stop_fence(environment_id);
        let (_database, permit) = crate::kubevirt_execution::test_permit(&mut fence).await?;
        let plan = KubeVirtCleanupPlan {
            environment_id,
            project_id,
            namespace: namespace.clone(),
            virtual_machine_name: "runtime".to_owned(),
            plan_sha256: fence.request_id,
        };
        assert!(matches!(
            KubeVirtExecutorBackend::execute(
                &executor,
                &fence,
                &KubeVirtExecutorRequest::Stop { plan },
                &permit,
            )
            .await,
            KubeVirtExecutorResponse::Failed { .. }
        ));
        let mut fixture = LifecycleKubernetes::owned(environment_id, project_id);
        fixture.pods_remaining = true;
        let mock = spawn_lifecycle_kubernetes(Some(fixture)).await?;
        let (_files, executor) = test_kubevirt_executor(&mock).await?;
        let fence = stop_fence(environment_id);
        let container_fence = ContainerBackendFence {
            protocol_version: crate::CONTAINER_BACKEND_PROTOCOL_VERSION,
            environment_id,
            operation_id: fence.operation_id,
            provider_step: 1,
            operation_generation: 2,
            attempt: 1,
            action: ReconcileAction::Stop,
            request_id: fence.request_id,
            trace_id: fence.trace_id,
            deadline_at: fence.deadline_at,
        };
        let plan = ContainerResourcePlan {
            environment_id,
            project_id,
            namespace,
            image: String::new(),
            resources: vec![],
            plan_sha256: fence.request_id,
        };
        assert!(
            executor
                .stop_container(&container_fence, &plan)
                .await
                .is_err()
        );
        assert!(
            mock.events
                .lock()
                .await
                .iter()
                .all(|event| !event.starts_with("DELETE"))
        );
        Ok(())
    }

    #[tokio::test]
    async fn unknown_provider_response_and_transport_failure_are_not_cleanup_absence()
    -> Result<(), Box<dyn std::error::Error>> {
        let environment_id = contracts::EnvironmentId::new();
        let project_id = contracts::ProjectId::new();
        let namespace = format!("lw-env-{environment_id}");
        let mut fixture = LifecycleKubernetes::owned(environment_id, project_id);
        fixture.objects.insert(
            format!("/api/v1/namespaces/{namespace}"),
            json!({"metadata":{}}),
        );
        let mock = spawn_lifecycle_kubernetes(Some(fixture)).await?;
        let (_files, executor) = test_kubevirt_executor(&mock).await?;
        assert!(
            executor
                .remove_owned_namespace(
                    environment_id,
                    project_id,
                    &namespace,
                    stop_fence(environment_id).deadline_at,
                    None,
                )
                .await
                .is_err()
        );
        assert!(
            mock.events
                .lock()
                .await
                .iter()
                .all(|event| event.starts_with("GET"))
        );
        mock.task.abort();
        tokio::task::yield_now().await;
        assert!(
            executor
                .remove_owned_namespace(
                    environment_id,
                    project_id,
                    &namespace,
                    stop_fence(environment_id).deadline_at,
                    None,
                )
                .await
                .is_err()
        );
        Ok(())
    }

    #[tokio::test]
    async fn cleanup_observes_absence_and_rejects_wrong_project_or_forbidden_before_mutation()
    -> Result<(), Box<dyn std::error::Error>> {
        let environment_id = contracts::EnvironmentId::new();
        let project_id = contracts::ProjectId::new();
        let namespace = format!("lw-env-{environment_id}");
        for scenario in ["owned", "absent", "wrong-project", "forbidden"] {
            let mut fixture = LifecycleKubernetes::owned(environment_id, project_id);
            match scenario {
                "absent" => {
                    fixture.objects.clear();
                }
                "wrong-project" => {
                    fixture
                        .objects
                        .get_mut(&format!("/api/v1/namespaces/{namespace}"))
                        .expect("namespace")["metadata"]["labels"]["labweaver.io/project-id"] =
                        json!(contracts::ProjectId::new());
                }
                "forbidden" => fixture.forbidden = true,
                _ => {}
            }
            let mock = spawn_lifecycle_kubernetes(Some(fixture)).await?;
            let (_files, executor) = test_kubevirt_executor(&mock).await?;
            let result = executor
                .remove_owned_namespace(
                    environment_id,
                    project_id,
                    &namespace,
                    stop_fence(environment_id).deadline_at,
                    None,
                )
                .await;
            let events = mock.events.lock().await;
            match scenario {
                "owned" => {
                    assert!(result.is_ok(), "{result:?}");
                    assert!(events.iter().any(|event| event.starts_with("DELETE")));
                    assert!(events.last().expect("final read").starts_with("GET"));
                }
                "absent" => {
                    assert!(result.is_ok());
                    assert_eq!(events.len(), 1);
                }
                _ => {
                    assert!(result.is_err());
                    assert!(
                        !events
                            .iter()
                            .any(|event| event.starts_with("DELETE") || event.starts_with("PATCH"))
                    );
                }
            }
        }
        Ok(())
    }

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
        assert!(matches!(
            resource_path("CiliumNetworkPolicy"),
            Ok(("/apis/cilium.io/v2", "ciliumnetworkpolicies", true))
        ));
    }

    #[test]
    fn cleanup_plan_does_not_require_provisioning_resources() {
        let environment_id = contracts::EnvironmentId::new();
        let cleanup = ContainerResourcePlan {
            environment_id,
            project_id: contracts::ProjectId::new(),
            namespace: format!("lw-env-{environment_id}"),
            image: String::new(),
            resources: Vec::new(),
            plan_sha256: Sha256Digest::of_bytes(b"cleanup"),
        };
        assert!(validate_cleanup_plan(&cleanup).is_ok());
        assert!(validate_plan(&cleanup).is_err());
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
