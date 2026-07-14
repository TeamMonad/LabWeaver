//! Repository workflow entry point for `LabWeaver`.

use std::fmt::{Display, Formatter};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, ExitCode};

use clap::{Args, Parser, Subcommand, ValueEnum};

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
    Package,
    PackageValidate,
    ReleaseGate,
    #[command(subcommand)]
    Contracts(ContractsCommand),
}

#[derive(Debug, Args)]
struct TestArgs {
    #[arg(long, value_enum, default_value_t = TestSuite::All)]
    suite: TestSuite,
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
    yes: bool,
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
    Reset(ConfirmArgs),
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
}

impl AppError {
    const fn diagnostic_code(&self) -> &'static str {
        match self {
            Self::ExternalCommand { .. } => "XTASK_EXTERNAL_COMMAND_FAILED",
            Self::NotImplemented { .. } => "XTASK_NOT_IMPLEMENTED",
            Self::ConfirmationRequired { .. } => "XTASK_CONFIRMATION_REQUIRED",
            Self::Io { .. } => "XTASK_IO_FAILED",
            Self::ContractDrift { .. } => "LW_CONTRACT_DRIFT",
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
        }
    }
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("[{}] {error}", error.diagnostic_code());
            ExitCode::from(1)
        }
    }
}

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
            TestSuite::Contract => {
                run_cargo("contract tests", ["test", "-p", "contracts", "--locked"])?;
                contracts_check()
            }
            TestSuite::Integration => not_implemented("test --suite integration"),
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
                }),
            })
        }
        Command::Bootstrap(args) => destructive_not_implemented("bootstrap", args.yes),
        Command::Preflight(args) => destructive_not_implemented("preflight", args.yes),
        Command::Deploy(args) => destructive_not_implemented("deploy", args.yes),
        Command::Verify(args) => not_implemented(format!("verify --env {}", args.env)),
        Command::Backup(args) => destructive_not_implemented("backup", args.yes),
        Command::Upgrade(args) => destructive_not_implemented("upgrade", args.yes),
        Command::Rollback(args) => destructive_not_implemented("rollback", args.yes),
        Command::Restore(args) => destructive_not_implemented("restore", args.yes),
        Command::Destroy(args) => destructive_not_implemented("destroy", args.yes),
        Command::Demo(command) => match command {
            DemoCommand::Seed(args) => not_implemented(format!("demo seed --env {}", args.env)),
            DemoCommand::Replay => not_implemented("demo replay"),
            DemoCommand::Reset(args) => destructive_not_implemented("demo reset", args.yes),
        },
        Command::Playwright(PlaywrightCommand::Install) => not_implemented("playwright install"),
        Command::Docs(DocsCommand::Serve) => not_implemented("docs serve"),
        Command::Tools(args) => destructive_not_implemented("tools", args.yes),
        Command::DevDeps(args) => destructive_not_implemented("dev-deps", args.yes),
        Command::Migrate(args) => destructive_not_implemented("migrate", args.yes),
        Command::Dev(args) => destructive_not_implemented("dev", args.yes),
        Command::Package => not_implemented("package"),
        Command::PackageValidate => not_implemented("package-validate"),
        Command::ReleaseGate => not_implemented("release-gate"),
        Command::Contracts(ContractsCommand::Generate) => contracts_generate(),
        Command::Contracts(ContractsCommand::Check) => contracts_check(),
    }
}

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn contracts_generate() -> Result<(), AppError> {
    write_contract_artifacts(&repository_root())?;
    run_web_script("generate TypeScript contracts", "contracts:generate")
}

fn contracts_check() -> Result<(), AppError> {
    let root = repository_root();
    let temporary = std::env::temp_dir().join(format!(
        "labweaver-contracts-check-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|error| AppError::Io {
                role: "create contract check identity",
                detail: error.to_string()
            })?
            .as_nanos()
    ));
    fs::create_dir_all(&temporary).map_err(|error| AppError::Io {
        role: "create contract check directory",
        detail: error.to_string(),
    })?;
    let result = (|| {
        write_contract_artifacts(&temporary)?;
        for artifact in contracts::schema::generate_all().map_err(|error| AppError::Io {
            role: "generate contracts",
            detail: error.to_string(),
        })? {
            let checked_in =
                fs::read(root.join(&artifact.relative_path)).map_err(|error| AppError::Io {
                    role: "read checked-in contract",
                    detail: format!("{}: {error}", artifact.relative_path),
                })?;
            let regenerated =
                fs::read(temporary.join(&artifact.relative_path)).map_err(|error| {
                    AppError::Io {
                        role: "read regenerated contract",
                        detail: format!("{}: {error}", artifact.relative_path),
                    }
                })?;
            if checked_in != regenerated {
                return Err(AppError::ContractDrift {
                    path: artifact.relative_path,
                });
            }
        }
        Ok(())
    })();
    let cleanup = fs::remove_dir_all(&temporary).map_err(|error| AppError::Io {
        role: "remove contract check directory",
        detail: error.to_string(),
    });
    result?;
    cleanup?;
    run_web_script("check TypeScript contract drift", "contracts:check")
}

fn run_web_script(role: &'static str, script: &str) -> Result<(), AppError> {
    let status = ProcessCommand::new("pnpm")
        .args(["run", script])
        .current_dir(repository_root().join("web"))
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

fn write_contract_artifacts(root: &Path) -> Result<(), AppError> {
    let schema_root = root.join("schemas/contracts/v1");
    if schema_root.exists() {
        fs::remove_dir_all(&schema_root).map_err(|error| AppError::Io {
            role: "remove stale generated contract schemas",
            detail: error.to_string(),
        })?;
    }
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
        fs::write(&destination, artifact.bytes).map_err(|error| AppError::Io {
            role: "write contract output",
            detail: format!("{}: {error}", artifact.relative_path),
        })?;
    }
    Ok(())
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
