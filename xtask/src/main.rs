//! Repository workflow entry point for `LabWeaver`.

use std::fmt::{Display, Formatter};
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
    infra: bool,
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
            #[cfg(not(target_os = "linux"))]
            Self::UnsupportedPlatform { command } => write!(
                formatter,
                "{command} must run on the approved Linux infrastructure controller"
            ),
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
            TestSuite::Contract => not_implemented("test --suite contract"),
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
        Command::Preflight(args) => preflight(&args),
        Command::Deploy(args) => deploy(&args),
        Command::Verify(args) => verify(&args),
        Command::Backup(args) => backup(&args),
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
    }
}

fn deploy(args: &EnvironmentArgs) -> Result<(), AppError> {
    if !args.yes {
        return Err(AppError::ConfirmationRequired { command: "deploy" });
    }
    if !args.infra {
        return not_implemented(format!("deploy --env {} (product deployment)", args.env));
    }
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

fn require_infrastructure(args: &EnvironmentArgs, command: &'static str) -> Result<(), AppError> {
    if args.infra {
        Ok(())
    } else {
        Err(AppError::NotImplemented {
            command: format!("{command} (product path)"),
        })
    }
}

#[cfg(target_os = "linux")]
fn run_infrastructure(
    environment: &str,
    playbook_name: &str,
    _command: &'static str,
) -> Result<(), AppError> {
    use ansible::{Play, Playbook};

    let root = std::env::current_dir().map_err(|error| AppError::ExternalCommand {
        role: "infrastructure controller working directory",
        code: None,
        detail: Some(error.to_string()),
    })?;
    let inventory = root
        .join("deploy/ansible/inventories")
        .join(environment)
        .join("hosts.yml");
    let vault_password = root
        .join("deploy/ansible/inventories")
        .join(environment)
        .join(".vault-password");
    let playbook = root.join("deploy/ansible/playbooks").join(playbook_name);
    for input in [&inventory, &vault_password, &playbook] {
        if !input.is_file() {
            return Err(AppError::ExternalCommand {
                role: "infrastructure deployment input",
                code: None,
                detail: Some(format!("required file is missing: {}", input.display())),
            });
        }
    }

    let ansible_binary = std::path::Path::new("/usr/bin/ansible-playbook");
    if !ansible_binary.is_file() {
        return Err(AppError::ExternalCommand {
            role: "approved ansible-playbook binary",
            code: None,
            detail: Some(
                "/usr/bin/ansible-playbook is required; PATH discovery is disabled".into(),
            ),
        });
    }
    let ansible_config = root.join("deploy/ansible/ansible.cfg");
    if !ansible_config.is_file() {
        return Err(AppError::ExternalCommand {
            role: "approved Ansible configuration",
            code: None,
            detail: Some(format!(
                "required file is missing: {}",
                ansible_config.display()
            )),
        });
    }

    let inventory = inventory.to_string_lossy().into_owned();
    let vault_password = vault_password.to_string_lossy().into_owned();
    let playbook = playbook.to_string_lossy().into_owned();
    let ansible_config = ansible_config.to_string_lossy().into_owned();
    let local_controller_root = root.join("deploy/ansible");
    let shared_controller_root = root
        .parent()
        .and_then(std::path::Path::parent)
        .map(|path| path.join("deploy/ansible"));
    let shared_controller_root =
        shared_controller_root.ok_or_else(|| AppError::ExternalCommand {
            role: "approved Ansible dependency root",
            code: None,
            detail: Some("router Ansible dependency root is missing".into()),
        })?;
    let collections_path = [
        local_controller_root.join("collections"),
        shared_controller_root.join("collections"),
    ]
    .into_iter()
    .find(|path| path.is_dir())
    .ok_or_else(|| AppError::ExternalCommand {
        role: "approved Ansible collections",
        code: None,
        detail: Some("locked Ansible collections are missing".into()),
    })?;
    let roles_path = [
        local_controller_root.join("roles"),
        shared_controller_root.join("roles"),
    ]
    .into_iter()
    .find(|path| path.is_dir())
    .ok_or_else(|| AppError::ExternalCommand {
        role: "approved Ansible roles",
        code: None,
        detail: Some("locked Ansible roles are missing".into()),
    })?;
    let collections_path = collections_path.to_string_lossy().into_owned();
    let roles_path = roles_path.to_string_lossy().into_owned();
    let commit_sha =
        std::env::var("LABWEAVER_SOURCE_COMMIT").map_err(|_| AppError::ExternalCommand {
            role: "infrastructure source identity",
            code: None,
            detail: Some(
                "LABWEAVER_SOURCE_COMMIT is required and must be the verified bundle commit".into(),
            ),
        })?;
    if !commit_sha
        .chars()
        .all(|character| character.is_ascii_hexdigit())
        || !(40..=64).contains(&commit_sha.len())
    {
        return Err(AppError::ExternalCommand {
            role: "infrastructure source identity",
            code: None,
            detail: Some(
                "LABWEAVER_SOURCE_COMMIT must contain 40-64 hexadecimal characters".into(),
            ),
        });
    }
    let run_id = format!(
        "infra-{}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|error| AppError::ExternalCommand {
                role: "infrastructure run identity",
                code: None,
                detail: Some(error.to_string()),
            })?
            .as_secs(),
        std::process::id()
    );
    let testflight_run_id = format!(
        "testflight-{}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|error| AppError::ExternalCommand {
                role: "infrastructure TestFlight identity",
                code: None,
                detail: Some(error.to_string()),
            })?
            .as_secs(),
        std::process::id()
    );
    let mut runner = Playbook::default();
    runner
        .set_system_envs()
        .filter_envs(["HOME"])
        .add_env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .add_env("ANSIBLE_CONFIG", ansible_config)
        .add_env("ANSIBLE_COLLECTIONS_PATH", collections_path)
        .add_env("ANSIBLE_ROLES_PATH", roles_path)
        .add_env("ANSIBLE_AUTO_INSTALL", "false")
        .add_env("ANSIBLE_NOCOWS", "1")
        .add_env("ANSIBLE_VAULT_PASSWORD_FILE", vault_password)
        .add_env("LABWEAVER_RUN_ID", run_id)
        .add_env("LABWEAVER_COMMIT_SHA", commit_sha)
        .add_env("LABWEAVER_TESTFLIGHT_RUN_ID", testflight_run_id)
        .set_inventory(&inventory);
    // ansible-rs 1.1.0 appends configured arguments twice in `run`; all
    // controller identity and vault inputs therefore travel through the
    // explicit environment contract above.
    runner
        .run(Play::from_file(playbook))
        .map(|_| ())
        .map_err(|_| AppError::ExternalCommand {
            role: "harbor infrastructure playbook",
            code: None,
            detail: Some(
                "ansible-rs returned a non-zero result; inspect the controller event log".into(),
            ),
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
    use super::{EnvironmentArgs, deploy};

    #[test]
    fn infrastructure_deploy_requires_explicit_confirmation() -> Result<(), String> {
        let Err(error) = deploy(&EnvironmentArgs {
            env: "dev".into(),
            infra: true,
            yes: false,
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

    #[test]
    fn product_deploy_never_selects_the_infrastructure_path() -> Result<(), String> {
        let Err(error) = deploy(&EnvironmentArgs {
            env: "dev".into(),
            infra: false,
            yes: true,
        }) else {
            return Err(
                "a product deployment must not silently run infrastructure reconciliation".into(),
            );
        };

        if error.diagnostic_code() != "XTASK_NOT_IMPLEMENTED" {
            return Err("unexpected product deployment diagnostic".into());
        }
        if !error.to_string().contains("product deployment") {
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
        }) else {
            return Err("non-Linux infrastructure deployment must fail".into());
        };

        if error.diagnostic_code() != "XTASK_INFRA_UNSUPPORTED_PLATFORM" {
            return Err("unexpected non-Linux diagnostic".into());
        }
        Ok(())
    }
}
