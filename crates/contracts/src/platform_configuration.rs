//! Control-owned non-secret platform settings and verified deployment facts.

use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{ActorId, CourseId, OperationId, ProjectId, Revision, UtcTimestamp};

/// Linux capability names from the kernel UAPI. `ALL` is never an addition.
#[derive(
    Clone, Copy, Debug, Deserialize, Eq, JsonSchema, Ord, PartialEq, PartialOrd, Serialize,
)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum LinuxCapability {
    Chown,
    DacOverride,
    DacReadSearch,
    Fowner,
    Fsetid,
    Kill,
    Setgid,
    Setuid,
    Setpcap,
    LinuxImmutable,
    NetBindService,
    NetBroadcast,
    NetAdmin,
    NetRaw,
    IpcLock,
    IpcOwner,
    SysModule,
    SysRawio,
    SysChroot,
    SysPtrace,
    SysPacct,
    SysAdmin,
    SysBoot,
    SysNice,
    SysResource,
    SysTime,
    SysTtyConfig,
    Mknod,
    Lease,
    AuditWrite,
    AuditControl,
    Setfcap,
    MacOverride,
    MacAdmin,
    Syslog,
    WakeAlarm,
    BlockSuspend,
    AuditRead,
    Perfmon,
    Bpf,
    CheckpointRestore,
}

/// An execution boundary declaration. The Environment consumer must prove exclusivity.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PrivilegedExecutionLocation {
    DedicatedNodePool {
        #[serde(rename = "nodeSelector")]
        node_selector: BTreeMap<String, String>,
        #[serde(rename = "taintKey")]
        taint_key: String,
        #[serde(rename = "taintValue")]
        taint_value: String,
    },
    VirtualMachineGuest {
        #[serde(rename = "providerBinding")]
        provider_binding: String,
    },
}

/// Object scope for a runtime preauthorization, never a builder authorization.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RuntimePreauthorizationScope {
    Project {
        #[serde(rename = "projectId")]
        project_id: ProjectId,
    },
    Course {
        #[serde(rename = "courseId")]
        course_id: CourseId,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimePreauthorization {
    pub scope: RuntimePreauthorizationScope,
    pub capabilities: BTreeSet<LinuxCapability>,
    pub privileged: bool,
}

/// Platform allowed range. This does not replace object authorization or actual placement.
#[derive(Clone, Debug, Default, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PlatformRuntimePolicy {
    /// Zero means guest/container root, without host privileges.
    pub default_container_uid: u32,
    pub allowed_capabilities: BTreeSet<LinuxCapability>,
    pub preauthorizations: Vec<RuntimePreauthorization>,
    pub privileged_allowed: bool,
    pub privileged_execution_location: Option<PrivilegedExecutionLocation>,
}

#[derive(
    Clone, Copy, Debug, Deserialize, Eq, JsonSchema, Ord, PartialEq, PartialOrd, Serialize,
)]
pub enum BuilderEntitlement {
    #[serde(rename = "network.host")]
    NetworkHost,
    #[serde(rename = "security.insecure")]
    SecurityInsecure,
}

/// Independently configured builder range. Runtime requests cannot change these fields.
#[derive(Clone, Debug, Default, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BuilderSecurityPolicy {
    pub capabilities: BTreeSet<LinuxCapability>,
    pub allow_privilege_escalation: bool,
    pub entitlements: BTreeSet<BuilderEntitlement>,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PlatformLifecycleDefaults {
    pub read_only_retention_seconds: u64,
    pub detailed_log_retention_seconds: u64,
    pub expiry_reminder_seconds: Vec<u64>,
    pub preemption_grace_seconds: u64,
}

impl Default for PlatformLifecycleDefaults {
    fn default() -> Self {
        Self {
            read_only_retention_seconds: 604_800,
            detailed_log_retention_seconds: 2_592_000,
            expiry_reminder_seconds: vec![86_400, 3_600],
            preemption_grace_seconds: 60,
        }
    }
}

/// The one managed settings document. It contains no credential values or secret locators.
#[derive(Clone, Debug, Default, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PlatformConfigurationDesired {
    pub lifecycle: PlatformLifecycleDefaults,
    pub runtime_policy: PlatformRuntimePolicy,
    pub builder_policy: BuilderSecurityPolicy,
}

/// Shared Kubernetes placement limits consumed by both Control validation and generated Helm schema.
pub const DEDICATED_NODE_SELECTOR_MIN_PROPERTIES: usize = 1;
pub const DEDICATED_NODE_SELECTOR_MAX_PROPERTIES: usize = 16;
pub const LABEL_PREFIX_MAX_LENGTH: usize = 253;
pub const LABEL_NAME_MAX_LENGTH: usize = 63;
pub const LABEL_VALUE_MAX_LENGTH: usize = 63;
pub const LABEL_KEY_MAX_LENGTH: usize = LABEL_PREFIX_MAX_LENGTH + 1 + LABEL_NAME_MAX_LENGTH;
pub const LABEL_KEY_PATTERN: &str = r"^([a-z0-9]([-a-z0-9]*[a-z0-9])?\.)*[a-z0-9]([-a-z0-9]*[a-z0-9])?/([A-Za-z0-9]([-A-Za-z0-9_.]*[A-Za-z0-9])?)$|^[A-Za-z0-9]([-A-Za-z0-9_.]*[A-Za-z0-9])?$";
/// DNS prefix expression with the same per-segment limit as `valid_label_key`.
#[must_use]
pub fn label_prefix_pattern() -> String {
    let segment = format!(
        "[a-z0-9]([-a-z0-9]{{0,{}}}[a-z0-9])?",
        LABEL_VALUE_MAX_LENGTH - 2
    );
    format!("^{segment}(\\.{segment})*$")
}
pub const LABEL_NAME_PATTERN: &str = r"^[A-Za-z0-9]([-A-Za-z0-9_.]*[A-Za-z0-9])?$";
pub const LABEL_VALUE_PATTERN: &str = r"^[A-Za-z0-9]([A-Za-z0-9_.-]*[A-Za-z0-9])?$";

/// Fields with no execution-side consumer in the current platform deployment.
pub const PLATFORM_CONFIGURATION_UNSUPPORTED_FIELDS: &[&str] = &[
    "runtimePolicy.defaultContainerUid",
    "runtimePolicy.allowedCapabilities",
    "runtimePolicy.preauthorizations",
    "runtimePolicy.privilegedExecutionLocation",
    "builderPolicy.capabilities",
    "builderPolicy.allowPrivilegeEscalation",
    "builderPolicy.entitlements",
];

/// A policy field whose execution-side support is decided by the Control validator.
#[derive(Clone, Copy)]
pub struct PlatformConfigurationPolicyDescriptor {
    pub field: &'static str,
    pub is_unsupported: fn(&PlatformConfigurationDesired) -> bool,
    #[cfg(test)]
    pub rejected_sample: fn(&mut PlatformConfigurationDesired),
    #[cfg(test)]
    pub accepted_sample: fn(&mut PlatformConfigurationDesired),
}

fn unsupported_default_container_uid(configuration: &PlatformConfigurationDesired) -> bool {
    configuration.runtime_policy.default_container_uid != 0
}

fn unsupported_allowed_capabilities(configuration: &PlatformConfigurationDesired) -> bool {
    !configuration.runtime_policy.allowed_capabilities.is_empty()
}

fn unsupported_preauthorizations(configuration: &PlatformConfigurationDesired) -> bool {
    !configuration.runtime_policy.preauthorizations.is_empty()
}

fn unsupported_privileged_execution_location(configuration: &PlatformConfigurationDesired) -> bool {
    matches!(
        configuration.runtime_policy.privileged_execution_location,
        Some(PrivilegedExecutionLocation::VirtualMachineGuest { .. })
    )
}

fn unsupported_builder_capabilities(configuration: &PlatformConfigurationDesired) -> bool {
    !configuration.builder_policy.capabilities.is_empty()
}

fn unsupported_builder_privilege_escalation(configuration: &PlatformConfigurationDesired) -> bool {
    configuration.builder_policy.allow_privilege_escalation
}

fn unsupported_builder_entitlements(configuration: &PlatformConfigurationDesired) -> bool {
    !configuration.builder_policy.entitlements.is_empty()
}

pub const PLATFORM_CONFIGURATION_POLICY_DESCRIPTORS: &[PlatformConfigurationPolicyDescriptor] = &[
    PlatformConfigurationPolicyDescriptor {
        field: "runtimePolicy.defaultContainerUid",
        is_unsupported: unsupported_default_container_uid,
        #[cfg(test)]
        rejected_sample: |configuration| configuration.runtime_policy.default_container_uid = 1000,
        #[cfg(test)]
        accepted_sample: |configuration| configuration.runtime_policy.default_container_uid = 0,
    },
    PlatformConfigurationPolicyDescriptor {
        field: "runtimePolicy.allowedCapabilities",
        is_unsupported: unsupported_allowed_capabilities,
        #[cfg(test)]
        rejected_sample: |configuration| {
            configuration
                .runtime_policy
                .allowed_capabilities
                .insert(LinuxCapability::SysPtrace);
        },
        #[cfg(test)]
        accepted_sample: |configuration| configuration.runtime_policy.allowed_capabilities.clear(),
    },
    PlatformConfigurationPolicyDescriptor {
        field: "runtimePolicy.preauthorizations",
        is_unsupported: unsupported_preauthorizations,
        #[cfg(test)]
        rejected_sample: |configuration| {
            configuration
                .runtime_policy
                .preauthorizations
                .push(RuntimePreauthorization {
                    scope: RuntimePreauthorizationScope::Project {
                        project_id: ProjectId::new(),
                    },
                    capabilities: BTreeSet::new(),
                    privileged: false,
                });
        },
        #[cfg(test)]
        accepted_sample: |configuration| configuration.runtime_policy.preauthorizations.clear(),
    },
    PlatformConfigurationPolicyDescriptor {
        field: "runtimePolicy.privilegedExecutionLocation",
        is_unsupported: unsupported_privileged_execution_location,
        #[cfg(test)]
        rejected_sample: |configuration| {
            configuration.runtime_policy.privileged_allowed = true;
            configuration.runtime_policy.privileged_execution_location =
                Some(PrivilegedExecutionLocation::VirtualMachineGuest {
                    provider_binding: "kubevirt".to_owned(),
                });
        },
        #[cfg(test)]
        accepted_sample: |configuration| {
            configuration.runtime_policy.privileged_allowed = true;
            configuration.runtime_policy.privileged_execution_location =
                Some(PrivilegedExecutionLocation::DedicatedNodePool {
                    node_selector: BTreeMap::from([(
                        String::from("pool"),
                        String::from("privileged"),
                    )]),
                    taint_key: String::from("labweaver.io/privileged"),
                    taint_value: String::from("true"),
                });
        },
    },
    PlatformConfigurationPolicyDescriptor {
        field: "builderPolicy.capabilities",
        is_unsupported: unsupported_builder_capabilities,
        #[cfg(test)]
        rejected_sample: |configuration| {
            configuration
                .builder_policy
                .capabilities
                .insert(LinuxCapability::SysAdmin);
        },
        #[cfg(test)]
        accepted_sample: |configuration| configuration.builder_policy.capabilities.clear(),
    },
    PlatformConfigurationPolicyDescriptor {
        field: "builderPolicy.allowPrivilegeEscalation",
        is_unsupported: unsupported_builder_privilege_escalation,
        #[cfg(test)]
        rejected_sample: |configuration| {
            configuration.builder_policy.allow_privilege_escalation = true;
        },
        #[cfg(test)]
        accepted_sample: |configuration| {
            configuration.builder_policy.allow_privilege_escalation = false;
        },
    },
    PlatformConfigurationPolicyDescriptor {
        field: "builderPolicy.entitlements",
        is_unsupported: unsupported_builder_entitlements,
        #[cfg(test)]
        rejected_sample: |configuration| {
            configuration
                .builder_policy
                .entitlements
                .insert(BuilderEntitlement::NetworkHost);
        },
        #[cfg(test)]
        accepted_sample: |configuration| configuration.builder_policy.entitlements.clear(),
    },
];

impl PlatformConfigurationDesired {
    pub fn validate(&self) -> Result<(), PlatformConfigurationContractError> {
        let lifecycle = &self.lifecycle;
        let reminders: BTreeSet<_> = lifecycle.expiry_reminder_seconds.iter().collect();
        if !(1..=31_536_000).contains(&lifecycle.read_only_retention_seconds)
            || !(1..=31_536_000).contains(&lifecycle.detailed_log_retention_seconds)
            || lifecycle.expiry_reminder_seconds.len() > 16
            || reminders.len() != lifecycle.expiry_reminder_seconds.len()
            || lifecycle
                .expiry_reminder_seconds
                .iter()
                .any(|v| !(1..=31_536_000).contains(v))
            || !(1..=300).contains(&lifecycle.preemption_grace_seconds)
        {
            return Err(PlatformConfigurationContractError::LifecycleInvalid);
        }
        let policy = &self.runtime_policy;
        if policy.privileged_allowed != policy.privileged_execution_location.is_some() {
            return Err(PlatformConfigurationContractError::ExecutionLocationInvalid);
        }
        if let Some(location) = &policy.privileged_execution_location {
            match location {
                PrivilegedExecutionLocation::DedicatedNodePool {
                    node_selector,
                    taint_key,
                    taint_value,
                } => {
                    if node_selector.len() < DEDICATED_NODE_SELECTOR_MIN_PROPERTIES
                        || node_selector.len() > DEDICATED_NODE_SELECTOR_MAX_PROPERTIES
                        || node_selector
                            .iter()
                            .any(|(k, v)| !valid_label_key(k) || !valid_label_value(v))
                        || !valid_label_key(taint_key)
                        || !valid_label_value(taint_value)
                    {
                        return Err(PlatformConfigurationContractError::ExecutionLocationInvalid);
                    }
                }
                PrivilegedExecutionLocation::VirtualMachineGuest { provider_binding } => {
                    if !valid_binding(provider_binding) {
                        return Err(PlatformConfigurationContractError::ExecutionLocationInvalid);
                    }
                }
            }
        }
        let mut scopes = BTreeSet::new();
        if policy.preauthorizations.len() > 512
            || policy.preauthorizations.iter().any(|grant| {
                !scopes.insert(&grant.scope)
                    || !grant.capabilities.is_subset(&policy.allowed_capabilities)
                    || (grant.privileged && !policy.privileged_allowed)
            })
        {
            return Err(PlatformConfigurationContractError::PreauthorizationInvalid);
        }
        for descriptor in PLATFORM_CONFIGURATION_POLICY_DESCRIPTORS {
            if (descriptor.is_unsupported)(self) {
                return Err(PlatformConfigurationContractError::PolicyUnsupported {
                    field: descriptor.field,
                });
            }
        }
        Ok(())
    }
}
fn valid_binding(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= LABEL_VALUE_MAX_LENGTH
        && value
            .bytes()
            .all(|v| v.is_ascii_alphanumeric() || b"-_.".contains(&v))
}

fn valid_label_key(value: &str) -> bool {
    let (prefix, name) = value
        .rsplit_once('/')
        .map_or((None, value), |(prefix, name)| (Some(prefix), name));
    prefix.is_none_or(|v| {
        !v.is_empty()
            && v.len() <= LABEL_PREFIX_MAX_LENGTH
            && v.split('.').all(|label| {
                valid_label_value(label)
                    && label
                        .bytes()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
            })
    }) && valid_label_value(name)
}

fn valid_label_value(value: &str) -> bool {
    valid_binding(value)
        && value.len() <= LABEL_NAME_MAX_LENGTH
        && value.starts_with(|c: char| c.is_ascii_alphanumeric())
        && value.ends_with(|c: char| c.is_ascii_alphanumeric())
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PlatformConfigurationVersion {
    pub revision: Revision,
    pub configuration: PlatformConfigurationDesired,
    /// SHA-256 of the canonical managed settings JSON, without a prefix.
    pub configuration_sha256: String,
    pub created_by: ActorId,
    pub created_at: UtcTimestamp,
}

/// An administrator's registration of received ciphertext, not deployment approval.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PlatformConfigurationBundle {
    pub registered_at_revision: Revision,
    pub desired_revision: Revision,
    /// Administrator-declared binding to the immutable settings snapshot.
    pub configuration_sha256: String,
    /// Computed by Control from actual uploaded ciphertext bytes.
    pub encrypted_bundle_sha256: String,
    pub encrypted_bytes: u64,
    pub registered_by: ActorId,
    pub registered_at: UtcTimestamp,
}

pub const PLATFORM_CONFIGURATION_BUNDLE_MAX_BYTES: u64 = 70 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RegisterPlatformConfigurationBundleRequest {
    pub expected_revision: u64,
    pub configuration_sha256: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PlatformConfigurationApplyStage {
    Requested,
    Validating,
    Applying,
    Verifying,
    Succeeded,
    Failed,
}

impl PlatformConfigurationApplyStage {
    #[must_use]
    pub const fn terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed)
    }
}

/// Readable next step, without executor logs or credential content.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PlatformConfigurationNextStep {
    ExecuteApprovedDeployment,
    WaitForExecutor,
    FixConfigurationAndRetry,
    ReconcileExternalResult,
    None,
}

/// Bounded failure categories. Executor log content is never an API payload.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PlatformConfigurationFailureReason {
    ConfigurationInvalid,
    DependencyUnavailable,
    ExecutionFailed,
    VerificationFailed,
    ExternalResultUnknown,
    Interrupted,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PlatformConfigurationApply {
    pub id: OperationId,
    /// Follow this link to read prior attempts after reconnecting or retrying.
    pub previous_apply_id: Option<OperationId>,
    pub revision: Revision,
    pub desired_revision: Revision,
    pub encrypted_bundle_sha256: String,
    pub stage: PlatformConfigurationApplyStage,
    pub failed_stage: Option<PlatformConfigurationApplyStage>,
    pub failure_reason: Option<PlatformConfigurationFailureReason>,
    pub next_step: PlatformConfigurationNextStep,
    pub requested_by: ActorId,
    pub requested_at: UtcTimestamp,
    pub executor_client_id: Option<String>,
    pub updated_at: UtcTimestamp,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PlatformConfigurationImpact {
    pub runtime_policy_changed: bool,
    pub builder_policy_changed: bool,
    pub lifecycle_policy_changed: bool,
}

/// Whether the current external configuration has a complete verified observation.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PlatformConfigurationAppliedState {
    #[default]
    Unconfigured,
    Verified,
    Unverified,
}

/// Applied means last fully verified configuration. A failed apply can leave external state unknown.
#[derive(Clone, Debug, Default, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PlatformConfigurationStatus {
    /// Aggregate fence, including apply updates. Zero is the unconfigured state.
    pub revision: u64,
    #[serde(default)]
    pub defaults: PlatformConfigurationDesired,
    pub desired: Option<PlatformConfigurationVersion>,
    /// Current desired's last registered artifact. Registration alone never applies it.
    pub registered_bundle: Option<PlatformConfigurationBundle>,
    pub applied: Option<PlatformConfigurationVersion>,
    pub applied_state: PlatformConfigurationAppliedState,
    pub applied_at: Option<UtcTimestamp>,
    pub applied_bundle_sha256: Option<String>,
    pub latest_apply: Option<PlatformConfigurationApply>,
    pub impact: PlatformConfigurationImpact,
}

impl PlatformConfigurationStatus {
    pub fn refresh_impact(&mut self) {
        self.impact =
            self.desired
                .as_ref()
                .map_or_else(PlatformConfigurationImpact::default, |desired| {
                    let applied = self.applied.as_ref().map(|v| &v.configuration);
                    PlatformConfigurationImpact {
                        runtime_policy_changed: applied.is_none_or(|v| {
                            v.runtime_policy != desired.configuration.runtime_policy
                        }),
                        builder_policy_changed: applied.is_none_or(|v| {
                            v.builder_policy != desired.configuration.builder_policy
                        }),
                        lifecycle_policy_changed: applied
                            .is_none_or(|v| v.lifecycle != desired.configuration.lifecycle),
                    }
                });
    }
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdatePlatformConfigurationRequest {
    pub expected_revision: u64,
    pub configuration: PlatformConfigurationDesired,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApplyPlatformConfigurationRequest {
    pub expected_revision: u64,
    pub desired_revision: Revision,
}

/// Only a permission-checked deployment identity can submit execution facts.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReportPlatformConfigurationApplyRequest {
    pub expected_revision: Revision,
    pub stage: PlatformConfigurationApplyStage,
    pub failure_reason: Option<PlatformConfigurationFailureReason>,
    /// Required only on success and must match the immutable desired snapshot.
    pub observed_configuration_sha256: Option<String>,
    pub observed_encrypted_bundle_sha256: Option<String>,
    /// One bounded, fresh observation of all Control replicas. No replica lifecycle is stored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control_replica_observations: Option<ControlConfigurationDeploymentObservation>,
}

/// Facts returned by a running Control process, rather than a reread of mounted files.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ControlConfigurationReplicaObservation {
    pub pod_name: String,
    pub pod_uid: String,
    pub namespace: String,
    /// Changes on process restart even if Kubernetes reuses the same Pod UID.
    pub instance_id: String,
    pub desired_revision: Revision,
    pub configuration_sha256: String,
    pub encrypted_bundle_sha256: String,
    pub observed_at: UtcTimestamp,
}

/// An authorized executor's current Deployment roster and actual per-process readbacks.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ControlConfigurationDeploymentObservation {
    pub deployment_uid: String,
    pub deployment_generation: u64,
    pub expected_replicas: u32,
    pub namespace: String,
    pub release_name: String,
    pub replicas: Vec<ControlConfigurationReplicaObservation>,
}

#[must_use]
pub fn valid_configuration_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v))
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum PlatformConfigurationContractError {
    #[error("LW_PLATFORM_CONFIGURATION_LIFECYCLE_INVALID")]
    LifecycleInvalid,
    #[error("LW_PLATFORM_CONFIGURATION_EXECUTION_LOCATION_INVALID")]
    ExecutionLocationInvalid,
    #[error("LW_PLATFORM_CONFIGURATION_PREAUTHORIZATION_INVALID")]
    PreauthorizationInvalid,
    #[error("LW_PLATFORM_CONFIGURATION_POLICY_UNSUPPORTED:{field}")]
    PolicyUnsupported { field: &'static str },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_runtime_changes_leave_builder_authority_independent()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut configuration = PlatformConfigurationDesired::default();
        configuration.validate()?;
        assert_eq!(configuration.lifecycle.read_only_retention_seconds, 604_800);
        assert_eq!(
            configuration.lifecycle.detailed_log_retention_seconds,
            2_592_000
        );
        assert_eq!(
            configuration.lifecycle.expiry_reminder_seconds,
            [86_400, 3_600]
        );
        assert_eq!(configuration.lifecycle.preemption_grace_seconds, 60);
        let builder = configuration.builder_policy.clone();
        configuration.runtime_policy.privileged_allowed = true;
        assert!(configuration.validate().is_err());
        configuration.runtime_policy.privileged_execution_location =
            Some(PrivilegedExecutionLocation::VirtualMachineGuest {
                provider_binding: "kubevirt-dedicated".to_owned(),
            });
        assert!(matches!(
            configuration.validate(),
            Err(PlatformConfigurationContractError::PolicyUnsupported {
                field: "runtimePolicy.privilegedExecutionLocation"
            })
        ));
        assert_eq!(configuration.builder_policy, builder);
        Ok(())
    }

    #[test]
    fn runtime_policy_rejects_unknown_capability_location_and_excess_preauthorization() {
        assert!(serde_json::from_str::<LinuxCapability>("\"ALL\"").is_err());
        assert!(serde_json::from_str::<LinuxCapability>("\"MADE_UP\"").is_err());
        assert!(
            serde_json::from_str::<PrivilegedExecutionLocation>(
                r#"{"kind":"namespace","namespace":"shared"}"#
            )
            .is_err()
        );
        let mut configuration = PlatformConfigurationDesired::default();
        configuration
            .runtime_policy
            .preauthorizations
            .push(RuntimePreauthorization {
                scope: RuntimePreauthorizationScope::Project {
                    project_id: ProjectId::new(),
                },
                capabilities: BTreeSet::new(),
                privileged: false,
            });
        assert!(matches!(
            configuration.validate(),
            Err(PlatformConfigurationContractError::PolicyUnsupported {
                field: "runtimePolicy.preauthorizations"
            })
        ));
        configuration
            .runtime_policy
            .preauthorizations
            .push(configuration.runtime_policy.preauthorizations[0].clone());
        assert!(configuration.validate().is_err());
    }

    #[test]
    fn location_samples_match_rust_validation() -> Result<(), Box<dyn std::error::Error>> {
        for sample in serde_json::from_str::<Vec<serde_json::Value>>(include_str!(
            "../../../tests/fixtures/platform-configuration-location-cases.json"
        ))? {
            let mut configuration = PlatformConfigurationDesired::default();
            configuration.runtime_policy.privileged_allowed = true;
            configuration.runtime_policy.privileged_execution_location =
                Some(serde_json::from_value(sample["location"].clone())?);
            let accepted = sample["accepted"].as_bool().ok_or("accepted")?;
            assert_eq!(
                configuration.validate().is_ok(),
                accepted,
                "{}",
                sample["name"]
            );
        }
        Ok(())
    }

    #[test]
    fn dedicated_node_pool_constraints_match_deployment_samples() {
        fn configuration(
            node_selector: BTreeMap<String, String>,
            taint_key: &str,
            taint_value: &str,
        ) -> PlatformConfigurationDesired {
            let mut configuration = PlatformConfigurationDesired::default();
            configuration.runtime_policy.privileged_allowed = true;
            configuration.runtime_policy.privileged_execution_location =
                Some(PrivilegedExecutionLocation::DedicatedNodePool {
                    node_selector,
                    taint_key: taint_key.to_owned(),
                    taint_value: taint_value.to_owned(),
                });
            configuration
        }

        let valid = configuration(
            BTreeMap::from([(String::from("pool"), String::from("privileged"))]),
            "labweaver.io/taint",
            "reserved",
        );
        assert!(valid.validate().is_ok());
        for invalid in [
            configuration(BTreeMap::new(), "labweaver.io/taint", "reserved"),
            configuration(
                (0..17)
                    .map(|i| (format!("pool{i}"), String::from("x")))
                    .collect(),
                "labweaver.io/taint",
                "reserved",
            ),
            configuration(
                BTreeMap::from([(String::from("INVALID KEY"), String::from("x"))]),
                "labweaver.io/taint",
                "reserved",
            ),
            configuration(
                BTreeMap::from([(String::from("pool"), String::from("bad value"))]),
                "labweaver.io/taint",
                "reserved",
            ),
            configuration(
                BTreeMap::from([(format!("{}/name", "a".repeat(254)), String::from("x"))]),
                "labweaver.io/taint",
                "reserved",
            ),
            configuration(
                BTreeMap::from([(String::from("pool"), String::from("x"))]),
                "",
                "reserved",
            ),
            configuration(
                BTreeMap::from([(String::from("pool"), String::from("x"))]),
                "labweaver.io/taint",
                "bad value",
            ),
        ] {
            assert!(matches!(
                invalid.validate(),
                Err(PlatformConfigurationContractError::ExecutionLocationInvalid)
            ));
        }
    }

    #[test]
    fn every_policy_descriptor_rejects_its_sample_and_accepts_its_supported_sample() {
        for descriptor in PLATFORM_CONFIGURATION_POLICY_DESCRIPTORS {
            let mut rejected = PlatformConfigurationDesired::default();
            (descriptor.rejected_sample)(&mut rejected);
            assert!(matches!(
                rejected.validate(),
                Err(PlatformConfigurationContractError::PolicyUnsupported { field })
                    if field == descriptor.field
            ));

            let mut accepted = PlatformConfigurationDesired::default();
            (descriptor.accepted_sample)(&mut accepted);
            assert!(accepted.validate().is_ok(), "{}", descriptor.field);
        }
    }

    #[test]
    fn json_and_yaml_use_same_configuration_and_reject_secret_fields()
    -> Result<(), Box<dyn std::error::Error>> {
        let configuration = PlatformConfigurationDesired::default();
        let request = UpdatePlatformConfigurationRequest {
            expected_revision: 0,
            configuration,
        };
        let json = serde_json::to_vec(&request)?;
        let yaml = serde_yaml::to_string(&request)?;
        assert_eq!(
            serde_json::from_slice::<UpdatePlatformConfigurationRequest>(&json)?,
            serde_yaml::from_str::<UpdatePlatformConfigurationRequest>(&yaml)?
        );
        assert!(
            serde_json::from_str::<PlatformConfigurationDesired>(r#"{"token":"synthetic-secret"}"#)
                .is_err()
        );
        Ok(())
    }
}
