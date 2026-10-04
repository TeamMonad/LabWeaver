//! Shell-free Ansible probe worker executed only inside the isolated probe Kubernetes Job.
//!
//! The worker binds the stage-one data semantics to one frozen playbook profile:
//! it validates the mounted short-lived SSH identity at consumption time
//! (`IdentityExpired` covers an expired, not-yet-valid, over-long-lived, or
//! key-mismatched certificate), pins the target host key through a russh
//! handshake before writing `known_hosts`, runs the fixed `ansible-playbook`
//! with a scrubbed environment, and reduces the `ansible.posix.json` callback
//! output to typed facts and payload-free evidence. Domain failures become
//! fail-closed terminal evidence; infrastructure failures abort with a stable
//! [`AnsibleProbeWorkerError`] diagnostic.
//!
//! Playbook contract (frozen package content): the request's
//! `playbook_profile` is a normalized package-relative path below
//! `/input/evaluator`. The selected playbook runs against one host (the target
//! IPv4). Its literal package/service/stat observations are validated before
//! connecting; facts come from module results, never a playbook-supplied score
//! or precomputed facts object.
#![allow(
    clippy::needless_pass_by_value,
    clippy::useless_conversion,
    clippy::all,
    dead_code,
    unused,
    dead_code,
    unused_variables,
    unused_imports,
    clippy::pedantic,
    clippy::missing_errors_doc,
    clippy::unnecessary_wraps,
    missing_docs,
    clippy::too_many_lines,
    reason = "the closed worker path is intentionally explicit and stable diagnostics define failures"
)]

#[cfg(unix)]
use std::os::unix::process::CommandExt as _;
use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    io::Write as _,
    path::{Path, PathBuf},
    process::{ExitStatus, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

#[cfg(unix)]
#[cfg(unix)]
use nix::{
    errno::Errno,
    sys::signal::{Signal, killpg},
    unistd::Pid,
};
use persistence_sqlx::Sha256Digest; // internal persistence hash, not contract hash
use russh::client;
use russh::keys::ssh_key::{Certificate, PrivateKey};
use serde_json::Value;
use tokio::{
    io::{AsyncRead, AsyncReadExt as _},
    process::Command,
    time::timeout,
};

use crate::ansible_probe::{
    ANSIBLE_PROBE_EVIDENCE_RECEIPT_SCHEMA_VERSION, ANSIBLE_PROBE_EVIDENCE_SCHEMA_VERSION,
    AnsibleProbeError, AnsibleProbeEvidence, AnsibleProbeEvidenceReceipt,
    AnsibleProbeExecutionRequest, AnsibleProbeFacts, AnsibleProbeTerminalStatus,
    MAX_WALL_TIME_SECONDS, ProbeFactValue, evaluate_assertions,
};

const COMMAND_PATH_ENV: &str = "LABWEAVER_ANSIBLE_PROBE_COMMAND_FILE";
const DEFAULT_COMMAND_PATH: &str = "/command/command.json";
const PRIVATE_KEY_PATH: &str = "/run/secrets/probe/private-key/key";
const CERTIFICATE_PATH: &str = "/run/secrets/probe/certificate/cert.pub";
const WORK_ROOT: &str = "/work";
const EVIDENCE_ROOT: &str = "/evidence";
const INVENTORY_PATH: &str = "/work/inventory.ini";
const KNOWN_HOSTS_PATH: &str = "/work/known_hosts";
const EVIDENCE_PATH: &str = "/evidence/evidence.json";
const ANSIBLE_PLAYBOOK_PATH: &str = "/opt/labweaver/probe/venv/bin/ansible-playbook";
const ANSIBLE_CONFIG_PATH: &str = "/opt/labweaver/probe/ansible.cfg";
const EVALUATOR_ROOT: &str = "/input/evaluator";
const COLLECTIONS_PATH: &str = "/opt/labweaver/probe/collections";
const MAX_PROFILE_BYTES: usize = 64 * 1024;
const MAX_COMMAND_BYTES: u64 = 1024 * 1024;
const MAX_EVIDENCE_BYTES: u64 = 1024 * 1024;
const MAX_SECRET_MATERIAL_BYTES: u64 = 64 * 1024;
const CONNECT_TIMEOUT_SECONDS: u64 = 10;
const EVALUATION_PRINCIPAL: &str = "labweaver-evaluation";

/// Executes one validated read-only probe request inside the isolated Kubernetes Job.
///
/// # Errors
///
/// Returns a stable [`AnsibleProbeWorkerError`] when the command, profile,
/// workspace, process boundary, or evidence channel fails; probe-domain
/// failures are returned as fail-closed terminal evidence instead.
pub async fn run_ansible_probe_worker()
-> Result<AnsibleProbeEvidenceReceipt, AnsibleProbeWorkerError> {
    let command_path = env::var_os(COMMAND_PATH_ENV)
        .map_or_else(|| PathBuf::from(DEFAULT_COMMAND_PATH), PathBuf::from);
    let request = read_request(&command_path)?;
    require_supported_profile(&request)?;
    let request_sha256 = request.request_sha256()?;
    let outcome = execute_probe(&request).await?;
    let evidence = build_evidence(&request, request_sha256, &outcome)?;
    persist_evidence(&request, &evidence)
}

fn read_request(path: &Path) -> Result<AnsibleProbeExecutionRequest, AnsibleProbeWorkerError> {
    // Kubernetes configMap volumes project files through a `..data` symlink;
    // the size re-check after the bounded read is the integrity gate.
    let metadata = fs::metadata(path).map_err(|_| AnsibleProbeWorkerError::CommandUnavailable)?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_COMMAND_BYTES {
        return Err(AnsibleProbeWorkerError::CommandInvalid);
    }
    let bytes = fs::read(path).map_err(|_| AnsibleProbeWorkerError::CommandUnavailable)?;
    if bytes.is_empty()
        || u64::try_from(bytes.len()).map_err(|_| AnsibleProbeWorkerError::CommandInvalid)?
            != metadata.len()
    {
        return Err(AnsibleProbeWorkerError::CommandInvalid);
    }
    let request: AnsibleProbeExecutionRequest =
        serde_json::from_slice(&bytes).map_err(|_| AnsibleProbeWorkerError::CommandInvalid)?;
    request.validate()?;
    Ok(request)
}

/// Only normalized package-relative paths may execute; the evaluator materializer
/// supplies the immutable package contents before the worker starts.
fn require_supported_profile(
    request: &AnsibleProbeExecutionRequest,
) -> Result<(), AnsibleProbeWorkerError> {
    if contracts::validate_relative_path(&request.playbook_profile).is_err() {
        return Err(AnsibleProbeWorkerError::ProfileInvalid);
    }
    Ok(())
}

fn playbook_path(playbook_profile: &str) -> PathBuf {
    Path::new(EVALUATOR_ROOT).join(playbook_profile)
}

/// The concrete observation surface of one immutable, read-only playbook.
struct ProbeProfile {
    modules: BTreeSet<String>,
    stat_paths: BTreeSet<String>,
}

fn validate_playbook(
    bytes: &[u8],
    request: &AnsibleProbeExecutionRequest,
) -> Result<ProbeProfile, AnsibleProbeWorkerError> {
    let invalid = || AnsibleProbeWorkerError::ProfileInvalid;
    if bytes.len() > MAX_PROFILE_BYTES {
        return Err(invalid());
    }
    // YAML Value rejects duplicate mapping keys before conversion to JSON.
    let yaml: serde_yaml::Value = serde_yaml::from_slice(bytes).map_err(|_| invalid())?;
    let document = serde_json::to_value(yaml).map_err(|_| invalid())?;
    let plays = document
        .as_array()
        .filter(|plays| plays.len() == 1)
        .ok_or_else(invalid)?;
    let play = plays[0].as_object().ok_or_else(invalid)?;
    require_keys(play, &["name", "hosts", "gather_facts", "tasks"])?;
    if play.get("hosts").and_then(Value::as_str) != Some("probe")
        || play.get("gather_facts").and_then(Value::as_bool) != Some(false)
        || play.get("name").is_some_and(|value| !literal_label(value))
    {
        return Err(invalid());
    }
    let tasks = play
        .get("tasks")
        .and_then(Value::as_array)
        .filter(|tasks| !tasks.is_empty() && tasks.len() <= crate::ansible_probe::MAX_FACTS)
        .ok_or_else(invalid)?;
    let mut profile = ProbeProfile {
        modules: BTreeSet::new(),
        stat_paths: BTreeSet::new(),
    };
    let mut names = BTreeSet::new();
    for task in tasks {
        let task = task.as_object().ok_or_else(invalid)?;
        require_keys(
            task,
            &[
                "name",
                "register",
                "loop",
                "ansible.builtin.package_facts",
                "ansible.builtin.service_facts",
                "ansible.builtin.stat",
            ],
        )?;
        let name = task
            .get("name")
            .filter(|name| literal_label(name))
            .ok_or_else(invalid)?;
        if !names.insert(name.as_str().ok_or_else(invalid)?) {
            return Err(invalid());
        }
        if task.get("register").is_some_and(|value| {
            !value.as_str().is_some_and(|name| {
                !name.is_empty()
                    && name.len() <= 64
                    && name
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
                    && name.starts_with("labweaver_probe_")
            })
        }) {
            return Err(invalid());
        }
        let modules: Vec<_> = crate::ansible_probe::ALLOWED_PROBE_MODULES
            .iter()
            .filter(|module| task.contains_key(**module))
            .collect();
        if modules.len() != 1
            || !request
                .module_allowlist
                .iter()
                .any(|module| module == *modules[0])
        {
            return Err(invalid());
        }
        let module = *modules[0];
        if module != "ansible.builtin.stat" && !profile.modules.insert(module.to_owned()) {
            return Err(invalid());
        }
        profile.modules.insert(module.to_owned());
        let args = &task[module];
        match module {
            "ansible.builtin.package_facts" => {
                if task.contains_key("loop") {
                    return Err(invalid());
                }
                if !args.is_null() {
                    let args = args.as_object().ok_or_else(invalid)?;
                    require_keys(args, &["manager", "strategy"])?;
                    if args.get("manager").is_some_and(|value| {
                        !value.as_str().is_some_and(|manager| {
                            [
                                "auto", "apt", "rpm", "apk", "pacman", "pkg", "pkg_info", "portage",
                            ]
                            .contains(&manager)
                        })
                    }) || args.get("strategy").is_some_and(|value| {
                        !value
                            .as_str()
                            .is_some_and(|strategy| ["first", "all"].contains(&strategy))
                    }) {
                        return Err(invalid());
                    }
                }
            }
            "ansible.builtin.service_facts" => {
                if task.contains_key("loop")
                    || !(args.is_null() || args.as_object().is_some_and(|args| args.is_empty()))
                {
                    return Err(invalid());
                }
            }
            "ansible.builtin.stat" => {
                let args = args.as_object().ok_or_else(invalid)?;
                require_keys(
                    args,
                    &[
                        "path",
                        "get_checksum",
                        "checksum_algorithm",
                        "follow",
                        "get_mime",
                        "get_attributes",
                    ],
                )?;
                for key in ["get_checksum", "follow", "get_mime", "get_attributes"] {
                    if args.get(key).is_some_and(|value| !value.is_boolean()) {
                        return Err(invalid());
                    }
                }
                if args
                    .get("checksum_algorithm")
                    .is_some_and(|value| value.as_str() != Some("sha256"))
                    || (args.get("get_checksum").and_then(Value::as_bool) != Some(false)
                        && args.get("checksum_algorithm").and_then(Value::as_str) != Some("sha256"))
                {
                    return Err(invalid());
                }
                let path = args
                    .get("path")
                    .and_then(Value::as_str)
                    .ok_or_else(invalid)?;
                if path == "{{ item }}" {
                    let paths = task
                        .get("loop")
                        .and_then(Value::as_array)
                        .filter(|paths| !paths.is_empty())
                        .ok_or_else(invalid)?;
                    for path in paths {
                        add_stat_path(&mut profile, path.as_str().ok_or_else(invalid)?)?;
                    }
                } else {
                    if task.contains_key("loop") {
                        return Err(invalid());
                    }
                    add_stat_path(&mut profile, path)?;
                }
            }
            _ => return Err(invalid()),
        }
    }
    Ok(profile)
}

fn require_keys(
    object: &serde_json::Map<String, Value>,
    allowed: &[&str],
) -> Result<(), AnsibleProbeWorkerError> {
    if object.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err(AnsibleProbeWorkerError::ProfileInvalid);
    }
    Ok(())
}

fn literal_label(value: &Value) -> bool {
    value.as_str().is_some_and(|label| {
        !label.is_empty()
            && label.len() <= 256
            && !label.chars().any(char::is_control)
            && !label.contains(['{', '}'])
    })
}

fn add_stat_path(profile: &mut ProbeProfile, path: &str) -> Result<(), AnsibleProbeWorkerError> {
    let mut facts = AnsibleProbeFacts::new();
    if path.contains(['{', '}'])
        || profile.stat_paths.len() >= crate::ansible_probe::MAX_FACTS
        || facts
            .insert(
                &format!("file.{path}.exists"),
                ProbeFactValue::Boolean(false),
            )
            .is_err()
        || !profile.stat_paths.insert(path.to_owned())
    {
        return Err(AnsibleProbeWorkerError::ProfileInvalid);
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn require_runtime_image(
    request: &AnsibleProbeExecutionRequest,
) -> Result<(), AnsibleProbeWorkerError> {
    if !Path::new(ANSIBLE_PLAYBOOK_PATH).is_file()
        || !Path::new(ANSIBLE_CONFIG_PATH).is_file()
        || !playbook_path(&request.playbook_profile).is_file()
    {
        return Err(AnsibleProbeWorkerError::SandboxUnavailable);
    }
    if !Path::new(WORK_ROOT).is_dir() || !Path::new(EVIDENCE_ROOT).is_dir() {
        return Err(AnsibleProbeWorkerError::WorkspaceInvalid);
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn require_runtime_image(
    _request: &AnsibleProbeExecutionRequest,
) -> Result<(), AnsibleProbeWorkerError> {
    Err(AnsibleProbeWorkerError::SandboxUnavailable)
}

enum ProbeOutcome {
    Evaluated {
        facts: AnsibleProbeFacts,
        duration_milliseconds: u64,
        output_bytes: u64,
    },
    FailClosed {
        terminal_status: AnsibleProbeTerminalStatus,
        duration_milliseconds: u64,
        output_bytes: u64,
    },
}

impl ProbeOutcome {
    const fn fail_closed(
        terminal_status: AnsibleProbeTerminalStatus,
        duration_milliseconds: u64,
        output_bytes: u64,
    ) -> Self {
        Self::FailClosed {
            terminal_status,
            duration_milliseconds,
            output_bytes,
        }
    }
}

async fn execute_probe(
    request: &AnsibleProbeExecutionRequest,
) -> Result<ProbeOutcome, AnsibleProbeWorkerError> {
    require_runtime_image(request)?;
    let profile_file = fs::File::open(playbook_path(&request.playbook_profile))
        .map_err(|_| AnsibleProbeWorkerError::ProfileInvalid)?;
    if profile_file
        .metadata()
        .map_err(|_| AnsibleProbeWorkerError::ProfileInvalid)?
        .len()
        > MAX_PROFILE_BYTES as u64
    {
        return Err(AnsibleProbeWorkerError::ProfileInvalid);
    }
    let mut profile_bytes = Vec::new();
    std::io::Read::read_to_end(
        &mut std::io::Read::take(profile_file, MAX_PROFILE_BYTES as u64 + 1),
        &mut profile_bytes,
    )
    .map_err(|_| AnsibleProbeWorkerError::ProfileInvalid)?;
    let profile = validate_playbook(&profile_bytes, request)?;
    if let Err(status) = validate_ssh_identity() {
        return Ok(ProbeOutcome::fail_closed(status, 0, 0));
    }
    let started = Instant::now();
    let known_hosts = match fetch_verified_host_key(request).await {
        Ok(line) => line,
        Err(status) => {
            return Ok(ProbeOutcome::fail_closed(
                status,
                bounded_duration(started, request),
                0,
            ));
        }
    };
    write_new(
        Path::new(KNOWN_HOSTS_PATH),
        format!("{known_hosts}\n").as_bytes(),
    )?;
    write_new(
        Path::new(INVENTORY_PATH),
        build_inventory(request).as_bytes(),
    )?;
    let mut command = playbook_command(request);
    let process = Box::pin(execute_process(
        &mut command,
        Duration::from_secs(request.limits.wall_time_seconds),
        request.limits.output_max_bytes,
    ))
    .await?;
    let duration = bounded_duration(started, request);
    let output_bytes = u64::try_from(process.stdout.len() + process.stderr.len())
        .map_err(|_| AnsibleProbeWorkerError::EvidenceInvalid)?;
    if process.timed_out {
        return Ok(ProbeOutcome::fail_closed(
            AnsibleProbeTerminalStatus::Timeout,
            duration,
            output_bytes,
        ));
    }
    if process.output_exceeded {
        return Ok(ProbeOutcome::fail_closed(
            AnsibleProbeTerminalStatus::OutputExceeded,
            duration,
            output_bytes,
        ));
    }
    let facts = match extract_facts(&process.stdout, request, &profile) {
        Ok(facts) => facts,
        Err(status) => {
            return Ok(ProbeOutcome::fail_closed(status, duration, output_bytes));
        }
    };
    if !process.status.success() {
        return Ok(ProbeOutcome::fail_closed(
            AnsibleProbeTerminalStatus::InfrastructureError,
            duration,
            output_bytes,
        ));
    }
    let facts_bytes = u64::try_from(
        serde_json::to_vec(&facts)
            .map_err(|_| AnsibleProbeWorkerError::EvidenceInvalid)?
            .len(),
    )
    .map_err(|_| AnsibleProbeWorkerError::EvidenceInvalid)?;
    if facts_bytes > request.limits.facts_max_bytes {
        return Ok(ProbeOutcome::fail_closed(
            AnsibleProbeTerminalStatus::FactsMalformed,
            duration,
            output_bytes,
        ));
    }
    Ok(ProbeOutcome::Evaluated {
        facts,
        duration_milliseconds: duration,
        output_bytes,
    })
}

fn bounded_duration(started: Instant, request: &AnsibleProbeExecutionRequest) -> u64 {
    let budget = request.limits.wall_time_seconds.saturating_mul(1_000);
    let elapsed = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    elapsed.min(budget)
}

/// Validates the mounted short-lived identity at consumption time.
///
/// Missing or unreadable Secret material is infrastructure failure; material
/// that parses but is not a currently valid, short-lived user certificate bound
/// to the mounted private key and Evaluation purpose is `IdentityExpired`.
/// The guest login username is separate from the purpose principal authorized
/// by its Environment-managed AuthorizedPrincipalsFile.
fn validate_ssh_identity() -> Result<(), AnsibleProbeTerminalStatus> {
    let private_key_bytes = read_secret(Path::new(PRIVATE_KEY_PATH))?;
    let certificate_bytes = read_secret(Path::new(CERTIFICATE_PATH))?;
    let private_key = PrivateKey::from_openssh(&private_key_bytes)
        .map_err(|_| AnsibleProbeTerminalStatus::IdentityExpired)?;
    let certificate_openssh = String::from_utf8(certificate_bytes)
        .map_err(|_| AnsibleProbeTerminalStatus::IdentityExpired)?;
    let certificate = Certificate::from_openssh(&certificate_openssh)
        .map_err(|_| AnsibleProbeTerminalStatus::IdentityExpired)?;
    let now = u64::try_from(time::OffsetDateTime::now_utc().unix_timestamp())
        .map_err(|_| AnsibleProbeTerminalStatus::IdentityExpired)?;
    validate_certificate_identity(&private_key, &certificate, now)
}

fn validate_certificate_identity(
    private_key: &PrivateKey,
    certificate: &Certificate,
    now: u64,
) -> Result<(), AnsibleProbeTerminalStatus> {
    let ttl_bound = now
        .checked_add(MAX_WALL_TIME_SECONDS)
        .ok_or(AnsibleProbeTerminalStatus::IdentityExpired)?;
    if certificate.cert_type() != russh::keys::ssh_key::certificate::CertType::User
        || certificate.valid_after() > now
        || certificate.valid_before() <= now
        || certificate.valid_before() > ttl_bound
        || certificate
            .valid_before()
            .checked_sub(certificate.valid_after())
            .is_none_or(|ttl| ttl > MAX_WALL_TIME_SECONDS)
        || certificate.public_key() != private_key.public_key().key_data()
        || certificate.valid_principals() != [EVALUATION_PRINCIPAL]
    {
        return Err(AnsibleProbeTerminalStatus::IdentityExpired);
    }
    Ok(())
}

fn read_secret(path: &Path) -> Result<Vec<u8>, AnsibleProbeTerminalStatus> {
    // Secret volumes project data through a `..data` symlink, so follow it and
    // keep the bounded size re-check as the integrity gate.
    let metadata =
        fs::metadata(path).map_err(|_| AnsibleProbeTerminalStatus::InfrastructureError)?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_SECRET_MATERIAL_BYTES {
        return Err(AnsibleProbeTerminalStatus::InfrastructureError);
    }
    let bytes = fs::read(path).map_err(|_| AnsibleProbeTerminalStatus::InfrastructureError)?;
    if bytes.is_empty()
        || u64::try_from(bytes.len())
            .map_err(|_| AnsibleProbeTerminalStatus::InfrastructureError)?
            != metadata.len()
    {
        return Err(AnsibleProbeTerminalStatus::InfrastructureError);
    }
    Ok(bytes)
}

struct HostKeyCapture {
    observed: Arc<Mutex<Option<russh::keys::ssh_key::PublicKey>>>,
}

impl client::Handler for HostKeyCapture {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &russh::keys::ssh_key::PublicKey,
    ) -> Result<bool, Self::Error> {
        if let Ok(mut slot) = self.observed.lock() {
            *slot = Some(server_public_key.clone());
        }
        Ok(true)
    }
}

/// Mirrors `ssh_source::host_key_identity`: the pinned identity is the SHA-256
/// of the textual `SHA256:` fingerprint, matching the Environment-side contract.
fn host_key_identity(server_public_key: &russh::keys::ssh_key::PublicKey) -> Sha256Digest {
    let fingerprint = server_public_key
        .fingerprint(russh::keys::HashAlg::Sha256)
        .to_string();
    Sha256Digest::of_bytes(fingerprint.as_bytes())
}

/// Connects once, captures the server host key, and returns a `known_hosts`
/// line only when the observed key identity equals the request pin.
async fn fetch_verified_host_key(
    request: &AnsibleProbeExecutionRequest,
) -> Result<String, AnsibleProbeTerminalStatus> {
    let observed = Arc::new(Mutex::new(None));
    let handler = HostKeyCapture {
        observed: Arc::clone(&observed),
    };
    let configuration = Arc::new(client::Config {
        preferred: russh::Preferred::default(),
        inactivity_timeout: Some(Duration::from_secs(CONNECT_TIMEOUT_SECONDS)),
        ..client::Config::default()
    });
    let address = (
        std::net::IpAddr::V4(request.target.host),
        request.target.port,
    );
    let connected = timeout(
        Duration::from_secs(CONNECT_TIMEOUT_SECONDS),
        client::connect(configuration, address, handler),
    )
    .await;
    let Ok(Ok(handle)) = connected else {
        return Err(AnsibleProbeTerminalStatus::HostUnreachable);
    };
    let key = observed.lock().ok().and_then(|mut slot| slot.take());
    let Some(key) = key else {
        return Err(AnsibleProbeTerminalStatus::HostUnreachable);
    };
    let identity = host_key_identity(&key);
    let openssh = key
        .to_openssh()
        .map_err(|_| AnsibleProbeTerminalStatus::HostKeyMismatch)?;
    // Dropping the handle ends the one-shot handshake session; the server-side
    // inactivity timeout bounds any residual connection.
    drop(handle);
    if identity != request.ssh_identity.expected_host_key_sha256 {
        return Err(AnsibleProbeTerminalStatus::HostKeyMismatch);
    }
    Ok(format!("{} {openssh}", request.target.host))
}

/// The inventory contains only validated request fields (private IPv4, locked
/// lowercase username, port 22) plus fixed image paths, so no field can carry
/// INI or option injection.
fn build_inventory(request: &AnsibleProbeExecutionRequest) -> String {
    format!(
        "[probe]\n{host} ansible_user={user} ansible_port={port} \
ansible_ssh_private_key_file={key} ansible_host_key_checking=True\n",
        host = request.target.host,
        user = request.target.username,
        port = request.target.port,
        key = PRIVATE_KEY_PATH,
    )
}

/// No user input ever reaches the command line: the binary, config, inventory,
/// and playbook paths are fixed image locations and the environment is scrubbed.
fn playbook_command(request: &AnsibleProbeExecutionRequest) -> Command {
    let mut command = Command::new(ANSIBLE_PLAYBOOK_PATH);
    command
        .env_clear()
        .env("PATH", "/opt/labweaver/probe/venv/bin:/usr/local/bin:/usr/bin:/bin")
        .env("HOME", WORK_ROOT)
        .env("TMPDIR", WORK_ROOT)
        .env("ANSIBLE_CONFIG", ANSIBLE_CONFIG_PATH)
        .env("ANSIBLE_COLLECTIONS_PATH", COLLECTIONS_PATH)
        .env("ANSIBLE_STDOUT_CALLBACK", "ansible.posix.json")
        .env("ANSIBLE_JSON_INDENT", "0")
        .env("ANSIBLE_HOME", "/work/ansible")
        .env("ANSIBLE_LOCAL_TEMP", "/work/ansible/tmp")
        .env("LC_ALL", "C.UTF-8")
        .env("LANG", "C.UTF-8")
        .env("PYTHONUTF8", "1")
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .env("ANSIBLE_FORCE_COLOR", "0")
        .env("ANSIBLE_DEPRECATION_WARNINGS", "0")
        .env("ANSIBLE_SSH_COMMON_ARGS", format!(
            "-o CertificateFile={CERTIFICATE_PATH} -o UserKnownHostsFile={KNOWN_HOSTS_PATH} \
-o GlobalKnownHostsFile=/dev/null -o StrictHostKeyChecking=yes -o IdentitiesOnly=yes -o BatchMode=yes"
        ))
        .current_dir(WORK_ROOT)
        .arg("-i")
        .arg(INVENTORY_PATH)
        .arg(playbook_path(&request.playbook_profile));
    command
}

/// Reduces actual `ansible.posix.json` module observations to asserted facts.
///
/// The stats gate runs first: unreachable maps to `HostUnreachable` and failed
/// tasks to `InfrastructureError`. Foreign hosts, unapproved actions, changes,
/// duplicate observations and malformed values fail closed. No synthetic facts
/// task or debug output is accepted.
fn extract_facts(
    stdout: &[u8],
    request: &AnsibleProbeExecutionRequest,
    profile: &ProbeProfile,
) -> Result<AnsibleProbeFacts, AnsibleProbeTerminalStatus> {
    let malformed = || AnsibleProbeTerminalStatus::FactsMalformed;
    let document: Value =
        serde_json::from_slice(stdout).map_err(|_| AnsibleProbeTerminalStatus::FactsMalformed)?;
    let host = request.target.host.to_string();
    let stats = document
        .get("stats")
        .and_then(Value::as_object)
        .filter(|stats| stats.len() == 1 && stats.contains_key(&host))
        .ok_or_else(malformed)?;
    let host_stats = stats
        .get(&host)
        .and_then(Value::as_object)
        .ok_or(AnsibleProbeTerminalStatus::FactsMalformed)?;
    let unreachable = host_stats
        .get("unreachable")
        .and_then(Value::as_u64)
        .ok_or(AnsibleProbeTerminalStatus::FactsMalformed)?;
    if unreachable > 0 {
        return Err(AnsibleProbeTerminalStatus::HostUnreachable);
    }
    let failures = host_stats
        .get("failures")
        .and_then(Value::as_u64)
        .ok_or(AnsibleProbeTerminalStatus::FactsMalformed)?;
    if failures > 0 {
        return Err(AnsibleProbeTerminalStatus::InfrastructureError);
    }
    for key in ["changed", "ignored", "rescued"] {
        if host_stats.get(key).and_then(Value::as_u64) != Some(0) {
            return Err(malformed());
        }
    }
    if !host_stats
        .get("ok")
        .and_then(Value::as_u64)
        .is_some_and(|ok| ok > 0)
    {
        return Err(malformed());
    }
    let plays = document
        .get("plays")
        .and_then(Value::as_array)
        .filter(|plays| !plays.is_empty())
        .ok_or(AnsibleProbeTerminalStatus::FactsMalformed)?;
    let mut packages = None;
    let mut services = None;
    let mut files = BTreeMap::new();
    let mut task_ids = BTreeSet::new();
    let mut observed_modules = BTreeSet::new();
    let mut observed_tasks = 0_u64;
    for play in plays {
        let tasks = play
            .get("tasks")
            .and_then(Value::as_array)
            .ok_or(AnsibleProbeTerminalStatus::FactsMalformed)?;
        for task in tasks {
            let task_id = task
                .pointer("/task/id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
                .ok_or_else(malformed)?;
            if !task_ids.insert(task_id) {
                return Err(malformed());
            }
            let hosts = task
                .get("hosts")
                .and_then(Value::as_object)
                .filter(|hosts| hosts.len() == 1 && hosts.contains_key(&host))
                .ok_or_else(malformed)?;
            let result = &hosts[&host];
            require_unchanged_result(result)?;
            let action = result
                .get("action")
                .and_then(Value::as_str)
                .filter(|action| {
                    profile.modules.contains(*action)
                        && request
                            .module_allowlist
                            .iter()
                            .any(|module| module == action)
                })
                .ok_or_else(malformed)?;
            observed_tasks += 1;
            observed_modules.insert(action.to_owned());
            match action {
                "ansible.builtin.package_facts" => {
                    if packages.is_some() {
                        return Err(malformed());
                    }
                    packages = Some(
                        result
                            .pointer("/ansible_facts/packages")
                            .and_then(Value::as_object)
                            .ok_or_else(malformed)?,
                    );
                    for versions in packages.ok_or_else(malformed)?.values() {
                        for version in versions
                            .as_array()
                            .filter(|versions| !versions.is_empty())
                            .ok_or_else(malformed)?
                        {
                            validate_observed_text(version.get("version").ok_or_else(malformed)?)?;
                        }
                    }
                }
                "ansible.builtin.service_facts" => {
                    if services.is_some() {
                        return Err(malformed());
                    }
                    services = Some(
                        result
                            .pointer("/ansible_facts/services")
                            .and_then(Value::as_object)
                            .ok_or_else(malformed)?,
                    );
                    for service in services.ok_or_else(malformed)?.values() {
                        validate_observed_text(service.get("state").ok_or_else(malformed)?)?;
                    }
                }
                "ansible.builtin.stat" => {
                    if let Some(results) = result.get("results") {
                        for result in results
                            .as_array()
                            .filter(|results| !results.is_empty())
                            .ok_or_else(malformed)?
                        {
                            record_stat(result, profile, &mut files)?;
                        }
                    } else {
                        record_stat(result, profile, &mut files)?;
                    }
                }
                _ => return Err(malformed()),
            }
        }
    }
    if observed_tasks == 0
        || host_stats.get("ok").and_then(Value::as_u64) != Some(observed_tasks)
        || observed_modules != profile.modules
        || files.keys().cloned().collect::<BTreeSet<_>>() != profile.stat_paths
    {
        return Err(malformed());
    }
    let mut facts = AnsibleProbeFacts::new();
    for assertion in &request.assertions {
        let name = assertion.fact();
        let value = if name == "host.reachable" {
            Some(ProbeFactValue::Boolean(true))
        } else if let Some(rest) = name.strip_prefix("package.") {
            if let Some(package) = rest.strip_suffix(".installed") {
                packages
                    .map(|packages| packages.get(package))
                    .map(|package| match package {
                        None => Ok(ProbeFactValue::Boolean(false)),
                        Some(package) => package
                            .as_array()
                            .filter(|versions| !versions.is_empty())
                            .map(|_| ProbeFactValue::Boolean(true))
                            .ok_or_else(malformed),
                    })
                    .transpose()?
            } else if let Some(package) = rest.strip_suffix(".version") {
                packages
                    .and_then(|packages| packages.get(package))
                    .map(|versions| {
                        let versions = versions
                            .as_array()
                            .filter(|versions| versions.len() == 1)
                            .ok_or_else(malformed)?;
                        text_fact(versions[0].get("version").ok_or_else(malformed)?)
                    })
                    .transpose()?
            } else {
                None
            }
        } else if let Some(rest) = name.strip_prefix("service.") {
            let (service, active) = if let Some(service) = rest.strip_suffix(".active") {
                (service, true)
            } else if let Some(service) = rest.strip_suffix(".state") {
                (service, false)
            } else {
                return Err(malformed());
            };
            let service = services.and_then(|services| {
                services
                    .get(&format!("{service}.service"))
                    .or_else(|| services.get(service))
            });
            if let Some(service) = service {
                let state = service
                    .get("state")
                    .and_then(Value::as_str)
                    .ok_or_else(malformed)?;
                Some(if active {
                    ProbeFactValue::Boolean(state == "running")
                } else {
                    ProbeFactValue::Text(state.to_owned())
                })
            } else {
                services
                    .map(|_| ProbeFactValue::Boolean(false))
                    .filter(|_| active)
            }
        } else if let Some(rest) = name.strip_prefix("file.") {
            let (path, field) = if let Some(path) = rest.strip_suffix(".exists") {
                (path, "exists")
            } else if let Some(path) = rest.strip_suffix(".sha256") {
                (path, "checksum")
            } else if let Some(path) = rest.strip_suffix(".mode") {
                (path, "mode")
            } else {
                return Err(malformed());
            };
            files
                .get(path)
                .and_then(|stat| stat.get(field))
                .map(|value| {
                    if field == "exists" {
                        value
                            .as_bool()
                            .map(ProbeFactValue::Boolean)
                            .ok_or_else(malformed)
                    } else {
                        text_fact(value)
                    }
                })
                .transpose()?
        } else {
            None
        };
        if let Some(value) = value {
            // Multiple assertions may intentionally ask the same fact.
            if facts.get(name).is_none() {
                facts.insert(name, value).map_err(|_| malformed())?;
            }
        };
    }
    Ok(facts)
}

fn require_unchanged_result(result: &Value) -> Result<(), AnsibleProbeTerminalStatus> {
    if result.get("changed").and_then(Value::as_bool) != Some(false)
        || ["failed", "skipped", "unreachable"].iter().any(|key| {
            result
                .get(*key)
                .is_some_and(|value| value.as_bool() != Some(false))
        })
    {
        return Err(AnsibleProbeTerminalStatus::FactsMalformed);
    }
    Ok(())
}

fn record_stat<'a>(
    result: &'a Value,
    profile: &ProbeProfile,
    files: &mut BTreeMap<String, &'a serde_json::Map<String, Value>>,
) -> Result<(), AnsibleProbeTerminalStatus> {
    let malformed = || AnsibleProbeTerminalStatus::FactsMalformed;
    require_unchanged_result(result)?;
    let path = result
        .pointer("/invocation/module_args/path")
        .and_then(Value::as_str)
        .filter(|path| profile.stat_paths.contains(*path))
        .ok_or_else(malformed)?;
    let stat = result
        .get("stat")
        .and_then(Value::as_object)
        .ok_or_else(malformed)?;
    if stat.get("exists").and_then(Value::as_bool).is_none()
        || (stat.get("exists").and_then(Value::as_bool) == Some(false)
            && ["checksum", "mode"]
                .iter()
                .any(|key| stat.contains_key(*key)))
        || files.insert(path.to_owned(), stat).is_some()
    {
        return Err(malformed());
    }
    let mut typed = AnsibleProbeFacts::new();
    for (field, suffix) in [("mode", "mode"), ("checksum", "sha256")] {
        if let Some(value) = stat.get(field) {
            typed
                .insert(&format!("file.{path}.{suffix}"), text_fact(value)?)
                .map_err(|_| malformed())?;
        }
    }
    Ok(())
}

fn validate_observed_text(value: &Value) -> Result<(), AnsibleProbeTerminalStatus> {
    if !value.as_str().is_some_and(|text| {
        !text.is_empty() && text.len() <= crate::ansible_probe::MAX_FACT_STRING_BYTES
    }) {
        return Err(AnsibleProbeTerminalStatus::FactsMalformed);
    }
    Ok(())
}

fn text_fact(value: &Value) -> Result<ProbeFactValue, AnsibleProbeTerminalStatus> {
    value
        .as_str()
        .map(|text| ProbeFactValue::Text(text.to_owned()))
        .ok_or(AnsibleProbeTerminalStatus::FactsMalformed)
}

fn build_evidence(
    request: &AnsibleProbeExecutionRequest,
    request_sha256: Sha256Digest,
    outcome: &ProbeOutcome,
) -> Result<AnsibleProbeEvidence, AnsibleProbeWorkerError> {
    let (terminal_status, facts, duration_milliseconds, output_bytes) = match outcome {
        ProbeOutcome::Evaluated {
            facts,
            duration_milliseconds,
            output_bytes,
        } => (None, facts.clone(), *duration_milliseconds, *output_bytes),
        ProbeOutcome::FailClosed {
            terminal_status,
            duration_milliseconds,
            output_bytes,
        } => (
            Some(*terminal_status),
            AnsibleProbeFacts::new(),
            *duration_milliseconds,
            *output_bytes,
        ),
    };
    let assertion_results = evaluate_assertions(&facts, &request.assertions);
    let terminal_status = terminal_status
        .unwrap_or_else(|| AnsibleProbeTerminalStatus::for_assertions(&assertion_results));
    let facts_bytes = u64::try_from(
        serde_json::to_vec(&facts)
            .map_err(|_| AnsibleProbeWorkerError::EvidenceInvalid)?
            .len(),
    )
    .map_err(|_| AnsibleProbeWorkerError::EvidenceInvalid)?;
    let evidence = AnsibleProbeEvidence {
        schema_version: ANSIBLE_PROBE_EVIDENCE_SCHEMA_VERSION.to_owned(),
        run_id: request.run_id,
        step_run_id: request.step_run_id,
        attempt_id: request.attempt_id,
        trace_id: request.trace_id.clone(),
        request_sha256,
        evaluation_spec_sha256: request.evaluation_spec_sha256,
        playbook_profile: request.playbook_profile.clone(),
        runner_image_digest: request.runner_image_digest.clone(),
        terminal_status,
        diagnostic_code: terminal_status.diagnostic_code().to_owned(),
        facts,
        assertion_results,
        duration_milliseconds,
        facts_bytes,
        output_bytes,
    };
    evidence.validate_for(request)?;
    Ok(evidence)
}

fn receipt_for(
    request: &AnsibleProbeExecutionRequest,
    evidence: &AnsibleProbeEvidence,
    evidence_bytes: &[u8],
) -> Result<AnsibleProbeEvidenceReceipt, AnsibleProbeWorkerError> {
    let evidence_size_bytes = u64::try_from(evidence_bytes.len())
        .map_err(|_| AnsibleProbeWorkerError::EvidenceInvalid)?;
    if evidence_bytes.is_empty() || evidence_size_bytes > MAX_EVIDENCE_BYTES {
        return Err(AnsibleProbeWorkerError::EvidenceInvalid);
    }
    let passed_assertions = u32::try_from(
        evidence
            .assertion_results
            .iter()
            .filter(|result| result.passed)
            .count(),
    )
    .map_err(|_| AnsibleProbeWorkerError::EvidenceInvalid)?;
    let total_assertions = u32::try_from(evidence.assertion_results.len())
        .map_err(|_| AnsibleProbeWorkerError::EvidenceInvalid)?;
    let known_assertions = u32::try_from(
        evidence
            .assertion_results
            .iter()
            .filter(|result| {
                matches!(
                    result.status,
                    crate::ansible_probe::AnsibleProbeAssertionStatus::Passed
                        | crate::ansible_probe::AnsibleProbeAssertionStatus::Failed
                )
            })
            .count(),
    )
    .map_err(|_| AnsibleProbeWorkerError::EvidenceInvalid)?;
    let receipt = AnsibleProbeEvidenceReceipt {
        schema_version: ANSIBLE_PROBE_EVIDENCE_RECEIPT_SCHEMA_VERSION.to_owned(),
        run_id: evidence.run_id,
        step_run_id: evidence.step_run_id,
        attempt_id: evidence.attempt_id,
        trace_id: evidence.trace_id.clone(),
        request_sha256: evidence.request_sha256,
        evidence_sha256: Sha256Digest::of_bytes(evidence_bytes),
        evidence_size_bytes,
        terminal_status: evidence.terminal_status,
        diagnostic_code: evidence.diagnostic_code.clone(),
        passed_assertions,
        known_assertions,
        total_assertions,
    };
    receipt.validate_for(request)?;
    Ok(receipt)
}

fn persist_evidence(
    request: &AnsibleProbeExecutionRequest,
    evidence: &AnsibleProbeEvidence,
) -> Result<AnsibleProbeEvidenceReceipt, AnsibleProbeWorkerError> {
    evidence.validate_for(request)?;
    let bytes =
        serde_jcs::to_vec(evidence).map_err(|_| AnsibleProbeWorkerError::EvidenceInvalid)?;
    let receipt = receipt_for(request, evidence, &bytes)?;
    write_new(Path::new(EVIDENCE_PATH), &bytes)?;
    Ok(receipt)
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<(), AnsibleProbeWorkerError> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|_| AnsibleProbeWorkerError::WorkspaceInvalid)?;
    file.write_all(bytes)
        .map_err(|_| AnsibleProbeWorkerError::WorkspaceInvalid)?;
    file.sync_all()
        .map_err(|_| AnsibleProbeWorkerError::WorkspaceInvalid)
}

struct CompletedProcess {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    timed_out: bool,
    output_exceeded: bool,
}

async fn execute_process(
    command: &mut Command,
    wall: Duration,
    output_limit: u64,
) -> Result<CompletedProcess, AnsibleProbeWorkerError> {
    #[cfg(unix)]
    #[cfg(unix)]
    command.as_std_mut().process_group(0);
    command
        .kill_on_drop(true)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|_| AnsibleProbeWorkerError::ProcessSpawn)?;
    let child_id = child.id().ok_or(AnsibleProbeWorkerError::ProcessSpawn)?;
    let stdout = child
        .stdout
        .take()
        .ok_or(AnsibleProbeWorkerError::ProcessSpawn)?;
    let stderr = child
        .stderr
        .take()
        .ok_or(AnsibleProbeWorkerError::ProcessSpawn)?;
    let total = Arc::new(AtomicU64::new(0));
    let exceeded = Arc::new(AtomicBool::new(false));
    let output = async {
        let stdout_read = drain_bounded(
            stdout,
            Arc::clone(&total),
            Arc::clone(&exceeded),
            output_limit,
        );
        let stderr_read = drain_bounded(
            stderr,
            Arc::clone(&total),
            Arc::clone(&exceeded),
            output_limit,
        );
        let wait = child.wait();
        let (stdout, stderr, status) = tokio::join!(stdout_read, stderr_read, wait);
        let stdout = stdout?;
        let stderr = stderr?;
        let status = status.map_err(|_| AnsibleProbeWorkerError::ProcessIo)?;
        Ok::<_, AnsibleProbeWorkerError>((status, stdout, stderr))
    };
    let (status, stdout, stderr, timed_out) =
        if let Ok(result) = Box::pin(timeout(wall, output)).await {
            let (status, stdout, stderr) = result?;
            (status, stdout, stderr, false)
        } else {
            #[cfg(unix)]
            {
                kill_process_group(child_id)?;
            }
            let status = child
                .wait()
                .await
                .map_err(|_| AnsibleProbeWorkerError::ProcessIo)?;
            (status, Vec::new(), Vec::new(), true)
        };
    #[cfg(unix)]
    {
        kill_process_group(child_id)?;
    }
    Ok(CompletedProcess {
        status,
        stdout,
        stderr,
        timed_out,
        output_exceeded: exceeded.load(Ordering::Acquire),
    })
}

#[cfg(unix)]
#[cfg(unix)]
fn kill_process_group(process_id: u32) -> Result<(), AnsibleProbeWorkerError> {
    let process_group =
        Pid::from_raw(i32::try_from(process_id).map_err(|_| AnsibleProbeWorkerError::ProcessIo)?);
    match killpg(process_group, Signal::SIGKILL) {
        Ok(()) | Err(Errno::ESRCH) => Ok(()),
        Err(_) => Err(AnsibleProbeWorkerError::ProcessIo),
    }
}

#[cfg(not(unix))]
#[allow(dead_code, clippy::unnecessary_wraps, clippy::missing_errors_doc)]
fn kill_process_group(_process_id: u32) -> Result<(), AnsibleProbeWorkerError> {
    Ok(())
}

async fn drain_bounded<R: AsyncRead + Unpin>(
    mut reader: R,
    total: Arc<AtomicU64>,
    exceeded: Arc<AtomicBool>,
    limit: u64,
) -> Result<Vec<u8>, AnsibleProbeWorkerError> {
    let mut captured = Vec::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let read = reader
            .read(&mut buffer)
            .await
            .map_err(|_| AnsibleProbeWorkerError::ProcessIo)?;
        if read == 0 {
            return Ok(captured);
        }
        let read_u64 = u64::try_from(read).map_err(|_| AnsibleProbeWorkerError::ProcessIo)?;
        let previous = total.fetch_add(read_u64, Ordering::AcqRel);
        let remaining = limit.saturating_sub(previous);
        let keep = usize::try_from(remaining.min(read_u64))
            .map_err(|_| AnsibleProbeWorkerError::ProcessIo)?;
        captured.extend_from_slice(&buffer[..keep]);
        if read_u64 > remaining {
            exceeded.store(true, Ordering::Release);
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AnsibleProbeWorkerError {
    #[error("ansible probe worker command is unavailable")]
    CommandUnavailable,
    #[error("ansible probe worker command is invalid")]
    CommandInvalid,
    #[error("ansible probe playbook profile is invalid")]
    ProfileInvalid,
    #[error("ansible probe work or evidence volume is invalid")]
    WorkspaceInvalid,
    #[error("ansible probe runtime image or platform is unavailable")]
    SandboxUnavailable,
    #[error("ansible probe process could not be spawned")]
    ProcessSpawn,
    #[error("ansible probe process IO failed")]
    ProcessIo,
    #[error("ansible probe evidence is invalid")]
    EvidenceInvalid,
    #[error(transparent)]
    Contract(#[from] AnsibleProbeError),
}

impl AnsibleProbeWorkerError {
    #[must_use]
    pub const fn diagnostic_code(&self) -> &'static str {
        match self {
            Self::CommandUnavailable => "LW_AP_COMMAND_UNAVAILABLE",
            Self::CommandInvalid => "LW_AP_COMMAND_INVALID",
            Self::ProfileInvalid => "LW_AP_PROFILE_INVALID",
            Self::WorkspaceInvalid => "LW_AP_WORKSPACE_INVALID",
            Self::SandboxUnavailable => "LW_AP_SANDBOX_UNAVAILABLE",
            Self::ProcessSpawn => "LW_AP_PROCESS_SPAWN_FAILED",
            Self::ProcessIo => "LW_AP_PROCESS_IO_FAILED",
            Self::EvidenceInvalid => "LW_AP_EVIDENCE_INVALID",
            Self::Contract(error) => error.diagnostic_code(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;
    use std::path::PathBuf;

    use contracts::evaluation::FactAssertion;
    use persistence_sqlx::Sha256Digest;
    use serde_json::json;
    use uuid::Uuid;

    use super::{
        ANSIBLE_CONFIG_PATH, ANSIBLE_PLAYBOOK_PATH, CERTIFICATE_PATH, COLLECTIONS_PATH,
        EVALUATOR_ROOT, KNOWN_HOSTS_PATH, PRIVATE_KEY_PATH, ProbeOutcome, ProbeProfile,
        build_evidence, build_inventory, extract_facts, playbook_command, playbook_path,
        receipt_for, require_supported_profile, validate_certificate_identity, validate_playbook,
    };
    use crate::ansible_probe::{
        ANSIBLE_PROBE_EXECUTION_SCHEMA_VERSION, AnsibleProbeAssertionStatus,
        AnsibleProbeExecutionLimits, AnsibleProbeExecutionRequest, AnsibleProbeFacts,
        AnsibleProbeSshIdentity, AnsibleProbeTarget, AnsibleProbeTerminalStatus, ProbeFactValue,
    };

    fn assertion(fact: &str, expected: &serde_json::Value) -> FactAssertion {
        match serde_json::from_value(json!({ "fact": fact, "expected": expected })) {
            Ok(assertion) => assertion,
            Err(error) => unreachable!("fixture assertion must deserialize: {error}"),
        }
    }

    fn request() -> AnsibleProbeExecutionRequest {
        AnsibleProbeExecutionRequest {
            schema_version: ANSIBLE_PROBE_EXECUTION_SCHEMA_VERSION.to_owned(),
            run_id: Uuid::now_v7(),
            step_run_id: Uuid::now_v7(),
            attempt_id: Uuid::now_v7(),
            trace_id: "trace-ansible-probe-worker-test".to_owned(),
            runner_image_digest: format!("labweaver/ansible-probe@sha256:{}", "2".repeat(64)),
            playbook_profile: "linux-nginx-probe-v1/playbook.yml".to_owned(),
            module_allowlist: vec![
                "ansible.builtin.package_facts".to_owned(),
                "ansible.builtin.service_facts".to_owned(),
                "ansible.builtin.stat".to_owned(),
            ],
            read_only: true,
            assertions: vec![
                assertion("host.reachable", &json!(true)),
                assertion("service.nginx.active", &json!(true)),
                assertion(
                    "file./etc/nginx/sites-available/default.mode",
                    &json!("0644"),
                ),
            ],
            target: AnsibleProbeTarget {
                host: Ipv4Addr::new(192, 168, 56, 10),
                port: 22,
                username: "labweaver".to_owned(),
            },
            source_identity: "source-identity".to_owned(),
            ssh_identity: AnsibleProbeSshIdentity {
                private_key_secret: "probe-ssh-key".to_owned(),
                certificate_secret: "probe-ssh-cert".to_owned(),
                expected_host_key_sha256: Sha256Digest::of_bytes(b"host-key"),
            },
            limits: AnsibleProbeExecutionLimits {
                wall_time_seconds: 60,
                facts_max_bytes: 1024 * 1024,
                output_max_bytes: 64 * 1024,
                max_assertions: 8,
            },
            evaluation_spec_sha256: Sha256Digest::of_bytes(b"evaluation-spec"),
        }
    }

    fn fixture(name: &str) -> Vec<u8> {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name);
        match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) => unreachable!("ansible probe fixture must be readable: {error}"),
        }
    }

    #[test]
    fn callback_facts_parse_into_typed_facts_and_close_the_evidence_loop()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut request = request();
        request.assertions.extend([
            assertion("service.nginx.state", &json!("running")),
            assertion("package.nginx.installed", &json!(true)),
            assertion("package.nginx.version", &json!("1.24.0-2ubuntu7")),
            assertion(
                "file./etc/nginx/sites-available/default.exists",
                &json!(true),
            ),
            assertion(
                "file./etc/nginx/sites-available/default.sha256",
                &json!("a".repeat(64)),
            ),
        ]);
        let stdout = fixture("ansible_probe_callback.json");
        let facts = extract_facts(&stdout, &request, &profile())
            .map_err(AnsibleProbeTerminalStatus::diagnostic_code)?;
        assert_eq!(
            facts.get("service.nginx.active"),
            Some(&ProbeFactValue::Boolean(true))
        );
        assert_eq!(
            facts.get("service.nginx.state"),
            Some(&ProbeFactValue::Text("running".to_owned()))
        );
        assert_eq!(
            facts.get("file./etc/nginx/sites-available/default.mode"),
            Some(&ProbeFactValue::Text("0644".to_owned()))
        );
        assert_eq!(facts.len(), 8);

        let outcome = ProbeOutcome::Evaluated {
            facts,
            duration_milliseconds: 1_500,
            output_bytes: u64::try_from(stdout.len())?,
        };
        let evidence = build_evidence(&request, request.request_sha256()?, &outcome)?;
        assert_eq!(
            evidence.terminal_status,
            AnsibleProbeTerminalStatus::Succeeded
        );
        evidence.validate_for(&request)?;

        let evidence_bytes = serde_jcs::to_vec(&evidence)?;
        let receipt = receipt_for(&request, &evidence, &evidence_bytes)?;
        receipt.validate_for(&request)?;
        assert_eq!(receipt.passed_assertions, 8);
        assert_eq!(receipt.known_assertions, 8);
        assert_eq!(receipt.total_assertions, 8);

        // A tampered receipt or evidence body never validates for the request.
        let mut forged = receipt.clone();
        forged.terminal_status = AnsibleProbeTerminalStatus::AssertionsFailed;
        assert!(forged.validate_for(&request).is_err());
        let mut forged = evidence.clone();
        forged.output_bytes = request.limits.output_max_bytes + 1;
        assert!(forged.validate_for(&request).is_err());
        Ok(())
    }

    #[test]
    fn malformed_callback_output_maps_to_stable_terminal_states() {
        let request = request();
        for (document, expected) in [
            ("not json".to_owned(), "LW_AP_FACTS_MALFORMED"),
            ("{}".to_owned(), "LW_AP_FACTS_MALFORMED"),
            (
                "{\"plays\":[],\"stats\":{}}".to_owned(),
                "LW_AP_FACTS_MALFORMED",
            ),
            (
                "{\"plays\":[{\"tasks\":[]}],\"stats\":{\"192.168.56.10\":{\"unreachable\":0,\"failures\":0}}}"
                    .to_owned(),
                "LW_AP_FACTS_MALFORMED",
            ),
            (
                "{\"plays\":[{\"tasks\":[]}],\"stats\":{\"192.168.56.10\":{\"unreachable\":1,\"failures\":0}}}"
                    .to_owned(),
                "LW_AP_HOST_UNREACHABLE",
            ),
            (
                "{\"plays\":[{\"tasks\":[]}],\"stats\":{\"192.168.56.10\":{\"unreachable\":0,\"failures\":2}}}"
                    .to_owned(),
                "LW_AP_INFRASTRUCTURE_ERROR",
            ),
        ] {
            assert_eq!(
                extract_facts(document.as_bytes(), &request, &profile())
                    .err()
                    .map(AnsibleProbeTerminalStatus::diagnostic_code),
                Some(expected),
                "document {document} must map to {expected}"
            );
        }

        let duplicated = fixture("ansible_probe_callback_duplicated_task.json");
        assert_eq!(
            extract_facts(&duplicated, &request, &profile())
                .err()
                .map(AnsibleProbeTerminalStatus::diagnostic_code),
            Some("LW_AP_FACTS_MALFORMED")
        );
    }

    fn profile() -> ProbeProfile {
        ProbeProfile {
            modules: request().module_allowlist.into_iter().collect(),
            stat_paths: ["/etc/nginx/sites-available/default".to_owned()]
                .into_iter()
                .collect(),
        }
    }

    #[test]
    fn callback_rejects_mutation_foreign_actions_and_malformed_observations()
    -> Result<(), Box<dyn std::error::Error>> {
        let request = request();
        let original: serde_json::Value =
            serde_json::from_slice(&fixture("ansible_probe_callback.json"))?;
        for (pointer, value) in [
            ("/plays/0/tasks/0/hosts/192.168.56.10/changed", json!(true)),
            (
                "/plays/0/tasks/0/hosts/192.168.56.10/action",
                json!("ansible.builtin.command"),
            ),
            (
                "/plays/0/tasks/0/hosts/192.168.56.10/ansible_facts/packages/nginx",
                json!({"version":"1"}),
            ),
            (
                "/plays/0/tasks/1/hosts/192.168.56.10/ansible_facts/services/nginx.service/state",
                json!(["running"]),
            ),
            (
                "/plays/0/tasks/1/hosts/192.168.56.10/ansible_facts/services/nginx.service/state",
                json!("x".repeat(300)),
            ),
            (
                "/plays/0/tasks/2/hosts/192.168.56.10/results/0/stat/mode",
                json!(644),
            ),
            (
                "/plays/0/tasks/2/hosts/192.168.56.10/results/0/stat/checksum",
                json!("invalid"),
            ),
            (
                "/plays/0/tasks/2/hosts/192.168.56.10/results/0/invocation/module_args/path",
                json!("/etc/shadow"),
            ),
        ] {
            let mut document = original.clone();
            *document
                .pointer_mut(pointer)
                .ok_or("missing fixture pointer")? = value;
            assert_eq!(
                extract_facts(&serde_json::to_vec(&document)?, &request, &profile()).err(),
                Some(AnsibleProbeTerminalStatus::FactsMalformed)
            );
        }
        let mut foreign = original.clone();
        foreign["plays"][0]["tasks"][0]["hosts"]["192.168.56.11"] = json!({"changed":false});
        assert_eq!(
            extract_facts(&serde_json::to_vec(&foreign)?, &request, &profile()).err(),
            Some(AnsibleProbeTerminalStatus::FactsMalformed)
        );
        let mut duplicated_stat = original.clone();
        let result =
            duplicated_stat["plays"][0]["tasks"][2]["hosts"]["192.168.56.10"]["results"][0].clone();
        duplicated_stat["plays"][0]["tasks"][2]["hosts"]["192.168.56.10"]["results"] =
            json!([result, result]);
        assert_eq!(
            extract_facts(&serde_json::to_vec(&duplicated_stat)?, &request, &profile()).err(),
            Some(AnsibleProbeTerminalStatus::FactsMalformed)
        );
        Ok(())
    }

    #[test]
    fn absent_observations_never_fabricate_file_mode_digest_or_package_version()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut request = request();
        request.assertions = vec![
            assertion("package.nginx.installed", &json!(true)),
            assertion("package.nginx.version", &json!("1")),
            assertion(
                "file./etc/nginx/sites-available/default.exists",
                &json!(true),
            ),
            assertion(
                "file./etc/nginx/sites-available/default.mode",
                &json!("0644"),
            ),
            assertion(
                "file./etc/nginx/sites-available/default.sha256",
                &json!("a".repeat(64)),
            ),
        ];
        let mut document: serde_json::Value =
            serde_json::from_slice(&fixture("ansible_probe_callback.json"))?;
        document["plays"][0]["tasks"][0]["hosts"]["192.168.56.10"]["ansible_facts"]["packages"] =
            json!({});
        document["plays"][0]["tasks"][2]["hosts"]["192.168.56.10"]["results"][0]["stat"] =
            json!({"exists":false});
        let facts = extract_facts(&serde_json::to_vec(&document)?, &request, &profile())
            .map_err(AnsibleProbeTerminalStatus::diagnostic_code)?;
        assert_eq!(
            facts.get("package.nginx.installed"),
            Some(&ProbeFactValue::Boolean(false))
        );
        assert_eq!(
            facts.get("file./etc/nginx/sites-available/default.exists"),
            Some(&ProbeFactValue::Boolean(false))
        );
        assert_eq!(facts.len(), 2);
        assert_eq!(
            crate::ansible_probe::evaluate_assertions(&facts, &request.assertions)[3].status,
            AnsibleProbeAssertionStatus::FactUnknown
        );
        Ok(())
    }

    #[test]
    fn profile_validation_rejects_mutating_and_controller_actions()
    -> Result<(), Box<dyn std::error::Error>> {
        let request = request();
        let shipped =
            include_bytes!("../../../containers/ansible-probe/linux-nginx-probe-v1/playbook.yml");
        let profile = validate_playbook(shipped, &request)?;
        assert_eq!(profile.stat_paths.len(), 4);
        let original: serde_json::Value = serde_yaml::from_slice(shipped)?;
        for (pointer, value) in [
            ("/0/hosts", json!("all")),
            ("/0/gather_facts", json!(true)),
            (
                "/0/tasks/0/ansible.builtin.package_facts/manager",
                json!("{{ lookup('pipe', 'id') }}"),
            ),
            (
                "/0/tasks/2/ansible.builtin.stat/path",
                json!("{{ lookup('file', '/etc/shadow') }}"),
            ),
            (
                "/0/tasks/2/ansible.builtin.stat/checksum_algorithm",
                json!("sha1"),
            ),
        ] {
            let mut document = original.clone();
            *document
                .pointer_mut(pointer)
                .ok_or("missing profile pointer")? = value;
            assert!(validate_playbook(&serde_json::to_vec(&document)?, &request).is_err());
        }
        for (key, value) in [
            ("become", json!(true)),
            ("delegate_to", json!("localhost")),
            (
                "vars",
                json!({"ansible_ssh_common_args":"-o StrictHostKeyChecking=no"}),
            ),
            ("ansible.builtin.command", json!("id")),
            ("include_tasks", json!("other.yml")),
            ("register", json!("ansible_ssh_common_args")),
        ] {
            let mut document = original.clone();
            document[0]["tasks"][0][key] = value;
            assert!(validate_playbook(&serde_json::to_vec(&document)?, &request).is_err());
        }
        assert!(
            validate_playbook(
                b"- hosts: probe\n  hosts: all\n  gather_facts: false\n  tasks: []\n",
                &request
            )
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn runtime_uses_trusted_venv_callback_and_certificate_host_pin() {
        let command = playbook_command(&request());
        let command = command.as_std();
        let environment: std::collections::BTreeMap<_, _> = command.get_envs().collect();
        assert_eq!(command.get_program(), ANSIBLE_PLAYBOOK_PATH);
        assert_eq!(
            environment.get(std::ffi::OsStr::new("ANSIBLE_STDOUT_CALLBACK")),
            Some(&Some(std::ffi::OsStr::new("ansible.posix.json")))
        );
        assert_eq!(
            environment.get(std::ffi::OsStr::new("ANSIBLE_COLLECTIONS_PATH")),
            Some(&Some(std::ffi::OsStr::new(COLLECTIONS_PATH)))
        );
        assert!(!environment.contains_key(std::ffi::OsStr::new("PYTHONPATH")));
        let ssh = environment
            .get(std::ffi::OsStr::new("ANSIBLE_SSH_COMMON_ARGS"))
            .and_then(|value| *value)
            .and_then(|value| value.to_str())
            .unwrap_or_default();
        for required in [
            "StrictHostKeyChecking=yes",
            "IdentitiesOnly=yes",
            "GlobalKnownHostsFile=/dev/null",
            CERTIFICATE_PATH,
            KNOWN_HOSTS_PATH,
        ] {
            assert!(ssh.contains(required));
        }
    }

    #[test]
    fn ssh_identity_requires_evaluation_purpose_short_lifetime_and_matching_key()
    -> Result<(), Box<dyn std::error::Error>> {
        use russh::keys::ssh_key::{PrivateKey, certificate, private::Ed25519Keypair};
        let ca = PrivateKey::from(Ed25519Keypair::from_seed(&[0x41; 32]));
        let subject = PrivateKey::from(Ed25519Keypair::from_seed(&[0x42; 32]));
        let other = PrivateKey::from(Ed25519Keypair::from_seed(&[0x43; 32]));
        let now = 1_800_000_000;
        for (principal, after, before, key_matches, passes) in [
            ("labweaver-evaluation", now - 1, now + 299, true, true),
            ("labweaver-collector", now - 1, now + 299, true, false),
            ("labweaver", now - 1, now + 299, true, false),
            ("labweaver-evaluation", now + 1, now + 299, true, false),
            ("labweaver-evaluation", now - 10, now, true, false),
            ("labweaver-evaluation", now - 10, now + 299, true, false),
            ("labweaver-evaluation", now - 1, now + 299, false, false),
        ] {
            let mut builder = certificate::Builder::new(
                vec![0x44; certificate::Builder::RECOMMENDED_NONCE_SIZE],
                subject.public_key(),
                after,
                before,
            )?;
            builder
                .cert_type(certificate::CertType::User)?
                .valid_principal(principal)?;
            let certificate = builder.sign(&ca)?;
            assert_eq!(
                validate_certificate_identity(
                    if key_matches { &subject } else { &other },
                    &certificate,
                    now
                )
                .is_ok(),
                passes
            );
        }
        let mut multiple = certificate::Builder::new(
            vec![0x45; certificate::Builder::RECOMMENDED_NONCE_SIZE],
            subject.public_key(),
            now - 1,
            now + 299,
        )?;
        multiple
            .cert_type(certificate::CertType::User)?
            .valid_principal("labweaver-evaluation")?
            .valid_principal("labweaver-agent")?;
        assert!(validate_certificate_identity(&subject, &multiple.sign(&ca)?, now).is_err());
        Ok(())
    }

    #[test]
    fn receipt_distinguishes_observed_mismatch_from_unknown()
    -> Result<(), Box<dyn std::error::Error>> {
        let request = request();
        for (observed, known) in [(Some(false), 3), (None, 2)] {
            let mut facts = crate::ansible_probe::AnsibleProbeFacts::new();
            facts.insert("host.reachable", ProbeFactValue::Boolean(true))?;
            facts.insert(
                "file./etc/nginx/sites-available/default.mode",
                ProbeFactValue::Text("0644".to_owned()),
            )?;
            if let Some(active) = observed {
                facts.insert("service.nginx.active", ProbeFactValue::Boolean(active))?;
            }
            let outcome = ProbeOutcome::Evaluated {
                facts,
                duration_milliseconds: 1,
                output_bytes: 1,
            };
            let evidence = build_evidence(&request, request.request_sha256()?, &outcome)?;
            assert_eq!(
                evidence.terminal_status,
                AnsibleProbeTerminalStatus::AssertionsFailed
            );
            let receipt = receipt_for(&request, &evidence, &serde_jcs::to_vec(&evidence)?)?;
            assert_eq!(receipt.passed_assertions, 2);
            assert_eq!(receipt.known_assertions, known);
            assert_eq!(receipt.total_assertions, 3);
        }
        Ok(())
    }

    #[test]
    fn fail_closed_outcome_yields_empty_facts_and_unknown_assertions()
    -> Result<(), Box<dyn std::error::Error>> {
        let request = request();
        let outcome = ProbeOutcome::fail_closed(AnsibleProbeTerminalStatus::Timeout, 60_000, 0);
        let evidence = build_evidence(&request, request.request_sha256()?, &outcome)?;
        assert_eq!(
            evidence.terminal_status,
            AnsibleProbeTerminalStatus::Timeout
        );
        assert!(evidence.facts.is_empty());
        assert!(evidence.assertion_results.iter().all(|result| result.status
            == AnsibleProbeAssertionStatus::FactUnknown
            && !result.passed));
        evidence.validate_for(&request)?;
        let receipt = receipt_for(&request, &evidence, &serde_jcs::to_vec(&evidence)?)?;
        receipt.validate_for(&request)?;
        assert_eq!(receipt.passed_assertions, 0);
        assert_eq!(receipt.known_assertions, 0);
        assert_eq!(receipt.diagnostic_code, "LW_AP_TIMEOUT");
        Ok(())
    }

    #[test]
    fn inventory_profile_and_paths_are_fixed_and_injection_free() {
        let expected = request();
        let inventory = build_inventory(&expected);
        assert_eq!(
            inventory,
            format!(
                "[probe]\n192.168.56.10 ansible_user=labweaver ansible_port=22 \
ansible_ssh_private_key_file={PRIVATE_KEY_PATH} ansible_host_key_checking=True\n"
            )
        );
        assert_eq!(
            playbook_path(&expected.playbook_profile),
            PathBuf::from(format!(
                "{EVALUATOR_ROOT}/linux-nginx-probe-v1/playbook.yml"
            ))
        );
        assert_eq!(
            ANSIBLE_PLAYBOOK_PATH,
            "/opt/labweaver/probe/venv/bin/ansible-playbook"
        );
        assert_eq!(ANSIBLE_CONFIG_PATH, "/opt/labweaver/probe/ansible.cfg");

        assert!(require_supported_profile(&expected).is_ok());
        for profile in [
            "../playbook.yml",
            "linux-nginx-probe-v1/../../x",
            "linux//playbook.yml",
            "/input/evaluator/playbook.yml",
            "\\input\\evaluator\\playbook.yml",
        ] {
            let mut invalid = request();
            invalid.playbook_profile = profile.to_owned();
            assert_eq!(
                require_supported_profile(&invalid)
                    .err()
                    .map(|error| error.diagnostic_code()),
                Some("LW_AP_PROFILE_INVALID"),
                "profile {profile} must fail closed"
            );
        }
    }

    #[test]
    fn evaluated_facts_deterministically_drive_the_terminal_status()
    -> Result<(), Box<dyn std::error::Error>> {
        let request = request();
        let mut facts = AnsibleProbeFacts::new();
        facts.insert("host.reachable", ProbeFactValue::Boolean(true))?;
        facts.insert("service.nginx.active", ProbeFactValue::Boolean(false))?;
        facts.insert(
            "file./etc/nginx/sites-available/default.mode",
            ProbeFactValue::Text("0644".to_owned()),
        )?;
        let outcome = ProbeOutcome::Evaluated {
            facts,
            duration_milliseconds: 1_000,
            output_bytes: 512,
        };
        let evidence = build_evidence(&request, request.request_sha256()?, &outcome)?;
        assert_eq!(
            evidence.terminal_status,
            AnsibleProbeTerminalStatus::AssertionsFailed
        );
        evidence.validate_for(&request)?;
        Ok(())
    }
}
