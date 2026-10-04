//! Applied launcher quota for the current managed KVM projection and `KubeVirt` 1.8.4.
//! Guest resources and billing remain logical; these bytes belong to the execution backend.

use serde_json::Value;

use crate::cdi_import::storage_matches;

const MI: u64 = 1 << 20;

#[derive(Debug, thiserror::Error)]
pub(crate) enum LauncherSizingError {
    #[error("unsupported KubeVirt launcher configuration")]
    Configuration,
    #[error("invalid managed launcher resource intent")]
    Intent,
    #[error("launcher resource arithmetic overflow")]
    Overflow,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct LauncherQuota {
    pub memory_request: u64,
    pub memory_limit: u64,
    pub cpu_request_millicores: u64,
    pub cpu_limit_millicores: u64,
}

pub(crate) fn launcher_quota(
    vm: &Value,
    quota: &Value,
    kubevirts: &Value,
) -> Result<LauncherQuota, LauncherSizingError> {
    let config = effective_configuration(kubevirts)?;
    if vm
        .pointer("/spec/template/metadata/annotations/hooks.kubevirt.io~1hookSidecars")
        .is_some_and(|value| !value.is_null())
    {
        return Err(LauncherSizingError::Intent);
    }
    let spec = vm
        .pointer("/spec/template/spec")
        .ok_or(LauncherSizingError::Intent)?;
    validate_managed_shape(spec)?;
    let request = positive(vm, "/spec/template/spec/domain/resources/requests/memory")?;
    let limit = positive(vm, "/spec/template/spec/domain/resources/limits/memory")?;
    let cpu_request = cpu(vm, "/spec/template/spec/domain/resources/requests/cpu")?;
    let cpu_limit = cpu(vm, "/spec/template/spec/domain/resources/limits/cpu")?;
    let headroom = annotation(quota, "vmi-memory-overhead-bytes")?;
    let cdi_request = annotation(quota, "cdi-importer-memory-request-bytes")?;
    let cdi_limit = annotation(quota, "cdi-importer-memory-limit-bytes")?;
    let cdi_cpu_request = annotation(quota, "cdi-importer-cpu-request-millicores")?;
    let cdi_cpu_limit = annotation(quota, "cdi-importer-cpu-limit-millicores")?;
    if add(request, headroom)? != limit
        || cpu_request != cpu_limit
        || spec
            .pointer("/domain/memory/guest")
            .is_some_and(|v| !v.as_str().is_some_and(|s| storage_matches(s, request)))
        || cdi_limit < cdi_request
        || cdi_cpu_limit < cdi_cpu_request
        || !memory_matches(quota, "requests.memory", add(limit, cdi_request)?)
        || !memory_matches(quota, "limits.memory", add(limit, cdi_limit)?)
        || cpu(quota, "/spec/hard/requests.cpu")? != add(cpu_request, cdi_cpu_request)?
        || cpu(quota, "/spec/hard/limits.cpu")? != add(cpu_limit, cdi_cpu_limit)?
    {
        return Err(LauncherSizingError::Intent);
    }
    let overhead = memory_overhead(spec, request, cpu_limit)?;
    let console = console_enabled(spec, config)?;
    validate_console_resources(config, console)?;
    Ok(LauncherQuota {
        memory_request: add(
            add(
                add(request, overhead)?,
                if console { 35_000_000 } else { 0 },
            )?,
            cdi_request,
        )?,
        memory_limit: add(
            add(add(limit, overhead)?, if console { 60_000_000 } else { 0 })?,
            cdi_limit,
        )?,
        cpu_request_millicores: add(
            add(cpu_request, if console { 5 } else { 0 })?,
            cdi_cpu_request,
        )?,
        cpu_limit_millicores: add(add(cpu_limit, if console { 15 } else { 0 })?, cdi_cpu_limit)?,
    })
}

fn effective_configuration(list: &Value) -> Result<&Value, LauncherSizingError> {
    let items = list
        .get("items")
        .and_then(Value::as_array)
        .ok_or(LauncherSizingError::Configuration)?;
    if items.len() != 1
        || list
            .pointer("/metadata/continue")
            .is_some_and(|value| value != "" && !value.is_null())
    {
        return Err(LauncherSizingError::Configuration);
    }
    let instance = items.first().ok_or(LauncherSizingError::Configuration)?;
    if instance
        .pointer("/status/observedKubeVirtVersion")
        .and_then(Value::as_str)
        != Some("v1.8.4")
        || instance
            .pointer("/status/targetKubeVirtVersion")
            .and_then(Value::as_str)
            != Some("v1.8.4")
    {
        return Err(LauncherSizingError::Configuration);
    }
    let config = instance
        .pointer("/spec/configuration")
        .filter(|value| value.is_object())
        .ok_or(LauncherSizingError::Configuration)?;
    if config
        .get("virtualMachineOptions")
        .is_some_and(|value| !value.is_null() && !value.is_object())
    {
        return Err(LauncherSizingError::Configuration);
    }
    // This managed implementation supports the current default ratio, not a guessed
    // floating-point approximation of a different deployment policy.
    match config.get("additionalGuestMemoryOverheadRatio") {
        None | Some(Value::Null) => {}
        Some(Value::String(value))
            if value.is_empty() || value.parse::<f64>().ok() == Some(1.0) => {}
        _ => return Err(LauncherSizingError::Configuration),
    }
    match config.get("hypervisors") {
        None | Some(Value::Null) => {}
        Some(Value::Array(values))
            if values.is_empty()
                || (values.len() == 1
                    && values
                        .first()
                        .and_then(|v| v.get("name"))
                        .and_then(Value::as_str)
                        == Some("kvm")) => {}
        _ => return Err(LauncherSizingError::Configuration),
    }
    Ok(config)
}

fn validate_managed_shape(spec: &Value) -> Result<(), LauncherSizingError> {
    if spec.get("architecture").and_then(Value::as_str) != Some("amd64")
        || [
            "/domain/memory/hugepages",
            "/domain/memory/reservedOverhead",
            "/domain/launchSecurity",
            "/domain/devices/tpm",
            "/domain/ioThreads",
            "/domain/ioThreadsPolicy",
            "/readinessProbe",
            "/livenessProbe",
        ]
        .iter()
        .any(|path| spec.pointer(path).is_some_and(|value| !value.is_null()))
        || optional_bool(spec, "/domain/cpu/dedicatedCpuPlacement", false)?
        || optional_bool(spec, "/domain/resources/overcommitGuestOverhead", false)?
        || spec
            .pointer("/domain/devices/filesystems")
            .is_some_and(|value| value.as_array().is_none_or(|values| !values.is_empty()))
        || spec
            .pointer("/domain/devices/interfaces")
            .and_then(Value::as_array)
            .is_none_or(|values| {
                values
                    .iter()
                    .any(|v| v.get("binding").is_some() || v.get("sriov").is_some())
            })
        || spec
            .get("volumes")
            .and_then(Value::as_array)
            .is_none_or(|values| {
                values
                    .iter()
                    .any(|v| v.get("containerDisk").is_some() || v.get("downwardMetrics").is_some())
            })
        || spec
            .pointer("/domain/firmware/kernelBoot")
            .is_some_and(|value| !value.is_null())
    {
        return Err(LauncherSizingError::Intent);
    }
    Ok(())
}

fn memory_overhead(spec: &Value, request: u64, cpu_limit: u64) -> Result<u64, LauncherSizingError> {
    // Go Quantity.ScaledValue(Kilo) rounds up before NewScaledQuantity and /512.
    let kilo_bytes = request
        .div_ceil(1000)
        .checked_mul(1000)
        .filter(|v| i64::try_from(*v).is_ok())
        .ok_or(LauncherSizingError::Overflow)?;
    let mut overhead = add(kilo_bytes / 512, 220 * MI)?;
    let cpus = match spec.pointer("/domain/cpu") {
        None | Some(Value::Null) => cpu_limit.div_ceil(1000),
        Some(topology) => {
            let count = ["cores", "sockets", "threads"]
                .iter()
                .try_fold(1_u64, |count, key| {
                    let value = topology
                        .get(key)
                        .and_then(Value::as_u64)
                        .filter(|v| *v > 0)
                        .ok_or(LauncherSizingError::Intent)?;
                    count
                        .checked_mul(value)
                        .ok_or(LauncherSizingError::Overflow)
                })?;
            if count != cpu_limit.div_ceil(1000) {
                return Err(LauncherSizingError::Intent);
            }
            count
        }
    };
    overhead = add(
        overhead,
        cpus.checked_mul(8 * MI)
            .ok_or(LauncherSizingError::Overflow)?,
    )?;
    overhead = add(overhead, 8 * MI)?;
    if optional_bool(spec, "/domain/devices/autoattachGraphicsDevice", true)? {
        overhead = add(overhead, 32 * MI)?;
    }
    let mut vfio = false;
    for name in ["gpus", "hostDevices"] {
        if let Some(value) = spec.pointer(&format!("/domain/devices/{name}")) {
            vfio |= !value
                .as_array()
                .ok_or(LauncherSizingError::Intent)?
                .is_empty();
        }
    }
    if vfio {
        overhead = add(overhead, 1 << 30)?;
    }
    Ok(overhead)
}

fn console_enabled(spec: &Value, config: &Value) -> Result<bool, LauncherSizingError> {
    Ok(
        optional_bool(spec, "/domain/devices/autoattachSerialConsole", true)?
            && optional_bool(
                spec,
                "/domain/devices/logSerialConsole",
                !optional_bool(
                    config,
                    "/virtualMachineOptions/disableSerialConsoleLog",
                    false,
                )?,
            )?,
    )
}

fn validate_console_resources(config: &Value, enabled: bool) -> Result<(), LauncherSizingError> {
    let entries = match config.get("supportContainerResources") {
        None | Some(Value::Null) => return Ok(()),
        Some(Value::Array(entries)) => entries,
        _ => return Err(LauncherSizingError::Configuration),
    };
    let mut seen = false;
    for entry in entries {
        let kind = entry
            .get("type")
            .and_then(Value::as_str)
            .ok_or(LauncherSizingError::Configuration)?;
        if kind != "guest-console-log" || !enabled {
            continue;
        }
        if seen {
            return Err(LauncherSizingError::Configuration);
        }
        seen = true;
        let resources = entry
            .get("resources")
            .and_then(Value::as_object)
            .ok_or(LauncherSizingError::Configuration)?;
        if ["requests", "limits"].iter().any(|key| {
            resources
                .get(*key)
                .is_some_and(|value| !value.is_null() && !value.is_object())
        }) {
            return Err(LauncherSizingError::Configuration);
        }
        for (path, accepted) in [
            ("/resources/requests/memory", ["35M", "35000000"]),
            ("/resources/limits/memory", ["60M", "60000000"]),
            ("/resources/requests/cpu", ["5m", "5m"]),
            ("/resources/limits/cpu", ["15m", "15m"]),
        ] {
            if let Some(value) = entry.pointer(path)
                && !value
                    .as_str()
                    .is_some_and(|value| accepted.contains(&value))
            {
                return Err(LauncherSizingError::Configuration);
            }
        }
    }
    Ok(())
}

fn optional_bool(value: &Value, path: &str, default: bool) -> Result<bool, LauncherSizingError> {
    match value.pointer(path) {
        None | Some(Value::Null) => Ok(default),
        Some(Value::Bool(value)) => Ok(*value),
        _ => Err(LauncherSizingError::Intent),
    }
}

fn positive(value: &Value, path: &str) -> Result<u64, LauncherSizingError> {
    value
        .pointer(path)
        .and_then(Value::as_str)
        .and_then(|v| v.parse().ok())
        .filter(|v| *v > 0 && i64::try_from(*v).is_ok())
        .ok_or(LauncherSizingError::Intent)
}
fn cpu(value: &Value, path: &str) -> Result<u64, LauncherSizingError> {
    let text = value
        .pointer(path)
        .and_then(Value::as_str)
        .ok_or(LauncherSizingError::Intent)?;
    let result = if let Some(text) = text.strip_suffix('m') {
        text.parse::<u64>().ok()
    } else {
        text.parse::<u64>().ok().and_then(|v| v.checked_mul(1000))
    };
    result
        .filter(|v| *v > 0 && i64::try_from(*v).is_ok())
        .ok_or(LauncherSizingError::Intent)
}
pub(crate) fn cpu_matches(value: &str, expected: u64) -> bool {
    cpu(&Value::String(value.to_owned()), "").is_ok_and(|actual| actual == expected)
}
fn annotation(quota: &Value, name: &str) -> Result<u64, LauncherSizingError> {
    positive(
        quota,
        &format!("/metadata/annotations/labweaver.io~1{name}"),
    )
}
fn memory_matches(quota: &Value, name: &str, bytes: u64) -> bool {
    quota
        .pointer(&format!("/spec/hard/{name}"))
        .and_then(Value::as_str)
        .is_some_and(|v| storage_matches(v, bytes))
}
fn add(left: u64, right: u64) -> Result<u64, LauncherSizingError> {
    left.checked_add(right)
        .filter(|v| i64::try_from(*v).is_ok())
        .ok_or(LauncherSizingError::Overflow)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn documents() -> (Value, Value, Value) {
        let vm = json!({"spec":{"template":{"spec":{"architecture":"amd64",
            "domain":{"resources":{"requests":{"memory":"2147483648","cpu":"1"},"limits":{"memory":"2684354560","cpu":"1"}},
                "devices":{"gpus":[{"deviceName":"nvidia.com/GRID_V100DX-2Q"}],"interfaces":[{"masquerade":{}}]}},"volumes":[]}}}});
        let quota = json!({"metadata":{"annotations":{
            "labweaver.io/vmi-memory-overhead-bytes":"536870912",
            "labweaver.io/cdi-importer-memory-request-bytes":"262144000",
            "labweaver.io/cdi-importer-memory-limit-bytes":"1073741824",
            "labweaver.io/cdi-importer-cpu-request-millicores":"1000",
            "labweaver.io/cdi-importer-cpu-limit-millicores":"4000"}},
            "spec":{"hard":{"requests.memory":"2946498560","limits.memory":"3758096384","requests.cpu":"2","limits.cpu":"5"}}});
        let cr = json!({"items":[{"spec":{"configuration":{}},"status":{"observedKubeVirtVersion":"v1.8.4","targetKubeVirtVersion":"v1.8.4"}}]});
        (vm, quota, cr)
    }

    #[test]
    fn refused_pod_is_covered_without_changing_guest_intent()
    -> Result<(), Box<dyn std::error::Error>> {
        let (vm, quota, cr) = documents();
        let before = vm.clone();
        let result = launcher_quota(&vm, &quota, &cr)?;
        assert_eq!(
            result,
            LauncherQuota {
                memory_request: 3_803_582_144,
                memory_limit: 5_177_050_880,
                cpu_request_millicores: 2005,
                cpu_limit_millicores: 5015
            }
        );
        assert_eq!(result.memory_request - 262_144_000, 3_541_438_144);
        assert_eq!(result.memory_limit - 1_073_741_824, 4_103_309_056);
        assert_eq!(vm, before);
        Ok(())
    }

    #[test]
    fn cpu_graphics_and_vfio_are_accounted_once() -> Result<(), Box<dyn std::error::Error>> {
        let (mut vm, mut quota, cr) = documents();
        let gpu = launcher_quota(&vm, &quota, &cr)?;
        vm["spec"]["template"]["spec"]["domain"]["devices"]["gpus"] = json!([]);
        let cpu_only = launcher_quota(&vm, &quota, &cr)?;
        assert_eq!(cpu_only.memory_request, gpu.memory_request - (1 << 30));
        vm["spec"]["template"]["spec"]["domain"]["devices"]["gpus"] = json!([{}, {}, {}, {}]);
        assert_eq!(launcher_quota(&vm, &quota, &cr)?, gpu);
        vm["spec"]["template"]["spec"]["domain"]["resources"]["requests"]["cpu"] = json!("4");
        vm["spec"]["template"]["spec"]["domain"]["resources"]["limits"]["cpu"] = json!("4");
        vm["spec"]["template"]["spec"]["domain"]["cpu"] =
            json!({"cores":2,"sockets":2,"threads":1});
        quota["spec"]["hard"]["requests.cpu"] = json!("5");
        quota["spec"]["hard"]["limits.cpu"] = json!("8");
        let four = launcher_quota(&vm, &quota, &cr)?;
        assert_eq!(four.memory_request, gpu.memory_request + 24 * MI);
        assert_eq!(four.cpu_request_millicores, 5005);
        vm["spec"]["template"]["spec"]["domain"]["devices"]["autoattachGraphicsDevice"] =
            json!(false);
        assert_eq!(
            launcher_quota(&vm, &quota, &cr)?.memory_limit,
            four.memory_limit - 32 * MI
        );
        Ok(())
    }

    #[test]
    fn console_disable_and_vm_override_follow_the_authoritative_cr()
    -> Result<(), Box<dyn std::error::Error>> {
        let (mut vm, quota, mut cr) = documents();
        let enabled = launcher_quota(&vm, &quota, &cr)?;
        cr["items"][0]["spec"]["configuration"]["virtualMachineOptions"] =
            json!({"disableSerialConsoleLog":true});
        let disabled = launcher_quota(&vm, &quota, &cr)?;
        assert_eq!(disabled.memory_request, enabled.memory_request - 35_000_000);
        assert_eq!(disabled.memory_limit, enabled.memory_limit - 60_000_000);
        assert_eq!(disabled.cpu_request_millicores, 2000);
        assert_eq!(disabled.cpu_limit_millicores, 5000);
        vm["spec"]["template"]["spec"]["domain"]["devices"]["logSerialConsole"] = json!(true);
        assert_eq!(launcher_quota(&vm, &quota, &cr)?, enabled);
        vm["spec"]["template"]["spec"]["domain"]["devices"]["autoattachSerialConsole"] =
            json!(false);
        assert_eq!(launcher_quota(&vm, &quota, &cr)?, disabled);
        Ok(())
    }

    #[test]
    fn unknown_authority_or_nondefault_sizing_policy_is_rejected() {
        let (vm, quota, cr) = documents();
        for (path, value) in [
            ("/items", json!([])),
            ("/items", json!([cr["items"][0], cr["items"][0]])),
            ("/items/0/status/targetKubeVirtVersion", json!("v1.8.5")),
            ("/items/0/spec/configuration", Value::Null),
        ] {
            let mut invalid = cr.clone();
            if let Some(slot) = invalid.pointer_mut(path) {
                *slot = value;
            }
            assert!(
                launcher_quota(&vm, &quota, &invalid).is_err(),
                "must reject {path}"
            );
        }
        for config in [
            json!({"additionalGuestMemoryOverheadRatio":"NaN"}),
            json!({"additionalGuestMemoryOverheadRatio":"1.2"}),
            json!({"hypervisors":[{"name":"hyperv-direct"}]}),
            json!({"supportContainerResources":[{"type":"guest-console-log","resources":{"requests":{"memory":"80M"}}}]}),
            json!({"supportContainerResources":{}}),
        ] {
            let mut invalid = cr.clone();
            invalid["items"][0]["spec"]["configuration"] = config;
            assert!(launcher_quota(&vm, &quota, &invalid).is_err());
        }
        let mut truncated = cr.clone();
        truncated["metadata"] = json!({"continue":"more"});
        assert!(launcher_quota(&vm, &quota, &truncated).is_err());
        let mut unrelated = cr;
        unrelated["items"][0]["spec"]["configuration"] = json!({"additionalGuestMemoryOverheadRatio":"1.000", "developerConfiguration":{"featureGates":["LiveMigration"]}, "autoMemoryLimits":true, "supportContainerResources":[{"type":"vmexport","resources":{"requests":{"memory":"90M"}}}]});
        assert!(launcher_quota(&vm, &quota, &unrelated).is_ok());
    }

    #[test]
    fn unmanaged_architecture_and_extra_launcher_work_are_rejected()
    -> Result<(), Box<dyn std::error::Error>> {
        let (vm, quota, cr) = documents();
        for spec_change in [
            json!({"architecture":"arm64"}),
            json!({"readinessProbe":{"exec":{"command":["check"]}}}),
            json!({"domain":{"devices":{"tpm":{}}}}),
            json!({"domain":{"devices":{"interfaces":[{"sriov":{}}]}}}),
        ] {
            let mut unsupported = vm.clone();
            for (key, value) in spec_change
                .as_object()
                .ok_or("fixture spec object missing")?
            {
                unsupported["spec"]["template"]["spec"][key] = value.clone();
            }
            assert!(launcher_quota(&unsupported, &quota, &cr).is_err());
        }
        let mut hooks = vm;
        hooks["spec"]["template"]["metadata"] = json!({"annotations":{
            "hooks.kubevirt.io/hookSidecars":"[]"}});
        assert!(launcher_quota(&hooks, &quota, &cr).is_err());
        Ok(())
    }

    #[test]
    fn malformed_and_overflowing_intent_never_authorizes_extra_quota() {
        let (vm, quota, cr) = documents();
        for (path, value) in [
            (
                "/spec/template/spec/domain/resources/requests/memory",
                json!("not-a-number"),
            ),
            (
                "/spec/template/spec/domain/resources/limits/memory",
                Value::Null,
            ),
            (
                "/spec/template/spec/domain/resources/requests/cpu",
                json!("18446744073709551615"),
            ),
            (
                "/spec/template/spec/domain/resources/overcommitGuestOverhead",
                json!(true),
            ),
            (
                "/spec/template/spec/domain/cpu",
                json!({"cores":u64::MAX,"threads":2,"sockets":2}),
            ),
        ] {
            let mut invalid = vm.clone();
            if let Some(slot) = invalid.pointer_mut(path) {
                *slot = value;
            } else if path.ends_with("/cpu") {
                invalid["spec"]["template"]["spec"]["domain"]["cpu"] = value;
            } else {
                invalid["spec"]["template"]["spec"]["domain"]["resources"]["overcommitGuestOverhead"] =
                    value;
            }
            assert!(
                launcher_quota(&invalid, &quota, &cr).is_err(),
                "must reject {path}"
            );
        }
        assert!(add(i64::MAX as u64, 1).is_err());
        assert!(memory_overhead(&vm["spec"]["template"]["spec"], i64::MAX as u64, 1000).is_err());
    }
}
