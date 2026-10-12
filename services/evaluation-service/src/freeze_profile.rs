//! Deployment-owned freeze worker resources. No cluster mutation or credentials.
#![allow(
    missing_docs,
    reason = "internal deployment values are documented with the coordinator"
)]

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Existing resources are supplied by an operator; Evaluation owns only its Job and `ConfigMap`.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum FreezeWorkerProfile {
    #[default]
    Managed,
    Existing {
        namespace: String,
        #[serde(rename = "secretName")]
        secret_name: String,
        #[serde(rename = "serviceAccountName")]
        service_account_name: String,
        #[serde(rename = "networkPolicyNames")]
        network_policy_names: Vec<String>,
        #[serde(rename = "resourceQuotaNames")]
        resource_quota_names: Vec<String>,
        #[serde(default, rename = "nodeName")]
        node_name: Option<String>,
        resources: FreezeWorkerResources,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FreezeWorkerResources {
    pub request_cpu_millicores: u64,
    pub limit_cpu_millicores: u64,
    pub request_memory_bytes: u64,
    pub limit_memory_bytes: u64,
}

impl Default for FreezeWorkerResources {
    fn default() -> Self {
        Self {
            request_cpu_millicores: 100,
            limit_cpu_millicores: 1000,
            request_memory_bytes: 128 * 1024 * 1024,
            limit_memory_bytes: 1024 * 1024 * 1024,
        }
    }
}

impl FreezeWorkerResources {
    #[must_use]
    pub fn valid(&self) -> bool {
        self.request_cpu_millicores > 0
            && self.request_cpu_millicores <= self.limit_cpu_millicores
            && self.limit_cpu_millicores <= 1000
            && self.request_memory_bytes > 0
            && self.request_memory_bytes <= self.limit_memory_bytes
            && self.limit_memory_bytes <= 1024 * 1024 * 1024
    }

    #[must_use]
    pub fn document(&self) -> Value {
        json!({
            "requests": {"cpu": format!("{}m", self.request_cpu_millicores), "memory": self.request_memory_bytes.to_string()},
            "limits": {"cpu": format!("{}m", self.limit_cpu_millicores), "memory": self.limit_memory_bytes.to_string()}
        })
    }
}

impl FreezeWorkerProfile {
    #[must_use]
    pub fn valid(&self) -> bool {
        match self {
            Self::Managed => true,
            Self::Existing {
                namespace,
                secret_name,
                service_account_name,
                network_policy_names,
                resource_quota_names,
                node_name,
                resources,
            } => {
                valid_name(namespace)
                    && valid_name(secret_name)
                    && valid_name(service_account_name)
                    && valid_references(network_policy_names)
                    && valid_references(resource_quota_names)
                    && node_name.as_deref().is_none_or(valid_name)
                    && resources.valid()
            }
        }
    }

    #[must_use]
    pub fn accepts_namespace(&self, namespace: &str) -> bool {
        valid_name(namespace)
            && match self {
                Self::Managed => true,
                Self::Existing {
                    namespace: expected,
                    ..
                } => expected == namespace,
            }
    }

    #[must_use]
    pub fn resources(&self) -> FreezeWorkerResources {
        match self {
            Self::Managed => FreezeWorkerResources::default(),
            Self::Existing { resources, .. } => resources.clone(),
        }
    }
}

pub(crate) fn valid_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 63
        && value
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'-' | b'.'))
        && !value.starts_with(['-', '.'])
        && !value.ends_with(['-', '.'])
}

fn valid_references(names: &[String]) -> bool {
    !names.is_empty()
        && names.len() <= 16
        && names.iter().all(|name| valid_name(name))
        && names
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            == names.len()
}

/// All fields come from the coordinator after real binding resolution, never public input.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum FreezeWorkerDispatch {
    /// A command accepted before dispatch tracking was introduced.
    #[default]
    Untracked,
    /// No Kubernetes write is possible until this becomes Bound in the command ledger.
    Pending,
    Bound {
        namespace: String,
        profile: FreezeWorkerProfile,
    },
}

impl FreezeWorkerDispatch {
    #[must_use]
    pub fn valid(&self) -> bool {
        match self {
            Self::Untracked | Self::Pending => true,
            Self::Bound { namespace, profile } => {
                profile.valid() && profile.accepts_namespace(namespace)
            }
        }
    }
}

/// The observed quota must cover all Pods, including the running Work. Unknown usage fails closed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum QuotaCheck {
    Fits,
    Full,
    Unavailable,
}

pub(crate) fn check_quota(quota: &Value, worker: &FreezeWorkerResources) -> QuotaCheck {
    if quota.pointer("/spec/scopeSelector").is_some()
        || quota
            .pointer("/spec/scopes")
            .is_some_and(|v| v.as_array().is_none_or(|v| !v.is_empty()))
    {
        return QuotaCheck::Unavailable;
    }
    let dimensions = [
        ("requests.cpu", worker.request_cpu_millicores, true),
        ("limits.cpu", worker.limit_cpu_millicores, true),
        ("requests.memory", worker.request_memory_bytes, false),
        ("limits.memory", worker.limit_memory_bytes, false),
    ];
    for (name, addition, cpu) in dimensions {
        let hard = quota.get("spec").and_then(|v| v.get("hard"));
        let status_hard = quota.get("status").and_then(|v| v.get("hard"));
        let used = quota.get("status").and_then(|v| v.get("used"));
        let number = |value: Option<&Value>, upper| {
            value
                .and_then(|v| v.get(name))
                .and_then(Value::as_str)
                .and_then(|text| quantity(text, cpu, upper))
        };
        let (Some(hard), Some(status_hard), Some(used)) = (
            number(hard, false),
            number(status_hard, false),
            number(used, true),
        ) else {
            return QuotaCheck::Unavailable;
        };
        if hard != status_hard {
            return QuotaCheck::Unavailable;
        }
        if used.checked_add(addition).is_none_or(|total| total > hard) {
            return QuotaCheck::Full;
        }
    }
    if let Some(hard) = quota.pointer("/spec/hard/pods") {
        let Some(hard) = hard.as_str().and_then(|v| v.parse::<u64>().ok()) else {
            return QuotaCheck::Unavailable;
        };
        let Some(used) = quota
            .pointer("/status/used/pods")
            .and_then(Value::as_str)
            .and_then(|v| v.parse::<u64>().ok())
        else {
            return QuotaCheck::Unavailable;
        };
        if used.checked_add(1).is_none_or(|total| total > hard) {
            return QuotaCheck::Full;
        }
    }
    QuotaCheck::Fits
}

// Exact integer arithmetic for the bounded forms emitted by these deployment profiles.
// Unknown units, negative values, exponents and precision above nine decimal places stop admission.
fn quantity(text: &str, cpu: bool, upper: bool) -> Option<u64> {
    let (number, scale) = if cpu {
        text.strip_suffix('m')
            .map_or((text, 1000_u64), |number| (number, 1))
    } else {
        let units = [
            ("Ki", 1_u64 << 10),
            ("Mi", 1 << 20),
            ("Gi", 1 << 30),
            ("Ti", 1 << 40),
            ("K", 1000),
            ("M", 1_000_000),
            ("G", 1_000_000_000),
        ];
        units
            .iter()
            .find_map(|(unit, scale)| text.strip_suffix(unit).map(|v| (v, *scale)))
            .unwrap_or((text, 1))
    };
    let (whole, fraction) = number.split_once('.').unwrap_or((number, ""));
    if whole.is_empty() && fraction.is_empty()
        || fraction.len() > 9
        || !whole.bytes().all(|v| v.is_ascii_digit())
        || !fraction.bytes().all(|v| v.is_ascii_digit())
    {
        return None;
    }
    let divisor = 10_u64.checked_pow(u32::try_from(fraction.len()).ok()?)?;
    let whole = if whole.is_empty() {
        0
    } else {
        whole.parse::<u64>().ok()?
    };
    let fraction = if fraction.is_empty() {
        0
    } else {
        fraction.parse::<u64>().ok()?
    };
    let numerator = whole
        .checked_mul(divisor)?
        .checked_add(fraction)?
        .checked_mul(scale)?;
    Some(if upper {
        numerator.checked_add(divisor - 1)? / divisor
    } else {
        numerator / divisor
    })
}

/// Verify the named policy actually selects this worker; do not create or alter policy rules.
pub(crate) fn selected_policy_types(policy: &Value, labels: &Value) -> Option<(bool, bool)> {
    let selector = policy.pointer("/spec/podSelector")?.as_object()?;
    if let Some(matches) = selector.get("matchLabels") {
        for (key, expected) in matches.as_object()? {
            if labels.get(key) != Some(expected) {
                return None;
            }
        }
    }
    if let Some(expressions) = selector.get("matchExpressions") {
        for expression in expressions.as_array()? {
            let key = expression.get("key")?.as_str()?;
            let actual = labels.get(key).and_then(Value::as_str);
            let values = expression.get("values").and_then(Value::as_array);
            let contained =
                |actual: &str| values.is_some_and(|v| v.iter().any(|v| v.as_str() == Some(actual)));
            let matches = match expression.get("operator")?.as_str()? {
                "In" => actual.is_some_and(contained),
                "NotIn" => actual.is_none_or(|actual| !contained(actual)),
                "Exists" => actual.is_some(),
                "DoesNotExist" => actual.is_none(),
                _ => false,
            };
            if !matches {
                return None;
            }
        }
    }
    let types = policy.pointer("/spec/policyTypes")?.as_array()?;
    let includes = |name| types.iter().any(|v| v.as_str() == Some(name));
    Some((includes("Ingress"), includes("Egress")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn budget() -> FreezeWorkerResources {
        FreezeWorkerResources {
            request_cpu_millicores: 100,
            limit_cpu_millicores: 500,
            request_memory_bytes: 64 * 1024 * 1024,
            limit_memory_bytes: 256 * 1024 * 1024,
        }
    }

    fn quota() -> Value {
        let hard = json!({"requests.cpu":"1","limits.cpu":"1","requests.memory":"512Mi","limits.memory":"512Mi"});
        json!({"spec":{"hard":hard},"status":{"hard":hard,"used":{
            "requests.cpu":"100m","limits.cpu":"500m","requests.memory":"64Mi","limits.memory":"256Mi"}}})
    }

    #[test]
    fn quota_counts_running_work_and_each_worker_dimension() {
        assert_eq!(check_quota(&quota(), &budget()), QuotaCheck::Fits);
        for (name, value) in [
            ("requests.cpu", "950m"),
            ("limits.cpu", "501m"),
            ("requests.memory", "449Mi"),
            ("limits.memory", "257Mi"),
        ] {
            let mut full = quota();
            full["status"]["used"][name] = json!(value);
            assert_eq!(check_quota(&full, &budget()), QuotaCheck::Full, "{name}");
        }
    }

    #[test]
    fn unknown_or_scoped_quota_is_not_zero_usage() {
        let mut unknown = quota();
        unknown["status"]["used"] =
            json!({"requests.cpu":"100m","requests.memory":"64Mi","limits.memory":"256Mi"});
        assert_eq!(check_quota(&unknown, &budget()), QuotaCheck::Unavailable);
        let mut scoped = quota();
        scoped["spec"]["scopes"] = json!(["NotTerminating"]);
        assert_eq!(check_quota(&scoped, &budget()), QuotaCheck::Unavailable);
        let mut drifted = quota();
        drifted["status"]["hard"]["limits.memory"] = json!("1Gi");
        assert_eq!(check_quota(&drifted, &budget()), QuotaCheck::Unavailable);
        let mut pod_full = quota();
        pod_full["spec"]["hard"]["pods"] = json!("1");
        pod_full["status"]["used"]["pods"] = json!("1");
        assert_eq!(check_quota(&pod_full, &budget()), QuotaCheck::Full);
    }

    #[test]
    fn quantities_round_conservatively_and_never_use_float_or_overflow() {
        assert_eq!(quantity("0.5", true, false), Some(500));
        assert_eq!(quantity("0.5Gi", false, false), Some(512 * 1024 * 1024));
        assert_eq!(quantity("0.1m", true, false), Some(0));
        assert_eq!(quantity("0.1m", true, true), Some(1));
        for text in [
            "unknown",
            "-1",
            "1e9",
            "18446744073709551616",
            "0.0000000001",
        ] {
            assert_eq!(quantity(text, true, false), None, "{text}");
        }
    }

    #[test]
    fn resource_limits_cannot_expand_the_previous_worker_ceiling() {
        assert!(budget().valid());
        let mut oversized = budget();
        oversized.limit_cpu_millicores = 1001;
        assert!(!oversized.valid());
        oversized = budget();
        oversized.request_memory_bytes = oversized.limit_memory_bytes + 1;
        assert!(!oversized.valid());
    }

    #[test]
    fn isolation_references_must_select_the_actual_worker_labels() {
        let labels = json!({"app":"freeze"});
        let mut policy = json!({"spec":{"podSelector":{"matchLabels":{"app":"freeze"}},"policyTypes":["Ingress","Egress"]}});
        assert_eq!(selected_policy_types(&policy, &labels), Some((true, true)));
        policy["spec"]["podSelector"]["matchLabels"]["app"] = json!("work");
        assert_eq!(selected_policy_types(&policy, &labels), None);
        policy["spec"]["podSelector"] =
            json!({"matchExpressions":[{"key":"app","operator":"In","values":["freeze"]}]});
        assert_eq!(selected_policy_types(&policy, &labels), Some((true, true)));
    }

    #[test]
    fn dispatch_profile_requires_the_same_reviewed_namespace() {
        let profile = FreezeWorkerProfile::Existing {
            namespace: "synthetic-work".to_owned(),
            secret_name: "synthetic-secret".to_owned(),
            service_account_name: "synthetic-account".to_owned(),
            network_policy_names: vec!["synthetic-isolation".to_owned()],
            resource_quota_names: vec!["synthetic-budget".to_owned()],
            node_name: None,
            resources: budget(),
        };
        assert!(profile.valid());
        assert!(
            FreezeWorkerDispatch::Bound {
                namespace: "synthetic-work".to_owned(),
                profile: profile.clone()
            }
            .valid()
        );
        assert!(
            !FreezeWorkerDispatch::Bound {
                namespace: "another-work".to_owned(),
                profile
            }
            .valid()
        );
    }
}
