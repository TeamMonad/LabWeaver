//! Shell-free Ansible probe worker executed only inside the isolated probe Kubernetes Job.
//!
//! The worker binds the stage-one data semantics to one frozen playbook profile:
//! it validates the mounted short-lived SSH identity at consumption time
//! (`IdentityExpired` covers an expired, not-yet-valid, over-long-lived, or
//! key-mismatched certificate), pins the target host key through a russh
//! handshake before writing `known_hosts`, runs the fixed `ansible-playbook`
//! with a scrubbed environment, and reduces the `ansible.builtin.json` callback
//! output to typed facts and payload-free evidence. Domain failures become
//! fail-closed terminal evidence; infrastructure failures abort with a stable
//! [`AnsibleProbeWorkerError`] diagnostic.
//!
//! Playbook contract (frozen image content): the playbook for
//! `linux-nginx-probe-v1` runs against one host (the target IPv4) and contains
//! exactly one task named `labweaver_probe_facts` whose per-host result carries
//! a flat `labweaver_probe_facts` object of fact name to boolean or string.
#![allow(
    missing_docs,
    clippy::too_many_lines,
    reason = "the closed worker path is intentionally explicit and stable diagnostics define failures"
)]

use std::{
    env, fs,
    io::Write as _,
    os::unix::process::CommandExt as _,
    path::{Path, PathBuf},
    process::{ExitStatus, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use persistence_sqlx::Sha256Digest; // internal persistence hash, not contract hash
use nix::{
    errno::Errno,
    sys::signal::{Signal, killpg},
    unistd::Pid,
};
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
const ANSIBLE_PLAYBOOK_PATH: &str = "/opt/labweaver/probe/bin/ansible-playbook";
const ANSIBLE_CONFIG_PATH: &str = "/opt/labweaver/probe/ansible.cfg";
const PLAYBOOK_ROOT: &str = "/opt/labweaver/probe";
const SUPPORTED_PLAYBOOK_PROFILE: &str = "linux-nginx-probe-v1";
const FACTS_TASK_NAME: &str = "labweaver_probe_facts";
const MAX_COMMAND_BYTES: u64 = 1024 * 1024;
const MAX_EVIDENCE_BYTES: u64 = 1024 * 1024;
const MAX_SECRET_MATERIAL_BYTES: u64 = 64 * 1024;
const CONNECT_TIMEOUT_SECONDS: u64 = 10;

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

/// Only the frozen v1 profile may execute; any other profile fails closed
/// before a process or network connection is attempted.
fn require_supported_profile(
    request: &AnsibleProbeExecutionRequest,
) -> Result<(), AnsibleProbeWorkerError> {
    let profile = request.playbook_profile.as_str();
    let safe_charset = profile
        .bytes()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
    if !safe_charset || profile != SUPPORTED_PLAYBOOK_PROFILE {
        return Err(AnsibleProbeWorkerError::ProfileInvalid);
    }
    Ok(())
}

fn playbook_path(playbook_profile: &str) -> PathBuf {
    Path::new(PLAYBOOK_ROOT)
        .join(playbook_profile)
        .join("playbook.yml")
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
    if let Err(status) = validate_ssh_identity(request) {
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
    let facts = match extract_facts(&process.stdout, request) {
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
/// to the mounted private key and probe username is `IdentityExpired`.
fn validate_ssh_identity(
    request: &AnsibleProbeExecutionRequest,
) -> Result<(), AnsibleProbeTerminalStatus> {
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
    let ttl_bound = now
        .checked_add(MAX_WALL_TIME_SECONDS)
        .ok_or(AnsibleProbeTerminalStatus::IdentityExpired)?;
    if certificate.cert_type() != russh::keys::ssh_key::certificate::CertType::User
        || certificate.valid_after() > now
        || certificate.valid_before() <= now
        || certificate.valid_before() > ttl_bound
        || certificate.public_key() != private_key.public_key().key_data()
        || certificate
            .valid_principals()
            .iter()
            .all(|principal| principal != &request.target.username)
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
ansible_ssh_private_key_file={key} ansible_ssh_certificate_file={certificate} \
ansible_ssh_known_hosts_file={known_hosts} ansible_host_key_checking=True\n",
        host = request.target.host,
        user = request.target.username,
        port = request.target.port,
        key = PRIVATE_KEY_PATH,
        certificate = CERTIFICATE_PATH,
        known_hosts = KNOWN_HOSTS_PATH,
    )
}

/// No user input ever reaches the command line: the binary, config, inventory,
/// and playbook paths are fixed image locations and the environment is scrubbed.
fn playbook_command(request: &AnsibleProbeExecutionRequest) -> Command {
    let mut command = Command::new(ANSIBLE_PLAYBOOK_PATH);
    command
        .env_clear()
        .env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .env("HOME", WORK_ROOT)
        .env("TMPDIR", WORK_ROOT)
        .env("ANSIBLE_CONFIG", ANSIBLE_CONFIG_PATH)
        .env("ANSIBLE_STDOUT_CALLBACK", "ansible.builtin.json")
        .env("ANSIBLE_FORCE_COLOR", "0")
        .env("ANSIBLE_DEPRECATION_WARNINGS", "0")
        .current_dir(WORK_ROOT)
        .arg("-i")
        .arg(INVENTORY_PATH)
        .arg(playbook_path(&request.playbook_profile));
    command
}

/// Reduces the `ansible.builtin.json` callback document to typed facts.
///
/// The stats gate runs first: unreachable maps to `HostUnreachable` and failed
/// tasks to `InfrastructureError`. A structurally broken document, a missing or
/// duplicated facts task, or any fact failing the bounded typed insert is
/// `FactsMalformed`.
fn extract_facts(
    stdout: &[u8],
    request: &AnsibleProbeExecutionRequest,
) -> Result<AnsibleProbeFacts, AnsibleProbeTerminalStatus> {
    let document: Value =
        serde_json::from_slice(stdout).map_err(|_| AnsibleProbeTerminalStatus::FactsMalformed)?;
    let host = request.target.host.to_string();
    let host_stats = document
        .get("stats")
        .and_then(Value::as_object)
        .and_then(|stats| stats.get(&host))
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
    let plays = document
        .get("plays")
        .and_then(Value::as_array)
        .filter(|plays| !plays.is_empty())
        .ok_or(AnsibleProbeTerminalStatus::FactsMalformed)?;
    let mut facts_object = None;
    for play in plays {
        let tasks = play
            .get("tasks")
            .and_then(Value::as_array)
            .ok_or(AnsibleProbeTerminalStatus::FactsMalformed)?;
        for task in tasks {
            let name = task
                .pointer("/task/name")
                .and_then(Value::as_str)
                .ok_or(AnsibleProbeTerminalStatus::FactsMalformed)?;
            if name == FACTS_TASK_NAME {
                if facts_object.is_some() {
                    return Err(AnsibleProbeTerminalStatus::FactsMalformed);
                }
                let host_result = task
                    .get("hosts")
                    .and_then(Value::as_object)
                    .and_then(|hosts| hosts.get(&host))
                    .ok_or(AnsibleProbeTerminalStatus::FactsMalformed)?;
                facts_object = Some(
                    host_result
                        .get(FACTS_TASK_NAME)
                        .and_then(Value::as_object)
                        .ok_or(AnsibleProbeTerminalStatus::FactsMalformed)?,
                );
            }
        }
    }
    let facts_object = facts_object.ok_or(AnsibleProbeTerminalStatus::FactsMalformed)?;
    let mut facts = AnsibleProbeFacts::new();
    for (name, value) in facts_object {
        let value = match value {
            Value::Bool(value) => ProbeFactValue::Boolean(*value),
            Value::String(value) => ProbeFactValue::Text(value.clone()),
            _ => return Err(AnsibleProbeTerminalStatus::FactsMalformed),
        };
        facts
            .insert(name, value)
            .map_err(|_| AnsibleProbeTerminalStatus::FactsMalformed)?;
    }
    Ok(facts)
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
            kill_process_group(child_id)?;
            let status = child
                .wait()
                .await
                .map_err(|_| AnsibleProbeWorkerError::ProcessIo)?;
            (status, Vec::new(), Vec::new(), true)
        };
    kill_process_group(child_id)?;
    Ok(CompletedProcess {
        status,
        stdout,
        stderr,
        timed_out,
        output_exceeded: exceeded.load(Ordering::Acquire),
    })
}

fn kill_process_group(process_id: u32) -> Result<(), AnsibleProbeWorkerError> {
    let process_group =
        Pid::from_raw(i32::try_from(process_id).map_err(|_| AnsibleProbeWorkerError::ProcessIo)?);
    match killpg(process_group, Signal::SIGKILL) {
        Ok(()) | Err(Errno::ESRCH) => Ok(()),
        Err(_) => Err(AnsibleProbeWorkerError::ProcessIo),
    }
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

    use persistence_sqlx::Sha256Digest;
    use contracts::evaluation::FactAssertion;
    use serde_json::json;
    use uuid::Uuid;

    use super::{
        ANSIBLE_CONFIG_PATH, ANSIBLE_PLAYBOOK_PATH, CERTIFICATE_PATH, KNOWN_HOSTS_PATH,
        PRIVATE_KEY_PATH, ProbeOutcome, SUPPORTED_PLAYBOOK_PROFILE, build_evidence,
        build_inventory, extract_facts, playbook_path, receipt_for, require_supported_profile,
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
            playbook_profile: SUPPORTED_PLAYBOOK_PROFILE.to_owned(),
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
        let request = request();
        let stdout = fixture("ansible_probe_callback.json");
        let facts = extract_facts(&stdout, &request)
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
        assert_eq!(receipt.passed_assertions, 3);
        assert_eq!(receipt.total_assertions, 3);

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
                extract_facts(document.as_bytes(), &request)
                    .err()
                    .map(AnsibleProbeTerminalStatus::diagnostic_code),
                Some(expected),
                "document {document} must map to {expected}"
            );
        }

        let duplicated = fixture("ansible_probe_callback_duplicated_task.json");
        assert_eq!(
            extract_facts(&duplicated, &request)
                .err()
                .map(AnsibleProbeTerminalStatus::diagnostic_code),
            Some("LW_AP_FACTS_MALFORMED")
        );
    }

    #[test]
    fn facts_mapping_is_bounded_and_typed() -> Result<(), Box<dyn std::error::Error>> {
        let request = request();
        let callback = |facts: serde_json::Value| {
            serde_json::to_vec(&json!({
                "plays":[{"tasks":[{
                    "hosts":{"192.168.56.10":{"labweaver_probe_facts":facts}},
                    "task":{"name":"labweaver_probe_facts"},
                }]}],
                "stats":{"192.168.56.10":{"unreachable":0,"failures":0,"ok":1}},
            }))
        };
        // Unknown fact families and non-scalar values are malformed.
        for facts in [
            json!({"tcp.80.open":true}),
            json!({"service.nginx.active":1}),
            json!({"service.nginx.state":["running"]}),
            json!({"service.nginx.state":"x".repeat(300)}),
        ] {
            assert_eq!(
                extract_facts(&callback(facts)?, &request)
                    .err()
                    .map(AnsibleProbeTerminalStatus::diagnostic_code),
                Some("LW_AP_FACTS_MALFORMED")
            );
        }
        // The fact count bound is enforced during insertion.
        let mut overflow = serde_json::Map::new();
        for index in 0..=crate::ansible_probe::MAX_FACTS {
            overflow.insert(format!("service.service-{index}.active"), json!(true));
        }
        assert_eq!(
            extract_facts(&callback(serde_json::Value::Object(overflow))?, &request)
                .err()
                .map(AnsibleProbeTerminalStatus::diagnostic_code),
            Some("LW_AP_FACTS_MALFORMED")
        );
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
ansible_ssh_private_key_file={PRIVATE_KEY_PATH} \
ansible_ssh_certificate_file={CERTIFICATE_PATH} \
ansible_ssh_known_hosts_file={KNOWN_HOSTS_PATH} ansible_host_key_checking=True\n"
            )
        );
        assert_eq!(
            playbook_path(&expected.playbook_profile),
            PathBuf::from("/opt/labweaver/probe/linux-nginx-probe-v1/playbook.yml")
        );
        assert_eq!(
            ANSIBLE_PLAYBOOK_PATH,
            "/opt/labweaver/probe/bin/ansible-playbook"
        );
        assert_eq!(ANSIBLE_CONFIG_PATH, "/opt/labweaver/probe/ansible.cfg");

        assert!(require_supported_profile(&expected).is_ok());
        for profile in [
            "other-profile",
            "Linux-Nginx-Probe-V1",
            "linux-nginx-probe-v1/../../x",
            "linux-nginx-probe-v2",
            "linux nginx probe v1",
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