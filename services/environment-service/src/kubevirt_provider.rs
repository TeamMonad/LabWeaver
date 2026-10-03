use persistence_sqlx::Sha256Digest; // internal persistence hash, not contract hash
use std::collections::BTreeSet;
use std::net::IpAddr;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use contracts::access::validate_ssh_public_key;
use contracts::authoring::{
    EnvironmentRuntimeSpec, NetworkPolicySpec, PrivilegeEscalationPolicy, PublicExposurePolicy,
    RootFilesystemPolicy, RuntimeKind, RuntimeUserPolicy,
};
use contracts::environment::{
    EndpointHealth, EnvironmentEndpoint, EnvironmentInstance, ObservedEnvironmentState,
};
use contracts::events::ReleasePublished;
use contracts::resource::{GpuAllocation, GpuAllocationMode};
use contracts::supply_chain::{ImageArtifact, VirtualMachineBaseDisk, VirtualMachineDiskFormat};
use contracts::{ArtifactRef, EndpointId, EnvironmentId, OperationId, Revision, UtcTimestamp};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{PgPool, Row};
use url::Url;
use uuid::Uuid;

use crate::container_provider::valid_extended_resource_name;
use crate::{
    ContainerReleaseResolver, EnvironmentProvider, KubeVirtExecutionInstance,
    KubeVirtExecutionPermit, ProviderFailure, ProviderFailureCode, ProviderObservation,
    ProviderOutcome, ReconcileAction, ReleaseProjectionError, ResolvedContainerRelease,
};

pub const KUBEVIRT_BACKEND_PROTOCOL_VERSION: u8 = 1;
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
const GATEWAY_LABEL_KEY: &str = "app.kubernetes.io/name";
const KUBEVIRT_NODE_LABEL_KEY: &str = "labweaver.io/kubevirt";
const KUBEVIRT_NODE_LABEL_VALUE: &str = "true";
const NVIDIA_GRIDD_PATH: &str = "/usr/bin/nvidia-gridd";
const NVIDIA_GRIDD_CONFIG_PATH: &str = "/etc/nvidia/gridd.conf";
const FASTAPI_DLS_SIGNING_ROOT_PATH: &str = "/etc/nvidia/labweaver-fastapi-dls-signing-root-ca.pem";
const VGPU_CLIENT_TOKEN_DIRECTORY: &str = "/etc/nvidia/ClientConfigToken/";
const VGPU_CLIENT_TOKEN_PATH: &str = "/etc/nvidia/ClientConfigToken/client_configuration_token.tok";

/// Deployment-owned Secret reference used by the VM vGPU licensing bootstrap.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KubeVirtSecretRef {
    pub namespace: String,
    pub name: String,
    pub key: String,
}

impl KubeVirtSecretRef {
    fn validate(&self) -> Result<(), ReleaseProjectionError> {
        if !valid_dns_label(&self.namespace)
            || !valid_dns_label(&self.name)
            || self.key.is_empty()
            || self.key.len() > 253
            || !self
                .key
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
        {
            return Err(ReleaseProjectionError::ConfigurationInvalid);
        }
        Ok(())
    }
}

/// The two supported NVIDIA licensing service implementations.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum KubeVirtVmVgpuLicenseMode {
    NvidiaDls,
    FastapiDls,
}

/// Non-secret, deployment-owned VM vGPU licensing configuration.
///
/// Secret values are deliberately represented only by Kubernetes Secret references. They are
/// resolved by the restricted `KubeVirt` executor at apply time and never become part of the
/// public resource plan or image.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KubeVirtVmVgpuLicensingConfiguration {
    pub mode: KubeVirtVmVgpuLicenseMode,
    /// HTTPS endpoint that must match the endpoint encoded in the deployment-issued client token.
    /// The executor keeps the token opaque and uses this value for the exact egress boundary.
    pub license_url: Url,
    pub token_secret_ref: KubeVirtSecretRef,
    pub tls_ca_secret_ref: KubeVirtSecretRef,
    #[serde(default)]
    pub fastapi_dls_signing_root_ca_secret_ref: Option<KubeVirtSecretRef>,
}

impl KubeVirtVmVgpuLicensingConfiguration {
    /// Validates the supported deployment modes and all non-secret references.
    pub fn validate(&self) -> Result<(), ReleaseProjectionError> {
        if self.license_url.scheme() != "https"
            || self.license_url.host_str().is_none()
            || self.license_url.username() != ""
            || self.license_url.password().is_some()
            || self.license_url.path() != "/"
            || self.license_url.query().is_some()
            || self.license_url.fragment().is_some()
            || (self.mode == KubeVirtVmVgpuLicenseMode::FastapiDls
                && self.license_url.port_or_known_default() != Some(443))
        {
            return Err(ReleaseProjectionError::ConfigurationInvalid);
        }
        self.token_secret_ref.validate()?;
        self.tls_ca_secret_ref.validate()?;
        match self.mode {
            KubeVirtVmVgpuLicenseMode::NvidiaDls => {
                if self.fastapi_dls_signing_root_ca_secret_ref.is_some() {
                    return Err(ReleaseProjectionError::ConfigurationInvalid);
                }
            }
            KubeVirtVmVgpuLicenseMode::FastapiDls => {
                self.fastapi_dls_signing_root_ca_secret_ref
                    .as_ref()
                    .ok_or(ReleaseProjectionError::ConfigurationInvalid)?
                    .validate()?;
            }
        }
        Ok(())
    }

    /// Fixed path used by the prebuilt guest image for the NVIDIA DLS token.
    #[must_use]
    pub const fn token_path() -> &'static str {
        VGPU_CLIENT_TOKEN_PATH
    }

    /// Fixed directory configured in `gridd.conf` for the prebuilt guest image.
    #[must_use]
    pub const fn token_directory_path() -> &'static str {
        VGPU_CLIENT_TOKEN_DIRECTORY
    }

    /// Fixed NVIDIA Grid daemon configuration path in the prebuilt guest image.
    #[must_use]
    pub const fn gridd_config_path() -> &'static str {
        NVIDIA_GRIDD_CONFIG_PATH
    }

    /// Fixed path used by the prebuilt guest image for FastAPI-DLS signing trust.
    #[must_use]
    pub const fn fastapi_signing_root_path() -> &'static str {
        FASTAPI_DLS_SIGNING_ROOT_PATH
    }

    /// Fixed guest daemon path passed to `gridd-unlock-patcher`.
    #[must_use]
    pub const fn nvidia_gridd_path() -> &'static str {
        NVIDIA_GRIDD_PATH
    }
}

/// Durable Environment operation identity carried across the KubeVirt/CDI boundary.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KubeVirtBackendFence {
    pub protocol_version: u8,
    pub environment_id: EnvironmentId,
    pub operation_id: OperationId,
    pub provider_step: u32,
    pub environment_generation: u64,
    pub attempt: u32,
    pub action: ReconcileAction,
    pub request_id: Sha256Digest,
    pub trace_id: String,
    pub deadline_at: UtcTimestamp,
}

impl KubeVirtBackendFence {
    fn for_action(
        instance: &EnvironmentInstance,
        action: ReconcileAction,
    ) -> Result<Self, ProviderFailure> {
        let request_id = Sha256Digest::of_canonical(&json!({
            "protocolVersion": KUBEVIRT_BACKEND_PROTOCOL_VERSION,
            "environmentId": instance.id,
            "operationId": instance.operation.id,
            "providerStep": instance.operation.provider_step,
            "environmentGeneration": instance.generation,
            "attempt": instance.operation.attempt,
            "action": action,
        }))
        .map_err(|_| invalid_observation())?;
        Ok(Self {
            protocol_version: KUBEVIRT_BACKEND_PROTOCOL_VERSION,
            environment_id: instance.id,
            operation_id: instance.operation.id,
            provider_step: instance.operation.provider_step,
            environment_generation: instance.generation,
            attempt: instance.operation.attempt,
            action,
            request_id,
            trace_id: instance.operation.trace_id.clone(),
            deadline_at: instance.operation.deadline_at,
        })
    }
}

/// One deterministic Kubernetes/KubeVirt object applied by the reviewed backend.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KubeVirtResource {
    pub kind: String,
    pub namespace: Option<String>,
    pub name: String,
    pub document: Value,
}

/// Immutable VM resource plan bound to one deployment-owned CDI source and imported disk hash.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KubeVirtResourcePlan {
    pub environment_id: EnvironmentId,
    pub namespace: String,
    pub virtual_machine_name: String,
    pub data_volume_name: String,
    pub base_disk: VirtualMachineBaseDisk,
    pub base_disk_format: VirtualMachineDiskFormat,
    /// Identity rule the CDI import must verify for this base disk.
    pub base_disk_identity: KubeVirtBaseDiskIdentity,
    /// Namespace of the CDI `DataSource` the per-environment clone references.
    pub base_disk_data_source_namespace: String,
    /// Name of the CDI `DataSource` the per-environment clone references.
    pub base_disk_data_source_name: String,
    /// Reviewed raw-disk SHA-256 for a seeded base; the declared manifest digest hex for a
    /// runtime-registered base.
    pub base_disk_disk_sha256: String,
    pub storage_class_name: String,
    /// Non-secret deployment-owned licensing references used only by a VM vGPU executor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vm_vgpu_licensing: Option<KubeVirtVmVgpuLicensingConfiguration>,
    pub resources: Vec<KubeVirtResource>,
    pub plan_sha256: Sha256Digest,
}

/// Durable identity rule that applies to one resolved VM base disk.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum KubeVirtBaseDiskIdentity {
    /// Deployment-seeded reviewed base disk; the reviewed raw-disk SHA-256 is the durable identity.
    ReviewedDiskSha256,
    /// Runtime-registered base disk; the declared registry manifest digest is the durable
    /// identity. Environment never learns the raw-disk SHA-256 for this path.
    RuntimeRegistryDigest,
}

impl KubeVirtBaseDiskIdentity {
    /// Stable annotation value that makes the identity rule explicit on the imported objects.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ReviewedDiskSha256 => "disk-sha256",
            Self::RuntimeRegistryDigest => "registry-digest",
        }
    }
}

/// One resolved base-disk binding plus the identity rule that applies to it.
#[derive(Clone, Debug)]
pub struct ResolvedVmBaseDisk {
    pub binding: KubeVirtBaseDiskBinding,
    pub identity: KubeVirtBaseDiskIdentity,
}

/// Deployment-owned runtime policy that admits a release-declared VM base disk which is not one of
/// the reviewed `baseDisks[]` seeds.
///
/// The policy supplies the storage class, CDI `DataSource` namespace, guest principal and SSH port
/// for runtime-registered bases and bounds both the number of distinct runtime bindings this
/// process admits and the capacity of one runtime base.
#[derive(Clone, Debug)]
pub struct RuntimeVmBasePolicy {
    storage_class_binding: String,
    storage_class_name: String,
    data_source_namespace: String,
    guest_user: String,
    ssh_port: u16,
    max_bases: u32,
    max_capacity_bytes: u64,
}

impl RuntimeVmBasePolicy {
    /// Validates the complete runtime policy or fails closed.
    #[allow(
        clippy::too_many_arguments,
        reason = "the reviewed policy fields stay explicit on one constructor"
    )]
    pub fn new(
        storage_class_binding: String,
        storage_class_name: String,
        data_source_namespace: String,
        guest_user: String,
        ssh_port: u16,
        max_bases: u32,
        max_capacity_bytes: u64,
    ) -> Result<Self, ReleaseProjectionError> {
        if !valid_binding(&storage_class_binding)
            || !valid_dns_label(&storage_class_name)
            || !valid_dns_label(&data_source_namespace)
            || !valid_guest_user(&guest_user)
            || ssh_port == 0
            || max_bases == 0
            || max_capacity_bytes == 0
        {
            return Err(ReleaseProjectionError::ConfigurationInvalid);
        }
        Ok(Self {
            storage_class_binding,
            storage_class_name,
            data_source_namespace,
            guest_user,
            ssh_port,
            max_bases,
            max_capacity_bytes,
        })
    }

    /// Reviewed storage class binding every runtime base must use.
    #[must_use]
    pub fn storage_class_binding(&self) -> &str {
        &self.storage_class_binding
    }

    /// Maximum number of distinct runtime bindings this process admits.
    #[must_use]
    pub const fn max_bases(&self) -> u32 {
        self.max_bases
    }

    /// Maximum capacity of one runtime base in bytes.
    #[must_use]
    pub const fn max_capacity_bytes(&self) -> u64 {
        self.max_capacity_bytes
    }

    /// Builds the runtime binding for one declared base disk.
    ///
    /// The reviewed raw-disk SHA-256 is unknown on this path, so the declared registry manifest
    /// digest is both the durable identity and the catalog-independent stand-in the binding type
    /// requires. The CDI `DataSource` name is derived from that digest so one imported disk is
    /// reused across environments regardless of the release-declared binding string.
    fn runtime_binding(
        &self,
        base_disk: &VirtualMachineBaseDisk,
        format: VirtualMachineDiskFormat,
    ) -> Option<KubeVirtBaseDiskBinding> {
        let manifest_digest = declared_manifest_digest(&base_disk.source_registry_digest)?;
        let short = manifest_digest.get(..32).unwrap_or(manifest_digest);
        KubeVirtBaseDiskBinding::new(
            base_disk.binding.clone(),
            base_disk.source_registry_digest.clone(),
            manifest_digest.to_owned(),
            base_disk.capacity_bytes,
            format,
            self.storage_class_binding.clone(),
            self.storage_class_name.clone(),
            self.data_source_namespace.clone(),
            format!("vm-base-{short}"),
            self.guest_user.clone(),
            self.ssh_port,
        )
        .ok()
    }
}

/// Extracts the lowercase `sha256:` hex payload from an immutable `docker://` registry digest.
fn declared_manifest_digest(source_registry_digest: &str) -> Option<&str> {
    let (_, digest) = source_registry_digest
        .strip_prefix("docker://")?
        .rsplit_once('@')?;
    let hex = digest.strip_prefix("sha256:")?;
    (hex.len() == 64
        && hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
    .then_some(hex)
}

/// Minimal deterministic namespace deletion plan used after Access revocation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KubeVirtCleanupPlan {
    pub environment_id: EnvironmentId,
    pub project_id: contracts::ProjectId,
    pub namespace: String,
    pub virtual_machine_name: String,
    pub plan_sha256: Sha256Digest,
}

/// Complete readiness identity returned only after VM, guest agent and SSH converge.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KubeVirtRunningObservation {
    pub observed_environment_generation: u64,
    pub vm_resource_generation: u64,
    pub observed_vm_resource_generation: u64,
    pub vm_uid: Uuid,
    pub vmi_uid: Uuid,
    pub root_disk_uid: Uuid,
    pub guest_ip: IpAddr,
    pub service_cluster_ip: IpAddr,
    pub ssh_host_key_sha256: Sha256Digest,
    pub guest_agent_connected: bool,
    pub ssh_ready: bool,
    pub observed_at: UtcTimestamp,
}

/// Stop result proving the VMI disappeared while the VM and root disk identities were preserved.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KubeVirtStoppedObservation {
    pub observed_environment_generation: u64,
    pub vm_uid: Uuid,
    pub root_disk_uid: Uuid,
    pub vmi_absent: bool,
    pub observed_at: UtcTimestamp,
}

/// Exact backend seam for `KubeVirt` server-side apply, lifecycle subresources and cleanup.
#[async_trait]
pub trait KubeVirtProviderBackend: Send + Sync {
    async fn apply(
        &self,
        fence: &KubeVirtBackendFence,
        plan: &KubeVirtResourcePlan,
    ) -> Result<ProviderOutcome<KubeVirtRunningObservation>, ProviderFailure>;

    async fn observe(
        &self,
        fence: &KubeVirtBackendFence,
        plan: &KubeVirtResourcePlan,
    ) -> Result<ProviderOutcome<KubeVirtRunningObservation>, ProviderFailure>;

    async fn start(
        &self,
        fence: &KubeVirtBackendFence,
        plan: &KubeVirtResourcePlan,
    ) -> Result<ProviderOutcome<KubeVirtRunningObservation>, ProviderFailure>;

    async fn stop(
        &self,
        fence: &KubeVirtBackendFence,
        plan: &KubeVirtCleanupPlan,
    ) -> Result<ProviderOutcome<KubeVirtStoppedObservation>, ProviderFailure>;

    async fn restart(
        &self,
        fence: &KubeVirtBackendFence,
        plan: &KubeVirtResourcePlan,
    ) -> Result<ProviderOutcome<KubeVirtRunningObservation>, ProviderFailure>;

    async fn delete_namespace(
        &self,
        fence: &KubeVirtBackendFence,
        plan: &KubeVirtCleanupPlan,
    ) -> Result<ProviderOutcome<ArtifactRef>, ProviderFailure>;
}

/// NATS adapter for the deployment-owned `KubeVirt` executor.
pub struct NatsKubeVirtProviderBackend {
    client: async_nats::Client,
    subject: String,
    request_timeout: Duration,
}

impl NatsKubeVirtProviderBackend {
    pub fn new(
        client: async_nats::Client,
        subject: String,
        request_timeout: Duration,
    ) -> Result<Self, ProviderFailure> {
        if !valid_subject(&subject)
            || request_timeout.is_zero()
            || request_timeout > Duration::from_mins(5)
        {
            return Err(configuration_invalid());
        }
        Ok(Self {
            client,
            subject,
            request_timeout,
        })
    }

    async fn request(
        &self,
        fence: &KubeVirtBackendFence,
        request: KubeVirtExecutorRequest,
    ) -> Result<KubeVirtExecutorResponse, ProviderFailure> {
        if !request.matches_action(fence.action) {
            return Err(invalid_observation());
        }
        let fence = bind_kubevirt_executor_request(fence.clone(), &request)?;
        let payload = serde_json::to_vec(&KubeVirtExecutorRequestEnvelope {
            fence: fence.clone(),
            request,
        })
        .map_err(|_| invalid_observation())?;
        let request = async_nats::Request::new()
            .timeout(Some(self.request_timeout))
            .payload(payload.into());
        let message = match self
            .client
            .send_request(self.subject.clone(), request)
            .await
            .map_err(|error| {
                if error.kind() == async_nats::client::RequestErrorKind::TimedOut {
                    return None;
                }
                tracing::warn!(
                    event = "environment.kubevirt_provider.executor_request_failed",
                    component = "kubevirt-provider",
                    operation = "kubevirt.executor.request",
                    outcome = "failed",
                    duration_ms = 0_u64,
                    trace_id = fence.trace_id,
                    diagnostic_code = "LW_ENVIRONMENT_PROVIDER_UNAVAILABLE",
                    error_kind = "provider_transport_failed",
                    failure_stage = "kubevirt.executor.request",
                    retryable = true,
                    safe_detail = "executor_request_failed",
                    environment_id = %fence.environment_id,
                    operation_id = %fence.operation_id,
                    attempt = fence.attempt,
                );
                Some(unavailable())
            }) {
            Ok(message) => message,
            Err(None) => return Ok(KubeVirtExecutorResponse::Pending),
            Err(Some(failure)) => return Err(failure),
        };
        if message.payload.len() > MAX_RESPONSE_BYTES {
            return Err(invalid_observation());
        }
        let response: KubeVirtExecutorResponseEnvelope =
            serde_json::from_slice(&message.payload).map_err(|_| invalid_observation())?;
        if response.protocol_version != fence.protocol_version
            || response.environment_id != fence.environment_id
            || response.operation_id != fence.operation_id
            || response.provider_step != fence.provider_step
            || response.environment_generation != fence.environment_generation
            || response.attempt != fence.attempt
            || response.action != fence.action
            || response.request_id != fence.request_id
        {
            return Err(invalid_observation());
        }
        Ok(response.response)
    }
}

#[async_trait]
impl KubeVirtProviderBackend for NatsKubeVirtProviderBackend {
    async fn apply(
        &self,
        fence: &KubeVirtBackendFence,
        plan: &KubeVirtResourcePlan,
    ) -> Result<ProviderOutcome<KubeVirtRunningObservation>, ProviderFailure> {
        running_response(
            &self
                .request(fence, KubeVirtExecutorRequest::Apply { plan: plan.clone() })
                .await?,
            plan,
        )
    }

    async fn observe(
        &self,
        fence: &KubeVirtBackendFence,
        plan: &KubeVirtResourcePlan,
    ) -> Result<ProviderOutcome<KubeVirtRunningObservation>, ProviderFailure> {
        running_response(
            &self
                .request(
                    fence,
                    KubeVirtExecutorRequest::Observe { plan: plan.clone() },
                )
                .await?,
            plan,
        )
    }

    async fn start(
        &self,
        fence: &KubeVirtBackendFence,
        plan: &KubeVirtResourcePlan,
    ) -> Result<ProviderOutcome<KubeVirtRunningObservation>, ProviderFailure> {
        running_response(
            &self
                .request(fence, KubeVirtExecutorRequest::Start { plan: plan.clone() })
                .await?,
            plan,
        )
    }

    async fn stop(
        &self,
        fence: &KubeVirtBackendFence,
        plan: &KubeVirtCleanupPlan,
    ) -> Result<ProviderOutcome<KubeVirtStoppedObservation>, ProviderFailure> {
        match self
            .request(fence, KubeVirtExecutorRequest::Stop { plan: plan.clone() })
            .await?
        {
            KubeVirtExecutorResponse::Stopped {
                plan_sha256,
                observation,
            } if plan_sha256 == plan.plan_sha256 => Ok(ProviderOutcome::Completed(observation)),
            KubeVirtExecutorResponse::Pending => Ok(ProviderOutcome::Pending),
            KubeVirtExecutorResponse::Failed { failure } => Err(failure),
            _ => Err(invalid_observation()),
        }
    }

    async fn restart(
        &self,
        fence: &KubeVirtBackendFence,
        plan: &KubeVirtResourcePlan,
    ) -> Result<ProviderOutcome<KubeVirtRunningObservation>, ProviderFailure> {
        running_response(
            &self
                .request(
                    fence,
                    KubeVirtExecutorRequest::Restart { plan: plan.clone() },
                )
                .await?,
            plan,
        )
    }

    async fn delete_namespace(
        &self,
        fence: &KubeVirtBackendFence,
        plan: &KubeVirtCleanupPlan,
    ) -> Result<ProviderOutcome<ArtifactRef>, ProviderFailure> {
        match self
            .request(
                fence,
                KubeVirtExecutorRequest::DeleteNamespace { plan: plan.clone() },
            )
            .await?
        {
            KubeVirtExecutorResponse::Deleted {
                plan_sha256,
                cleanup_evidence,
            } if plan_sha256 == plan.plan_sha256 && valid_artifact_ref(&cleanup_evidence) => {
                Ok(ProviderOutcome::Completed(cleanup_evidence))
            }
            KubeVirtExecutorResponse::Pending => Ok(ProviderOutcome::Pending),
            KubeVirtExecutorResponse::Failed { failure } => Err(failure),
            _ => Err(ProviderFailure {
                code: ProviderFailureCode::CleanupFailed,
                retryable: true,
            }),
        }
    }
}

fn running_response(
    response: &KubeVirtExecutorResponse,
    plan: &KubeVirtResourcePlan,
) -> Result<ProviderOutcome<KubeVirtRunningObservation>, ProviderFailure> {
    match response {
        KubeVirtExecutorResponse::Running {
            plan_sha256,
            observation,
        } if *plan_sha256 == plan.plan_sha256 => Ok(ProviderOutcome::Completed(*observation)),
        KubeVirtExecutorResponse::Pending => Ok(ProviderOutcome::Pending),
        KubeVirtExecutorResponse::Failed { failure } => Err(*failure),
        _ => Err(invalid_observation()),
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KubeVirtExecutorRequestEnvelope {
    #[serde(flatten)]
    pub fence: KubeVirtBackendFence,
    pub request: KubeVirtExecutorRequest,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(
    tag = "backendAction",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum KubeVirtExecutorRequest {
    Apply { plan: KubeVirtResourcePlan },
    Observe { plan: KubeVirtResourcePlan },
    Start { plan: KubeVirtResourcePlan },
    Stop { plan: KubeVirtCleanupPlan },
    Restart { plan: KubeVirtResourcePlan },
    DeleteNamespace { plan: KubeVirtCleanupPlan },
}

impl KubeVirtExecutorRequest {
    const fn matches_action(&self, action: ReconcileAction) -> bool {
        matches!(
            (self, action),
            (
                Self::Apply { .. },
                ReconcileAction::Provision | ReconcileAction::Reset
            ) | (Self::Observe { .. }, ReconcileAction::Observe)
                | (Self::Start { .. }, ReconcileAction::Start)
                | (Self::Stop { .. }, ReconcileAction::Stop)
                | (Self::Restart { .. }, ReconcileAction::Restart)
                | (Self::DeleteNamespace { .. }, ReconcileAction::Cleanup)
        )
    }
}

fn bind_kubevirt_executor_request(
    mut fence: KubeVirtBackendFence,
    request: &KubeVirtExecutorRequest,
) -> Result<KubeVirtBackendFence, ProviderFailure> {
    fence.request_id = kubevirt_executor_request_id(&fence, request)?;
    Ok(fence)
}

fn kubevirt_executor_request_id(
    fence: &KubeVirtBackendFence,
    request: &KubeVirtExecutorRequest,
) -> Result<Sha256Digest, ProviderFailure> {
    Sha256Digest::of_canonical(&json!({
        "protocolVersion": fence.protocol_version,
        "environmentId": fence.environment_id,
        "operationId": fence.operation_id,
        "providerStep": fence.provider_step,
        "environmentGeneration": fence.environment_generation,
        "attempt": fence.attempt,
        "action": fence.action,
        "deadlineAt": fence.deadline_at,
        "request": request,
    }))
    .map_err(|_| invalid_observation())
}

const fn kubevirt_executor_environment_id(request: &KubeVirtExecutorRequest) -> EnvironmentId {
    match request {
        KubeVirtExecutorRequest::Apply { plan }
        | KubeVirtExecutorRequest::Observe { plan }
        | KubeVirtExecutorRequest::Start { plan }
        | KubeVirtExecutorRequest::Restart { plan } => plan.environment_id,
        KubeVirtExecutorRequest::Stop { plan }
        | KubeVirtExecutorRequest::DeleteNamespace { plan } => plan.environment_id,
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KubeVirtExecutorResponseEnvelope {
    pub protocol_version: u8,
    pub environment_id: EnvironmentId,
    pub operation_id: OperationId,
    pub provider_step: u32,
    pub environment_generation: u64,
    pub attempt: u32,
    pub action: ReconcileAction,
    pub request_id: Sha256Digest,
    pub response: KubeVirtExecutorResponse,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(
    tag = "status",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum KubeVirtExecutorResponse {
    Pending,
    Running {
        plan_sha256: Sha256Digest,
        observation: KubeVirtRunningObservation,
    },
    Stopped {
        plan_sha256: Sha256Digest,
        observation: KubeVirtStoppedObservation,
    },
    Deleted {
        plan_sha256: Sha256Digest,
        cleanup_evidence: ArtifactRef,
    },
    Failed {
        failure: ProviderFailure,
    },
}

/// `KubeVirt` side-effect adapter invoked only after durable executor admission.
#[async_trait]
pub trait KubeVirtExecutorBackend: Send + Sync {
    async fn execute(
        &self,
        fence: &KubeVirtBackendFence,
        request: &KubeVirtExecutorRequest,
        permit: &KubeVirtExecutionPermit,
    ) -> KubeVirtExecutorResponse;
}

/// Persistent highest-generation and permanent-cleanup ledger for VM operations.
#[derive(Clone, Debug)]
pub struct PgKubeVirtExecutorFenceStore {
    pool: PgPool,
}

#[derive(Clone)]
struct UnfinishedKubeVirtExecution {
    instance: KubeVirtExecutionInstance,
    request_id: String,
    generation: i64,
    operation_id: Uuid,
    provider_step: i32,
    attempt: i32,
    deadline_at: time::OffsetDateTime,
}

impl PgKubeVirtExecutorFenceStore {
    async fn recover_previous_boot(
        &self,
        instance: &KubeVirtExecutionInstance,
    ) -> Result<(), KubeVirtExecutorFenceError> {
        let rows=sqlx::query("SELECT environment_id,execution_owner,last_request_id,highest_generation,operation_id,provider_step,attempt,deadline_at FROM environment.kubevirt_executor_fences WHERE last_response IS NULL AND execution_owner->>'podUid'=$1 AND execution_owner->>'containerName'=$2")
            .bind(instance.pod_uid.to_string()).bind(&instance.container_name).fetch_all(&self.pool).await?;
        for row in rows {
            let owner: KubeVirtExecutionInstance =
                serde_json::from_value(row.try_get("execution_owner")?)
                    .map_err(|_| KubeVirtExecutorFenceError::IdentityMismatch)?;
            if owner.namespace != instance.namespace
                || owner.pod_name != instance.pod_name
                || owner.boot_token == instance.boot_token
            {
                return Err(KubeVirtExecutorFenceError::IdentityMismatch);
            }
            let previous = UnfinishedKubeVirtExecution {
                instance: owner,
                request_id: row.try_get("last_request_id")?,
                generation: row.try_get("highest_generation")?,
                operation_id: row.try_get("operation_id")?,
                provider_step: row.try_get("provider_step")?,
                attempt: row.try_get("attempt")?,
                deadline_at: row.try_get("deadline_at")?,
            };
            let environment_id =
                EnvironmentId::from_str(&row.try_get::<Uuid, _>("environment_id")?.to_string())
                    .map_err(|_| KubeVirtExecutorFenceError::IdentityMismatch)?;
            self.retire_previous_boot(environment_id, &previous).await?;
        }
        Ok(())
    }

    /// Only the new verified PID1 may retire an earlier boot in the same isolated Pod container.
    async fn retire_previous_boot(
        &self,
        environment_id: EnvironmentId,
        previous: &UnfinishedKubeVirtExecution,
    ) -> Result<(), KubeVirtExecutorFenceError> {
        let now: time::OffsetDateTime = sqlx::query_scalar("SELECT clock_timestamp()")
            .fetch_one(&self.pool)
            .await?;
        let code = if now >= previous.deadline_at {
            ProviderFailureCode::Timeout
        } else {
            ProviderFailureCode::Cancelled
        };
        let response = KubeVirtExecutorResponse::Failed {
            failure: ProviderFailure {
                code,
                retryable: false,
            },
        };
        let affected = sqlx::query(
            "UPDATE environment.kubevirt_executor_fences SET last_response=$8, \
             updated_at=clock_timestamp() WHERE environment_id=$1 AND highest_generation=$2 \
             AND operation_id=$3 AND provider_step=$4 AND attempt=$5 AND last_request_id=$6 \
             AND execution_owner=$7 AND last_response IS NULL",
        )
        .bind(environment_id.as_uuid())
        .bind(previous.generation)
        .bind(previous.operation_id)
        .bind(previous.provider_step)
        .bind(previous.attempt)
        .bind(&previous.request_id)
        .bind(
            serde_json::to_value(&previous.instance)
                .map_err(|_| KubeVirtExecutorFenceError::IdentityMismatch)?,
        )
        .bind(
            serde_json::to_value(response)
                .map_err(|_| KubeVirtExecutorFenceError::IdentityMismatch)?,
        )
        .execute(&self.pool)
        .await?
        .rows_affected();
        if affected != 1 {
            return Err(KubeVirtExecutorFenceError::StaleGeneration);
        }
        Ok(())
    }

    fn permit(
        &self,
        fence: KubeVirtBackendFence,
        instance: KubeVirtExecutionInstance,
    ) -> KubeVirtExecutionPermit {
        KubeVirtExecutionPermit {
            pool: self.pool.clone(),
            fence,
            instance,
        }
    }
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the row lock, generation fence and cleanup tombstone form one admission decision"
    )]
    async fn admit(
        &self,
        envelope: &KubeVirtExecutorRequestEnvelope,
        instance: &KubeVirtExecutionInstance,
    ) -> Result<KubeVirtExecutorAdmission, KubeVirtExecutorFenceError> {
        validate_kubevirt_executor_request(envelope)?;
        let fence = &envelope.fence;
        let mut transaction = self.pool.begin().await?;
        lock_execution_environment(&mut transaction, fence.environment_id).await?;
        let current = sqlx::query(
            "SELECT highest_generation,operation_id,provider_step,attempt,tombstoned, \
                    last_request_id,last_response,deadline_at \
             FROM environment.kubevirt_executor_fences WHERE environment_id=$1 FOR UPDATE",
        )
        .bind(fence.environment_id.as_uuid())
        .fetch_optional(&mut *transaction)
        .await?;
        let authority_now: time::OffsetDateTime =
            sqlx::query_scalar("SELECT date_trunc('milliseconds',clock_timestamp())")
                .fetch_one(&mut *transaction)
                .await?;
        if let Some(row) = current {
            let highest_generation = u64::try_from(row.try_get::<i64, _>("highest_generation")?)
                .map_err(|_| KubeVirtExecutorFenceError::IdentityMismatch)?;
            let operation_id =
                OperationId::from_str(&row.try_get::<Uuid, _>("operation_id")?.to_string())
                    .map_err(|_| KubeVirtExecutorFenceError::IdentityMismatch)?;
            let provider_step = u32::try_from(row.try_get::<i32, _>("provider_step")?)
                .map_err(|_| KubeVirtExecutorFenceError::IdentityMismatch)?;
            let attempt = u32::try_from(row.try_get::<i32, _>("attempt")?)
                .map_err(|_| KubeVirtExecutorFenceError::IdentityMismatch)?;
            let tombstoned: bool = row.try_get("tombstoned")?;
            let last_request_id: String = row.try_get("last_request_id")?;
            let last_response = row.try_get::<Option<Value>, _>("last_response")?;

            if last_request_id == fence.request_id.to_string() {
                if let Some(value) = last_response {
                    transaction.rollback().await?;
                    return Ok(KubeVirtExecutorAdmission::Replay(value));
                }
                return Err(KubeVirtExecutorFenceError::InProgress);
            }
            if authority_now >= fence.deadline_at.get() {
                return Err(KubeVirtExecutorFenceError::DeadlineExceeded);
            }
            if last_response.is_none() {
                return Err(KubeVirtExecutorFenceError::InProgress);
            }
            let cleanup_succeeded = last_response
                .as_ref()
                .and_then(|value| value.get("status"))
                .and_then(Value::as_str)
                == Some("deleted");
            if tombstoned && cleanup_succeeded {
                if fence.action == ReconcileAction::Cleanup {
                    let value = last_response
                        .as_ref()
                        .ok_or(KubeVirtExecutorFenceError::IdentityMismatch)?
                        .clone();
                    transaction.rollback().await?;
                    return Ok(KubeVirtExecutorAdmission::Replay(value));
                }
                return Err(KubeVirtExecutorFenceError::Tombstoned);
            }
            if fence.environment_generation < highest_generation
                || (fence.environment_generation == highest_generation
                    && (fence.provider_step < provider_step
                        || (fence.provider_step == provider_step && fence.attempt < attempt)))
            {
                return Err(KubeVirtExecutorFenceError::StaleGeneration);
            }
            if fence.environment_generation == highest_generation
                && fence.operation_id != operation_id
            {
                return Err(KubeVirtExecutorFenceError::IdentityMismatch);
            }
            sqlx::query(
                "UPDATE environment.kubevirt_executor_fences SET highest_generation=$2, \
                 operation_id=$3,provider_step=$4,attempt=$5,tombstoned=$6,last_action=$7, \
                 last_request_id=$8,last_response=NULL,deadline_at=$9,execution_owner=$10,updated_at=clock_timestamp() \
                 WHERE environment_id=$1",
            )
            .bind(fence.environment_id.as_uuid())
            .bind(
                i64::try_from(fence.environment_generation)
                    .map_err(|_| KubeVirtExecutorFenceError::IdentityMismatch)?,
            )
            .bind(fence.operation_id.as_uuid())
            .bind(
                i32::try_from(fence.provider_step)
                    .map_err(|_| KubeVirtExecutorFenceError::IdentityMismatch)?,
            )
            .bind(
                i32::try_from(fence.attempt)
                    .map_err(|_| KubeVirtExecutorFenceError::IdentityMismatch)?,
            )
            .bind(false)
            .bind(kubevirt_action_name(fence.action))
            .bind(fence.request_id.to_string())
            .bind(fence.deadline_at.get())
            .bind(serde_json::to_value(instance).map_err(|_| KubeVirtExecutorFenceError::IdentityMismatch)?)
            .execute(&mut *transaction)
            .await?;
        } else {
            if authority_now >= fence.deadline_at.get() {
                return Err(KubeVirtExecutorFenceError::DeadlineExceeded);
            }
            let admitted = sqlx::query(
                "INSERT INTO environment.kubevirt_executor_fences \
                 (environment_id,highest_generation,operation_id,provider_step,attempt,tombstoned, \
                  last_action,last_request_id,deadline_at,execution_owner) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10) ON CONFLICT (environment_id) DO NOTHING",
            )
            .bind(fence.environment_id.as_uuid())
            .bind(
                i64::try_from(fence.environment_generation)
                    .map_err(|_| KubeVirtExecutorFenceError::IdentityMismatch)?,
            )
            .bind(fence.operation_id.as_uuid())
            .bind(
                i32::try_from(fence.provider_step)
                    .map_err(|_| KubeVirtExecutorFenceError::IdentityMismatch)?,
            )
            .bind(
                i32::try_from(fence.attempt)
                    .map_err(|_| KubeVirtExecutorFenceError::IdentityMismatch)?,
            )
            .bind(false)
            .bind(kubevirt_action_name(fence.action))
            .bind(fence.request_id.to_string())
            .bind(fence.deadline_at.get())
            .bind(serde_json::to_value(instance).map_err(|_| KubeVirtExecutorFenceError::IdentityMismatch)?)
            .execute(&mut *transaction)
            .await?;
            if admitted.rows_affected() != 1 {
                return Err(KubeVirtExecutorFenceError::InProgress);
            }
        }
        transaction.commit().await?;
        Ok(KubeVirtExecutorAdmission::Execute)
    }

    async fn complete(
        &self,
        fence: KubeVirtBackendFence,
        response: &KubeVirtExecutorResponse,
        instance: &KubeVirtExecutionInstance,
    ) -> Result<(), KubeVirtExecutorFenceError> {
        let value = serde_json::to_value(response)
            .map_err(|_| KubeVirtExecutorFenceError::IdentityMismatch)?;
        let mut transaction = self.pool.begin().await?;
        lock_execution_environment(&mut transaction, fence.environment_id).await?;
        let updated = sqlx::query(
            "UPDATE environment.kubevirt_executor_fences SET last_response=$7, \
                 tombstoned=CASE WHEN $8 THEN TRUE ELSE tombstoned END,updated_at=clock_timestamp() \
             WHERE environment_id=$1 AND highest_generation=$2 AND operation_id=$3 \
               AND provider_step=$4 AND attempt=$5 AND last_request_id=$6 AND execution_owner=$9 AND (last_response IS NULL OR last_response=$7)",
        )
        .bind(fence.environment_id.as_uuid())
        .bind(i64::try_from(fence.environment_generation).map_err(|_| KubeVirtExecutorFenceError::IdentityMismatch)?)
        .bind(fence.operation_id.as_uuid())
        .bind(i32::try_from(fence.provider_step).map_err(|_| KubeVirtExecutorFenceError::IdentityMismatch)?)
        .bind(i32::try_from(fence.attempt).map_err(|_| KubeVirtExecutorFenceError::IdentityMismatch)?)
        .bind(fence.request_id.to_string())
        .bind(value)
        .bind(matches!(response, KubeVirtExecutorResponse::Deleted { .. }))
        .bind(serde_json::to_value(instance).map_err(|_| KubeVirtExecutorFenceError::IdentityMismatch)?)
        .execute(&mut *transaction)
        .await?;
        if updated.rows_affected() != 1 {
            return Err(KubeVirtExecutorFenceError::StaleGeneration);
        }
        transaction.commit().await?;
        Ok(())
    }
}

/// A DB-only barrier settles any earlier admission commit before a no-row completion decision.
async fn lock_execution_environment(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    environment_id: EnvironmentId,
) -> Result<(), KubeVirtExecutorFenceError> {
    sqlx::query("SELECT environment_id FROM environment.environment_instances WHERE environment_id=$1 FOR UPDATE")
        .bind(environment_id.as_uuid())
        .fetch_optional(&mut **transaction)
        .await?
        .ok_or(KubeVirtExecutorFenceError::Cancelled)?;
    Ok(())
}

enum KubeVirtExecutorAdmission {
    Execute,
    Replay(Value),
}

/// Each process remembers its actual in-flight execution until its terminal response is durable.
#[derive(Clone)]
struct ActiveKubeVirtExecution {
    fence: KubeVirtBackendFence,
    terminal: Option<KubeVirtExecutorResponse>,
}

/// Kubernetes-incarnation-fenced executor. No database lock spans backend I/O.
pub struct FencedKubeVirtExecutor<B> {
    store: PgKubeVirtExecutorFenceStore,
    backend: B,
    instance: KubeVirtExecutionInstance,
    active: Mutex<std::collections::BTreeMap<Uuid, ActiveKubeVirtExecution>>,
    shutdown: tokio::sync::watch::Sender<bool>,
}

impl<B: KubeVirtExecutorBackend> FencedKubeVirtExecutor<B> {
    #[must_use]
    pub fn new(
        store: PgKubeVirtExecutorFenceStore,
        backend: B,
        instance: KubeVirtExecutionInstance,
    ) -> Self {
        Self {
            store,
            backend,
            instance,
            active: Mutex::new(std::collections::BTreeMap::new()),
            shutdown: tokio::sync::watch::channel(false).0,
        }
    }

    /// Called once before listening, after the production PID1/PodSpec startup check.
    pub async fn prepare_startup(&self) -> Result<(), KubeVirtExecutorFenceError> {
        self.store.recover_previous_boot(&self.instance).await
    }

    fn cancel_active(&self) {
        self.shutdown.send_replace(true);
    }

    async fn finish_terminals(&self) -> Result<(), KubeVirtExecutorFenceError> {
        let records = self
            .active
            .lock()
            .map_err(|_| KubeVirtExecutorFenceError::IdentityMismatch)?
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for record in records {
            let terminal = record
                .terminal
                .ok_or(KubeVirtExecutorFenceError::InProgress)?;
            self.store
                .complete(record.fence.clone(), &terminal, &self.instance)
                .await?;
            self.forget(&record.fence)?;
        }
        Ok(())
    }

    pub async fn execute(
        &self,
        envelope: KubeVirtExecutorRequestEnvelope,
    ) -> Result<KubeVirtExecutorResponseEnvelope, KubeVirtExecutorFenceError> {
        validate_kubevirt_executor_request(&envelope)?;
        if *self.shutdown.borrow() {
            return Err(KubeVirtExecutorFenceError::Cancelled);
        }
        let fence = &envelope.fence;
        let previous = {
            let mut active = self
                .active
                .lock()
                .map_err(|_| KubeVirtExecutorFenceError::IdentityMismatch)?;
            if let Some(previous) = active.get(&fence.environment_id.as_uuid()) {
                Some(previous.clone())
            } else {
                active.insert(
                    fence.environment_id.as_uuid(),
                    ActiveKubeVirtExecution {
                        fence: fence.clone(),
                        terminal: None,
                    },
                );
                None
            }
        };
        if let Some(previous) = previous {
            if let Some(terminal) = previous.terminal {
                match self
                    .store
                    .complete(previous.fence.clone(), &terminal, &self.instance)
                    .await
                {
                    Ok(()) => {
                        self.forget(&previous.fence)?;
                        if previous.fence.request_id == fence.request_id {
                            return Ok(kubevirt_response_envelope(fence, terminal));
                        }
                    }
                    Err(KubeVirtExecutorFenceError::StaleGeneration) => {
                        self.forget(&previous.fence)?;
                    }
                    Err(_) => {}
                }
            }
            return Ok(kubevirt_response_envelope(
                fence,
                KubeVirtExecutorResponse::Pending,
            ));
        }
        let result = self.execute_reserved(&envelope).await;
        // A terminal whose persistence failed stays in memory; polling retries only completion.
        if self
            .active
            .lock()
            .map_err(|_| KubeVirtExecutorFenceError::IdentityMismatch)?
            .get(&fence.environment_id.as_uuid())
            .is_none_or(|record| record.terminal.is_none())
        {
            self.forget(fence)?;
        }
        result.map(|response| kubevirt_response_envelope(fence, response))
    }

    async fn execute_reserved(
        &self,
        envelope: &KubeVirtExecutorRequestEnvelope,
    ) -> Result<KubeVirtExecutorResponse, KubeVirtExecutorFenceError> {
        match self.store.admit(envelope, &self.instance).await {
            Ok(KubeVirtExecutorAdmission::Replay(value)) => serde_json::from_value(value)
                .map_err(|_| KubeVirtExecutorFenceError::IdentityMismatch),
            Err(KubeVirtExecutorFenceError::InProgress) => Ok(KubeVirtExecutorResponse::Pending),
            Err(error @ KubeVirtExecutorFenceError::Database(_)) => {
                self.finish_unstarted_admission(&envelope.fence, error)
                    .await
            }
            Err(error) => Err(error),
            Ok(KubeVirtExecutorAdmission::Execute) => {
                let permit = self
                    .store
                    .permit(envelope.fence.clone(), self.instance.clone());
                let response = self.run_to_terminal(envelope, &permit).await;
                {
                    let mut active = self
                        .active
                        .lock()
                        .map_err(|_| KubeVirtExecutorFenceError::IdentityMismatch)?;
                    let record = active
                        .get_mut(&envelope.fence.environment_id.as_uuid())
                        .ok_or(KubeVirtExecutorFenceError::IdentityMismatch)?;
                    record.terminal = Some(response.clone());
                }
                if self
                    .store
                    .complete(envelope.fence.clone(), &response, &self.instance)
                    .await
                    .is_err()
                {
                    return Ok(KubeVirtExecutorResponse::Pending);
                }
                self.forget(&envelope.fence)?;
                Ok(response)
            }
        }
    }

    async fn finish_unstarted_admission(
        &self,
        fence: &KubeVirtBackendFence,
        original_error: KubeVirtExecutorFenceError,
    ) -> Result<KubeVirtExecutorResponse, KubeVirtExecutorFenceError> {
        // No backend was invoked. A committed admission can be finished without repeating effects.
        let response = KubeVirtExecutorResponse::Failed {
            failure: unavailable(),
        };
        {
            let mut active = self
                .active
                .lock()
                .map_err(|_| KubeVirtExecutorFenceError::IdentityMismatch)?;
            let record = active
                .get_mut(&fence.environment_id.as_uuid())
                .ok_or(KubeVirtExecutorFenceError::IdentityMismatch)?;
            record.terminal = Some(response.clone());
        }
        match self
            .store
            .complete(fence.clone(), &response, &self.instance)
            .await
        {
            Ok(()) => {
                self.forget(fence)?;
                Ok(response)
            }
            Err(
                KubeVirtExecutorFenceError::StaleGeneration | KubeVirtExecutorFenceError::Cancelled,
            ) => {
                // complete's Environment barrier proves the earlier DB transaction has settled.
                self.forget(fence)?;
                Err(original_error)
            }
            Err(_) => Ok(KubeVirtExecutorResponse::Pending),
        }
    }

    async fn run_to_terminal(
        &self,
        envelope: &KubeVirtExecutorRequestEnvelope,
        permit: &KubeVirtExecutionPermit,
    ) -> KubeVirtExecutorResponse {
        let mut shutdown = self.shutdown.subscribe();
        if *shutdown.borrow() {
            return execution_terminal_failure(&KubeVirtExecutorFenceError::Cancelled);
        }
        let remaining = match permit.check().await {
            Ok(remaining) => remaining,
            Err(error) => return execution_terminal_failure(&error),
        };
        // Leaving this block drops the actual backend future before completion is persisted.
        let response = {
            let work = self
                .backend
                .execute(&envelope.fence, &envelope.request, permit);
            tokio::pin!(work);
            let watch_authority = async {
                loop {
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    if let Err(error) = permit.check().await {
                        return error;
                    }
                }
            };
            tokio::select! {
                response = &mut work => response,
                _ = shutdown.changed() => execution_terminal_failure(&KubeVirtExecutorFenceError::Cancelled),
                error = watch_authority => execution_terminal_failure(&error),
                () = tokio::time::sleep(remaining) => execution_terminal_failure(&KubeVirtExecutorFenceError::DeadlineExceeded),
            }
        };
        // The backend only completes when its work is terminal. Pending is a transport reply,
        // never a durable result or permission to repeat a possibly accepted write.
        if matches!(response, KubeVirtExecutorResponse::Pending) {
            return KubeVirtExecutorResponse::Failed {
                failure: ProviderFailure {
                    code: ProviderFailureCode::ObservationInvalid,
                    retryable: false,
                },
            };
        }
        if !matches!(response, KubeVirtExecutorResponse::Failed { .. })
            && let Err(error) = permit.check().await
        {
            return execution_terminal_failure(&error);
        }
        response
    }

    fn forget(&self, fence: &KubeVirtBackendFence) -> Result<(), KubeVirtExecutorFenceError> {
        let mut active = self
            .active
            .lock()
            .map_err(|_| KubeVirtExecutorFenceError::IdentityMismatch)?;
        if active
            .get(&fence.environment_id.as_uuid())
            .is_some_and(|record| record.fence.request_id == fence.request_id)
        {
            active.remove(&fence.environment_id.as_uuid());
        }
        Ok(())
    }
}

fn execution_terminal_failure(error: &KubeVirtExecutorFenceError) -> KubeVirtExecutorResponse {
    let failure = match error {
        KubeVirtExecutorFenceError::DeadlineExceeded => ProviderFailure {
            code: ProviderFailureCode::Timeout,
            retryable: false,
        },
        KubeVirtExecutorFenceError::Cancelled => ProviderFailure {
            code: ProviderFailureCode::Cancelled,
            retryable: false,
        },
        _ => kubevirt_executor_failure(error),
    };
    KubeVirtExecutorResponse::Failed { failure }
}

fn kubevirt_response_envelope(
    fence: &KubeVirtBackendFence,
    response: KubeVirtExecutorResponse,
) -> KubeVirtExecutorResponseEnvelope {
    KubeVirtExecutorResponseEnvelope {
        protocol_version: fence.protocol_version,
        environment_id: fence.environment_id,
        operation_id: fence.operation_id,
        provider_step: fence.provider_step,
        environment_generation: fence.environment_generation,
        attempt: fence.attempt,
        action: fence.action,
        request_id: fence.request_id,
        response,
    }
}

pub struct NatsKubeVirtExecutorServer<B> {
    client: async_nats::Client,
    subject: String,
    executor: Arc<FencedKubeVirtExecutor<B>>,
}

impl<B: KubeVirtExecutorBackend + 'static> NatsKubeVirtExecutorServer<B> {
    pub fn new(
        client: async_nats::Client,
        subject: String,
        executor: FencedKubeVirtExecutor<B>,
    ) -> Result<Self, KubeVirtExecutorFenceError> {
        if !valid_subject(&subject) {
            return Err(KubeVirtExecutorFenceError::ConfigurationInvalid);
        }
        Ok(Self {
            client,
            subject,
            executor: Arc::new(executor),
        })
    }

    pub async fn serve(
        self,
        mut shutdown: tokio::sync::watch::Receiver<bool>,
    ) -> Result<(), KubeVirtExecutorFenceError> {
        let mut subscriber = self
            .client
            .subscribe(self.subject)
            .await
            .map_err(|_| KubeVirtExecutorFenceError::Transport)?;
        let mut tasks = tokio::task::JoinSet::new();
        let result = loop {
            if *shutdown.borrow() {
                break Ok(());
            }
            tokio::select! {
                biased;
                _ = shutdown.changed() => break Ok(()),
                result = tasks.join_next(), if !tasks.is_empty() => {
                    if result.is_some_and(|result|result.is_err()) {break Err(KubeVirtExecutorFenceError::Transport);}
                }
                message = subscriber.next() => {
                    let Some(message)=message else {break Err(KubeVirtExecutorFenceError::Transport);};
                    let Some(reply)=message.reply else {
                        tracing::warn!(event="environment.kubevirt_executor.request_rejected",reason="replyRequired");
                        continue;
                    };
                    if message.payload.len()>MAX_RESPONSE_BYTES {
                        tracing::warn!(event="environment.kubevirt_executor.request_rejected",reason="payloadTooLarge");
                        continue;
                    }
                    let Ok(envelope)=serde_json::from_slice::<KubeVirtExecutorRequestEnvelope>(&message.payload) else {
                        tracing::warn!(event="environment.kubevirt_executor.request_rejected",reason="contractInvalid");
                        continue;
                    };
                    let client=self.client.clone();let executor=Arc::clone(&self.executor);
                    tasks.spawn(async move {
                        let fence=envelope.fence.clone();
                        let response=executor.execute(envelope).await.unwrap_or_else(|error|kubevirt_response_envelope(&fence,KubeVirtExecutorResponse::Failed {failure:kubevirt_executor_failure(&error)}));
                        let Ok(payload)=serde_json::to_vec(&response) else {return;};
                        if client.publish(reply,payload.into()).await.is_err() {
                            tracing::warn!(event="environment.kubevirt_executor.response_failed",diagnostic="LW_ENVIRONMENT_KUBEVIRT_EXECUTOR_TRANSPORT_FAILED");
                        }
                    });
                }
            }
        };
        // Dropping the subscription rejects new work; accepted tasks still retain their exact owner.
        drop(subscriber);
        self.executor.cancel_active();
        let mut task_failed = false;
        while let Some(task) = tasks.join_next().await {
            task_failed |= task.is_err();
        }
        let completion = self.executor.finish_terminals().await;
        if completion.is_err() {
            tracing::error!(
                event = "environment.kubevirt_executor.drain_failed",
                diagnostic = "LW_ENVIRONMENT_KUBEVIRT_TERMINAL_PERSIST_FAILED"
            );
        }
        result?;
        if task_failed {
            return Err(KubeVirtExecutorFenceError::Transport);
        }
        completion
    }
}

fn validate_kubevirt_executor_request(
    envelope: &KubeVirtExecutorRequestEnvelope,
) -> Result<(), KubeVirtExecutorFenceError> {
    let fence = &envelope.fence;
    let expected = kubevirt_executor_request_id(fence, &envelope.request)
        .map_err(|_| KubeVirtExecutorFenceError::IdentityMismatch)?;
    if fence.protocol_version != KUBEVIRT_BACKEND_PROTOCOL_VERSION
        || fence.environment_generation == 0
        || fence.request_id != expected
        || !envelope.request.matches_action(fence.action)
        || kubevirt_executor_environment_id(&envelope.request) != fence.environment_id
    {
        return Err(KubeVirtExecutorFenceError::IdentityMismatch);
    }
    Ok(())
}

const fn kubevirt_executor_failure(error: &KubeVirtExecutorFenceError) -> ProviderFailure {
    match error {
        KubeVirtExecutorFenceError::InProgress
        | KubeVirtExecutorFenceError::Database(_)
        | KubeVirtExecutorFenceError::Transport => unavailable(),
        KubeVirtExecutorFenceError::DeadlineExceeded => ProviderFailure {
            code: ProviderFailureCode::Timeout,
            retryable: false,
        },
        KubeVirtExecutorFenceError::Cancelled => ProviderFailure {
            code: ProviderFailureCode::Cancelled,
            retryable: false,
        },
        _ => configuration_invalid(),
    }
}

const fn kubevirt_action_name(action: ReconcileAction) -> &'static str {
    match action {
        ReconcileAction::Validate => "validate",
        ReconcileAction::Build => "build",
        ReconcileAction::Provision => "provision",
        ReconcileAction::Observe => "observe",
        ReconcileAction::Start => "start",
        ReconcileAction::Stop => "stop",
        ReconcileAction::Restart => "restart",
        ReconcileAction::Reset => "reset",
        ReconcileAction::Configure => "configure",
        ReconcileAction::Cleanup => "cleanup",
    }
}

#[derive(Debug, thiserror::Error)]
pub enum KubeVirtExecutorFenceError {
    #[error("LW_ENVIRONMENT_KUBEVIRT_EXECUTOR_CONFIGURATION_INVALID")]
    ConfigurationInvalid,
    #[error("LW_ENVIRONMENT_KUBEVIRT_EXECUTOR_IDENTITY_MISMATCH")]
    IdentityMismatch,
    #[error("LW_ENVIRONMENT_KUBEVIRT_EXECUTOR_DEADLINE_EXCEEDED")]
    DeadlineExceeded,
    #[error("LW_ENVIRONMENT_KUBEVIRT_EXECUTOR_CANCELLED")]
    Cancelled,
    #[error("LW_ENVIRONMENT_KUBEVIRT_EXECUTOR_STALE_GENERATION")]
    StaleGeneration,
    #[error("LW_ENVIRONMENT_KUBEVIRT_EXECUTOR_TOMBSTONED")]
    Tombstoned,
    #[error("LW_ENVIRONMENT_KUBEVIRT_EXECUTOR_REQUEST_IN_PROGRESS")]
    InProgress,
    #[error("LW_ENVIRONMENT_KUBEVIRT_EXECUTOR_DATABASE_FAILED")]
    Database(#[from] sqlx::Error),
    #[error("LW_ENVIRONMENT_KUBEVIRT_EXECUTOR_TRANSPORT_FAILED")]
    Transport,
}

/// Deployment-owned mapping from a reviewed base-disk binding to one CDI `DataSource`,
/// storage class, guest principal and imported disk identity.
#[derive(Clone, Debug)]
pub struct KubeVirtBaseDiskBinding {
    pub binding: String,
    pub source_registry_digest: String,
    pub disk_sha256: String,
    pub capacity_bytes: u64,
    pub format: VirtualMachineDiskFormat,
    pub storage_class_binding: String,
    pub storage_class_name: String,
    pub data_source_namespace: String,
    pub data_source_name: String,
    pub guest_user: String,
    pub ssh_port: u16,
}

impl KubeVirtBaseDiskBinding {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        binding: String,
        source_registry_digest: String,
        disk_sha256: String,
        capacity_bytes: u64,
        format: VirtualMachineDiskFormat,
        storage_class_binding: String,
        storage_class_name: String,
        data_source_namespace: String,
        data_source_name: String,
        guest_user: String,
        ssh_port: u16,
    ) -> Result<Self, ReleaseProjectionError> {
        let disk = VirtualMachineBaseDisk {
            binding: binding.clone(),
            source_registry_digest: source_registry_digest.clone(),
            capacity_bytes,
        };
        if !valid_binding(&binding)
            || !valid_binding(&storage_class_binding)
            || !valid_dns_label(&storage_class_name)
            || !valid_dns_label(&data_source_namespace)
            || !valid_dns_label(&data_source_name)
            || !valid_guest_user(&guest_user)
            || ssh_port == 0
            || disk.validate().is_err()
            || disk_sha256.len() != 64
            || !disk_sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(ReleaseProjectionError::ConfigurationInvalid);
        }
        Ok(Self {
            binding,
            source_registry_digest,
            disk_sha256,
            capacity_bytes,
            format,
            storage_class_binding,
            storage_class_name,
            data_source_namespace,
            data_source_name,
            guest_user,
            ssh_port,
        })
    }
}

/// Public-only SSH bootstrap material. No private key or per-user credential is accepted.
#[derive(Clone, Debug)]
pub struct KubeVirtSshBootstrap {
    pub gateway_namespace: String,
    pub gateway_pod_label: String,
    pub collector_namespace: String,
    pub collector_pod_label: String,
    /// Optional exact ingress selector for Evaluation execution Jobs. Freeze
    /// collectors keep their own selector because they use a different SSH
    /// principal and protocol.
    pub evaluation_namespace: Option<String>,
    pub evaluation_pod_label: Option<String>,
    pub user_ca_public_key: String,
}

impl KubeVirtSshBootstrap {
    pub fn new(
        gateway_namespace: String,
        gateway_pod_label: String,
        collector_namespace: String,
        collector_pod_label: String,
        user_ca_public_key: &str,
    ) -> Result<Self, ReleaseProjectionError> {
        let public_key = validate_ssh_public_key(user_ca_public_key)
            .map_err(|_| ReleaseProjectionError::ConfigurationInvalid)?;
        if !valid_dns_label(&gateway_namespace)
            || !valid_dns_label(&gateway_pod_label)
            || !valid_dns_label(&collector_namespace)
            || !valid_dns_label(&collector_pod_label)
        {
            return Err(ReleaseProjectionError::ConfigurationInvalid);
        }
        Ok(Self {
            gateway_namespace,
            gateway_pod_label,
            collector_namespace,
            collector_pod_label,
            evaluation_namespace: None,
            evaluation_pod_label: None,
            user_ca_public_key: public_key.normalized_openssh,
        })
    }

    /// Adds the exact namespace and pod label allowed to reach the VM for
    /// Evaluation execution. Both values are deployment-owned selectors.
    pub fn with_evaluation_ingress(
        mut self,
        evaluation_namespace: String,
        evaluation_pod_label: String,
    ) -> Result<Self, ReleaseProjectionError> {
        if !valid_dns_label(&evaluation_namespace) || !valid_dns_label(&evaluation_pod_label) {
            return Err(ReleaseProjectionError::ConfigurationInvalid);
        }
        self.evaluation_namespace = Some(evaluation_namespace);
        self.evaluation_pod_label = Some(evaluation_pod_label);
        Ok(self)
    }
}

/// Reviewed non-secret configuration for one exact `KubeVirt` Provider binding.
#[derive(Clone, Debug)]
pub struct KubeVirtProviderConfiguration {
    pub trust_revision: Revision,
    pub base_disks: Vec<KubeVirtBaseDiskBinding>,
    /// Optional runtime policy admitting release-declared base disks that are not seeded.
    pub runtime_vm_base: Option<RuntimeVmBasePolicy>,
    /// Optional deployment-owned VM vGPU licensing configuration. A VM vGPU plan requires it.
    pub vm_vgpu_licensing: Option<KubeVirtVmVgpuLicensingConfiguration>,
    pub ssh: KubeVirtSshBootstrap,
    pub resource_budget: KubeVirtResourceBudget,
}

/// Deployment-owned capacity reserved beyond the approved guest resources.
#[derive(Clone, Copy, Debug)]
pub struct KubeVirtResourceBudget {
    vmi_memory_overhead_bytes: u64,
    cdi_importer_cpu_request_millicores: u32,
    cdi_importer_cpu_limit_millicores: u32,
    cdi_importer_memory_request_bytes: u64,
    cdi_importer_memory_limit_bytes: u64,
    cdi_scratch_storage_bytes: u64,
}

impl KubeVirtResourceBudget {
    pub const fn new(
        vmi_memory_overhead_bytes: u64,
        cdi_importer_cpu_request_millicores: u32,
        cdi_importer_cpu_limit_millicores: u32,
        cdi_importer_memory_request_bytes: u64,
        cdi_importer_memory_limit_bytes: u64,
        cdi_scratch_storage_bytes: u64,
    ) -> Result<Self, ReleaseProjectionError> {
        if vmi_memory_overhead_bytes == 0
            || cdi_importer_cpu_request_millicores == 0
            || cdi_importer_cpu_limit_millicores < cdi_importer_cpu_request_millicores
            || cdi_importer_memory_request_bytes == 0
            || cdi_importer_memory_limit_bytes < cdi_importer_memory_request_bytes
            || cdi_scratch_storage_bytes == 0
        {
            return Err(ReleaseProjectionError::ConfigurationInvalid);
        }
        Ok(Self {
            vmi_memory_overhead_bytes,
            cdi_importer_cpu_request_millicores,
            cdi_importer_cpu_limit_millicores,
            cdi_importer_memory_request_bytes,
            cdi_importer_memory_limit_bytes,
            cdi_scratch_storage_bytes,
        })
    }
}

impl KubeVirtProviderConfiguration {
    pub fn new(
        trust_revision: Revision,
        base_disks: Vec<KubeVirtBaseDiskBinding>,
        runtime_vm_base: Option<RuntimeVmBasePolicy>,
        ssh: KubeVirtSshBootstrap,
        resource_budget: KubeVirtResourceBudget,
    ) -> Result<Self, ReleaseProjectionError> {
        if base_disks.is_empty()
            || base_disks.iter().enumerate().any(|(index, entry)| {
                base_disks[..index]
                    .iter()
                    .any(|other| other.binding == entry.binding)
            })
        {
            return Err(ReleaseProjectionError::ConfigurationInvalid);
        }
        Ok(Self {
            trust_revision,
            base_disks,
            runtime_vm_base,
            vm_vgpu_licensing: None,
            ssh,
            resource_budget,
        })
    }

    /// Adds the optional deployment-owned VM vGPU licensing configuration.
    pub fn with_vm_vgpu_licensing(
        mut self,
        licensing: Option<KubeVirtVmVgpuLicensingConfiguration>,
    ) -> Result<Self, ReleaseProjectionError> {
        if let Some(configuration) = licensing.as_ref() {
            configuration.validate()?;
        }
        self.vm_vgpu_licensing = licensing;
        Ok(self)
    }

    fn base_disk_binding(&self, binding: &str) -> Option<&KubeVirtBaseDiskBinding> {
        self.base_disks
            .iter()
            .find(|entry| entry.binding == binding)
    }
}

/// Environment-owned durable VM identity projection, independent from executor memory.
#[async_trait]
pub trait KubeVirtObservationStore: Send + Sync {
    async fn record_running(
        &self,
        fence: &KubeVirtBackendFence,
        plan: &KubeVirtResourcePlan,
        observation: &KubeVirtRunningObservation,
    ) -> Result<(), KubeVirtObservationStoreError>;

    async fn record_stopped(
        &self,
        fence: &KubeVirtBackendFence,
        plan: &KubeVirtCleanupPlan,
        observation: &KubeVirtStoppedObservation,
    ) -> Result<(), KubeVirtObservationStoreError>;

    async fn record_deleted(
        &self,
        fence: &KubeVirtBackendFence,
        plan: &KubeVirtCleanupPlan,
        cleanup_evidence: &ArtifactRef,
    ) -> Result<(), KubeVirtObservationStoreError>;
}

/// `PostgreSQL` projection of the last accepted `KubeVirt` identity and deletion tombstone.
#[derive(Clone)]
pub struct PgKubeVirtObservationStore {
    pool: PgPool,
}

impl PgKubeVirtObservationStore {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    async fn record(
        &self,
        fence: &KubeVirtBackendFence,
        mut record: ObservationRecord,
    ) -> Result<(), KubeVirtObservationStoreError> {
        let mut transaction = self.pool.begin().await?;
        // `SELECT .. FOR UPDATE` cannot lock an absent first-observation row.
        // Serialize that insert race on the full environment identity before
        // reading so an older first completion cannot win `ON CONFLICT` last.
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1::text, 530001))")
            .bind(fence.environment_id.as_uuid())
            .fetch_one(&mut *transaction)
            .await?;
        let existing = sqlx::query(
            "SELECT state,environment_generation,attempt,provider_step,request_id, \
                    observation_sha256,vm_uid,root_disk_uid,ssh_host_key_sha256 \
             FROM environment.kubevirt_runtime_observations \
             WHERE environment_id=$1 FOR UPDATE",
        )
        .bind(fence.environment_id.as_uuid())
        .fetch_optional(&mut *transaction)
        .await?
        .map(|row| decode_stored_observation(&row))
        .transpose()?;

        if let Some(existing) = &existing {
            if existing.request_id == fence.request_id {
                if existing.state == record.state
                    && existing.observation_sha256 == record.observation_sha256
                {
                    transaction.commit().await?;
                    return Ok(());
                }
                return Err(KubeVirtObservationStoreError::IdentityMismatch);
            }
            if existing.state == "deleted" {
                return Err(KubeVirtObservationStoreError::Tombstoned);
            }
            if fence_tuple(fence)? <= existing.fence_tuple {
                return Err(KubeVirtObservationStoreError::StaleFence);
            }
        }
        validate_record_transition(existing.as_ref(), fence.action, &record)?;
        if let Some(existing) = &existing {
            record.vm_uid = record.vm_uid.or(existing.vm_uid);
            record.root_disk_uid = record.root_disk_uid.or(existing.root_disk_uid);
            if record.ssh_host_key_sha256.is_none() {
                record.ssh_host_key_sha256 = existing
                    .ssh_host_key_sha256
                    .map(|digest| digest.to_string());
            }
        }

        sqlx::query(
            "INSERT INTO environment.kubevirt_runtime_observations \
             (environment_id,state,operation_id,provider_step,environment_generation,attempt,request_id, \
              namespace,virtual_machine_name,vm_resource_generation,observed_vm_resource_generation, \
              vm_uid,vmi_uid,root_disk_uid,guest_ip,service_cluster_ip,ssh_host_key_sha256, \
              observation_sha256,cleanup_evidence,observed_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19, \
                     COALESCE($20,clock_timestamp())) \
             ON CONFLICT (environment_id) DO UPDATE SET \
              state=EXCLUDED.state,operation_id=EXCLUDED.operation_id,provider_step=EXCLUDED.provider_step, \
              environment_generation=EXCLUDED.environment_generation,attempt=EXCLUDED.attempt, \
              request_id=EXCLUDED.request_id,namespace=EXCLUDED.namespace, \
              virtual_machine_name=EXCLUDED.virtual_machine_name, \
              vm_resource_generation=EXCLUDED.vm_resource_generation, \
              observed_vm_resource_generation=EXCLUDED.observed_vm_resource_generation, \
              vm_uid=COALESCE(EXCLUDED.vm_uid,kubevirt_runtime_observations.vm_uid), \
              vmi_uid=EXCLUDED.vmi_uid, \
              root_disk_uid=COALESCE(EXCLUDED.root_disk_uid,kubevirt_runtime_observations.root_disk_uid), \
              guest_ip=EXCLUDED.guest_ip,service_cluster_ip=EXCLUDED.service_cluster_ip, \
              ssh_host_key_sha256=COALESCE(EXCLUDED.ssh_host_key_sha256,kubevirt_runtime_observations.ssh_host_key_sha256), \
              observation_sha256=EXCLUDED.observation_sha256,cleanup_evidence=EXCLUDED.cleanup_evidence, \
              observed_at=EXCLUDED.observed_at,updated_at=clock_timestamp()",
        )
        .bind(fence.environment_id.as_uuid())
        .bind(record.state)
        .bind(fence.operation_id.as_uuid())
        .bind(i32::try_from(fence.provider_step).map_err(|_| KubeVirtObservationStoreError::InvalidObservation)?)
        .bind(i64::try_from(fence.environment_generation).map_err(|_| KubeVirtObservationStoreError::InvalidObservation)?)
        .bind(i32::try_from(fence.attempt).map_err(|_| KubeVirtObservationStoreError::InvalidObservation)?)
        .bind(fence.request_id.to_string())
        .bind(record.namespace)
        .bind(record.virtual_machine_name)
        .bind(record.vm_resource_generation)
        .bind(record.observed_vm_resource_generation)
        .bind(record.vm_uid)
        .bind(record.vmi_uid)
        .bind(record.root_disk_uid)
        .bind(record.guest_ip)
        .bind(record.service_cluster_ip)
        .bind(record.ssh_host_key_sha256)
        .bind(record.observation_sha256.to_string())
        .bind(record.cleanup_evidence)
        .bind(record.observed_at.map(UtcTimestamp::get))
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(())
    }
}

#[async_trait]
impl KubeVirtObservationStore for PgKubeVirtObservationStore {
    async fn record_running(
        &self,
        fence: &KubeVirtBackendFence,
        plan: &KubeVirtResourcePlan,
        observation: &KubeVirtRunningObservation,
    ) -> Result<(), KubeVirtObservationStoreError> {
        if plan.environment_id != fence.environment_id
            || !matches!(
                fence.action,
                ReconcileAction::Provision
                    | ReconcileAction::Observe
                    | ReconcileAction::Start
                    | ReconcileAction::Restart
                    | ReconcileAction::Reset
            )
            || !valid_running_observation(fence.environment_generation, observation)
        {
            return Err(KubeVirtObservationStoreError::InvalidObservation);
        }
        let observation_sha256 = Sha256Digest::of_canonical(observation)
            .map_err(|_| KubeVirtObservationStoreError::InvalidObservation)?;
        self.record(
            fence,
            ObservationRecord {
                state: "running",
                namespace: plan.namespace.clone(),
                virtual_machine_name: plan.virtual_machine_name.clone(),
                vm_resource_generation: Some(as_i64(observation.vm_resource_generation)?),
                observed_vm_resource_generation: Some(as_i64(
                    observation.observed_vm_resource_generation,
                )?),
                vm_uid: Some(observation.vm_uid),
                vmi_uid: Some(observation.vmi_uid),
                root_disk_uid: Some(observation.root_disk_uid),
                guest_ip: Some(observation.guest_ip.to_string()),
                service_cluster_ip: Some(observation.service_cluster_ip.to_string()),
                ssh_host_key_sha256: Some(observation.ssh_host_key_sha256.to_string()),
                observation_sha256,
                cleanup_evidence: None,
                observed_at: Some(observation.observed_at),
            },
        )
        .await
    }

    async fn record_stopped(
        &self,
        fence: &KubeVirtBackendFence,
        plan: &KubeVirtCleanupPlan,
        observation: &KubeVirtStoppedObservation,
    ) -> Result<(), KubeVirtObservationStoreError> {
        if plan.environment_id != fence.environment_id
            || fence.action != ReconcileAction::Stop
            || !valid_stopped_observation(fence.environment_generation, observation)
        {
            return Err(KubeVirtObservationStoreError::InvalidObservation);
        }
        let observation_sha256 = Sha256Digest::of_canonical(observation)
            .map_err(|_| KubeVirtObservationStoreError::InvalidObservation)?;
        self.record(
            fence,
            ObservationRecord {
                state: "stopped",
                namespace: plan.namespace.clone(),
                virtual_machine_name: plan.virtual_machine_name.clone(),
                vm_resource_generation: None,
                observed_vm_resource_generation: None,
                vm_uid: Some(observation.vm_uid),
                vmi_uid: None,
                root_disk_uid: Some(observation.root_disk_uid),
                guest_ip: None,
                service_cluster_ip: None,
                ssh_host_key_sha256: None,
                observation_sha256,
                cleanup_evidence: None,
                observed_at: Some(observation.observed_at),
            },
        )
        .await
    }

    async fn record_deleted(
        &self,
        fence: &KubeVirtBackendFence,
        plan: &KubeVirtCleanupPlan,
        cleanup_evidence: &ArtifactRef,
    ) -> Result<(), KubeVirtObservationStoreError> {
        if plan.environment_id != fence.environment_id
            || fence.action != ReconcileAction::Cleanup
            || !valid_artifact_ref(cleanup_evidence)
        {
            return Err(KubeVirtObservationStoreError::InvalidObservation);
        }
        let observation_sha256 = Sha256Digest::of_canonical(&json!({
            "plan": plan,
            "cleanupEvidence": cleanup_evidence,
        }))
        .map_err(|_| KubeVirtObservationStoreError::InvalidObservation)?;
        self.record(
            fence,
            ObservationRecord {
                state: "deleted",
                namespace: plan.namespace.clone(),
                virtual_machine_name: plan.virtual_machine_name.clone(),
                vm_resource_generation: None,
                observed_vm_resource_generation: None,
                vm_uid: None,
                vmi_uid: None,
                root_disk_uid: None,
                guest_ip: None,
                service_cluster_ip: None,
                ssh_host_key_sha256: None,
                observation_sha256,
                cleanup_evidence: Some(
                    serde_json::to_value(cleanup_evidence)
                        .map_err(|_| KubeVirtObservationStoreError::InvalidObservation)?,
                ),
                observed_at: None,
            },
        )
        .await
    }
}

struct ObservationRecord {
    state: &'static str,
    namespace: String,
    virtual_machine_name: String,
    vm_resource_generation: Option<i64>,
    observed_vm_resource_generation: Option<i64>,
    vm_uid: Option<Uuid>,
    vmi_uid: Option<Uuid>,
    root_disk_uid: Option<Uuid>,
    guest_ip: Option<String>,
    service_cluster_ip: Option<String>,
    ssh_host_key_sha256: Option<String>,
    observation_sha256: Sha256Digest,
    cleanup_evidence: Option<Value>,
    observed_at: Option<UtcTimestamp>,
}

struct StoredObservation {
    state: String,
    fence_tuple: (i64, i32, i32),
    request_id: Sha256Digest,
    observation_sha256: Sha256Digest,
    vm_uid: Option<Uuid>,
    root_disk_uid: Option<Uuid>,
    ssh_host_key_sha256: Option<Sha256Digest>,
}

fn decode_stored_observation(
    row: &sqlx::postgres::PgRow,
) -> Result<StoredObservation, KubeVirtObservationStoreError> {
    Ok(StoredObservation {
        state: row.try_get("state")?,
        fence_tuple: (
            row.try_get("environment_generation")?,
            row.try_get("attempt")?,
            row.try_get("provider_step")?,
        ),
        request_id: row
            .try_get::<String, _>("request_id")?
            .parse()
            .map_err(|_| KubeVirtObservationStoreError::InvalidObservation)?,
        observation_sha256: row
            .try_get::<String, _>("observation_sha256")?
            .parse()
            .map_err(|_| KubeVirtObservationStoreError::InvalidObservation)?,
        vm_uid: row.try_get("vm_uid")?,
        root_disk_uid: row.try_get("root_disk_uid")?,
        ssh_host_key_sha256: row
            .try_get::<Option<String>, _>("ssh_host_key_sha256")?
            .map(|value| value.parse())
            .transpose()
            .map_err(|_| KubeVirtObservationStoreError::InvalidObservation)?,
    })
}

fn validate_record_transition(
    existing: Option<&StoredObservation>,
    action: ReconcileAction,
    record: &ObservationRecord,
) -> Result<(), KubeVirtObservationStoreError> {
    match record.state {
        "deleted" => Ok(()),
        "stopped" => {
            let existing = existing.ok_or(KubeVirtObservationStoreError::IdentityMismatch)?;
            if !matches!(existing.state.as_str(), "running" | "stopped")
                || existing.vm_uid != record.vm_uid
                || existing.root_disk_uid != record.root_disk_uid
            {
                return Err(KubeVirtObservationStoreError::IdentityMismatch);
            }
            Ok(())
        }
        "running" => {
            if let Some(existing) = existing {
                if action == ReconcileAction::Start && existing.state != "stopped" {
                    return Err(KubeVirtObservationStoreError::IdentityMismatch);
                }
                let replacing_disk = action == ReconcileAction::Reset;
                let record_host_key = record
                    .ssh_host_key_sha256
                    .as_deref()
                    .and_then(|value| value.parse().ok());
                if !replacing_disk
                    && (existing.vm_uid != record.vm_uid
                        || existing.root_disk_uid != record.root_disk_uid
                        || existing.ssh_host_key_sha256 != record_host_key)
                {
                    return Err(KubeVirtObservationStoreError::IdentityMismatch);
                }
            } else if !matches!(
                action,
                ReconcileAction::Provision | ReconcileAction::Reset | ReconcileAction::Observe
            ) {
                return Err(KubeVirtObservationStoreError::IdentityMismatch);
            }
            Ok(())
        }
        _ => Err(KubeVirtObservationStoreError::InvalidObservation),
    }
}

fn fence_tuple(
    fence: &KubeVirtBackendFence,
) -> Result<(i64, i32, i32), KubeVirtObservationStoreError> {
    Ok((
        as_i64(fence.environment_generation)?,
        i32::try_from(fence.attempt)
            .map_err(|_| KubeVirtObservationStoreError::InvalidObservation)?,
        i32::try_from(fence.provider_step)
            .map_err(|_| KubeVirtObservationStoreError::InvalidObservation)?,
    ))
}

fn as_i64(value: u64) -> Result<i64, KubeVirtObservationStoreError> {
    i64::try_from(value).map_err(|_| KubeVirtObservationStoreError::InvalidObservation)
}

#[derive(Debug, thiserror::Error)]
pub enum KubeVirtObservationStoreError {
    #[error("LW_ENVIRONMENT_KUBEVIRT_OBSERVATION_INVALID")]
    InvalidObservation,
    #[error("LW_ENVIRONMENT_KUBEVIRT_OBSERVATION_IDENTITY_MISMATCH")]
    IdentityMismatch,
    #[error("LW_ENVIRONMENT_KUBEVIRT_FENCE_STALE")]
    StaleFence,
    #[error("LW_ENVIRONMENT_KUBEVIRT_TOMBSTONED")]
    Tombstoned,
    #[error("LW_ENVIRONMENT_KUBEVIRT_OBSERVATION_DATABASE_FAILED")]
    Database(#[from] sqlx::Error),
}

/// `KubeVirt` implementation of the frozen Environment Provider state machine.
pub struct KubeVirtProvider<B, R, S> {
    binding: String,
    backend: Arc<B>,
    releases: Arc<R>,
    observations: Arc<S>,
    configuration: KubeVirtProviderConfiguration,
    /// Distinct runtime base-disk registry digests this process has admitted. Control owns the
    /// authoritative deployment-wide bound; this is the Environment-side process-local guard.
    runtime_bases: Mutex<BTreeSet<String>>,
}

impl<B, R, S> KubeVirtProvider<B, R, S>
where
    B: KubeVirtProviderBackend,
    R: ContainerReleaseResolver,
    S: KubeVirtObservationStore,
{
    pub fn new(
        binding: String,
        backend: Arc<B>,
        releases: Arc<R>,
        observations: Arc<S>,
        configuration: KubeVirtProviderConfiguration,
    ) -> Result<Self, ReleaseProjectionError> {
        if !valid_binding(&binding) {
            return Err(ReleaseProjectionError::ConfigurationInvalid);
        }
        Ok(Self {
            binding,
            backend,
            releases,
            observations,
            configuration,
            runtime_bases: Mutex::new(BTreeSet::new()),
        })
    }

    /// Resolves the exact base-disk binding for one VM resource plan.
    ///
    /// Precedence: a deployment-seeded `baseDisks[]` entry wins and keeps its reviewed raw-disk
    /// SHA-256 identity. A binding that is not seeded resolves through the optional runtime policy
    /// and is identified by its declared registry manifest digest. Without the runtime policy an
    /// unseeded binding fails closed.
    fn resolve_base_disk(
        &self,
        base_disk: &VirtualMachineBaseDisk,
        format: VirtualMachineDiskFormat,
    ) -> Result<ResolvedVmBaseDisk, ReleaseProjectionError> {
        if let Some(seeded) = self.configuration.base_disk_binding(&base_disk.binding) {
            return Ok(ResolvedVmBaseDisk {
                binding: seeded.clone(),
                identity: KubeVirtBaseDiskIdentity::ReviewedDiskSha256,
            });
        }
        let Some(policy) = self.configuration.runtime_vm_base.as_ref() else {
            return Err(ReleaseProjectionError::SecurityPostureInvalid);
        };
        if base_disk.validate().is_err() {
            return Err(ReleaseProjectionError::VmBaseImportFailed);
        }
        if base_disk.capacity_bytes > policy.max_capacity_bytes {
            return Err(ReleaseProjectionError::VmBaseCapacityExceeded);
        }
        let mut admitted = self
            .runtime_bases
            .lock()
            .map_err(|_| ReleaseProjectionError::VmBaseImportFailed)?;
        let identity = base_disk.source_registry_digest.clone();
        if !admitted.contains(&identity)
            && admitted.len() >= usize::try_from(policy.max_bases).unwrap_or(usize::MAX)
        {
            return Err(ReleaseProjectionError::VmBaseImportFailed);
        }
        let binding = policy
            .runtime_binding(base_disk, format)
            .ok_or(ReleaseProjectionError::VmBaseImportFailed)?;
        admitted.insert(identity);
        Ok(ResolvedVmBaseDisk {
            binding,
            identity: KubeVirtBaseDiskIdentity::RuntimeRegistryDigest,
        })
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the complete deterministic KubeVirt security bundle is reviewed as one projection"
    )]
    pub fn plan(
        &self,
        instance: &EnvironmentInstance,
        resolved: &ResolvedContainerRelease,
        action: ReconcileAction,
    ) -> Result<KubeVirtResourcePlan, ReleaseProjectionError> {
        let projection = &resolved.projection;
        projection
            .validate()
            .map_err(|_| ReleaseProjectionError::ContractInvalid)?;
        self.validate_release_use(resolved, action)?;
        if instance.runtime_kind != RuntimeKind::VirtualMachine
            || instance.release_id != projection.release.id
            || instance.release_version != projection.release.version
            || instance.project_id != projection.release.project_id
            || instance.course_id != projection.release.course_id
            || instance.provider_binding != self.binding
        {
            return Err(ReleaseProjectionError::IdentityMismatch);
        }
        let EnvironmentRuntimeSpec::VirtualMachine {
            provider_binding,
            base_disk: spec_base_disk,
            storage_class_binding,
            ssh_port,
        } = &projection.environment_spec.runtime
        else {
            return Err(ReleaseProjectionError::IdentityMismatch);
        };
        let ImageArtifact::VirtualMachine {
            base_disk, format, ..
        } = &projection.release.artifact
        else {
            return Err(ReleaseProjectionError::IdentityMismatch);
        };
        let resolved_base = self.resolve_base_disk(base_disk, *format)?;
        let base_binding = &resolved_base.binding;
        if provider_binding != &self.binding
            || storage_class_binding != &base_binding.storage_class_binding
            || *ssh_port != base_binding.ssh_port
            || spec_base_disk != base_disk
            || base_binding.source_registry_digest != base_disk.source_registry_digest
            || base_binding.capacity_bytes != base_disk.capacity_bytes
            || base_binding.format != *format
            || projection.environment_spec.security.user_policy
                != RuntimeUserPolicy::NonRootRequired
            || projection.environment_spec.security.root_filesystem_policy
                != RootFilesystemPolicy::MutableRequired
            || projection
                .environment_spec
                .security
                .privilege_escalation_policy
                != PrivilegeEscalationPolicy::Deny
            || projection.environment_spec.security.public_exposure_policy
                != PublicExposurePolicy::Deny
            || base_disk.validate().is_err()
        {
            return Err(ReleaseProjectionError::SecurityPostureInvalid);
        }
        let [entry] = projection.environment_spec.entries.as_slice() else {
            return Err(ReleaseProjectionError::SecurityPostureInvalid);
        };
        if entry.protocol != contracts::environment::EndpointProtocol::Ssh
            || entry.service_port != *ssh_port
        {
            return Err(ReleaseProjectionError::SecurityPostureInvalid);
        }

        let namespace = format!("lw-env-{}", instance.id);
        let virtual_machine_name = "runtime".to_owned();
        let data_volume_name = "rootdisk".to_owned();
        let mut labels = json!({
            "app.kubernetes.io/name": "labweaver-vm-runtime",
            "labweaver.io/environment-id": instance.id.to_string(),
            "labweaver.io/project-id": instance.project_id.to_string(),
            "labweaver.io/managed": "true",
            "labweaver.io/environment": "true",
        });
        if let Some(course_id) = instance.course_id {
            labels["labweaver.io/course-id"] = json!(course_id.to_string());
        }
        let mut annotations = json!({
            "labweaver.io/release-id": projection.release.id.to_string(),
            "labweaver.io/release-version": projection.release.version.to_string(),
            "labweaver.io/base-disk-binding": base_disk.binding,
            "labweaver.io/base-disk-source-registry": base_disk.source_registry_digest,
            "labweaver.io/base-disk-identity": resolved_base.identity.as_str(),
            "labweaver.io/environment-generation": instance.generation.to_string(),
        });
        // A runtime-registered base is identified by its declared registry digest; the reviewed
        // raw-disk SHA-256 annotation belongs only to a deployment-seeded reviewed base.
        if resolved_base.identity == KubeVirtBaseDiskIdentity::ReviewedDiskSha256 {
            annotations["labweaver.io/base-disk-sha256"] = json!(base_binding.disk_sha256);
        }
        let (cpu_millicores, memory_bytes, storage_bytes, gpu_allocation) =
            approved_resources(instance, projection)?;
        let vm_vgpu = gpu_allocation
            .as_ref()
            .is_some_and(|allocation| allocation.mode == GpuAllocationMode::VmVgpu);
        let vm_vgpu_licensing = if vm_vgpu {
            Some(
                self.configuration
                    .vm_vgpu_licensing
                    .clone()
                    .ok_or(ReleaseProjectionError::SecurityPostureInvalid)?,
            )
        } else {
            None
        };
        if let Some(licensing) = vm_vgpu_licensing.as_ref() {
            licensing.validate()?;
            labels["labweaver.io/gpu-mode"] = json!("vm_vgpu");
        }
        let cpu = format!("{cpu_millicores}m");
        let memory = memory_bytes.to_string();
        let storage = storage_bytes.to_string();
        let budget = self.configuration.resource_budget;
        if storage_bytes > budget.cdi_scratch_storage_bytes {
            return Err(ReleaseProjectionError::SecurityPostureInvalid);
        }
        let quota_cpu_request_millicores = cpu_millicores
            .checked_add(budget.cdi_importer_cpu_request_millicores)
            .ok_or(ReleaseProjectionError::SecurityPostureInvalid)?;
        let quota_cpu_limit_millicores = cpu_millicores
            .checked_add(budget.cdi_importer_cpu_limit_millicores)
            .ok_or(ReleaseProjectionError::SecurityPostureInvalid)?;
        let vmi_memory_limit_bytes = memory_bytes
            .checked_add(budget.vmi_memory_overhead_bytes)
            .ok_or(ReleaseProjectionError::SecurityPostureInvalid)?;
        let quota_memory_request_bytes = vmi_memory_limit_bytes
            .checked_add(budget.cdi_importer_memory_request_bytes)
            .ok_or(ReleaseProjectionError::SecurityPostureInvalid)?;
        let quota_memory_limit_bytes = vmi_memory_limit_bytes
            .checked_add(budget.cdi_importer_memory_limit_bytes)
            .ok_or(ReleaseProjectionError::SecurityPostureInvalid)?;
        let quota_storage_bytes = storage_bytes
            .checked_add(budget.cdi_scratch_storage_bytes)
            .ok_or(ReleaseProjectionError::SecurityPostureInvalid)?;
        let quota_cpu_request = format!("{quota_cpu_request_millicores}m");
        let quota_cpu_limit = format!("{quota_cpu_limit_millicores}m");
        let quota_memory_request = quota_memory_request_bytes.to_string();
        let quota_memory_limit = quota_memory_limit_bytes.to_string();
        let quota_storage = quota_storage_bytes.to_string();
        let vmi_memory_limit = vmi_memory_limit_bytes.to_string();
        let vmi_requests = json!({"cpu":cpu,"memory":memory});
        let mut vmi_limits = json!({"cpu":cpu,"memory":vmi_memory_limit});
        let mut gpu_devices = Vec::new();
        if let Some(ref allocation) = gpu_allocation {
            // Resource resolves the catalog mode and binding before this projection. VM vGPU
            // entries must use their own extended-resource binding; this provider consumes the
            // exact binding and never treats a node label as a GPU allocation proof.
            if allocation.mode != GpuAllocationMode::VmVgpu
                || allocation.provider_binding != self.binding
                || !valid_extended_resource_name(&allocation.allocation_binding)
            {
                return Err(ReleaseProjectionError::SecurityPostureInvalid);
            }
            let quantity = json!(allocation.count.to_string());
            vmi_limits[&allocation.allocation_binding] = quantity;
            for index in 0..allocation.count {
                gpu_devices.push(json!({
                    "name": format!("runtime-gpu-{index}"),
                    "deviceName": allocation.allocation_binding,
                }));
            }
        }
        let mut pod_labels = json!({
            "app": "runtime",
            "labweaver.io/environment-id": instance.id.to_string(),
            "labweaver.io/project-id": instance.project_id.to_string(),
            "labweaver.io/release-id": projection.release.id.to_string(),
            "labweaver.io/release-version": projection.release.version.to_string()
        });
        if let Some(course_id) = instance.course_id {
            pod_labels["labweaver.io/course-id"] = json!(course_id.to_string());
        }
        if vm_vgpu {
            pod_labels["labweaver.io/gpu-mode"] = json!("vm_vgpu");
        }
        let mut quota_hard = serde_json::Map::from_iter([
            ("requests.cpu".to_owned(), json!(quota_cpu_request)),
            ("limits.cpu".to_owned(), json!(quota_cpu_limit)),
            ("requests.memory".to_owned(), json!(quota_memory_request)),
            ("limits.memory".to_owned(), json!(quota_memory_limit)),
            ("requests.storage".to_owned(), json!(quota_storage)),
            ("persistentvolumeclaims".to_owned(), json!("2")),
            ("pods".to_owned(), json!("2")),
        ]);
        if let Some(allocation) = gpu_allocation {
            quota_hard.insert(
                format!("limits.{}", allocation.allocation_binding),
                json!(allocation.count.to_string()),
            );
        }
        let quota_annotations = json!({
            "labweaver.io/vmi-memory-overhead-bytes": budget.vmi_memory_overhead_bytes.to_string(),
            "labweaver.io/cdi-importer-cpu-request-millicores": budget.cdi_importer_cpu_request_millicores.to_string(),
            "labweaver.io/cdi-importer-cpu-limit-millicores": budget.cdi_importer_cpu_limit_millicores.to_string(),
            "labweaver.io/cdi-importer-memory-request-bytes": budget.cdi_importer_memory_request_bytes.to_string(),
            "labweaver.io/cdi-importer-memory-limit-bytes": budget.cdi_importer_memory_limit_bytes.to_string(),
            "labweaver.io/cdi-scratch-storage-bytes": budget.cdi_scratch_storage_bytes.to_string(),
        });
        let cloud_init = self.cloud_init_user_data(&base_binding.guest_user);
        let cloud_init_data = BASE64_STANDARD.encode(cloud_init.as_bytes());
        let cloud_init_network_data = BASE64_STANDARD.encode(
            b"version: 2\nethernets:\n  default:\n    match:\n      name: \"en*\"\n    dhcp4: true\n",
        );
        let mut ssh_ingress = vec![
            json!({
                "namespaceSelector":{"matchLabels":{"kubernetes.io/metadata.name":self.configuration.ssh.gateway_namespace}},
                "podSelector":{"matchLabels":{GATEWAY_LABEL_KEY:self.configuration.ssh.gateway_pod_label}}
            }),
            json!({
                "namespaceSelector":{"matchLabels":{"kubernetes.io/metadata.name":self.configuration.ssh.collector_namespace}},
                "podSelector":{"matchLabels":{GATEWAY_LABEL_KEY:self.configuration.ssh.collector_pod_label}}
            }),
        ];
        if let (Some(namespace), Some(pod_label)) = (
            &self.configuration.ssh.evaluation_namespace,
            &self.configuration.ssh.evaluation_pod_label,
        ) {
            ssh_ingress.push(json!({
                "namespaceSelector":{"matchLabels":{"kubernetes.io/metadata.name":namespace}},
                "podSelector":{"matchLabels":{GATEWAY_LABEL_KEY:pod_label}}
            }));
        }
        ssh_ingress.push(json!({
            "namespaceSelector":{"matchLabels":{"kubernetes.io/metadata.name":base_binding.data_source_namespace}},
            "podSelector":{"matchLabels":{GATEWAY_LABEL_KEY:"kubevirt-executor"}}
        }));
        let mut documents = vec![
            resource(
                "Namespace",
                None,
                &namespace,
                json!({
                    "apiVersion":"v1","kind":"Namespace",
                    "metadata":{"name":namespace,"labels":labels,"finalizers":["labweaver.io/environment-cleanup"]}
                }),
            ),
            resource(
                "ResourceQuota",
                Some(&namespace),
                "runtime-quota",
                json!({
                    "apiVersion":"v1","kind":"ResourceQuota",
                    "metadata":{"name":"runtime-quota","namespace":namespace,"labels":labels,"annotations":quota_annotations},
                    "spec":{"hard":quota_hard}
                }),
            ),
            resource(
                "NetworkPolicy",
                Some(&namespace),
                "default-deny",
                json!({
                    "apiVersion":"networking.k8s.io/v1","kind":"NetworkPolicy",
                    "metadata":{"name":"default-deny","namespace":namespace,"labels":labels},
                    "spec":{"podSelector":{},"policyTypes":["Ingress","Egress"]}
                }),
            ),
            resource(
                "NetworkPolicy",
                Some(&namespace),
                "openssh-gateway-ingress",
                json!({
                    "apiVersion":"networking.k8s.io/v1","kind":"NetworkPolicy",
                    "metadata":{"name":"openssh-gateway-ingress","namespace":namespace,"labels":labels},
                    "spec":{"podSelector":{"matchLabels":{"labweaver.io/environment-id":instance.id.to_string()}},"policyTypes":["Ingress"],"ingress":[{"from":ssh_ingress,"ports":[{"protocol":"TCP","port":ssh_port}]}]}
                }),
            ),
            resource(
                "NetworkPolicy",
                Some(&namespace),
                "cdi-clone-ingress",
                json!({
                    "apiVersion":"networking.k8s.io/v1","kind":"NetworkPolicy",
                    "metadata":{"name":"cdi-clone-ingress","namespace":namespace,"labels":labels},
                    "spec":{
                        "podSelector":{"matchLabels":{"cdi.kubevirt.io":"cdi-upload-server"}},
                        "policyTypes":["Ingress"],
                        "ingress":[{
                            "from":[{
                                "namespaceSelector":{"matchLabels":{"kubernetes.io/metadata.name":base_binding.data_source_namespace}},
                                "podSelector":{"matchLabels":{"cdi.kubevirt.io":"cdi-clone-source"}}
                            }],
                            "ports":[{"protocol":"TCP","port":8443}]
                        }]
                    }
                }),
            ),
            resource(
                "Secret",
                Some(&namespace),
                "cloud-init",
                json!({
                    "apiVersion":"v1","kind":"Secret","type":"Opaque",
                    "metadata":{"name":"cloud-init","namespace":namespace,"labels":labels,"annotations":annotations},
                    "data":{"userdata":cloud_init_data,"networkdata":cloud_init_network_data}
                }),
            ),
            resource(
                "DataVolume",
                Some(&namespace),
                &data_volume_name,
                json!({
                    "apiVersion":"cdi.kubevirt.io/v1beta1","kind":"DataVolume",
                    "metadata":{"name":data_volume_name,"namespace":namespace,"labels":labels,"annotations":annotations},
                    "spec":{
                        "sourceRef":{"kind":"DataSource","namespace":base_binding.data_source_namespace,"name":base_binding.data_source_name},
                        "storage":{"storageClassName":base_binding.storage_class_name,"accessModes":["ReadWriteOnce"],"resources":{"requests":{"storage":storage}}}
                    }
                }),
            ),
            resource(
                "VirtualMachine",
                Some(&namespace),
                &virtual_machine_name,
                json!({
                    "apiVersion":"kubevirt.io/v1","kind":"VirtualMachine",
                    "metadata":{"name":virtual_machine_name,"namespace":namespace,"labels":labels,"annotations":annotations},
                    "spec":{
                        "runStrategy":"Always",
                        "template":{
                            "metadata":{"labels":pod_labels},
                            "spec":{
                                "terminationGracePeriodSeconds":30,
                                "nodeSelector":{KUBEVIRT_NODE_LABEL_KEY:KUBEVIRT_NODE_LABEL_VALUE},
                                "domain":{
                                    "resources":{"requests":vmi_requests,"limits":vmi_limits},
                                    "devices":{
                                        "autoattachGraphicsDevice":true,
                                        "autoattachSerialConsole":true,
                                        "gpus":gpu_devices,
                                        "disks":[
                                            {"name":"rootdisk","disk":{"bus":"virtio"},"bootOrder":1},
                                            {"name":"cloudinit","disk":{"bus":"virtio"}}
                                        ],
                                        "interfaces":[{"name":"default","masquerade":{}}]
                                    }
                                },
                                "networks":[{"name":"default","pod":{}}],
                                "volumes":[
                                    {"name":"rootdisk","persistentVolumeClaim":{"claimName":data_volume_name}},
                                    {"name":"cloudinit","cloudInitNoCloud":{
                                        "secretRef":{"name":"cloud-init"},
                                        "networkDataSecretRef":{"name":"cloud-init"}
                                    }}
                                ]
                            }
                        }
                    }
                }),
            ),
            resource(
                "Service",
                Some(&namespace),
                "ssh",
                json!({
                    "apiVersion":"v1","kind":"Service",
                    "metadata":{"name":"ssh","namespace":namespace,"labels":labels,"annotations":{"labweaver.io/access-controlled":"true"}},
                    "spec":{"type":"ClusterIP","selector":{"labweaver.io/environment-id":instance.id.to_string()},"ports":[{"name":"ssh","protocol":"TCP","port":ssh_port,"targetPort":ssh_port}]}
                }),
            ),
        ];
        if let NetworkPolicySpec::Restricted { policy_binding } =
            &projection.environment_spec.network
        {
            documents.push(resource(
                "NetworkPolicy",
                Some(&namespace),
                "restricted-egress",
                json!({
                    "apiVersion":"networking.k8s.io/v1","kind":"NetworkPolicy",
                    "metadata":{"name":"restricted-egress","namespace":namespace,"labels":labels},
                    "spec":{"podSelector":{"matchLabels":{"labweaver.io/environment-id":instance.id.to_string()}},"policyTypes":["Egress"],"egress":[{"to":[{"namespaceSelector":{"matchLabels":{"labweaver.io/egress-policy":policy_binding}}}]}]}
                }),
            ));
        }
        if vm_vgpu {
            let licensing = vm_vgpu_licensing
                .as_ref()
                .ok_or(ReleaseProjectionError::SecurityPostureInvalid)?;
            documents.push(resource(
                "CiliumNetworkPolicy",
                Some(&namespace),
                "vm-vgpu-license-egress",
                json!({
                    "apiVersion":"cilium.io/v2","kind":"CiliumNetworkPolicy",
                    "metadata":{"name":"vm-vgpu-license-egress","namespace":namespace,"labels":labels},
                    "spec":{"endpointSelector":{"matchLabels":{"labweaver.io/environment-id":instance.id.to_string()}},"egress":vm_vgpu_license_egress(licensing)?}
                }),
            ));
        }
        let plan_sha256 = canonical_hash(&json!({
            "environmentId": instance.id,
            "releaseId": projection.release.id,
            "releaseVersion": projection.release.version,
            "baseDisk": base_disk,
            "format": format,
            "storageClassName": base_binding.storage_class_name,
            "vmVgpuLicensing": &vm_vgpu_licensing,
            "resources": documents,
        }))?;
        Ok(KubeVirtResourcePlan {
            environment_id: instance.id,
            namespace,
            virtual_machine_name,
            data_volume_name,
            base_disk: base_disk.clone(),
            base_disk_format: *format,
            base_disk_identity: resolved_base.identity,
            base_disk_data_source_namespace: base_binding.data_source_namespace.clone(),
            base_disk_data_source_name: base_binding.data_source_name.clone(),
            base_disk_disk_sha256: base_binding.disk_sha256.clone(),
            storage_class_name: base_binding.storage_class_name.clone(),
            vm_vgpu_licensing,
            resources: documents,
            plan_sha256,
        })
    }

    fn validate_release_use(
        &self,
        resolved: &ResolvedContainerRelease,
        action: ReconcileAction,
    ) -> Result<(), ReleaseProjectionError> {
        if action == ReconcileAction::Stop {
            return Ok(());
        }
        let release = &resolved.projection.release;
        if resolved.withdrawn_at.is_some() {
            return Err(ReleaseProjectionError::Withdrawn);
        }
        if release.approval.trust_revision != self.configuration.trust_revision {
            return Err(ReleaseProjectionError::TrustRevisionMismatch);
        }
        Ok(())
    }

    fn cloud_init_user_data(&self, guest_user: &str) -> String {
        format!(
            "#cloud-config\nusers:\n  - name: {user}\n    lock_passwd: true\n    shell: /bin/bash\nwrite_files:\n  - path: /etc/ssh/labweaver_user_ca.pub\n    owner: root:root\n    permissions: '0644'\n    content: |\n      {ca}\n  - path: /etc/ssh/auth_principals/{user}\n    owner: root:root\n    permissions: '0644'\n    content: |\n      labweaver-gateway\n      labweaver-collector\n      labweaver-evaluation\n      labweaver-agent\n  - path: /etc/ssh/sshd_config.d/99-labweaver.conf\n    owner: root:root\n    permissions: '0644'\n    content: |\n      TrustedUserCAKeys /etc/ssh/labweaver_user_ca.pub\n      AuthorizedPrincipalsFile /etc/ssh/auth_principals/%u\n      AuthorizedKeysFile none\n      PubkeyAuthentication yes\n      AuthenticationMethods publickey\n      AllowUsers {user}\n      PasswordAuthentication no\n      KbdInteractiveAuthentication no\n      PermitRootLogin no\n      AllowTcpForwarding no\n      AllowAgentForwarding no\n      PermitTunnel no\n      X11Forwarding no\nruncmd:\n  - [install, -d, -o, {user}, -g, {user}, -m, '0700', /home/{user}/workspace]\n  - [sshd, -t]\n  - [systemctl, enable, --now, ssh.service]\n",
            user = guest_user,
            ca = self.configuration.ssh.user_ca_public_key,
        )
    }

    fn cleanup_plan(
        &self,
        instance: &EnvironmentInstance,
    ) -> Result<KubeVirtCleanupPlan, ReleaseProjectionError> {
        if instance.runtime_kind != RuntimeKind::VirtualMachine
            || instance.provider_binding != self.binding
        {
            return Err(ReleaseProjectionError::IdentityMismatch);
        }
        let namespace = format!("lw-env-{}", instance.id);
        let virtual_machine_name = "runtime".to_owned();
        let plan_sha256 = canonical_hash(&json!({
            "environmentId": instance.id,
            "projectId": instance.project_id,
            "namespace": namespace,
            "virtualMachineName": virtual_machine_name,
            "action": "cleanup",
        }))?;
        Ok(KubeVirtCleanupPlan {
            environment_id: instance.id,
            project_id: instance.project_id,
            namespace,
            virtual_machine_name,
            plan_sha256,
        })
    }
}

#[async_trait]
impl<B, R, S> EnvironmentProvider for KubeVirtProvider<B, R, S>
where
    B: KubeVirtProviderBackend,
    R: ContainerReleaseResolver,
    S: KubeVirtObservationStore,
{
    fn binding(&self) -> &str {
        &self.binding
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one concrete provider action dispatch keeps pending distinct from completed observations"
    )]
    async fn execute(
        &self,
        action: ReconcileAction,
        instance: &EnvironmentInstance,
    ) -> Result<crate::ProviderOutcome<ProviderObservation>, ProviderFailure> {
        let fence = KubeVirtBackendFence::for_action(instance, action)?;
        let no_endpoints = |next_state, operation_complete| {
            ProviderOutcome::Completed(ProviderObservation {
                next_state,
                endpoints: Vec::new(),
                cleanup_evidence: None,
                operation_complete,
            })
        };
        if action == ReconcileAction::Cleanup
            && instance.observed_state == ObservedEnvironmentState::Deleting
        {
            let plan = self
                .cleanup_plan(instance)
                .map_err(|error| projection_failure(&error))?;
            let ProviderOutcome::Completed(cleanup_evidence) =
                self.backend.delete_namespace(&fence, &plan).await?
            else {
                return Ok(ProviderOutcome::Pending);
            };
            if !valid_artifact_ref(&cleanup_evidence) {
                return Err(ProviderFailure {
                    code: ProviderFailureCode::CleanupFailed,
                    retryable: true,
                });
            }
            self.observations
                .record_deleted(&fence, &plan, &cleanup_evidence)
                .await
                .map_err(|error| observation_store_failure(&error))?;
            return Ok(ProviderOutcome::Completed(ProviderObservation {
                next_state: ObservedEnvironmentState::Deleted,
                endpoints: Vec::new(),
                cleanup_evidence: Some(cleanup_evidence),
                operation_complete: true,
            }));
        }
        if action == ReconcileAction::Cleanup
            && instance.observed_state == ObservedEnvironmentState::Stopped
            && instance.desired_state == contracts::environment::DesiredEnvironmentState::Deleted
        {
            return Ok(no_endpoints(ObservedEnvironmentState::Deleting, false));
        }
        if action == ReconcileAction::Stop
            && matches!(
                instance.observed_state,
                ObservedEnvironmentState::Stopping | ObservedEnvironmentState::Expiring
            )
        {
            if instance.desired_state == contracts::environment::DesiredEnvironmentState::Deleted {
                return Ok(no_endpoints(ObservedEnvironmentState::Deleting, false));
            }
            if matches!(
                self.stop_runtime(&fence, instance).await?,
                ProviderOutcome::Pending
            ) {
                return Ok(ProviderOutcome::Pending);
            }
            return Ok(no_endpoints(ObservedEnvironmentState::Stopped, true));
        }
        let resolved = self
            .releases
            .resolve(instance.release_id, instance.release_version)
            .await
            .map_err(|error| projection_failure(&error))?;
        let plan = self
            .plan(instance, &resolved, action)
            .map_err(|error| projection_failure(&error))?;
        match (action, instance.observed_state) {
            (ReconcileAction::Validate, ObservedEnvironmentState::Requested) => {
                Ok(no_endpoints(ObservedEnvironmentState::Validating, false))
            }
            (ReconcileAction::Validate, ObservedEnvironmentState::Validating) => {
                Ok(no_endpoints(ObservedEnvironmentState::Building, false))
            }
            (ReconcileAction::Build, ObservedEnvironmentState::Building) => {
                Ok(no_endpoints(ObservedEnvironmentState::Provisioning, false))
            }
            (
                ReconcileAction::Provision | ReconcileAction::Reset,
                ObservedEnvironmentState::Provisioning,
            ) => {
                let ProviderOutcome::Completed(observed) =
                    self.backend.apply(&fence, &plan).await?
                else {
                    return Ok(ProviderOutcome::Pending);
                };
                self.accept_running_observation(&fence, &plan, instance, observed)
                    .await
                    .map(ProviderOutcome::Completed)
            }
            (ReconcileAction::Observe, _) => {
                let ProviderOutcome::Completed(observed) =
                    self.backend.observe(&fence, &plan).await?
                else {
                    return Ok(ProviderOutcome::Pending);
                };
                self.accept_running_observation(&fence, &plan, instance, observed)
                    .await
                    .map(ProviderOutcome::Completed)
            }
            (ReconcileAction::Start, ObservedEnvironmentState::Stopped) => {
                let ProviderOutcome::Completed(observed) =
                    self.backend.start(&fence, &plan).await?
                else {
                    return Ok(ProviderOutcome::Pending);
                };
                self.accept_running_observation(&fence, &plan, instance, observed)
                    .await
                    .map(ProviderOutcome::Completed)
            }
            (ReconcileAction::Restart, ObservedEnvironmentState::Provisioning) => {
                let ProviderOutcome::Completed(observed) =
                    self.backend.restart(&fence, &plan).await?
                else {
                    return Ok(ProviderOutcome::Pending);
                };
                self.accept_running_observation(&fence, &plan, instance, observed)
                    .await
                    .map(ProviderOutcome::Completed)
            }
            _ => Err(ProviderFailure {
                code: ProviderFailureCode::Rejected,
                retryable: false,
            }),
        }
    }
}

impl<B, R, S> KubeVirtProvider<B, R, S>
where
    B: KubeVirtProviderBackend,
    R: ContainerReleaseResolver,
    S: KubeVirtObservationStore,
{
    async fn stop_runtime(
        &self,
        fence: &KubeVirtBackendFence,
        instance: &EnvironmentInstance,
    ) -> Result<ProviderOutcome<()>, ProviderFailure> {
        let plan = self
            .cleanup_plan(instance)
            .map_err(|error| projection_failure(&error))?;
        let ProviderOutcome::Completed(observed) = self.backend.stop(fence, &plan).await? else {
            return Ok(ProviderOutcome::Pending);
        };
        validate_stopped_observation(instance, observed)?;
        self.observations
            .record_stopped(fence, &plan, &observed)
            .await
            .map_err(|error| observation_store_failure(&error))?;
        Ok(ProviderOutcome::Completed(()))
    }

    async fn accept_running_observation(
        &self,
        fence: &KubeVirtBackendFence,
        plan: &KubeVirtResourcePlan,
        instance: &EnvironmentInstance,
        observed: KubeVirtRunningObservation,
    ) -> Result<ProviderObservation, ProviderFailure> {
        if running_observation_ready(instance, &observed) {
            self.observations
                .record_running(fence, plan, &observed)
                .await
                .map_err(|error| observation_store_failure(&error))?;
        }
        ready_observation(instance, observed)
    }
}

fn ready_observation(
    instance: &EnvironmentInstance,
    observed: KubeVirtRunningObservation,
) -> Result<ProviderObservation, ProviderFailure> {
    if !running_observation_ready(instance, &observed) {
        return Ok(ProviderObservation {
            next_state: ObservedEnvironmentState::Provisioning,
            endpoints: Vec::new(),
            cleanup_evidence: None,
            operation_complete: false,
        });
    }
    let revision = next_revision(instance.revision)?;
    Ok(ProviderObservation {
        next_state: ObservedEnvironmentState::Ready,
        endpoints: vec![EnvironmentEndpoint {
            id: deterministic_endpoint_id(instance.id)?,
            protocol: contracts::environment::EndpointProtocol::Ssh,
            revision,
            health: EndpointHealth::Healthy,

            observed_at: observed.observed_at,
        }],
        cleanup_evidence: None,
        operation_complete: true,
    })
}

fn running_observation_ready(
    instance: &EnvironmentInstance,
    observed: &KubeVirtRunningObservation,
) -> bool {
    valid_running_observation(instance.generation, observed)
}

fn valid_running_observation(
    environment_generation: u64,
    observed: &KubeVirtRunningObservation,
) -> bool {
    observed.observed_environment_generation == environment_generation
        && observed.vm_resource_generation > 0
        && observed.observed_vm_resource_generation >= observed.vm_resource_generation
        && !observed.vm_uid.is_nil()
        && !observed.vmi_uid.is_nil()
        && !observed.root_disk_uid.is_nil()
        && private_route_ip(observed.guest_ip)
        && private_route_ip(observed.service_cluster_ip)
        && observed.ssh_host_key_sha256 != Sha256Digest::of_bytes(&[])
        && observed.ssh_ready
}

fn observation_store_failure(error: &KubeVirtObservationStoreError) -> ProviderFailure {
    match error {
        KubeVirtObservationStoreError::Database(_) => unavailable(),
        KubeVirtObservationStoreError::InvalidObservation
        | KubeVirtObservationStoreError::IdentityMismatch
        | KubeVirtObservationStoreError::StaleFence
        | KubeVirtObservationStoreError::Tombstoned => invalid_observation(),
    }
}

fn validate_stopped_observation(
    instance: &EnvironmentInstance,
    observed: KubeVirtStoppedObservation,
) -> Result<(), ProviderFailure> {
    if !valid_stopped_observation(instance.generation, &observed) {
        return Err(invalid_observation());
    }
    Ok(())
}

fn valid_stopped_observation(
    environment_generation: u64,
    observed: &KubeVirtStoppedObservation,
) -> bool {
    observed.observed_environment_generation == environment_generation
        && !observed.vm_uid.is_nil()
        && !observed.root_disk_uid.is_nil()
        && observed.vmi_absent
}

fn next_revision(revision: Revision) -> Result<Revision, ProviderFailure> {
    revision
        .get()
        .checked_add(1)
        .and_then(|value| Revision::new(value).ok())
        .ok_or_else(invalid_observation)
}

fn deterministic_endpoint_id(environment_id: EnvironmentId) -> Result<EndpointId, ProviderFailure> {
    let mut bytes = *environment_id.as_uuid().as_bytes();
    bytes[15] ^= 2;
    EndpointId::from_str(&Uuid::from_bytes(bytes).to_string()).map_err(|_| invalid_observation())
}

fn private_route_ip(value: IpAddr) -> bool {
    match value {
        IpAddr::V4(value) => value.is_private(),
        IpAddr::V6(value) => value.is_unique_local(),
    }
}

fn resource(kind: &str, namespace: Option<&str>, name: &str, document: Value) -> KubeVirtResource {
    KubeVirtResource {
        kind: kind.to_owned(),
        namespace: namespace.map(str::to_owned),
        name: name.to_owned(),
        document,
    }
}

fn canonical_hash<T: Serialize>(value: &T) -> Result<Sha256Digest, ReleaseProjectionError> {
    Sha256Digest::of_canonical(value).map_err(|_| ReleaseProjectionError::ContractInvalid)
}

fn projection_failure(error: &ReleaseProjectionError) -> ProviderFailure {
    match error {
        ReleaseProjectionError::Database(_)
        | ReleaseProjectionError::PersistenceFailed
        | ReleaseProjectionError::NotFound => unavailable(),
        ReleaseProjectionError::ConfigurationInvalid
        | ReleaseProjectionError::ContractInvalid
        | ReleaseProjectionError::IdentityMismatch
        | ReleaseProjectionError::SecurityPostureInvalid
        | ReleaseProjectionError::Withdrawn
        | ReleaseProjectionError::EvidenceExpired
        | ReleaseProjectionError::TrustRevisionMismatch
        | ReleaseProjectionError::VmBaseImportFailed
        | ReleaseProjectionError::VmBaseCapacityExceeded => ProviderFailure {
            code: ProviderFailureCode::Rejected,
            retryable: false,
        },
    }
}

fn valid_binding(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.:".contains(&byte))
}

fn approved_resources(
    instance: &EnvironmentInstance,
    projection: &ReleasePublished,
) -> Result<(u32, u64, u64, Option<GpuAllocation>), ReleaseProjectionError> {
    match instance.class {
        contracts::authoring::EnvironmentClass::Experiment => {
            let resources = &projection.environment_spec.resources;
            Ok((
                resources.cpu_millicores,
                resources.memory_bytes,
                resources.storage_bytes,
                experiment_gpu_allocation(instance, projection)?,
            ))
        }
        contracts::authoring::EnvironmentClass::Work => {
            let authorization = instance
                .operation
                .lease_authorization
                .as_ref()
                .ok_or(ReleaseProjectionError::IdentityMismatch)?;
            if authorization.project_id != instance.project_id
                || authorization.course_id != instance.course_id
                || authorization.owner_actor_id != instance.owner_id
                || Some(authorization.lease_id) != instance.lease_id
                || Some(authorization.capacity_binding.as_str())
                    != instance.capacity_binding.as_deref()
            {
                return Err(ReleaseProjectionError::IdentityMismatch);
            }
            authorization
                .validate()
                .map_err(|_| ReleaseProjectionError::SecurityPostureInvalid)?;
            Ok((
                authorization.approved_resources.cpu_millicores,
                authorization.approved_resources.memory_bytes,
                authorization.approved_resources.storage_bytes,
                authorization.gpu_allocation.clone(),
            ))
        }
    }
}

/// Returns the Resource-resolved Experiment GPU allocation, failing closed on any mismatch.
///
/// A release that declares a GPU but has no durable allocation, or whose allocation does not
/// match the declared class and count, is rejected before any VM object is rendered.
fn experiment_gpu_allocation(
    instance: &EnvironmentInstance,
    projection: &ReleasePublished,
) -> Result<Option<GpuAllocation>, ReleaseProjectionError> {
    match (
        &instance.gpu_allocation,
        projection.environment_spec.resources.gpu.as_ref(),
    ) {
        (None, None) => Ok(None),
        (Some(allocation), Some(request))
            if allocation.class == request.class && allocation.count == request.count =>
        {
            allocation
                .validate()
                .map_err(|_| ReleaseProjectionError::SecurityPostureInvalid)?;
            Ok(Some(allocation.clone()))
        }
        _ => Err(ReleaseProjectionError::SecurityPostureInvalid),
    }
}

fn vm_vgpu_license_egress(
    licensing: &KubeVirtVmVgpuLicensingConfiguration,
) -> Result<Vec<Value>, ReleaseProjectionError> {
    licensing.validate()?;
    let mut egress = vec![json!({
        "toEndpoints":[{"matchLabels":{
            "k8s:io.kubernetes.pod.namespace":"kube-system",
            "k8s:k8s-app":"kube-dns"
        }}],
        "toPorts":[{"ports":[{"protocol":"UDP","port":"53"},{"protocol":"TCP","port":"53"}],"rules":{"dns":[{"matchPattern":"*"}]}}]
    })];
    match licensing.mode {
        KubeVirtVmVgpuLicenseMode::NvidiaDls => {
            let host = licensing
                .license_url
                .host_str()
                .ok_or(ReleaseProjectionError::ConfigurationInvalid)?;
            let destination = if let Ok(address) = IpAddr::from_str(host) {
                let prefix = if address.is_ipv4() { 32 } else { 128 };
                json!({"toCIDR":[format!("{address}/{prefix}")]})
            } else {
                json!({"toFQDNs":[{"matchName":host}]})
            };
            let mut destination = destination;
            destination["toPorts"] = json!([{"ports":[{"protocol":"TCP","port":licensing.license_url.port_or_known_default().unwrap_or(443).to_string()}]}]);
            egress.push(destination);
        }
        KubeVirtVmVgpuLicenseMode::FastapiDls => {
            let host = licensing
                .license_url
                .host_str()
                .ok_or(ReleaseProjectionError::ConfigurationInvalid)?;
            let parts = host.split('.').collect::<Vec<_>>();
            if parts.len() != 5
                || !valid_dns_label(parts[0])
                || parts[2..] != ["svc", "cluster", "local"]
                || !valid_dns_label(parts[1])
            {
                return Err(ReleaseProjectionError::ConfigurationInvalid);
            }
            egress.push(json!({
                "toEndpoints":[{"matchLabels":{
                    "k8s:io.kubernetes.pod.namespace":parts[1],
                    "app.kubernetes.io/name":"fastapi-dls"
                }}],
                "toPorts":[{"ports":[{"protocol":"TCP","port":"8443"}]}]
            }));
        }
    }
    Ok(egress)
}

fn valid_subject(value: &str) -> bool {
    valid_binding(value) && !value.contains('*') && !value.contains('>')
}

pub(crate) fn valid_dns_label(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 63
        && !value.starts_with('-')
        && !value.ends_with('-')
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn valid_guest_user(value: &str) -> bool {
    valid_dns_label(value)
        && value
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase())
}

fn valid_artifact_ref(artifact: &ArtifactRef) -> bool {
    artifact.size_bytes > 0
        && !artifact.store_binding.trim().is_empty()
        && !artifact.object_version.trim().is_empty()
        && !artifact.media_type.trim().is_empty()
        && !artifact
            .store_binding
            .bytes()
            .any(|byte| byte.is_ascii_whitespace())
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
        retryable: false,
    }
}

const fn configuration_invalid() -> ProviderFailure {
    ProviderFailure {
        code: ProviderFailureCode::Rejected,
        retryable: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct UnstartedBackend(std::sync::atomic::AtomicUsize);

    #[async_trait]
    impl KubeVirtExecutorBackend for UnstartedBackend {
        async fn execute(
            &self,
            _fence: &KubeVirtBackendFence,
            _request: &KubeVirtExecutorRequest,
            _permit: &KubeVirtExecutionPermit,
        ) -> KubeVirtExecutorResponse {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            KubeVirtExecutorResponse::Failed {
                failure: configuration_invalid(),
            }
        }
    }

    #[tokio::test]
    #[allow(
        clippy::too_many_lines,
        reason = "one real database race controls the admission commit barrier and subsequent absent-row completion"
    )]
    async fn unstarted_completion_waits_for_admission_commit_before_deciding_absence()
    -> Result<(), Box<dyn std::error::Error>> {
        let authority = crate::test_support::requested_instance();
        let mut fence = KubeVirtBackendFence::for_action(&authority, ReconcileAction::Provision)
            .map_err(|error| format!("{error:?}"))?;
        let (_container, permit) = crate::kubevirt_execution::test_permit(&mut fence).await?;
        let pool = permit.pool.clone();
        sqlx::query("DELETE FROM environment.kubevirt_executor_fences WHERE environment_id=$1")
            .bind(fence.environment_id.as_uuid())
            .execute(&pool)
            .await?;
        let executor = Arc::new(FencedKubeVirtExecutor::new(
            PgKubeVirtExecutorFenceStore::new(pool.clone()),
            UnstartedBackend(std::sync::atomic::AtomicUsize::new(0)),
            permit.instance.clone(),
        ));
        executor
            .active
            .lock()
            .map_err(|_| "active fixture lock")?
            .insert(
                fence.environment_id.as_uuid(),
                ActiveKubeVirtExecution {
                    fence: fence.clone(),
                    terminal: None,
                },
            );
        let mut admission = pool.begin().await?;
        lock_execution_environment(&mut admission, fence.environment_id).await?;
        sqlx::query("INSERT INTO environment.kubevirt_executor_fences (environment_id,highest_generation,operation_id,provider_step,attempt,tombstoned,last_action,last_request_id,deadline_at,execution_owner) VALUES ($1,$2,$3,$4,$5,FALSE,'provision',$6,$7,$8)")
            .bind(fence.environment_id.as_uuid()).bind(i64::try_from(fence.environment_generation)?)
            .bind(fence.operation_id.as_uuid()).bind(i32::try_from(fence.provider_step)?).bind(i32::try_from(fence.attempt)?)
            .bind(fence.request_id.to_string()).bind(fence.deadline_at.get()).bind(serde_json::to_value(&permit.instance)?)
            .execute(&mut *admission).await?;
        let (started, receiver) = tokio::sync::oneshot::channel();
        let executing = Arc::clone(&executor);
        let task_fence = fence.clone();
        let mut completion = tokio::spawn(async move {
            let _ = started.send(());
            executing
                .finish_unstarted_admission(
                    &task_fence,
                    KubeVirtExecutorFenceError::Database(sqlx::Error::Protocol(
                        "admission acknowledgement lost".to_owned(),
                    )),
                )
                .await
        });
        receiver.await?;
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut completion)
                .await
                .is_err()
        );
        admission.commit().await?;
        assert!(matches!(
            completion.await??,
            KubeVirtExecutorResponse::Failed {
                failure: ProviderFailure {
                    code: ProviderFailureCode::Unavailable,
                    ..
                }
            }
        ));
        let terminal: Value = sqlx::query_scalar("SELECT last_response FROM environment.kubevirt_executor_fences WHERE environment_id=$1")
            .bind(fence.environment_id.as_uuid()).fetch_one(&pool).await?;
        assert_eq!(terminal["status"], "failed");
        assert_eq!(
            executor.backend.0.load(std::sync::atomic::Ordering::SeqCst),
            0
        );
        assert!(
            executor
                .active
                .lock()
                .map_err(|_| "active fixture lock")?
                .is_empty()
        );
        sqlx::query("DELETE FROM environment.kubevirt_executor_fences WHERE environment_id=$1")
            .bind(fence.environment_id.as_uuid())
            .execute(&pool)
            .await?;
        executor
            .active
            .lock()
            .map_err(|_| "active fixture lock")?
            .insert(
                fence.environment_id.as_uuid(),
                ActiveKubeVirtExecution {
                    fence: fence.clone(),
                    terminal: None,
                },
            );
        assert!(matches!(
            executor
                .finish_unstarted_admission(
                    &fence,
                    KubeVirtExecutorFenceError::Database(sqlx::Error::Protocol(
                        "admission rolled back".to_owned()
                    ))
                )
                .await,
            Err(KubeVirtExecutorFenceError::Database(_))
        ));
        assert!(
            executor
                .active
                .lock()
                .map_err(|_| "active fixture lock")?
                .is_empty()
        );
        Ok(())
    }

    #[tokio::test]
    async fn nats_timeout_is_pending_but_no_responders_is_real_failure()
    -> Result<(), Box<dyn std::error::Error>> {
        use testcontainers::{
            GenericImage,
            core::{IntoContainerPort, WaitFor},
            runners::AsyncRunner,
        };
        let nats = GenericImage::new("nats", "2.11.8-alpine")
            .with_exposed_port(4222.tcp())
            .with_wait_for(WaitFor::message_on_stderr("Server is ready"))
            .start()
            .await?;
        let client = async_nats::connect(format!(
            "nats://127.0.0.1:{}",
            nats.get_host_port_ipv4(4222).await?
        ))
        .await?;
        let mut subscriber = client.subscribe("fixture.kubevirt.accepted").await?;
        client.flush().await?;
        let instance = crate::test_support::requested_instance();
        let fence = KubeVirtBackendFence {
            protocol_version: KUBEVIRT_BACKEND_PROTOCOL_VERSION,
            environment_id: instance.id,
            operation_id: instance.operation.id,
            provider_step: 1,
            environment_generation: 1,
            attempt: 1,
            action: ReconcileAction::Cleanup,
            request_id: Sha256Digest::of_bytes(b"bound-in-request"),
            trace_id: "transport-fixture".to_owned(),
            deadline_at: instance.operation.deadline_at,
        };
        let request = KubeVirtExecutorRequest::DeleteNamespace {
            plan: KubeVirtCleanupPlan {
                environment_id: instance.id,
                project_id: instance.project_id,
                namespace: format!("lw-env-{}", instance.id),
                virtual_machine_name: "runtime".to_owned(),
                plan_sha256: Sha256Digest::of_bytes(b"cleanup"),
            },
        };
        let backend = NatsKubeVirtProviderBackend::new(
            client.clone(),
            "fixture.kubevirt.accepted".to_owned(),
            Duration::from_millis(30),
        )
        .map_err(|error| format!("{error:?}"))?;
        assert!(matches!(
            backend
                .request(&fence, request.clone())
                .await
                .map_err(|error| format!("{error:?}"))?,
            KubeVirtExecutorResponse::Pending
        ));
        let accepted = tokio::time::timeout(Duration::from_secs(1), subscriber.next())
            .await?
            .ok_or("request not accepted")?;
        let accepted: KubeVirtExecutorRequestEnvelope = serde_json::from_slice(&accepted.payload)?;
        assert_eq!(accepted.fence.operation_id, fence.operation_id);
        assert_eq!(accepted.fence.attempt, 1);
        assert_eq!(accepted.fence.deadline_at, fence.deadline_at);
        let no_responder = NatsKubeVirtProviderBackend::new(
            client,
            "fixture.kubevirt.no_responder".to_owned(),
            Duration::from_millis(30),
        )
        .map_err(|error| format!("{error:?}"))?;
        assert!(matches!(
            no_responder.request(&fence, request).await,
            Err(ProviderFailure {
                code: ProviderFailureCode::Unavailable,
                retryable: true
            })
        ));
        Ok(())
    }

    #[test]
    fn external_nvidia_dls_egress_uses_exact_fqdn_and_port() -> Result<(), ReleaseProjectionError> {
        let license_url = "https://licenses.example.test:8443/"
            .parse()
            .map_err(|_| ReleaseProjectionError::ConfigurationInvalid)?;
        let licensing = KubeVirtVmVgpuLicensingConfiguration {
            mode: KubeVirtVmVgpuLicenseMode::NvidiaDls,
            license_url,
            token_secret_ref: KubeVirtSecretRef {
                namespace: "license-system".to_owned(),
                name: "client-token".to_owned(),
                key: "token".to_owned(),
            },
            tls_ca_secret_ref: KubeVirtSecretRef {
                namespace: "license-system".to_owned(),
                name: "license-tls".to_owned(),
                key: "ca.crt".to_owned(),
            },
            fastapi_dls_signing_root_ca_secret_ref: None,
        };
        let egress = vm_vgpu_license_egress(&licensing)?;
        assert_eq!(
            egress[1]["toFQDNs"][0]["matchName"],
            json!("licenses.example.test")
        );
        assert_eq!(egress[1]["toPorts"][0]["ports"][0]["port"], json!("8443"));
        Ok(())
    }
}
