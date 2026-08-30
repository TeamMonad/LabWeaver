//! Shell-free C++17 worker executed only inside the isolated OJ Kubernetes Job.
#![allow(
    missing_docs,
    clippy::too_many_lines,
    reason = "the closed worker path is intentionally explicit and stable diagnostics define failures"
)]

use std::{
    env,
    ffi::CString,
    fs,
    io::Write as _,
    os::unix::process::{CommandExt as _, ExitStatusExt as _},
    path::{Path, PathBuf},
    process::{ExitStatus, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

#[cfg(target_os = "linux")]
use landlock::{
    ABI, Access as _, AccessFs, CompatLevel, Compatible as _, Ruleset, RulesetAttr as _,
    RulesetCreatedAttr as _, RulesetStatus, path_beneath_rules,
};
use nix::{
    errno::Errno,
    libc,
    sys::resource::{Resource, setrlimit},
    sys::signal::{Signal, killpg},
    unistd::{Pid, execv},
};
use persistence_sqlx::Sha256Digest; // internal persistence hash, not contract hash
use tokio::{
    io::{AsyncRead, AsyncReadExt as _, AsyncWriteExt as _},
    process::Command,
    time::timeout,
};

use crate::{
    PvcSnapshotSource, SnapshotSource,
    oj::{
        OJ_EVIDENCE_RECEIPT_SCHEMA_VERSION, OJ_EVIDENCE_SCHEMA_VERSION, OjAggregate, OjCaseBinding,
        OjCaseEvidence, OjCaseStatus, OjError, OjEvidenceReceipt, OjExecutionEvidence,
        OjExecutionPhase, OjExecutionRequest, OjProcessEvidence, OjTerminalStatus,
        aggregate_case_evidence, check_output,
    },
};

const COMMAND_PATH_ENV: &str = "LABWEAVER_OJ_COMMAND_FILE";
const DEFAULT_COMMAND_PATH: &str = "/etc/labweaver/oj/command.json";
const SUBMISSION_ROOT: &str = "/input/submission";
const EVALUATOR_ROOT: &str = "/input/evaluator";
const WORK_ROOT: &str = "/work";
const EVIDENCE_PATH: &str = "/evidence/evidence.json";
const GXX_PATH: &str = "/usr/bin/g++";
const SERVICE_PATH: &str = "/usr/local/bin/labweaver-service";
const SUBMISSION_SOURCE_PATH: &str = "/work/submission.cpp";
const SUBMISSION_BINARY_PATH: &str = "/work/submission";
const COMPILE_HELPER_READY_PATH: &str = "/work/.compile-helper-ready";
const CASE_HELPER_READY_PATH: &str = "/work/.case-helper-ready";
const HELPER_READY_CONTENT: &[u8] = b"ready\n";
#[cfg(any(target_os = "linux", test))]
const SUBMISSION_READ_PATHS: [&str; 10] = [
    SUBMISSION_BINARY_PATH,
    "/lib",
    "/lib64",
    "/usr/lib",
    "/usr/lib64",
    "/usr/share",
    "/etc/ld.so.cache",
    "/etc/localtime",
    "/dev/null",
    "/dev/urandom",
];
#[cfg(any(target_os = "linux", test))]
const COMPILER_READ_PATHS: [&str; 11] = [
    "/usr/bin",
    "/usr/include",
    "/usr/lib",
    "/usr/lib64",
    "/usr/share",
    "/lib",
    "/lib64",
    "/etc/ld.so.cache",
    "/etc/localtime",
    "/dev/null",
    "/dev/urandom",
];
const MAX_COMMAND_BYTES: u64 = 1024 * 1024;
const MAX_EVIDENCE_BYTES: u64 = 1024 * 1024;
#[cfg(target_os = "linux")]
const MAX_SUBMISSION_PROCESSES: u64 = 64;
#[cfg(target_os = "linux")]
const MAX_SUBMISSION_CGROUP_PROCESSES: u64 = 128;

/// Executes one validated OJ request inside the isolated Kubernetes Job.
///
/// # Errors
///
/// Returns a stable [`OjWorkerError`] when any identity, input, process, or evidence check fails.
pub async fn run_oj_worker() -> Result<OjEvidenceReceipt, OjWorkerError> {
    let command_path = env::var_os(COMMAND_PATH_ENV)
        .map_or_else(|| PathBuf::from(DEFAULT_COMMAND_PATH), PathBuf::from);
    let request = read_request(&command_path)?;
    let request_sha256 = request.request_sha256()?;
    let submission =
        PvcSnapshotSource::open(Path::new(SUBMISSION_ROOT), request.submission_identity)
            .map_err(|_| OjWorkerError::SourceUnavailable)?;
    let source = read_verified(&submission, &request.source).await?;
    let source_path = PathBuf::from(SUBMISSION_SOURCE_PATH);
    write_new(&source_path, &source)?;
    let binary_path = Path::new(WORK_ROOT).join("submission");
    let compile = Box::pin(compile_cpp17(&request, &source_path, &binary_path)).await?;
    if !compile.status.success() || compile.capture.timed_out || compile.capture.output_exceeded {
        let evidence = compile_failure_evidence(&request, request_sha256, &compile)?;
        return persist_evidence(&request, &evidence);
    }
    if request.phase == OjExecutionPhase::Compile {
        let evidence = compile_success_evidence(&request, request_sha256, &compile)?;
        return persist_evidence(&request, &evidence);
    }
    let checker = request.checker.ok_or(OjWorkerError::CommandInvalid)?;
    let evaluator_identity = request
        .evaluator_identity
        .ok_or(OjWorkerError::CommandInvalid)?;
    let evaluator = PvcSnapshotSource::open(Path::new(EVALUATOR_ROOT), evaluator_identity)
        .map_err(|_| OjWorkerError::SourceUnavailable)?;

    let mut cases = Vec::with_capacity(request.cases.len());
    for case in &request.cases {
        let input = read_verified(&evaluator, &case.input).await?;
        let expected = read_verified(&evaluator, &case.expected).await?;
        cases.push(
            Box::pin(run_case(
                &request,
                case,
                &binary_path,
                &input,
                &expected,
                checker,
            ))
            .await?,
        );
    }
    let aggregate = aggregate_case_evidence(&request, &cases)?;
    let evidence = OjExecutionEvidence {
        schema_version: OJ_EVIDENCE_SCHEMA_VERSION.to_owned(),
        run_id: request.run_id,
        step_run_id: request.step_run_id,
        attempt_id: request.attempt_id,
        trace_id: request.trace_id.clone(),
        request_sha256,
        submission_identity: request.submission_identity,
        evaluator_identity: request.evaluator_identity,
        toolchain_profile: request.toolchain_profile.clone(),
        toolchain_image_digest: request.toolchain_image_digest.clone(),
        terminal_status: aggregate.status,
        diagnostic_code: aggregate.diagnostic_code.clone(),
        compile: compile.capture.to_evidence(compile.status)?,
        cases,
        aggregate,
    };
    evidence.validate_for(&request)?;
    persist_evidence(&request, &evidence)
}

fn read_request(path: &Path) -> Result<OjExecutionRequest, OjWorkerError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| OjWorkerError::CommandUnavailable)?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() > MAX_COMMAND_BYTES
    {
        return Err(OjWorkerError::CommandInvalid);
    }
    let bytes = fs::read(path).map_err(|_| OjWorkerError::CommandUnavailable)?;
    if bytes.is_empty()
        || u64::try_from(bytes.len()).map_err(|_| OjWorkerError::CommandInvalid)? != metadata.len()
    {
        return Err(OjWorkerError::CommandInvalid);
    }
    let request: OjExecutionRequest =
        serde_json::from_slice(&bytes).map_err(|_| OjWorkerError::CommandInvalid)?;
    request.validate()?;
    Ok(request)
}

async fn read_verified(
    source: &PvcSnapshotSource,
    binding: &crate::oj::OjFileBinding,
) -> Result<Vec<u8>, OjWorkerError> {
    let bytes = source
        .read_file(&binding.path, binding.size_bytes)
        .await
        .map_err(|_| OjWorkerError::SourceInvalid)?;
    if u64::try_from(bytes.len()).map_err(|_| OjWorkerError::SourceInvalid)? != binding.size_bytes
        || Sha256Digest::of_bytes(&bytes) != binding.sha256
    {
        return Err(OjWorkerError::SourceIdentityMismatch);
    }
    Ok(bytes)
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<(), OjWorkerError> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|_| OjWorkerError::WorkspaceInvalid)?;
    file.write_all(bytes)
        .map_err(|_| OjWorkerError::WorkspaceInvalid)?;
    file.sync_all().map_err(|_| OjWorkerError::WorkspaceInvalid)
}

struct CompletedProcess {
    status: ExitStatus,
    capture: ProcessCapture,
}

async fn compile_cpp17(
    request: &OjExecutionRequest,
    source: &Path,
    binary: &Path,
) -> Result<CompletedProcess, OjWorkerError> {
    if source != Path::new(SUBMISSION_SOURCE_PATH)
        || binary != Path::new(SUBMISSION_BINARY_PATH)
        || Path::new(COMPILE_HELPER_READY_PATH).exists()
    {
        return Err(OjWorkerError::WorkspaceInvalid);
    }
    let mut command = Command::new(SERVICE_PATH);
    command
        .env_clear()
        .env("TMPDIR", WORK_ROOT)
        .current_dir(WORK_ROOT)
        .arg("--mode")
        .arg("oj-compile-exec");
    let process = Box::pin(execute_process(
        &mut command,
        &[],
        request.limits.compile_wall_milliseconds,
        request.limits.output_bytes,
        None,
    ))
    .await?;
    consume_helper_ready(Path::new(COMPILE_HELPER_READY_PATH))?;
    Ok(process)
}

async fn run_case(
    request: &OjExecutionRequest,
    case: &OjCaseBinding,
    binary: &Path,
    input: &[u8],
    expected: &[u8],
    checker: crate::oj::OjCheckerKind,
) -> Result<OjCaseEvidence, OjWorkerError> {
    let cpu_seconds = request
        .limits
        .cpu_milliseconds
        .checked_add(999)
        .map(|milliseconds| milliseconds / 1000)
        .ok_or(OjWorkerError::LimitInvalid)?;
    let mut command = Command::new(SERVICE_PATH);
    command
        .env_clear()
        .current_dir(WORK_ROOT)
        .arg("--mode")
        .arg("oj-case-exec")
        .arg("--memory-bytes")
        .arg(request.limits.memory_bytes.to_string())
        .arg("--cpu-seconds")
        .arg(cpu_seconds.to_string())
        .arg("--file-bytes")
        .arg(request.limits.output_bytes.to_string());
    if binary != Path::new(SUBMISSION_BINARY_PATH) {
        return Err(OjWorkerError::WorkspaceInvalid);
    }
    if Path::new(CASE_HELPER_READY_PATH).exists() {
        return Err(OjWorkerError::WorkspaceInvalid);
    }
    let process = Box::pin(execute_process(
        &mut command,
        input,
        request.limits.run_wall_milliseconds,
        request.limits.output_bytes,
        Some(request.limits.memory_bytes),
    ))
    .await?;
    consume_helper_ready(Path::new(CASE_HELPER_READY_PATH))?;
    let status = classify_case(&process, request.limits.memory_bytes, checker, expected);
    let awarded_points = if status == OjCaseStatus::Accepted {
        case.max_points
    } else {
        0
    };
    Ok(OjCaseEvidence {
        case_id: case.id.clone(),
        status,
        actual_output_sha256: Sha256Digest::of_bytes(&process.capture.stdout),
        stdout_bytes: u64::try_from(process.capture.stdout.len())
            .map_err(|_| OjWorkerError::EvidenceInvalid)?,
        stderr_sha256: Sha256Digest::of_bytes(&process.capture.stderr),
        stderr_bytes: u64::try_from(process.capture.stderr.len())
            .map_err(|_| OjWorkerError::EvidenceInvalid)?,
        duration_milliseconds: process.capture.duration_milliseconds,
        peak_memory_bytes: process.capture.peak_memory_bytes,
        awarded_points,
        diagnostic_code: status.diagnostic_code().to_owned(),
    })
}

fn classify_case(
    process: &CompletedProcess,
    memory_limit: u64,
    checker: crate::oj::OjCheckerKind,
    expected: &[u8],
) -> OjCaseStatus {
    if process.capture.output_exceeded {
        return OjCaseStatus::OutputLimitExceeded;
    }
    let signal = process.status.signal();
    if process.capture.timed_out || signal == Some(libc::SIGXCPU) {
        return OjCaseStatus::TimeLimitExceeded;
    }
    let near_memory_limit = process
        .capture
        .peak_virtual_memory_bytes
        .and_then(|bytes| bytes.checked_mul(10))
        .is_some_and(|scaled| scaled >= memory_limit.saturating_mul(8));
    if near_memory_limit && matches!(signal, Some(6 | 11)) {
        return OjCaseStatus::MemoryLimitExceeded;
    }
    if signal == Some(25) {
        return OjCaseStatus::OutputLimitExceeded;
    }
    if !process.status.success() {
        return OjCaseStatus::RuntimeError;
    }
    if check_output(checker, &process.capture.stdout, expected) {
        OjCaseStatus::Accepted
    } else {
        OjCaseStatus::WrongAnswer
    }
}

/// Applies process rlimits and replaces the worker helper with the fixed submission binary.
///
/// # Errors
///
/// Returns a stable [`OjWorkerError`] when limits are invalid, cannot be applied, or exec fails.
pub fn run_oj_case_exec(
    memory_bytes: u64,
    cpu_seconds: u64,
    file_bytes: u64,
) -> Result<(), OjWorkerError> {
    if !(crate::oj::MIN_MEMORY_BYTES..=crate::oj::MAX_MEMORY_BYTES).contains(&memory_bytes)
        || cpu_seconds == 0
        || cpu_seconds > 30
        || file_bytes == 0
        || file_bytes > crate::oj::MAX_OUTPUT_BYTES
    {
        return Err(OjWorkerError::LimitInvalid);
    }
    let mut ready = create_helper_ready(Path::new(CASE_HELPER_READY_PATH))?;
    setrlimit(Resource::RLIMIT_AS, memory_bytes, memory_bytes)
        .map_err(|_| OjWorkerError::LimitApply)?;
    let hard_cpu_seconds = cpu_seconds
        .checked_add(1)
        .ok_or(OjWorkerError::LimitInvalid)?;
    setrlimit(Resource::RLIMIT_CPU, cpu_seconds, hard_cpu_seconds)
        .map_err(|_| OjWorkerError::LimitApply)?;
    setrlimit(Resource::RLIMIT_FSIZE, file_bytes, file_bytes)
        .map_err(|_| OjWorkerError::LimitApply)?;
    setrlimit(Resource::RLIMIT_CORE, 0, 0).map_err(|_| OjWorkerError::LimitApply)?;
    require_submission_cgroup_process_limit()?;
    apply_submission_process_limit()?;
    apply_submission_filesystem_sandbox()?;
    apply_submission_syscall_sandbox()?;
    mark_helper_ready(&mut ready)?;
    let binary =
        CString::new(SUBMISSION_BINARY_PATH).map_err(|_| OjWorkerError::WorkspaceInvalid)?;
    match execv(&binary, std::slice::from_ref(&binary)) {
        Ok(never) => match never {},
        Err(_) => Err(OjWorkerError::ProcessSpawn),
    }
}

/// Applies the compiler filesystem sandbox and replaces the helper with the fixed compiler.
///
/// # Errors
///
/// Returns a stable [`OjWorkerError`] when the workspace, sandbox, or compiler exec is invalid.
pub fn run_oj_compile_exec() -> Result<(), OjWorkerError> {
    if !Path::new(SUBMISSION_SOURCE_PATH).is_file()
        || !Path::new(GXX_PATH).is_file()
        || Path::new(SUBMISSION_BINARY_PATH).exists()
    {
        return Err(OjWorkerError::WorkspaceInvalid);
    }
    let mut ready = create_helper_ready(Path::new(COMPILE_HELPER_READY_PATH))?;
    apply_compiler_filesystem_sandbox()?;
    mark_helper_ready(&mut ready)?;
    let arguments = [
        GXX_PATH,
        "-std=c++17",
        "-O2",
        "-pipe",
        "-fno-diagnostics-color",
        "-o",
        SUBMISSION_BINARY_PATH,
        "--",
        SUBMISSION_SOURCE_PATH,
    ]
    .into_iter()
    .map(CString::new)
    .collect::<Result<Vec<_>, _>>()
    .map_err(|_| OjWorkerError::WorkspaceInvalid)?;
    let compiler = arguments.first().ok_or(OjWorkerError::WorkspaceInvalid)?;
    match execv(compiler, &arguments) {
        Ok(never) => match never {},
        Err(_) => Err(OjWorkerError::ProcessSpawn),
    }
}

fn create_helper_ready(path: &Path) -> Result<fs::File, OjWorkerError> {
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|_| OjWorkerError::WorkspaceInvalid)
}

fn mark_helper_ready(file: &mut fs::File) -> Result<(), OjWorkerError> {
    file.write_all(HELPER_READY_CONTENT)
        .map_err(|_| OjWorkerError::WorkspaceInvalid)?;
    file.sync_all().map_err(|_| OjWorkerError::WorkspaceInvalid)
}

fn consume_helper_ready(path: &Path) -> Result<(), OjWorkerError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| OjWorkerError::SandboxUnavailable)?;
    let expected_size =
        u64::try_from(HELPER_READY_CONTENT.len()).map_err(|_| OjWorkerError::WorkspaceInvalid)?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() != expected_size
        || fs::read(path).map_err(|_| OjWorkerError::SandboxUnavailable)? != HELPER_READY_CONTENT
    {
        return Err(OjWorkerError::SandboxUnavailable);
    }
    fs::remove_file(path).map_err(|_| OjWorkerError::WorkspaceInvalid)
}

#[cfg(target_os = "linux")]
fn apply_submission_filesystem_sandbox() -> Result<(), OjWorkerError> {
    if !Path::new(SUBMISSION_BINARY_PATH).is_file() {
        return Err(OjWorkerError::WorkspaceInvalid);
    }
    let abi = ABI::V3;
    let readable = SUBMISSION_READ_PATHS
        .into_iter()
        .filter(|path| Path::new(path).exists());
    let status = Ruleset::default()
        .set_compatibility(CompatLevel::HardRequirement)
        .handle_access(AccessFs::from_all(abi))
        .map_err(|_| OjWorkerError::SandboxUnavailable)?
        .create()
        .map_err(|_| OjWorkerError::SandboxUnavailable)?
        .add_rules(path_beneath_rules(readable, AccessFs::from_read(abi)))
        .map_err(|_| OjWorkerError::SandboxUnavailable)?
        .restrict_self()
        .map_err(|_| OjWorkerError::SandboxUnavailable)?;
    if status.ruleset != RulesetStatus::FullyEnforced || !status.no_new_privs {
        return Err(OjWorkerError::SandboxUnavailable);
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn apply_submission_process_limit() -> Result<(), OjWorkerError> {
    setrlimit(
        Resource::RLIMIT_NPROC,
        MAX_SUBMISSION_PROCESSES,
        MAX_SUBMISSION_PROCESSES,
    )
    .map_err(|_| OjWorkerError::LimitApply)
}

#[cfg(target_os = "linux")]
fn require_submission_cgroup_process_limit() -> Result<(), OjWorkerError> {
    let membership =
        fs::read_to_string("/proc/self/cgroup").map_err(|_| OjWorkerError::LimitApply)?;
    let path = membership
        .lines()
        .find_map(|line| {
            let mut fields = line.splitn(3, ':');
            let hierarchy = fields.next()?;
            let controllers = fields.next()?;
            let path = fields.next()?;
            (hierarchy == "0" && controllers.is_empty()).then_some(path)
        })
        .and_then(cgroup_v2_pids_max_path)
        .ok_or(OjWorkerError::LimitApply)?;
    let root = Path::new("/sys/fs/cgroup");
    let mut directory = path.parent().ok_or(OjWorkerError::LimitApply)?;
    let mut effective_limit = None;
    loop {
        let value = fs::read_to_string(directory.join("pids.max"))
            .map_err(|_| OjWorkerError::LimitApply)?;
        if let Some(limit) = parse_cgroup_pids_max(&value)? {
            effective_limit =
                Some(effective_limit.map_or(limit, |current: u64| current.min(limit)));
        }
        if directory == root {
            break;
        }
        directory = directory
            .parent()
            .filter(|parent| parent.starts_with(root))
            .ok_or(OjWorkerError::LimitApply)?;
    }
    effective_limit
        .filter(|limit| (2..=MAX_SUBMISSION_CGROUP_PROCESSES).contains(limit))
        .map(|_| ())
        .ok_or(OjWorkerError::LimitApply)
}

#[cfg(target_os = "linux")]
fn cgroup_v2_pids_max_path(membership: &str) -> Option<PathBuf> {
    let relative = membership.strip_prefix('/')?;
    if Path::new(relative)
        .components()
        .any(|component| !matches!(component, std::path::Component::Normal(_)))
        && !relative.is_empty()
    {
        return None;
    }
    Some(Path::new("/sys/fs/cgroup").join(relative).join("pids.max"))
}

#[cfg(target_os = "linux")]
fn parse_cgroup_pids_max(value: &str) -> Result<Option<u64>, OjWorkerError> {
    let value = value.trim();
    if value == "max" {
        Ok(None)
    } else {
        value
            .parse::<u64>()
            .map(Some)
            .map_err(|_| OjWorkerError::LimitApply)
    }
}

#[cfg(target_os = "linux")]
fn apply_submission_syscall_sandbox() -> Result<(), OjWorkerError> {
    use seccompiler::{
        BpfProgram, SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition, SeccompFilter,
        SeccompRule, TargetArch,
    };
    use std::collections::BTreeMap;

    const CLONE_NAMESPACE_FLAGS: [u64; 7] = [
        0x0002_0000,
        0x0200_0000,
        0x0400_0000,
        0x0800_0000,
        0x1000_0000,
        0x2000_0000,
        0x4000_0000,
    ];

    let target_arch = TargetArch::try_from(std::env::consts::ARCH)
        .map_err(|_| OjWorkerError::SandboxUnavailable)?;
    let clone_rules = CLONE_NAMESPACE_FLAGS
        .into_iter()
        .map(|flag| {
            SeccompCondition::new(
                0,
                SeccompCmpArgLen::Qword,
                SeccompCmpOp::MaskedEq(flag),
                flag,
            )
            .and_then(|condition| SeccompRule::new(vec![condition]))
            .map_err(|_| OjWorkerError::SandboxUnavailable)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let denied_syscalls = [
        (libc::SYS_setsid, Vec::new()),
        (libc::SYS_setpgid, Vec::new()),
        (libc::SYS_unshare, Vec::new()),
        (libc::SYS_setns, Vec::new()),
        (libc::SYS_clone, clone_rules),
    ]
    .into_iter()
    .collect::<BTreeMap<_, _>>();
    let denied_program: BpfProgram = SeccompFilter::new(
        denied_syscalls,
        SeccompAction::Allow,
        SeccompAction::Errno(
            u32::try_from(libc::EPERM).map_err(|_| OjWorkerError::SandboxUnavailable)?,
        ),
        target_arch,
    )
    .and_then(TryInto::try_into)
    .map_err(|_| OjWorkerError::SandboxUnavailable)?;
    seccompiler::apply_filter(&denied_program).map_err(|_| OjWorkerError::SandboxUnavailable)?;

    let clone3_program: BpfProgram = SeccompFilter::new(
        [(libc::SYS_clone3, Vec::new())].into_iter().collect(),
        SeccompAction::Allow,
        SeccompAction::Errno(
            u32::try_from(libc::ENOSYS).map_err(|_| OjWorkerError::SandboxUnavailable)?,
        ),
        target_arch,
    )
    .and_then(TryInto::try_into)
    .map_err(|_| OjWorkerError::SandboxUnavailable)?;
    seccompiler::apply_filter(&clone3_program).map_err(|_| OjWorkerError::SandboxUnavailable)
}

#[cfg(target_os = "linux")]
fn apply_compiler_filesystem_sandbox() -> Result<(), OjWorkerError> {
    let abi = ABI::V3;
    let readable = COMPILER_READ_PATHS
        .into_iter()
        .filter(|path| Path::new(path).exists());
    let writable = [WORK_ROOT]
        .into_iter()
        .filter(|path| Path::new(path).is_dir());
    let status = Ruleset::default()
        .set_compatibility(CompatLevel::HardRequirement)
        .handle_access(AccessFs::from_all(abi))
        .map_err(|_| OjWorkerError::SandboxUnavailable)?
        .create()
        .map_err(|_| OjWorkerError::SandboxUnavailable)?
        .add_rules(path_beneath_rules(readable, AccessFs::from_read(abi)))
        .map_err(|_| OjWorkerError::SandboxUnavailable)?
        .add_rules(path_beneath_rules(writable, AccessFs::from_all(abi)))
        .map_err(|_| OjWorkerError::SandboxUnavailable)?
        .restrict_self()
        .map_err(|_| OjWorkerError::SandboxUnavailable)?;
    if status.ruleset != RulesetStatus::FullyEnforced || !status.no_new_privs {
        return Err(OjWorkerError::SandboxUnavailable);
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn apply_submission_filesystem_sandbox() -> Result<(), OjWorkerError> {
    Err(OjWorkerError::SandboxUnavailable)
}

#[cfg(not(target_os = "linux"))]
fn apply_submission_process_limit() -> Result<(), OjWorkerError> {
    Err(OjWorkerError::SandboxUnavailable)
}

#[cfg(not(target_os = "linux"))]
fn require_submission_cgroup_process_limit() -> Result<(), OjWorkerError> {
    Err(OjWorkerError::SandboxUnavailable)
}

#[cfg(not(target_os = "linux"))]
fn apply_submission_syscall_sandbox() -> Result<(), OjWorkerError> {
    Err(OjWorkerError::SandboxUnavailable)
}

#[cfg(not(target_os = "linux"))]
fn apply_compiler_filesystem_sandbox() -> Result<(), OjWorkerError> {
    Err(OjWorkerError::SandboxUnavailable)
}

fn compile_failure_evidence(
    request: &OjExecutionRequest,
    request_sha256: Sha256Digest,
    compile: &CompletedProcess,
) -> Result<OjExecutionEvidence, OjWorkerError> {
    let total_cases =
        u32::try_from(request.cases.len()).map_err(|_| OjWorkerError::EvidenceInvalid)?;
    let aggregate = OjAggregate {
        status: OjTerminalStatus::CompileError,
        awarded_points: 0,
        max_points: request.score_max_points,
        passed_cases: 0,
        total_cases,
        diagnostic_code: OjTerminalStatus::CompileError.diagnostic_code().to_owned(),
    };
    let evidence = OjExecutionEvidence {
        schema_version: OJ_EVIDENCE_SCHEMA_VERSION.to_owned(),
        run_id: request.run_id,
        step_run_id: request.step_run_id,
        attempt_id: request.attempt_id,
        trace_id: request.trace_id.clone(),
        request_sha256,
        submission_identity: request.submission_identity,
        evaluator_identity: request.evaluator_identity,
        toolchain_profile: request.toolchain_profile.clone(),
        toolchain_image_digest: request.toolchain_image_digest.clone(),
        terminal_status: OjTerminalStatus::CompileError,
        diagnostic_code: OjTerminalStatus::CompileError.diagnostic_code().to_owned(),
        compile: compile.capture.to_evidence(compile.status)?,
        cases: Vec::new(),
        aggregate,
    };
    evidence.validate_for(request)?;
    Ok(evidence)
}

fn compile_success_evidence(
    request: &OjExecutionRequest,
    request_sha256: Sha256Digest,
    compile: &CompletedProcess,
) -> Result<OjExecutionEvidence, OjWorkerError> {
    let aggregate = OjAggregate {
        status: OjTerminalStatus::Accepted,
        awarded_points: 0,
        max_points: 0,
        passed_cases: 0,
        total_cases: 0,
        diagnostic_code: OjTerminalStatus::Accepted.diagnostic_code().to_owned(),
    };
    let evidence = OjExecutionEvidence {
        schema_version: OJ_EVIDENCE_SCHEMA_VERSION.to_owned(),
        run_id: request.run_id,
        step_run_id: request.step_run_id,
        attempt_id: request.attempt_id,
        trace_id: request.trace_id.clone(),
        request_sha256,
        submission_identity: request.submission_identity,
        evaluator_identity: request.evaluator_identity,
        toolchain_profile: request.toolchain_profile.clone(),
        toolchain_image_digest: request.toolchain_image_digest.clone(),
        terminal_status: OjTerminalStatus::Accepted,
        diagnostic_code: OjTerminalStatus::Accepted.diagnostic_code().to_owned(),
        compile: compile.capture.to_evidence(compile.status)?,
        cases: Vec::new(),
        aggregate,
    };
    evidence.validate_for(request)?;
    Ok(evidence)
}

fn persist_evidence(
    request: &OjExecutionRequest,
    evidence: &OjExecutionEvidence,
) -> Result<OjEvidenceReceipt, OjWorkerError> {
    evidence.validate_for(request)?;
    let bytes = serde_jcs::to_vec(evidence).map_err(|_| OjWorkerError::EvidenceInvalid)?;
    let size_bytes = u64::try_from(bytes.len()).map_err(|_| OjWorkerError::EvidenceInvalid)?;
    if bytes.is_empty() || size_bytes > MAX_EVIDENCE_BYTES {
        return Err(OjWorkerError::EvidenceInvalid);
    }
    write_new(Path::new(EVIDENCE_PATH), &bytes)?;
    let receipt = OjEvidenceReceipt {
        schema_version: OJ_EVIDENCE_RECEIPT_SCHEMA_VERSION.to_owned(),
        run_id: evidence.run_id,
        step_run_id: evidence.step_run_id,
        attempt_id: evidence.attempt_id,
        trace_id: evidence.trace_id.clone(),
        request_sha256: evidence.request_sha256,
        evidence_sha256: Sha256Digest::of_bytes(&bytes),
        evidence_size_bytes: size_bytes,
        terminal_status: evidence.terminal_status,
        diagnostic_code: evidence.diagnostic_code.clone(),
        awarded_points: evidence.aggregate.awarded_points,
        max_points: evidence.aggregate.max_points,
    };
    receipt.validate_for(request)?;
    Ok(receipt)
}

struct ProcessCapture {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    duration_milliseconds: u64,
    peak_memory_bytes: Option<u64>,
    peak_virtual_memory_bytes: Option<u64>,
    timed_out: bool,
    output_exceeded: bool,
}

impl ProcessCapture {
    fn to_evidence(&self, status: ExitStatus) -> Result<OjProcessEvidence, OjWorkerError> {
        Ok(OjProcessEvidence {
            exit_code: status.code(),
            signal: status.signal(),
            stdout_sha256: Sha256Digest::of_bytes(&self.stdout),
            stdout_bytes: u64::try_from(self.stdout.len())
                .map_err(|_| OjWorkerError::EvidenceInvalid)?,
            stderr_sha256: Sha256Digest::of_bytes(&self.stderr),
            stderr_bytes: u64::try_from(self.stderr.len())
                .map_err(|_| OjWorkerError::EvidenceInvalid)?,
            duration_milliseconds: self.duration_milliseconds,
            peak_memory_bytes: self.peak_memory_bytes,
            timed_out: self.timed_out,
            output_exceeded: self.output_exceeded,
        })
    }
}

async fn execute_process(
    command: &mut Command,
    input: &[u8],
    wall_milliseconds: u64,
    output_limit: u64,
    memory_limit: Option<u64>,
) -> Result<CompletedProcess, OjWorkerError> {
    command.as_std_mut().process_group(0);
    command
        .kill_on_drop(true)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let started = Instant::now();
    let mut child = command.spawn().map_err(|_| OjWorkerError::ProcessSpawn)?;
    let child_id = child.id().ok_or(OjWorkerError::ProcessSpawn)?;
    let monitor_stop = Arc::new(AtomicBool::new(false));
    let monitor = memory_limit
        .map(|_| tokio::spawn(monitor_peak_memory(child_id, Arc::clone(&monitor_stop))));
    let mut stdin = child.stdin.take().ok_or(OjWorkerError::ProcessSpawn)?;
    let stdout = child.stdout.take().ok_or(OjWorkerError::ProcessSpawn)?;
    let stderr = child.stderr.take().ok_or(OjWorkerError::ProcessSpawn)?;
    let total = Arc::new(AtomicU64::new(0));
    let exceeded = Arc::new(AtomicBool::new(false));
    let output = async {
        let write_input = async {
            stdin
                .write_all(input)
                .await
                .map_err(|_| OjWorkerError::ProcessIo)?;
            stdin.shutdown().await.map_err(|_| OjWorkerError::ProcessIo)
        };
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
        let (write, stdout, stderr, status) =
            tokio::join!(write_input, stdout_read, stderr_read, wait);
        write?;
        let stdout = stdout?;
        let stderr = stderr?;
        let status = status.map_err(|_| OjWorkerError::ProcessIo)?;
        Ok::<_, OjWorkerError>((status, stdout, stderr))
    };
    let wall = Duration::from_millis(wall_milliseconds);
    let (status, stdout, stderr, timed_out) =
        if let Ok(result) = Box::pin(timeout(wall, output)).await {
            let (status, stdout, stderr) = result?;
            (status, stdout, stderr, false)
        } else {
            kill_process_group(child_id)?;
            let status = child.wait().await.map_err(|_| OjWorkerError::ProcessIo)?;
            (status, Vec::new(), Vec::new(), true)
        };
    kill_process_group(child_id)?;
    monitor_stop.store(true, Ordering::Release);
    let peak_memory = match monitor {
        Some(monitor) => monitor.await.map_err(|_| OjWorkerError::EvidenceInvalid)?,
        None => MemoryObservation::default(),
    };
    let duration_milliseconds =
        u64::try_from(started.elapsed().as_millis()).map_err(|_| OjWorkerError::EvidenceInvalid)?;
    Ok(CompletedProcess {
        status,
        capture: ProcessCapture {
            stdout,
            stderr,
            duration_milliseconds,
            peak_memory_bytes: peak_memory.peak_resident_bytes,
            peak_virtual_memory_bytes: peak_memory.peak_virtual_bytes,
            timed_out,
            output_exceeded: exceeded.load(Ordering::Acquire),
        },
    })
}

fn kill_process_group(process_id: u32) -> Result<(), OjWorkerError> {
    let process_group =
        Pid::from_raw(i32::try_from(process_id).map_err(|_| OjWorkerError::ProcessIo)?);
    match killpg(process_group, Signal::SIGKILL) {
        Ok(()) | Err(Errno::ESRCH) => Ok(()),
        Err(_) => Err(OjWorkerError::ProcessIo),
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct MemoryObservation {
    peak_resident_bytes: Option<u64>,
    peak_virtual_bytes: Option<u64>,
}

async fn monitor_peak_memory(process_id: u32, stop: Arc<AtomicBool>) -> MemoryObservation {
    let status_path = PathBuf::from(format!("/proc/{process_id}/status"));
    let executable_path = PathBuf::from(format!("/proc/{process_id}/exe"));
    let mut observation = MemoryObservation::default();
    while !stop.load(Ordering::Acquire) {
        let is_submission = fs::read_link(&executable_path)
            .ok()
            .is_some_and(|path| path == Path::new(SUBMISSION_BINARY_PATH));
        if is_submission {
            let Some(status) = fs::read_to_string(&status_path).ok() else {
                break;
            };
            for line in status.lines() {
                if let Some(value) = line.strip_prefix("VmRSS:") {
                    observation.peak_resident_bytes =
                        max_memory(observation.peak_resident_bytes, parse_proc_kib(value));
                } else if let Some(value) = line.strip_prefix("VmSize:") {
                    observation.peak_virtual_bytes =
                        max_memory(observation.peak_virtual_bytes, parse_proc_kib(value));
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    observation
}

fn parse_proc_kib(value: &str) -> Option<u64> {
    let mut fields = value.split_ascii_whitespace();
    let kibibytes = fields.next()?.parse::<u64>().ok()?;
    if fields.next()? != "kB" || fields.next().is_some() {
        return None;
    }
    kibibytes.checked_mul(1024)
}

fn max_memory(current: Option<u64>, candidate: Option<u64>) -> Option<u64> {
    match (current, candidate) {
        (Some(current), Some(candidate)) => Some(current.max(candidate)),
        (Some(current), None) => Some(current),
        (None, candidate) => candidate,
    }
}

async fn drain_bounded<R: AsyncRead + Unpin>(
    mut reader: R,
    total: Arc<AtomicU64>,
    exceeded: Arc<AtomicBool>,
    limit: u64,
) -> Result<Vec<u8>, OjWorkerError> {
    let mut captured = Vec::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let read = reader
            .read(&mut buffer)
            .await
            .map_err(|_| OjWorkerError::ProcessIo)?;
        if read == 0 {
            return Ok(captured);
        }
        let read_u64 = u64::try_from(read).map_err(|_| OjWorkerError::ProcessIo)?;
        let previous = total.fetch_add(read_u64, Ordering::AcqRel);
        let remaining = limit.saturating_sub(previous);
        let keep =
            usize::try_from(remaining.min(read_u64)).map_err(|_| OjWorkerError::ProcessIo)?;
        captured.extend_from_slice(&buffer[..keep]);
        if read_u64 > remaining {
            exceeded.store(true, Ordering::Release);
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum OjWorkerError {
    #[error("OJ worker command is unavailable")]
    CommandUnavailable,
    #[error("OJ worker command is invalid")]
    CommandInvalid,
    #[error("OJ source volume is unavailable")]
    SourceUnavailable,
    #[error("OJ source file is invalid")]
    SourceInvalid,
    #[error("OJ source hash or size does not match")]
    SourceIdentityMismatch,
    #[error("OJ work volume is invalid")]
    WorkspaceInvalid,
    #[error("OJ process could not be spawned")]
    ProcessSpawn,
    #[error("OJ process IO failed")]
    ProcessIo,
    #[error("OJ execution limit is invalid")]
    LimitInvalid,
    #[error("OJ process limits could not be applied")]
    LimitApply,
    #[error("OJ submission filesystem sandbox is unavailable")]
    SandboxUnavailable,
    #[error("OJ evidence is invalid")]
    EvidenceInvalid,
    #[error(transparent)]
    Contract(#[from] OjError),
}

impl OjWorkerError {
    #[must_use]
    pub const fn diagnostic_code(&self) -> &'static str {
        match self {
            Self::CommandUnavailable => "LW_OJ_COMMAND_UNAVAILABLE",
            Self::CommandInvalid => "LW_OJ_COMMAND_INVALID",
            Self::SourceUnavailable => "LW_OJ_SOURCE_UNAVAILABLE",
            Self::SourceInvalid => "LW_OJ_SOURCE_INVALID",
            Self::SourceIdentityMismatch => "LW_OJ_SOURCE_IDENTITY_MISMATCH",
            Self::WorkspaceInvalid => "LW_OJ_WORKSPACE_INVALID",
            Self::ProcessSpawn => "LW_OJ_PROCESS_SPAWN_FAILED",
            Self::ProcessIo => "LW_OJ_PROCESS_IO_FAILED",
            Self::LimitInvalid => "LW_OJ_LIMIT_INVALID",
            Self::LimitApply => "LW_OJ_LIMIT_APPLY_FAILED",
            Self::SandboxUnavailable => "LW_OJ_SANDBOX_UNAVAILABLE",
            Self::EvidenceInvalid => "LW_OJ_EVIDENCE_INVALID",
            Self::Contract(error) => error.diagnostic_code(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::process::ExitStatusExt as _;
    #[cfg(target_os = "linux")]
    use std::process::Command as StdCommand;

    use super::{
        COMMAND_PATH_ENV, COMPILER_READ_PATHS, CompletedProcess, EVALUATOR_ROOT, ProcessCapture,
        SUBMISSION_READ_PATHS, classify_case, consume_helper_ready, create_helper_ready,
        mark_helper_ready,
    };
    #[cfg(target_os = "linux")]
    use super::{
        apply_submission_process_limit, apply_submission_syscall_sandbox, cgroup_v2_pids_max_path,
        parse_cgroup_pids_max,
    };
    use crate::oj::{OjCaseStatus, OjCheckerKind};

    fn process(raw_status: i32, stdout: &[u8]) -> CompletedProcess {
        CompletedProcess {
            status: std::process::ExitStatus::from_raw(raw_status),
            capture: ProcessCapture {
                stdout: stdout.to_vec(),
                stderr: Vec::new(),
                duration_milliseconds: 1,
                peak_memory_bytes: None,
                peak_virtual_memory_bytes: None,
                timed_out: false,
                output_exceeded: false,
            },
        }
    }

    #[test]
    fn case_classification_is_closed_and_deterministic() {
        let accepted = process(0, b"42\n");
        assert_eq!(
            classify_case(&accepted, 32 * 1024 * 1024, OjCheckerKind::Exact, b"42\n"),
            OjCaseStatus::Accepted
        );
        assert_eq!(
            classify_case(&accepted, 32 * 1024 * 1024, OjCheckerKind::Exact, b"41\n"),
            OjCaseStatus::WrongAnswer
        );

        let memory = process(11, b"");
        let mut memory = memory;
        memory.capture.peak_virtual_memory_bytes = Some(31 * 1024 * 1024);
        assert_eq!(
            classify_case(&memory, 32 * 1024 * 1024, OjCheckerKind::Exact, b""),
            OjCaseStatus::MemoryLimitExceeded
        );

        let cpu = process(24, b"");
        assert_eq!(
            classify_case(&cpu, 32 * 1024 * 1024, OjCheckerKind::Exact, b""),
            OjCaseStatus::TimeLimitExceeded
        );

        let self_sigkill = process(9, b"");
        assert_eq!(
            classify_case(&self_sigkill, 32 * 1024 * 1024, OjCheckerKind::Exact, b""),
            OjCaseStatus::RuntimeError
        );

        let mut wall_timeout = process(9, b"");
        wall_timeout.capture.timed_out = true;
        assert_eq!(
            classify_case(&wall_timeout, 32 * 1024 * 1024, OjCheckerKind::Exact, b""),
            OjCaseStatus::TimeLimitExceeded
        );

        let runtime = process(256, b"");
        assert_eq!(
            classify_case(&runtime, 32 * 1024 * 1024, OjCheckerKind::Exact, b""),
            OjCaseStatus::RuntimeError
        );

        let mut output = process(0, b"");
        output.capture.output_exceeded = true;
        assert_eq!(
            classify_case(&output, 32 * 1024 * 1024, OjCheckerKind::Exact, b""),
            OjCaseStatus::OutputLimitExceeded
        );
    }

    #[test]
    fn submission_filesystem_allowlist_excludes_commands_and_private_tests() {
        assert!(!SUBMISSION_READ_PATHS.contains(&EVALUATOR_ROOT));
        assert!(
            !SUBMISSION_READ_PATHS
                .iter()
                .any(|path| path.starts_with("/etc/labweaver"))
        );
        assert!(
            !SUBMISSION_READ_PATHS
                .iter()
                .any(|path| path.starts_with("/input"))
        );
    }

    #[test]
    fn compiler_filesystem_allowlist_excludes_commands_and_private_tests() {
        assert!(!COMPILER_READ_PATHS.contains(&EVALUATOR_ROOT));
        assert!(
            !COMPILER_READ_PATHS
                .iter()
                .any(|path| path.starts_with("/etc/labweaver"))
        );
        assert!(
            !COMPILER_READ_PATHS
                .iter()
                .any(|path| path.starts_with("/input"))
        );
        assert_eq!(COMMAND_PATH_ENV, "LABWEAVER_OJ_COMMAND_FILE");
    }

    #[test]
    fn helper_readiness_is_create_new_exact_and_consumed() -> Result<(), Box<dyn std::error::Error>>
    {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("ready");
        let mut file = create_helper_ready(&path)?;
        mark_helper_ready(&mut file)?;
        drop(file);
        consume_helper_ready(&path)?;
        assert!(!path.exists());

        std::fs::write(&path, b"forged")?;
        assert!(consume_helper_ready(&path).is_err());
        Ok(())
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn cgroup_pid_values_and_membership_are_strict() {
        assert_eq!(parse_cgroup_pids_max("128\n").ok(), Some(Some(128)));
        assert_eq!(parse_cgroup_pids_max("max\n").ok(), Some(None));
        assert!(parse_cgroup_pids_max("invalid\n").is_err());
        assert_eq!(
            cgroup_v2_pids_max_path("/kubepods/pod/worker"),
            Some(std::path::PathBuf::from(
                "/sys/fs/cgroup/kubepods/pod/worker/pids.max"
            ))
        );
        assert!(cgroup_v2_pids_max_path("/../host").is_none());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn submission_sandbox_denies_process_group_and_namespace_escape()
    -> Result<(), Box<dyn std::error::Error>> {
        const CHILD_ENV: &str = "LABWEAVER_OJ_SECCOMP_TEST_CHILD";
        if std::env::var_os(CHILD_ENV).is_some() {
            apply_submission_process_limit()?;
            assert_eq!(
                nix::sys::resource::getrlimit(nix::sys::resource::Resource::RLIMIT_NPROC)?,
                (64, 64)
            );
            apply_submission_syscall_sandbox()?;
            assert_eq!(nix::unistd::setsid(), Err(nix::errno::Errno::EPERM));
            assert_eq!(
                nix::unistd::setpgid(nix::unistd::Pid::from_raw(0), nix::unistd::Pid::from_raw(0)),
                Err(nix::errno::Errno::EPERM)
            );
            return Ok(());
        }

        let executable = std::env::current_exe()?;
        let status = StdCommand::new(executable)
            .arg("--exact")
            .arg("oj_worker::tests::submission_sandbox_denies_process_group_and_namespace_escape")
            .env(CHILD_ENV, "1")
            .status()?;
        assert!(status.success());
        Ok(())
    }
}
