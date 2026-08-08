//! Repository workflow entry point for `LabWeaver`.

use std::fmt::{Display, Formatter};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, ExitCode};

use clap::{Args, Parser, Subcommand, ValueEnum};
#[cfg(target_os = "linux")]
use sha2::{Digest, Sha256};

mod acceptance_assets;
mod console_evidence;
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
mod execution_ledger;
mod integration;
mod local_preflight;
mod migration_catalog;
mod platform_images;
mod release_gate;

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
    Bootstrap(ConfirmArgs),
    Preflight(EnvironmentArgs),
    Deploy(EnvironmentArgs),
    Verify(EnvironmentArgs),
    Backup(EnvironmentArgs),
    /// Reconcile or verify the private Keycloak identity foundation.
    IdentityFoundation(IdentityFoundationArgs),
    /// Reconcile the persistent `PostgreSQL`, NATS, and `MinIO` Sprint 2 foundation.
    Sprint2Foundation(EnvironmentArgs),
    /// Reconcile the dedicated rootless `BuildKit` Sprint 2 foundation.
    Sprint2Buildkit(EnvironmentArgs),
    /// Adopt the existing Harbor Gateway route without reconciling Harbor state.
    Sprint2HarborRoute(EnvironmentArgs),
    /// Adopt existing data services and atomically deploy the Sprint 2 application profile.
    Sprint2Application(EnvironmentArgs),
    /// Deploy the independently reviewed Resource authority profile.
    ResourceApplication(EnvironmentArgs),
    /// Execute the identity-bound public Resource Lease acceptance replay.
    #[command(subcommand)]
    Resource(ResourceCommand),
    /// Read-only Docker Desktop capability discovery for local validation.
    #[command(subcommand)]
    Local(LocalCommand),
    Upgrade(UpgradeArgs),
    Rollback(RollbackArgs),
    Restore(RestoreArgs),
    Destroy(EnvironmentArgs),
    #[command(subcommand)]
    Demo(DemoCommand),
    #[command(subcommand)]
    Playwright(PlaywrightCommand),
    #[command(subcommand)]
    Docs(DocsCommand),
    Tools(ConfirmArgs),
    DevDeps(ConfirmArgs),
    Migrate(ConfirmArgs),
    Dev(ConfirmArgs),
    Package(PackageArgs),
    PackageValidate(PackageValidateArgs),
    ReleaseGate,
    /// Validate a sanitized connected xterm/noVNC evidence report without executing a provider.
    ConsoleEvidence(ConsoleEvidenceArgs),
    /// Validate frozen Sprint 3 acceptance assets without executing a provider.
    AcceptanceAssets(AcceptanceAssetsArgs),
    #[command(subcommand)]
    Contracts(ContractsCommand),
}

#[derive(Debug, Args)]
struct AcceptanceAssetsArgs {
    #[command(subcommand)]
    action: AcceptanceAssetsAction,
}

#[derive(Debug, Args)]
struct ConsoleEvidenceArgs {
    #[command(subcommand)]
    action: ConsoleEvidenceAction,
}

#[derive(Debug, Subcommand)]
enum ConsoleEvidenceAction {
    ValidateReport {
        #[arg(long)]
        report: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
enum AcceptanceAssetsAction {
    /// Fail closed unless every checked-in acceptance asset is internally consistent.
    Validate,
    /// Print the three future live E4 scenario identifiers.
    List,
    ValidateReport {
        #[arg(long)]
        report: PathBuf,
    },
    ValidateFeatureComplete {
        #[arg(long)]
        report: PathBuf,
    },
    ValidateFixtures,
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
    E2e,
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
    #[arg(long, value_enum, default_value_t = PackageProfile::Sprint2)]
    profile: PackageProfile,
    #[arg(long)]
    yes: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum PackageProfile {
    Sprint2,
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
struct ConfirmArgs {
    #[arg(long)]
    yes: bool,
}

#[derive(Debug, Args)]
struct UpgradeArgs {
    #[arg(long)]
    env: String,
    #[arg(long)]
    version: String,
    #[arg(long)]
    yes: bool,
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

#[derive(Debug, Args)]
struct RestoreArgs {
    #[arg(long)]
    env: String,
    #[arg(long)]
    backup_id: String,
    #[arg(long)]
    yes: bool,
}

#[derive(Debug, Subcommand)]
enum DemoCommand {
    Seed(EnvironmentArgs),
    Replay,
    Reset(EnvironmentArgs),
}

#[derive(Debug, Subcommand)]
enum ResourceCommand {
    /// Produce fresh private browser BFF sessions for the public Resource replay.
    Auth(EnvironmentArgs),
    /// Replay the Work publication and Resource Lease lifecycle through public APIs only.
    Replay(ResourceReplayArgs),
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

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum ResourceReplayMode {
    Connected,
    Local,
}

#[derive(Debug, Args)]
struct ResourceReplayArgs {
    #[arg(long)]
    env: String,
    #[arg(long)]
    profile: PathBuf,
    #[arg(long)]
    authentication: PathBuf,
    #[arg(long)]
    deployment_manifest: PathBuf,
    #[arg(long)]
    package_manifest: PathBuf,
    #[arg(long, value_enum, default_value_t = ResourceReplayMode::Connected)]
    mode: ResourceReplayMode,
    #[arg(long)]
    preflight: bool,
}

#[derive(Debug, Subcommand)]
enum PlaywrightCommand {
    Install,
}

#[derive(Debug, Subcommand)]
enum DocsCommand {
    Serve,
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
    NotImplemented {
        command: String,
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
    AcceptanceAsset {
        code: &'static str,
        detail: String,
    },
    Integration {
        code: &'static str,
        detail: String,
    },
    ReleaseGate {
        code: &'static str,
        detail: String,
    },
    #[allow(dead_code)]
    ExecutionLedger {
        code: &'static str,
        detail: String,
    },
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
            Self::NotImplemented { .. } => "XTASK_NOT_IMPLEMENTED",
            Self::ConfirmationRequired { .. } => "XTASK_CONFIRMATION_REQUIRED",
            Self::Io { .. } => "XTASK_IO_FAILED",
            Self::ContractDrift { .. } => "LW_CONTRACT_DRIFT",
            Self::PlatformImage { code, .. } => code,
            Self::AcceptanceAsset { code, .. } => code,
            Self::Integration { code, .. } => code,
            Self::ReleaseGate { code, .. } => code,
            Self::ExecutionLedger { code, .. } => code,
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
            Self::NotImplemented { command } => write!(
                formatter,
                "{command} is declared in the design but has no implementation in this checkout"
            ),
            Self::ConfirmationRequired { command } => write!(
                formatter,
                "{command} is a destructive operation and requires explicit --yes"
            ),
            Self::Io { role, detail } => write!(formatter, "{role} failed: {detail}"),
            Self::ContractDrift { path } => {
                write!(formatter, "generated contract differs from {path}")
            }
            Self::PlatformImage { code, detail } => write!(formatter, "{code}: {detail}"),
            Self::AcceptanceAsset { detail, .. } => write!(formatter, "{detail}"),
            Self::Integration { detail, .. } => write!(formatter, "{detail}"),
            Self::ReleaseGate { detail, .. } => write!(formatter, "{detail}"),
            Self::ExecutionLedger { detail, .. } => write!(formatter, "{detail}"),
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
            TestSuite::E2e => not_implemented("test --suite e2e"),
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
        Command::Bootstrap(args) => destructive_not_implemented("bootstrap", args.yes),
        Command::Preflight(args) => preflight(&args),
        Command::Deploy(args) => deploy(&args),
        Command::Verify(args) => verify(&args),
        Command::Backup(args) => backup(&args),
        Command::IdentityFoundation(args) => identity_foundation(&args),
        Command::Sprint2Foundation(args) => sprint2_foundation(&args),
        Command::Sprint2Buildkit(args) => sprint2_buildkit(&args),
        Command::Sprint2HarborRoute(args) => sprint2_harbor_route(&args),
        Command::Sprint2Application(args) => sprint2_application(&args),
        Command::ResourceApplication(args) => resource_application(&args),
        Command::Resource(ResourceCommand::Auth(args)) => resource_replay_auth(&args),
        Command::Resource(ResourceCommand::Replay(args)) => resource_replay(&args),
        Command::Local(LocalCommand::Preflight(args)) => {
            local_preflight::run(&repository_root(), &args.profile)
        }
        Command::AcceptanceAssets(args) => run_acceptance_assets(args),
        Command::Upgrade(args) => destructive_not_implemented("upgrade", args.yes),
        Command::Rollback(args) => platform_images::rollback(
            &args.env,
            &args.release_revision,
            args.yes,
            &repository_root(),
        ),
        Command::Restore(args) => destructive_not_implemented("restore", args.yes),
        Command::Destroy(args) => destructive_not_implemented("destroy", args.yes),
        Command::Demo(command) => match command {
            DemoCommand::Seed(args) => not_implemented(format!("demo seed --env {}", args.env)),
            DemoCommand::Replay => demo_replay(),
            DemoCommand::Reset(args) => sprint2_reset(&args),
        },
        Command::Playwright(PlaywrightCommand::Install) => not_implemented("playwright install"),
        Command::Docs(DocsCommand::Serve) => not_implemented("docs serve"),
        Command::Tools(args) => destructive_not_implemented("tools", args.yes),
        Command::DevDeps(args) => destructive_not_implemented("dev-deps", args.yes),
        Command::Migrate(args) => destructive_not_implemented("migrate", args.yes),
        Command::Dev(args) => destructive_not_implemented("dev", args.yes),
        Command::Package(args) => package_command(&args),
        Command::PackageValidate(args) => platform_images::validate(
            &args.manifest,
            args.mode == PackageValidationMode::Connected,
            args.env.as_deref(),
            &repository_root(),
        ),
        Command::ReleaseGate => release_gate::run(&repository_root()),
        Command::ConsoleEvidence(args) => match args.action {
            ConsoleEvidenceAction::ValidateReport { report } => {
                console_evidence::validate_report(&repository_root(), &report)
            }
        },
        Command::Contracts(ContractsCommand::Generate) => contracts_generate(),
        Command::Contracts(ContractsCommand::Check) => contracts_check(),
    }
}

fn run_acceptance_assets(args: AcceptanceAssetsArgs) -> Result<(), AppError> {
    match args.action {
        AcceptanceAssetsAction::Validate => acceptance_assets::validate(&repository_root()),
        AcceptanceAssetsAction::List => {
            acceptance_assets::list();
            Ok(())
        }
        AcceptanceAssetsAction::ValidateReport { report } => {
            acceptance_assets::validate_report(&repository_root(), &report)
        }
        AcceptanceAssetsAction::ValidateFeatureComplete { report } => {
            acceptance_assets::validate_feature_complete(&repository_root(), &report)
        }
        AcceptanceAssetsAction::ValidateFixtures => {
            acceptance_assets::validate_fixtures(&repository_root())
        }
    }
}

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn git_output_any<const N: usize>(root: &Path, arguments: [&str; N]) -> Result<String, AppError> {
    let output = ProcessCommand::new("git")
        .current_dir(root)
        .args(arguments)
        .output()
        .map_err(|error| AppError::ExternalCommand {
            role: "read Git source identity",
            code: None,
            detail: Some(error.to_string()),
        })?;
    if !output.status.success() {
        return Err(AppError::ExternalCommand {
            role: "read Git source identity",
            code: output.status.code(),
            detail: Some(String::from_utf8_lossy(&output.stderr).trim().to_owned()),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
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
        PackageProfile::Sprint2 => "sprint2",
        PackageProfile::Resource => "resource",
    };
    if !args.yes {
        return Err(AppError::ConfirmationRequired { command: "package" });
    }
    #[cfg(target_os = "linux")]
    {
        let root = repository_root();
        let source_commit = git_output(&root, ["rev-parse", "HEAD"])?;
        let component_lock_hash = file_sha256(&root.join("deploy/versions.lock.yml"))?;
        let migration_catalog_hash = file_sha256(&root.join("migrations/catalog.yaml"))?;
        let configuration_sha256 = identity_hash(&[
            &component_lock_hash,
            &migration_catalog_hash,
            profile,
            &args.release,
        ]);
        let run_id = format!(
            "pkg-{}-{}-{}",
            args.env,
            args.release,
            &source_commit[..source_commit.len().min(12)]
        );
        let root_path = std::env::var_os("LABWEAVER_EXECUTION_LEDGER_ROOT")
            .map(PathBuf::from)
            .ok_or_else(|| AppError::ExecutionLedger {
                code: "LW_EXECUTION_LEDGER_ROOT_MISSING",
                detail: "LABWEAVER_EXECUTION_LEDGER_ROOT is required for connected packaging; provide a private controller directory before starting another package".to_owned(),
            })?;
        let lease = execution_ledger::acquire(
            &root_path,
            execution_ledger::ExecutionIdentity {
                // Keep the ledger operation stable across release labels. The
                // release label participates in configuration_sha256 above, so
                // distinct packages remain distinct candidates while the
                // operation-wide cycle budget still prevents endless retries.
                operation: format!("package-{profile}"),
                environment: args.env.clone(),
                source_commit,
                configuration_sha256: Some(configuration_sha256),
                package_sha256: None,
                deployment_sha256: None,
                run_id,
                testflight_run_id: None,
            },
            1,
            3,
        )
        .map_err(|error| AppError::ExecutionLedger {
            code: error.diagnostic_code(),
            detail: "a package with this candidate identity is already active, exhausted, or requires operator inspection; do not start another package".to_owned(),
        })?;
        let result = platform_images::package(&args.env, &args.release, profile, args.yes, &root);
        let diagnostic = result.as_ref().err().map(AppError::diagnostic_code);
        lease
            .finish(result.is_ok(), diagnostic)
            .map_err(|error| AppError::ExecutionLedger {
                code: error.diagnostic_code(),
                detail: "could not finalize the package execution ledger; package state must be inspected before another package".to_owned(),
            })?;
        result
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

fn sprint2_foundation(args: &EnvironmentArgs) -> Result<(), AppError> {
    if !args.yes {
        return Err(AppError::ConfirmationRequired {
            command: "sprint2-foundation",
        });
    }
    require_infrastructure(args, "sprint2-foundation --infra")?;
    if args.package_manifest.is_some() {
        return Err(AppError::InvalidArgument {
            role: "Sprint 2 foundation does not accept --package-manifest",
        });
    }
    run_infrastructure(
        &args.env,
        "92-sprint2-foundation.yml",
        "sprint2-foundation --infra",
    )
}

fn sprint2_buildkit(args: &EnvironmentArgs) -> Result<(), AppError> {
    if !args.yes {
        return Err(AppError::ConfirmationRequired {
            command: "sprint2-buildkit",
        });
    }
    require_infrastructure(args, "sprint2-buildkit --infra")?;
    if args.package_manifest.is_some() {
        return Err(AppError::InvalidArgument {
            role: "Sprint 2 BuildKit does not accept --package-manifest",
        });
    }
    run_infrastructure(
        &args.env,
        "92-sprint2-buildkit.yml",
        "sprint2-buildkit --infra",
    )
}

fn sprint2_harbor_route(args: &EnvironmentArgs) -> Result<(), AppError> {
    if !args.yes {
        return Err(AppError::ConfirmationRequired {
            command: "sprint2-harbor-route",
        });
    }
    require_infrastructure(args, "sprint2-harbor-route --infra")?;
    if args.package_manifest.is_some() {
        return Err(AppError::InvalidArgument {
            role: "Sprint 2 Harbor route adoption does not accept --package-manifest",
        });
    }
    run_infrastructure(
        &args.env,
        "92-sprint2-harbor-route.yml",
        "sprint2-harbor-route --infra",
    )
}

fn sprint2_application(args: &EnvironmentArgs) -> Result<(), AppError> {
    if !args.yes {
        return Err(AppError::ConfirmationRequired {
            command: "sprint2-application",
        });
    }
    require_infrastructure(args, "sprint2-application --infra")?;
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
        "93-sprint2-application.yml",
        "sprint2-application --infra",
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

fn resource_replay_auth(args: &EnvironmentArgs) -> Result<(), AppError> {
    if !args.yes {
        return Err(AppError::ConfirmationRequired {
            command: "resource auth",
        });
    }
    require_infrastructure(args, "resource auth --infra")?;
    if args.package_manifest.is_some() {
        return Err(AppError::InvalidArgument {
            role: "Resource replay browser authentication does not accept --package-manifest",
        });
    }
    run_infrastructure(
        &args.env,
        "94-resource-replay-auth.yml",
        "resource auth --infra",
    )
}

fn resource_replay(args: &ResourceReplayArgs) -> Result<(), AppError> {
    let (package_manifest, deployment) = validate_resource_replay_common(args)?;
    match args.mode {
        ResourceReplayMode::Local => resource_replay_local(args, &package_manifest, &deployment),
        ResourceReplayMode::Connected => {
            resource_replay_connected(args, &package_manifest, &deployment)
        }
    }
}

fn validate_resource_replay_common(
    args: &ResourceReplayArgs,
) -> Result<(PathBuf, serde_json::Value), AppError> {
    validate_environment_name(&args.env)?;
    for (role, path) in [
        ("Resource acceptance profile", &args.profile),
        (
            "Resource replay authentication locator",
            &args.authentication,
        ),
    ] {
        require_private_locator(role, path)?;
    }
    for (role, path) in [
        ("Resource deployment manifest", &args.deployment_manifest),
        ("Resource package manifest", &args.package_manifest),
    ] {
        require_regular_locator(role, path)?;
    }
    let package_manifest = args
        .package_manifest
        .canonicalize()
        .map_err(|error| AppError::Io {
            role: "resolve Resource replay package manifest",
            detail: error.to_string(),
        })?;
    let root = repository_root();
    platform_images::validate_profile(&package_manifest, "resource", &root)?;
    let deployment_manifest =
        fs::read(&args.deployment_manifest).map_err(|error| AppError::Io {
            role: "read Resource deployment manifest",
            detail: error.to_string(),
        })?;
    let deployment: serde_json::Value =
        serde_json::from_slice(&deployment_manifest).map_err(|error| AppError::Io {
            role: "parse Resource deployment manifest",
            detail: error.to_string(),
        })?;
    if deployment
        .get("schemaVersion")
        .and_then(serde_json::Value::as_str)
        != Some("resource-deployment-manifest.v1")
    {
        return Err(AppError::ReleaseGate {
            code: "LW_RESOURCE_REPLAY_DEPLOYMENT_MANIFEST_INVALID",
            detail: "Resource deployment manifest must use resource-deployment-manifest.v1"
                .to_owned(),
        });
    }
    Ok((package_manifest, deployment))
}

fn resource_replay_local(
    args: &ResourceReplayArgs,
    package_manifest: &Path,
    deployment: &serde_json::Value,
) -> Result<(), AppError> {
    if !args.preflight {
        return Err(AppError::Integration {
            code: "LW_LOCAL_REPLAY_WRITES_DISABLED",
            detail: "local replay is read-only in this workflow; run `cargo xtask local preflight` or pass --preflight".to_owned(),
        });
    }
    let deployment_run_id = resource_replay_run_id(deployment)?;
    let root = repository_root();
    let source_commit = git_output_any(&root, ["rev-parse", "HEAD"])?;
    validate_resource_replay_inputs_before_ledger(
        &args.profile,
        &args.authentication,
        &args.deployment_manifest,
        package_manifest,
        &source_commit,
        deployment_run_id,
    )?;
    let profile = args.profile.canonicalize().map_err(|error| AppError::Io {
        role: "resolve local Resource acceptance profile",
        detail: error.to_string(),
    })?;
    let authentication = args
        .authentication
        .canonicalize()
        .map_err(|error| AppError::Io {
            role: "resolve local Resource replay authentication locator",
            detail: error.to_string(),
        })?;
    let deployment_manifest =
        args.deployment_manifest
            .canonicalize()
            .map_err(|error| AppError::Io {
                role: "resolve local Resource deployment manifest",
                detail: error.to_string(),
            })?;
    let configuration_bundle_sha256 = deployment
        .get("configurationBundleSha256")
        .and_then(serde_json::Value::as_str)
        .filter(|value| is_sha256_digest(value))
        .ok_or(AppError::ReleaseGate {
            code: "LW_LOCAL_REPLAY_DEPLOYMENT_MANIFEST_INVALID",
            detail: "Resource deployment manifest must contain a valid configurationBundleSha256"
                .to_owned(),
        })?;
    let identity = serde_json::json!({
        "kind": "resource-replay-plan",
        "profile": local_preflight::file_identity(&root, &profile)?,
        "authentication": local_preflight::file_identity(&root, &authentication)?,
        "deploymentManifest": local_preflight::file_identity(&root, &deployment_manifest)?,
        "packageManifest": local_preflight::file_identity(&root, package_manifest)?,
        "resourceImage": local_preflight::resource_image_reference(package_manifest)?,
        "configurationBundleSha256": configuration_bundle_sha256,
    });
    local_preflight::run_with_identity(&root, "local-hostpath", Some(identity))
}

fn resource_replay_connected(
    args: &ResourceReplayArgs,
    package_manifest: &Path,
    deployment: &serde_json::Value,
) -> Result<(), AppError> {
    #[cfg(not(target_os = "linux"))]
    let _ = deployment;
    #[cfg(target_os = "linux")]
    {
        let deployment_run_id = resource_replay_run_id(deployment)?;
        let run_id = required_run_id("LABWEAVER_RUN_ID", "Resource replay run identity")?;
        if run_id != deployment_run_id {
            return Err(AppError::ReleaseGate {
                code: "LW_RESOURCE_REPLAY_DEPLOYMENT_IDENTITY_MISMATCH",
                detail: "LABWEAVER_RUN_ID must match the Resource deployment manifest before the connected ledger is acquired".to_owned(),
            });
        }
        let root = repository_root();
        let source_commit = git_output(&root, ["rev-parse", "HEAD"])?;
        validate_resource_replay_inputs_before_ledger(
            &args.profile,
            &args.authentication,
            &args.deployment_manifest,
            package_manifest,
            &source_commit,
            deployment_run_id,
        )?;
    }
    run_infrastructure_with_package(
        &args.env,
        "95-resource-replay.yml",
        "resource replay repair",
        Some(package_manifest),
        &[
            (
                "LABWEAVER_RESOURCE_REPLAY_PROFILE",
                args.profile.display().to_string(),
            ),
            (
                "LABWEAVER_RESOURCE_REPLAY_AUTHENTICATION",
                args.authentication.display().to_string(),
            ),
            (
                "LABWEAVER_RESOURCE_REPLAY_DEPLOYMENT_MANIFEST",
                args.deployment_manifest.display().to_string(),
            ),
        ],
    )
}

fn resource_replay_run_id(deployment: &serde_json::Value) -> Result<&str, AppError> {
    deployment
        .get("runId")
        .and_then(serde_json::Value::as_str)
        .ok_or(AppError::ReleaseGate {
            code: "LW_RESOURCE_REPLAY_DEPLOYMENT_MANIFEST_INVALID",
            detail: "Resource deployment manifest must contain runId".to_owned(),
        })
}

fn validate_resource_replay_inputs_before_ledger(
    profile: &Path,
    authentication: &Path,
    deployment_manifest: &Path,
    package_manifest: &Path,
    source_commit: &str,
    run_id: &str,
) -> Result<(), AppError> {
    let python = std::env::var("LABWEAVER_PYTHON").unwrap_or_else(|_| "python3".to_owned());
    let validator = repository_root().join("tools/validate_resource_replay_inputs.py");
    let profile = profile.to_string_lossy().into_owned();
    let authentication = authentication.to_string_lossy().into_owned();
    let deployment_manifest = deployment_manifest.to_string_lossy().into_owned();
    let package_manifest = package_manifest.to_string_lossy().into_owned();
    let output = ProcessCommand::new(python)
        .arg(&validator)
        .args([
            "--profile",
            &profile,
            "--authentication",
            &authentication,
            "--deployment-manifest",
            &deployment_manifest,
            "--package-manifest",
            &package_manifest,
            "--source-commit",
            source_commit,
            "--run-id",
            run_id,
        ])
        .current_dir(repository_root())
        .output()
        .map_err(|error| AppError::ReleaseGate {
            code: "LW_RESOURCE_REPLAY_INPUT_PREFLIGHT_FAILED",
            detail: format!("could not execute the replay input validator: {error}"),
        })?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let diagnostic = stderr
        .lines()
        .find(|line| line.starts_with("LW_"))
        .unwrap_or("LW_RESOURCE_REPLAY_INPUT_PREFLIGHT_FAILED");
    Err(AppError::ReleaseGate {
        code: "LW_RESOURCE_REPLAY_INPUT_PREFLIGHT_FAILED",
        detail: format!("replay input validator blocked the operation: {diagnostic}"),
    })
}

fn require_private_locator(role: &'static str, path: &Path) -> Result<(), AppError> {
    let canonical = path.canonicalize().map_err(|error| AppError::Io {
        role,
        detail: error.to_string(),
    })?;
    if canonical
        .components()
        .any(|component| component.as_os_str() == ".private")
    {
        Ok(())
    } else {
        Err(AppError::InvalidArgument { role })
    }
}

fn require_regular_locator(role: &'static str, path: &Path) -> Result<(), AppError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| AppError::Io {
        role,
        detail: error.to_string(),
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(AppError::InvalidArgument { role });
    }
    Ok(())
}

fn is_sha256_digest(value: &str) -> bool {
    value.len() == "sha256:".len() + 64
        && value.starts_with("sha256:")
        && value["sha256:".len()..]
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
}

fn require_infrastructure(args: &EnvironmentArgs, command: &'static str) -> Result<(), AppError> {
    if !args.infra {
        return Err(AppError::NotImplemented {
            command: format!("{command} (product path)"),
        });
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

fn demo_replay() -> Result<(), AppError> {
    let environment = std::env::var("LABWEAVER_DEMO_ENV").map_err(|_| AppError::ReleaseGate {
        code: "LW_DEMO_ENVIRONMENT_MISSING",
        detail: "LABWEAVER_DEMO_ENV is required".to_owned(),
    })?;
    validate_environment_name(&environment)?;
    let package_manifest =
        std::env::var("LABWEAVER_DEMO_PACKAGE_MANIFEST").map_err(|_| AppError::ReleaseGate {
            code: "LW_DEMO_PACKAGE_MANIFEST_MISSING",
            detail: "LABWEAVER_DEMO_PACKAGE_MANIFEST is required".to_owned(),
        })?;
    // Sprint 2 adopts retained infrastructure. Re-running the broad Harbor
    // installation verifier would bind the application replay to a historical
    // infrastructure-install commit and would require an unrelated Harbor data
    // backup/reconciliation. Reconcile and verify the current application
    // package through the allowlisted non-destructive adoption path instead.
    sprint2_application(&EnvironmentArgs {
        env: environment,
        infra: true,
        yes: true,
        package_manifest: Some(PathBuf::from(package_manifest)),
    })?;
    let resource_profile = required_environment_path("LABWEAVER_RESOURCE_REPLAY_PROFILE")?;
    let resource_authentication =
        required_environment_path("LABWEAVER_RESOURCE_REPLAY_AUTHENTICATION")?;
    let resource_deployment_manifest =
        required_environment_path("LABWEAVER_RESOURCE_DEPLOYMENT_MANIFEST")?;
    let resource_package_manifest =
        required_environment_path("LABWEAVER_RESOURCE_PACKAGE_MANIFEST")?;
    resource_replay(&ResourceReplayArgs {
        env: std::env::var("LABWEAVER_DEMO_ENV").map_err(|_| AppError::ReleaseGate {
            code: "LW_DEMO_ENVIRONMENT_MISSING",
            detail: "LABWEAVER_DEMO_ENV is required".to_owned(),
        })?,
        profile: resource_profile,
        authentication: resource_authentication,
        deployment_manifest: resource_deployment_manifest,
        package_manifest: resource_package_manifest,
        mode: ResourceReplayMode::Connected,
        preflight: false,
    })?;
    let status = ProcessCommand::new("pnpm")
        .args(["--dir=web", "test:e2e:live"])
        .current_dir(repository_root())
        .status()
        .map_err(|error| AppError::ExternalCommand {
            role: "live Playwright demo replay",
            code: None,
            detail: Some(error.to_string()),
        })?;
    if !status.success() {
        return Err(AppError::ExternalCommand {
            role: "live Playwright demo replay",
            code: status.code(),
            detail: None,
        });
    }
    release_gate::run(&repository_root())
}

fn required_environment_path(name: &'static str) -> Result<PathBuf, AppError> {
    let value = std::env::var(name).map_err(|_| AppError::ReleaseGate {
        code: "LW_RESOURCE_REPLAY_INPUT_MISSING",
        detail: format!("{name} is required"),
    })?;
    if value.trim().is_empty() {
        return Err(AppError::ReleaseGate {
            code: "LW_RESOURCE_REPLAY_INPUT_MISSING",
            detail: format!("{name} is required"),
        });
    }
    Ok(PathBuf::from(value))
}

fn sprint2_reset(args: &EnvironmentArgs) -> Result<(), AppError> {
    if !args.yes {
        return Err(AppError::ConfirmationRequired {
            command: "demo reset",
        });
    }
    require_infrastructure(args, "demo reset --infra")?;
    if args.package_manifest.is_some() {
        return Err(AppError::InvalidArgument {
            role: "Sprint 2 reset does not accept --package-manifest",
        });
    }
    run_infrastructure(&args.env, "93-sprint2-reset.yml", "demo reset --infra")
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
    command: &'static str,
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
            "LABWEAVER_SPRINT2_RESET_CONFIRMATION",
            std::env::var("LABWEAVER_SPRINT2_RESET_CONFIRMATION").unwrap_or_default(),
        )
        .add_env("LABWEAVER_IDENTITY_SECRET_LOCATOR", identity_secret_locator)
        .set_inventory(&inventory);
    for (name, value) in extra_environment {
        runner.add_env(*name, value);
    }
    let ledger = begin_execution_ledger(
        environment,
        command,
        &commit_sha,
        &inventory_hash,
        &component_lock_hash,
        &run_id,
        &testflight_run_id,
        package_manifest,
        extra_environment,
    )?;
    // ansible-rs 1.1.0 appends configured arguments twice in `run`; all
    // controller identity and vault inputs therefore travel through the
    // explicit environment contract above.
    let result = runner
        .run(Play::from_file(playbook))
        .map(|_| ())
        .map_err(|error| AppError::ExternalCommand {
            role: "allowlisted infrastructure playbook",
            code: None,
            detail: Some(format!("ansible-rs returned a non-zero result: {error:?}")),
        });
    if let Some(ledger) = ledger {
        let diagnostic = result.as_ref().err().map(AppError::diagnostic_code);
        ledger
            .finish(result.is_ok(), diagnostic)
            .map_err(|error| AppError::ExecutionLedger {
                code: error.diagnostic_code(),
                detail: "could not finalize the connected execution ledger; cluster state must be inspected before another write".to_owned(),
            })?;
    }
    result
}

#[cfg(target_os = "linux")]
fn execution_budget(command: &str) -> Option<(u32, u32)> {
    let command = command.to_ascii_lowercase();
    if command.contains("resource replay") {
        Some((1, 3))
    } else if command.contains("application") {
        Some((2, 3))
    } else if command.contains("identity-foundation-deploy")
        || command.contains("sprint2-foundation")
        || command.contains("sprint2-buildkit")
        || command.contains("sprint2-harbor-route")
        || command.contains("backup")
        || command.contains("deploy")
        || command.contains("reset")
    {
        Some((1, 1))
    } else if command.contains("resource auth") {
        // Browser BFF sessions are short-lived and must be refreshed immediately
        // before every replay attempt; keep the per-candidate fence but allow
        // one refresh per replay slot in the operation budget.
        Some((1, 3))
    } else {
        None
    }
}

#[cfg(target_os = "linux")]
#[allow(clippy::too_many_arguments)]
fn begin_execution_ledger(
    environment: &str,
    command: &str,
    source_commit: &str,
    inventory_hash: &str,
    component_lock_hash: &str,
    run_id: &str,
    testflight_run_id: &str,
    package_manifest: Option<&Path>,
    extra_environment: &[(&str, String)],
) -> Result<Option<execution_ledger::ExecutionLease>, AppError> {
    let Some((max_attempts, max_operation_attempts)) = execution_budget(command) else {
        return Ok(None);
    };
    let package_sha256 = package_manifest.map(file_sha256).transpose()?;
    let deployment_sha256 = extra_environment
        .iter()
        .find(|(name, _)| *name == "LABWEAVER_RESOURCE_REPLAY_DEPLOYMENT_MANIFEST")
        .map(|(_, value)| file_sha256(Path::new(value)))
        .transpose()?;
    let migration_catalog_sha256 = file_sha256(&repository_root().join("migrations/catalog.yaml"))?;
    let extra_environment_sha256 = extra_environment_identity(extra_environment)?;
    let configuration_sha256 = identity_hash(&[
        inventory_hash,
        component_lock_hash,
        &migration_catalog_sha256,
        &extra_environment_sha256,
    ]);
    let root = std::env::var_os("LABWEAVER_EXECUTION_LEDGER_ROOT")
        .map(PathBuf::from)
        .ok_or_else(|| AppError::ExecutionLedger {
            code: "LW_EXECUTION_LEDGER_ROOT_MISSING",
            detail: "LABWEAVER_EXECUTION_LEDGER_ROOT is required for connected execution; provide a private controller directory before starting another deployment".to_owned(),
        })?;
    execution_ledger::acquire(
        &root,
        execution_ledger::ExecutionIdentity {
            operation: command.to_owned(),
            environment: environment.to_owned(),
            source_commit: source_commit.to_owned(),
            configuration_sha256: Some(configuration_sha256),
            package_sha256,
            deployment_sha256,
            run_id: run_id.to_owned(),
            testflight_run_id: Some(testflight_run_id.to_owned()),
        },
        max_attempts,
        max_operation_attempts,
    )
    .map(Some)
    .map_err(|error| AppError::ExecutionLedger {
        code: error.diagnostic_code(),
        detail: "a connected operation with this target or candidate is already active, exhausted, or requires operator inspection; do not start another deployment".to_owned(),
    })
}

#[cfg(target_os = "linux")]
fn identity_hash(fields: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for field in fields {
        hasher.update(field.as_bytes());
        hasher.update([0]);
    }
    format!("sha256:{:x}", hasher.finalize())
}

#[cfg(target_os = "linux")]
fn extra_environment_identity(extra_environment: &[(&str, String)]) -> Result<String, AppError> {
    let mut fields = Vec::with_capacity(extra_environment.len());
    for (name, value) in extra_environment {
        let locator_hash = {
            let path = std::path::Path::new(value);
            if path.is_file() {
                file_sha256(path)?
            } else {
                "missing".to_owned()
            }
        };
        fields.push(format!("{name}={value}\0{locator_hash}"));
    }
    fields.sort();
    let references = fields.iter().map(String::as_str).collect::<Vec<_>>();
    Ok(identity_hash(&references))
}

#[cfg(target_os = "linux")]
struct InfrastructureInputs {
    inventory: String,
    vault_password: String,
    playbook: String,
    ansible_config: String,
    collections_path: String,
    roles_path: String,
    commit_sha: String,
    controller_id: String,
    inventory_hash: String,
    component_lock_hash: String,
    harbor_data_backup_locator: String,
    identity_secret_locator: String,
}

#[cfg(target_os = "linux")]
impl InfrastructureInputs {
    fn load(environment: &str, playbook_name: &str) -> Result<Self, AppError> {
        let root = infrastructure_root()?;
        let controller_root = root.join("deploy/ansible");
        let inventory_root = controller_root.join("inventories").join(environment);
        let inventory = inventory_root.join("hosts.yml");
        let vault_password = inventory_root.join(".vault-password");
        let playbook = controller_root.join("playbooks").join(playbook_name);
        require_infrastructure_file("infrastructure deployment input", &inventory)?;
        require_infrastructure_file("infrastructure deployment input", &vault_password)?;
        require_infrastructure_file("infrastructure deployment input", &playbook)?;
        let ansible_binary = std::path::Path::new("/usr/local/bin/ansible-playbook");
        require_infrastructure_file("approved ansible-playbook binary", ansible_binary)?;
        let ansible_config = controller_root.join("ansible.cfg");
        require_infrastructure_file("approved Ansible configuration", &ansible_config)?;
        let controller_lock = controller_root.join("controller.lock.yml");
        require_infrastructure_file("approved infrastructure controller lock", &controller_lock)?;
        require_ansible_version(&controller_lock, ansible_binary)?;
        require_python_module_version(
            &controller_lock,
            ansible_binary,
            "kubernetes",
            "python_kubernetes_version",
        )?;

        let shared_controller_root = infrastructure_dependency_root()?;
        let roles_path = resolve_infrastructure_directory(
            "approved Ansible roles",
            [controller_root.as_path(), shared_controller_root.as_path()],
            "roles",
        )?;

        let PlaybookLocators {
            identity_secret_locator,
        } = PlaybookLocators::load(playbook_name)?;

        Ok(Self {
            inventory: infrastructure_path(&inventory),
            vault_password: infrastructure_path(&vault_password),
            playbook: infrastructure_path(&playbook),
            ansible_config: infrastructure_path(&ansible_config),
            // Collection requirements are lockfile inputs, but a controller may
            // legitimately execute these playbooks using only built-in modules
            // and repository-local modules.  Requiring an otherwise unused
            // `collections/` directory made the allowlisted entrypoint reject
            // the same controller that Ansible could safely execute directly.
            // Keep the standard location explicit for Ansible without making a
            // non-existent directory a false deployment dependency.
            collections_path: infrastructure_path(&controller_root.join("collections")),
            roles_path: infrastructure_path(&roles_path),
            commit_sha: infrastructure_commit_sha()?,
            controller_id: approved_controller_identity(&controller_lock)?,
            inventory_hash: inventory_identity_hash(&inventory_root)?,
            component_lock_hash: file_sha256(&root.join("deploy/versions.lock.yml"))?,
            harbor_data_backup_locator: std::env::var("LABWEAVER_HARBOR_DATA_BACKUP_LOCATOR")
                .unwrap_or_default(),
            identity_secret_locator,
        })
    }
}

#[cfg(target_os = "linux")]
struct PlaybookLocators {
    identity_secret_locator: String,
}

#[cfg(target_os = "linux")]
impl PlaybookLocators {
    fn load(playbook_name: &str) -> Result<Self, AppError> {
        let identity_foundation = matches!(
            playbook_name,
            "91-identity-foundation.yml" | "92-identity-foundation-verify.yml"
        );
        Ok(Self {
            identity_secret_locator: locator(
                "LABWEAVER_IDENTITY_SECRET_LOCATOR",
                "identity-foundation secret locator",
                identity_foundation,
            )?,
        })
    }
}

#[cfg(target_os = "linux")]
fn locator(variable: &str, role: &'static str, required: bool) -> Result<String, AppError> {
    if required {
        required_environment_value(variable, role)
    } else {
        Ok(std::env::var(variable).unwrap_or_default())
    }
}

#[cfg(target_os = "linux")]
fn required_environment_value(variable: &str, role: &'static str) -> Result<String, AppError> {
    std::env::var(variable)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| AppError::ExternalCommand {
            role,
            code: None,
            detail: Some(format!("{variable} is required")),
        })
}

#[cfg(target_os = "linux")]
fn infrastructure_root() -> Result<std::path::PathBuf, AppError> {
    std::env::current_dir().map_err(|error| AppError::ExternalCommand {
        role: "infrastructure controller working directory",
        code: None,
        detail: Some(error.to_string()),
    })
}

#[cfg(target_os = "linux")]
fn infrastructure_dependency_root() -> Result<std::path::PathBuf, AppError> {
    let dependency_root = std::env::var("LABWEAVER_ANSIBLE_DEPENDENCY_ROOT").map_err(|_| {
        AppError::ExternalCommand {
            role: "approved Ansible dependency root",
            code: None,
            detail: Some(
                "LABWEAVER_ANSIBLE_DEPENDENCY_ROOT is required for the router-controlled collections"
                    .into(),
            ),
        }
    })?;
    let dependency_root = std::path::PathBuf::from(dependency_root);
    if dependency_root.is_dir() {
        return Ok(dependency_root);
    }
    Err(AppError::ExternalCommand {
        role: "approved Ansible dependency root",
        code: None,
        detail: Some("LABWEAVER_ANSIBLE_DEPENDENCY_ROOT is not a readable directory".into()),
    })
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

fn destructive_not_implemented(command: &'static str, confirmed: bool) -> Result<(), AppError> {
    if !confirmed {
        return Err(AppError::ConfirmationRequired { command });
    }
    not_implemented(command)
}

fn not_implemented(command: impl Into<String>) -> Result<(), AppError> {
    Err(AppError::NotImplemented {
        command: command.into(),
    })
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "linux")]
    use super::extra_environment_identity;
    #[cfg(target_os = "linux")]
    use super::is_uuid_v7_run_id;
    use super::{
        EnvironmentArgs, IdentityFoundationAction, IdentityFoundationArgs, deploy,
        identity_foundation, sprint2_application, sprint2_buildkit, sprint2_foundation,
        sprint2_harbor_route,
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
    fn connected_identity_changes_when_a_private_locator_changes() -> Result<(), String> {
        let directory = tempfile::tempdir().map_err(|error| error.to_string())?;
        let path = directory.path().join("locator.json");
        std::fs::write(&path, b"first").map_err(|error| error.to_string())?;
        let first = extra_environment_identity(&[(
            "LABWEAVER_RESOURCE_REPLAY_AUTHENTICATION",
            path.display().to_string(),
        )])
        .map_err(|error| error.to_string())?;
        std::fs::write(&path, b"second").map_err(|error| error.to_string())?;
        let second = extra_environment_identity(&[(
            "LABWEAVER_RESOURCE_REPLAY_AUTHENTICATION",
            path.display().to_string(),
        )])
        .map_err(|error| error.to_string())?;
        if first == second {
            return Err("connected identity ignored a changed private locator".into());
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
    fn sprint2_foundation_requires_confirmation() -> Result<(), String> {
        let Err(error) = sprint2_foundation(&EnvironmentArgs {
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
    fn sprint2_buildkit_requires_confirmation() -> Result<(), String> {
        let Err(error) = sprint2_buildkit(&EnvironmentArgs {
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
    fn sprint2_harbor_route_requires_confirmation() -> Result<(), String> {
        let Err(error) = sprint2_harbor_route(&EnvironmentArgs {
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
    fn sprint2_application_requires_confirmation() -> Result<(), String> {
        let Err(error) = sprint2_application(&EnvironmentArgs {
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
