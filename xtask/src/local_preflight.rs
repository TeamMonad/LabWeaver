//! Read-only capability discovery for Docker Desktop local validation.

use std::fs;
use std::path::Path;
use std::process::Command;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::AppError;

const PROFILE: &str = "local-hostpath";
const REPORT_SCHEMA: &str = "local-connected-non-release.v1";

pub(crate) fn run(root: &Path, profile: &str) -> Result<(), AppError> {
    run_with_identity(root, profile, None)
}

/// Run the read-only local probe and attach a sanitized replay identity when
/// the caller has already validated a Resource replay input set. The identity
/// is deliberately JSON-shaped so this module cannot accidentally gain access
/// to credentials or private payloads.
#[allow(
    clippy::too_many_lines,
    reason = "the local preflight boundary assembles one complete sanitized report"
)]
pub(crate) fn run_with_identity(
    root: &Path,
    profile: &str,
    replay_identity: Option<Value>,
) -> Result<(), AppError> {
    if profile != PROFILE {
        return Err(AppError::InvalidArgument {
            role: "local preflight profile",
        });
    }

    let source_commit = git_commit(root)?;
    let run_id = replay_identity
        .as_ref()
        .and_then(|value| value.get("runId"))
        .and_then(Value::as_str)
        .map(|value| {
            Uuid::parse_str(value).map_err(|error| AppError::ReleaseGate {
                code: "LW_LOCAL_REPLAY_IDENTITY_INVALID",
                detail: format!("local replay runId is invalid: {error}"),
            })
        })
        .transpose()?
        .unwrap_or_else(Uuid::now_v7);
    let mut blockers = Vec::new();
    let (docker_context, kubernetes_context) = probe_contexts(&mut blockers);
    let cluster = probe_cluster(&mut blockers);
    let ecnu_configured = probe_ecnu(root, &mut blockers)?;
    let hostpath = cluster
        .storage_classes
        .iter()
        .any(|name| name == "hostpath");
    let nfs_rwx = cluster.storage_classes.iter().any(|name| name == "nfs-rwx");
    let kubevirt = cluster
        .crd_names
        .iter()
        .any(|name| name == "kubevirts.kubevirt.io" || name == "virtualmachines.kubevirt.io");
    let cdi = cluster
        .crd_names
        .iter()
        .any(|name| name == "datavolumes.cdi.kubevirt.io" || name == "cdis.cdi.kubevirt.io");
    let capability_gaps = blockers
        .iter()
        .filter(|code| {
            matches!(
                code.as_str(),
                "LW_LOCAL_PREFLIGHT_NFS_RWX_UNAVAILABLE"
                    | "LW_LOCAL_PREFLIGHT_KUBEVIRT_UNAVAILABLE"
                    | "LW_LOCAL_PREFLIGHT_CDI_UNAVAILABLE"
                    | "LW_LOCAL_PREFLIGHT_HOSTPATH_UNAVAILABLE"
                    | "LW_LOCAL_PREFLIGHT_SINGLE_READY_NODE_REQUIRED"
                    | "LW_LOCAL_PREFLIGHT_DOCKER_CONTEXT_INVALID"
                    | "LW_LOCAL_PREFLIGHT_KUBERNETES_CONTEXT_INVALID"
                    | "LW_LOCAL_PREFLIGHT_ECNU_KEY_MISSING"
            )
        })
        .cloned()
        .collect::<Vec<_>>();
    let identity = replay_identity
        .map(|mut value| {
            let object = value.as_object_mut().ok_or(AppError::ReleaseGate {
                code: "LW_LOCAL_REPLAY_IDENTITY_INVALID",
                detail: "local replay identity must be a JSON object".to_owned(),
            })?;
            object.insert("sourceCommit".to_owned(), json!(source_commit));
            object.insert("runId".to_owned(), json!(run_id));
            Ok::<Value, AppError>(value)
        })
        .transpose()?
        .unwrap_or_else(|| {
            json!({
                "kind": "local-preflight",
                "sourceCommit": source_commit,
                "runId": run_id,
            })
        });

    let report = json!({
        "schemaVersion": REPORT_SCHEMA,
        "mode": PROFILE,
        "releaseEligible": false,
        "sourceCommit": source_commit,
        "runId": run_id,
        "dockerContext": docker_context,
        "kubernetesContext": kubernetes_context,
        "nodeCount": cluster.node_count,
        "readyNodeCount": cluster.ready_node_count,
        "storageClasses": cluster.storage_classes,
        "capabilities": {
            "singleReadyNode": cluster.node_count == Some(1) && cluster.ready_node_count == Some(1),
            "hostpath": hostpath,
            "nfsRwx": nfs_rwx,
            "kubevirt": kubevirt,
            "cdi": cdi,
            "ecnuApiKey": ecnu_configured,
        },
        "capabilityGaps": capability_gaps,
        "blockers": blockers,
        "identity": identity,
    });
    let report_bytes = serde_json::to_vec_pretty(&report).map_err(|error| AppError::Io {
        role: "serialize local preflight report",
        detail: error.to_string(),
    })?;
    let report_path = root
        .join("artifacts/local-replay")
        .join(format!("local-connected-non-release-{run_id}.json"));
    let Some(report_parent) = report_path.parent() else {
        return Err(AppError::Io {
            role: "resolve local preflight report directory",
            detail: "report path has no parent".to_owned(),
        });
    };
    fs::create_dir_all(report_parent).map_err(|error| AppError::Io {
        role: "create local preflight report directory",
        detail: error.to_string(),
    })?;
    fs::write(&report_path, report_bytes).map_err(|error| AppError::Io {
        role: "write local preflight report",
        detail: error.to_string(),
    })?;
    println!("{}", relative_path(root, &report_path));

    if blockers.is_empty() {
        Ok(())
    } else {
        Err(AppError::Integration {
            code: "LW_LOCAL_PREFLIGHT_CAPABILITY_BLOCKED",
            detail: format!(
                "local-hostpath preflight recorded {} blocker(s)",
                blockers.len()
            ),
        })
    }
}

pub(crate) fn file_identity(root: &Path, path: &Path) -> Result<Value, AppError> {
    let canonical_root = root.canonicalize().map_err(|error| AppError::Io {
        role: "resolve local replay repository root",
        detail: error.to_string(),
    })?;
    let canonical = path.canonicalize().map_err(|error| AppError::Io {
        role: "resolve local replay identity locator",
        detail: error.to_string(),
    })?;
    let relative = canonical
        .strip_prefix(&canonical_root)
        .map_err(|_| AppError::ReleaseGate {
            code: "LW_LOCAL_REPLAY_IDENTITY_LOCATOR_OUTSIDE_REPOSITORY",
            detail: "local replay identity locators must be relative to the repository".to_owned(),
        })?;
    if relative.components().any(|component| {
        matches!(
            component,
            std::path::Component::ParentDir | std::path::Component::RootDir
        )
    }) {
        return Err(AppError::ReleaseGate {
            code: "LW_LOCAL_REPLAY_IDENTITY_LOCATOR_INVALID",
            detail: "local replay identity locator must not escape the repository".to_owned(),
        });
    }
    let bytes = fs::read(&canonical).map_err(|error| AppError::Io {
        role: "hash local replay identity locator",
        detail: error.to_string(),
    })?;
    Ok(json!({
        "path": relative.to_string_lossy().replace('\\', "/"),
        "sha256": format!("sha256:{:x}", Sha256::digest(bytes)),
    }))
}

pub(crate) fn resource_image_reference(package_manifest: &Path) -> Result<String, AppError> {
    let bytes = fs::read(package_manifest).map_err(|error| AppError::Io {
        role: "read local Resource package manifest",
        detail: error.to_string(),
    })?;
    let manifest: Value =
        serde_json::from_slice(&bytes).map_err(|error| AppError::ReleaseGate {
            code: "LW_LOCAL_REPLAY_PACKAGE_MANIFEST_INVALID",
            detail: error.to_string(),
        })?;
    manifest
        .pointer("/images/0/reference")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or(AppError::ReleaseGate {
            code: "LW_LOCAL_REPLAY_PACKAGE_MANIFEST_INVALID",
            detail: "Resource package manifest has no immutable image reference".to_owned(),
        })
}

#[derive(Debug, Default)]
struct ClusterProbe {
    node_count: Option<usize>,
    ready_node_count: Option<usize>,
    storage_classes: Vec<String>,
    crd_names: Vec<String>,
}

fn probe_contexts(blockers: &mut Vec<String>) -> (String, String) {
    let docker_context = read_text("docker", &["context", "show"]).unwrap_or_else(|code| {
        blockers.push(code);
        String::new()
    });
    if docker_context != "desktop-linux" {
        blockers.push("LW_LOCAL_PREFLIGHT_DOCKER_CONTEXT_INVALID".to_owned());
    }
    let kubernetes_context =
        read_text("kubectl", &["config", "current-context"]).unwrap_or_else(|code| {
            blockers.push(code);
            String::new()
        });
    if kubernetes_context != "docker-desktop" {
        blockers.push("LW_LOCAL_PREFLIGHT_KUBERNETES_CONTEXT_INVALID".to_owned());
    }
    (docker_context, kubernetes_context)
}

fn probe_cluster(blockers: &mut Vec<String>) -> ClusterProbe {
    let nodes = read_json("kubectl", &["get", "nodes", "-o", "json"]).unwrap_or_else(|code| {
        blockers.push(code);
        Value::Null
    });
    let node_count = item_count(&nodes);
    let ready_node_count = ready_node_count(&nodes);
    let single_ready_node = node_count == Some(1) && ready_node_count == Some(1);
    if !single_ready_node {
        blockers.push("LW_LOCAL_PREFLIGHT_SINGLE_READY_NODE_REQUIRED".to_owned());
    }

    let storage_classes = match read_json("kubectl", &["get", "storageclass", "-o", "json"]) {
        Ok(value) => names(&value),
        Err(code) => {
            blockers.push(code);
            Vec::new()
        }
    };
    let hostpath = storage_classes.iter().any(|name| name == "hostpath");
    let nfs_rwx = storage_classes.iter().any(|name| name == "nfs-rwx");
    if !hostpath {
        blockers.push("LW_LOCAL_PREFLIGHT_HOSTPATH_UNAVAILABLE".to_owned());
    }
    if !nfs_rwx {
        blockers.push("LW_LOCAL_PREFLIGHT_NFS_RWX_UNAVAILABLE".to_owned());
    }

    let crds = read_json("kubectl", &["get", "crd", "-o", "json"]).unwrap_or_else(|code| {
        blockers.push(code);
        Value::Null
    });
    let crd_names = names(&crds);
    let kubevirt = crd_names
        .iter()
        .any(|name| name == "kubevirts.kubevirt.io" || name == "virtualmachines.kubevirt.io");
    let cdi = crd_names
        .iter()
        .any(|name| name == "datavolumes.cdi.kubevirt.io" || name == "cdis.cdi.kubevirt.io");
    if !kubevirt {
        blockers.push("LW_LOCAL_PREFLIGHT_KUBEVIRT_UNAVAILABLE".to_owned());
    }
    if !cdi {
        blockers.push("LW_LOCAL_PREFLIGHT_CDI_UNAVAILABLE".to_owned());
    }
    ClusterProbe {
        node_count,
        ready_node_count,
        storage_classes,
        crd_names,
    }
}

fn probe_ecnu(root: &Path, blockers: &mut Vec<String>) -> Result<bool, AppError> {
    let env_path = std::env::var_os("LABWEAVER_LOCAL_ECNU_ENV_FILE")
        .map_or_else(|| root.join(".env"), std::path::PathBuf::from);
    let configured =
        env_path.is_file() && env_file_contains_nonempty_key(&env_path, "ECNU_API_KEY")?;
    if !configured {
        blockers.push("LW_LOCAL_PREFLIGHT_ECNU_KEY_MISSING".to_owned());
    }
    Ok(configured)
}

fn read_text(program: &str, args: &[&str]) -> Result<String, String> {
    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|_| "LW_LOCAL_PREFLIGHT_TOOL_UNAVAILABLE".to_owned())?;
    if !output.status.success() {
        return Err("LW_LOCAL_PREFLIGHT_READONLY_PROBE_FAILED".to_owned());
    }
    String::from_utf8(output.stdout)
        .map(|value| value.trim().to_owned())
        .map_err(|_| "LW_LOCAL_PREFLIGHT_READONLY_PROBE_INVALID".to_owned())
}

fn read_json(program: &str, args: &[&str]) -> Result<Value, String> {
    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|_| "LW_LOCAL_PREFLIGHT_TOOL_UNAVAILABLE".to_owned())?;
    if !output.status.success() {
        return Err("LW_LOCAL_PREFLIGHT_READONLY_PROBE_FAILED".to_owned());
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|_| "LW_LOCAL_PREFLIGHT_READONLY_PROBE_INVALID".to_owned())
}

fn item_count(value: &Value) -> Option<usize> {
    value.get("items").and_then(Value::as_array).map(Vec::len)
}

fn names(value: &Value) -> Vec<String> {
    value
        .get("items")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| item.pointer("/metadata/name").and_then(Value::as_str))
        .map(str::to_owned)
        .collect()
}

fn ready_node_count(value: &Value) -> Option<usize> {
    let items = value.get("items")?.as_array()?;
    Some(
        items
            .iter()
            .filter(|node| {
                node.pointer("/status/conditions")
                    .and_then(Value::as_array)
                    .is_some_and(|conditions| {
                        conditions.iter().any(|condition| {
                            condition.get("type").and_then(Value::as_str) == Some("Ready")
                                && condition.get("status").and_then(Value::as_str) == Some("True")
                        })
                    })
            })
            .count(),
    )
}

fn env_file_contains_nonempty_key(path: &Path, key: &str) -> Result<bool, AppError> {
    let bytes = fs::read(path).map_err(|error| AppError::Io {
        role: "read private ECNU environment locator",
        detail: error.to_string(),
    })?;
    for line in String::from_utf8_lossy(&bytes).lines() {
        let Some((name, value)) = line.split_once('=') else {
            continue;
        };
        if name.trim() == key {
            return Ok(!value.trim().is_empty() && !value.trim_start().starts_with('#'));
        }
    }
    Ok(false)
}

fn git_commit(root: &Path) -> Result<String, AppError> {
    let output = Command::new("git")
        .current_dir(root)
        .args(["rev-parse", "HEAD"])
        .output()
        .map_err(|error| AppError::Io {
            role: "read local preflight source identity",
            detail: error.to_string(),
        })?;
    if !output.status.success() {
        return Err(AppError::ExternalCommand {
            role: "read local preflight source identity",
            code: output.status.code(),
            detail: None,
        });
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if value.len() != 40 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(AppError::ReleaseGate {
            code: "LW_LOCAL_PREFLIGHT_SOURCE_IDENTITY_INVALID",
            detail: "Git HEAD is not a full hexadecimal commit".to_owned(),
        });
    }
    Ok(value)
}

fn relative_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::{file_identity, item_count, names, ready_node_count};
    use serde_json::json;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn capability_probe_counts_only_ready_nodes() {
        let value = json!({
            "items": [
                {"metadata": {"name": "docker-desktop"}, "status": {"conditions": [{"type": "Ready", "status": "True"}]}},
                {"metadata": {"name": "not-ready"}, "status": {"conditions": [{"type": "Ready", "status": "False"}]}}
            ]
        });
        assert_eq!(item_count(&value), Some(2));
        assert_eq!(ready_node_count(&value), Some(1));
        assert_eq!(names(&value), vec!["docker-desktop", "not-ready"]);
    }

    #[test]
    fn file_identity_accepts_a_noncanonical_repository_root() -> Result<(), String> {
        let temporary = tempdir().map_err(|error| error.to_string())?;
        let repository = temporary.path().join("repo");
        fs::create_dir(&repository).map_err(|error| error.to_string())?;
        let locator = repository.join(".private").join("locator.json");
        let parent = locator
            .parent()
            .ok_or_else(|| "locator parent is missing".to_owned())?;
        fs::create_dir(parent).map_err(|error| error.to_string())?;
        fs::write(&locator, b"synthetic locator").map_err(|error| error.to_string())?;
        let aliased_root = temporary.path().join("repo").join("..").join("repo");
        let identity =
            file_identity(&aliased_root, &locator).map_err(|error| format!("{error:?}"))?;
        assert_eq!(identity["path"], ".private/locator.json");
        assert!(
            identity["sha256"]
                .as_str()
                .is_some_and(|value| value.starts_with("sha256:"))
        );
        Ok(())
    }
}
