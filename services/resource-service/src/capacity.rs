//! Explicit Kubernetes capacity provider for Resource-owned quota shells.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use contracts::resource::{CapacityClaim, ResourceRequest};
use reqwest::{Certificate, Client, StatusCode, Url};
use serde::Deserialize;
use serde_json::{Value, json};

const FIELD_MANAGER: &str = "labweaver-resource-service";
const QUOTA_NAME: &str = "resource-quota";

/// Reviewed non-secret binding for one capacity Provider. Selection is exact by `binding`.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KubernetesCapacityProviderConfiguration {
    pub binding: String,
    pub api_server: String,
    pub bearer_token_file: PathBuf,
    pub cluster_ca_file: PathBuf,
    pub request_timeout_milliseconds: u64,
    #[serde(default)]
    pub namespace_labels: BTreeMap<String, String>,
}

impl KubernetesCapacityProviderConfiguration {
    fn validate(&self) -> Result<(), CapacityProviderError> {
        if !valid_dns_label(&self.binding)
            || Url::parse(&self.api_server)
                .ok()
                .is_none_or(|url| url.scheme() != "https" || url.host_str().is_none())
            || !self.bearer_token_file.is_absolute()
            || !self.cluster_ca_file.is_absolute()
            || !(1..=30_000).contains(&self.request_timeout_milliseconds)
            || self.namespace_labels.iter().any(|(key, value)| {
                key.is_empty() || key.len() > 253 || value.len() > 63 || value.is_empty()
            })
        {
            return Err(CapacityProviderError::Configuration);
        }
        Ok(())
    }
}

/// Exact Resource-owned Kubernetes plan, safe to persist as a capacity claim snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KubernetesQuotaShellPlan {
    pub binding: String,
    pub namespace: String,
    pub claim_id: contracts::CapacityClaimId,
    pub claim_revision: contracts::Revision,
    pub quota_plan_sha256: contracts::Sha256Digest,
    pub namespace_document: Value,
    pub quota_document: Value,
}

impl KubernetesQuotaShellPlan {
    pub fn from_claim(
        configuration: &KubernetesCapacityProviderConfiguration,
        request: &ResourceRequest,
        claim: &CapacityClaim,
    ) -> Result<Self, CapacityProviderError> {
        configuration.validate()?;
        request
            .validate()
            .map_err(|_| CapacityProviderError::Plan)?;
        claim.validate().map_err(|_| CapacityProviderError::Plan)?;
        if claim.provider_binding != configuration.binding || claim.request_id != request.id {
            return Err(CapacityProviderError::BindingMismatch);
        }
        // GPU classes are catalog values, not Kubernetes extended-resource names. Sprint 3
        // admits zero GPU capacity, so any such request must have been rejected before here.
        if claim.quota_resources.gpu.is_some() || claim.workload_resources.gpu.is_some() {
            return Err(CapacityProviderError::GpuUnsupported);
        }
        let namespace = format!("lw-work-{}", request.target.environment_id);
        if !valid_dns_label(&namespace) {
            return Err(CapacityProviderError::Plan);
        }
        let mut labels = configuration.namespace_labels.clone();
        labels.insert("labweaver.io/managed-by".into(), "resource-service".into());
        labels.insert(
            "labweaver.io/environment-id".into(),
            request.target.environment_id.to_string(),
        );
        labels.insert(
            "labweaver.io/capacity-claim-id".into(),
            claim.id.to_string(),
        );
        let annotations = json!({
            "labweaver.io/capacity-claim-revision": claim.revision.get().to_string(),
            "labweaver.io/quota-plan-sha256": claim.quota_plan_sha256.to_string(),
            "labweaver.io/provider-binding": claim.provider_binding,
        });
        let namespace_document = json!({
            "apiVersion": "v1", "kind": "Namespace",
            "metadata": {"name": namespace, "labels": labels, "annotations": annotations}
        });
        let hard = json!({
            "requests.cpu": format!("{}m", claim.quota_resources.cpu_millicores),
            "limits.cpu": format!("{}m", claim.quota_resources.cpu_millicores),
            "requests.memory": claim.quota_resources.memory_bytes.to_string(),
            "limits.memory": claim.quota_resources.memory_bytes.to_string(),
            "requests.storage": claim.quota_resources.storage_bytes.to_string(),
            "persistentvolumeclaims": "1",
            "pods": "2"
        });
        let quota_document = json!({
            "apiVersion": "v1", "kind": "ResourceQuota",
            "metadata": {"name": QUOTA_NAME, "namespace": namespace, "labels": labels, "annotations": annotations},
            "spec": {"hard": hard}
        });
        Ok(Self {
            binding: configuration.binding.clone(),
            namespace,
            claim_id: claim.id,
            claim_revision: claim.revision,
            quota_plan_sha256: claim.quota_plan_sha256.clone(),
            namespace_document,
            quota_document,
        })
    }
}

/// Provider readback used to fence later Environment adoption.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KubernetesQuotaShellReadback {
    pub namespace_uid: String,
    pub quota_uid: String,
}

/// Kubernetes API provider; no command execution, discovery fallback, or implicit binding exists.
#[derive(Clone)]
pub struct KubernetesCapacityProvider {
    configuration: KubernetesCapacityProviderConfiguration,
    api_server: Url,
    client: Client,
    token: String,
}

impl KubernetesCapacityProvider {
    pub fn new(
        configuration: KubernetesCapacityProviderConfiguration,
    ) -> Result<Self, CapacityProviderError> {
        configuration.validate()?;
        let api_server = Url::parse(&configuration.api_server)
            .map_err(|_| CapacityProviderError::Configuration)?;
        let token = std::fs::read_to_string(&configuration.bearer_token_file)
            .map_err(|_| CapacityProviderError::Configuration)?;
        if token.trim().is_empty() {
            return Err(CapacityProviderError::Configuration);
        }
        let certificate = Certificate::from_pem(
            &std::fs::read(&configuration.cluster_ca_file)
                .map_err(|_| CapacityProviderError::Configuration)?,
        )
        .map_err(|_| CapacityProviderError::Configuration)?;
        let client = Client::builder()
            .https_only(true)
            .add_root_certificate(certificate)
            .timeout(Duration::from_millis(
                configuration.request_timeout_milliseconds,
            ))
            .build()
            .map_err(|_| CapacityProviderError::Configuration)?;
        Ok(Self {
            configuration,
            api_server,
            client,
            token: token.trim().into(),
        })
    }

    /// Creates a quota shell, or verifies a conflicting existing shell has the exact claim fence.
    pub async fn reserve(
        &self,
        plan: &KubernetesQuotaShellPlan,
    ) -> Result<KubernetesQuotaShellReadback, CapacityProviderError> {
        if plan.binding != self.configuration.binding {
            return Err(CapacityProviderError::BindingMismatch);
        }
        self.create_or_verify(
            "api/v1/namespaces",
            &plan.namespace_document,
            &plan.namespace_document,
        )
        .await?;
        let quota_path = format!("api/v1/namespaces/{}/resourcequotas", plan.namespace);
        self.create_or_verify(&quota_path, &plan.quota_document, &plan.quota_document)
            .await?;
        let namespace = self
            .get(&format!("api/v1/namespaces/{}", plan.namespace))
            .await?;
        let quota = self
            .get(&format!(
                "api/v1/namespaces/{}/resourcequotas/{QUOTA_NAME}",
                plan.namespace
            ))
            .await?;
        verify_document(&namespace, &plan.namespace_document)?;
        verify_document(&quota, &plan.quota_document)?;
        Ok(KubernetesQuotaShellReadback {
            namespace_uid: metadata_uid(&namespace)?,
            quota_uid: metadata_uid(&quota)?,
        })
    }

    /// Used only before successful handoff. Environment owns deletion after handoff.
    pub async fn release_before_handoff(
        &self,
        plan: &KubernetesQuotaShellPlan,
    ) -> Result<(), CapacityProviderError> {
        let response = self
            .client
            .delete(self.url(&format!("api/v1/namespaces/{}", plan.namespace))?)
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|_| CapacityProviderError::Unavailable)?;
        if response.status().is_success() || response.status() == StatusCode::NOT_FOUND {
            Ok(())
        } else {
            Err(classify(response.status()))
        }
    }

    async fn create_or_verify(
        &self,
        collection: &str,
        document: &Value,
        expected: &Value,
    ) -> Result<(), CapacityProviderError> {
        let response = self
            .client
            .post(self.url(collection)?)
            .bearer_auth(&self.token)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .header("X-Field-Manager", FIELD_MANAGER)
            .json(document)
            .send()
            .await
            .map_err(|_| CapacityProviderError::Unavailable)?;
        match response.status() {
            status if status.is_success() => Ok(()),
            StatusCode::CONFLICT => {
                let metadata = expected
                    .get("metadata")
                    .and_then(Value::as_object)
                    .ok_or(CapacityProviderError::Plan)?;
                let name = metadata
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or(CapacityProviderError::Plan)?;
                let namespace = metadata.get("namespace").and_then(Value::as_str);
                let path = namespace.map_or_else(
                    || format!("api/v1/namespaces/{name}"),
                    |value| format!("api/v1/namespaces/{value}/resourcequotas/{name}"),
                );
                verify_document(&self.get(&path).await?, expected)
            }
            status => Err(classify(status)),
        }
    }

    async fn get(&self, path: &str) -> Result<Value, CapacityProviderError> {
        let response = self
            .client
            .get(self.url(path)?)
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|_| CapacityProviderError::Unavailable)?;
        if !response.status().is_success() {
            return Err(classify(response.status()));
        }
        response
            .json()
            .await
            .map_err(|_| CapacityProviderError::Readback)
    }

    fn url(&self, path: &str) -> Result<Url, CapacityProviderError> {
        self.api_server
            .join(path)
            .map_err(|_| CapacityProviderError::Configuration)
    }
}

fn verify_document(actual: &Value, expected: &Value) -> Result<(), CapacityProviderError> {
    for pointer in [
        "/metadata/name",
        "/metadata/labels/labweaver.io~1managed-by",
        "/metadata/labels/labweaver.io~1environment-id",
        "/metadata/labels/labweaver.io~1capacity-claim-id",
        "/metadata/annotations/labweaver.io~1capacity-claim-revision",
        "/metadata/annotations/labweaver.io~1quota-plan-sha256",
        "/metadata/annotations/labweaver.io~1provider-binding",
    ] {
        if actual.pointer(pointer) != expected.pointer(pointer) {
            return Err(CapacityProviderError::IdentityMismatch);
        }
    }
    if expected.get("kind").and_then(Value::as_str) == Some("ResourceQuota")
        && actual.pointer("/spec/hard") != expected.pointer("/spec/hard")
    {
        return Err(CapacityProviderError::IdentityMismatch);
    }
    Ok(())
}

fn metadata_uid(document: &Value) -> Result<String, CapacityProviderError> {
    document
        .pointer("/metadata/uid")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or(CapacityProviderError::Readback)
}
fn classify(status: StatusCode) -> CapacityProviderError {
    if matches!(
        status,
        StatusCode::UNAUTHORIZED
            | StatusCode::FORBIDDEN
            | StatusCode::CONFLICT
            | StatusCode::UNPROCESSABLE_ENTITY
    ) {
        CapacityProviderError::Rejected
    } else {
        CapacityProviderError::Unavailable
    }
}
fn valid_dns_label(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 63
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && !value.starts_with('-')
        && !value.ends_with('-')
}

#[derive(Debug, thiserror::Error)]
pub enum CapacityProviderError {
    #[error("LW_RESOURCE_CAPACITY_PROVIDER_CONFIGURATION_INVALID")]
    Configuration,
    #[error("LW_RESOURCE_CAPACITY_PROVIDER_BINDING_MISMATCH")]
    BindingMismatch,
    #[error("LW_RESOURCE_CAPACITY_PLAN_INVALID")]
    Plan,
    #[error("LW_RESOURCE_CAPACITY_EXHAUSTED")]
    GpuUnsupported,
    #[error("LW_RESOURCE_CAPACITY_IDENTITY_MISMATCH")]
    IdentityMismatch,
    #[error("LW_RESOURCE_CAPACITY_READBACK_INVALID")]
    Readback,
    #[error("LW_RESOURCE_CAPACITY_REJECTED")]
    Rejected,
    #[error("LW_RESOURCE_CAPACITY_UNAVAILABLE")]
    Unavailable,
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use contracts::resource::{ResourceTarget, WorkloadResources};
    use contracts::{
        ActorId, CourseId, EnvironmentId, ReleaseId, ResourceApprovalId, ResourceRequestId,
    };

    use super::*;

    #[test]
    fn quota_shell_plan_is_fenced_to_claim_and_never_translates_gpu_catalog_values() {
        let configuration = KubernetesCapacityProviderConfiguration {
            binding: "kubernetes-standard".into(),
            api_server: "https://kubernetes.example.test/".into(),
            bearer_token_file: PathBuf::from("C:/token"),
            cluster_ca_file: PathBuf::from("C:/ca.pem"),
            request_timeout_milliseconds: 5_000,
            namespace_labels: BTreeMap::new(),
        };
        let request = ResourceRequest {
            id: ResourceRequestId::new(),
            generation: 1,
            request_key: "workbench-1".into(),
            requester_id: ActorId::new(),
            course_id: CourseId::new(),
            project_id: None,
            target: ResourceTarget {
                environment_id: EnvironmentId::new(),
                release_id: ReleaseId::new(),
                release_version: 1,
                release_sha256: digest(),
            },
            requested_resources: resources(),
            requested_duration_seconds: 600,
            state: contracts::resource::ResourceRequestState::Reviewing,
            revision: contracts::Revision::new(1).expect("positive"),
            created_at: timestamp(),
            updated_at: timestamp(),
            diagnostic_code: None,
        };
        let claim = CapacityClaim {
            id: contracts::CapacityClaimId::new(),
            request_id: request.id,
            approval_id: ResourceApprovalId::new(),
            provider_binding: configuration.binding.clone(),
            policy_sha256: digest(),
            workload_resources: resources(),
            quota_resources: resources(),
            quota_plan_sha256: digest(),
            revision: contracts::Revision::new(1).expect("positive"),
        };
        let plan = KubernetesQuotaShellPlan::from_claim(&configuration, &request, &claim)
            .expect("valid plan");
        assert_eq!(
            plan.quota_document
                .pointer("/spec/hard/requests.cpu")
                .and_then(Value::as_str),
            Some("500m")
        );
        let expected_claim_id = claim.id.to_string();
        assert_eq!(
            plan.namespace_document
                .pointer("/metadata/labels/labweaver.io~1capacity-claim-id")
                .and_then(Value::as_str),
            Some(expected_claim_id.as_str())
        );
        let mut gpu_claim = claim.clone();
        gpu_claim.quota_resources.gpu = Some(contracts::resource::GpuRequest {
            class: "gpu-a100".into(),
            count: 1,
        });
        assert!(matches!(
            KubernetesQuotaShellPlan::from_claim(&configuration, &request, &gpu_claim),
            Err(CapacityProviderError::GpuUnsupported)
        ));
    }

    fn resources() -> WorkloadResources {
        WorkloadResources {
            cpu_millicores: 500,
            memory_bytes: 512,
            storage_bytes: 1024,
            gpu: None,
        }
    }
    fn digest() -> contracts::Sha256Digest {
        contracts::Sha256Digest::from_str(&"a".repeat(64)).expect("digest")
    }
    fn timestamp() -> contracts::UtcTimestamp {
        contracts::UtcTimestamp::from_str("2026-07-30T00:00:00.000Z").expect("timestamp")
    }
}
