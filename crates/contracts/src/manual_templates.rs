//! Read models for model-free Work publication. Runtime bindings belong to Environment;
//! retention policy identity and duration belong to Control.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{PolicyId, Revision, authoring::RuntimeKind};

#[derive(Clone, Debug, Default, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManualTemplateRuntimeOptions {
    pub providers: Vec<ManualTemplateProviderOption>,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManualTemplateProviderOption {
    pub binding: String,
    pub label: String,
    pub description: String,
    pub runtime_kind: RuntimeKind,
    pub storage_classes: Vec<ManualTemplateStorageOption>,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManualTemplateStorageOption {
    pub binding: String,
    pub label: String,
    pub description: String,
    pub max_capacity_bytes: u64,
    /// Deployment-seeded disks take precedence over dynamic catalog entries with the same binding.
    pub seeded_disks: Vec<ManualTemplateSeededDiskOption>,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManualTemplateSeededDiskOption {
    pub binding: String,
    pub disk_sha256: String,
    pub capacity_bytes: u64,
    pub source_registry_digest: String,
    pub format: crate::supply_chain::VirtualMachineDiskFormat,
    pub storage_class_binding: String,
}
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManualTemplateRetentionOption {
    pub policy_id: PolicyId,
    pub policy_revision: Revision,
    pub label: String,
    pub description: String,
    pub max_retention_seconds: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManualTemplateOptions {
    pub runtime: ManualTemplateRuntimeOptions,
    pub retention: ManualTemplateRetentionOption,
}
