//! Fail-closed Container release projection and protected Kubernetes resource planning.

use std::str::FromStr;
use std::sync::Arc;

use async_trait::async_trait;
use contracts::authoring::{
    EnvironmentRuntimeSpec, NetworkPolicySpec, RootFilesystemPolicy, RuntimeKind,
};
use contracts::environment::{
    EndpointHealth, EnvironmentEndpoint, EnvironmentInstance, ObservedEnvironmentState,
};
use contracts::events::{CloudEvent, EVENT_CONTRACTS, ReleasePublishedV2, subjects};
use contracts::supply_chain::ImageArtifact;
use contracts::{ArtifactRef, EndpointId, ReleaseId, Revision, Sha256Digest, UtcTimestamp};
use persistence_sqlx::{Domain, InboxDecision, InboxStore};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{PgPool, Row};

use crate::{
    EnvironmentProvider, ProviderFailure, ProviderFailureCode, ProviderObservation, ReconcileAction,
};

/// One server-side-apply document. The name is deterministic and never user-controlled.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContainerResource {
    pub kind: String,
    pub namespace: Option<String>,
    pub name: String,
    pub document: Value,
}

/// Complete least-privilege Kubernetes projection for one immutable release.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContainerResourcePlan {
    pub environment_id: contracts::EnvironmentId,
    pub namespace: String,
    pub image: String,
    pub resources: Vec<ContainerResource>,
    pub plan_sha256: Sha256Digest,
}

/// Sanitized backend observation; raw Kubernetes objects are never persisted.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContainerApplyObservation {
    pub ready: bool,
    pub observed_at: UtcTimestamp,
}

/// Exact backend seam for Kubernetes server-side apply, observation, and cleanup.
#[async_trait]
pub trait ContainerProviderBackend: Send + Sync {
    async fn apply(
        &self,
        plan: &ContainerResourcePlan,
    ) -> Result<ContainerApplyObservation, ProviderFailure>;

    async fn observe(
        &self,
        plan: &ContainerResourcePlan,
    ) -> Result<ContainerApplyObservation, ProviderFailure>;

    async fn scale(
        &self,
        plan: &ContainerResourcePlan,
        replicas: u32,
    ) -> Result<ContainerApplyObservation, ProviderFailure>;

    async fn restart(
        &self,
        plan: &ContainerResourcePlan,
        operation_revision: Revision,
    ) -> Result<ContainerApplyObservation, ProviderFailure>;

    async fn delete_namespace(
        &self,
        plan: &ContainerResourcePlan,
    ) -> Result<ArtifactRef, ProviderFailure>;
}

/// Typed NATS backend for the deployment-owned Kubernetes apply/observe executor.
pub struct NatsContainerProviderBackend {
    client: async_nats::Client,
    subject: String,
}

impl NatsContainerProviderBackend {
    pub fn new(
        client: async_nats::Client,
        subject: String,
    ) -> Result<Self, ReleaseProjectionError> {
        if !valid_subject(&subject) {
            return Err(ReleaseProjectionError::ConfigurationInvalid);
        }
        Ok(Self { client, subject })
    }

    async fn request(
        &self,
        request: ContainerBackendRequest<'_>,
    ) -> Result<ContainerBackendResponse, ProviderFailure> {
        let payload = serde_json::to_vec(&request).map_err(|_| invalid_observation())?;
        let message = self
            .client
            .request(self.subject.clone(), payload.into())
            .await
            .map_err(|_| unavailable())?;
        if message.payload.len() > 1024 * 1024 {
            return Err(invalid_observation());
        }
        serde_json::from_slice(&message.payload).map_err(|_| invalid_observation())
    }
}

#[async_trait]
impl ContainerProviderBackend for NatsContainerProviderBackend {
    async fn apply(
        &self,
        plan: &ContainerResourcePlan,
    ) -> Result<ContainerApplyObservation, ProviderFailure> {
        match self
            .request(ContainerBackendRequest::Apply { plan })
            .await?
        {
            ContainerBackendResponse::Observed {
                environment_id,
                plan_sha256,
                observation,
            } if environment_id == plan.environment_id && plan_sha256 == plan.plan_sha256 => {
                Ok(observation)
            }
            ContainerBackendResponse::Failed { failure } => Err(failure),
            _ => Err(invalid_observation()),
        }
    }

    async fn observe(
        &self,
        plan: &ContainerResourcePlan,
    ) -> Result<ContainerApplyObservation, ProviderFailure> {
        match self
            .request(ContainerBackendRequest::Observe { plan })
            .await?
        {
            ContainerBackendResponse::Observed {
                environment_id,
                plan_sha256,
                observation,
            } if environment_id == plan.environment_id && plan_sha256 == plan.plan_sha256 => {
                Ok(observation)
            }
            ContainerBackendResponse::Failed { failure } => Err(failure),
            _ => Err(invalid_observation()),
        }
    }

    async fn scale(
        &self,
        plan: &ContainerResourcePlan,
        replicas: u32,
    ) -> Result<ContainerApplyObservation, ProviderFailure> {
        match self
            .request(ContainerBackendRequest::Scale { plan, replicas })
            .await?
        {
            ContainerBackendResponse::Observed {
                environment_id,
                plan_sha256,
                observation,
            } if environment_id == plan.environment_id && plan_sha256 == plan.plan_sha256 => {
                Ok(observation)
            }
            ContainerBackendResponse::Failed { failure } => Err(failure),
            _ => Err(invalid_observation()),
        }
    }

    async fn restart(
        &self,
        plan: &ContainerResourcePlan,
        operation_revision: Revision,
    ) -> Result<ContainerApplyObservation, ProviderFailure> {
        match self
            .request(ContainerBackendRequest::Restart {
                plan,
                operation_revision,
            })
            .await?
        {
            ContainerBackendResponse::Observed {
                environment_id,
                plan_sha256,
                observation,
            } if environment_id == plan.environment_id && plan_sha256 == plan.plan_sha256 => {
                Ok(observation)
            }
            ContainerBackendResponse::Failed { failure } => Err(failure),
            _ => Err(invalid_observation()),
        }
    }

    async fn delete_namespace(
        &self,
        plan: &ContainerResourcePlan,
    ) -> Result<ArtifactRef, ProviderFailure> {
        match self
            .request(ContainerBackendRequest::DeleteNamespace { plan })
            .await?
        {
            ContainerBackendResponse::Deleted {
                environment_id,
                plan_sha256,
                cleanup_evidence,
            } if environment_id == plan.environment_id
                && plan_sha256 == plan.plan_sha256
                && valid_artifact_ref(&cleanup_evidence) =>
            {
                Ok(cleanup_evidence)
            }
            ContainerBackendResponse::Failed { failure } => Err(failure),
            _ => Err(ProviderFailure {
                code: ProviderFailureCode::CleanupFailed,
                retryable: true,
            }),
        }
    }
}

#[derive(Serialize)]
#[serde(
    tag = "action",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
enum ContainerBackendRequest<'a> {
    Apply {
        plan: &'a ContainerResourcePlan,
    },
    Observe {
        plan: &'a ContainerResourcePlan,
    },
    Scale {
        plan: &'a ContainerResourcePlan,
        replicas: u32,
    },
    Restart {
        plan: &'a ContainerResourcePlan,
        operation_revision: Revision,
    },
    DeleteNamespace {
        plan: &'a ContainerResourcePlan,
    },
}

#[derive(Deserialize)]
#[serde(
    tag = "status",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
enum ContainerBackendResponse {
    Observed {
        environment_id: contracts::EnvironmentId,
        plan_sha256: Sha256Digest,
        observation: ContainerApplyObservation,
    },
    Deleted {
        environment_id: contracts::EnvironmentId,
        plan_sha256: Sha256Digest,
        cleanup_evidence: ArtifactRef,
    },
    Failed {
        failure: ProviderFailure,
    },
}

/// Exact immutable Release lookup used by a Provider action.
#[async_trait]
pub trait ContainerReleaseResolver: Send + Sync {
    async fn resolve(
        &self,
        release_id: ReleaseId,
        release_version: u64,
    ) -> Result<ReleasePublishedV2, ReleaseProjectionError>;
}

/// Durable projection result for a release `CloudEvent`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReleaseProjectionDecision {
    Applied,
    Duplicate,
    Stale,
    Gap,
}

/// Environment-owned immutable release projection.
#[derive(Clone)]
pub struct PgReleaseProjectionStore {
    pool: PgPool,
}

impl PgReleaseProjectionStore {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn accept(
        &self,
        consumer: &str,
        event: &CloudEvent<ReleasePublishedV2>,
    ) -> Result<ReleaseProjectionDecision, ReleaseProjectionError> {
        let contract = EVENT_CONTRACTS
            .iter()
            .copied()
            .find(|contract| {
                contract.subject == subjects::ENVIRONMENT_TEMPLATE_RELEASE_PUBLISHED_V2
            })
            .ok_or(ReleaseProjectionError::ContractInvalid)?;
        event
            .validate(contract)
            .map_err(|_| ReleaseProjectionError::ContractInvalid)?;
        event
            .data
            .validate()
            .map_err(|_| ReleaseProjectionError::ContractInvalid)?;
        if event.subject != subjects::ENVIRONMENT_TEMPLATE_RELEASE_PUBLISHED_V2
            || event.course_id != event.data.release.course_id
            || event.aggregate_revision
                != Revision::new(event.data.release.version)
                    .map_err(|_| ReleaseProjectionError::ContractInvalid)?
        {
            return Err(ReleaseProjectionError::IdentityMismatch);
        }
        let provider_binding = container_provider_binding(&event.data)?;
        let payload =
            serde_json::to_value(event).map_err(|_| ReleaseProjectionError::ContractInvalid)?;
        let payload_sha256 = canonical_hash(&payload)?;
        let mut transaction = self.pool.begin().await?;
        let decision = InboxStore::accept(
            &mut transaction,
            Domain::Environment,
            consumer,
            event.id.as_uuid(),
            event.data.release.id.as_uuid(),
            event.aggregate_sequence.0,
            payload_sha256,
        )
        .await
        .map_err(|_| ReleaseProjectionError::PersistenceFailed)?;
        let outcome = match decision {
            InboxDecision::Accepted => {
                let contract = serde_json::to_value(&event.data)
                    .map_err(|_| ReleaseProjectionError::ContractInvalid)?;
                sqlx::query(
                    "INSERT INTO environment.release_projections \
                     (release_id,course_id,release_version,provider_binding,projection_sha256,contract,projected_event_id) \
                     VALUES ($1,$2,$3,$4,$5,$6,$7)",
                )
                .bind(event.data.release.id.as_uuid())
                .bind(event.course_id.as_uuid())
                .bind(i64::try_from(event.data.release.version).map_err(|_| ReleaseProjectionError::IdentityMismatch)?)
                .bind(provider_binding)
                .bind(event.data.projection_sha256.to_string())
                .bind(contract)
                .bind(event.id.as_uuid())
                .execute(&mut *transaction)
                .await
                .map_err(|error| {
                    if is_unique_violation(&error) {
                        ReleaseProjectionError::IdentityMismatch
                    } else {
                        ReleaseProjectionError::Database(error)
                    }
                })?;
                ReleaseProjectionDecision::Applied
            }
            InboxDecision::Duplicate => ReleaseProjectionDecision::Duplicate,
            InboxDecision::Stale => ReleaseProjectionDecision::Stale,
            InboxDecision::Gap => {
                transaction.rollback().await?;
                return Ok(ReleaseProjectionDecision::Gap);
            }
        };
        transaction.commit().await?;
        Ok(outcome)
    }
}

#[async_trait]
impl ContainerReleaseResolver for PgReleaseProjectionStore {
    async fn resolve(
        &self,
        release_id: ReleaseId,
        release_version: u64,
    ) -> Result<ReleasePublishedV2, ReleaseProjectionError> {
        let row = sqlx::query(
            "SELECT release_version,contract,projection_sha256 FROM environment.release_projections \
             WHERE release_id=$1",
        )
        .bind(release_id.as_uuid())
        .fetch_optional(&self.pool)
        .await?
        .ok_or(ReleaseProjectionError::NotFound)?;
        let stored_version: i64 = row.try_get("release_version")?;
        if u64::try_from(stored_version).ok() != Some(release_version) {
            return Err(ReleaseProjectionError::IdentityMismatch);
        }
        let projection: ReleasePublishedV2 = serde_json::from_value(row.try_get("contract")?)
            .map_err(|_| ReleaseProjectionError::ContractInvalid)?;
        projection
            .validate()
            .map_err(|_| ReleaseProjectionError::ContractInvalid)?;
        let stored_sha256: String = row.try_get("projection_sha256")?;
        if projection.release.id != release_id
            || projection.release.version != release_version
            || projection.projection_sha256.to_string() != stored_sha256
        {
            return Err(ReleaseProjectionError::IdentityMismatch);
        }
        Ok(projection)
    }
}

/// Container implementation of the existing Environment Provider state machine.
pub struct ContainerProvider<B, R> {
    binding: String,
    backend: Arc<B>,
    releases: Arc<R>,
    gateway_namespace: String,
    gateway_name: String,
    gateway_section: String,
    image_pull_secret_name: String,
}

impl<B, R> ContainerProvider<B, R>
where
    B: ContainerProviderBackend,
    R: ContainerReleaseResolver,
{
    pub fn new(
        binding: String,
        backend: Arc<B>,
        releases: Arc<R>,
        gateway_namespace: String,
        gateway_name: String,
        gateway_section: String,
        image_pull_secret_name: String,
    ) -> Result<Self, ReleaseProjectionError> {
        if !valid_binding(&binding)
            || !valid_dns_label(&gateway_namespace)
            || !valid_dns_label(&gateway_name)
            || !valid_dns_label(&gateway_section)
            || !valid_dns_label(&image_pull_secret_name)
        {
            return Err(ReleaseProjectionError::ConfigurationInvalid);
        }
        Ok(Self {
            binding,
            backend,
            releases,
            gateway_namespace,
            gateway_name,
            gateway_section,
            image_pull_secret_name,
        })
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the complete deterministic Kubernetes security bundle is reviewed as one projection"
    )]
    pub fn plan(
        &self,
        instance: &EnvironmentInstance,
        projection: &ReleasePublishedV2,
    ) -> Result<ContainerResourcePlan, ReleaseProjectionError> {
        projection
            .validate()
            .map_err(|_| ReleaseProjectionError::ContractInvalid)?;
        if instance.runtime_kind != RuntimeKind::Container
            || instance.release_id != projection.release.id
            || instance.release_version != projection.release.version
            || instance.course_id != projection.release.course_id
            || instance.provider_binding != self.binding
            || container_provider_binding(projection)? != self.binding
        {
            return Err(ReleaseProjectionError::IdentityMismatch);
        }
        let ImageArtifact::Container {
            repository, digest, ..
        } = &projection.release.artifact
        else {
            return Err(ReleaseProjectionError::IdentityMismatch);
        };
        if !digest.starts_with("sha256:")
            || repository.contains('@')
            || repository.contains(char::is_whitespace)
        {
            return Err(ReleaseProjectionError::ContractInvalid);
        }
        let mut repository_parts = repository.split('/');
        let registry = repository_parts.next().unwrap_or_default();
        let project = repository_parts.next().unwrap_or_default();
        let image_name = repository_parts.next().unwrap_or_default();
        if registry.is_empty()
            || project != format!("course-{}", projection.release.course_id)
            || image_name != projection.release.candidate_id.to_string()
            || repository_parts.next().is_some()
        {
            return Err(ReleaseProjectionError::IdentityMismatch);
        }
        let image = format!("{repository}@{digest}");
        let namespace = format!("lw-env-{}", instance.id);
        let app_name = "runtime";
        let labels = json!({
            "app.kubernetes.io/name": "labweaver-environment",
            "labweaver.io/environment-id": instance.id.to_string(),
            "labweaver.io/course-id": instance.course_id.to_string(),
            "labweaver.io/managed": "true",
        });
        let resources = &projection.environment_spec.resources;
        let service_port = match &projection.environment_spec.runtime {
            EnvironmentRuntimeSpec::Container { service_port, .. } => *service_port,
            EnvironmentRuntimeSpec::VirtualMachine { .. } => {
                return Err(ReleaseProjectionError::IdentityMismatch);
            }
        };
        if !projection.environment_spec.entries.iter().any(|entry| {
            entry.service_port == service_port
                && entry.protocol == contracts::environment::EndpointProtocol::Https
        }) {
            return Err(ReleaseProjectionError::SecurityPostureInvalid);
        }
        if projection.environment_spec.security.root_filesystem_policy
            != RootFilesystemPolicy::ReadOnlyRequired
        {
            return Err(ReleaseProjectionError::SecurityPostureInvalid);
        }
        let cpu = format!("{}m", resources.cpu_millicores);
        let memory = resources.memory_bytes.to_string();
        let storage = resources.storage_bytes.to_string();
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
                    "metadata":{"name":"runtime-quota","namespace":namespace,"labels":labels},
                    "spec":{"hard":{"requests.cpu":cpu,"limits.cpu":cpu,"requests.memory":memory,"limits.memory":memory,"requests.storage":storage,"persistentvolumeclaims":"1","pods":"1"}}
                }),
            ),
            resource(
                "LimitRange",
                Some(&namespace),
                "runtime-limits",
                json!({
                    "apiVersion":"v1","kind":"LimitRange",
                    "metadata":{"name":"runtime-limits","namespace":namespace,"labels":labels},
                    "spec":{"limits":[{"type":"Container","default":{"cpu":cpu,"memory":memory},"defaultRequest":{"cpu":cpu,"memory":memory}}]}
                }),
            ),
            resource(
                "ServiceAccount",
                Some(&namespace),
                "runtime",
                json!({
                    "apiVersion":"v1","kind":"ServiceAccount",
                    "metadata":{"name":"runtime","namespace":namespace,"labels":labels},
                    "automountServiceAccountToken":false,
                    "imagePullSecrets":[{"name":self.image_pull_secret_name}]
                }),
            ),
            resource(
                "PersistentVolumeClaim",
                Some(&namespace),
                "workspace",
                json!({
                    "apiVersion":"v1","kind":"PersistentVolumeClaim",
                    "metadata":{"name":"workspace","namespace":namespace,"labels":labels},
                    "spec":{"accessModes":["ReadWriteOnce"],"resources":{"requests":{"storage":storage}}}
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
                "protected-gateway-ingress",
                json!({
                    "apiVersion":"networking.k8s.io/v1","kind":"NetworkPolicy",
                    "metadata":{"name":"protected-gateway-ingress","namespace":namespace,"labels":labels},
                    "spec":{"podSelector":{"matchLabels":{"app":app_name}},"policyTypes":["Ingress"],"ingress":[{"from":[{"namespaceSelector":{"matchLabels":{"kubernetes.io/metadata.name":self.gateway_namespace}},"podSelector":{"matchLabels":{"gateway.networking.k8s.io/gateway-name":self.gateway_name}}}],"ports":[{"protocol":"TCP","port":service_port}]}]}
                }),
            ),
            resource(
                "Deployment",
                Some(&namespace),
                app_name,
                json!({
                    "apiVersion":"apps/v1","kind":"Deployment",
                    "metadata":{"name":app_name,"namespace":namespace,"labels":labels},
                    "spec":{
                        "replicas":1,
                        "selector":{"matchLabels":{"app":app_name}},
                        "template":{
                            "metadata":{"labels":{"app":app_name,"labweaver.io/environment-id":instance.id.to_string()}},
                            "spec":{
                                "serviceAccountName":"runtime","automountServiceAccountToken":false,
                                "securityContext":{"runAsNonRoot":true,"seccompProfile":{"type":"RuntimeDefault"}},
                                "containers":[{
                                    "name":"runtime","image":image,"imagePullPolicy":"IfNotPresent",
                                    "ports":[{"name":"service","containerPort":service_port}],
                                    "resources":{"requests":{"cpu":cpu,"memory":memory},"limits":{"cpu":cpu,"memory":memory}},
                                    "securityContext":{"allowPrivilegeEscalation":false,"readOnlyRootFilesystem":true,"runAsNonRoot":true,"capabilities":{"drop":["ALL"]}},
                                    "volumeMounts":[{"name":"workspace","mountPath":"/workspace"}],
                                    "readinessProbe":{"tcpSocket":{"port":"service"},"periodSeconds":2,"failureThreshold":30}
                                }],
                                "volumes":[{"name":"workspace","persistentVolumeClaim":{"claimName":"workspace"}}]
                            }
                        }
                    }
                }),
            ),
            resource(
                "Service",
                Some(&namespace),
                app_name,
                json!({
                    "apiVersion":"v1","kind":"Service",
                    "metadata":{"name":app_name,"namespace":namespace,"labels":labels},
                    "spec":{"type":"ClusterIP","selector":{"app":app_name},"ports":[{"name":"service","port":service_port,"targetPort":"service"}]}
                }),
            ),
            resource(
                "HTTPRoute",
                Some(&namespace),
                "protected",
                json!({
                    "apiVersion":"gateway.networking.k8s.io/v1","kind":"HTTPRoute",
                    "metadata":{"name":"protected","namespace":namespace,"labels":labels,"annotations":{"labweaver.io/access-controlled":"true"}},
                    "spec":{"parentRefs":[{"group":"gateway.networking.k8s.io","kind":"Gateway","namespace":self.gateway_namespace,"name":self.gateway_name,"sectionName":self.gateway_section}],
                        "rules":[{"matches":[{"path":{"type":"PathPrefix","value":format!("/environments/{}/",instance.id)}}],"filters":[{"type":"URLRewrite","urlRewrite":{"path":{"type":"ReplacePrefixMatch","replacePrefixMatch":"/"}}}],"backendRefs":[{"group":"","kind":"Service","name":app_name,"port":service_port}]}]}
                }),
            ),
        ];
        if let NetworkPolicySpec::Restricted { policy_binding } =
            &projection.environment_spec.network
        {
            documents.push(resource("NetworkPolicy", Some(&namespace), "restricted-egress", json!({
                "apiVersion":"networking.k8s.io/v1","kind":"NetworkPolicy",
                "metadata":{"name":"restricted-egress","namespace":namespace,"labels":labels},
                "spec":{"podSelector":{"matchLabels":{"app":app_name}},"policyTypes":["Egress"],"egress":[{"to":[{"namespaceSelector":{"matchLabels":{"labweaver.io/egress-policy":policy_binding}}}]}]}
            })));
        }
        let plan_sha256 = canonical_hash(&json!({
            "environmentId": instance.id,
            "releaseId": projection.release.id,
            "releaseVersion": projection.release.version,
            "image": image,
            "resources": documents,
        }))?;
        Ok(ContainerResourcePlan {
            environment_id: instance.id,
            namespace,
            image,
            resources: documents,
            plan_sha256,
        })
    }

    fn cleanup_plan(
        &self,
        instance: &EnvironmentInstance,
    ) -> Result<ContainerResourcePlan, ReleaseProjectionError> {
        if instance.runtime_kind != RuntimeKind::Container
            || instance.provider_binding != self.binding
        {
            return Err(ReleaseProjectionError::IdentityMismatch);
        }
        let namespace = format!("lw-env-{}", instance.id);
        let plan_sha256 = canonical_hash(&json!({
            "environmentId": instance.id,
            "namespace": namespace,
            "action": "cleanup",
        }))?;
        Ok(ContainerResourcePlan {
            environment_id: instance.id,
            namespace,
            image: String::new(),
            resources: Vec::new(),
            plan_sha256,
        })
    }
}

#[async_trait]
impl<B, R> EnvironmentProvider for ContainerProvider<B, R>
where
    B: ContainerProviderBackend,
    R: ContainerReleaseResolver,
{
    fn binding(&self) -> &str {
        &self.binding
    }

    async fn execute(
        &self,
        action: ReconcileAction,
        instance: &EnvironmentInstance,
    ) -> Result<ProviderObservation, ProviderFailure> {
        if action == ReconcileAction::Cleanup
            && instance.observed_state == ObservedEnvironmentState::Deleting
        {
            let plan = self
                .cleanup_plan(instance)
                .map_err(|error| projection_failure(&error))?;
            let cleanup_evidence = self.backend.delete_namespace(&plan).await?;
            if !valid_artifact_ref(&cleanup_evidence) {
                return Err(ProviderFailure {
                    code: ProviderFailureCode::CleanupFailed,
                    retryable: true,
                });
            }
            return Ok(ProviderObservation {
                next_state: ObservedEnvironmentState::Deleted,
                endpoints: Vec::new(),
                cleanup_evidence: Some(cleanup_evidence),
                operation_complete: true,
            });
        }
        let projection = self
            .releases
            .resolve(instance.release_id, instance.release_version)
            .await
            .map_err(|error| projection_failure(&error))?;
        let plan = self
            .plan(instance, &projection)
            .map_err(|error| projection_failure(&error))?;
        let no_endpoints = |next_state, operation_complete| ProviderObservation {
            next_state,
            endpoints: Vec::new(),
            cleanup_evidence: None,
            operation_complete,
        };
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
                let observed = self.backend.apply(&plan).await?;
                ready_observation(instance, observed)
            }
            (ReconcileAction::Observe, _) => {
                let observed = self.backend.observe(&plan).await?;
                ready_observation(instance, observed)
            }
            (ReconcileAction::Start, ObservedEnvironmentState::Stopped) => {
                let observed = self.backend.scale(&plan, 1).await?;
                ready_observation(instance, observed)
            }
            (ReconcileAction::Restart, ObservedEnvironmentState::Provisioning) => {
                let observed = self.backend.restart(&plan, instance.revision).await?;
                ready_observation(instance, observed)
            }
            (
                ReconcileAction::Stop,
                ObservedEnvironmentState::Stopping | ObservedEnvironmentState::Expiring,
            ) => {
                self.backend.scale(&plan, 0).await?;
                Ok(no_endpoints(ObservedEnvironmentState::Stopped, true))
            }
            _ => Err(ProviderFailure {
                code: ProviderFailureCode::Rejected,
                retryable: false,
            }),
        }
    }
}

fn ready_observation(
    instance: &EnvironmentInstance,
    observed: ContainerApplyObservation,
) -> Result<ProviderObservation, ProviderFailure> {
    if !observed.ready {
        return Ok(ProviderObservation {
            next_state: ObservedEnvironmentState::Provisioning,
            endpoints: Vec::new(),
            cleanup_evidence: None,
            operation_complete: false,
        });
    }
    let revision = instance
        .revision
        .get()
        .checked_add(1)
        .and_then(|value| Revision::new(value).ok())
        .ok_or(ProviderFailure {
            code: ProviderFailureCode::ObservationInvalid,
            retryable: false,
        })?;
    Ok(ProviderObservation {
        next_state: ObservedEnvironmentState::Ready,
        endpoints: vec![EnvironmentEndpoint {
            id: deterministic_endpoint_id(instance.id)?,
            protocol: contracts::environment::EndpointProtocol::Https,
            revision,
            health: EndpointHealth::Healthy,
            observed_at: observed.observed_at,
        }],
        cleanup_evidence: None,
        operation_complete: true,
    })
}

fn deterministic_endpoint_id(
    environment_id: contracts::EnvironmentId,
) -> Result<EndpointId, ProviderFailure> {
    let mut bytes = *environment_id.as_uuid().as_bytes();
    bytes[15] ^= 1;
    EndpointId::from_str(&uuid::Uuid::from_bytes(bytes).to_string())
        .map_err(|_| invalid_observation())
}

fn resource(kind: &str, namespace: Option<&str>, name: &str, document: Value) -> ContainerResource {
    ContainerResource {
        kind: kind.to_owned(),
        namespace: namespace.map(str::to_owned),
        name: name.to_owned(),
        document,
    }
}

fn container_provider_binding(
    projection: &ReleasePublishedV2,
) -> Result<&str, ReleaseProjectionError> {
    match &projection.environment_spec.runtime {
        EnvironmentRuntimeSpec::Container {
            provider_binding, ..
        } => Ok(provider_binding),
        EnvironmentRuntimeSpec::VirtualMachine { .. } => {
            Err(ReleaseProjectionError::IdentityMismatch)
        }
    }
}

fn projection_failure(error: &ReleaseProjectionError) -> ProviderFailure {
    match error {
        ReleaseProjectionError::Database(_) | ReleaseProjectionError::PersistenceFailed => {
            ProviderFailure {
                code: ProviderFailureCode::Unavailable,
                retryable: true,
            }
        }
        ReleaseProjectionError::NotFound => ProviderFailure {
            code: ProviderFailureCode::Unavailable,
            retryable: true,
        },
        ReleaseProjectionError::ConfigurationInvalid
        | ReleaseProjectionError::ContractInvalid
        | ReleaseProjectionError::IdentityMismatch
        | ReleaseProjectionError::SecurityPostureInvalid => ProviderFailure {
            code: ProviderFailureCode::Rejected,
            retryable: false,
        },
    }
}

fn canonical_hash<T: Serialize>(value: &T) -> Result<Sha256Digest, ReleaseProjectionError> {
    Sha256Digest::of_canonical(value).map_err(|_| ReleaseProjectionError::ContractInvalid)
}

fn valid_binding(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.:".contains(&byte))
}

fn valid_subject(value: &str) -> bool {
    valid_binding(value) && !value.contains('*') && !value.contains('>')
}

fn valid_artifact_ref(artifact: &ArtifactRef) -> bool {
    artifact.size_bytes > 0
        && artifact.sha256 != Sha256Digest::of_bytes(&[])
        && !artifact.store_binding.trim().is_empty()
        && !artifact.object_version.trim().is_empty()
        && !artifact.media_type.trim().is_empty()
        && !artifact
            .store_binding
            .bytes()
            .any(|byte| byte.is_ascii_whitespace())
}

fn valid_dns_label(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 63
        && !value.starts_with('-')
        && !value.ends_with('-')
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn is_unique_violation(error: &sqlx::Error) -> bool {
    error
        .as_database_error()
        .and_then(sqlx::error::DatabaseError::code)
        .is_some_and(|code| code == "23505")
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

#[derive(Debug, thiserror::Error)]
pub enum ReleaseProjectionError {
    #[error("LW_ENVIRONMENT_CONTAINER_CONFIGURATION_INVALID")]
    ConfigurationInvalid,
    #[error("LW_ENVIRONMENT_RELEASE_NOT_FOUND")]
    NotFound,
    #[error("LW_ENVIRONMENT_RELEASE_CONTRACT_INVALID")]
    ContractInvalid,
    #[error("LW_ENVIRONMENT_RELEASE_IDENTITY_MISMATCH")]
    IdentityMismatch,
    #[error("LW_ENVIRONMENT_CONTAINER_SECURITY_POSTURE_INVALID")]
    SecurityPostureInvalid,
    #[error("LW_ENVIRONMENT_RELEASE_PERSISTENCE_FAILED")]
    PersistenceFailed,
    #[error("LW_ENVIRONMENT_RELEASE_DATABASE_FAILED")]
    Database(#[from] sqlx::Error),
}
