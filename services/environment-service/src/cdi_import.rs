//! On-demand CDI import of the VM base disk a `KubeVirt` resource plan references.
//!
//! The reviewed deployment seeds its `baseDisks[]` as CDI `DataVolume`/`DataSource` pairs ahead of
//! time. A runtime-registered base has no seeded objects, so the deployment-owned executor imports
//! it here before the per-environment clone `DataVolume` (whose `sourceRef` targets the base
//! `DataSource`) is applied. The import is identity-checked on every use so a drifted or
//! over-capacity object can never be silently reused.

use std::collections::BTreeMap;
use std::time::Duration;

use async_trait::async_trait;
use reqwest::{Client, Method, StatusCode, Url};
use serde_json::{Value, json};

use crate::KubeVirtBaseDiskIdentity;

const FIELD_MANAGER: &str = "labweaver-runtime-executor";
const CDI_PREFIX: &str = "/apis/cdi.kubevirt.io/v1beta1";
/// Reviewed registry digest recorded on the imported objects.
const SOURCE_REGISTRY_ANNOTATION: &str = "labweaver.io/source-registry";
/// Reviewed raw-disk SHA-256, written only for a deployment-seeded base disk.
const DISK_SHA256_ANNOTATION: &str = "labweaver.io/disk-sha256";
/// Declared identity rule (`disk-sha256` or `registry-digest`).
const IDENTITY_ANNOTATION: &str = "labweaver.io/base-disk-identity";
/// Declared logical disk capacity recorded on the base DV and `DataSource`.
const CAPACITY_ANNOTATION: &str = "labweaver.io/base-disk-capacity-bytes";
/// CDI annotation that binds the importer claim immediately.
const IMMEDIATE_BINDING_ANNOTATION: &str = "cdi.kubevirt.io/storage.bind.immediate.requested";

/// Hard cap on one base-disk import wait. A timeout leaves the objects in place for diagnosis and
/// fails closed instead of retrying without bound.
pub const BASE_DISK_IMPORT_TIMEOUT: Duration = Duration::from_mins(30);

/// Stable on-demand CDI import diagnostics. Raw Kubernetes messages are never surfaced.
#[derive(Debug, thiserror::Error)]
pub enum CdiImportError {
    /// The recorded identity disagrees with the resolved binding.
    #[error("LW_ENVIRONMENT_VM_BASE_IDENTITY_MISMATCH")]
    IdentityMismatch,
    /// Logical capacity or overhead-derived physical capacity disagrees with the admitted disk.
    #[error("LW_ENVIRONMENT_VM_BASE_CAPACITY_EXCEEDED")]
    CapacityExceeded,
    /// The import could not be created, observed or completed in time.
    #[error("LW_ENVIRONMENT_VM_BASE_IMPORT_FAILED")]
    ImportFailed,
}

/// Filesystem sizing from CDI's effective configuration, shared by imports and tenant quotas.
pub(crate) struct CdiStorageSizing {
    root_overhead: f64,
    scratch_overhead: f64,
}

const CDI_ALIGNMENT: u64 = 1 << 20;
const MAX_EXACT_FLOAT_BYTES: u64 = 1 << 53;

impl CdiStorageSizing {
    pub(crate) fn from_config(config: &Value, storage_class: &str) -> Result<Self, CdiImportError> {
        if storage_class.is_empty() {
            return Err(CdiImportError::IdentityMismatch);
        }
        let scratch_class = match config.pointer("/status/scratchSpaceStorageClass") {
            None | Some(Value::Null) => storage_class,
            Some(Value::String(value)) if value.is_empty() => storage_class,
            Some(Value::String(value)) => value,
            Some(_) => return Err(CdiImportError::IdentityMismatch),
        };
        Ok(Self {
            root_overhead: effective_overhead(config, storage_class)?,
            scratch_overhead: effective_overhead(config, scratch_class)?,
        })
    }

    pub(crate) fn root_bytes(&self, logical: u64) -> Result<u64, CdiImportError> {
        required_space(self.root_overhead, logical)
    }

    pub(crate) fn scratch_bytes(&self, logical_budget: u64) -> Result<u64, CdiImportError> {
        let physical = self.root_bytes(logical_budget)?;
        let usable = rounded_bytes(bytes_as_float(physical)? / (1.0 + self.root_overhead))?;
        let usable = usable / CDI_ALIGNMENT * CDI_ALIGNMENT;
        align_cdi(required_space(self.scratch_overhead, usable)?)
    }
}

fn effective_overhead(config: &Value, storage_class: &str) -> Result<f64, CdiImportError> {
    let overhead = config
        .pointer("/status/filesystemOverhead")
        .ok_or(CdiImportError::ImportFailed)?;
    let by_class = match overhead.get("storageClass") {
        None | Some(Value::Null) => None,
        Some(Value::Object(values)) => values.get(storage_class),
        Some(_) => return Err(CdiImportError::ImportFailed),
    };
    let configured = by_class
        .or_else(|| overhead.get("global"))
        .and_then(Value::as_str)
        .ok_or(CdiImportError::ImportFailed)?;
    let parsed = configured
        .parse::<f64>()
        .map_err(|_| CdiImportError::CapacityExceeded)?;
    if !parsed.is_finite() || !(0.0..1.0).contains(&parsed) {
        return Err(CdiImportError::CapacityExceeded);
    }
    Ok(parsed)
}

fn align_cdi(bytes: u64) -> Result<u64, CdiImportError> {
    if bytes == 0 || bytes > MAX_EXACT_FLOAT_BYTES {
        return Err(CdiImportError::CapacityExceeded);
    }
    let aligned = bytes
        .checked_add(CDI_ALIGNMENT - 1)
        .map(|value| value / CDI_ALIGNMENT * CDI_ALIGNMENT)
        .ok_or(CdiImportError::CapacityExceeded)?;
    if aligned > MAX_EXACT_FLOAT_BYTES {
        return Err(CdiImportError::CapacityExceeded);
    }
    Ok(aligned)
}

#[expect(
    clippy::cast_precision_loss,
    reason = "integers are bounded to f64's exact 53-bit range"
)]
fn bytes_as_float(bytes: u64) -> Result<f64, CdiImportError> {
    if bytes == 0 || bytes > MAX_EXACT_FLOAT_BYTES {
        return Err(CdiImportError::CapacityExceeded);
    }
    Ok(bytes as f64)
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "ceil is finite, positive and bounded to the exact integer range before conversion"
)]
fn rounded_bytes(value: f64) -> Result<u64, CdiImportError> {
    let rounded = value.ceil();
    if !rounded.is_finite() || !(1.0..=9_007_199_254_740_992.0).contains(&rounded) {
        return Err(CdiImportError::CapacityExceeded);
    }
    Ok(rounded as u64)
}

fn required_space(overhead: f64, logical: u64) -> Result<u64, CdiImportError> {
    rounded_bytes(bytes_as_float(align_cdi(logical)?)? * (1.0 + overhead))
}

/// Nonzero profile overrides require sizing beyond the managed filesystem calculation.
pub(crate) fn validate_cdi_storage_profile(
    profile: Option<&Value>,
    storage_class: &str,
) -> Result<(), CdiImportError> {
    let Some(profile) = profile else {
        return Ok(());
    };
    if required_meta(profile, "name")? != storage_class {
        return Err(CdiImportError::IdentityMismatch);
    }
    if let Some(minimum) =
        profile.pointer("/metadata/annotations/cdi.kubevirt.io~1minimumSupportedPvcSize")
        && !minimum
            .as_str()
            .is_some_and(|value| storage_matches(value, 0))
    {
        return Err(CdiImportError::CapacityExceeded);
    }
    Ok(())
}

/// One fully resolved base-disk import request handed to the CDI importer.
#[derive(Clone, Debug)]
pub struct KubeVirtBaseDiskImport {
    pub data_source_namespace: String,
    pub data_source_name: String,
    pub source_registry_digest: String,
    /// Reviewed raw-disk SHA-256 for a seeded base; the manifest digest hex for a runtime base.
    pub disk_sha256: String,
    pub identity: KubeVirtBaseDiskIdentity,
    pub storage_class_name: String,
    /// Declared logical disk bytes; CDI adds filesystem overhead to the physical PVC request.
    pub capacity_bytes: u64,
}

impl KubeVirtBaseDiskImport {
    /// Deterministic importer `DataVolume` name for this base disk.
    #[must_use]
    pub fn data_volume_name(&self) -> String {
        format!("{}-seed", self.data_source_name)
    }
}

/// Confirmed CDI `DataSource` the per-environment clone resolves.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BaseDiskRef {
    pub data_source_namespace: String,
    pub data_source_name: String,
    pub identity: KubeVirtBaseDiskIdentity,
}

impl BaseDiskRef {
    fn from_import(import: &KubeVirtBaseDiskImport) -> Self {
        Self {
            data_source_namespace: import.data_source_namespace.clone(),
            data_source_name: import.data_source_name.clone(),
            identity: import.identity,
        }
    }
}

/// Minimal CDI surface required to import and publish one base disk.
#[async_trait]
pub trait CdiImportClient: Send + Sync {
    /// Reads the recorded identity annotations of the published base `DataSource`, if it exists.
    async fn read_base_data_source(
        &self,
        namespace: &str,
        name: &str,
    ) -> Result<Option<BTreeMap<String, String>>, CdiImportError>;

    /// Creates the importer `DataVolume` when absent, waits for `Succeeded`, and returns its UID.
    async fn import_base_data_volume(
        &self,
        import: &KubeVirtBaseDiskImport,
    ) -> Result<String, CdiImportError>;

    /// Publishes the base `DataSource` owned by the imported `DataVolume`.
    async fn publish_base_data_source(
        &self,
        import: &KubeVirtBaseDiskImport,
        data_volume_uid: &str,
    ) -> Result<(), CdiImportError>;
}

/// Ensures the CDI base disk exists and matches its recorded identity before the per-environment
/// clone `DataVolume` references it.
pub async fn ensure_base_disk(
    client: &dyn CdiImportClient,
    import: &KubeVirtBaseDiskImport,
) -> Result<BaseDiskRef, CdiImportError> {
    if let Some(annotations) = client
        .read_base_data_source(&import.data_source_namespace, &import.data_source_name)
        .await?
    {
        verify_recorded_identity(import, &annotations)?;
        return Ok(BaseDiskRef::from_import(import));
    }
    let data_volume_uid = client.import_base_data_volume(import).await?;
    client
        .publish_base_data_source(import, &data_volume_uid)
        .await?;
    Ok(BaseDiskRef::from_import(import))
}

/// Rejects a published base disk whose recorded identity or capacity drifted from the binding.
fn verify_recorded_identity(
    import: &KubeVirtBaseDiskImport,
    annotations: &BTreeMap<String, String>,
) -> Result<(), CdiImportError> {
    if annotations.get(SOURCE_REGISTRY_ANNOTATION) != Some(&import.source_registry_digest) {
        return Err(CdiImportError::IdentityMismatch);
    }
    // The reviewed raw-disk SHA-256 is only meaningful for a deployment-seeded base. A
    // runtime-registered base is identified solely by its declared registry digest.
    if import.identity == KubeVirtBaseDiskIdentity::ReviewedDiskSha256
        && annotations.get(DISK_SHA256_ANNOTATION) != Some(&import.disk_sha256)
    {
        return Err(CdiImportError::IdentityMismatch);
    }
    if let Some(capacity) = annotations.get(CAPACITY_ANNOTATION) {
        let capacity = capacity
            .parse::<u64>()
            .map_err(|_| CdiImportError::IdentityMismatch)?;
        if capacity > import.capacity_bytes {
            return Err(CdiImportError::CapacityExceeded);
        }
    }
    Ok(())
}

/// Identity annotations written onto the importer `DataVolume` and the published `DataSource`.
fn identity_annotations(import: &KubeVirtBaseDiskImport) -> serde_json::Map<String, Value> {
    let mut annotations = serde_json::Map::from_iter([
        (
            SOURCE_REGISTRY_ANNOTATION.to_owned(),
            json!(import.source_registry_digest),
        ),
        (
            IDENTITY_ANNOTATION.to_owned(),
            json!(import.identity.as_str()),
        ),
        (
            CAPACITY_ANNOTATION.to_owned(),
            json!(import.capacity_bytes.to_string()),
        ),
    ]);
    if import.identity == KubeVirtBaseDiskIdentity::ReviewedDiskSha256 {
        annotations.insert(DISK_SHA256_ANNOTATION.to_owned(), json!(import.disk_sha256));
    }
    annotations
}

fn base_metadata(
    name: &str,
    namespace: &str,
    annotations: &serde_json::Map<String, Value>,
) -> Value {
    json!({
        "name": name,
        "namespace": namespace,
        "labels": {
            "app.kubernetes.io/part-of": "labweaver",
            "labweaver.io/managed": "true",
        },
        "annotations": annotations,
    })
}

fn data_volume_document(import: &KubeVirtBaseDiskImport) -> Value {
    let name = import.data_volume_name();
    let mut annotations = identity_annotations(import);
    annotations.insert(IMMEDIATE_BINDING_ANNOTATION.to_owned(), json!("true"));
    json!({
        "apiVersion": "cdi.kubevirt.io/v1beta1",
        "kind": "DataVolume",
        "metadata": base_metadata(&name, &import.data_source_namespace, &annotations),
        "spec": {
            "source": {"registry": {"url": import.source_registry_digest, "pullMethod": "node"}},
            "storage": {
                "storageClassName": import.storage_class_name,
                "volumeMode": "Filesystem",
                "accessModes": ["ReadWriteOnce"],
                "resources": {"requests": {"storage": import.capacity_bytes.to_string()}},
            }
        }
    })
}

fn data_source_document(import: &KubeVirtBaseDiskImport, data_volume_uid: &str) -> Value {
    let data_volume_name = import.data_volume_name();
    json!({
        "apiVersion": "cdi.kubevirt.io/v1beta1",
        "kind": "DataSource",
        "metadata": {
            "name": import.data_source_name,
            "namespace": import.data_source_namespace,
            "labels": {
                "app.kubernetes.io/part-of": "labweaver",
                "labweaver.io/managed": "true",
            },
            "annotations": identity_annotations(import),
            "ownerReferences": [{
                "apiVersion": "cdi.kubevirt.io/v1beta1",
                "kind": "DataVolume",
                "name": data_volume_name,
                "uid": data_volume_uid,
                "controller": true,
                "blockOwnerDeletion": true,
            }],
        },
        "spec": {
            "source": {"pvc": {"name": data_volume_name, "namespace": import.data_source_namespace}}
        }
    })
}

/// Real CDI importer over the deployment-owned executor's reviewed Kubernetes transport.
#[derive(Clone)]
pub struct KubernetesCdiImportClient {
    client: Client,
    api_server: Url,
    token: String,
    poll_interval: Duration,
    import_timeout: Duration,
}

impl KubernetesCdiImportClient {
    /// Binds the importer to the executor's authenticated Kubernetes client.
    #[must_use]
    pub fn new(
        client: Client,
        api_server: Url,
        token: String,
        poll_interval: Duration,
        import_timeout: Duration,
    ) -> Self {
        Self {
            client,
            api_server,
            token,
            poll_interval,
            import_timeout,
        }
    }

    fn url(&self, path: &str) -> Result<Url, CdiImportError> {
        self.api_server
            .join(path.trim_start_matches('/'))
            .map_err(|_| CdiImportError::ImportFailed)
    }

    fn authorized(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        request.bearer_auth(&self.token)
    }

    async fn get_json(&self, path: &str) -> Result<Option<Value>, CdiImportError> {
        let response = self
            .authorized(self.client.get(self.url(path)?))
            .send()
            .await
            .map_err(|_| CdiImportError::ImportFailed)?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !response.status().is_success() {
            return Err(CdiImportError::ImportFailed);
        }
        response
            .json()
            .await
            .map(Some)
            .map_err(|_| CdiImportError::ImportFailed)
    }

    async fn list_json(&self, path: &str) -> Result<Vec<Value>, CdiImportError> {
        self.get_json(path)
            .await?
            .and_then(|value| value.get("items").and_then(Value::as_array).cloned())
            .ok_or(CdiImportError::ImportFailed)
    }

    /// The existing base importer owns retry; CDI remains the only Pod/spec creator.
    async fn reset_stale_importer(
        &self,
        import: &KubeVirtBaseDiskImport,
        dv: &Value,
    ) -> Result<(), CdiImportError> {
        verify_import_data_volume(import, dv)?;
        if data_volume_succeeded(dv) {
            return Ok(());
        }
        if dv.pointer("/status/phase").and_then(Value::as_str) == Some("Failed") {
            return Err(CdiImportError::ImportFailed);
        }
        if dv.pointer("/status/phase").and_then(Value::as_str) != Some("ImportScheduled") {
            return Ok(());
        }
        let namespace = &import.data_source_namespace;
        let name = import.data_volume_name();
        if !crate::kubevirt_provider::valid_dns_label(namespace)
            || !crate::kubevirt_provider::valid_dns_label(&name)
        {
            return Err(CdiImportError::IdentityMismatch);
        }
        let Some(pvc) = self
            .get_json(&format!(
                "/api/v1/namespaces/{namespace}/persistentvolumeclaims/{name}"
            ))
            .await?
        else {
            return Ok(());
        };
        if required_meta(&pvc, "namespace")? != namespace || required_meta(&pvc, "name")? != name {
            return Err(CdiImportError::IdentityMismatch);
        }
        let dv_uid = required_meta(dv, "uid")?;
        verify_controller(&pvc, "DataVolume", &name, dv_uid)?;
        if pvc.pointer("/status/phase").and_then(Value::as_str) != Some("Bound") {
            return Ok(());
        }
        let config = self.verify_bound_import_storage(import, &pvc).await?;
        let pods = self
            .list_json(&format!("/api/v1/namespaces/{namespace}/pods"))
            .await?;
        let consumers = pods
            .iter()
            .filter(|pod| pod_uses_pvc(pod, &name))
            .collect::<Vec<_>>();
        if consumers.len() != 1 {
            return Ok(());
        }
        let pod = consumers[0];
        if required_meta(pod, "namespace")? != namespace {
            return Err(CdiImportError::IdentityMismatch);
        }
        verify_controller(
            pod,
            "PersistentVolumeClaim",
            &name,
            required_meta(&pvc, "uid")?,
        )?;
        if pod
            .pointer("/metadata/deletionTimestamp")
            .is_some_and(|value| !value.is_null())
        {
            return Ok(());
        }
        if !noauth_image_pull_failure(pod) {
            return Ok(());
        }
        let refs = pull_secret_names(config.pointer("/status/imagePullSecrets"))?;
        if refs.is_empty() || refs == pull_secret_names(pod.pointer("/spec/imagePullSecrets"))? {
            return Ok(());
        }
        self.verify_import_pull_secrets(namespace, &refs).await?;
        self.verify_unconsumed_base(import, &name).await?;
        let pod_name = required_meta(pod, "name")?;
        if !crate::kubevirt_provider::valid_dns_label(pod_name) {
            return Err(CdiImportError::IdentityMismatch);
        }
        let response = self.authorized(self.client.delete(self.url(&format!("/api/v1/namespaces/{namespace}/pods/{pod_name}"))?))
            .json(&json!({"apiVersion":"v1","kind":"DeleteOptions","preconditions":{
                "uid":required_meta(pod,"uid")?,"resourceVersion":required_meta(pod,"resourceVersion")?
            }})).send().await.map_err(|_| CdiImportError::ImportFailed)?;
        if response.status().is_success() || response.status() == StatusCode::NOT_FOUND {
            Ok(())
        } else {
            Err(CdiImportError::ImportFailed)
        }
    }

    async fn verify_bound_import_storage(
        &self,
        import: &KubeVirtBaseDiskImport,
        pvc: &Value,
    ) -> Result<Value, CdiImportError> {
        if pvc
            .pointer("/spec/storageClassName")
            .and_then(Value::as_str)
            != Some(&import.storage_class_name)
            || !matches!(pvc.pointer("/spec/volumeMode"), None | Some(Value::Null))
                && pvc.pointer("/spec/volumeMode").and_then(Value::as_str) != Some("Filesystem")
        {
            return Err(CdiImportError::IdentityMismatch);
        }
        let config = self
            .get_json(&format!("{CDI_PREFIX}/cdiconfigs/config"))
            .await?
            .ok_or(CdiImportError::ImportFailed)?;
        let profile = self
            .get_json(&format!(
                "{CDI_PREFIX}/storageprofiles/{}",
                import.storage_class_name
            ))
            .await?;
        validate_cdi_storage_profile(profile.as_ref(), &import.storage_class_name)?;
        let expected_capacity = CdiStorageSizing::from_config(&config, &import.storage_class_name)?
            .root_bytes(import.capacity_bytes)?;
        let capacity = pvc
            .pointer("/spec/resources/requests/storage")
            .and_then(Value::as_str)
            .ok_or(CdiImportError::IdentityMismatch)?;
        if !storage_matches(capacity, expected_capacity)
            || !pvc
                .pointer("/status/capacity/storage")
                .and_then(Value::as_str)
                .is_some_and(|capacity| storage_matches(capacity, expected_capacity))
        {
            return Err(CdiImportError::CapacityExceeded);
        }
        Ok(config)
    }

    async fn verify_import_pull_secrets(
        &self,
        namespace: &str,
        refs: &[String],
    ) -> Result<(), CdiImportError> {
        for secret_name in refs {
            if !crate::kubevirt_provider::valid_dns_label(secret_name) {
                return Err(CdiImportError::IdentityMismatch);
            }
            let secret = self
                .get_json(&format!(
                    "/api/v1/namespaces/{namespace}/secrets/{secret_name}"
                ))
                .await?
                .ok_or(CdiImportError::ImportFailed)?;
            if required_meta(&secret, "namespace")? != namespace
                || required_meta(&secret, "name")? != secret_name
            {
                return Err(CdiImportError::IdentityMismatch);
            }
            if secret.get("type").and_then(Value::as_str) != Some("kubernetes.io/dockerconfigjson")
                || secret
                    .pointer("/metadata/labels/app.kubernetes.io~1part-of")
                    .and_then(Value::as_str)
                    != Some("labweaver")
            {
                return Err(CdiImportError::IdentityMismatch);
            }
        }
        Ok(())
    }

    async fn verify_unconsumed_base(
        &self,
        import: &KubeVirtBaseDiskImport,
        name: &str,
    ) -> Result<(), CdiImportError> {
        let namespace = &import.data_source_namespace;
        // Any published base or clone/VM consumer makes a shared importer reset unsafe.
        let sources = self.list_json(&format!("{CDI_PREFIX}/datasources")).await?;
        if sources.iter().any(|source| {
            references_base_pvc(source, "/spec/source/pvc", namespace, name)
                || references_base_pvc(source, "/status/source/pvc", namespace, name)
        }) {
            return Err(CdiImportError::ImportFailed);
        }
        let volumes = self.list_json(&format!("{CDI_PREFIX}/datavolumes")).await?;
        if volumes.iter().any(|volume| {
            references_base_pvc(volume, "/spec/source/pvc", namespace, name)
                || references_base_source(volume, namespace, &import.data_source_name)
        }) {
            return Err(CdiImportError::ImportFailed);
        }
        for kind in ["virtualmachines", "virtualmachineinstances"] {
            let machines = self
                .list_json(&format!(
                    "/apis/kubevirt.io/v1/namespaces/{namespace}/{kind}"
                ))
                .await?;
            if machines.iter().any(|vm| {
                pod_uses_pvc(vm, name)
                    || vm
                        .pointer("/spec/template")
                        .is_some_and(|template| pod_uses_pvc(template, name))
            }) {
                return Err(CdiImportError::ImportFailed);
            }
        }
        Ok(())
    }

    async fn apply_json(&self, path: &str, document: &Value) -> Result<Value, CdiImportError> {
        let response = self
            .authorized(
                self.client
                    .request(Method::PATCH, self.url(path)?)
                    .query(&[("fieldManager", FIELD_MANAGER), ("force", "false")])
                    .header("content-type", "application/apply-patch+yaml")
                    .body(serde_json::to_vec(document).map_err(|_| CdiImportError::ImportFailed)?),
            )
            .send()
            .await
            .map_err(|_| CdiImportError::ImportFailed)?;
        if !response.status().is_success() {
            return Err(CdiImportError::ImportFailed);
        }
        response
            .json()
            .await
            .map_err(|_| CdiImportError::ImportFailed)
    }
}

#[async_trait]
impl CdiImportClient for KubernetesCdiImportClient {
    async fn read_base_data_source(
        &self,
        namespace: &str,
        name: &str,
    ) -> Result<Option<BTreeMap<String, String>>, CdiImportError> {
        let path = format!("{CDI_PREFIX}/namespaces/{namespace}/datasources/{name}");
        let Some(document) = self.get_json(&path).await? else {
            return Ok(None);
        };
        let annotations = document
            .pointer("/metadata/annotations")
            .and_then(Value::as_object)
            .map(|annotations| {
                annotations
                    .iter()
                    .filter_map(|(key, value)| {
                        value.as_str().map(|value| (key.clone(), value.to_owned()))
                    })
                    .collect::<BTreeMap<_, _>>()
            })
            .unwrap_or_default();
        Ok(Some(annotations))
    }

    async fn import_base_data_volume(
        &self,
        import: &KubeVirtBaseDiskImport,
    ) -> Result<String, CdiImportError> {
        let name = import.data_volume_name();
        let path = format!(
            "{CDI_PREFIX}/namespaces/{}/datavolumes/{name}",
            import.data_source_namespace
        );
        let mut observed = match self.get_json(&path).await? {
            Some(document) => document,
            None => {
                self.apply_json(&path, &data_volume_document(import))
                    .await?
            }
        };
        verify_import_data_volume(import, &observed)?;
        let admitted_uid = data_volume_uid(&observed)?;
        self.reset_stale_importer(import, &observed).await?;
        let deadline = tokio::time::Instant::now() + self.import_timeout;
        loop {
            verify_import_data_volume(import, &observed)?;
            if data_volume_uid(&observed)? != admitted_uid {
                return Err(CdiImportError::IdentityMismatch);
            }
            if data_volume_succeeded(&observed) {
                return Ok(admitted_uid);
            }
            if tokio::time::Instant::now() >= deadline {
                // Leave the DataVolume in place: the retry reuses it instead of importing twice.
                return Err(CdiImportError::ImportFailed);
            }
            tokio::time::sleep(self.poll_interval).await;
            observed = self
                .get_json(&path)
                .await?
                .ok_or(CdiImportError::ImportFailed)?;
        }
    }

    async fn publish_base_data_source(
        &self,
        import: &KubeVirtBaseDiskImport,
        data_volume_uid: &str,
    ) -> Result<(), CdiImportError> {
        let path = format!(
            "{CDI_PREFIX}/namespaces/{}/datasources/{}",
            import.data_source_namespace, import.data_source_name
        );
        self.apply_json(&path, &data_source_document(import, data_volume_uid))
            .await
            .map(|_document| ())
    }
}

fn data_volume_succeeded(document: &Value) -> bool {
    document.pointer("/status/phase").and_then(Value::as_str) == Some("Succeeded")
}

fn data_volume_uid(document: &Value) -> Result<String, CdiImportError> {
    document
        .pointer("/metadata/uid")
        .and_then(Value::as_str)
        .filter(|uid| !uid.is_empty())
        .map(str::to_owned)
        .ok_or(CdiImportError::ImportFailed)
}

fn required_meta<'a>(value: &'a Value, key: &str) -> Result<&'a str, CdiImportError> {
    value
        .get("metadata")
        .and_then(|metadata| metadata.get(key))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or(CdiImportError::IdentityMismatch)
}
fn verify_controller(
    value: &Value,
    kind: &str,
    name: &str,
    uid: &str,
) -> Result<(), CdiImportError> {
    let owners = value
        .pointer("/metadata/ownerReferences")
        .and_then(Value::as_array)
        .ok_or(CdiImportError::IdentityMismatch)?;
    if owners
        .iter()
        .filter(|owner| owner.get("controller").and_then(Value::as_bool) == Some(true))
        .count()
        != 1
        || !owners.iter().any(|owner| {
            owner.get("controller").and_then(Value::as_bool) == Some(true)
                && owner.get("kind").and_then(Value::as_str) == Some(kind)
                && owner.get("name").and_then(Value::as_str) == Some(name)
                && owner.get("uid").and_then(Value::as_str) == Some(uid)
        })
    {
        return Err(CdiImportError::IdentityMismatch);
    }
    Ok(())
}
pub(crate) fn storage_matches(value: &str, bytes: u64) -> bool {
    value == bytes.to_string()
        || [
            ("Ki", 1_u64 << 10),
            ("Mi", 1_u64 << 20),
            ("Gi", 1_u64 << 30),
            ("Ti", 1_u64 << 40),
        ]
        .iter()
        .any(|(suffix, unit)| {
            bytes.is_multiple_of(*unit) && value == format!("{}{suffix}", bytes / unit)
        })
}
fn verify_import_data_volume(
    import: &KubeVirtBaseDiskImport,
    dv: &Value,
) -> Result<(), CdiImportError> {
    if required_meta(dv, "name")? != import.data_volume_name()
        || required_meta(dv, "namespace")? != import.data_source_namespace
        || dv
            .pointer("/metadata/labels/labweaver.io~1managed")
            .and_then(Value::as_str)
            != Some("true")
        || dv
            .pointer("/spec/source/registry/url")
            .and_then(Value::as_str)
            != Some(import.source_registry_digest.as_str())
        || dv
            .pointer("/spec/storage/storageClassName")
            .and_then(Value::as_str)
            != Some(import.storage_class_name.as_str())
        || !matches!(
            dv.pointer("/spec/storage/volumeMode"),
            None | Some(Value::Null)
        ) && dv
            .pointer("/spec/storage/volumeMode")
            .and_then(Value::as_str)
            != Some("Filesystem")
        || dv
            .pointer("/status/claimName")
            .and_then(Value::as_str)
            .is_some_and(|name| name != import.data_volume_name())
        || dv
            .pointer("/spec/source/registry/pullMethod")
            .and_then(Value::as_str)
            != Some("node")
    {
        return Err(CdiImportError::IdentityMismatch);
    }
    let annotations = dv
        .pointer("/metadata/annotations")
        .and_then(Value::as_object)
        .ok_or(CdiImportError::IdentityMismatch)?;
    for (key, value) in identity_annotations(import) {
        if annotations.get(&key) != Some(&value) {
            return Err(CdiImportError::IdentityMismatch);
        }
    }
    let capacity = dv
        .pointer("/spec/storage/resources/requests/storage")
        .and_then(Value::as_str)
        .ok_or(CdiImportError::IdentityMismatch)?;
    if !storage_matches(capacity, import.capacity_bytes) {
        return Err(CdiImportError::CapacityExceeded);
    }
    required_meta(dv, "uid")?;
    Ok(())
}
fn pod_uses_pvc(pod: &Value, name: &str) -> bool {
    pod.pointer("/spec/volumes")
        .and_then(Value::as_array)
        .is_some_and(|volumes| {
            volumes.iter().any(|volume| {
                volume
                    .pointer("/persistentVolumeClaim/claimName")
                    .and_then(Value::as_str)
                    == Some(name)
                    || volume.pointer("/dataVolume/name").and_then(Value::as_str) == Some(name)
            })
        })
}
fn pull_secret_names(value: Option<&Value>) -> Result<Vec<String>, CdiImportError> {
    let Some(value) = value.filter(|value| !value.is_null()) else {
        return Ok(Vec::new());
    };
    let mut names = value
        .as_array()
        .ok_or(CdiImportError::IdentityMismatch)?
        .iter()
        .map(|reference| {
            reference
                .get("name")
                .and_then(Value::as_str)
                .filter(|name| !name.is_empty())
                .map(str::to_owned)
                .ok_or(CdiImportError::IdentityMismatch)
        })
        .collect::<Result<Vec<_>, _>>()?;
    names.sort();
    names.dedup();
    Ok(names)
}
fn noauth_image_pull_failure(pod: &Value) -> bool {
    pod.pointer("/status/containerStatuses")
        .and_then(Value::as_array)
        .is_some_and(|statuses| {
            statuses.iter().any(|status| {
                status.get("name").and_then(Value::as_str) == Some("server")
                    && status
                        .pointer("/state/waiting/reason")
                        .and_then(Value::as_str)
                        == Some("ImagePullBackOff")
                    && status
                        .pointer("/state/waiting/message")
                        .and_then(Value::as_str)
                        .is_some_and(|message| message.contains("no basic auth credentials"))
            })
        })
}

fn references_base_pvc(value: &Value, path: &str, namespace: &str, name: &str) -> bool {
    let Some(source) = value.pointer(path) else {
        return false;
    };
    source.get("name").and_then(Value::as_str) == Some(name)
        && source
            .get("namespace")
            .and_then(Value::as_str)
            .is_none_or(|source_namespace| {
                source_namespace.is_empty() || source_namespace == namespace
            })
}
fn references_base_source(value: &Value, namespace: &str, name: &str) -> bool {
    let Some(source) = value.pointer("/spec/sourceRef") else {
        return false;
    };
    let source_namespace = source
        .get("namespace")
        .and_then(Value::as_str)
        .or_else(|| value.pointer("/metadata/namespace").and_then(Value::as_str));
    source.get("kind").and_then(Value::as_str) == Some("DataSource")
        && source.get("name").and_then(Value::as_str) == Some(name)
        && source_namespace.is_none_or(|source_namespace| source_namespace == namespace)
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "external API fixtures require their seeded documents"
)]
mod tests {
    use super::*;
    use axum::{
        Router,
        body::{Body, to_bytes},
        extract::State,
        http::Request,
        response::{IntoResponse, Response},
    };
    use std::sync::Arc;
    use tokio::sync::Mutex;

    #[test]
    fn cdi_sizing_preserves_logical_disk_and_scratch_alignment()
    -> Result<(), Box<dyn std::error::Error>> {
        let config = json!({"status":{"scratchSpaceStorageClass":"local-path",
            "filesystemOverhead":{"global":"0.25","storageClass":{"local-path":"0.06"}}}});
        let sizing = CdiStorageSizing::from_config(&config, "local-path")?;
        assert_eq!(sizing.root_bytes(16 << 30)?, 18_210_661_336);
        assert_eq!(sizing.scratch_bytes(16 << 30)?, 18_211_667_968);
        assert_eq!(sizing.root_bytes((16 << 30) - 1)?, 18_210_661_336);
        assert!(sizing.root_bytes(0).is_err());
        assert!(sizing.root_bytes(MAX_EXACT_FLOAT_BYTES + 1).is_err());
        assert!(sizing.root_bytes(MAX_EXACT_FLOAT_BYTES).is_err());
        let different_scratch = json!({"status":{"scratchSpaceStorageClass":"scratch",
            "filesystemOverhead":{"global":"0.0","storageClass":{"local-path":"0.06","scratch":"0.0"}}}});
        assert_eq!(
            CdiStorageSizing::from_config(&different_scratch, "local-path")?
                .scratch_bytes(16 << 30)?,
            16 << 30
        );
        for invalid in ["NaN", "inf", "-0.1", "1", "invalid"] {
            let config = json!({"status":{"filesystemOverhead":{"global":invalid}}});
            assert!(CdiStorageSizing::from_config(&config, "local-path").is_err());
        }
        assert!(CdiStorageSizing::from_config(&json!({"status":{}}), "local-path").is_err());
        let global = CdiStorageSizing::from_config(
            &json!({"status":{"filesystemOverhead":{"global":"0.0"}}}),
            "local-path",
        )?;
        assert_eq!(global.root_bytes(1)?, CDI_ALIGNMENT);
        Ok(())
    }

    struct MockCdi {
        documents: Arc<Mutex<BTreeMap<String, Value>>>,
        deletes: Arc<Mutex<Vec<Value>>>,
        task: tokio::task::JoinHandle<()>,
        client: KubernetesCdiImportClient,
    }
    impl Drop for MockCdi {
        fn drop(&mut self) {
            self.task.abort();
        }
    }
    type MockState = (Arc<Mutex<BTreeMap<String, Value>>>, Arc<Mutex<Vec<Value>>>);
    async fn handle(
        State((documents, deletes)): State<MockState>,
        request: Request<Body>,
    ) -> Response {
        let path = request.uri().path().to_owned();
        let method = request.method().clone();
        if method == Method::GET {
            let mut documents = documents.lock().await;
            let value = documents.get(&path).cloned();
            if let Some(current) = documents.get_mut(&path)
                && current.get("replaceAfterRead").and_then(Value::as_bool) == Some(true)
            {
                current["metadata"]["uid"] = json!("replacement-dv");
                current["status"]["phase"] = json!("Succeeded");
                current["replaceAfterRead"] = json!(false);
            }
            return match value {
                Some(value) => axum::Json(value).into_response(),
                None => StatusCode::NOT_FOUND.into_response(),
            };
        }
        if method == Method::DELETE && path == "/api/v1/namespaces/labweaver-system/pods/importer" {
            let Ok(bytes) = to_bytes(request.into_body(), 65536).await else {
                return StatusCode::BAD_REQUEST.into_response();
            };
            let Ok(value) = serde_json::from_slice::<Value>(&bytes) else {
                return StatusCode::BAD_REQUEST.into_response();
            };
            if value["preconditions"]["uid"] != "pod-uid"
                || value["preconditions"]["resourceVersion"] != "10"
            {
                return StatusCode::CONFLICT.into_response();
            }
            deletes.lock().await.push(value);
            let mut documents = documents.lock().await;
            let pod = &mut documents
                .get_mut("/api/v1/namespaces/labweaver-system/pods")
                .expect("pod list")["items"][0];
            pod["metadata"]["uid"] = json!("new-pod-uid");
            pod["metadata"]["resourceVersion"] = json!("11");
            pod["spec"]["imagePullSecrets"] = json!([{"name":"harbor-pull"}]);
            return StatusCode::ACCEPTED.into_response();
        }
        StatusCode::FORBIDDEN.into_response()
    }
    fn import() -> KubeVirtBaseDiskImport {
        KubeVirtBaseDiskImport {
        data_source_namespace:"labweaver-system".to_owned(),data_source_name:"base".to_owned(),
        source_registry_digest:"docker://registry.invalid/base@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".to_owned(),
        disk_sha256:String::new(),identity:KubeVirtBaseDiskIdentity::RuntimeRegistryDigest,storage_class_name:"local-path".to_owned(),capacity_bytes:16_u64<<30,
    }
    }
    async fn fixture() -> Result<(MockCdi, Value), Box<dyn std::error::Error>> {
        let mut dv = data_volume_document(&import());
        dv["metadata"]["uid"] = json!("dv-uid");
        dv["status"]["phase"] = json!("ImportScheduled");
        let pvc = json!({"metadata":{"name":"base-seed","namespace":"labweaver-system","uid":"pvc-uid","ownerReferences":[{"kind":"DataVolume","name":"base-seed","uid":"dv-uid","controller":true}]},"status":{"phase":"Bound","capacity":{"storage":"18210661336"}},"spec":{"storageClassName":"local-path","volumeMode":"Filesystem","resources":{"requests":{"storage":"18210661336"}}}});
        let pod = json!({"metadata":{"name":"importer","namespace":"labweaver-system","uid":"pod-uid","resourceVersion":"10","ownerReferences":[{"kind":"PersistentVolumeClaim","name":"base-seed","uid":"pvc-uid","controller":true}]},"spec":{"volumes":[{"persistentVolumeClaim":{"claimName":"base-seed"}}]},"status":{"containerStatuses":[{"name":"server","state":{"waiting":{"reason":"ImagePullBackOff","message":"no basic auth credentials"}}}]}});
        let documents = Arc::new(Mutex::new(BTreeMap::from([
            (
                "/api/v1/namespaces/labweaver-system/persistentvolumeclaims/base-seed".to_owned(),
                pvc,
            ),
            (
                "/api/v1/namespaces/labweaver-system/pods".to_owned(),
                json!({"items":[pod]}),
            ),
            (
                format!("{CDI_PREFIX}/cdiconfigs/config"),
                json!({"status":{"imagePullSecrets":[{"name":"harbor-pull"}],"scratchSpaceStorageClass":"local-path","filesystemOverhead":{"global":"0.06","storageClass":{"local-path":"0.06"}}}}),
            ),
            (
                format!("{CDI_PREFIX}/storageprofiles/local-path"),
                json!({"metadata":{"name":"local-path"}}),
            ),
            (
                "/api/v1/namespaces/labweaver-system/secrets/harbor-pull".to_owned(),
                json!({"type":"kubernetes.io/dockerconfigjson","metadata":{"name":"harbor-pull","namespace":"labweaver-system","labels":{"app.kubernetes.io/part-of":"labweaver"}}}),
            ),
            (format!("{CDI_PREFIX}/datasources"), json!({"items":[]})),
            (format!("{CDI_PREFIX}/datavolumes"), json!({"items":[]})),
            (
                "/apis/kubevirt.io/v1/namespaces/labweaver-system/virtualmachines".to_owned(),
                json!({"items":[]}),
            ),
            (
                "/apis/kubevirt.io/v1/namespaces/labweaver-system/virtualmachineinstances"
                    .to_owned(),
                json!({"items":[]}),
            ),
        ])));
        let deletes = Arc::new(Mutex::new(Vec::new()));
        let router = Router::new()
            .fallback(handle)
            .with_state((Arc::clone(&documents), Arc::clone(&deletes)));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let url = Url::parse(&format!("http://{}/", listener.local_addr()?))?;
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        let client = KubernetesCdiImportClient::new(
            Client::new(),
            url,
            "fixture-token".to_owned(),
            Duration::from_millis(1),
            Duration::from_secs(1),
        );
        Ok((
            MockCdi {
                documents,
                deletes,
                task,
                client,
            },
            dv,
        ))
    }
    #[tokio::test]
    async fn stale_noauth_importer_is_reset_once_with_uid_and_revision_preserving_pvc()
    -> Result<(), Box<dyn std::error::Error>> {
        let (fixture, mut dv) = fixture().await?;
        assert_eq!(dv["spec"]["storage"]["volumeMode"], "Filesystem");
        dv["spec"]["storage"]
            .as_object_mut()
            .expect("DV storage")
            .remove("volumeMode");
        fixture
            .documents
            .lock()
            .await
            .get_mut(&format!("{CDI_PREFIX}/storageprofiles/local-path"))
            .expect("profile")["metadata"]["annotations"] =
            json!({"cdi.kubevirt.io/minimumSupportedPvcSize":"0Gi"});
        let pvc=fixture.documents.lock().await["/api/v1/namespaces/labweaver-system/persistentvolumeclaims/base-seed"].clone();
        fixture.client.reset_stale_importer(&import(), &dv).await?;
        fixture.client.reset_stale_importer(&import(), &dv).await?;
        let deletes = fixture.deletes.lock().await;
        assert_eq!(deletes.len(), 1);
        assert_eq!(
            deletes[0]["preconditions"],
            json!({"uid":"pod-uid","resourceVersion":"10"})
        );
        assert_eq!(
            fixture.documents.lock().await["/api/v1/namespaces/labweaver-system/persistentvolumeclaims/base-seed"],
            pvc
        );
        Ok(())
    }
    #[tokio::test]
    async fn import_poll_rejects_same_name_replacement_even_when_new_dv_is_ready()
    -> Result<(), Box<dyn std::error::Error>> {
        let (fixture, mut dv) = fixture().await?;
        dv["replaceAfterRead"] = json!(true);
        {
            let mut documents = fixture.documents.lock().await;
            documents.insert(
                format!("{CDI_PREFIX}/namespaces/labweaver-system/datavolumes/base-seed"),
                dv,
            );
            documents
                .get_mut("/api/v1/namespaces/labweaver-system/pods")
                .expect("pods")["items"][0]["status"]["containerStatuses"] = json!([]);
        }
        assert!(matches!(
            fixture.client.import_base_data_volume(&import()).await,
            Err(CdiImportError::IdentityMismatch)
        ));
        assert!(fixture.deletes.lock().await.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn importer_retry_rejects_drift_consumers_and_does_not_reset_ready_or_current_refs()
    -> Result<(), Box<dyn std::error::Error>> {
        for scenario in [
            "ready",
            "fatal",
            "unknown-phase",
            "other-container",
            "wrong-uid",
            "wrong-scope",
            "consumer",
            "clone-default-namespace",
            "published",
            "config-empty",
            "current-refs",
            "healthy",
            "wrong-secret",
            "wrong-digest",
            "wrong-capacity",
        ] {
            let (fixture, mut dv) = fixture().await?;
            {
                let mut docs = fixture.documents.lock().await;
                match scenario {
                    "ready" => dv["status"]["phase"] = json!("Succeeded"),
                    "fatal" => dv["status"]["phase"] = json!("Failed"),
                    "unknown-phase" => dv["status"]["phase"] = json!("UnknownNewPhase"),
                    "other-container" => {
                        docs.get_mut("/api/v1/namespaces/labweaver-system/pods")
                            .expect("pods")["items"][0]["status"]["containerStatuses"][0]["name"] =
                            json!("other");
                    }
                    "wrong-uid" => {
                        docs.get_mut("/api/v1/namespaces/labweaver-system/pods")
                            .expect("pods")["items"][0]["metadata"]["ownerReferences"][0]["uid"] =
                            json!("other-pvc");
                    }
                    "wrong-scope" => {
                        docs.get_mut("/api/v1/namespaces/labweaver-system/pods")
                            .expect("pods")["items"][0]["metadata"]["namespace"] = json!("other");
                    }
                    "consumer" => {
                        docs.get_mut(
                            "/apis/kubevirt.io/v1/namespaces/labweaver-system/virtualmachines",
                        )
                        .expect("VMs")["items"] = json!([{"spec":{"template":{"spec":{"volumes":[{"dataVolume":{"name":"base-seed"}}]}}}}]);
                    }
                    "clone-default-namespace" => {
                        docs.get_mut(&format!("{CDI_PREFIX}/datavolumes"))
                            .expect("DVs")["items"] = json!([{"metadata":{"namespace":"labweaver-system"},"spec":{"sourceRef":{"kind":"DataSource","name":"base"}}}]);
                    }
                    "published" => {
                        docs.get_mut(&format!("{CDI_PREFIX}/datasources"))
                            .expect("DSs")["items"] = json!([{"spec":{"source":{"pvc":{"name":"base-seed","namespace":"labweaver-system"}}}}]);
                    }
                    "config-empty" => {
                        docs.get_mut(&format!("{CDI_PREFIX}/cdiconfigs/config"))
                            .expect("config")["status"]["imagePullSecrets"] = json!([]);
                    }
                    "current-refs" => {
                        docs.get_mut("/api/v1/namespaces/labweaver-system/pods")
                            .expect("pods")["items"][0]["spec"]["imagePullSecrets"] =
                            json!([{"name":"harbor-pull"}]);
                    }
                    "healthy" => {
                        docs.get_mut("/api/v1/namespaces/labweaver-system/pods")
                            .expect("pods")["items"][0]["status"]["containerStatuses"] = json!([]);
                    }
                    "wrong-secret" => {
                        docs.get_mut("/api/v1/namespaces/labweaver-system/secrets/harbor-pull")
                            .expect("secret")["type"] = json!("Opaque");
                    }
                    "wrong-digest" => {
                        dv["spec"]["source"]["registry"]["url"] = json!("docker://other");
                    }
                    "wrong-capacity" => {
                        dv["spec"]["storage"]["resources"]["requests"]["storage"] = json!("32Gi");
                    }
                    _ => return Err("unknown retry scenario".into()),
                }
            }
            let result = fixture.client.reset_stale_importer(&import(), &dv).await;
            if [
                "ready",
                "config-empty",
                "current-refs",
                "healthy",
                "unknown-phase",
                "other-container",
            ]
            .contains(&scenario)
            {
                assert!(result.is_ok(), "{scenario}: {result:?}");
            } else {
                assert!(result.is_err(), "{scenario} unexpectedly accepted");
            }
            assert!(
                fixture.deletes.lock().await.is_empty(),
                "{scenario} deleted shared importer"
            );
        }
        Ok(())
    }
    #[tokio::test]
    async fn importer_retry_rejects_physical_storage_drift()
    -> Result<(), Box<dyn std::error::Error>> {
        for scenario in [
            "wrong-mode",
            "wrong-overhead",
            "unknown-overhead",
            "invalid-overhead",
            "physical-request-drift",
            "physical-status-drift",
            "oversized-pvc",
            "wrong-storage-class",
            "block-volume",
            "unbound",
            "profile-minimum",
            "profile-invalid-minimum",
            "profile-scope-drift",
        ] {
            let (fixture, mut dv) = fixture().await?;
            {
                let mut docs = fixture.documents.lock().await;
                match scenario {
                    "wrong-mode" => {
                        dv["spec"]["storage"]["volumeMode"] = json!("Block");
                    }
                    "wrong-overhead" => {
                        docs.get_mut(&format!("{CDI_PREFIX}/cdiconfigs/config"))
                            .expect("config")["status"]["filesystemOverhead"]["storageClass"]["local-path"] =
                            json!("0.07");
                    }
                    "unknown-overhead" => {
                        docs.get_mut(&format!("{CDI_PREFIX}/cdiconfigs/config"))
                            .expect("config")["status"]["filesystemOverhead"] = Value::Null;
                    }
                    "invalid-overhead" => {
                        docs.get_mut(&format!("{CDI_PREFIX}/cdiconfigs/config"))
                            .expect("config")["status"]["filesystemOverhead"]["storageClass"]["local-path"] =
                            json!("NaN");
                    }
                    "profile-minimum" | "profile-invalid-minimum" => {
                        docs.get_mut(&format!("{CDI_PREFIX}/storageprofiles/local-path"))
                            .expect("profile")["metadata"]["annotations"] = json!({"cdi.kubevirt.io/minimumSupportedPvcSize":if scenario=="profile-minimum" {"32Gi"} else {"invalid"}});
                    }
                    "profile-scope-drift" => {
                        docs.get_mut(&format!("{CDI_PREFIX}/storageprofiles/local-path"))
                            .expect("profile")["metadata"]["name"] = json!("other");
                    }
                    "physical-request-drift"
                    | "physical-status-drift"
                    | "oversized-pvc"
                    | "wrong-storage-class"
                    | "block-volume"
                    | "unbound" => {
                        let pvc = docs.get_mut("/api/v1/namespaces/labweaver-system/persistentvolumeclaims/base-seed").expect("PVC");
                        match scenario {
                            "physical-request-drift" => {
                                pvc["spec"]["resources"]["requests"]["storage"] = json!("16Gi");
                            }
                            "physical-status-drift" => {
                                pvc["status"]["capacity"]["storage"] = json!("16Gi");
                            }
                            "oversized-pvc" => {
                                pvc["spec"]["resources"]["requests"]["storage"] = json!("32Gi");
                                pvc["status"]["capacity"]["storage"] = json!("32Gi");
                            }
                            "wrong-storage-class" => {
                                pvc["spec"]["storageClassName"] = json!("other");
                            }
                            "block-volume" => pvc["spec"]["volumeMode"] = json!("Block"),
                            "unbound" => pvc["status"]["phase"] = json!("Pending"),
                            _ => return Err("unknown PVC scenario".into()),
                        }
                    }
                    _ => return Err("unknown storage scenario".into()),
                }
            }
            let result = fixture.client.reset_stale_importer(&import(), &dv).await;
            if scenario == "unbound" {
                assert!(result.is_ok());
            } else {
                assert!(result.is_err(), "{scenario} unexpectedly accepted");
            }
            assert!(
                fixture.deletes.lock().await.is_empty(),
                "{scenario} deleted importer"
            );
        }
        Ok(())
    }
}
