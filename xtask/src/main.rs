//! Repository workflow entry point for `LabWeaver`.

use std::fmt::{Display, Formatter};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, ExitCode};

use clap::{Args, Parser, Subcommand, ValueEnum};
#[cfg(target_os = "linux")]
use sha2::{Digest, Sha256};

mod integration;
mod local_preflight;
mod platform_images;

#[derive(Debug, Parser)]
#[command(
    name = "cargo xtask",
    version,
    about = "LabWeaver repository workflow runner"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Format,
    Lint,
    Build,
    Test(TestArgs),
    Check,
    Preflight(EnvironmentArgs),
    Deploy(EnvironmentArgs),
    Verify(EnvironmentArgs),
    Backup(EnvironmentArgs),
    /// Reconcile or verify the private Keycloak identity foundation.
    IdentityFoundation(IdentityFoundationArgs),
    /// Reconcile the persistent `PostgreSQL`, NATS, and `MinIO` Sprint 2 foundation.
    PlatformFoundation(EnvironmentArgs),
    /// Reconcile the dedicated rootless `BuildKit` Sprint 2 foundation.
    PlatformBuildkit(EnvironmentArgs),
    /// Adopt the existing Harbor Gateway route without reconciling Harbor state.
    PlatformHarborRoute(EnvironmentArgs),
    /// Adopt existing data services and atomically deploy the Sprint 2 application profile.
    PlatformApplication(EnvironmentArgs),
    /// Deploy the independently reviewed Resource authority profile.
    ResourceApplication(EnvironmentArgs),
    /// Read-only Docker Desktop capability discovery for local validation.
    #[command(subcommand)]
    Local(LocalCommand),
    Rollback(RollbackArgs),
    Package(PackageArgs),
    PackageValidate(PackageValidateArgs),
    #[command(subcommand)]
    Contracts(ContractsCommand),
}

#[derive(Debug, Args)]
struct TestArgs {
    #[arg(long, value_enum, default_value_t = TestSuite::All)]
    suite: TestSuite,
    #[arg(long, value_enum, default_value_t = IntegrationScope::Candidate)]
    scope: IntegrationScope,
    #[arg(long)]
    base_ref: Option<String>,
    #[arg(long)]
    include_kind: bool,
    #[arg(long)]
    kind_only: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum IntegrationScope {
    Changed,
    Candidate,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum TestSuite {
    All,
    Contract,
    Integration,
}

#[derive(Debug, Args)]
struct EnvironmentArgs {
    #[arg(long)]
    env: String,
    #[arg(long)]
    infra: bool,
    #[arg(long)]
    yes: bool,
    #[arg(long)]
    package_manifest: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct PackageArgs {
    #[arg(long)]
    env: String,
    #[arg(long)]
    release: String,
    #[arg(long, value_enum, default_value_t = PackageProfile::Platform)]
    profile: PackageProfile,
    #[arg(long)]
    yes: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum PackageProfile {
    Platform,
    Resource,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum PackageValidationMode {
    Static,
    Connected,
}

#[derive(Debug, Args)]
struct PackageValidateArgs {
    #[arg(long)]
    manifest: PathBuf,
    #[arg(long, value_enum)]
    mode: PackageValidationMode,
    #[arg(long, required_if_eq("mode", "connected"))]
    env: Option<String>,
}

#[derive(Debug, Args)]
struct IdentityFoundationArgs {
    #[command(flatten)]
    environment: EnvironmentArgs,
    #[arg(long, value_enum)]
    action: IdentityFoundationAction,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum IdentityFoundationAction {
    Deploy,
    Verify,
}

impl IdentityFoundationAction {
    const fn playbook(self) -> &'static str {
        match self {
            Self::Deploy => "91-identity-foundation.yml",
            Self::Verify => "92-identity-foundation-verify.yml",
        }
    }
}

#[derive(Debug, Args)]
struct RollbackArgs {
    #[arg(long)]
    env: String,
    #[arg(long)]
    release_revision: String,
    #[arg(long)]
    yes: bool,
}

#[derive(Debug, Subcommand)]
enum LocalCommand {
    /// Probe Docker Desktop Kubernetes without applying any object.
    Preflight(LocalPreflightArgs),
}

#[derive(Debug, Args)]
struct LocalPreflightArgs {
    #[arg(long, default_value = "local-hostpath")]
    profile: String,
}

#[derive(Debug, Subcommand)]
enum ContractsCommand {
    Generate,
    Check,
}

#[derive(Debug)]
enum AppError {
    ExternalCommand {
        role: &'static str,
        code: Option<i32>,
        detail: Option<String>,
    },
    InfrastructureRequired {
        command: &'static str,
    },
    ConfirmationRequired {
        command: &'static str,
    },
    Io {
        role: &'static str,
        detail: String,
    },
    ContractDrift {
        path: String,
    },
    PlatformImage {
        code: &'static str,
        detail: String,
    },
    Integration {
        code: &'static str,
        detail: String,
    },
    #[allow(dead_code)]
    InvalidArgument {
        role: &'static str,
    },
    #[cfg(not(target_os = "linux"))]
    UnsupportedPlatform {
        command: &'static str,
    },
}

impl AppError {
    const fn diagnostic_code(&self) -> &'static str {
        match self {
            Self::ExternalCommand { .. } => "XTASK_EXTERNAL_COMMAND_FAILED",
            Self::InfrastructureRequired { .. } => "XTASK_INFRASTRUCTURE_REQUIRED",
            Self::ConfirmationRequired { .. } => "XTASK_CONFIRMATION_REQUIRED",
            Self::Io { .. } => "XTASK_IO_FAILED",
            Self::ContractDrift { .. } => "LW_CONTRACT_DRIFT",
            Self::PlatformImage { code, .. } => code,
            Self::Integration { code, .. } => code,
            Self::InvalidArgument { .. } => "XTASK_INVALID_ARGUMENT",
            #[cfg(not(target_os = "linux"))]
            Self::UnsupportedPlatform { .. } => "XTASK_INFRA_UNSUPPORTED_PLATFORM",
        }
    }
}

impl Display for AppError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ExternalCommand { role, code, detail } => {
                write!(
                    formatter,
                    "{role} failed with process exit code {}",
                    code.map_or_else(|| "unknown".to_owned(), |value| value.to_string())
                )?;
                if let Some(detail) = detail {
                    write!(formatter, ": {detail}")?;
                }
                Ok(())
            }
            Self::InfrastructureRequired { command } => {
                write!(formatter, "{command} requires explicit --infra")
            }
            Self::ConfirmationRequired { command } => write!(
                formatter,
                "{command} is a destructive operation and requires explicit --yes"
            ),
            Self::Io { role, detail } => write!(formatter, "{role} failed: {detail}"),
            Self::ContractDrift { path } => {
                write!(formatter, "generated contract differs from {path}")
            }
            Self::PlatformImage { code, detail } => write!(formatter, "{code}: {detail}"),
            Self::Integration { detail, .. } => write!(formatter, "{detail}"),
            Self::InvalidArgument { role } => {
                write!(
                    formatter,
                    "{role} must use a lowercase allowlisted identifier"
                )
            }
            #[cfg(not(target_os = "linux"))]
            Self::UnsupportedPlatform { command } => write!(
                formatter,
                "{command} must run on the approved Linux infrastructure controller"
            ),
        }
    }
}

impl std::error::Error for AppError {}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("[{}] {error}", error.diagnostic_code());
            ExitCode::from(1)
        }
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "the workflow command dispatch is the single public xtask boundary"
)]
fn run(cli: Cli) -> Result<(), AppError> {
    match cli.command {
        Command::Format => run_cargo("format", ["fmt", "--all", "--", "--check"]),
        Command::Lint => run_cargo(
            "lint",
            [
                "clippy",
                "--workspace",
                "--exclude",
                "xtask",
                "--all-targets",
                "--all-features",
                "--",
                "-D",
                "warnings",
            ],
        ),
        Command::Build => run_cargo("build", ["build", "--workspace", "--exclude", "xtask"]),
        Command::Test(args) => match args.suite {
            TestSuite::All => run_cargo("test", ["test", "--workspace", "--exclude", "xtask"]),
            TestSuite::Contract => contract_test_suite(),
            TestSuite::Integration => integration::run(
                &repository_root(),
                args.scope,
                args.base_ref.as_deref(),
                args.include_kind,
                args.kind_only,
            ),
        },
        Command::Check => {
            run(Cli {
                command: Command::Format,
            })?;
            run(Cli {
                command: Command::Lint,
            })?;
            run(Cli {
                command: Command::Build,
            })?;
            run(Cli {
                command: Command::Test(TestArgs {
                    suite: TestSuite::All,
                    scope: IntegrationScope::Candidate,
                    base_ref: None,
                    include_kind: false,
                    kind_only: false,
                }),
            })
        }
        Command::Preflight(args) => preflight(&args),
        Command::Deploy(args) => deploy(&args),
        Command::Verify(args) => verify(&args),
        Command::Backup(args) => backup(&args),
        Command::IdentityFoundation(args) => identity_foundation(&args),
        Command::PlatformFoundation(args) => platform_foundation(&args),
        Command::PlatformBuildkit(args) => platform_buildkit(&args),
        Command::PlatformHarborRoute(args) => platform_harbor_route(&args),
        Command::PlatformApplication(args) => platform_application(&args),
        Command::ResourceApplication(args) => resource_application(&args),
        Command::Local(LocalCommand::Preflight(args)) => {
            local_preflight::run(&repository_root(), &args.profile)
        }
        Command::Rollback(args) => platform_images::rollback(
            &args.env,
            &args.release_revision,
            args.yes,
            &repository_root(),
        ),
        Command::Package(args) => package_command(&args),
        Command::PackageValidate(args) => platform_images::validate(
            &args.manifest,
            args.mode == PackageValidationMode::Connected,
            args.env.as_deref(),
            &repository_root(),
        ),
        Command::Contracts(ContractsCommand::Generate) => contracts_generate(),
        Command::Contracts(ContractsCommand::Check) => contracts_check(),
    }
}

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

#[cfg(target_os = "linux")]
fn git_output<const N: usize>(root: &Path, arguments: [&str; N]) -> Result<String, AppError> {
    let output = ProcessCommand::new("git")
        .current_dir(root)
        .args(arguments)
        .output()
        .map_err(|error| AppError::ExternalCommand {
            role: "read Git package identity",
            code: None,
            detail: Some(error.to_string()),
        })?;
    if !output.status.success() {
        return Err(AppError::ExternalCommand {
            role: "read Git package identity",
            code: output.status.code(),
            detail: Some(String::from_utf8_lossy(&output.stderr).trim().to_owned()),
        });
    }
    String::from_utf8(output.stdout)
        .map(|value| value.trim().to_owned())
        .map_err(|error| AppError::Io {
            role: "decode Git package identity",
            detail: error.to_string(),
        })
}

fn package_command(args: &PackageArgs) -> Result<(), AppError> {
    let profile = match args.profile {
        PackageProfile::Platform => "platform",
        PackageProfile::Resource => "resource",
    };
    if !args.yes {
        return Err(AppError::ConfirmationRequired { command: "package" });
    }
    #[cfg(target_os = "linux")]
    {
        let root = repository_root();
        platform_images::package(&args.env, &args.release, profile, args.yes, &root)
    }
    #[cfg(not(target_os = "linux"))]
    {
        platform_images::package(
            &args.env,
            &args.release,
            profile,
            args.yes,
            &repository_root(),
        )
    }
}

fn contracts_generate() -> Result<(), AppError> {
    write_contract_artifacts(&repository_root())
}

fn contracts_check() -> Result<(), AppError> {
    let root = repository_root();
    for artifact in contracts::schema::generate_all().map_err(|error| AppError::Io {
        role: "generate contracts",
        detail: error.to_string(),
    })? {
        let checked_in =
            fs::read(root.join(&artifact.relative_path)).map_err(|error| AppError::Io {
                role: "read checked-in contract",
                detail: format!("{}: {error}", artifact.relative_path),
            })?;
        if checked_in != artifact.bytes {
            return Err(AppError::ContractDrift {
                path: artifact.relative_path,
            });
        }
    }
    Ok(())
}

fn contract_test_suite() -> Result<(), AppError> {
    contracts_check()?;
    run_cargo(
        "contract tests",
        ["test", "-p", "contracts", "--all-targets", "--all-features"],
    )?;
    let status = ProcessCommand::new("pnpm")
        .arg("contracts:check")
        .current_dir(repository_root().join("web"))
        .status()
        .map_err(|error| AppError::ExternalCommand {
            role: "web contract drift check",
            code: None,
            detail: Some(error.to_string()),
        })?;
    if status.success() {
        Ok(())
    } else {
        Err(AppError::ExternalCommand {
            role: "web contract drift check",
            code: status.code(),
            detail: None,
        })
    }
}

fn write_contract_artifacts(root: &Path) -> Result<(), AppError> {
    for artifact in contracts::schema::generate_all().map_err(|error| AppError::Io {
        role: "generate contracts",
        detail: error.to_string(),
    })? {
        let destination = root.join(&artifact.relative_path);
        let parent = destination.parent().ok_or_else(|| AppError::Io {
            role: "resolve contract output",
            detail: artifact.relative_path.clone(),
        })?;
        fs::create_dir_all(parent).map_err(|error| AppError::Io {
            role: "create contract output directory",
            detail: error.to_string(),
        })?;
        fs::write(destination, artifact.bytes).map_err(|error| AppError::Io {
            role: "write contract output",
            detail: error.to_string(),
        })?;
    }
    Ok(())
}

fn deploy(args: &EnvironmentArgs) -> Result<(), AppError> {
    if !args.yes {
        return Err(AppError::ConfirmationRequired { command: "deploy" });
    }
    if !args.infra {
        let manifest = args
            .package_manifest
            .as_deref()
            .ok_or(AppError::InvalidArgument {
                role: "product deployment package manifest",
            })?;
        return platform_images::deploy(&args.env, manifest, &repository_root());
    }
    if args.package_manifest.is_some() {
        return Err(AppError::InvalidArgument {
            role: "infrastructure deployment does not accept --package-manifest",
        });
    }
    validate_environment_name(&args.env)?;
    run_infrastructure(&args.env, "95-harbor.yml", "deploy --infra")
}

fn preflight(args: &EnvironmentArgs) -> Result<(), AppError> {
    require_infrastructure(args, "preflight --infra")?;
    run_infrastructure(&args.env, "00-preflight.yml", "preflight --infra")
}

fn verify(args: &EnvironmentArgs) -> Result<(), AppError> {
    if !args.yes {
        return Err(AppError::ConfirmationRequired { command: "verify" });
    }
    require_infrastructure(args, "verify --infra")?;
    run_infrastructure(&args.env, "90-verify.yml", "verify --infra")
}

fn backup(args: &EnvironmentArgs) -> Result<(), AppError> {
    if !args.yes {
        return Err(AppError::ConfirmationRequired { command: "backup" });
    }
    require_infrastructure(args, "backup --infra")?;
    run_infrastructure(&args.env, "85-backup.yml", "backup --infra")
}

fn identity_foundation(args: &IdentityFoundationArgs) -> Result<(), AppError> {
    if !args.environment.yes {
        return Err(AppError::ConfirmationRequired {
            command: "identity-foundation",
        });
    }
    require_infrastructure(&args.environment, "identity-foundation --infra")?;
    run_infrastructure(
        &args.environment.env,
        args.action.playbook(),
        match args.action {
            IdentityFoundationAction::Deploy => "identity-foundation-deploy --infra",
            IdentityFoundationAction::Verify => "identity-foundation-verify --infra",
        },
    )
}

fn platform_foundation(args: &EnvironmentArgs) -> Result<(), AppError> {
    if !args.yes {
        return Err(AppError::ConfirmationRequired {
            command: "platform-foundation",
        });
    }
    require_infrastructure(args, "platform-foundation --infra")?;
    if args.package_manifest.is_some() {
        return Err(AppError::InvalidArgument {
            role: "Sprint 2 foundation does not accept --package-manifest",
        });
    }
    run_infrastructure(
        &args.env,
        "92-platform-foundation.yml",
        "platform-foundation --infra",
    )
}

fn platform_buildkit(args: &EnvironmentArgs) -> Result<(), AppError> {
    if !args.yes {
        return Err(AppError::ConfirmationRequired {
            command: "platform-buildkit",
        });
    }
    require_infrastructure(args, "platform-buildkit --infra")?;
    if args.package_manifest.is_some() {
        return Err(AppError::InvalidArgument {
            role: "Sprint 2 BuildKit does not accept --package-manifest",
        });
    }
    run_infrastructure(
        &args.env,
        "92-platform-buildkit.yml",
        "platform-buildkit --infra",
    )
}

fn platform_harbor_route(args: &EnvironmentArgs) -> Result<(), AppError> {
    if !args.yes {
        return Err(AppError::ConfirmationRequired {
            command: "platform-harbor-route",
        });
    }
    require_infrastructure(args, "platform-harbor-route --infra")?;
    if args.package_manifest.is_some() {
        return Err(AppError::InvalidArgument {
            role: "Sprint 2 Harbor route adoption does not accept --package-manifest",
        });
    }
    run_infrastructure(
        &args.env,
        "92-platform-harbor-route.yml",
        "platform-harbor-route --infra",
    )
}

fn platform_application(args: &EnvironmentArgs) -> Result<(), AppError> {
    if !args.yes {
        return Err(AppError::ConfirmationRequired {
            command: "platform-application",
        });
    }
    require_infrastructure(args, "platform-application --infra")?;
    let package_manifest = args
        .package_manifest
        .as_deref()
        .ok_or(AppError::InvalidArgument {
            role: "Sprint 2 application package manifest",
        })?;
    let package_manifest = package_manifest
        .canonicalize()
        .map_err(|error| AppError::Io {
            role: "resolve Sprint 2 application package manifest",
            detail: error.to_string(),
        })?;
    platform_images::validate(&package_manifest, false, None, &repository_root())?;
    run_infrastructure_with_package(
        &args.env,
        "93-platform-application.yml",
        "platform-application --infra",
        Some(&package_manifest),
        &[],
    )
}

fn resource_application(args: &EnvironmentArgs) -> Result<(), AppError> {
    if !args.yes {
        return Err(AppError::ConfirmationRequired {
            command: "resource-application",
        });
    }
    require_infrastructure(args, "resource-application --infra")?;
    let package_manifest = args
        .package_manifest
        .as_deref()
        .ok_or(AppError::InvalidArgument {
            role: "Resource application package manifest",
        })?
        .canonicalize()
        .map_err(|error| AppError::Io {
            role: "resolve Resource application package manifest",
            detail: error.to_string(),
        })?;
    platform_images::validate_profile(&package_manifest, "resource", &repository_root())?;
    run_infrastructure_with_package(
        &args.env,
        "94-resource-application.yml",
        "resource-application-repair --infra",
        Some(&package_manifest),
        &[],
    )
}

fn require_infrastructure(args: &EnvironmentArgs, command: &'static str) -> Result<(), AppError> {
    if !args.infra {
        return Err(AppError::InfrastructureRequired { command });
    }
    validate_environment_name(&args.env)
}

fn validate_environment_name(environment: &str) -> Result<(), AppError> {
    let valid = !environment.is_empty()
        && environment.len() <= 32
        && environment
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_lowercase)
        && environment.chars().all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-'
        });
    if valid {
        Ok(())
    } else {
        Err(AppError::InvalidArgument {
            role: "infrastructure environment",
        })
    }
}

#[cfg(target_os = "linux")]
fn require_infrastructure_file(role: &'static str, path: &std::path::Path) -> Result<(), AppError> {
    if path.is_file() {
        return Ok(());
    }
    Err(AppError::ExternalCommand {
        role,
        code: None,
        detail: Some(format!("required file is missing: {}", path.display())),
    })
}

#[cfg(target_os = "linux")]
fn resolve_infrastructure_file(
    role: &'static str,
    roots: [&std::path::Path; 2],
    relative: &str,
) -> Result<std::path::PathBuf, AppError> {
    roots
        .into_iter()
        .map(|root| root.join(relative))
        .find(|path| path.is_file())
        .ok_or_else(|| AppError::ExternalCommand {
            role,
            code: None,
            detail: Some(format!("required file is missing: {relative}")),
        })
}

#[cfg(target_os = "linux")]
fn resolve_infrastructure_directory(
    role: &'static str,
    roots: [&std::path::Path; 2],
    leaf: &str,
) -> Result<std::path::PathBuf, AppError> {
    roots
        .into_iter()
        .map(|root| root.join(leaf))
        .find(|path| path.is_dir())
        .ok_or_else(|| AppError::ExternalCommand {
            role,
            code: None,
            detail: Some(format!("locked Ansible {leaf} are missing")),
        })
}

#[cfg(target_os = "linux")]
fn infrastructure_path(path: &std::path::Path) -> String {
    path.to_string_lossy().into_owned()
}

#[cfg(target_os = "linux")]
fn infrastructure_commit_sha() -> Result<String, AppError> {
    let commit_sha =
        std::env::var("LABWEAVER_SOURCE_COMMIT").map_err(|_| AppError::ExternalCommand {
            role: "infrastructure source identity",
            code: None,
            detail: Some(
                "LABWEAVER_SOURCE_COMMIT is required and must be the verified bundle commit".into(),
            ),
        })?;
    if commit_sha
        .chars()
        .all(|character| character.is_ascii_hexdigit())
        && (40..=64).contains(&commit_sha.len())
    {
        return Ok(commit_sha);
    }
    Err(AppError::ExternalCommand {
        role: "infrastructure source identity",
        code: None,
        detail: Some("LABWEAVER_SOURCE_COMMIT must contain 40-64 hexadecimal characters".into()),
    })
}

#[cfg(target_os = "linux")]
fn file_sha256(path: &std::path::Path) -> Result<String, AppError> {
    let data = std::fs::read(path).map_err(|error| AppError::ExternalCommand {
        role: "infrastructure identity hash input",
        code: None,
        detail: Some(error.to_string()),
    })?;
    Ok(format!("sha256:{:x}", Sha256::digest(data)))
}

#[cfg(target_os = "linux")]
fn inventory_identity_hash(root: &std::path::Path) -> Result<String, AppError> {
    let mut pending = vec![root.to_path_buf()];
    let mut files = Vec::new();
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory).map_err(|error| AppError::ExternalCommand {
            role: "infrastructure inventory identity",
            code: None,
            detail: Some(error.to_string()),
        })? {
            let path = entry
                .map_err(|error| AppError::ExternalCommand {
                    role: "infrastructure inventory identity",
                    code: None,
                    detail: Some(error.to_string()),
                })?
                .path();
            if path.is_dir() {
                pending.push(path);
            } else if path.file_name().and_then(|name| name.to_str()) != Some(".vault-password") {
                files.push(path);
            }
        }
    }
    files.sort();
    let mut hasher = Sha256::new();
    for path in files {
        let relative = path
            .strip_prefix(root)
            .map_err(|error| AppError::ExternalCommand {
                role: "infrastructure inventory identity",
                code: None,
                detail: Some(error.to_string()),
            })?;
        let data = std::fs::read(&path).map_err(|error| AppError::ExternalCommand {
            role: "infrastructure inventory identity",
            code: None,
            detail: Some(error.to_string()),
        })?;
        hasher.update(relative.to_string_lossy().as_bytes());
        hasher.update([0]);
        hasher.update((data.len() as u64).to_be_bytes());
        hasher.update(data);
    }
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

#[cfg(target_os = "linux")]
fn approved_controller_identity(lock_path: &std::path::Path) -> Result<String, AppError> {
    use std::os::unix::fs::MetadataExt;

    let approved = controller_identity_field(
        &std::fs::read_to_string(lock_path).map_err(|error| AppError::ExternalCommand {
            role: "approved infrastructure controller lock",
            code: None,
            detail: Some(error.to_string()),
        })?,
        "approved_controller_ids",
    )?;
    let locator = std::env::var("LABWEAVER_CONTROLLER_IDENTITY_FILE").map_err(|_| {
        AppError::ExternalCommand {
            role: "approved router controller identity",
            code: None,
            detail: Some("LABWEAVER_CONTROLLER_IDENTITY_FILE is required".into()),
        }
    })?;
    let locator_path = std::path::PathBuf::from(locator);
    let metadata = std::fs::metadata(&locator_path).map_err(|error| AppError::ExternalCommand {
        role: "approved router controller identity",
        code: None,
        detail: Some(error.to_string()),
    })?;
    if metadata.uid() != 0 || metadata.mode() & 0o077 != 0 {
        return Err(AppError::ExternalCommand {
            role: "approved router controller identity",
            code: None,
            detail: Some("identity locator must be root-owned and mode 0600 or stricter".into()),
        });
    }
    let identity =
        std::fs::read_to_string(locator_path).map_err(|error| AppError::ExternalCommand {
            role: "approved router controller identity",
            code: None,
            detail: Some(error.to_string()),
        })?;
    let controller_id = controller_identity_field(&identity, "controller_id")?;
    let declared_machine_id = controller_identity_field(&identity, "machine_id")?;
    let actual_machine_id =
        std::fs::read_to_string("/etc/machine-id").map_err(|error| AppError::ExternalCommand {
            role: "approved router controller identity",
            code: None,
            detail: Some(error.to_string()),
        })?;
    let approved_ids = approved.split(',').map(str::trim).collect::<Vec<_>>();
    if !approved_ids.contains(&controller_id.as_str())
        || declared_machine_id != actual_machine_id.trim()
    {
        return Err(AppError::ExternalCommand {
            role: "approved router controller identity",
            code: None,
            detail: Some("controller identity does not match the approved controller lock".into()),
        });
    }
    Ok(controller_id)
}

#[cfg(target_os = "linux")]
fn require_ansible_version(
    lock_path: &std::path::Path,
    ansible_binary: &std::path::Path,
) -> Result<(), AppError> {
    let lock = std::fs::read_to_string(lock_path).map_err(|error| AppError::ExternalCommand {
        role: "approved infrastructure controller lock",
        code: None,
        detail: Some(error.to_string()),
    })?;
    let expected = controller_identity_field(&lock, "ansible_core_version")?;
    let output = ProcessCommand::new(ansible_binary)
        .arg("--version")
        .output()
        .map_err(|error| AppError::ExternalCommand {
            role: "approved Ansible version",
            code: None,
            detail: Some(error.to_string()),
        })?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    if output.status.success() && stdout.contains(&format!("core {expected}")) {
        return Ok(());
    }
    Err(AppError::ExternalCommand {
        role: "approved Ansible version",
        code: output.status.code(),
        detail: Some(format!("expected ansible-core {expected}")),
    })
}

#[cfg(target_os = "linux")]
fn require_python_module_version(
    lock_path: &std::path::Path,
    ansible_binary: &std::path::Path,
    module: &'static str,
    lock_field: &str,
) -> Result<(), AppError> {
    let lock = std::fs::read_to_string(lock_path).map_err(|error| AppError::ExternalCommand {
        role: "approved infrastructure controller lock",
        code: None,
        detail: Some(error.to_string()),
    })?;
    let expected = controller_identity_field(&lock, lock_field)?;
    let canonical_ansible =
        std::fs::canonicalize(ansible_binary).map_err(|error| AppError::ExternalCommand {
            role: "approved Ansible Python runtime",
            code: None,
            detail: Some(error.to_string()),
        })?;
    let python = canonical_ansible
        .parent()
        .ok_or_else(|| AppError::ExternalCommand {
            role: "approved Ansible Python runtime",
            code: None,
            detail: Some("ansible-playbook has no parent runtime directory".into()),
        })?
        .join("python");
    require_infrastructure_file("approved Ansible Python runtime", &python)?;
    let code = format!("import importlib.metadata; print(importlib.metadata.version({module:?}))");
    let output = ProcessCommand::new(python)
        .args(["-c", &code])
        .output()
        .map_err(|error| AppError::ExternalCommand {
            role: "approved Ansible Python dependency",
            code: None,
            detail: Some(error.to_string()),
        })?;
    if output.status.success() && String::from_utf8_lossy(&output.stdout).trim() == expected {
        return Ok(());
    }
    Err(AppError::ExternalCommand {
        role: "approved Ansible Python dependency",
        code: output.status.code(),
        detail: Some(format!("expected Python module {module} {expected}")),
    })
}

#[cfg(target_os = "linux")]
fn controller_identity_field(content: &str, key: &str) -> Result<String, AppError> {
    content
        .lines()
        .find_map(|line| line.trim().strip_prefix(&format!("{key}:")))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| AppError::ExternalCommand {
            role: "approved router controller identity",
            code: None,
            detail: Some(format!("required {key} field is missing")),
        })
}

#[cfg(target_os = "linux")]
fn required_run_id(variable: &str, role: &'static str) -> Result<String, AppError> {
    let value = std::env::var(variable).map_err(|_| AppError::ExternalCommand {
        role,
        code: None,
        detail: Some(format!("{variable} is required")),
    })?;
    let named_run_id = (8..=96).contains(&value.len())
        && value.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
    if named_run_id || is_uuid_v7_run_id(&value) {
        Ok(value)
    } else {
        Err(AppError::ExternalCommand {
            role,
            code: None,
            detail: Some(format!(
                "{variable} must be an explicit lowercase run identifier or UUIDv7"
            )),
        })
    }
}

#[cfg(target_os = "linux")]
fn is_uuid_v7_run_id(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 36
        || ![8, 13, 18, 23]
            .iter()
            .all(|index| bytes.get(*index) == Some(&b'-'))
        || bytes.get(14) != Some(&b'7')
        || !matches!(bytes.get(19), Some(b'8'..=b'b'))
    {
        return false;
    }
    bytes.iter().enumerate().all(|(index, byte)| {
        matches!(index, 8 | 13 | 18 | 23) || byte.is_ascii_digit() || matches!(byte, b'a'..=b'f')
    })
}

#[cfg(target_os = "linux")]
fn run_infrastructure(
    environment: &str,
    playbook_name: &str,
    command: &'static str,
) -> Result<(), AppError> {
    run_infrastructure_with_package(environment, playbook_name, command, None, &[])
}

#[cfg(not(target_os = "linux"))]
fn run_infrastructure_with_package(
    _environment: &str,
    _playbook_name: &str,
    command: &'static str,
    _package_manifest: Option<&Path>,
    _extra_environment: &[(&str, String)],
) -> Result<(), AppError> {
    Err(AppError::UnsupportedPlatform { command })
}

#[cfg(target_os = "linux")]
fn run_infrastructure_with_package(
    environment: &str,
    playbook_name: &str,
    _command: &'static str,
    package_manifest: Option<&Path>,
    extra_environment: &[(&str, String)],
) -> Result<(), AppError> {
    use ansible::{Play, Playbook};

    let InfrastructureInputs {
        inventory,
        vault_password,
        playbook,
        ansible_config,
        collections_path,
        roles_path,
        commit_sha,
        controller_id,
        inventory_hash,
        component_lock_hash,
        harbor_data_backup_locator,
        identity_secret_locator,
    } = InfrastructureInputs::load(environment, playbook_name)?;
    let run_id = required_run_id("LABWEAVER_RUN_ID", "infrastructure run identity")?;
    let testflight_run_id = required_run_id(
        "LABWEAVER_TESTFLIGHT_RUN_ID",
        "infrastructure TestFlight identity",
    )?;
    let mut runner = Playbook::default();
    runner
        .set_system_envs()
        .filter_envs(["HOME"])
        .add_env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .add_env("ANSIBLE_CONFIG", ansible_config)
        .add_env("ANSIBLE_COLLECTIONS_PATH", collections_path.clone())
        // ansible-rs may launch from a different working directory; pass the
        // documented plural variable as well as the legacy spelling.
        .add_env("ANSIBLE_COLLECTIONS_PATHS", collections_path)
        .add_env("ANSIBLE_ROLES_PATH", roles_path)
        .add_env("ANSIBLE_AUTO_INSTALL", "false")
        .add_env("ANSIBLE_NOCOWS", "1")
        .add_env("ANSIBLE_VAULT_PASSWORD_FILE", vault_password)
        .add_env("LABWEAVER_RUN_ID", &run_id)
        .add_env("LABWEAVER_COMMIT_SHA", &commit_sha)
        .add_env(
            "LABWEAVER_PACKAGE_SOURCE_COMMIT",
            std::env::var("LABWEAVER_PACKAGE_SOURCE_COMMIT").unwrap_or_else(|_| String::new()),
        )
        .add_env("LABWEAVER_CONTROLLER_ID", &controller_id)
        .add_env("LABWEAVER_INVENTORY_HASH", &inventory_hash)
        .add_env("LABWEAVER_COMPONENT_LOCK_HASH", &component_lock_hash)
        .add_env(
            "LABWEAVER_HARBOR_DATA_BACKUP_LOCATOR",
            harbor_data_backup_locator,
        )
        .add_env("LABWEAVER_TESTFLIGHT_RUN_ID", &testflight_run_id)
        .add_env(
            "LABWEAVER_PACKAGE_MANIFEST",
            package_manifest.map_or_else(String::new, infrastructure_path),
        )
        .add_env(
            "LABWEAVER_PLATFORM_RESET_CONFIRMATION",
            std::env::var("LABWEAVER_PLATFORM_RESET_CONFIRMATION").unwrap_or_default(),
        )
        .add_env("LABWEAVER_IDENTITY_SECRET_LOCATOR", identity_secret_locator)
        .set_inventory(&inventory);
    for (name, value) in extra_environment {
        runner.add_env(*name, value);
    }
    // ansible-rs 1.1.0 appends configured arguments twice in `run`; all
    // controller identity and vault inputs therefore travel through the
    // explicit environment contract above.
    runner
        .run(Play::from_file(playbook))
        .map(|_| ())
        .map_err(|error| AppError::ExternalCommand {
            role: "allowlisted infrastructure playbook",
            code: None,
            detail: Some(format!("ansible-rs returned a non-zero result: {error:?}")),
        })
}

#[cfg(not(target_os = "linux"))]
fn run_infrastructure(
    _environment: &str,
    _playbook_name: &str,
    command: &'static str,
) -> Result<(), AppError> {
    Err(AppError::UnsupportedPlatform { command })
}

fn run_cargo<const N: usize>(role: &'static str, arguments: [&str; N]) -> Result<(), AppError> {
    let status = ProcessCommand::new("cargo")
        .args(arguments)
        .status()
        .map_err(|error| AppError::ExternalCommand {
            role,
            code: None,
            detail: Some(error.to_string()),
        })?;
    if status.success() {
        Ok(())
    } else {
        Err(AppError::ExternalCommand {
            role,
            code: status.code(),
            detail: None,
        })
    }
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "linux")]
    use super::is_uuid_v7_run_id;
    use super::{
        EnvironmentArgs, IdentityFoundationAction, IdentityFoundationArgs, deploy,
        identity_foundation, platform_application, platform_buildkit, platform_foundation,
        platform_harbor_route,
    };

    #[cfg(target_os = "linux")]
    #[test]
    fn explicit_run_identity_accepts_only_uuidv7() {
        assert!(is_uuid_v7_run_id("019fa9d0-0000-7000-8000-000000000142"));
        assert!(!is_uuid_v7_run_id("019fa9d0-0000-6000-8000-000000000142"));
        assert!(!is_uuid_v7_run_id("019fa9d0-0000-7000-c000-000000000142"));
        assert!(!is_uuid_v7_run_id("019FA9D0-0000-7000-8000-000000000142"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn infrastructure_collections_fall_back_to_shared_controller_root() -> Result<(), String> {
        let source_root = tempfile::tempdir().map_err(|error| error.to_string())?;
        let controller_root = tempfile::tempdir().map_err(|error| error.to_string())?;
        let collections = controller_root.path().join("collections");
        std::fs::create_dir(&collections).map_err(|error| error.to_string())?;

        let resolved = super::resolve_infrastructure_directory(
            "approved Ansible collections",
            [source_root.path(), controller_root.path()],
            "collections",
        )
        .map_err(|error| error.to_string())?;

        if resolved != collections {
            return Err("shared controller collections were not selected".into());
        }
        Ok(())
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn infrastructure_file_resolution_prefers_the_controlled_controller() -> Result<(), String> {
        let source_root = tempfile::tempdir().map_err(|error| error.to_string())?;
        let controller_root = tempfile::tempdir().map_err(|error| error.to_string())?;
        let source_file = source_root.path().join("inventories/demo/hosts.yml");
        let controller_file = controller_root.path().join("inventories/demo/hosts.yml");
        std::fs::create_dir_all(source_file.parent().ok_or("source parent")?)
            .map_err(|error| error.to_string())?;
        std::fs::create_dir_all(controller_file.parent().ok_or("controller parent")?)
            .map_err(|error| error.to_string())?;
        std::fs::write(&source_file, b"source").map_err(|error| error.to_string())?;
        std::fs::write(&controller_file, b"controller").map_err(|error| error.to_string())?;

        let resolved = super::resolve_infrastructure_file(
            "infrastructure deployment input",
            [controller_root.path(), source_root.path()],
            "inventories/demo/hosts.yml",
        )
        .map_err(|error| error.to_string())?;

        if resolved != controller_file {
            return Err("controlled controller input was not selected".into());
        }
        Ok(())
    }

    fn identity_args(env: &str, infra: bool, yes: bool) -> IdentityFoundationArgs {
        IdentityFoundationArgs {
            environment: EnvironmentArgs {
                env: env.into(),
                infra,
                yes,
                package_manifest: None,
            },
            action: IdentityFoundationAction::Deploy,
        }
    }

    #[test]
    fn infrastructure_deploy_requires_explicit_confirmation() -> Result<(), String> {
        let Err(error) = deploy(&EnvironmentArgs {
            env: "dev".into(),
            infra: true,
            yes: false,
            package_manifest: None,
        }) else {
            return Err("an infrastructure deployment without --yes must fail".into());
        };

        if error.diagnostic_code() != "XTASK_CONFIRMATION_REQUIRED" {
            return Err("unexpected confirmation diagnostic".into());
        }
        if error.to_string() != "deploy is a destructive operation and requires explicit --yes" {
            return Err("unexpected confirmation message".into());
        }
        Ok(())
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn controller_identity_field_rejects_an_unapproved_controller() -> Result<(), String> {
        let locked = super::controller_identity_field(
            "approved_controller_ids: edge-router,wsl-a-controller\n",
            "approved_controller_ids",
        )
        .map_err(|error| error.to_string())?;
        let presented = super::controller_identity_field(
            "controller_id: unapproved-linux-host\n",
            "controller_id",
        )
        .map_err(|error| error.to_string())?;
        if locked == presented {
            return Err("an unapproved Linux controller was accepted".into());
        }
        Ok(())
    }

    #[test]
    fn product_deploy_requires_an_explicit_verified_manifest() -> Result<(), String> {
        let Err(error) = deploy(&EnvironmentArgs {
            env: "dev".into(),
            infra: false,
            yes: true,
            package_manifest: None,
        }) else {
            return Err(
                "a product deployment must not silently run infrastructure reconciliation".into(),
            );
        };

        if error.diagnostic_code() != "XTASK_INVALID_ARGUMENT" {
            return Err("unexpected product deployment diagnostic".into());
        }
        if !error
            .to_string()
            .contains("product deployment package manifest")
        {
            return Err("product deployment diagnostic omitted the product path".into());
        }
        Ok(())
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn infra_deploy_has_a_stable_non_linux_diagnostic() -> Result<(), String> {
        let Err(error) = deploy(&EnvironmentArgs {
            env: "dev".into(),
            infra: true,
            yes: true,
            package_manifest: None,
        }) else {
            return Err("non-Linux infrastructure deployment must fail".into());
        };

        if error.diagnostic_code() != "XTASK_INFRA_UNSUPPORTED_PLATFORM" {
            return Err("unexpected non-Linux diagnostic".into());
        }
        Ok(())
    }

    #[test]
    fn identity_foundation_requires_confirmation_and_fixed_playbooks() -> Result<(), String> {
        let Err(error) = identity_foundation(&identity_args("dev", true, false)) else {
            return Err("identity-foundation without --yes must fail".into());
        };
        assert_eq!(error.diagnostic_code(), "XTASK_CONFIRMATION_REQUIRED");
        assert_eq!(
            IdentityFoundationAction::Deploy.playbook(),
            "91-identity-foundation.yml"
        );
        assert_eq!(
            IdentityFoundationAction::Verify.playbook(),
            "92-identity-foundation-verify.yml"
        );
        Ok(())
    }

    #[test]
    fn platform_foundation_requires_confirmation() -> Result<(), String> {
        let Err(error) = platform_foundation(&EnvironmentArgs {
            env: "demo".into(),
            infra: true,
            yes: false,
            package_manifest: None,
        }) else {
            return Err("Sprint 2 foundation without --yes must fail".into());
        };
        assert_eq!(error.diagnostic_code(), "XTASK_CONFIRMATION_REQUIRED");
        Ok(())
    }

    #[test]
    fn platform_buildkit_requires_confirmation() -> Result<(), String> {
        let Err(error) = platform_buildkit(&EnvironmentArgs {
            env: "demo".into(),
            infra: true,
            yes: false,
            package_manifest: None,
        }) else {
            return Err("Sprint 2 BuildKit without --yes must fail".into());
        };
        assert_eq!(error.diagnostic_code(), "XTASK_CONFIRMATION_REQUIRED");
        Ok(())
    }

    #[test]
    fn platform_harbor_route_requires_confirmation() -> Result<(), String> {
        let Err(error) = platform_harbor_route(&EnvironmentArgs {
            env: "demo".into(),
            infra: true,
            yes: false,
            package_manifest: None,
        }) else {
            return Err("Sprint 2 Harbor route adoption without --yes must fail".into());
        };
        assert_eq!(error.diagnostic_code(), "XTASK_CONFIRMATION_REQUIRED");
        Ok(())
    }

    #[test]
    fn platform_application_requires_confirmation() -> Result<(), String> {
        let Err(error) = platform_application(&EnvironmentArgs {
            env: "demo".into(),
            infra: true,
            yes: false,
            package_manifest: None,
        }) else {
            return Err("Sprint 2 application adoption without --yes must fail".into());
        };
        assert_eq!(error.diagnostic_code(), "XTASK_CONFIRMATION_REQUIRED");
        Ok(())
    }
}
