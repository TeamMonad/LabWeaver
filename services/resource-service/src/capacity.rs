//! Durable Resource capacity reconciliation and owner handoff.
//!
//! Resource owns admission, claims, Leases and GPU reservations. Environment owns Kubernetes
//! objects, so this module only sends an explicit Work handoff and reads the bounded cleanup
//! status contract. One-shot Task claims are consumed by Evaluation through the internal owner
//! API; this worker never creates a synthetic Environment or acknowledges a Task.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use auth::ServiceTokenClient;
use contracts::UtcTimestamp;
use contracts::environment::{
    ObservedEnvironmentState, ResourceWorkCleanup, ResourceWorkCleanupStatus, ResourceWorkHandoff,
    ResourceWorkLeaseUpdate,
};
use contracts::resource::{ResourceLeaseState, ResourceTarget};
use reqwest::{Certificate, Client, StatusCode};
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::watch;
use url::Url;

use crate::store::ActiveGpuReservation;

const MAX_KUBERNETES_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
const MAX_KUBERNETES_LIST_ITEMS: u32 = 100_000;
const MAX_KUBERNETES_TOKEN_BYTES: usize = 16 * 1024;
const MAX_KUBERNETES_CA_BYTES: usize = 1024 * 1024;

/// Resource capacity worker configuration. Environment remains the sole owner of runtime
/// objects; the optional GPU observers only read explicit Kubernetes status for capacity
/// accounting.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourceCapacityConfiguration {
    pub poll_interval_milliseconds: u64,
    pub environment_handoff: EnvironmentHandoffConfiguration,
    /// Read-only Kubernetes observers keyed by the Resource provider binding. An empty list is
    /// valid for installations without GPU catalog entries; a configured GPU entry without a
    /// matching observer remains unavailable until an observation is recorded.
    #[serde(default)]
    pub gpu_observers: Vec<GpuCapacityObservationConfiguration>,
}

impl ResourceCapacityConfiguration {
    pub fn build_worker(
        self,
        store: crate::store::PgResourceStore,
        environment_token_client: ServiceTokenClient,
    ) -> Result<CapacityReconcileWorker, CapacityProviderError> {
        if !(100..=60_000).contains(&self.poll_interval_milliseconds) {
            return Err(CapacityProviderError::Configuration);
        }
        let mut gpu_observers = BTreeMap::new();
        for configuration in self.gpu_observers {
            let binding = configuration.provider_binding.clone();
            if gpu_observers
                .insert(binding, KubernetesGpuCapacityObserver::new(configuration)?)
                .is_some()
            {
                return Err(CapacityProviderError::Configuration);
            }
        }
        Ok(CapacityReconcileWorker {
            store,
            environment_handoff: EnvironmentHandoffClient::new(
                self.environment_handoff,
                environment_token_client,
            )?,
            gpu_observers,
            poll_interval: Duration::from_millis(self.poll_interval_milliseconds),
        })
    }
}

/// Read-only Kubernetes node/pod observation configuration for one Resource provider binding.
///
/// The observer reads only Node status and Pod scheduling data. It never creates, patches, or
/// deletes Kubernetes objects. Resource-owned Environment pods are identified by their existing
/// A pod is excluded from external occupancy only when its target identity, namespace,
/// requested allocation, and durable Resource reservation all match. Labels by themselves do not
/// establish ownership.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GpuCapacityObservationConfiguration {
    pub provider_binding: String,
    pub api_server: Url,
    pub bearer_token_file: PathBuf,
    pub cluster_ca_file: PathBuf,
    pub request_timeout_milliseconds: u64,
    pub observation_ttl_seconds: u64,
    pub max_nodes: u32,
    pub max_pods: u32,
}

impl GpuCapacityObservationConfiguration {
    fn validate(&self) -> Result<(), CapacityProviderError> {
        if self.provider_binding.trim().is_empty()
            || self.provider_binding.len() > 120
            || self.api_server.scheme() != "https"
            || self.api_server.host_str().is_none()
            || !self.api_server.username().is_empty()
            || self.api_server.password().is_some()
            || (self.api_server.path() != "" && self.api_server.path() != "/")
            || self.api_server.query().is_some()
            || self.api_server.fragment().is_some()
            || !self.bearer_token_file.is_absolute()
            || !self.cluster_ca_file.is_absolute()
            || !(1..=30_000).contains(&self.request_timeout_milliseconds)
            || !(1..=300).contains(&self.observation_ttl_seconds)
            || !(1..=10_000).contains(&self.max_nodes)
            || !(1..=MAX_KUBERNETES_LIST_ITEMS).contains(&self.max_pods)
        {
            return Err(CapacityProviderError::Configuration);
        }
        Ok(())
    }
}

#[derive(Clone)]
struct KubernetesGpuCapacityObserver {
    provider_binding: String,
    api_server: Url,
    client: Client,
    token_file: PathBuf,
    observation_ttl: Duration,
    max_nodes: usize,
    max_pods: usize,
}

impl KubernetesGpuCapacityObserver {
    fn new(
        configuration: GpuCapacityObservationConfiguration,
    ) -> Result<Self, CapacityProviderError> {
        configuration.validate()?;
        // Validate the initial projected token, but retain only its controlled file path. Service
        // account token projections rotate in place; caching the initial string would leave a
        // long-running observer permanently unauthorized after the first rotation.
        read_bearer_token(&configuration.bearer_token_file)
            .map_err(|_| CapacityProviderError::Configuration)?;
        let ca = std::fs::read(&configuration.cluster_ca_file)
            .map_err(|_| CapacityProviderError::Configuration)?;
        if ca.len() > MAX_KUBERNETES_CA_BYTES {
            return Err(CapacityProviderError::Configuration);
        }
        let client = Client::builder()
            .no_proxy()
            .https_only(true)
            .tls_built_in_root_certs(false)
            .add_root_certificate(
                Certificate::from_pem(&ca).map_err(|_| CapacityProviderError::Configuration)?,
            )
            .timeout(Duration::from_millis(
                configuration.request_timeout_milliseconds,
            ))
            .build()
            .map_err(|_| CapacityProviderError::Configuration)?;
        Ok(Self {
            provider_binding: configuration.provider_binding,
            api_server: configuration.api_server,
            client,
            token_file: configuration.bearer_token_file,
            observation_ttl: Duration::from_secs(configuration.observation_ttl_seconds),
            max_nodes: configuration.max_nodes as usize,
            max_pods: configuration.max_pods as usize,
        })
    }

    async fn observe(
        &self,
        entries: &[contracts::resource::GpuCatalogEntry],
        active_reservations: &[ActiveGpuReservation],
    ) -> Result<BTreeMap<contracts::GpuCatalogEntryId, u32>, CapacityProviderError> {
        if entries.is_empty() {
            return Err(CapacityProviderError::ObservationReadback);
        }
        let nodes = self.list("api/v1/nodes", self.max_nodes).await?;
        let pods = self.list("api/v1/pods", self.max_pods).await?;
        let mut observed = BTreeMap::new();
        let bindings = entries
            .iter()
            .map(|entry| {
                if entry.provider_binding != self.provider_binding {
                    return Err(CapacityProviderError::ObservationBindingMismatch);
                }
                Ok(entry.allocation_binding.as_str())
            })
            .collect::<Result<BTreeSet<_>, _>>()?;
        for allocation_binding in bindings {
            let mut eligible_nodes = BTreeMap::new();
            for node in &nodes {
                if let Some(snapshot) = eligible_node(node, allocation_binding)?
                    && eligible_nodes
                        .insert(snapshot.name, snapshot.allocatable_units)
                        .is_some()
                {
                    return Err(CapacityProviderError::ObservationReadback);
                }
            }
            let mut outside_occupancy = 0_u32;
            for pod in &pods {
                let Some(node_name) = pod_node_name(pod)? else {
                    continue;
                };
                if !eligible_nodes.contains_key(&node_name)
                    || pod_is_resource_owned(pod, active_reservations, allocation_binding)?
                {
                    continue;
                }
                outside_occupancy = outside_occupancy
                    .checked_add(pod_requested_units(pod, allocation_binding)?)
                    .ok_or(CapacityProviderError::ObservationReadback)?;
            }
            for entry in entries
                .iter()
                .filter(|entry| entry.allocation_binding == allocation_binding)
            {
                let total = nodes_capacity(&eligible_nodes, allocation_binding, &entry.class)?;
                let available = total
                    .min(entry.capacity_units)
                    .saturating_sub(outside_occupancy);
                observed.insert(entry.id, available);
            }
        }
        Ok(observed)
    }

    async fn list(&self, path: &str, limit: usize) -> Result<Vec<Value>, CapacityProviderError> {
        // Re-read the source-controlled projected token for every request. There is no fallback
        // credential: a missing or malformed rotation makes this observation unavailable and
        // leaves the last durable snapshot to expire normally.
        let token = read_bearer_token(&self.token_file).map_err(|error| {
            tracing::warn!(
                event = "resource.gpu_capacity.token_read_failed",
                diagnostic_code = error.diagnostic(),
                retryable = true,
            );
            CapacityProviderError::ObservationUnavailable
        })?;
        let mut url = self
            .api_server
            .join(path)
            .map_err(|_| CapacityProviderError::Configuration)?;
        url.query_pairs_mut()
            .append_pair("limit", &limit.to_string());
        let response = self
            .client
            .get(url)
            .bearer_auth(token)
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(|_| CapacityProviderError::ObservationUnavailable)?;
        if response.status() == StatusCode::UNAUTHORIZED
            || response.status() == StatusCode::FORBIDDEN
        {
            return Err(CapacityProviderError::ObservationPermissionDenied);
        }
        if !response.status().is_success() {
            return Err(CapacityProviderError::ObservationUnavailable);
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_KUBERNETES_RESPONSE_BYTES as u64)
        {
            return Err(CapacityProviderError::ObservationBoundExceeded);
        }
        let body = response
            .bytes()
            .await
            .map_err(|_| CapacityProviderError::ObservationUnavailable)?;
        if body.len() > MAX_KUBERNETES_RESPONSE_BYTES {
            return Err(CapacityProviderError::ObservationBoundExceeded);
        }
        let document: Value = serde_json::from_slice(&body)
            .map_err(|_| CapacityProviderError::ObservationReadback)?;
        let metadata = document
            .get("metadata")
            .and_then(Value::as_object)
            .ok_or(CapacityProviderError::ObservationReadback)?;
        if metadata
            .get("continue")
            .and_then(Value::as_str)
            .is_some_and(|value| !value.is_empty())
        {
            // A continue token means the bounded page omitted objects. Failing closed avoids
            // authorizing capacity from a partial cluster view.
            return Err(CapacityProviderError::ObservationBoundExceeded);
        }
        let items = document
            .get("items")
            .and_then(Value::as_array)
            .ok_or(CapacityProviderError::ObservationReadback)?;
        if items.len() > limit {
            return Err(CapacityProviderError::ObservationBoundExceeded);
        }
        Ok(items.clone())
    }
}

fn read_bearer_token(path: &Path) -> Result<String, CapacityProviderError> {
    let token = std::fs::read(path).map_err(|_| CapacityProviderError::ObservationUnavailable)?;
    if token.len() > MAX_KUBERNETES_TOKEN_BYTES {
        return Err(CapacityProviderError::ObservationBoundExceeded);
    }
    let token = std::str::from_utf8(&token)
        .map_err(|_| CapacityProviderError::ObservationReadback)?
        .trim();
    if token.is_empty() || token.chars().any(char::is_whitespace) {
        return Err(CapacityProviderError::ObservationReadback);
    }
    Ok(token.to_owned())
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct EligibleNode {
    name: String,
    allocatable_units: u32,
}

fn eligible_node(
    node: &Value,
    allocation_binding: &str,
) -> Result<Option<EligibleNode>, CapacityProviderError> {
    let object = node
        .as_object()
        .ok_or(CapacityProviderError::ObservationReadback)?;
    let metadata = object
        .get("metadata")
        .and_then(Value::as_object)
        .ok_or(CapacityProviderError::ObservationReadback)?;
    let name = metadata
        .get("name")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or(CapacityProviderError::ObservationReadback)?;
    let spec = object
        .get("spec")
        .and_then(Value::as_object)
        .ok_or(CapacityProviderError::ObservationReadback)?;
    let unschedulable = spec
        .get("unschedulable")
        .map(|value| {
            value
                .as_bool()
                .ok_or(CapacityProviderError::ObservationReadback)
        })
        .transpose()?
        .unwrap_or(false);
    let status = object
        .get("status")
        .and_then(Value::as_object)
        .ok_or(CapacityProviderError::ObservationReadback)?;
    let conditions = status
        .get("conditions")
        .and_then(Value::as_array)
        .ok_or(CapacityProviderError::ObservationReadback)?;
    let ready = conditions.iter().any(|condition| {
        condition.get("type").and_then(Value::as_str) == Some("Ready")
            && condition.get("status").and_then(Value::as_str) == Some("True")
    });
    let allocatable = status
        .get("allocatable")
        .and_then(Value::as_object)
        .ok_or(CapacityProviderError::ObservationReadback)?;
    let allocatable_units = allocatable
        .get(allocation_binding)
        .map(parse_gpu_quantity)
        .transpose()?
        .unwrap_or(0);
    if !ready || unschedulable {
        return Ok(None);
    }
    Ok(Some(EligibleNode {
        name: name.to_owned(),
        allocatable_units,
    }))
}

fn nodes_capacity(
    nodes: &BTreeMap<String, u32>,
    allocation_binding: &str,
    class: &str,
) -> Result<u32, CapacityProviderError> {
    let total = nodes.values().try_fold(0_u32, |total, units| {
        total
            .checked_add(*units)
            .ok_or(CapacityProviderError::ObservationReadback)
    })?;
    tracing::debug!(
        event = "resource.gpu_capacity.observed",
        gpu_class = class,
        allocation_binding,
        eligible_nodes = nodes.len(),
        allocatable_units = total,
    );
    Ok(total)
}

fn parse_gpu_quantity(value: &Value) -> Result<u32, CapacityProviderError> {
    let value = value
        .as_str()
        .ok_or(CapacityProviderError::ObservationReadback)?;
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(CapacityProviderError::ObservationReadback);
    }
    value
        .parse::<u32>()
        .map_err(|_| CapacityProviderError::ObservationReadback)
}

fn pod_node_name(pod: &Value) -> Result<Option<String>, CapacityProviderError> {
    let spec = pod
        .get("spec")
        .and_then(Value::as_object)
        .ok_or(CapacityProviderError::ObservationReadback)?;
    spec.get("nodeName")
        .map(|value| {
            value
                .as_str()
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .ok_or(CapacityProviderError::ObservationReadback)
        })
        .transpose()
}

fn pod_is_resource_owned(
    pod: &Value,
    active_reservations: &[ActiveGpuReservation],
    allocation_binding: &str,
) -> Result<bool, CapacityProviderError> {
    let metadata = pod
        .get("metadata")
        .and_then(Value::as_object)
        .ok_or(CapacityProviderError::ObservationReadback)?;
    let labels = metadata
        .get("labels")
        .map(|value| {
            value
                .as_object()
                .ok_or(CapacityProviderError::ObservationReadback)
        })
        .transpose()?;
    let environment_id = pod_label(labels, "labweaver.io/environment-id")?;
    let task_run_id = pod_label(labels, "labweaver.io/task-run-id")?;
    let namespace = metadata
        .get("namespace")
        .map(|value| {
            value
                .as_str()
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .ok_or(CapacityProviderError::ObservationReadback)
        })
        .transpose()?;

    for reservation in active_reservations.iter().filter(|reservation| {
        // The allocation binding is the physical resource key. A catalog
        // revision/provider may change while an existing reservation is
        // still represented by the same workload; retain it as owned so the
        // observer does not count that workload as external capacity usage.
        reservation.allocation_binding == allocation_binding
    }) {
        let namespace_matches = match &reservation.target {
            ResourceTarget::Environment { .. } => {
                reservation.namespace_name.as_deref() == namespace.as_deref()
            }
            ResourceTarget::Task { .. } => reservation
                .namespace_name
                .as_deref()
                .is_some_and(|expected| namespace.as_deref() == Some(expected)),
        };
        if !namespace_matches {
            continue;
        }
        let target_matches = match &reservation.target {
            ResourceTarget::Environment {
                environment_id: id, ..
            } => {
                task_run_id.is_none() && environment_id.as_deref() == Some(id.to_string().as_str())
            }
            ResourceTarget::Task { task_run_id: id } => {
                environment_id.is_none() && task_run_id.as_deref() == Some(id.to_string().as_str())
            }
        };
        if target_matches && pod_requested_units(pod, allocation_binding)? == reservation.units {
            tracing::debug!(
                event = "resource.gpu_capacity.reservation_excluded",
                claim_id = %reservation.claim_id,
                entry_id = %reservation.entry_id,
                allocation_binding,
                units = reservation.units,
            );
            return Ok(true);
        }
    }
    Ok(false)
}

fn pod_label(
    labels: Option<&serde_json::Map<String, Value>>,
    key: &str,
) -> Result<Option<String>, CapacityProviderError> {
    let Some(value) = labels.and_then(|labels| labels.get(key)) else {
        return Ok(None);
    };
    value
        .as_str()
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or(CapacityProviderError::ObservationReadback)
        .map(Some)
}

fn pod_requested_units(
    pod: &Value,
    allocation_binding: &str,
) -> Result<u32, CapacityProviderError> {
    let object = pod
        .as_object()
        .ok_or(CapacityProviderError::ObservationReadback)?;
    let status = object
        .get("status")
        .and_then(Value::as_object)
        .ok_or(CapacityProviderError::ObservationReadback)?;
    let phase = status
        .get("phase")
        .and_then(Value::as_str)
        .ok_or(CapacityProviderError::ObservationReadback)?;
    if matches!(phase, "Succeeded" | "Failed") {
        return Ok(0);
    }
    if !matches!(phase, "Pending" | "Running" | "Unknown") {
        return Err(CapacityProviderError::ObservationReadback);
    }
    let spec = object
        .get("spec")
        .and_then(Value::as_object)
        .ok_or(CapacityProviderError::ObservationReadback)?;
    let containers = spec
        .get("containers")
        .and_then(Value::as_array)
        .ok_or(CapacityProviderError::ObservationReadback)?;
    let regular = containers.iter().try_fold(0_u32, |total, container| {
        total
            .checked_add(container_requested_units(container, allocation_binding)?)
            .ok_or(CapacityProviderError::ObservationReadback)
    })?;
    let init_max = spec
        .get("initContainers")
        .map(|value| {
            let containers = value
                .as_array()
                .ok_or(CapacityProviderError::ObservationReadback)?;
            containers.iter().try_fold(0_u32, |maximum, container| {
                Ok::<_, CapacityProviderError>(
                    maximum.max(container_requested_units(container, allocation_binding)?),
                )
            })
        })
        .transpose()?
        .unwrap_or(0);
    Ok(regular.max(init_max))
}

fn container_requested_units(
    container: &Value,
    allocation_binding: &str,
) -> Result<u32, CapacityProviderError> {
    let empty = serde_json::Map::new();
    let resources = match container.get("resources") {
        Some(value) => value
            .as_object()
            .ok_or(CapacityProviderError::ObservationReadback)?,
        None => &empty,
    };
    let requests = quantity_from_resource_map(resources, "requests", allocation_binding)?;
    let limits = quantity_from_resource_map(resources, "limits", allocation_binding)?;
    Ok(requests.max(limits))
}

fn quantity_from_resource_map(
    resources: &serde_json::Map<String, Value>,
    field: &str,
    allocation_binding: &str,
) -> Result<u32, CapacityProviderError> {
    let Some(values) = resources.get(field) else {
        return Ok(0);
    };
    let values = values
        .as_object()
        .ok_or(CapacityProviderError::ObservationReadback)?;
    values
        .get(allocation_binding)
        .map(parse_gpu_quantity)
        .transpose()
        .map(|value| value.unwrap_or(0))
}

/// Explicit TLS destination for Environment-owned Work commands. The bearer token carries the
/// service identity; no ambient roots, proxy, discovery, or alternate endpoint is permitted.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentHandoffConfiguration {
    pub base_uri: Url,
    pub ca_file: PathBuf,
    pub timeout_milliseconds: u64,
    pub system_actor_id: contracts::ActorId,
}

fn cleanup_lease_revision(
    synced_revision: Option<contracts::Revision>,
    current_revision: contracts::Revision,
) -> Result<contracts::Revision, CapacityProviderError> {
    let synced_revision = synced_revision.ok_or(CapacityProviderError::HandoffFence)?;
    if synced_revision > current_revision {
        return Err(CapacityProviderError::HandoffFence);
    }
    Ok(synced_revision)
}

fn cleanup_readback_matches(
    synced_revision: Option<contracts::Revision>,
    current_revision: contracts::Revision,
    reported_revision: contracts::Revision,
) -> bool {
    cleanup_lease_revision(synced_revision, current_revision)
        .is_ok_and(|expected_revision| expected_revision == reported_revision)
}

#[derive(Clone)]
struct EnvironmentHandoffClient {
    base_uri: Url,
    client: Client,
    token_client: ServiceTokenClient,
    system_actor_id: contracts::ActorId,
}

impl EnvironmentHandoffClient {
    fn new(
        configuration: EnvironmentHandoffConfiguration,
        token_client: ServiceTokenClient,
    ) -> Result<Self, CapacityProviderError> {
        if configuration.base_uri.scheme() != "https"
            || configuration.base_uri.host_str().is_none()
            || !configuration.base_uri.username().is_empty()
            || configuration.base_uri.password().is_some()
            || (configuration.base_uri.path() != "" && configuration.base_uri.path() != "/")
            || configuration.base_uri.query().is_some()
            || configuration.base_uri.fragment().is_some()
            || !configuration.ca_file.is_absolute()
            || !(1..=30_000).contains(&configuration.timeout_milliseconds)
        {
            return Err(CapacityProviderError::Configuration);
        }
        let ca = std::fs::read(&configuration.ca_file)
            .map_err(|_| CapacityProviderError::Configuration)?;
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .https_only(true)
            .tls_built_in_root_certs(false)
            .add_root_certificate(
                Certificate::from_pem(&ca).map_err(|_| CapacityProviderError::Configuration)?,
            )
            .timeout(Duration::from_millis(configuration.timeout_milliseconds))
            .build()
            .map_err(|_| CapacityProviderError::Configuration)?;
        Ok(Self {
            base_uri: configuration.base_uri,
            client,
            token_client,
            system_actor_id: configuration.system_actor_id,
        })
    }

    async fn send(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, CapacityProviderError> {
        let mut headers = reqwest::header::HeaderMap::new();
        self.token_client
            .bearer_auth(&mut headers)
            .await
            .map_err(|_| CapacityProviderError::EnvironmentAuthentication)?;
        request
            .headers(headers)
            .send()
            .await
            .map_err(|_| CapacityProviderError::Unavailable)
    }

    async fn handoff(
        &self,
        item: &crate::store::ProvisioningCapacityClaim,
    ) -> Result<(), CapacityProviderError> {
        if item.lease.state != ResourceLeaseState::Active
            || !matches!(
                item.claim.state,
                contracts::resource::CapacityClaimState::Provisioning
                    | contracts::resource::CapacityClaimState::Ready
            )
        {
            return Err(CapacityProviderError::HandoffFence);
        }
        let ResourceTarget::Environment {
            environment_id,
            release_id,
            release_version,
        } = item.request.target
        else {
            return Err(CapacityProviderError::TaskOwnerRequired);
        };
        let handoff = ResourceWorkHandoff {
            version: 1,
            request_id: item.request.id,
            request_revision: item.request.revision,
            lease_id: item.lease.id,
            lease_revision: item.lease.revision,
            claim_id: item.claim.id,
            claim_revision: item.claim.revision,
            environment_id,
            project_id: item.request.project_id,
            course_id: item.request.course_id,
            owner_actor_id: item.request.requester_id,
            display_label: item.request.request_key.clone(),
            release_id,
            release_version,
            provider_binding: item.claim.provider_binding.clone(),
            capacity_binding: item.claim.id.to_string(),
            approved_resources: item.claim.workload_resources.clone(),
            gpu_allocation: item.claim.gpu_allocation.clone(),
            trace_id: format!("resource-handoff-{}", item.claim.id),
        };
        handoff
            .validate()
            .map_err(|_| CapacityProviderError::HandoffFence)?;
        let response = self
            .send(
                self.client
                    .post(self.endpoint("internal/v1/resource/work-handoffs")?)
                    .json(&handoff),
            )
            .await?;
        if response.status() == StatusCode::ACCEPTED {
            Ok(())
        } else {
            Err(CapacityProviderError::HandoffRejected)
        }
    }

    async fn sync_lease(
        &self,
        item: &crate::store::ProvisioningCapacityClaim,
    ) -> Result<(), CapacityProviderError> {
        let expires_at = item
            .lease
            .expires_at
            .ok_or(CapacityProviderError::HandoffFence)?;
        let ResourceTarget::Environment { environment_id, .. } = item.request.target else {
            return Err(CapacityProviderError::TaskOwnerRequired);
        };
        let update = ResourceWorkLeaseUpdate {
            version: 1,
            lease_id: item.lease.id,
            lease_revision: item.lease.revision,
            environment_id,
            project_id: item.request.project_id,
            course_id: item.request.course_id,
            owner_actor_id: item.request.requester_id,
            capacity_binding: item.claim.id.to_string(),
            expires_at,
            trace_id: format!("resource-lease-sync-{}", item.lease.id),
        };
        update
            .validate()
            .map_err(|_| CapacityProviderError::HandoffFence)?;
        let response = self
            .send(
                self.client
                    .post(self.endpoint("internal/v1/resource/work-lease-updates")?)
                    .json(&update),
            )
            .await?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(CapacityProviderError::LeaseSyncRejected)
        }
    }

    async fn request_cleanup(
        &self,
        item: &crate::store::ProvisioningCapacityClaim,
    ) -> Result<(), CapacityProviderError> {
        let ResourceTarget::Environment { environment_id, .. } = item.request.target else {
            return Err(CapacityProviderError::TaskOwnerRequired);
        };
        let lease_revision =
            cleanup_lease_revision(item.lease_synced_revision, item.lease.revision)?;
        let cleanup = ResourceWorkCleanup {
            version: 1,
            lease_id: item.lease.id,
            lease_revision,
            environment_id,
            project_id: item.request.project_id,
            course_id: item.request.course_id,
            owner_actor_id: item.request.requester_id,
            capacity_binding: item.claim.id.to_string(),
            reason_code: if item.lease.revoke_reason_code.is_some() {
                "LW_RESOURCE_LEASE_REVOKED"
            } else {
                "LW_RESOURCE_LEASE_EXPIRED"
            }
            .into(),
            trace_id: format!("resource-work-cleanup-{}", item.lease.id),
        };
        cleanup
            .validate()
            .map_err(|_| CapacityProviderError::HandoffFence)?;
        let response = self
            .send(
                self.client
                    .post(self.endpoint("internal/v1/resource/work-cleanups")?)
                    .json(&cleanup),
            )
            .await?;
        if response.status() == StatusCode::ACCEPTED || response.status() == StatusCode::CONFLICT {
            Ok(())
        } else {
            Err(CapacityProviderError::CleanupRejected)
        }
    }

    async fn cleanup_status(
        &self,
        item: &crate::store::ProvisioningCapacityClaim,
    ) -> Result<ResourceWorkCleanupStatus, CapacityProviderError> {
        let ResourceTarget::Environment { environment_id, .. } = item.request.target else {
            return Err(CapacityProviderError::TaskOwnerRequired);
        };
        let response = self
            .send(self.client.get(self.endpoint(&format!(
                "internal/v1/resource/work-cleanups/{environment_id}"
            ))?))
            .await?;
        if !response.status().is_success() {
            return Err(CapacityProviderError::CleanupRejected);
        }
        let status = response
            .json::<ResourceWorkCleanupStatus>()
            .await
            .map_err(|_| CapacityProviderError::Readback)?;
        status
            .validate()
            .map_err(|_| CapacityProviderError::Readback)?;
        Ok(status)
    }

    fn endpoint(&self, path: &str) -> Result<Url, CapacityProviderError> {
        self.base_uri
            .join(path)
            .map_err(|_| CapacityProviderError::Configuration)
    }
}

/// Reconciles durable Resource claims and Environment owner acknowledgements.
pub struct CapacityReconcileWorker {
    store: crate::store::PgResourceStore,
    environment_handoff: EnvironmentHandoffClient,
    gpu_observers: BTreeMap<String, KubernetesGpuCapacityObserver>,
    poll_interval: Duration,
}

impl CapacityReconcileWorker {
    /// Reconciles due capacity and cleanup work. A Task target is deliberately left to the
    /// Evaluation owner API after it has claimed the durable reservation.
    pub async fn reconcile_once(&self) -> Result<bool, crate::store::ResourceStoreError> {
        let mut did_work = false;
        if self.reconcile_gpu_observations().await? {
            did_work = true;
        }
        if let Some(item) = self.store.claim_next_capacity_shell().await? {
            did_work = true;
            self.reconcile_provisioning(item).await?;
        }
        if self.reconcile_handoff().await? {
            did_work = true;
        }
        if self.reconcile_lease_sync().await? {
            did_work = true;
        }
        if self.reconcile_lease_cleanup().await? {
            did_work = true;
        }
        Ok(did_work)
    }

    async fn reconcile_gpu_observations(&self) -> Result<bool, crate::store::ResourceStoreError> {
        let entries = self.store.list_gpu_catalog().await?;
        let mut by_provider = BTreeMap::<String, Vec<contracts::resource::GpuCatalogEntry>>::new();
        for entry in entries.into_iter().filter(|entry| entry.active) {
            by_provider
                .entry(entry.provider_binding.clone())
                .or_default()
                .push(entry);
        }
        if by_provider.is_empty() {
            return Ok(false);
        }
        let active_reservations = self.store.list_active_gpu_reservations().await?;
        let mut did_work = false;
        for (provider_binding, entries) in by_provider {
            let Some(observer) = self.gpu_observers.get(&provider_binding) else {
                let error = CapacityProviderError::ObservationProviderMissing;
                tracing::error!(
                    event = "resource.gpu_capacity.observer_missing",
                    provider_binding,
                    diagnostic_code = error.diagnostic(),
                );
                continue;
            };
            let ttl_seconds = i64::try_from(observer.observation_ttl.as_secs())
                .map_err(|_| crate::store::ResourceStoreError::CapacityReadbackInvalid)?;
            match observer.observe(&entries, &active_reservations).await {
                Ok(observations) => {
                    // Start the observation TTL after the provider read has completed. A slow
                    // API request must not make a fresh snapshot stale before it can be used.
                    let observed_at = self.store.current_time().await?;
                    let valid_until = UtcTimestamp::from_utc(
                        observed_at.get() + time::Duration::seconds(ttl_seconds),
                    )
                    .map_err(|_| crate::store::ResourceStoreError::CapacityReadbackInvalid)?;
                    for entry in entries {
                        let available_units = observations
                            .get(&entry.id)
                            .copied()
                            .ok_or(crate::store::ResourceStoreError::GpuObservationInvalid)?;
                        self.store
                            .record_gpu_capacity_observation(
                                entry.id,
                                available_units,
                                &format!("{}:node-status", entry.provider_binding),
                                observed_at,
                                valid_until,
                            )
                            .await?;
                    }
                    did_work = true;
                }
                Err(error) => {
                    tracing::error!(
                        event = "resource.gpu_capacity.observation_failed",
                        provider_binding,
                        diagnostic_code = error.diagnostic(),
                    );
                }
            }
        }
        Ok(did_work)
    }

    async fn reconcile_provisioning(
        &self,
        mut item: crate::store::ProvisioningCapacityClaim,
    ) -> Result<(), crate::store::ResourceStoreError> {
        if item.request.target.environment_id().is_none() {
            return Ok(());
        }
        if item.lease.state == ResourceLeaseState::Allocating {
            let now = self.store.current_time().await?;
            let duration = i64::try_from(item.request.requested_duration_seconds)
                .map_err(|_| crate::store::ResourceStoreError::CapacityReadbackInvalid)?;
            let expires_at = UtcTimestamp::from_utc(now.get() + time::Duration::seconds(duration))
                .map_err(|_| crate::store::ResourceStoreError::CapacityReadbackInvalid)?;
            self.store
                .activate_lease(
                    item.lease.id,
                    item.lease.revision,
                    now,
                    expires_at,
                    self.environment_handoff.system_actor_id,
                    &format!("resource-lease-activate-{}", item.claim.id),
                )
                .await?;
            let Some(refreshed) = self.store.refresh_provisioning_claim(item.claim.id).await?
            else {
                return Ok(());
            };
            item = refreshed;
        }
        if let Err(error) = self.environment_handoff.handoff(&item).await {
            tracing::error!(
                event = "resource.capacity.handoff_failed",
                claim_id = %item.claim.id,
                diagnostic_code = %error.diagnostic()
            );
            self.store
                .retry_or_block_capacity_handoff(
                    item.claim.id,
                    item.claim.revision,
                    error.diagnostic(),
                )
                .await?;
        } else {
            self.store
                .mark_capacity_handed_off(
                    item.claim.id,
                    item.claim.revision,
                    item.lease.id,
                    item.lease.revision,
                )
                .await?;
        }
        Ok(())
    }

    async fn reconcile_handoff(&self) -> Result<bool, crate::store::ResourceStoreError> {
        let Some(item) = self.store.next_provisioning_capacity_handoff().await? else {
            return Ok(false);
        };
        match self.environment_handoff.handoff(&item).await {
            Ok(()) => {
                self.store
                    .mark_capacity_handed_off(
                        item.claim.id,
                        item.claim.revision,
                        item.lease.id,
                        item.lease.revision,
                    )
                    .await?;
            }
            Err(error) => {
                tracing::error!(
                    event = "resource.capacity.handoff_failed",
                    claim_id = %item.claim.id,
                    diagnostic_code = %error.diagnostic()
                );
                self.store
                    .retry_or_block_capacity_handoff(
                        item.claim.id,
                        item.claim.revision,
                        error.diagnostic(),
                    )
                    .await?;
            }
        }
        Ok(true)
    }

    async fn reconcile_lease_sync(&self) -> Result<bool, crate::store::ResourceStoreError> {
        let Some(item) = self.store.next_unsynced_active_lease().await? else {
            return Ok(false);
        };
        match self.environment_handoff.sync_lease(&item).await {
            Ok(()) => {
                self.store
                    .mark_lease_synced(item.claim.id, item.lease.revision)
                    .await?;
            }
            Err(error) => {
                tracing::error!(
                    event = "resource.lease.sync_failed",
                    lease_id = %item.lease.id,
                    diagnostic_code = %error.diagnostic()
                );
            }
        }
        Ok(true)
    }

    async fn reconcile_lease_cleanup(&self) -> Result<bool, crate::store::ResourceStoreError> {
        let Some(item) = self
            .store
            .next_lease_cleanup(self.environment_handoff.system_actor_id)
            .await?
        else {
            return Ok(false);
        };
        match item.claim.state {
            contracts::resource::CapacityClaimState::HandedOff => {
                if let Err(error) = self.environment_handoff.request_cleanup(&item).await {
                    tracing::error!(
                        event = "resource.lease.cleanup_request_failed",
                        lease_id = %item.lease.id,
                        diagnostic_code = %error.diagnostic()
                    );
                    self.store
                        .record_reconciliation_failure(
                            item.claim.id,
                            "expire_environment",
                            error.diagnostic(),
                        )
                        .await?;
                } else {
                    self.store
                        .mark_capacity_releasing(item.claim.id, item.claim.revision)
                        .await?;
                }
            }
            contracts::resource::CapacityClaimState::Reserved
            | contracts::resource::CapacityClaimState::Provisioning
            | contracts::resource::CapacityClaimState::Blocked
            | contracts::resource::CapacityClaimState::Ready => {
                // The target never accepted a handoff. There is no Environment object to clean;
                // release the pre-handoff Resource reservation through the separate fence.
                let releasing = self
                    .store
                    .mark_capacity_releasing(item.claim.id, item.claim.revision)
                    .await?;
                self.store
                    .complete_pre_handoff_release(
                        releasing.id,
                        releasing.revision,
                        item.lease.id,
                        item.lease.revision,
                        self.environment_handoff.system_actor_id,
                        &format!("resource-pre-handoff-release-{}", item.claim.id),
                    )
                    .await?;
            }
            contracts::resource::CapacityClaimState::Releasing => {
                self.reconcile_releasing_capacity(&item).await?;
            }
            contracts::resource::CapacityClaimState::Released => {}
        }
        Ok(true)
    }

    async fn reconcile_releasing_capacity(
        &self,
        item: &crate::store::ProvisioningCapacityClaim,
    ) -> Result<(), crate::store::ResourceStoreError> {
        let Ok(lease_synced_revision) =
            cleanup_lease_revision(item.lease_synced_revision, item.lease.revision)
        else {
            tracing::error!(
                event = "resource.lease.cleanup_fence_invalid",
                lease_id = %item.lease.id,
                claim_id = %item.claim.id,
                lease_synced_revision = ?item.lease_synced_revision,
                lease_revision = item.lease.revision.get(),
                diagnostic_code = "LW_RESOURCE_ENVIRONMENT_HANDOFF_FENCE_INVALID"
            );
            self.store
                .record_reconciliation_failure(
                    item.claim.id,
                    "expire_environment",
                    "LW_RESOURCE_ENVIRONMENT_HANDOFF_FENCE_INVALID",
                )
                .await?;
            return Ok(());
        };
        let status = match self.environment_handoff.cleanup_status(item).await {
            Ok(status) => status,
            Err(error) => {
                tracing::error!(
                    event = "resource.lease.cleanup_readback_failed",
                    lease_id = %item.lease.id,
                    diagnostic_code = %error.diagnostic()
                );
                self.store
                    .record_reconciliation_failure(
                        item.claim.id,
                        "expire_environment",
                        error.diagnostic(),
                    )
                    .await?;
                return Ok(());
            }
        };
        let ResourceTarget::Environment { environment_id, .. } = item.request.target else {
            return Err(crate::store::ResourceStoreError::CapacityClaimStateConflict);
        };
        let identity_matches = status.environment_id == environment_id
            && status.project_id == item.request.project_id
            && status.course_id == item.request.course_id
            && status.owner_actor_id == item.request.requester_id
            && status.lease_id == item.lease.id
            && cleanup_readback_matches(
                Some(lease_synced_revision),
                item.lease.revision,
                status.lease_revision,
            )
            && status.capacity_binding == item.claim.id.to_string();
        if !identity_matches {
            tracing::error!(
                event = "resource.lease.cleanup_identity_mismatch",
                lease_id = %item.lease.id,
                claim_id = %item.claim.id,
                diagnostic_code = "LW_RESOURCE_CAPACITY_IDENTITY_MISMATCH"
            );
            self.store
                .record_reconciliation_failure(
                    item.claim.id,
                    "expire_environment",
                    "LW_RESOURCE_CAPACITY_IDENTITY_MISMATCH",
                )
                .await?;
            return Ok(());
        }
        if !status.cleanup_complete || status.observed_state != ObservedEnvironmentState::Deleted {
            if status.observed_state == ObservedEnvironmentState::Failed {
                let diagnostic = status
                    .diagnostic_code
                    .as_deref()
                    .unwrap_or("LW_RESOURCE_ENVIRONMENT_CLEANUP_FAILED");
                self.store
                    .record_reconciliation_failure(item.claim.id, "expire_environment", diagnostic)
                    .await?;
            }
            return Ok(());
        }
        self.store
            .complete_capacity_release(
                item.claim.id,
                item.claim.revision,
                item.lease.id,
                item.lease.revision,
                self.environment_handoff.system_actor_id,
                &format!("resource-capacity-release-{}", item.claim.id),
            )
            .await?;
        Ok(())
    }

    /// Runs until shutdown.
    pub async fn run(
        &self,
        mut shutdown: watch::Receiver<bool>,
    ) -> Result<(), crate::store::ResourceStoreError> {
        let mut interval = tokio::time::interval(self.poll_interval);
        loop {
            tokio::select! {
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() { return Ok(()); }
                }
                _ = interval.tick() => { let _ = self.reconcile_once().await?; }
            }
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CapacityProviderError {
    #[error("LW_RESOURCE_CAPACITY_CONFIGURATION_INVALID")]
    Configuration,
    #[error("LW_RESOURCE_CAPACITY_IDENTITY_MISMATCH")]
    IdentityMismatch,
    #[error("LW_RESOURCE_CAPACITY_READBACK_INVALID")]
    Readback,
    #[error("LW_RESOURCE_CAPACITY_UNAVAILABLE")]
    Unavailable,
    #[error("LW_RESOURCE_ENVIRONMENT_HANDOFF_FENCE_INVALID")]
    HandoffFence,
    #[error("LW_RESOURCE_ENVIRONMENT_HANDOFF_REJECTED")]
    HandoffRejected,
    #[error("LW_RESOURCE_ENVIRONMENT_LEASE_SYNC_REJECTED")]
    LeaseSyncRejected,
    #[error("LW_RESOURCE_ENVIRONMENT_CLEANUP_REJECTED")]
    CleanupRejected,
    #[error("LW_RESOURCE_TASK_OWNER_REQUIRED")]
    TaskOwnerRequired,
    #[error("LW_RESOURCE_ENVIRONMENT_AUTHENTICATION_FAILED")]
    EnvironmentAuthentication,
    #[error("LW_RESOURCE_GPU_OBSERVATION_PROVIDER_MISSING")]
    ObservationProviderMissing,
    #[error("LW_RESOURCE_GPU_OBSERVATION_BINDING_MISMATCH")]
    ObservationBindingMismatch,
    #[error("LW_RESOURCE_GPU_OBSERVATION_UNAVAILABLE")]
    ObservationUnavailable,
    #[error("LW_RESOURCE_GPU_OBSERVATION_PERMISSION_DENIED")]
    ObservationPermissionDenied,
    #[error("LW_RESOURCE_GPU_OBSERVATION_RESPONSE_INVALID")]
    ObservationReadback,
    #[error("LW_RESOURCE_GPU_OBSERVATION_BOUND_EXCEEDED")]
    ObservationBoundExceeded,
}

impl CapacityProviderError {
    const fn diagnostic(&self) -> &'static str {
        match self {
            Self::Configuration => "LW_RESOURCE_CAPACITY_CONFIGURATION_INVALID",
            Self::IdentityMismatch => "LW_RESOURCE_CAPACITY_IDENTITY_MISMATCH",
            Self::Readback => "LW_RESOURCE_CAPACITY_READBACK_INVALID",
            Self::Unavailable => "LW_RESOURCE_CAPACITY_UNAVAILABLE",
            Self::HandoffFence => "LW_RESOURCE_ENVIRONMENT_HANDOFF_FENCE_INVALID",
            Self::HandoffRejected => "LW_RESOURCE_ENVIRONMENT_HANDOFF_REJECTED",
            Self::LeaseSyncRejected => "LW_RESOURCE_ENVIRONMENT_LEASE_SYNC_REJECTED",
            Self::CleanupRejected => "LW_RESOURCE_ENVIRONMENT_CLEANUP_REJECTED",
            Self::TaskOwnerRequired => "LW_RESOURCE_TASK_OWNER_REQUIRED",
            Self::EnvironmentAuthentication => "LW_RESOURCE_ENVIRONMENT_AUTHENTICATION_FAILED",
            Self::ObservationProviderMissing => "LW_RESOURCE_GPU_OBSERVATION_PROVIDER_MISSING",
            Self::ObservationBindingMismatch => "LW_RESOURCE_GPU_OBSERVATION_BINDING_MISMATCH",
            Self::ObservationUnavailable => "LW_RESOURCE_GPU_OBSERVATION_UNAVAILABLE",
            Self::ObservationPermissionDenied => "LW_RESOURCE_GPU_OBSERVATION_PERMISSION_DENIED",
            Self::ObservationReadback => "LW_RESOURCE_GPU_OBSERVATION_RESPONSE_INVALID",
            Self::ObservationBoundExceeded => "LW_RESOURCE_GPU_OBSERVATION_BOUND_EXCEEDED",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn node(name: &str, ready: bool, unschedulable: bool, units: &Value) -> Value {
        json!({
            "metadata": {"name": name},
            "spec": {"unschedulable": unschedulable},
            "status": {
                "conditions": [{"type": "Ready", "status": if ready { "True" } else { "False" }}],
                "allocatable": {"nvidia.com/gpu": units}
            }
        })
    }

    #[test]
    fn eligible_node_requires_ready_schedulable_and_integer_allocatable()
    -> Result<(), Box<dyn std::error::Error>> {
        assert_eq!(
            eligible_node(&node("gpu-a", true, false, &json!("4")), "nvidia.com/gpu")?,
            Some(EligibleNode {
                name: "gpu-a".to_owned(),
                allocatable_units: 4,
            })
        );
        assert_eq!(
            eligible_node(&node("gpu-b", false, false, &json!("4")), "nvidia.com/gpu")?,
            None
        );
        assert_eq!(
            eligible_node(&node("gpu-c", true, true, &json!("4")), "nvidia.com/gpu")?,
            None
        );
        assert!(
            eligible_node(
                &node("gpu-d", true, false, &json!("four")),
                "nvidia.com/gpu"
            )
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn pod_gpu_usage_sums_regular_and_maxes_init_containers()
    -> Result<(), Box<dyn std::error::Error>> {
        let pod = json!({
            "metadata": {"name": "external"},
            "status": {"phase": "Running"},
            "spec": {
                "nodeName": "gpu-a",
                "containers": [
                    {"resources": {"requests": {"nvidia.com/gpu": "1"}, "limits": {"nvidia.com/gpu": "2"}}},
                    {"resources": {"requests": {"nvidia.com/gpu": "1"}}}
                ],
                "initContainers": [
                    {"resources": {"requests": {"nvidia.com/gpu": "4"}}},
                    {"resources": {"requests": {"nvidia.com/gpu": "3"}}}
                ]
            }
        });
        assert_eq!(pod_node_name(&pod)?, Some("gpu-a".to_owned()));
        assert_eq!(pod_requested_units(&pod, "nvidia.com/gpu")?, 4);

        let mut terminal = pod;
        terminal["status"]["phase"] = json!("Succeeded");
        assert_eq!(pod_requested_units(&terminal, "nvidia.com/gpu")?, 0);
        Ok(())
    }

    #[test]
    fn pod_labels_and_phases_fail_closed() -> Result<(), Box<dyn std::error::Error>> {
        let environment_id = contracts::EnvironmentId::new();
        let reservation = ActiveGpuReservation {
            claim_id: contracts::CapacityClaimId::new(),
            entry_id: contracts::GpuCatalogEntryId::new(),
            units: 1,
            allocation_binding: "nvidia.com/gpu".to_owned(),
            namespace_name: Some("lw-env".to_owned()),
            target: ResourceTarget::Environment {
                environment_id,
                release_id: contracts::ReleaseId::new(),
                release_version: 1,
            },
        };
        let mut pod = json!({
            "metadata": {
                "name": "owned",
                "namespace": "lw-env",
                "labels": {"labweaver.io/environment-id": environment_id.to_string()}
            },
            "status": {"phase": "Pending"},
            "spec": {
                "nodeName": "gpu-a",
                "containers": [{"resources": {"requests": {"nvidia.com/gpu": "1"}}}]
            }
        });
        assert!(pod_is_resource_owned(
            &pod,
            std::slice::from_ref(&reservation),
            "nvidia.com/gpu",
        )?);
        pod["metadata"]["labels"] = json!({});
        assert!(!pod_is_resource_owned(
            &pod,
            std::slice::from_ref(&reservation),
            "nvidia.com/gpu",
        )?);
        pod["metadata"]["labels"] = json!({"labweaver.io/environment-id": true});
        assert!(
            pod_is_resource_owned(&pod, std::slice::from_ref(&reservation), "nvidia.com/gpu",)
                .is_err()
        );
        pod["metadata"]["labels"] =
            json!({"labweaver.io/environment-id": environment_id.to_string()});
        pod["status"]["phase"] = json!("Unexpected");
        assert!(pod_requested_units(&pod, "nvidia.com/gpu").is_err());
        Ok(())
    }

    #[test]
    fn task_pod_is_owned_only_by_exact_task_identity_and_allocation()
    -> Result<(), Box<dyn std::error::Error>> {
        let task_run_id = contracts::TaskRunId::new();
        let reservation = ActiveGpuReservation {
            claim_id: contracts::CapacityClaimId::new(),
            entry_id: contracts::GpuCatalogEntryId::new(),
            units: 2,
            allocation_binding: "nvidia.com/gpu".to_owned(),
            namespace_name: Some("labweaver-evaluation-runs".to_owned()),
            target: ResourceTarget::Task { task_run_id },
        };
        let pod = json!({
            "metadata": {
                "name": "task",
                "namespace": "labweaver-evaluation-runs",
                "labels": {"labweaver.io/task-run-id": task_run_id.to_string()}
            },
            "status": {"phase": "Running"},
            "spec": {
                "nodeName": "gpu-a",
                "containers": [{"resources": {"requests": {"nvidia.com/gpu": "2"}}}]
            }
        });
        assert!(pod_is_resource_owned(
            &pod,
            std::slice::from_ref(&reservation),
            "nvidia.com/gpu",
        )?);
        let mut historical_entry = reservation.clone();
        historical_entry.entry_id = contracts::GpuCatalogEntryId::new();
        assert!(pod_is_resource_owned(
            &pod,
            std::slice::from_ref(&historical_entry),
            "nvidia.com/gpu",
        )?);

        let mut wrong_units = pod.clone();
        wrong_units["spec"]["containers"][0]["resources"]["requests"]["nvidia.com/gpu"] =
            json!("1");
        assert!(!pod_is_resource_owned(
            &wrong_units,
            std::slice::from_ref(&reservation),
            "nvidia.com/gpu",
        )?);

        let mut wrong_task = pod;
        wrong_task["metadata"]["labels"]["labweaver.io/task-run-id"] =
            json!(contracts::TaskRunId::new().to_string());
        assert!(!pod_is_resource_owned(
            &wrong_task,
            std::slice::from_ref(&reservation),
            "nvidia.com/gpu",
        )?);
        Ok(())
    }

    #[test]
    fn fixed_gpu_quantity_is_bounded() -> Result<(), Box<dyn std::error::Error>> {
        assert_eq!(parse_gpu_quantity(&json!("0"))?, 0);
        assert_eq!(parse_gpu_quantity(&json!("12"))?, 12);
        assert!(parse_gpu_quantity(&json!("1Gi")).is_err());
        assert!(parse_gpu_quantity(&json!(-1)).is_err());
        Ok(())
    }

    #[test]
    fn projected_service_account_token_is_reloaded_after_rotation()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = std::env::temp_dir().join(format!(
            "labweaver-gpu-observer-token-{}",
            uuid::Uuid::now_v7()
        ));
        std::fs::write(&path, "token-one")?;
        assert_eq!(read_bearer_token(&path)?, "token-one");
        std::fs::write(&path, "token-two")?;
        assert_eq!(read_bearer_token(&path)?, "token-two");
        std::fs::remove_file(path)?;
        Ok(())
    }

    #[test]
    fn cleanup_fence_requires_a_synced_revision_and_exact_owner_readback()
    -> Result<(), Box<dyn std::error::Error>> {
        let synced = contracts::Revision::new(3)?;
        let current = contracts::Revision::new(4)?;
        assert_eq!(cleanup_lease_revision(Some(synced), current)?, synced);
        assert!(cleanup_lease_revision(None, current).is_err());
        assert!(cleanup_lease_revision(Some(contracts::Revision::new(5)?), current).is_err());
        assert!(cleanup_readback_matches(Some(synced), current, synced));
        assert!(!cleanup_readback_matches(
            Some(synced),
            current,
            contracts::Revision::new(2)?,
        ));
        Ok(())
    }
}
