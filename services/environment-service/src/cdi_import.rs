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
/// Reviewed PVC capacity recorded so an over-capacity reuse is rejected.
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
    /// The recorded capacity exceeds the reviewed capacity.
    #[error("LW_ENVIRONMENT_VM_BASE_CAPACITY_EXCEEDED")]
    CapacityExceeded,
    /// The import could not be created, observed or completed in time.
    #[error("LW_ENVIRONMENT_VM_BASE_IMPORT_FAILED")]
    ImportFailed,
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
        let deadline = tokio::time::Instant::now() + self.import_timeout;
        loop {
            if data_volume_succeeded(&observed) {
                return data_volume_uid(&observed);
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
