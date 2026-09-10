//! Environment-authoritative immutable submission source resolution.

use persistence_sqlx::Sha256Digest;
use std::{path::PathBuf, sync::Arc}; // internal persistence hash, not contract hash

use contracts::{
    EnvironmentId, UtcTimestamp,
    authoring::RuntimeKind,
    environment::{
        DesiredEnvironmentState, EndpointHealth, EnvironmentExecutionBinding,
        EnvironmentExecutionPurpose, EnvironmentExecutionSourceBinding, ObservedEnvironmentState,
    },
    submission::{
        EnvironmentFreezeBinding, EnvironmentFreezeBindingRequest, EnvironmentFreezeSourceBinding,
        FrozenEnvironmentIdentity,
    },
    supply_chain::ImageArtifact,
};
use rand::RngCore as _;
use russh::keys::{
    load_secret_key,
    ssh_key::{PrivateKey, PublicKey, certificate},
};
use sqlx::{PgPool, Row};

use crate::{
    ContainerReleaseResolver, EnvironmentStoreError, PgEnvironmentStore, PgReleaseProjectionStore,
    ReleaseProjectionError, WorkAdmissionClient, WorkAdmissionClientError, WorkAdmissionResolver,
};

const CERTIFICATE_TTL_SECONDS: i64 = 299;

/// Reviewed deployment values required by the enabled freeze transports.
#[derive(Clone, Debug)]
pub struct FreezeBindingConfiguration {
    pub container_workspace_storage_class: String,
    pub vm: Option<VmFreezeBindingConfiguration>,
}

/// Reviewed deployment values for the optional `KubeVirt` freeze transport.
#[derive(Clone, Debug)]
pub struct VmFreezeBindingConfiguration {
    pub username: String,
    pub workspace_root: String,
    pub ssh_user_ca_public_key: String,
    pub ssh_user_ca_private_key_path: PathBuf,
}

const EVALUATION_EXECUTION_PRINCIPAL: &str = "labweaver-evaluation";
const AGENT_EXECUTION_PRINCIPAL: &str = "labweaver-agent";

/// Resolves current runtime state and issues VM collector credentials.
#[derive(Clone)]
pub struct FreezeBindingService {
    pool: PgPool,
    store: PgEnvironmentStore,
    releases: PgReleaseProjectionStore,
    configuration: FreezeBindingConfiguration,
    ssh_user_ca: Option<Arc<PrivateKey>>,
    work_admission: Arc<dyn WorkAdmissionResolver>,
}

impl FreezeBindingService {
    pub fn new(
        pool: PgPool,
        releases: PgReleaseProjectionStore,
        configuration: FreezeBindingConfiguration,
        work_admission: WorkAdmissionClient,
    ) -> Result<Self, FreezeBindingError> {
        Self::new_with_admission_resolver(pool, releases, configuration, work_admission)
    }

    pub fn new_with_admission_resolver<A>(
        pool: PgPool,
        releases: PgReleaseProjectionStore,
        configuration: FreezeBindingConfiguration,
        work_admission: A,
    ) -> Result<Self, FreezeBindingError>
    where
        A: WorkAdmissionResolver + 'static,
    {
        if configuration
            .container_workspace_storage_class
            .trim()
            .is_empty()
        {
            return Err(FreezeBindingError::ConfigurationInvalid);
        }

        let ssh_user_ca = configuration
            .vm
            .as_ref()
            .map(|vm| {
                if vm.username.trim().is_empty()
                    || !vm.username.bytes().all(|byte| {
                        byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'
                    })
                    || !safe_absolute_workspace(&vm.workspace_root)
                    || !vm.ssh_user_ca_private_key_path.is_absolute()
                {
                    return Err(FreezeBindingError::ConfigurationInvalid);
                }
                let ssh_user_ca = load_secret_key(&vm.ssh_user_ca_private_key_path, None)
                    .map_err(|_| FreezeBindingError::ConfigurationInvalid)?;
                let configured_public_key = PublicKey::from_openssh(&vm.ssh_user_ca_public_key)
                    .map_err(|_| FreezeBindingError::ConfigurationInvalid)?;
                if ssh_user_ca.public_key() != &configured_public_key {
                    return Err(FreezeBindingError::ConfigurationInvalid);
                }
                Ok(Arc::new(ssh_user_ca))
            })
            .transpose()?;
        Ok(Self {
            store: PgEnvironmentStore::new(pool.clone()),
            pool,
            releases,
            configuration,
            ssh_user_ca,
            work_admission: Arc::new(work_admission),
        })
    }

    pub async fn resolve(
        &self,
        environment_id: EnvironmentId,
        request: &EnvironmentFreezeBindingRequest,
    ) -> Result<EnvironmentFreezeBinding, FreezeBindingError> {
        let now = self.store.current_time().await?;
        let instance = self.store.load(environment_id).await?;
        if instance.project_id != request.project_id
            || instance.course_id != request.course_id
            || instance.owner_id != request.actor_id
            || instance.revision != request.expected_revision
            || instance.desired_state != DesiredEnvironmentState::Running
            || instance.observed_state != ObservedEnvironmentState::Ready
            || instance.observed_generation != instance.generation
            || instance.eligibility_expires_at <= now
            || !instance
                .endpoints
                .iter()
                .any(|endpoint| endpoint.health == EndpointHealth::Healthy)
        {
            return Err(FreezeBindingError::EnvironmentNotEligible);
        }
        let release = self
            .releases
            .resolve(instance.release_id, instance.release_version)
            .await?;
        if release.withdrawn_at.is_some()
            || release.projection.release.project_id != instance.project_id
            || release.projection.release.course_id != instance.course_id
            || release.projection.release.runtime_kind != instance.runtime_kind
        {
            return Err(FreezeBindingError::ReleaseIdentityMismatch);
        }
        let artifact = &release.projection.release.artifact;
        let environment = FrozenEnvironmentIdentity {
            environment_id: instance.id,
            environment_revision: instance.revision,
            release_id: instance.release_id,
            release_version: instance.release_version,
            runtime_kind: instance.runtime_kind,
            build_request_id: match artifact {
                ImageArtifact::Container {
                    build_request_id, ..
                } => Some(*build_request_id),
                ImageArtifact::VirtualMachine { .. } => None,
            },
        };
        let source = match instance.runtime_kind {
            RuntimeKind::Container => EnvironmentFreezeSourceBinding::Container {
                namespace: format!("lw-env-{}", instance.id),
                persistent_volume_claim: "workspace".to_owned(),
                storage_class_name: self.configuration.container_workspace_storage_class.clone(),
            },
            RuntimeKind::VirtualMachine => {
                self.vm_configuration()?;
                let public_key = request
                    .collector_public_key_openssh
                    .as_deref()
                    .ok_or(FreezeBindingError::CollectorKeyRequired)?;
                self.resolve_vm(instance.id, instance.generation, public_key, now)
                    .await?
            }
        };
        Ok(EnvironmentFreezeBinding {
            environment,
            agent_run_id: release.projection.release.agent_run_id,
            source,
        })
    }

    /// Resolves a fresh VM credential for one Evaluation or Agent execution.
    ///
    /// The target and frozen identity are resolved from the current Environment
    /// projection on every call. The issued user certificate has only the
    /// purpose-specific principal; it deliberately carries no force-command or
    /// collector restrictions because the caller runs an explicit execution
    /// protocol inside its own isolated Job.
    #[allow(clippy::too_many_lines)]
    pub async fn resolve_execution(
        &self,
        environment_id: EnvironmentId,
        request: &contracts::environment::EnvironmentExecutionBindingRequest,
    ) -> Result<EnvironmentExecutionBinding, FreezeBindingError> {
        request
            .validate()
            .map_err(|_| FreezeBindingError::ExecutionBindingInvalid)?;
        self.vm_configuration()?;
        let (expected_class, principal, purpose_key, work_run) = match &request.purpose {
            EnvironmentExecutionPurpose::EvaluationProbe {
                run_id,
                step_run_id,
                attempt,
            } => (
                contracts::authoring::EnvironmentClass::Experiment,
                EVALUATION_EXECUTION_PRINCIPAL,
                format!("evaluation:{run_id}:{step_run_id}:{attempt}"),
                None,
            ),
            EnvironmentExecutionPurpose::WorkConfiguration {
                agent_run_id,
                run_revision,
            } => (
                contracts::authoring::EnvironmentClass::Work,
                AGENT_EXECUTION_PRINCIPAL,
                format!("agent:{agent_run_id}:{}", run_revision.get()),
                Some((*agent_run_id, *run_revision, None, None)),
            ),
            EnvironmentExecutionPurpose::WorkConfigurationRecovery {
                agent_run_id,
                run_revision,
                execution_id,
                plan_id,
                plan_revision,
                source_identity,
            } => (
                contracts::authoring::EnvironmentClass::Work,
                AGENT_EXECUTION_PRINCIPAL,
                format!(
                    "agent-recovery:{agent_run_id}:{}:{execution_id}:{plan_id}:{}:{source_identity}",
                    run_revision.get(),
                    plan_revision.get()
                ),
                Some((
                    *agent_run_id,
                    *run_revision,
                    Some(*execution_id),
                    Some((*plan_id, *plan_revision, source_identity.as_str())),
                )),
            ),
        };

        // Read the admission against an authority timestamp, then refresh it immediately before
        // resolving and signing. A short-lived grant must not be stretched by a slow Control
        // response or a busy database.
        let admission_now = self.store.current_time().await?;
        let mut work_admission_expires_at = None;
        if let Some((agent_run_id, run_revision, execution_id, recovery)) = work_run {
            let admission = self
                .work_admission
                .resolve(
                    agent_run_id,
                    &contracts::http::WorkConfigurationAdmissionQuery {
                        project_id: request.project_id,
                        course_id: request.course_id,
                        environment_id,
                        environment_revision: request.expected_revision,
                        actor_id: request.actor_id,
                        run_revision,
                        execution_id,
                    },
                    admission_now,
                )
                .await?;
            if admission.run_id != agent_run_id
                || admission.validate_recovery_for(execution_id).is_err()
            {
                return Err(FreezeBindingError::ExecutionBindingInvalid);
            }
            match recovery {
                None => {
                    let preauthorization = admission
                        .preauthorization
                        .as_ref()
                        .ok_or(FreezeBindingError::ExecutionBindingInvalid)?;
                    work_admission_expires_at = Some(preauthorization.expires_at);
                }
                Some((plan_id, plan_revision, source_identity)) => {
                    let binding = admission
                        .recovery
                        .as_ref()
                        .ok_or(FreezeBindingError::ExecutionBindingInvalid)?;
                    if binding.plan_id != plan_id
                        || binding.plan_revision != plan_revision
                        || binding.source_identity != source_identity
                    {
                        return Err(FreezeBindingError::ExecutionBindingInvalid);
                    }
                }
            }
        }
        let now = self.store.current_time().await?;
        if work_admission_expires_at.is_some_and(|expires_at| expires_at <= now) {
            return Err(FreezeBindingError::ExecutionBindingInvalid);
        }
        let instance = self.store.load(environment_id).await?;
        let recovery = match &request.purpose {
            EnvironmentExecutionPurpose::WorkConfigurationRecovery { .. } => true,
            EnvironmentExecutionPurpose::EvaluationProbe { .. }
            | EnvironmentExecutionPurpose::WorkConfiguration { .. } => false,
        };
        if instance.project_id != request.project_id
            || instance.course_id != request.course_id
            || instance.owner_id != request.actor_id
            || instance.revision != request.expected_revision
            || instance.class != expected_class
            || instance.runtime_kind != RuntimeKind::VirtualMachine
            || instance.desired_state != DesiredEnvironmentState::Running
            || instance.observed_state != ObservedEnvironmentState::Ready
            || instance.observed_generation != instance.generation
            || (!recovery && instance.eligibility_expires_at <= now)
            || !instance
                .endpoints
                .iter()
                .any(|endpoint| endpoint.health == EndpointHealth::Healthy)
        {
            return Err(FreezeBindingError::EnvironmentNotEligible);
        }
        let release = self
            .releases
            .resolve(instance.release_id, instance.release_version)
            .await?;
        if release.withdrawn_at.is_some()
            || release.projection.release.project_id != instance.project_id
            || release.projection.release.course_id != instance.course_id
            || release.projection.release.runtime_kind != instance.runtime_kind
        {
            return Err(FreezeBindingError::ReleaseIdentityMismatch);
        }
        let environment = frozen_identity(&instance, &release.projection.release.artifact);
        let expires_at = if recovery {
            // Recovery is authorized by the durable execution intent and the
            // current VM source identity.  The original environment
            // eligibility window is deliberately not reused: a process that
            // survived that window still receives only a new short TTL.
            execution_certificate_expiry_for_recovery(now)?
        } else {
            execution_certificate_expiry(
                now,
                instance.eligibility_expires_at,
                work_admission_expires_at,
            )?
        };
        let expected_source_identity = match &request.purpose {
            EnvironmentExecutionPurpose::WorkConfigurationRecovery {
                source_identity, ..
            } => Some(source_identity.as_str()),
            EnvironmentExecutionPurpose::EvaluationProbe { .. }
            | EnvironmentExecutionPurpose::WorkConfiguration { .. } => None,
        };
        let source = self
            .resolve_vm_execution(
                instance.id,
                instance.generation,
                &request.public_key_openssh,
                principal,
                &purpose_key,
                expected_source_identity,
                now,
                expires_at,
            )
            .await?;
        let binding = EnvironmentExecutionBinding {
            environment,
            source,
        };
        binding
            .validate_for(environment_id, request, now)
            .map_err(|_| FreezeBindingError::ExecutionBindingInvalid)?;
        Ok(binding)
    }

    async fn resolve_vm(
        &self,
        environment_id: EnvironmentId,
        generation: u64,
        public_key_openssh: &str,
        now: UtcTimestamp,
    ) -> Result<EnvironmentFreezeSourceBinding, FreezeBindingError> {
        let (vm, ssh_user_ca) = self.vm_configuration()?;
        let row = sqlx::query(
            "SELECT environment_generation,vm_uid,root_disk_uid,service_cluster_ip,ssh_host_key_sha256,observation_sha256 \
             FROM environment.kubevirt_runtime_observations WHERE environment_id=$1 AND state='running'",
        )
        .bind(environment_id.as_uuid())
        .fetch_optional(&self.pool)
        .await?
        .ok_or(FreezeBindingError::EnvironmentNotEligible)?;
        let observed_generation: i64 = row.try_get("environment_generation")?;
        if u64::try_from(observed_generation).ok() != Some(generation) {
            return Err(FreezeBindingError::EnvironmentNotEligible);
        }
        let vm_uid: uuid::Uuid = row.try_get("vm_uid")?;
        let root_disk_uid: uuid::Uuid = row.try_get("root_disk_uid")?;
        let host: String = row.try_get("service_cluster_ip")?;
        let expected_host_key_sha256 = row.try_get::<String, _>("ssh_host_key_sha256")?;
        let observation_sha256 = row.try_get::<String, _>("observation_sha256")?;
        let source_identity = Sha256Digest::of_canonical(&serde_json::json!({
            "environmentId": environment_id,
            "generation": generation,
            "namespace": format!("lw-env-{environment_id}"),
            "vmUid": vm_uid,
            "rootDiskUid": root_disk_uid,
            "serviceClusterIp": host,
            "sshHostKeySha256": expected_host_key_sha256,
            "observationSha256": observation_sha256,
        }))
        .map_err(|_| FreezeBindingError::ObservationInvalid)?
        .to_string();
        let expires_at =
            UtcTimestamp::from_utc(now.get() + time::Duration::seconds(CERTIFICATE_TTL_SECONDS))
                .map_err(|_| FreezeBindingError::CertificateFailed)?;
        let certificate = sign_collector_certificate(
            ssh_user_ca,
            public_key_openssh,
            environment_id,
            now,
            expires_at,
        )?;
        Ok(EnvironmentFreezeSourceBinding::VirtualMachine {
            namespace: format!("lw-env-{environment_id}"),
            host,
            port: 22,
            username: vm.username.clone(),
            workspace_root: vm.workspace_root.clone(),
            expected_host_key_sha256,
            source_identity,
            collector_certificate_openssh: certificate,
            expires_at,
        })
    }

    #[allow(clippy::too_many_arguments)]
    async fn resolve_vm_execution(
        &self,
        environment_id: EnvironmentId,
        generation: u64,
        public_key_openssh: &str,
        principal: &str,
        purpose_key: &str,
        expected_source_identity: Option<&str>,
        now: UtcTimestamp,
        expires_at: UtcTimestamp,
    ) -> Result<EnvironmentExecutionSourceBinding, FreezeBindingError> {
        let (vm, ssh_user_ca) = self.vm_configuration()?;
        let row = sqlx::query(
            "SELECT environment_generation,vm_uid,root_disk_uid,service_cluster_ip,ssh_host_key_sha256,observation_sha256 \
             FROM environment.kubevirt_runtime_observations WHERE environment_id=$1 AND state='running'",
        )
        .bind(environment_id.as_uuid())
        .fetch_optional(&self.pool)
        .await?
        .ok_or(FreezeBindingError::EnvironmentNotEligible)?;
        let observed_generation: i64 = row.try_get("environment_generation")?;
        if u64::try_from(observed_generation).ok() != Some(generation) {
            return Err(FreezeBindingError::EnvironmentNotEligible);
        }
        let vm_uid: uuid::Uuid = row.try_get("vm_uid")?;
        let root_disk_uid: uuid::Uuid = row.try_get("root_disk_uid")?;
        let host: String = row.try_get("service_cluster_ip")?;
        let expected_host_key_sha256 = row.try_get::<String, _>("ssh_host_key_sha256")?;
        let observation_sha256 = row.try_get::<String, _>("observation_sha256")?;
        let source_identity = Sha256Digest::of_canonical(&serde_json::json!({
            "environmentId": environment_id,
            "generation": generation,
            "namespace": format!("lw-env-{environment_id}"),
            "vmUid": vm_uid,
            "rootDiskUid": root_disk_uid,
            "serviceClusterIp": host,
            "sshHostKeySha256": expected_host_key_sha256,
            "observationSha256": observation_sha256,
        }))
        .map_err(|_| FreezeBindingError::ObservationInvalid)?
        .to_string();
        if expected_source_identity.is_some_and(|expected| expected != source_identity) {
            return Err(FreezeBindingError::ExecutionBindingInvalid);
        }
        let certificate = sign_execution_certificate(
            ssh_user_ca,
            public_key_openssh,
            environment_id,
            principal,
            purpose_key,
            now,
            expires_at,
        )?;
        Ok(EnvironmentExecutionSourceBinding::VirtualMachine {
            namespace: format!("lw-env-{environment_id}"),
            host,
            port: 22,
            username: vm.username.clone(),
            workspace_root: vm.workspace_root.clone(),
            expected_host_key_sha256,
            source_identity,
            execution_certificate_openssh: certificate,
            expires_at,
        })
    }

    fn vm_configuration(
        &self,
    ) -> Result<(&VmFreezeBindingConfiguration, &PrivateKey), FreezeBindingError> {
        match (self.configuration.vm.as_ref(), self.ssh_user_ca.as_deref()) {
            (Some(configuration), Some(ssh_user_ca)) => Ok((configuration, ssh_user_ca)),
            _ => Err(FreezeBindingError::ProviderUnavailable),
        }
    }
}

fn frozen_identity(
    instance: &contracts::environment::EnvironmentInstance,
    artifact: &ImageArtifact,
) -> FrozenEnvironmentIdentity {
    FrozenEnvironmentIdentity {
        environment_id: instance.id,
        environment_revision: instance.revision,
        release_id: instance.release_id,
        release_version: instance.release_version,
        runtime_kind: instance.runtime_kind,
        build_request_id: match artifact {
            ImageArtifact::Container {
                build_request_id, ..
            } => Some(*build_request_id),
            ImageArtifact::VirtualMachine { .. } => None,
        },
    }
}

fn execution_certificate_expiry(
    now: UtcTimestamp,
    environment_expires_at: UtcTimestamp,
    work_admission_expires_at: Option<UtcTimestamp>,
) -> Result<UtcTimestamp, FreezeBindingError> {
    let short_lived_expires_at =
        UtcTimestamp::from_utc(now.get() + time::Duration::seconds(CERTIFICATE_TTL_SECONDS))
            .map_err(|_| FreezeBindingError::CertificateFailed)?;
    let expires_at = work_admission_expires_at.map_or(environment_expires_at, |grant_expires_at| {
        std::cmp::min(environment_expires_at, grant_expires_at)
    });
    let expires_at = std::cmp::min(short_lived_expires_at, expires_at);
    if expires_at <= now {
        return Err(FreezeBindingError::ExecutionBindingInvalid);
    }
    Ok(expires_at)
}

fn execution_certificate_expiry_for_recovery(
    now: UtcTimestamp,
) -> Result<UtcTimestamp, FreezeBindingError> {
    let short_lived_expires_at =
        UtcTimestamp::from_utc(now.get() + time::Duration::seconds(CERTIFICATE_TTL_SECONDS))
            .map_err(|_| FreezeBindingError::CertificateFailed)?;
    if short_lived_expires_at <= now {
        return Err(FreezeBindingError::ExecutionBindingInvalid);
    }
    Ok(short_lived_expires_at)
}

fn sign_collector_certificate(
    ca: &PrivateKey,
    public_key_openssh: &str,
    environment_id: EnvironmentId,
    valid_after: UtcTimestamp,
    valid_before: UtcTimestamp,
) -> Result<String, FreezeBindingError> {
    sign_user_certificate(
        ca,
        public_key_openssh,
        "labweaver-collector",
        &format!("collector:{environment_id}"),
        valid_after,
        valid_before,
        Some("internal-sftp -R"),
    )
}

fn sign_execution_certificate(
    ca: &PrivateKey,
    public_key_openssh: &str,
    environment_id: EnvironmentId,
    principal: &str,
    purpose_key: &str,
    valid_after: UtcTimestamp,
    valid_before: UtcTimestamp,
) -> Result<String, FreezeBindingError> {
    sign_user_certificate(
        ca,
        public_key_openssh,
        principal,
        &format!("execution:{purpose_key}:{environment_id}"),
        valid_after,
        valid_before,
        None,
    )
}

fn sign_user_certificate(
    ca: &PrivateKey,
    public_key_openssh: &str,
    principal: &str,
    key_id: &str,
    valid_after: UtcTimestamp,
    valid_before: UtcTimestamp,
    force_command: Option<&str>,
) -> Result<String, FreezeBindingError> {
    if public_key_openssh.len() > 4096 || public_key_openssh.chars().any(char::is_control) {
        return Err(FreezeBindingError::CollectorKeyInvalid);
    }
    let public_key = PublicKey::from_openssh(public_key_openssh)
        .map_err(|_| FreezeBindingError::CollectorKeyInvalid)?;
    let after = u64::try_from(valid_after.get().unix_timestamp())
        .map_err(|_| FreezeBindingError::CertificateFailed)?;
    let before = u64::try_from(valid_before.get().unix_timestamp())
        .map_err(|_| FreezeBindingError::CertificateFailed)?;
    let mut nonce = vec![0_u8; certificate::Builder::RECOMMENDED_NONCE_SIZE];
    rand::rng().fill_bytes(&mut nonce);
    let mut builder = certificate::Builder::new(nonce, &public_key, after, before)
        .map_err(|_| FreezeBindingError::CertificateFailed)?;
    builder
        .cert_type(certificate::CertType::User)
        .map_err(|_| FreezeBindingError::CertificateFailed)?;
    builder
        .key_id(key_id.to_owned())
        .map_err(|_| FreezeBindingError::CertificateFailed)?;
    builder
        .valid_principal(principal)
        .map_err(|_| FreezeBindingError::CertificateFailed)?;
    if let Some(force_command) = force_command {
        builder
            .critical_option("force-command", force_command)
            .map_err(|_| FreezeBindingError::CertificateFailed)?;
    }
    builder
        .sign(ca)
        .and_then(|certificate| certificate.to_openssh())
        .map_err(|_| FreezeBindingError::CertificateFailed)
}

fn safe_absolute_workspace(value: &str) -> bool {
    value.starts_with('/')
        && value.len() <= 512
        && !value.contains("//")
        && !value.split('/').any(|part| part == "." || part == "..")
        && !value.chars().any(char::is_control)
}

/// Stable Environment-owned freeze binding failures.
#[derive(Debug, thiserror::Error)]
pub enum FreezeBindingError {
    #[error("LW_ENVIRONMENT_FREEZE_CONFIG_INVALID")]
    ConfigurationInvalid,
    #[error("LW_ENVIRONMENT_FREEZE_NOT_ELIGIBLE")]
    EnvironmentNotEligible,
    #[error("LW_ENVIRONMENT_FREEZE_PROVIDER_UNAVAILABLE")]
    ProviderUnavailable,
    #[error("LW_ENVIRONMENT_RELEASE_IDENTITY_MISMATCH")]
    ReleaseIdentityMismatch,
    #[error("LW_ENVIRONMENT_COLLECTOR_KEY_REQUIRED")]
    CollectorKeyRequired,
    #[error("LW_ENVIRONMENT_COLLECTOR_KEY_INVALID")]
    CollectorKeyInvalid,
    #[error("LW_ENVIRONMENT_FREEZE_OBSERVATION_INVALID")]
    ObservationInvalid,
    #[error("LW_ENVIRONMENT_COLLECTOR_CERTIFICATE_FAILED")]
    CertificateFailed,
    #[error("LW_ENVIRONMENT_EXECUTION_BINDING_INVALID")]
    ExecutionBindingInvalid,
    #[error(transparent)]
    WorkAdmission(#[from] WorkAdmissionClientError),
    #[error(transparent)]
    Store(#[from] EnvironmentStoreError),
    #[error(transparent)]
    Release(#[from] ReleaseProjectionError),
    #[error("LW_ENVIRONMENT_DATABASE_FAILED")]
    Database(#[from] sqlx::Error),
}

#[cfg(test)]
mod tests {
    use super::{
        execution_certificate_expiry, execution_certificate_expiry_for_recovery,
        safe_absolute_workspace, sign_collector_certificate, sign_execution_certificate,
    };
    use contracts::{EnvironmentId, UtcTimestamp};
    use russh::keys::ssh_key::{Certificate, PrivateKey, private::Ed25519Keypair};

    #[test]
    fn vm_workspace_root_is_normalized_posix_absolute() {
        assert!(safe_absolute_workspace("/home/lab/workspace"));
        assert!(!safe_absolute_workspace("home/lab/workspace"));
        assert!(!safe_absolute_workspace("/home/lab/../root"));
        assert!(!safe_absolute_workspace("/home//lab"));
    }

    #[test]
    fn collector_certificate_is_short_lived_and_read_only() -> Result<(), Box<dyn std::error::Error>>
    {
        let ca = PrivateKey::from(Ed25519Keypair::from_seed(&[0x41; 32]));
        let subject = PrivateKey::from(Ed25519Keypair::from_seed(&[0x42; 32]));
        let now: UtcTimestamp = "2026-07-19T08:00:00.000Z".parse()?;
        let expires_at: UtcTimestamp = "2026-07-19T08:04:59.000Z".parse()?;
        let encoded = sign_collector_certificate(
            &ca,
            &subject.public_key().to_openssh()?,
            EnvironmentId::new(),
            now,
            expires_at,
        )?;
        let certificate = Certificate::from_openssh(&encoded)?;
        assert_eq!(certificate.valid_principals(), ["labweaver-collector"]);
        assert_eq!(
            certificate.critical_options().get("force-command"),
            Some(&"internal-sftp -R".to_owned())
        );
        assert_eq!(certificate.valid_before() - certificate.valid_after(), 299);
        Ok(())
    }

    #[test]
    fn execution_certificate_has_purpose_principal_without_force_command()
    -> Result<(), Box<dyn std::error::Error>> {
        let ca = PrivateKey::from(Ed25519Keypair::from_seed(&[0x41; 32]));
        let subject = PrivateKey::from(Ed25519Keypair::from_seed(&[0x42; 32]));
        let now: UtcTimestamp = "2026-07-19T08:00:00.000Z".parse()?;
        let expires_at: UtcTimestamp = "2026-07-19T08:04:59.000Z".parse()?;
        let encoded = sign_execution_certificate(
            &ca,
            &subject.public_key().to_openssh()?,
            EnvironmentId::new(),
            "labweaver-evaluation",
            "evaluation:run:step:1",
            now,
            expires_at,
        )?;
        let certificate = Certificate::from_openssh(&encoded)?;
        assert_eq!(certificate.valid_principals(), ["labweaver-evaluation"]);
        assert!(certificate.critical_options().is_empty());
        assert_eq!(certificate.valid_before() - certificate.valid_after(), 299);
        Ok(())
    }

    #[test]
    fn execution_certificate_expiry_is_capped_by_environment_and_grant()
    -> Result<(), Box<dyn std::error::Error>> {
        let now: UtcTimestamp = "2026-07-19T08:00:00.000Z".parse()?;
        let environment_expires_at: UtcTimestamp = "2026-07-19T08:10:00.000Z".parse()?;
        let grant_expires_at: UtcTimestamp = "2026-07-19T08:02:00.000Z".parse()?;
        assert_eq!(
            execution_certificate_expiry(now, environment_expires_at, Some(grant_expires_at),)?,
            grant_expires_at
        );
        assert_eq!(
            execution_certificate_expiry(now, environment_expires_at, None)?,
            "2026-07-19T08:04:59.000Z".parse()?
        );
        Ok(())
    }

    #[test]
    fn execution_certificate_expiry_rejects_expired_grant() -> Result<(), Box<dyn std::error::Error>>
    {
        let now: UtcTimestamp = "2026-07-19T08:00:00.000Z".parse()?;
        let expired: UtcTimestamp = "2026-07-19T07:59:59.000Z".parse()?;
        assert!(matches!(
            execution_certificate_expiry(now, "2026-07-19T08:10:00.000Z".parse()?, Some(expired)),
            Err(super::FreezeBindingError::ExecutionBindingInvalid)
        ));
        Ok(())
    }

    #[test]
    fn recovery_certificate_expiry_ignores_expired_environment_eligibility()
    -> Result<(), Box<dyn std::error::Error>> {
        let now: UtcTimestamp = "2026-07-19T08:00:00.000Z".parse()?;
        let expires_at = execution_certificate_expiry_for_recovery(now)?;
        assert_eq!(expires_at, "2026-07-19T08:04:59.000Z".parse()?);
        Ok(())
    }
}
