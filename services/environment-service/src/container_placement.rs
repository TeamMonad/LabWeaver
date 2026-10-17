//! Deployment-selected Container Work placement, pinned in the existing operation ledger.

use contracts::authoring::{EnvironmentClass, RuntimeKind};
use contracts::environment::EnvironmentInstance;
use contracts::{EnvironmentId, ProjectId};
use persistence_sqlx::{Domain, IdempotencyDecision, IdempotencyStore, Sha256Digest};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

const PLACEMENT_OPERATION: &str = "container-namespace-placement";

/// This file is deployment input shared by the owner and Container executor.
#[derive(Clone, Debug, Deserialize)]
#[serde(
    tag = "mode",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ContainerNamespaceProfile {
    Dedicated {},
    Existing {
        namespace: String,
        namespace_uid: Uuid,
        project_id: ProjectId,
    },
}

impl Default for ContainerNamespaceProfile {
    fn default() -> Self {
        Self::Dedicated {}
    }
}

impl ContainerNamespaceProfile {
    pub fn from_env() -> Result<Self, ContainerPlacementError> {
        let Some(path) = std::env::var_os("LABWEAVER_CONTAINER_PLACEMENT_CONFIG_FILE") else {
            return Ok(Self::Dedicated {});
        };
        let path = std::path::PathBuf::from(path);
        if !path.is_absolute() {
            return Err(ContainerPlacementError::Invalid);
        }
        let profile: Self = serde_json::from_slice(&std::fs::read(path)?)?;
        profile.validate()?;
        Ok(profile)
    }

    pub fn validate(&self) -> Result<(), ContainerPlacementError> {
        if let Self::Existing {
            namespace,
            namespace_uid,
            ..
        } = self
            && (!valid_name(namespace) || namespace_uid.is_nil())
        {
            return Err(ContainerPlacementError::Invalid);
        }
        Ok(())
    }

    pub fn for_instance(
        &self,
        instance: &EnvironmentInstance,
    ) -> Result<ContainerPlacement, ContainerPlacementError> {
        self.validate()?;
        let placement = self.for_identity(instance.id, instance.project_id)?;
        if placement.namespace_uid.is_some()
            && (instance.class != EnvironmentClass::Work
                || instance.runtime_kind != RuntimeKind::Container)
        {
            return Err(ContainerPlacementError::Invalid);
        }
        Ok(placement)
    }

    pub fn for_identity(
        &self,
        environment_id: EnvironmentId,
        project_id: ProjectId,
    ) -> Result<ContainerPlacement, ContainerPlacementError> {
        self.validate()?;
        let (namespace, namespace_uid) = match self {
            Self::Dedicated {} => (format!("lw-env-{environment_id}"), None),
            Self::Existing {
                namespace,
                namespace_uid,
                project_id: allowed,
            } if *allowed == project_id => (namespace.clone(), Some(*namespace_uid)),
            Self::Existing { .. } => return Err(ContainerPlacementError::Invalid),
        };
        Ok(ContainerPlacement {
            environment_id,
            project_id,
            namespace,
            namespace_uid,
        })
    }
}

/// Immutable result of selecting a Work's namespace, before the first Kubernetes effect.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContainerPlacement {
    pub environment_id: EnvironmentId,
    pub project_id: ProjectId,
    pub namespace: String,
    pub namespace_uid: Option<Uuid>,
}

impl ContainerPlacement {
    #[must_use]
    pub fn resource_name(&self, dedicated_name: &str) -> String {
        if self.namespace_uid.is_some() {
            format!("lw-{}-{dedicated_name}", self.environment_id)
        } else {
            dedicated_name.to_owned()
        }
    }

    pub fn validate(&self) -> Result<(), ContainerPlacementError> {
        if !valid_name(&self.namespace)
            || self.namespace_uid.is_some_and(|uid| uid.is_nil())
            || (self.namespace_uid.is_none()
                && self.namespace != format!("lw-env-{}", self.environment_id))
        {
            return Err(ContainerPlacementError::Invalid);
        }
        Ok(())
    }

    /// Bind atomically with executor admission. A changed deployment cannot retarget this Work.
    pub async fn pin(
        &self,
        transaction: &mut Transaction<'_, Postgres>,
    ) -> Result<bool, ContainerPlacementError> {
        self.validate()?;
        let key = self.environment_id.to_string();
        match IdempotencyStore::reserve(
            transaction,
            Domain::Environment,
            PLACEMENT_OPERATION,
            &key,
            placement_hash(self.environment_id, self.project_id)?,
        )
        .await?
        {
            IdempotencyDecision::Reserved => {
                IdempotencyStore::complete(
                    transaction,
                    Domain::Environment,
                    PLACEMENT_OPERATION,
                    &key,
                    &serde_json::to_value(self)?,
                )
                .await?;
                return Ok(true);
            }
            IdempotencyDecision::Replay(value) => {
                if serde_json::from_value::<Self>(value)? != *self {
                    return Err(ContainerPlacementError::Invalid);
                }
            }
            _ => return Err(ContainerPlacementError::Invalid),
        }
        Ok(false)
    }

    /// Legacy dedicated instances have no placement reservation; all new executors pin one.
    pub async fn load(
        pool: &PgPool,
        environment_id: EnvironmentId,
        project_id: ProjectId,
    ) -> Result<Self, ContainerPlacementError> {
        let placement = match IdempotencyStore::lookup(
            pool,
            Domain::Environment,
            PLACEMENT_OPERATION,
            &environment_id.to_string(),
            placement_hash(environment_id, project_id)?,
        )
        .await?
        {
            Some(IdempotencyDecision::Replay(value)) => serde_json::from_value::<Self>(value)?,
            None => {
                ContainerNamespaceProfile::Dedicated {}.for_identity(environment_id, project_id)?
            }
            _ => return Err(ContainerPlacementError::Invalid),
        };
        placement.validate()?;
        if placement.environment_id != environment_id || placement.project_id != project_id {
            return Err(ContainerPlacementError::Invalid);
        }
        Ok(placement)
    }
}

fn placement_hash(
    environment_id: EnvironmentId,
    project_id: ProjectId,
) -> Result<Sha256Digest, ContainerPlacementError> {
    Sha256Digest::of_canonical(
        &serde_json::json!({"environmentId":environment_id,"projectId":project_id}),
    )
    .map_err(|_| ContainerPlacementError::Invalid)
}

pub(crate) fn valid_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 63
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && value
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && value
            .as_bytes()
            .last()
            .is_some_and(u8::is_ascii_alphanumeric)
}

#[derive(Debug, thiserror::Error)]
pub enum ContainerPlacementError {
    #[error("LW_CONTAINER_PLACEMENT_INVALID")]
    Invalid,
    #[error("LW_CONTAINER_PLACEMENT_UNAVAILABLE")]
    Persistence(#[from] persistence_sqlx::PersistenceError),
    #[error("LW_CONTAINER_PLACEMENT_CONFIG_INVALID")]
    Json(#[from] serde_json::Error),
    #[error("LW_CONTAINER_PLACEMENT_CONFIG_UNAVAILABLE")]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn deployment_profile_is_strict_and_uses_synthetic_example() {
        let profile: ContainerNamespaceProfile = serde_json::from_str(include_str!(
            "../../../deploy/config/container-placement.json.example"
        ))
        .expect("example");
        profile.validate().expect("valid example");
        for invalid in [
            r#"{"mode":"existing","namespace":"wrong/path","namespaceUid":"00000000-0000-0000-0000-000000000207","projectId":"01900000-0000-7000-8000-000000000208"}"#,
            r#"{"mode":"existing","namespace":"synthetic","namespaceUid":"00000000-0000-0000-0000-000000000000","projectId":"01900000-0000-7000-8000-000000000208"}"#,
        ] {
            assert!(
                serde_json::from_str::<ContainerNamespaceProfile>(invalid)
                    .expect("well-formed JSON")
                    .validate()
                    .is_err()
            );
        }
        assert!(
            serde_json::from_str::<ContainerNamespaceProfile>(
                r#"{"mode":"dedicated","privileged":true}"#
            )
            .is_err()
        );
        assert!(
            serde_json::from_str::<ContainerNamespaceProfile>(r#"{"mode":"privileged"}"#).is_err()
        );
    }
}
