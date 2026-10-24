//! Manual Work publication shares Control's release transaction and outbox with Agent output.
use contracts::authoring::{EnvironmentClass, EnvironmentRuntimeSpec, RuntimeKind};
use contracts::http::{
    ManualWorkTemplateRequest, PlatformImageEntry, PlatformImageKind, PlatformImageStatus,
};
use contracts::supply_chain::{ImageArtifact, ManualTemplateApproval};
use contracts::{ActorId, ImageArtifactId, UtcTimestamp};

use crate::{ControlError, ControlService, ensure_retention_active};

impl ControlService {
    pub(crate) fn review_manual_work_template(
        &self,
        request: &ManualWorkTemplateRequest,
        runtime_kind: RuntimeKind,
        actor_id: ActorId,
        now: UtcTimestamp,
        catalog: &[PlatformImageEntry],
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
                let (resolved_id, format) = self
                    .config
                    .virtual_machine_bases
                    .resolve_with_catalog(
                        provider_binding,
                        storage_class_binding,
                        base_disk,
                        catalog,
                        self.config.trust_revision,
                    )
                    .ok_or(ControlError::ArtifactMismatch)?;
                if resolved_id != artifact_id {
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
