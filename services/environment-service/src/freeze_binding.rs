//! Environment-authoritative immutable submission source resolution.

use persistence_sqlx::Sha256Digest;
use std::{path::PathBuf, sync::Arc}; // internal persistence hash, not contract hash

use contracts::{
    EnvironmentId, UtcTimestamp,
    authoring::RuntimeKind,
    environment::{DesiredEnvironmentState, EndpointHealth, ObservedEnvironmentState},
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
    ReleaseProjectionError,
};

const CERTIFICATE_TTL_SECONDS: i64 = 299;

/// Reviewed deployment values required to bind both Sprint 2 freeze transports.
#[derive(Clone, Debug)]
pub struct FreezeBindingConfiguration {
    pub container_workspace_storage_class: String,
    pub vm_username: String,
    pub vm_workspace_root: String,
    pub ssh_user_ca_public_key: String,
    pub ssh_user_ca_private_key_path: PathBuf,
}

/// Resolves current runtime state and issues VM collector credentials.
#[derive(Clone)]
pub struct FreezeBindingService {
    pool: PgPool,
    store: PgEnvironmentStore,
    releases: PgReleaseProjectionStore,
    configuration: FreezeBindingConfiguration,
    ssh_user_ca: Arc<PrivateKey>,
}

impl FreezeBindingService {
    pub fn new(
        pool: PgPool,
        releases: PgReleaseProjectionStore,
        configuration: FreezeBindingConfiguration,
    ) -> Result<Self, FreezeBindingError> {
        if configuration
            .container_workspace_storage_class
            .trim()
            .is_empty()
            || configuration.vm_username.trim().is_empty()
            || !configuration
                .vm_username
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
            || !safe_absolute_workspace(&configuration.vm_workspace_root)
            || !configuration.ssh_user_ca_private_key_path.is_absolute()
        {
            return Err(FreezeBindingError::ConfigurationInvalid);
        }
        let ssh_user_ca = load_secret_key(&configuration.ssh_user_ca_private_key_path, None)
            .map_err(|_| FreezeBindingError::ConfigurationInvalid)?;
        let configured_public_key = PublicKey::from_openssh(&configuration.ssh_user_ca_public_key)
            .map_err(|_| FreezeBindingError::ConfigurationInvalid)?;
        if ssh_user_ca.public_key() != &configured_public_key {
            return Err(FreezeBindingError::ConfigurationInvalid);
        }
        Ok(Self {
            store: PgEnvironmentStore::new(pool.clone()),
            pool,
            releases,
            configuration,
            ssh_user_ca: Arc::new(ssh_user_ca),
        })
    }

    pub async fn resolve(
        &self,
        environment_id: EnvironmentId,
        request: &EnvironmentFreezeBindingRequest,
    ) -> Result<EnvironmentFreezeBinding, FreezeBindingError> {
        let now = self.store.current_time().await?;
        let instance = self.store.load(environment_id).await?;
        if instance.course_id != request.course_id
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

    async fn resolve_vm(
        &self,
        environment_id: EnvironmentId,
        generation: u64,
        public_key_openssh: &str,
        now: UtcTimestamp,
    ) -> Result<EnvironmentFreezeSourceBinding, FreezeBindingError> {
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
            &self.ssh_user_ca,
            public_key_openssh,
            environment_id,
            now,
            expires_at,
        )?;
        Ok(EnvironmentFreezeSourceBinding::VirtualMachine {
            namespace: format!("lw-env-{environment_id}"),
            host,
            port: 22,
            username: self.configuration.vm_username.clone(),
            workspace_root: self.configuration.vm_workspace_root.clone(),
            expected_host_key_sha256,
            source_identity,
            collector_certificate_openssh: certificate,
            expires_at,
        })
    }
}

fn sign_collector_certificate(
    ca: &PrivateKey,
    public_key_openssh: &str,
    environment_id: EnvironmentId,
    valid_after: UtcTimestamp,
    valid_before: UtcTimestamp,
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
        .key_id(format!("labweaver-collector:{environment_id}"))
        .map_err(|_| FreezeBindingError::CertificateFailed)?;
    builder
        .valid_principal("labweaver-collector")
        .map_err(|_| FreezeBindingError::CertificateFailed)?;
    builder
        .critical_option("force-command", "internal-sftp -R")
        .map_err(|_| FreezeBindingError::CertificateFailed)?;
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
    #[error(transparent)]
    Store(#[from] EnvironmentStoreError),
    #[error(transparent)]
    Release(#[from] ReleaseProjectionError),
    #[error("LW_ENVIRONMENT_DATABASE_FAILED")]
    Database(#[from] sqlx::Error),
}

#[cfg(test)]
mod tests {
    use super::{safe_absolute_workspace, sign_collector_certificate};
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
}
