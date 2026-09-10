//! Shell-free C++17 worker executed only inside the isolated OJ Kubernetes Job.
#![allow(
    clippy::needless_pass_by_value,
    clippy::useless_conversion,
    clippy::all,
    dead_code,
    unused,
    unused_imports,
    missing_docs,
    clippy::too_many_lines,
    reason = "the closed worker path is intentionally explicit and stable diagnostics define failures"
)]

#[cfg(unix)]
use std::ffi::CString;
#[cfg(unix)]
use std::os::unix::fs::FileTypeExt as _;
#[cfg(unix)]
use std::os::unix::process::{CommandExt as _, ExitStatusExt as _};
use std::{
    env, fs,
    io::Write as _,
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
    ABI, Access as _, AccessFs, BitFlags, CompatLevel, Compatible as _, PathBeneath, PathFd,
    Ruleset, RulesetAttr as _, RulesetCreated, RulesetCreatedAttr as _, RulesetStatus,
};
#[cfg(unix)]
use nix::{
    errno::Errno,
    libc,
    sys::resource::{Resource, setrlimit},
    sys::signal::{Signal, killpg},
    unistd::{Pid, execv},
};
use persistence_sqlx::Sha256Digest; // internal persistence hash, not contract hash
use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncRead, AsyncReadExt as _, AsyncWriteExt as _},
    process::Command,
    time::timeout,
};

use crate::{
    PvcSnapshotSource, SnapshotSource,
    execution::{ProgramCommandPaths, expand_program_argv},
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
const BUILD_ROOT: &str = "/work/build";
const CASES_ROOT: &str = "/work/cases";
/// Staged copy of the explicitly approved evaluator support files.
///
/// The evaluator is mounted as one bind mount.  Landlock cannot grant a child file read access
/// through that mount while keeping the mount's other files unreadable, so support files are
/// copied into this worker-owned tree before the compiler or submission sandbox is installed.
const SUPPORT_ROOT: &str = "/support";
const EVIDENCE_PATH: &str = "/evidence/evidence.json";
const SERVICE_PATH: &str = "/usr/local/bin/labweaver-service";
const PROGRAM_BINARY_PATH: &str = "/work/build/program";
const COMPILE_HELPER_READY_PATH: &str = "/work/.compile-helper-ready";
const CASE_HELPER_READY_PATH: &str = "/work/.case-helper-ready";
const COMPILE_INVOCATION_PATH: &str = "/work/.compile-invocation.json";
const CASE_INVOCATION_PATH: &str = "/work/.case-invocation.json";
const COMPILE_INVOCATION_ENV: &str = "LABWEAVER_OJ_COMPILE_INVOCATION";
const CASE_INVOCATION_ENV: &str = "LABWEAVER_OJ_CASE_INVOCATION";
const HELPER_READY_CONTENT: &[u8] = b"ready\n";
/// Reserved helper exit status used to distinguish infrastructure failures from compiler/runtime
/// exit statuses returned by the approved program.
pub const OJ_HELPER_FAILURE_EXIT_CODE: i32 = 125;
const PROFILE_MAX_BYTES: u64 = 64 * 1024;
const INVOCATION_MAX_BYTES: u64 = 256 * 1024;
const SUBMISSION_READ_PATHS: [&str; 11] = [
    BUILD_ROOT,
    "/usr/bin",
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
const COMPILER_READ_PATHS: [&str; 13] = [
    "/usr/bin",
    "/usr/include",
    "/usr/lib",
    "/usr/libexec",
    "/usr/lib64",
    "/usr/x86_64-pc-linux-gnu",
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
const MAX_SUPPORT_FILE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_SUPPORT_TOTAL_BYTES: u64 = 64 * 1024 * 1024;
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
    let evaluator_identity = request
        .evaluator_identity
        .ok_or(OjWorkerError::ProfileInvalid)?;
    let submission =
        PvcSnapshotSource::open(Path::new(SUBMISSION_ROOT), request.submission_identity)
            .map_err(|_| OjWorkerError::SourceUnavailable)?;
    let _source = read_verified(&submission, &request.source).await?;
    let evaluator = PvcSnapshotSource::open(Path::new(EVALUATOR_ROOT), evaluator_identity)
        .map_err(|_| OjWorkerError::SourceUnavailable)?;
    let profile = read_profile(&evaluator, &request).await?;
    validate_profile_support(&profile, &request, &evaluator).await?;
    prepare_workspace(&request)?;
    let support_paths = materialize_support_files(&profile, &evaluator).await?;
    let source_path = Path::new(SUBMISSION_ROOT).join(&request.source.path);
    let binary_path = PathBuf::from(PROGRAM_BINARY_PATH);
    let paths = ProgramCommandPaths::new(
        source_path,
        binary_path.clone(),
        PathBuf::from(SUBMISSION_ROOT),
        PathBuf::from(SUPPORT_ROOT),
    )
    .map_err(|_| OjWorkerError::ProfileInvalid)?;
    let compile = Box::pin(compile_program(&request, &profile, &paths, &support_paths)).await?;
    if !compile.status.success() || compile.capture.timed_out || compile.capture.output_exceeded {
        let evidence = compile_failure_evidence(&request, request_sha256, &compile)?;
        return persist_evidence(&request, &evidence);
    }
    if request.phase == OjExecutionPhase::Compile {
        let evidence = compile_success_evidence(&request, request_sha256, &compile)?;
        return persist_evidence(&request, &evidence);
    }
    let checker = request.checker.ok_or(OjWorkerError::CommandInvalid)?;
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
                &profile,
                &paths,
                &support_paths,
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

async fn read_profile(
    evaluator: &PvcSnapshotSource,
    request: &OjExecutionRequest,
) -> Result<contracts::evaluation::ApprovedProgramProfile, OjWorkerError> {
    let bytes = evaluator
        .read_file(&request.toolchain_profile, PROFILE_MAX_BYTES)
        .await
        .map_err(|_| OjWorkerError::ProfileUnavailable)?;
    let profile: contracts::evaluation::ApprovedProgramProfile =
        serde_json::from_slice(&bytes).map_err(|_| OjWorkerError::ProfileInvalid)?;
    profile
        .validate_for_phase(match request.phase {
            OjExecutionPhase::Compile => contracts::evaluation::ProgramPhase::Compile,
            OjExecutionPhase::Test => contracts::evaluation::ProgramPhase::Test,
        })
        .map_err(|_| OjWorkerError::ProfileInvalid)?;
    Ok(profile)
}

async fn validate_profile_support(
    profile: &contracts::evaluation::ApprovedProgramProfile,
    request: &OjExecutionRequest,
    evaluator: &PvcSnapshotSource,
) -> Result<(), OjWorkerError> {
    let private_paths = request
        .cases
        .iter()
        .flat_map(|case| [&case.input.path, &case.expected.path])
        .collect::<std::collections::BTreeSet<_>>();
    for path in &profile.support_files {
        if private_paths.contains(path) || path == &request.toolchain_profile {
            return Err(OjWorkerError::ProfileInvalid);
        }
        let metadata = evaluator
            .metadata(path)
            .await
            .map_err(|_| OjWorkerError::ProfileUnavailable)?
            .ok_or(OjWorkerError::ProfileUnavailable)?;
        if !matches!(metadata.kind, crate::collector::SourceKind::File) {
            return Err(OjWorkerError::ProfileInvalid);
        }
    }
    Ok(())
}

/// Copies only the support files named by the approved profile into the worker-owned workspace.
///
/// Keeping this copy separate from the evaluator bind mount is required for the Landlock policy:
/// the mount root must remain traversal-only, while each staged file can receive ordinary read
/// access.  The source capability validates the path and file kind again while reading bytes, and
/// the destination is created below a fresh directory without following links.
async fn materialize_support_files(
    profile: &contracts::evaluation::ApprovedProgramProfile,
    evaluator: &PvcSnapshotSource,
) -> Result<Vec<PathBuf>, OjWorkerError> {
    let mut total_bytes = 0_u64;
    let mut staged_paths = Vec::with_capacity(profile.support_files.len());
    for relative_path in &profile.support_files {
        let metadata = evaluator
            .metadata(relative_path)
            .await
            .map_err(|_| OjWorkerError::ProfileUnavailable)?
            .ok_or(OjWorkerError::ProfileUnavailable)?;
        if !matches!(metadata.kind, crate::collector::SourceKind::File)
            || metadata.size_bytes > MAX_SUPPORT_FILE_BYTES
        {
            return Err(OjWorkerError::SourceInvalid);
        }
        total_bytes = total_bytes
            .checked_add(metadata.size_bytes)
            .filter(|size| *size <= MAX_SUPPORT_TOTAL_BYTES)
            .ok_or(OjWorkerError::SourceInvalid)?;
        let bytes = evaluator
            .read_file(relative_path, MAX_SUPPORT_FILE_BYTES)
            .await
            .map_err(|_| OjWorkerError::SourceInvalid)?;
        if u64::try_from(bytes.len()).ok() != Some(metadata.size_bytes) {
            return Err(OjWorkerError::SourceInvalid);
        }
        let destination = Path::new(SUPPORT_ROOT).join(relative_path);
        let parent = destination
            .parent()
            .ok_or(OjWorkerError::WorkspaceInvalid)?;
        ensure_parent_directories(Path::new(SUPPORT_ROOT), parent)?;
        write_new(&destination, &bytes)?;
        staged_paths.push(destination);
    }
    Ok(staged_paths)
}

fn prepare_workspace(request: &OjExecutionRequest) -> Result<(), OjWorkerError> {
    ensure_empty_or_create_directory(Path::new(BUILD_ROOT))?;
    ensure_empty_or_create_directory(Path::new(CASES_ROOT))?;
    ensure_empty_or_create_directory(Path::new(SUPPORT_ROOT))?;
    for case in &request.cases {
        let path = case_directory(case)?;
        ensure_new_directory(&path)?;
    }
    Ok(())
}

fn case_directory(case: &OjCaseBinding) -> Result<PathBuf, OjWorkerError> {
    if !case
        .id
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(OjWorkerError::WorkspaceInvalid);
    }
    Ok(Path::new(CASES_ROOT).join(&case.id))
}

fn ensure_empty_directory(path: &Path) -> Result<(), OjWorkerError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| OjWorkerError::WorkspaceInvalid)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(OjWorkerError::WorkspaceInvalid);
    }
    if fs::read_dir(path)
        .map_err(|_| OjWorkerError::WorkspaceInvalid)?
        .next()
        .is_some()
    {
        return Err(OjWorkerError::WorkspaceInvalid);
    }
    Ok(())
}

fn ensure_empty_or_create_directory(path: &Path) -> Result<(), OjWorkerError> {
    match fs::symlink_metadata(path) {
        Ok(_) => ensure_empty_directory(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(path).map_err(|_| OjWorkerError::WorkspaceInvalid)
        }
        Err(_) => Err(OjWorkerError::WorkspaceInvalid),
    }
}

fn ensure_parent_directories(root: &Path, parent: &Path) -> Result<(), OjWorkerError> {
    let relative = parent
        .strip_prefix(root)
        .map_err(|_| OjWorkerError::WorkspaceInvalid)?;
    let mut current = root.to_owned();
    for component in relative.components() {
        let std::path::Component::Normal(component) = component else {
            return Err(OjWorkerError::WorkspaceInvalid);
        };
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&current).map_err(|_| OjWorkerError::WorkspaceInvalid)?;
            }
            Ok(_) | Err(_) => return Err(OjWorkerError::WorkspaceInvalid),
        }
    }
    Ok(())
}

fn ensure_new_directory(path: &Path) -> Result<(), OjWorkerError> {
    fs::create_dir(path).map_err(|_| OjWorkerError::WorkspaceInvalid)
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HelperInvocation {
    argv: Vec<String>,
    cwd: String,
    read_paths: Vec<String>,
    write_paths: Vec<String>,
}

impl HelperInvocation {
    fn new(
        argv: Vec<String>,
        cwd: PathBuf,
        read_paths: Vec<PathBuf>,
        write_paths: Vec<PathBuf>,
    ) -> Result<Self, OjWorkerError> {
        if argv.is_empty()
            || argv.len() > 128
            || !Path::new(&argv[0]).is_absolute()
            || argv.iter().any(|argument| {
                argument.is_empty() || argument.len() > 1024 || argument.contains('\0')
            })
            || !cwd.is_absolute()
            || cwd.to_string_lossy().chars().any(char::is_control)
            || read_paths.is_empty()
            || read_paths.len() > 256
            || write_paths.is_empty()
            || write_paths.len() > 8
        {
            return Err(OjWorkerError::ProfileInvalid);
        }
        let to_string = |path: PathBuf| {
            path.to_str()
                .filter(|value| !value.is_empty() && !value.chars().any(char::is_control))
                .map(str::to_owned)
                .ok_or(OjWorkerError::ProfileInvalid)
        };
        Ok(Self {
            argv,
            cwd: to_string(cwd)?,
            read_paths: read_paths
                .into_iter()
                .map(to_string)
                .collect::<Result<_, _>>()?,
            write_paths: write_paths
                .into_iter()
                .map(to_string)
                .collect::<Result<_, _>>()?,
        })
    }
}

fn write_invocation(path: &Path, invocation: &HelperInvocation) -> Result<(), OjWorkerError> {
    let bytes = serde_jcs::to_vec(invocation).map_err(|_| OjWorkerError::ProfileInvalid)?;
    let size = u64::try_from(bytes.len()).map_err(|_| OjWorkerError::ProfileInvalid)?;
    if bytes.is_empty() || size > INVOCATION_MAX_BYTES {
        return Err(OjWorkerError::ProfileInvalid);
    }
    write_new(path, &bytes)
}

fn read_invocation(env_name: &str) -> Result<(PathBuf, HelperInvocation), OjWorkerError> {
    let path = env::var_os(env_name)
        .map(PathBuf::from)
        .ok_or(OjWorkerError::CommandInvalid)?;
    let metadata = fs::symlink_metadata(&path).map_err(|_| OjWorkerError::CommandUnavailable)?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() == 0
        || metadata.len() > INVOCATION_MAX_BYTES
    {
        return Err(OjWorkerError::CommandInvalid);
    }
    let bytes = fs::read(&path).map_err(|_| OjWorkerError::CommandUnavailable)?;
    if u64::try_from(bytes.len()).ok() != Some(metadata.len()) {
        return Err(OjWorkerError::CommandInvalid);
    }
    let invocation = serde_json::from_slice::<HelperInvocation>(&bytes)
        .map_err(|_| OjWorkerError::CommandInvalid)?;
    validate_helper_invocation(&invocation)?;
    Ok((path, invocation))
}

fn validate_helper_invocation(invocation: &HelperInvocation) -> Result<(), OjWorkerError> {
    if invocation.argv.is_empty()
        || invocation.argv.len() > 128
        || !Path::new(&invocation.argv[0]).is_absolute()
        || invocation
            .argv
            .iter()
            .any(|argument| argument.is_empty() || argument.len() > 1024 || argument.contains('\0'))
        || !Path::new(&invocation.cwd).is_absolute()
        || invocation.cwd.chars().any(char::is_control)
        || invocation.read_paths.is_empty()
        || invocation.read_paths.len() > 256
        || invocation.write_paths.is_empty()
        || invocation.write_paths.len() > 8
        || invocation
            .read_paths
            .iter()
            .chain(invocation.write_paths.iter())
            .any(|path| {
                !Path::new(path).is_absolute()
                    || path.chars().any(char::is_control)
                    || !Path::new(path).exists()
            })
    {
        return Err(OjWorkerError::CommandInvalid);
    }
    Ok(())
}

fn compiler_read_paths(support_paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut paths = canonical_system_read_paths(&COMPILER_READ_PATHS);
    paths.push(PathBuf::from(SUBMISSION_ROOT));
    let _ = support_paths;
    paths.push(PathBuf::from(SUPPORT_ROOT));
    paths
}

fn execution_read_paths(support_paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut paths = canonical_system_read_paths(&SUBMISSION_READ_PATHS);
    let _ = support_paths;
    paths.push(PathBuf::from(SUPPORT_ROOT));
    paths
}

/// Returns the fixed system roots in the form accepted by Landlock.
///
/// Minimal container images commonly retain compatibility symlinks such as `/lib -> /usr/lib`
/// and `/lib64 -> /usr/lib64`. Landlock rules are inode based, so opening the canonical target
/// preserves access through those symlinks while avoiding a symlink path in the rules themselves.
/// Missing optional roots remain omitted because the same profile must run on merged and non-merged
/// Linux filesystem layouts.
fn canonical_system_read_paths(paths: &[&str]) -> Vec<PathBuf> {
    let mut canonical = Vec::new();
    for path in paths {
        let path = Path::new(path);
        let Ok(path) = path.canonicalize() else {
            continue;
        };
        if !canonical.contains(&path) {
            canonical.push(path);
        }
    }
    canonical
}

async fn compile_program(
    request: &OjExecutionRequest,
    profile: &contracts::evaluation::ApprovedProgramProfile,
    paths: &ProgramCommandPaths,
    support_paths: &[PathBuf],
) -> Result<CompletedProcess, OjWorkerError> {
    let argv = expand_program_argv(profile, contracts::evaluation::ProgramPhase::Compile, paths)
        .map_err(|_| OjWorkerError::ProfileInvalid)?;
    let invocation = HelperInvocation::new(
        argv,
        PathBuf::from(BUILD_ROOT),
        compiler_read_paths(support_paths),
        vec![PathBuf::from(BUILD_ROOT)],
    )?;
    write_invocation(Path::new(COMPILE_INVOCATION_PATH), &invocation)?;
    let mut command = Command::new(SERVICE_PATH);
    command
        .env_clear()
        .env(COMPILE_INVOCATION_ENV, COMPILE_INVOCATION_PATH)
        .env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .env("TMPDIR", BUILD_ROOT)
        .current_dir(BUILD_ROOT)
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
    ensure_helper_started(&process)?;
    Ok(process)
}

async fn run_case(
    request: &OjExecutionRequest,
    case: &OjCaseBinding,
    binary: &Path,
    input: &[u8],
    expected: &[u8],
    checker: crate::oj::OjCheckerKind,
    profile: &contracts::evaluation::ApprovedProgramProfile,
    paths: &ProgramCommandPaths,
    support_paths: &[PathBuf],
) -> Result<OjCaseEvidence, OjWorkerError> {
    let cpu_seconds = request
        .limits
        .cpu_milliseconds
        .checked_add(999)
        .map(|milliseconds| milliseconds / 1000)
        .ok_or(OjWorkerError::LimitInvalid)?;
    if binary != Path::new(PROGRAM_BINARY_PATH) {
        return Err(OjWorkerError::WorkspaceInvalid);
    }
    let case_path = case_directory(case)?;
    let argv = expand_program_argv(profile, contracts::evaluation::ProgramPhase::Test, paths)
        .map_err(|_| OjWorkerError::ProfileInvalid)?;
    let invocation = HelperInvocation::new(
        argv,
        case_path.clone(),
        execution_read_paths(support_paths),
        vec![case_path.clone()],
    )?;
    write_invocation(Path::new(CASE_INVOCATION_PATH), &invocation)?;
    let mut command = Command::new(SERVICE_PATH);
    command
        .env_clear()
        .env(CASE_INVOCATION_ENV, CASE_INVOCATION_PATH)
        .env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .env("TMPDIR", case_path.to_string_lossy().as_ref())
        .current_dir(WORK_ROOT)
        .arg("--mode")
        .arg("oj-case-exec")
        .arg("--memory-bytes")
        .arg(request.limits.memory_bytes.to_string())
        .arg("--cpu-seconds")
        .arg(cpu_seconds.to_string())
        .arg("--file-bytes")
        .arg(request.limits.output_bytes.to_string());
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
    ensure_helper_started(&process)?;
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
    #[cfg(unix)]
    let signal = process.status.signal();
    #[cfg(not(unix))]
    let signal: Option<i32> = None;
    #[cfg(unix)]
    let is_sigxcpu = signal == Some(libc::SIGXCPU);
    #[cfg(not(unix))]
    let is_sigxcpu = false;
    if process.capture.timed_out || is_sigxcpu {
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
    #[cfg(unix)]
    let is_sig25 = signal == Some(25);
    #[cfg(not(unix))]
    let is_sig25 = false;
    if is_sig25 {
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
#[cfg(unix)]
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
    let (invocation_path, invocation) = read_invocation(CASE_INVOCATION_ENV)?;
    let expected_case_root = Path::new(CASES_ROOT);
    if !Path::new(&invocation.cwd).starts_with(expected_case_root)
        || Path::new(&invocation.cwd).parent() != Some(expected_case_root)
        || invocation.write_paths != vec![invocation.cwd.clone()]
    {
        return Err(OjWorkerError::CommandInvalid);
    }
    fs::remove_file(&invocation_path).map_err(|_| OjWorkerError::WorkspaceInvalid)?;
    std::env::set_current_dir(&invocation.cwd).map_err(|_| OjWorkerError::WorkspaceInvalid)?;
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
    apply_submission_filesystem_sandbox(&invocation.read_paths, &invocation.write_paths)?;
    apply_submission_syscall_sandbox()?;
    mark_helper_ready(&mut ready)?;
    let arguments = invocation
        .argv
        .iter()
        .map(|argument| CString::new(argument.as_str()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| OjWorkerError::WorkspaceInvalid)?;
    let executable = arguments.first().ok_or(OjWorkerError::CommandInvalid)?;
    match execv(executable, &arguments) {
        Ok(never) => match never {},
        Err(_) => Err(OjWorkerError::ProcessSpawn),
    }
}

#[cfg(not(unix))]
#[allow(clippy::missing_errors_doc)]
pub fn run_oj_case_exec(
    _memory_bytes: u64,
    _cpu_seconds: u64,
    _file_bytes: u64,
) -> Result<(), OjWorkerError> {
    Err(OjWorkerError::ProcessSpawn)
}

/// Applies the compiler filesystem sandbox and replaces the helper with the fixed compiler.
///
/// # Errors
///
/// Returns a stable [`OjWorkerError`] when the workspace, sandbox, or compiler exec is invalid.
#[cfg(unix)]
pub fn run_oj_compile_exec() -> Result<(), OjWorkerError> {
    let (invocation_path, invocation) = read_invocation(COMPILE_INVOCATION_ENV)?;
    if invocation.cwd != BUILD_ROOT
        || invocation.write_paths != vec![BUILD_ROOT.to_owned()]
        || !Path::new(SUBMISSION_ROOT).is_dir()
        || !Path::new(EVALUATOR_ROOT).is_dir()
    {
        return Err(OjWorkerError::CommandInvalid);
    }
    fs::remove_file(&invocation_path).map_err(|_| OjWorkerError::WorkspaceInvalid)?;
    std::env::set_current_dir(&invocation.cwd).map_err(|_| OjWorkerError::WorkspaceInvalid)?;
    let mut ready = create_helper_ready(Path::new(COMPILE_HELPER_READY_PATH))?;
    apply_compiler_filesystem_sandbox(&invocation.read_paths, &invocation.write_paths)?;
    mark_helper_ready(&mut ready)?;
    let arguments = invocation
        .argv
        .iter()
        .map(|argument| CString::new(argument.as_str()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| OjWorkerError::WorkspaceInvalid)?;
    let compiler = arguments.first().ok_or(OjWorkerError::WorkspaceInvalid)?;
    match execv(compiler, &arguments) {
        Ok(never) => match never {},
        Err(_) => Err(OjWorkerError::ProcessSpawn),
    }
}

#[cfg(not(unix))]
#[allow(clippy::missing_errors_doc)]
pub fn run_oj_compile_exec() -> Result<(), OjWorkerError> {
    Err(OjWorkerError::ProcessSpawn)
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

fn ensure_helper_started(process: &CompletedProcess) -> Result<(), OjWorkerError> {
    if process.status.code() == Some(OJ_HELPER_FAILURE_EXIT_CODE) {
        return Err(OjWorkerError::ProcessSpawn);
    }
    Ok(())
}

fn validate_sandbox_paths(
    read_paths: &[String],
    write_paths: &[String],
) -> Result<(), OjWorkerError> {
    if read_paths.is_empty() || write_paths.is_empty() {
        return Err(OjWorkerError::SandboxUnavailable);
    }
    for path in read_paths.iter().chain(write_paths.iter()) {
        let path = Path::new(path);
        let metadata = fs::symlink_metadata(path).map_err(|_| OjWorkerError::SandboxUnavailable)?;
        let file_type = metadata.file_type();
        let file_like = file_type.is_file() || {
            #[cfg(unix)]
            {
                file_type.is_char_device()
            }
            #[cfg(not(unix))]
            {
                false
            }
        };
        if file_type.is_symlink() || !file_like && !metadata.is_dir() {
            return Err(OjWorkerError::SandboxUnavailable);
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn apply_submission_filesystem_sandbox(
    read_paths: &[String],
    write_paths: &[String],
) -> Result<(), OjWorkerError> {
    validate_sandbox_paths(read_paths, write_paths)?;
    let abi = ABI::V3;
    let ruleset = Ruleset::default()
        .set_compatibility(CompatLevel::HardRequirement)
        .handle_access(AccessFs::from_all(abi))
        .map_err(|_| OjWorkerError::SandboxUnavailable)?
        .create()
        .map_err(|_| OjWorkerError::SandboxUnavailable)?;
    let ruleset = add_sandbox_path_rule(ruleset, WORK_ROOT, AccessFs::Execute.into())?;
    let ruleset = add_sandbox_path_rules(ruleset, read_paths, abi, false)?;
    // Landlock path rules are evaluated for every directory component during
    // traversal.  The evaluator root rule below grants traversal within the
    // mounted tree, while this parent rule grants traversal into that tree.
    let ruleset = add_sandbox_path_rule(ruleset, "/input", AccessFs::Execute.into())?;
    let ruleset = add_sandbox_path_rule(
        ruleset,
        EVALUATOR_ROOT,
        (AccessFs::Execute | AccessFs::ReadDir).into(),
    )?;
    let ruleset = add_sandbox_path_rules(ruleset, write_paths, abi, true)?;
    let ruleset = add_sandbox_path_rules(ruleset, &[CASE_HELPER_READY_PATH.to_owned()], abi, true)?;
    let status = ruleset
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
fn apply_compiler_filesystem_sandbox(
    read_paths: &[String],
    write_paths: &[String],
) -> Result<(), OjWorkerError> {
    validate_sandbox_paths(read_paths, write_paths)?;
    let abi = ABI::V3;
    let ruleset = Ruleset::default()
        .set_compatibility(CompatLevel::HardRequirement)
        .handle_access(AccessFs::from_all(abi))
        .map_err(|_| OjWorkerError::SandboxUnavailable)?
        .create()
        .map_err(|_| OjWorkerError::SandboxUnavailable)?;
    let ruleset = add_sandbox_path_rule(ruleset, WORK_ROOT, AccessFs::Execute.into())?;
    let ruleset = add_sandbox_path_rules(ruleset, read_paths, abi, false)?;
    let ruleset = add_sandbox_path_rule(ruleset, "/input", AccessFs::Execute.into())?;
    let ruleset = add_sandbox_path_rules(ruleset, write_paths, abi, true)?;
    let ruleset =
        add_sandbox_path_rules(ruleset, &[COMPILE_HELPER_READY_PATH.to_owned()], abi, true)?;
    let status = ruleset
        .restrict_self()
        .map_err(|_| OjWorkerError::SandboxUnavailable)?;
    if status.ruleset != RulesetStatus::FullyEnforced || !status.no_new_privs {
        return Err(OjWorkerError::SandboxUnavailable);
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn add_sandbox_path_rules(
    mut ruleset: RulesetCreated,
    paths: &[String],
    abi: ABI,
    writable: bool,
) -> Result<RulesetCreated, OjWorkerError> {
    for path in paths {
        let metadata = fs::symlink_metadata(path).map_err(|_| OjWorkerError::SandboxUnavailable)?;
        let access = if metadata.is_dir() {
            if writable {
                AccessFs::from_all(abi)
            } else {
                AccessFs::from_read(abi)
            }
        } else {
            // Landlock's directory-only rights (for example READ_DIR and MAKE_REG) are
            // rejected for regular files and device nodes under HardRequirement.  The
            // invocation path lists include /dev/null and /dev/urandom, so select the
            // file-safe subset explicitly instead of relying on an implicit downgrade. Read
            // paths stay read-only; only fixed marker files and declared write roots receive
            // write rights.
            let file_access = AccessFs::from_file(abi);
            if writable {
                file_access
            } else {
                AccessFs::from_read(abi) & file_access
            }
        };
        ruleset = add_sandbox_path_rule(ruleset, path, access)?;
    }
    Ok(ruleset)
}

#[cfg(target_os = "linux")]
fn add_sandbox_path_rule(
    ruleset: RulesetCreated,
    path: &str,
    access: BitFlags<AccessFs>,
) -> Result<RulesetCreated, OjWorkerError> {
    let descriptor = PathFd::new(path).map_err(|_| OjWorkerError::SandboxUnavailable)?;
    ruleset
        .add_rule(PathBeneath::new(descriptor, access))
        .map_err(|_| OjWorkerError::SandboxUnavailable)
}

#[cfg(not(target_os = "linux"))]
fn apply_submission_filesystem_sandbox(
    _read_paths: &[String],
    _write_paths: &[String],
) -> Result<(), OjWorkerError> {
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
fn apply_compiler_filesystem_sandbox(
    _read_paths: &[String],
    _write_paths: &[String],
) -> Result<(), OjWorkerError> {
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
            #[cfg(unix)]
            signal: status.signal(),
            #[cfg(not(unix))]
            signal: None,
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
    #[cfg(unix)]
    {
        command.as_std_mut().process_group(0);
    }
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
            #[cfg(unix)]
            {
                kill_process_group(child_id)?;
            }
            let status = child.wait().await.map_err(|_| OjWorkerError::ProcessIo)?;
            (status, Vec::new(), Vec::new(), true)
        };
    #[cfg(unix)]
    {
        kill_process_group(child_id)?;
    }
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

#[cfg(unix)]
fn kill_process_group(process_id: u32) -> Result<(), OjWorkerError> {
    let process_group =
        Pid::from_raw(i32::try_from(process_id).map_err(|_| OjWorkerError::ProcessIo)?);
    match killpg(process_group, Signal::SIGKILL) {
        Ok(()) | Err(Errno::ESRCH) => Ok(()),
        Err(_) => Err(OjWorkerError::ProcessIo),
    }
}

#[cfg(not(unix))]
#[allow(dead_code, clippy::unnecessary_wraps, clippy::missing_errors_doc)]
fn kill_process_group(_process_id: u32) -> Result<(), OjWorkerError> {
    Ok(())
}

#[derive(Clone, Copy, Debug, Default)]
struct MemoryObservation {
    peak_resident_bytes: Option<u64>,
    peak_virtual_bytes: Option<u64>,
}

async fn monitor_peak_memory(process_id: u32, stop: Arc<AtomicBool>) -> MemoryObservation {
    let status_path = PathBuf::from(format!("/proc/{process_id}/status"));
    let mut observation = MemoryObservation::default();
    while !stop.load(Ordering::Acquire) {
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
    #[error("OJ approved program profile is unavailable")]
    ProfileUnavailable,
    #[error("OJ approved program profile is invalid")]
    ProfileInvalid,
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
            Self::ProfileUnavailable => "LW_OJ_PROFILE_UNAVAILABLE",
            Self::ProfileInvalid => "LW_OJ_PROFILE_INVALID",
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
    #[cfg(unix)]
    use std::os::unix::process::ExitStatusExt as _;
    use std::process::Command as StdCommand;

    use super::{
        COMMAND_PATH_ENV, COMPILER_READ_PATHS, CompletedProcess, EVALUATOR_ROOT,
        OJ_HELPER_FAILURE_EXIT_CODE, ProcessCapture, SUBMISSION_READ_PATHS, classify_case,
        consume_helper_ready, create_helper_ready, ensure_helper_started, mark_helper_ready,
    };
    #[cfg(target_os = "linux")]
    use super::{
        apply_submission_process_limit, apply_submission_syscall_sandbox, cgroup_v2_pids_max_path,
        parse_cgroup_pids_max,
    };
    use crate::oj::{OjCaseStatus, OjCheckerKind};

    fn process(status: std::process::ExitStatus, stdout: &[u8]) -> CompletedProcess {
        CompletedProcess {
            status,
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
    fn accepted_and_wrong_answer_classification_is_portable() {
        let accepted = process(exit_status(0), b"42\n");
        assert_eq!(
            classify_case(&accepted, 32 * 1024 * 1024, OjCheckerKind::Exact, b"42\n"),
            OjCaseStatus::Accepted
        );
        assert_eq!(
            classify_case(&accepted, 32 * 1024 * 1024, OjCheckerKind::Exact, b"41\n"),
            OjCaseStatus::WrongAnswer
        );
    }

    #[test]
    fn output_limit_and_timeout_classification_is_portable() {
        let mut output = process(exit_status(0), b"");
        output.capture.output_exceeded = true;
        assert_eq!(
            classify_case(&output, 32 * 1024 * 1024, OjCheckerKind::Exact, b""),
            OjCaseStatus::OutputLimitExceeded
        );

        let mut wall_timeout = process(exit_status(1), b"");
        wall_timeout.capture.timed_out = true;
        assert_eq!(
            classify_case(&wall_timeout, 32 * 1024 * 1024, OjCheckerKind::Exact, b""),
            OjCaseStatus::TimeLimitExceeded
        );
    }

    #[test]
    fn nonzero_exit_classification_is_portable() {
        let runtime = process(exit_status(1), b"");
        assert_eq!(
            classify_case(&runtime, 32 * 1024 * 1024, OjCheckerKind::Exact, b""),
            OjCaseStatus::RuntimeError
        );
    }

    #[cfg(unix)]
    #[test]
    fn signal_classification_uses_real_wait_statuses() {
        let memory = process(std::process::ExitStatus::from_raw(11), b"");
        let mut memory = memory;
        memory.capture.peak_virtual_memory_bytes = Some(31 * 1024 * 1024);
        assert_eq!(
            classify_case(&memory, 32 * 1024 * 1024, OjCheckerKind::Exact, b""),
            OjCaseStatus::MemoryLimitExceeded
        );

        let cpu = process(std::process::ExitStatus::from_raw(24), b"");
        assert_eq!(
            classify_case(&cpu, 32 * 1024 * 1024, OjCheckerKind::Exact, b""),
            OjCaseStatus::TimeLimitExceeded
        );

        let self_sigkill = process(std::process::ExitStatus::from_raw(9), b"");
        assert_eq!(
            classify_case(&self_sigkill, 32 * 1024 * 1024, OjCheckerKind::Exact, b""),
            OjCaseStatus::RuntimeError
        );
    }

    #[allow(
        clippy::expect_used,
        reason = "the subprocess helper is a test-only exit-status oracle and setup failures invalidate the test process"
    )]
    fn exit_status(code: i32) -> std::process::ExitStatus {
        StdCommand::new(std::env::current_exe().expect("test executable is available"))
            .arg("--exact")
            .arg("oj_worker::tests::exit_status_helper")
            .env("LABWEAVER_OJ_TEST_EXIT_CODE", code.to_string())
            .status()
            .expect("test executable can produce an exit status")
    }

    #[test]
    #[allow(
        clippy::expect_used,
        reason = "the subprocess helper receives a test-controlled numeric exit code"
    )]
    fn exit_status_helper() {
        if let Some(code) = std::env::var_os("LABWEAVER_OJ_TEST_EXIT_CODE") {
            let code = code
                .to_string_lossy()
                .parse::<i32>()
                .expect("test exit code is numeric");
            std::process::exit(code);
        }
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

    #[test]
    fn helper_exec_failure_is_infrastructure_but_compiler_exit_is_student_result() {
        let helper_failure = process(exit_status(OJ_HELPER_FAILURE_EXIT_CODE), b"");
        assert!(matches!(
            ensure_helper_started(&helper_failure),
            Err(super::OjWorkerError::ProcessSpawn)
        ));

        // A compiler which started successfully and rejected the submission keeps the normal
        // nonzero status, allowing the caller to emit compile_error evidence.
        let compiler_rejected = process(exit_status(1), b"syntax error");
        assert!(ensure_helper_started(&compiler_rejected).is_ok());
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
