//! Manual Work publication shares Control's release transaction and outbox with Agent output.
use contracts::authoring::{EnvironmentClass, EnvironmentRuntimeSpec, RuntimeKind};
use contracts::http::{
    ManualWorkTemplateRequest, PlatformImageEntry, PlatformImageKind, PlatformImageStatus,
};
use contracts::supply_chain::{ImageArtifact, ManualTemplateApproval};
use contracts::{ActorId, ImageArtifactId, UtcTimestamp};

use crate::{ControlError, ControlService, ensure_retention_active};

impl ControlService {
    /// Uses the exact governance rule also enforced by the publication transaction.
    pub async fn require_manual_template_publisher(
        &self,
        project_id: contracts::ProjectId,
        actor_id: ActorId,
        platform_admin: bool,
    ) -> Result<(), ControlError> {
        let mut transaction = self.pool.begin().await.map_err(crate::db)?;
        let project = crate::project_from_tx(&mut transaction, project_id).await?;
        if !platform_admin
            && project.owner_actor_id != actor_id
            && !crate::object_authorization::permits_in_transaction(
                &mut transaction,
                project_id,
                actor_id,
                contracts::object_authorization::ObjectAction::ManageMembers,
            )
            .await?
        {
            return Err(ControlError::ProjectGovernanceDenied);
        }
        transaction.rollback().await.map_err(crate::db)?;
        Ok(())
    }

    /// Combines Environment's loaded runtime choices with Control's real retention policy.
    pub fn manual_template_options(
        &self,
        runtime: contracts::manual_templates::ManualTemplateRuntimeOptions,
    ) -> Result<contracts::manual_templates::ManualTemplateOptions, ControlError> {
        Ok(contracts::manual_templates::ManualTemplateOptions {
            runtime,
            retention: contracts::manual_templates::ManualTemplateRetentionOption {
                policy_id: self.config.retention_policy_id,
                policy_revision: contracts::Revision::new(1)
                    .map_err(|_| ControlError::ConfigurationInvalid)?,
                label: "平台材料保留策略".to_owned(),
                description: format!(
                    "保留到所选日期，最长 {} 天；到期按删除策略处理。此期限不代表计算资源已经释放。",
                    self.config.retention_seconds / 86_400
                ),
                max_retention_seconds: self.config.retention_seconds,
            },
        })
    }

    fn validate_manual_template_options(
        &self,
        request: &ManualWorkTemplateRequest,
        runtime: &contracts::manual_templates::ManualTemplateRuntimeOptions,
        now: UtcTimestamp,
    ) -> Result<(), ControlError> {
        use contracts::{RetentionClass, RetentionDisposition};
        let spec = &request.spec;
        let binding = match &spec.runtime {
            EnvironmentRuntimeSpec::Container {
                provider_binding, ..
            }
            | EnvironmentRuntimeSpec::VirtualMachine {
                provider_binding, ..
            } => provider_binding,
        };
        let provider = runtime
            .providers
            .iter()
            .find(|provider| {
                &provider.binding == binding && provider.runtime_kind == spec.runtime.kind()
            })
            .ok_or(ControlError::ManualTemplateOptionInvalid)?;
        if let EnvironmentRuntimeSpec::VirtualMachine {
            storage_class_binding,
            base_disk,
            ..
        } = &spec.runtime
        {
            let storage = provider
                .storage_classes
                .iter()
                .find(|storage| &storage.binding == storage_class_binding)
                .ok_or(ControlError::ManualTemplateOptionInvalid)?;
            if base_disk.capacity_bytes > storage.max_capacity_bytes
                || spec.resources.storage_bytes > storage.max_capacity_bytes
            {
                return Err(ControlError::ManualTemplateOptionInvalid);
            }
        }
        let retention = &spec.retention;
        let milliseconds = retention
            .retain_until
            .map(|until| (until.get() - now.get()).whole_milliseconds());
        if retention.policy_id != self.config.retention_policy_id
            || retention.policy_revision.get() != 1
            || retention.class != RetentionClass::RunEvidence
            || retention.disposition != RetentionDisposition::Delete
            || !milliseconds.is_some_and(|milliseconds| {
                milliseconds > 0
                    && milliseconds <= i128::from(self.config.retention_seconds) * 1_000
            })
        {
            return Err(ControlError::ManualTemplateOptionInvalid);
        }
        Ok(())
    }

    pub(crate) fn review_manual_work_template(
        &self,
        request: &ManualWorkTemplateRequest,
        runtime_kind: RuntimeKind,
        actor_id: ActorId,
        now: UtcTimestamp,
        catalog: &[PlatformImageEntry],
        runtime_options: &contracts::manual_templates::ManualTemplateRuntimeOptions,
    ) -> Result<(ManualTemplateApproval, ImageArtifact), ControlError> {
        request
            .spec
            .validate()
            .map_err(|_| ControlError::ContractInvalid)?;
        if !request.confirmed
            || request.spec.class != EnvironmentClass::Work
            || request.spec.runtime.kind() != runtime_kind
            || request.reason.trim().is_empty()
            || request.reason.chars().count() > 500
            || request.description.chars().count() > 4_000
            || request.template_family_id.is_empty()
            || request.template_family_id.len() > 128
            || !request
                .template_family_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
        {
            return Err(ControlError::ContractInvalid);
        }
        ensure_retention_active(&request.spec.retention, now)?;
        self.validate_manual_template_options(request, runtime_options, now)?;
        let image = catalog
            .iter()
            .find(|entry| {
                entry.catalog_id == request.platform_image_id
                    && entry.repin_generation == request.platform_image_generation
                    && entry.status == PlatformImageStatus::Active
                    && entry.trust_revision == self.config.trust_revision.get()
            })
            .ok_or(ControlError::ArtifactNotAuthoritative)?;
        let artifact_id: ImageArtifactId = image
            .catalog_id
            .to_string()
            .parse()
            .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
        let artifact = match &request.spec.runtime {
            EnvironmentRuntimeSpec::Container { .. }
                if image.kind == PlatformImageKind::Container =>
            {
                let reference = image.source_reference.split('@').next().unwrap_or_default();
                let repository = reference
                    .rsplit_once(':')
                    .filter(|(_, tag)| !tag.contains('/'))
                    .map_or(reference, |(repository, _)| repository);
                if !repository.starts_with(&format!(
                    "{}/",
                    self.config
                        .container_build
                        .output_repository_prefix
                        .trim_end_matches('/')
                )) {
                    return Err(ControlError::ArtifactMismatch);
                }
                ImageArtifact::CatalogContainer {
                    id: artifact_id,
                    catalog_id: image.catalog_id,
                    repin_generation: image.repin_generation,
                    trust_revision: self.config.trust_revision,
                    repository: repository.to_owned(),
                    digest: image.resolved_digest.clone(),
                }
            }
            EnvironmentRuntimeSpec::VirtualMachine {
                provider_binding,
                base_disk,
                storage_class_binding,
                ..
            } if image.kind == PlatformImageKind::VirtualMachine => {
                // Restrict resolution to the selected catalog row. A similarly named static base
                // cannot replace the explicitly reviewed image.
                if image.binding != base_disk.binding {
                    return Err(ControlError::ArtifactMismatch);
                }
                // Environment's loaded provider catalog authorizes placement; the selected
                // active image is the disk authority. Do not consult Control's Agent-only VM
                // catalog copy for manual publication.
                let format = image.format.ok_or(ControlError::ArtifactMismatch)?;
                let reference = image.source_reference.split('@').next().unwrap_or_default();
                let repository = reference
                    .rsplit_once(':')
                    .filter(|(_, tag)| !tag.contains('/'))
                    .map_or(reference, |(repository, _)| repository);
                if image.capacity_bytes != Some(base_disk.capacity_bytes)
                    || base_disk.source_registry_digest
                        != format!("docker://{repository}@{}", image.resolved_digest)
                    || image.disk_sha256.is_none()
                    || provider_binding.trim().is_empty()
                    || storage_class_binding.trim().is_empty()
                {
                    return Err(ControlError::ArtifactMismatch);
                }
                ImageArtifact::VirtualMachine {
                    id: artifact_id,
                    base_disk: base_disk.clone(),
                    format,
                }
            }
            _ => return Err(ControlError::ArtifactMismatch),
        };
        artifact
            .validate()
            .map_err(|_| ControlError::ArtifactMismatch)?;
        Ok((
            ManualTemplateApproval {
                template_family_id: request.template_family_id.clone(),
                description: request.description.clone(),
                platform_image: image.clone(),
                spec: request.spec.clone(),
                reason: request.reason.clone(),
                approved_by: actor_id,
                approved_at: now,
                trust_revision: self.config.trust_revision,
            },
            artifact,
        ))
    }
}
