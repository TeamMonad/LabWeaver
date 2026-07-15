//! Repository workflow entry point for `LabWeaver`.

use std::fmt::{Display, Formatter};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, ExitCode};

use clap::{Args, Parser, Subcommand, ValueEnum};
#[cfg(target_os = "linux")]
use sha2::{Digest, Sha256};

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
    /// Run an allowlisted Private Sigstore lifecycle action.
    PrivateSigstore(PrivateSigstoreArgs),
    /// Reconcile or verify the private Keycloak identity foundation.
    IdentityFoundation(IdentityFoundationArgs),
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
    infra: bool,
    #[arg(long)]
    yes: bool,
}

#[derive(Debug, Args)]
struct PrivateSigstoreArgs {
    #[command(flatten)]
    environment: EnvironmentArgs,
    #[arg(long, value_enum)]
    action: PrivateSigstoreAction,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum PrivateSigstoreAction {
    Deploy,
    Backup,
    Restore,
    Rotate,
    Verify,
    Cleanup,
    DisasterRecovery,
}

impl PrivateSigstoreAction {
    const fn playbook(self) -> &'static str {
        match self {
            Self::Deploy => "96-private-sigstore.yml",
            Self::Backup => "97-private-sigstore-backup.yml",
            Self::Restore => "98-private-sigstore-restore.yml",
            Self::Rotate => "99-private-sigstore-rotate.yml",
            Self::Verify => "100-private-sigstore-verify.yml",
            Self::Cleanup => "101-private-sigstore-cleanup.yml",
            Self::DisasterRecovery => "102-private-sigstore-disaster-recovery.yml",
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
            TestSuite::Contract => contract_test_suite(),
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
        Command::PrivateSigstore(args) => private_sigstore(&args),
        Command::IdentityFoundation(args) => identity_foundation(&args),
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
        return not_implemented(format!("deploy --env {} (product deployment)", args.env));
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

fn private_sigstore(args: &PrivateSigstoreArgs) -> Result<(), AppError> {
    if !args.environment.yes {
        return Err(AppError::ConfirmationRequired {
            command: "private-sigstore",
        });
    }
    require_infrastructure(&args.environment, "private-sigstore --infra")?;
    run_infrastructure(
        &args.environment.env,
        args.action.playbook(),
        "private-sigstore --infra",
    )
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
        "identity-foundation --infra",
    )
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

#[cfg(target_os = "linux")]
fn run_infrastructure(
    environment: &str,
    playbook_name: &str,
    _command: &'static str,
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
        sigstore_backup_locator,
        sigstore_secret_locator,
        sigstore_tuf_root_locator,
        deployment_manifest_hash,
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
        .add_env("ANSIBLE_COLLECTIONS_PATH", collections_path)
        .add_env("ANSIBLE_ROLES_PATH", roles_path)
        .add_env("ANSIBLE_AUTO_INSTALL", "false")
        .add_env("ANSIBLE_NOCOWS", "1")
        .add_env("ANSIBLE_VAULT_PASSWORD_FILE", vault_password)
        .add_env("LABWEAVER_RUN_ID", run_id)
        .add_env("LABWEAVER_COMMIT_SHA", commit_sha)
        .add_env("LABWEAVER_CONTROLLER_ID", controller_id)
        .add_env("LABWEAVER_INVENTORY_HASH", inventory_hash)
        .add_env("LABWEAVER_COMPONENT_LOCK_HASH", component_lock_hash)
        .add_env(
            "LABWEAVER_HARBOR_DATA_BACKUP_LOCATOR",
            harbor_data_backup_locator,
        )
        .add_env("LABWEAVER_TESTFLIGHT_RUN_ID", testflight_run_id)
        .add_env("LABWEAVER_SIGSTORE_BACKUP_LOCATOR", sigstore_backup_locator)
        .add_env("LABWEAVER_SIGSTORE_SECRET_LOCATOR", sigstore_secret_locator)
        .add_env(
            "LABWEAVER_SIGSTORE_TUF_ROOT_LOCATOR",
            sigstore_tuf_root_locator,
        )
        .add_env(
            "LABWEAVER_DEPLOYMENT_MANIFEST_HASH",
            deployment_manifest_hash,
        )
        .add_env("LABWEAVER_IDENTITY_SECRET_LOCATOR", identity_secret_locator)
        .set_inventory(&inventory);
    // ansible-rs 1.1.0 appends configured arguments twice in `run`; all
    // controller identity and vault inputs therefore travel through the
    // explicit environment contract above.
    runner
        .run(Play::from_file(playbook))
        .map(|_| ())
        .map_err(|_| AppError::ExternalCommand {
            role: "allowlisted infrastructure playbook",
            code: None,
            detail: Some(
                "ansible-rs returned a non-zero result; inspect the controller event log".into(),
            ),
        })
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
    sigstore_backup_locator: String,
    sigstore_secret_locator: String,
    sigstore_tuf_root_locator: String,
    deployment_manifest_hash: String,
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
        let collections_path = resolve_infrastructure_directory(
            "approved Ansible collections",
            [controller_root.as_path(), shared_controller_root.as_path()],
            "collections",
        )?;
        let roles_path = resolve_infrastructure_directory(
            "approved Ansible roles",
            [controller_root.as_path(), shared_controller_root.as_path()],
            "roles",
        )?;

        let PlaybookLocators {
            sigstore_backup_locator,
            sigstore_secret_locator,
            sigstore_tuf_root_locator,
            deployment_manifest_hash,
            identity_secret_locator,
        } = PlaybookLocators::load(playbook_name)?;

        Ok(Self {
            inventory: infrastructure_path(&inventory),
            vault_password: infrastructure_path(&vault_password),
            playbook: infrastructure_path(&playbook),
            ansible_config: infrastructure_path(&ansible_config),
            collections_path: infrastructure_path(&collections_path),
            roles_path: infrastructure_path(&roles_path),
            commit_sha: infrastructure_commit_sha()?,
            controller_id: approved_controller_identity(&controller_lock)?,
            inventory_hash: inventory_identity_hash(&inventory_root)?,
            component_lock_hash: file_sha256(&root.join("deploy/versions.lock.yml"))?,
            harbor_data_backup_locator: std::env::var("LABWEAVER_HARBOR_DATA_BACKUP_LOCATOR")
                .unwrap_or_default(),
            sigstore_backup_locator,
            sigstore_secret_locator,
            sigstore_tuf_root_locator,
            deployment_manifest_hash,
            identity_secret_locator,
        })
    }
}

#[cfg(target_os = "linux")]
struct PlaybookLocators {
    sigstore_backup_locator: String,
    sigstore_secret_locator: String,
    sigstore_tuf_root_locator: String,
    deployment_manifest_hash: String,
    identity_secret_locator: String,
}

#[cfg(target_os = "linux")]
impl PlaybookLocators {
    fn load(playbook_name: &str) -> Result<Self, AppError> {
        let private_sigstore = matches!(
            playbook_name,
            "96-private-sigstore.yml"
                | "97-private-sigstore-backup.yml"
                | "98-private-sigstore-restore.yml"
                | "99-private-sigstore-rotate.yml"
                | "100-private-sigstore-verify.yml"
                | "101-private-sigstore-cleanup.yml"
                | "102-private-sigstore-disaster-recovery.yml"
        );
        let identity_foundation = matches!(
            playbook_name,
            "91-identity-foundation.yml" | "92-identity-foundation-verify.yml"
        );
        let deployment_manifest_hash = deployment_manifest_hash(private_sigstore)?;
        Ok(Self {
            sigstore_backup_locator: locator(
                "LABWEAVER_SIGSTORE_BACKUP_LOCATOR",
                "Private Sigstore backup locator",
                private_sigstore,
            )?,
            sigstore_secret_locator: locator(
                "LABWEAVER_SIGSTORE_SECRET_LOCATOR",
                "Private Sigstore secret locator",
                private_sigstore,
            )?,
            sigstore_tuf_root_locator: locator(
                "LABWEAVER_SIGSTORE_TUF_ROOT_LOCATOR",
                "Private Sigstore TUF root locator",
                private_sigstore,
            )?,
            deployment_manifest_hash,
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
fn deployment_manifest_hash(required: bool) -> Result<String, AppError> {
    let value = locator(
        "LABWEAVER_DEPLOYMENT_MANIFEST_HASH",
        "deployment manifest identity",
        required,
    )?;
    if !required || is_sha256_identity(&value) {
        return Ok(value);
    }
    Err(AppError::ExternalCommand {
        role: "deployment manifest identity",
        code: None,
        detail: Some("LABWEAVER_DEPLOYMENT_MANIFEST_HASH must be sha256:<64 lowercase hex>".into()),
    })
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
fn is_sha256_identity(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
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
    let valid = (8..=96).contains(&value.len())
        && value.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
    if valid {
        Ok(value)
    } else {
        Err(AppError::ExternalCommand {
            role,
            code: None,
            detail: Some(format!(
                "{variable} must be an explicit lowercase run identifier"
            )),
        })
    }
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
    use super::{
        EnvironmentArgs, IdentityFoundationAction, IdentityFoundationArgs, PrivateSigstoreAction,
        PrivateSigstoreArgs, deploy, identity_foundation, private_sigstore,
    };

    fn sigstore_args(env: &str, infra: bool, yes: bool) -> PrivateSigstoreArgs {
        PrivateSigstoreArgs {
            environment: EnvironmentArgs {
                env: env.into(),
                infra,
                yes,
            },
            action: PrivateSigstoreAction::Deploy,
        }
    }

    fn identity_args(env: &str, infra: bool, yes: bool) -> IdentityFoundationArgs {
        IdentityFoundationArgs {
            environment: EnvironmentArgs {
                env: env.into(),
                infra,
                yes,
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

    #[test]
    fn private_sigstore_requires_confirmation_and_infra_boundary() -> Result<(), String> {
        let Err(unconfirmed) = private_sigstore(&sigstore_args("dev", true, false)) else {
            return Err("Private Sigstore deployment without --yes must fail".into());
        };
        if unconfirmed.diagnostic_code() != "XTASK_CONFIRMATION_REQUIRED" {
            return Err("unexpected Private Sigstore confirmation diagnostic".into());
        }

        let Err(product_path) = private_sigstore(&sigstore_args("dev", false, true)) else {
            return Err("Private Sigstore must not run through the product path".into());
        };
        if product_path.diagnostic_code() != "XTASK_NOT_IMPLEMENTED" {
            return Err("unexpected Private Sigstore product-path diagnostic".into());
        }
        Ok(())
    }

    #[test]
    fn private_sigstore_rejects_path_traversal_before_platform_dispatch() -> Result<(), String> {
        let Err(error) = private_sigstore(&sigstore_args("../../private", true, true)) else {
            return Err("an environment path traversal must fail".into());
        };
        if error.diagnostic_code() != "XTASK_INVALID_ARGUMENT" {
            return Err("unexpected environment traversal diagnostic".into());
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

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn private_sigstore_has_a_stable_non_linux_diagnostic() -> Result<(), String> {
        let Err(error) = private_sigstore(&sigstore_args("dev", true, true)) else {
            return Err("non-Linux Private Sigstore deployment must fail".into());
        };
        if error.diagnostic_code() != "XTASK_INFRA_UNSUPPORTED_PLATFORM" {
            return Err("unexpected Private Sigstore non-Linux diagnostic".into());
        }
        Ok(())
    }

    #[test]
    fn private_sigstore_actions_are_fixed_playbooks() {
        let mappings = [
            (PrivateSigstoreAction::Deploy, "96-private-sigstore.yml"),
            (
                PrivateSigstoreAction::Backup,
                "97-private-sigstore-backup.yml",
            ),
            (
                PrivateSigstoreAction::Restore,
                "98-private-sigstore-restore.yml",
            ),
            (
                PrivateSigstoreAction::Rotate,
                "99-private-sigstore-rotate.yml",
            ),
            (
                PrivateSigstoreAction::Verify,
                "100-private-sigstore-verify.yml",
            ),
            (
                PrivateSigstoreAction::Cleanup,
                "101-private-sigstore-cleanup.yml",
            ),
            (
                PrivateSigstoreAction::DisasterRecovery,
                "102-private-sigstore-disaster-recovery.yml",
            ),
        ];
        for (action, expected) in mappings {
            assert_eq!(action.playbook(), expected);
        }
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
}
