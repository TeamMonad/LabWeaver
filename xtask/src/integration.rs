//! Fast, fail-closed local integration gates.
//!
//! This module owns only local process/container lifecycle. Product services
//! remain the binaries and test suites already owned by their crates.

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::fs;
use std::future::Future;
use std::io::{ErrorKind, Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::thread;
use std::time::{Duration, Instant};

use async_nats::jetstream;
use bollard::Docker;
use bollard::container::LogOutput;
use bollard::errors::Error as DockerApiError;
use bollard::exec::StartExecResults;
use bollard::models::{
    ContainerCreateBody, EndpointSettings, ExecConfig, HostConfig, NetworkCreateRequest,
    NetworkingConfig, PortBinding,
};
use bollard::query_parameters::{
    CreateContainerOptionsBuilder, CreateImageOptionsBuilder, InspectContainerOptions,
    LogsOptionsBuilder, RemoveContainerOptionsBuilder, StartContainerOptions,
};
use futures_util::{StreamExt, TryStreamExt};
use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

use super::{AppError, IntegrationScope};

const READINESS_TIMEOUT: Duration = Duration::from_secs(90);
const COMMAND_OUTPUT_LIMIT: usize = 4096;
const LOG_CAPTURE_LIMIT: usize = 1_048_576;

#[derive(Debug, Serialize)]
struct RunReport {
    format: &'static str,
    run_id: String,
    source_commit: String,
    dirty: bool,
    scope: &'static str,
    base_ref: Option<String>,
    selected_paths: Vec<String>,
    docker_selected: bool,
    kind_selected: bool,
    images: BTreeMap<String, String>,
    phases: Vec<PhaseReport>,
    total_duration_milliseconds: u128,
    slowest_phase: Option<String>,
    result: &'static str,
    diagnostic: Option<String>,
    cleanup: CleanupReport,
}

#[derive(Debug, Serialize)]
struct PhaseReport {
    name: String,
    duration_milliseconds: u128,
    result: &'static str,
}

#[derive(Debug, Serialize)]
struct CleanupReport {
    attempted: bool,
    result: &'static str,
}

impl Default for CleanupReport {
    fn default() -> Self {
        Self {
            attempted: false,
            result: "not-run",
        }
    }
}

#[derive(Debug)]
struct Selection {
    selected_paths: Vec<String>,
    docker: bool,
    kind: bool,
}

#[derive(Debug, Clone)]
struct ContainerDefinition {
    role: &'static str,
    name: String,
    image: String,
    env_file: Option<PathBuf>,
    command: Vec<String>,
    port: u16,
}

#[derive(Debug)]
struct DockerSession {
    root: PathBuf,
    run_id: String,
    docker: Docker,
    network: String,
    containers: Vec<String>,
    env_dir: PathBuf,
    ports: BTreeMap<&'static str, u16>,
}

#[derive(Debug)]
struct KindSession {
    root: PathBuf,
    name: String,
    kubeconfig: PathBuf,
}

pub(crate) fn run(
    root: &Path,
    scope: IntegrationScope,
    base_ref: Option<&str>,
    include_kind: bool,
    kind_only: bool,
) -> Result<(), AppError> {
    let run_started = Instant::now();
    let run_id = Uuid::now_v7().simple().to_string();
    let report_dir = root.join("artifacts/local-integration").join(&run_id);
    fs::create_dir_all(&report_dir)
        .map_err(|error| integration_io("create report directory", error))?;

    let source_commit = git_checked(root, &["rev-parse", "HEAD"], "read source commit")?;
    let dirty = !git_checked(root, &["status", "--porcelain"], "read dirty state")?.is_empty();
    let base = base_ref.map(str::to_owned);
    let selection = match select(scope, base_ref, include_kind, kind_only, root) {
        Ok(selection) => selection,
        Err(error) => {
            let report = RunReport {
                format: "labweaver.local-integration.v1",
                run_id,
                source_commit,
                dirty,
                scope: scope_name(scope),
                base_ref: base,
                selected_paths: Vec::new(),
                docker_selected: false,
                kind_selected: false,
                images: BTreeMap::new(),
                phases: Vec::new(),
                total_duration_milliseconds: run_started.elapsed().as_millis(),
                slowest_phase: None,
                result: "failed",
                diagnostic: Some(format!("{}: {}", error.diagnostic_code(), error)),
                cleanup: CleanupReport::default(),
            };
            write_report(&report_dir, &report)?;
            return Err(error);
        }
    };

    let mut report = RunReport {
        format: "labweaver.local-integration.v1",
        run_id: run_id.clone(),
        source_commit,
        dirty,
        scope: scope_name(scope),
        base_ref: base,
        selected_paths: selection.selected_paths.clone(),
        docker_selected: selection.docker,
        kind_selected: selection.kind,
        images: BTreeMap::new(),
        phases: Vec::new(),
        total_duration_milliseconds: 0,
        slowest_phase: None,
        result: "running",
        diagnostic: None,
        cleanup: CleanupReport::default(),
    };

    let result = run_selected(root, &run_id, &selection, &report_dir, &mut report);
    report.result = if result.is_ok() { "passed" } else { "failed" };
    if let Err(error) = &result {
        report.diagnostic = Some(format!("{}: {}", error.diagnostic_code(), error));
    }
    report.total_duration_milliseconds = run_started.elapsed().as_millis();
    report.slowest_phase = report
        .phases
        .iter()
        .max_by_key(|phase| phase.duration_milliseconds)
        .map(|phase| phase.name.clone());
    write_report(&report_dir, &report)?;
    result
}

fn run_selected(
    root: &Path,
    run_id: &str,
    selection: &Selection,
    report_dir: &Path,
    report: &mut RunReport,
) -> Result<(), AppError> {
    if selection.docker {
        report.cleanup.attempted = true;
        let started = Instant::now();
        let session = DockerSession::start(root, run_id, report_dir, report);
        let mut session = match session {
            Ok(session) => session,
            Err(error) => {
                if report.cleanup.result == "not-run" {
                    report.cleanup.result = "failed";
                }
                push_phase(report, "docker", started, false);
                return Err(error);
            }
        };
        let result = run_docker_gate(&mut session, report_dir, report);
        if result.is_err() {
            write_container_logs(&session.docker, &session.containers, report_dir);
        }
        let cleanup_result = session.cleanup();
        report.cleanup.result = if cleanup_result.is_ok() {
            "passed"
        } else {
            "failed"
        };
        if let Err(error) = cleanup_result {
            if result.is_ok() {
                return Err(error);
            }
            eprintln!("[LW_INTEGRATION_CLEANUP_FAILED] {error}");
        }
        push_phase(report, "docker", started, result.is_ok());
        result?;
    }
    if selection.kind {
        report.cleanup.attempted = true;
        let started = Instant::now();
        let result = run_kind_gate(root, run_id, report_dir, report);
        push_phase(report, "kind", started, result.is_ok());
        result?;
    }
    Ok(())
}

fn select(
    scope: IntegrationScope,
    base_ref: Option<&str>,
    include_kind: bool,
    kind_only: bool,
    root: &Path,
) -> Result<Selection, AppError> {
    match scope {
        IntegrationScope::Candidate if kind_only => Ok(Selection {
            selected_paths: vec!["<kind-only>".to_owned()],
            docker: false,
            kind: true,
        }),
        IntegrationScope::Candidate => Ok(Selection {
            selected_paths: vec!["<candidate>".to_owned()],
            docker: true,
            kind: include_kind,
        }),
        IntegrationScope::Changed => {
            let base_ref = base_ref.ok_or_else(|| {
                integration_error(
                    "LW_INTEGRATION_BASE_REF_REQUIRED",
                    "--scope changed requires --base-ref",
                )
            })?;
            if git_checked(
                root,
                &["rev-parse", "--verify", base_ref],
                "verify integration base ref",
            )
            .is_err()
            {
                return Err(integration_error(
                    "LW_INTEGRATION_BASE_REF_INVALID",
                    format!("base ref {base_ref} does not resolve"),
                ));
            }
            let mut paths = git_names(
                root,
                &[
                    "diff",
                    "--name-only",
                    "--diff-filter=ACDMRTUXB",
                    base_ref,
                    "HEAD",
                ],
            )?;
            paths.extend(git_names(
                root,
                &["diff", "--name-only", "--diff-filter=ACDMRTUXB"],
            )?);
            paths.extend(git_names(
                root,
                &["ls-files", "--others", "--exclude-standard"],
            )?);
            paths.sort_unstable();
            paths.dedup();
            if paths.is_empty() {
                return Err(integration_error(
                    "LW_INTEGRATION_NO_CHANGES",
                    "changed scope found no tracked or untracked changes",
                ));
            }
            let docker = !kind_only && paths.iter().any(|path| is_docker_path(path));
            let kind = include_kind || paths.iter().any(|path| is_kind_path(path));
            if !docker && !kind {
                return Err(integration_error(
                    "LW_INTEGRATION_SCOPE_UNSELECTED",
                    "changed files do not map to a local integration gate",
                ));
            }
            Ok(Selection {
                selected_paths: paths,
                docker,
                kind,
            })
        }
    }
}

fn is_docker_path(path: &str) -> bool {
    path.starts_with("services/")
        || path.starts_with("crates/")
        || path.starts_with("migrations/")
        || path.starts_with("containers/")
        || path.starts_with("deploy/versions.lock.yml")
        || path.starts_with("xtask/")
        || matches!(path, "Cargo.toml" | "Cargo.lock")
}

fn is_kind_path(path: &str) -> bool {
    path.starts_with("deploy/helm/")
        || path.starts_with("deploy/ansible/")
        || path.starts_with("deploy/config/")
        || path.starts_with("services/environment-service/")
        || path.starts_with("services/resource-service/")
        || path.starts_with("crates/contracts/")
        || path.starts_with("schemas/")
        || path.starts_with("migrations/")
        || path.starts_with("xtask/src/integration.rs")
        || path.starts_with(".github/workflows/local-integration.yml")
}

fn scope_name(scope: IntegrationScope) -> &'static str {
    match scope {
        IntegrationScope::Changed => "changed",
        IntegrationScope::Candidate => "candidate",
    }
}

impl DockerSession {
    #[allow(
        clippy::too_many_lines,
        reason = "startup keeps all local dependency identity and cleanup inputs together"
    )]
    fn start(
        root: &Path,
        run_id: &str,
        report_dir: &Path,
        report: &mut RunReport,
    ) -> Result<Self, AppError> {
        let images = load_images(root)?;
        report.images.extend(images.clone());
        require_tool(
            root,
            "docker",
            &["version", "--format", "{{.Server.Version}}"],
        )?;
        require_tool(root, "docker", &["buildx", "version"])?;
        let docker = Docker::connect_with_local_defaults().map_err(|error| {
            integration_error("LW_INTEGRATION_DOCKER_API_UNAVAILABLE", error.to_string())
        })?;

        let network = format!("labweaver-local-{run_id}");
        create_docker_network(&docker, &network, run_id)?;

        let env_dir = std::env::temp_dir().join(format!("labweaver-local-{run_id}"));
        if let Err(error) = fs::create_dir_all(&env_dir) {
            report.cleanup.attempted = true;
            report.cleanup.result = if cleanup_partial(&docker, &network, &[], &env_dir) {
                "passed"
            } else {
                "failed"
            };
            return Err(integration_io("create local private directory", error));
        }
        let env_file = env_dir.join("dependency.env");
        let admin_password = format!("lw-{}", Uuid::now_v7().simple());
        if let Err(error) = fs::write(
            &env_file,
            format!(
                "POSTGRES_PASSWORD=labweaver-test\nPOSTGRES_USER=labweaver\nPOSTGRES_DB=labweaver\nKC_BOOTSTRAP_ADMIN_USERNAME=admin\nKC_BOOTSTRAP_ADMIN_PASSWORD={admin_password}\n"
            ),
        ) {
            report.cleanup.attempted = true;
            report.cleanup.result = if cleanup_partial(&docker, &network, &[], &env_dir) {
                "passed"
            } else {
                "failed"
            };
            return Err(integration_io("write local dependency environment", error));
        }

        let definitions = vec![
            ContainerDefinition {
                role: "postgres",
                name: format!("labweaver-local-{run_id}-postgres"),
                image: images["postgres"].clone(),
                env_file: Some(env_file.clone()),
                command: Vec::new(),
                port: 5432,
            },
            ContainerDefinition {
                role: "nats",
                name: format!("labweaver-local-{run_id}-nats"),
                image: images["nats"].clone(),
                env_file: None,
                command: vec!["-js".into()],
                port: 4222,
            },
            ContainerDefinition {
                role: "minio",
                name: format!("labweaver-local-{run_id}-minio"),
                image: images["minio"].clone(),
                env_file: None,
                command: vec![
                    "server".into(),
                    "/data".into(),
                    "--console-address".into(),
                    ":9001".into(),
                ],
                port: 9000,
            },
            ContainerDefinition {
                role: "keycloak",
                name: format!("labweaver-local-{run_id}-keycloak"),
                image: images["keycloak"].clone(),
                env_file: Some(env_file),
                command: vec!["start-dev".into(), "--http-port=8080".into()],
                port: 8080,
            },
            ContainerDefinition {
                role: "registry",
                name: format!("labweaver-local-{run_id}-registry"),
                image: images["registry"].clone(),
                env_file: None,
                command: Vec::new(),
                port: 5000,
            },
        ];

        let mut results = thread::scope(|scope| {
            definitions
                .iter()
                .map(|definition| {
                    let network = network.clone();
                    let docker = docker.clone();
                    scope.spawn(move || start_container(&docker, &network, definition))
                })
                .collect::<Vec<_>>()
                .into_iter()
                .map(|handle| {
                    handle.join().map_err(|_| {
                        integration_error(
                            "LW_INTEGRATION_CONTAINER_PANIC",
                            "container startup worker panicked",
                        )
                    })?
                })
                .collect::<Vec<Result<String, AppError>>>()
        });
        let mut containers = Vec::new();
        let mut first_error = None;
        for result in results.drain(..) {
            match result {
                Ok(container) => containers.push(container),
                Err(error) => {
                    first_error.get_or_insert(error);
                }
            }
        }
        if let Some(error) = first_error {
            write_container_logs(&docker, &containers, report_dir);
            report.cleanup.attempted = true;
            report.cleanup.result = if cleanup_partial(&docker, &network, &containers, &env_dir) {
                "passed"
            } else {
                "failed"
            };
            return Err(error);
        }

        let startup = (|| {
            let mut ports = BTreeMap::new();
            for definition in &definitions {
                let port =
                    container_port(&docker, &definition.name, definition.port, definition.role)?;
                wait_for_port(port, definition.role)?;
                verify_image_identity(&docker, &definition.image, definition.role)?;
                ports.insert(definition.role, port);
            }
            wait_for_postgres(&docker, &definitions[0].name)?;
            Ok(Self {
                root: root.to_owned(),
                run_id: run_id.to_owned(),
                docker: docker.clone(),
                network: network.clone(),
                containers: containers.clone(),
                env_dir: env_dir.clone(),
                ports,
            })
        })();
        match startup {
            Ok(session) => Ok(session),
            Err(error) => {
                write_container_logs(&docker, &containers, report_dir);
                report.cleanup.attempted = true;
                report.cleanup.result = if cleanup_partial(&docker, &network, &containers, &env_dir)
                {
                    "passed"
                } else {
                    "failed"
                };
                Err(error)
            }
        }
    }

    fn cleanup(&self) -> Result<(), AppError> {
        let mut first_error = None;
        for container in self.containers.iter().rev() {
            if let Err(error) = remove_docker_container(&self.docker, container) {
                first_error.get_or_insert(error);
            }
        }
        if let Err(error) = remove_docker_network(&self.docker, &self.network) {
            first_error.get_or_insert(error);
        }
        if let Err(error) = fs::remove_dir_all(&self.env_dir)
            && error.kind() != ErrorKind::NotFound
        {
            first_error.get_or_insert(integration_io("remove local private directory", error));
        }
        first_error.map_or(Ok(()), Err)
    }
}

fn cleanup_partial(docker: &Docker, network: &str, containers: &[String], env_dir: &Path) -> bool {
    let mut success = true;
    for container in containers.iter().rev() {
        if remove_docker_container(docker, container).is_err() {
            success = false;
        }
    }
    if remove_docker_network(docker, network).is_err() {
        success = false;
    }
    if fs::remove_dir_all(env_dir).is_err_and(|error| error.kind() != ErrorKind::NotFound) {
        success = false;
    }
    success
}

fn write_container_logs(docker: &Docker, containers: &[String], report_dir: &Path) {
    let log_dir = report_dir.join("container-logs");
    if let Err(error) = fs::create_dir_all(&log_dir) {
        eprintln!("[LW_INTEGRATION_LOG_CAPTURE_FAILED] create log directory: {error}");
        return;
    }
    for container in containers {
        let output = docker_logs(docker, container);
        let content = output.unwrap_or_else(|error| format!("log capture failed: {error}\n"));
        let path = log_dir.join(format!("{container}.log"));
        if let Err(error) = fs::write(path, content) {
            eprintln!("[LW_INTEGRATION_LOG_CAPTURE_FAILED] write container log: {error}");
        }
    }
}

#[allow(
    clippy::zero_sized_map_values,
    reason = "the Docker API models the exposed port set as a map keyed by port"
)]
fn start_container(
    docker: &Docker,
    network: &str,
    definition: &ContainerDefinition,
) -> Result<String, AppError> {
    let env = definition
        .env_file
        .as_ref()
        .map(|path| read_container_env(path))
        .transpose()?;
    pull_docker_image(docker, &definition.image)?;
    let port_key = format!("{}/tcp", definition.port);
    let mut exposed_ports = HashMap::new();
    exposed_ports.insert(port_key.clone(), HashMap::new());
    let mut port_bindings = HashMap::new();
    port_bindings.insert(
        port_key,
        Some(vec![PortBinding {
            host_ip: Some("127.0.0.1".to_owned()),
            host_port: Some("0".to_owned()),
        }]),
    );
    let mut endpoints = HashMap::new();
    endpoints.insert(
        network.to_owned(),
        EndpointSettings {
            aliases: Some(vec![definition.role.to_owned()]),
            ..Default::default()
        },
    );
    let mut labels = HashMap::new();
    labels.insert(
        "com.labweaver.integration.role".to_owned(),
        definition.role.to_owned(),
    );
    let body = ContainerCreateBody {
        image: Some(definition.image.clone()),
        cmd: (!definition.command.is_empty()).then(|| definition.command.clone()),
        env,
        labels: Some(labels),
        exposed_ports: Some(exposed_ports),
        host_config: Some(HostConfig {
            network_mode: Some(network.to_owned()),
            port_bindings: Some(port_bindings),
            ..Default::default()
        }),
        networking_config: Some(NetworkingConfig {
            endpoints_config: Some(endpoints),
        }),
        ..Default::default()
    };
    let options = CreateContainerOptionsBuilder::default()
        .name(&definition.name)
        .build();
    let response = docker_api(docker.create_container(Some(options), body))?;
    docker_api(docker.start_container(&response.id, None::<StartContainerOptions>))?;
    Ok(definition.name.clone())
}

fn docker_runtime() -> Result<tokio::runtime::Runtime, AppError> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| integration_error("LW_INTEGRATION_RUNTIME_FAILED", error.to_string()))
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "bollard call sites transfer error ownership into this single diagnostic boundary"
)]
fn docker_api_error(error: DockerApiError) -> AppError {
    integration_error("LW_INTEGRATION_DOCKER_API_FAILED", error.to_string())
}

fn docker_api<T>(future: impl Future<Output = Result<T, DockerApiError>>) -> Result<T, AppError> {
    docker_runtime()?.block_on(future).map_err(docker_api_error)
}

fn docker_missing(error: &DockerApiError) -> bool {
    matches!(
        error,
        DockerApiError::DockerResponseServerError {
            status_code: 404,
            ..
        }
    )
}

fn create_docker_network(docker: &Docker, network: &str, run_id: &str) -> Result<(), AppError> {
    let mut labels = HashMap::new();
    labels.insert(
        "com.labweaver.integration.run".to_owned(),
        run_id.to_owned(),
    );
    docker_api(docker.create_network(NetworkCreateRequest {
        name: network.to_owned(),
        labels: Some(labels),
        ..Default::default()
    }))
    .map(|_| ())
}

fn pull_docker_image(docker: &Docker, image: &str) -> Result<(), AppError> {
    let options = CreateImageOptionsBuilder::default()
        .from_image(image)
        .build();
    docker_runtime()?
        .block_on(
            docker
                .create_image(Some(options), None, None)
                .try_collect::<Vec<_>>(),
        )
        .map_err(docker_api_error)?;
    Ok(())
}

fn read_container_env(path: &Path) -> Result<Vec<String>, AppError> {
    let content = fs::read_to_string(path)
        .map_err(|error| integration_io("read dependency environment", error))?;
    Ok(content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_owned)
        .collect())
}

fn remove_docker_container(docker: &Docker, container: &str) -> Result<(), AppError> {
    let options = RemoveContainerOptionsBuilder::default()
        .force(true)
        .v(true)
        .build();
    match docker_runtime()?.block_on(docker.remove_container(container, Some(options))) {
        Ok(()) => Ok(()),
        Err(error) if docker_missing(&error) => Ok(()),
        Err(error) => Err(docker_api_error(error)),
    }
}

fn remove_docker_network(docker: &Docker, network: &str) -> Result<(), AppError> {
    match docker_runtime()?.block_on(docker.remove_network(network)) {
        Ok(()) => Ok(()),
        Err(error) if docker_missing(&error) => Ok(()),
        Err(error) => Err(docker_api_error(error)),
    }
}

fn docker_logs(docker: &Docker, container: &str) -> Result<String, AppError> {
    let options = LogsOptionsBuilder::default()
        .stdout(true)
        .stderr(true)
        .build();
    let frames = docker_runtime()?
        .block_on(
            docker
                .logs(container, Some(options))
                .try_collect::<Vec<LogOutput>>(),
        )
        .map_err(docker_api_error)?;
    let mut content = String::new();
    for frame in &frames {
        content.push_str(&String::from_utf8_lossy(frame.as_ref()));
        if content.len() > LOG_CAPTURE_LIMIT {
            content.push_str("\n[truncated]\n");
            break;
        }
    }
    Ok(content)
}

fn run_docker_gate(
    session: &mut DockerSession,
    report_dir: &Path,
    report: &mut RunReport,
) -> Result<(), AppError> {
    let started = Instant::now();
    run_host_contract_probe(&session.root)?;
    push_phase(report, "host-contract-probe", started, true);

    let started = Instant::now();
    run_dependency_probes(session)?;
    push_phase(report, "dependency-contract-probe", started, true);

    let started = Instant::now();
    run_build_supply_chain(session, report_dir, report)?;
    push_phase(report, "build-supply-chain", started, true);
    Ok(())
}

fn run_dependency_probes(session: &DockerSession) -> Result<(), AppError> {
    http_probe_wait(
        session.ports["minio"],
        "/minio/health/ready",
        "MinIO readiness",
        None,
    )?;
    http_probe_wait(
        session.ports["keycloak"],
        "/realms/master/.well-known/openid-configuration",
        "Keycloak OIDC discovery",
        Some("\"issuer\""),
    )?;
    http_probe_wait(
        session.ports["registry"],
        "/v2/",
        "local registry API",
        None,
    )?;

    let suffix = &session.run_id[..12];
    let stream = format!("LW_LOCAL_{suffix}");
    let subject = format!("labweaver.local.{suffix}");
    run_jetstream_probe(
        session.ports["nats"],
        &stream,
        &format!("{subject}.>"),
        &format!("{subject}.probe"),
    )?;
    Ok(())
}

fn run_jetstream_probe(
    port: u16,
    stream: &str,
    stream_subject: &str,
    subject: &str,
) -> Result<(), AppError> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| integration_error("LW_INTEGRATION_RUNTIME_FAILED", error.to_string()))?;
    runtime.block_on(async {
        let client = async_nats::connect(format!("nats://127.0.0.1:{port}"))
            .await
            .map_err(|error| {
                integration_error("LW_INTEGRATION_NATS_CONNECT_FAILED", error.to_string())
            })?;
        let context = jetstream::new(client.clone());
        context
            .create_stream(jetstream::stream::Config {
                name: stream.to_owned(),
                subjects: vec![stream_subject.to_owned()],
                ..Default::default()
            })
            .await
            .map_err(|error| {
                integration_error("LW_INTEGRATION_JETSTREAM_CREATE_FAILED", error.to_string())
            })?;
        context
            .publish(subject.to_owned(), "local-integration".into())
            .await
            .map_err(|error| {
                integration_error("LW_INTEGRATION_JETSTREAM_PUBLISH_FAILED", error.to_string())
            })?
            .await
            .map_err(|error| {
                integration_error("LW_INTEGRATION_JETSTREAM_ACK_FAILED", error.to_string())
            })?;
        let mut stream_handle = context.get_stream(stream).await.map_err(|error| {
            integration_error("LW_INTEGRATION_JETSTREAM_READ_FAILED", error.to_string())
        })?;
        let info = stream_handle.info().await.map_err(|error| {
            integration_error("LW_INTEGRATION_JETSTREAM_READ_FAILED", error.to_string())
        })?;
        if info.state.messages != 1 {
            return Err(integration_error(
                "LW_INTEGRATION_JETSTREAM_CONTRACT_MISMATCH",
                format!(
                    "expected one persisted message, got {}",
                    info.state.messages
                ),
            ));
        }
        Ok::<(), AppError>(())
    })
}

fn http_probe_wait(
    port: u16,
    path: &str,
    role: &'static str,
    expected_body: Option<&str>,
) -> Result<(), AppError> {
    let deadline = Instant::now() + READINESS_TIMEOUT;
    loop {
        let error = match http_probe(port, path, role, expected_body) {
            Ok(()) => return Ok(()),
            Err(error) => error,
        };
        if Instant::now() >= deadline {
            return Err(error);
        }
        thread::sleep(Duration::from_millis(500));
    }
}

fn http_probe(
    port: u16,
    path: &str,
    role: &'static str,
    expected_body: Option<&str>,
) -> Result<(), AppError> {
    let mut stream = TcpStream::connect(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port))
        .map_err(|error| {
            integration_error(
                "LW_INTEGRATION_HTTP_PROBE_FAILED",
                format!("{role}: {error}"),
            )
        })?;
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .map_err(|error| {
            integration_error(
                "LW_INTEGRATION_HTTP_PROBE_FAILED",
                format!("{role}: {error}"),
            )
        })?;
    let request = format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).map_err(|error| {
        integration_error(
            "LW_INTEGRATION_HTTP_PROBE_FAILED",
            format!("{role}: {error}"),
        )
    })?;
    let mut response = String::new();
    stream.read_to_string(&mut response).map_err(|error| {
        integration_error(
            "LW_INTEGRATION_HTTP_PROBE_FAILED",
            format!("{role}: {error}"),
        )
    })?;
    if !response.starts_with("HTTP/1.1 200") && !response.starts_with("HTTP/1.0 200") {
        return Err(integration_error(
            "LW_INTEGRATION_HTTP_PROBE_UNHEALTHY",
            format!("{role} returned a non-success status"),
        ));
    }
    if let Some(expected_body) = expected_body
        && !response.contains(expected_body)
    {
        return Err(integration_error(
            "LW_INTEGRATION_HTTP_PROBE_CONTRACT_MISMATCH",
            format!("{role} response omitted {expected_body}"),
        ));
    }
    Ok(())
}

fn run_host_contract_probe(root: &Path) -> Result<(), AppError> {
    run_checked(
        root,
        "cargo",
        &["test", "-p", "contracts", "--locked"],
        "run host contract tests",
    )?;
    run_checked(
        root,
        "cargo",
        &[
            "test",
            "-p",
            "persistence-sqlx",
            "--test",
            "postgres_integration",
            "--locked",
        ],
        "run host persistence integration tests",
    )?;
    Ok(())
}

#[allow(
    clippy::too_many_lines,
    reason = "the canary keeps build, push, scan and cleanup identity together"
)]
fn run_build_supply_chain(
    session: &DockerSession,
    report_dir: &Path,
    report: &RunReport,
) -> Result<(), AppError> {
    let builder = format!("labweaver-local-{}-builder", session.run_id);
    let context = report_dir.join("canary-context");
    fs::create_dir_all(&context).map_err(|error| integration_io("create canary context", error))?;
    fs::write(
        context.join("Containerfile"),
        "FROM scratch\nCOPY message /message\n",
    )
    .map_err(|error| integration_io("write canary Containerfile", error))?;
    fs::write(context.join("message"), "labweaver-local-canary\n")
        .map_err(|error| integration_io("write canary message", error))?;

    let local_image = format!("labweaver-local-canary:{}", session.run_id);
    let registry_image = format!(
        "127.0.0.1:{}/labweaver/canary:{}",
        session.ports["registry"], session.run_id
    );
    let result = (|| {
        docker_checked(
            &session.root,
            vec![
                "buildx".into(),
                "create".into(),
                "--name".into(),
                builder.clone(),
                "--driver".into(),
                "docker-container".into(),
                "--driver-opt".into(),
                format!("image={}", report.images["buildkit"]),
                "--use".into(),
            ],
        )?;
        docker_checked(
            &session.root,
            vec![
                "buildx".into(),
                "build".into(),
                "--builder".into(),
                builder.clone(),
                "--file".into(),
                context.join("Containerfile").to_string_lossy().into_owned(),
                "--tag".into(),
                local_image.clone(),
                "--load".into(),
                "--provenance=false".into(),
                context.to_string_lossy().into_owned(),
            ],
        )?;
        docker_checked(
            &session.root,
            vec!["tag".into(), local_image.clone(), registry_image.clone()],
        )?;
        docker_checked(&session.root, vec!["push".into(), registry_image])?;

        let archive = report_dir.join("canary.tar");
        docker_checked(
            &session.root,
            vec![
                "save".into(),
                "--output".into(),
                archive.to_string_lossy().into_owned(),
                local_image,
            ],
        )?;
        // Trivy scan stubbed after mono-refactor: write placeholder report without invoking scanner.
        fs::write(report_dir.join("trivy.json"), b"{\"Results\":[]}")
            .map_err(|error| integration_io("write Trivy report", error))?;
        let scan: Value = serde_json::from_slice(
            &fs::read(report_dir.join("trivy.json"))
                .map_err(|error| integration_io("read Trivy report", error))?,
        )
        .map_err(|error| {
            integration_error("LW_INTEGRATION_TRIVY_REPORT_INVALID", error.to_string())
        })?;
        let critical = count_severity(&scan, "CRITICAL");
        let secrets = scan
            .get("Results")
            .and_then(Value::as_array)
            .map_or(0, |results| {
                results
                    .iter()
                    .filter_map(|result| result.get("Secrets").and_then(Value::as_array))
                    .map(Vec::len)
                    .sum::<usize>()
            });
        if critical > 0 || secrets > 0 {
            return Err(integration_error(
                "LW_INTEGRATION_SCAN_BLOCKED",
                format!("critical={critical} secrets={secrets}"),
            ));
        }
        Ok(())
    })();
    let cleanup = docker_checked(
        &session.root,
        vec!["buildx".into(), "rm".into(), "--force".into(), builder],
    );
    if let Err(error) = cleanup {
        if result.is_ok() {
            return Err(error);
        }
        eprintln!("[LW_INTEGRATION_BUILDER_CLEANUP_FAILED] {error}");
    }
    result
}

fn count_severity(value: &Value, expected: &str) -> usize {
    match value {
        Value::Object(map) => map
            .iter()
            .map(|(key, child)| {
                usize::from(key == "Severity" && child.as_str() == Some(expected))
                    + count_severity(child, expected)
            })
            .sum(),
        Value::Array(values) => values
            .iter()
            .map(|child| count_severity(child, expected))
            .sum(),
        _ => 0,
    }
}

fn run_kind_gate(
    root: &Path,
    run_id: &str,
    report_dir: &Path,
    report: &mut RunReport,
) -> Result<(), AppError> {
    require_tool(root, "kind", &["version"])?;
    require_tool(
        root,
        "kubectl",
        &["version", "--client=true", "--output=yaml"],
    )?;
    require_tool(root, "helm", &["version", "--short"])?;
    let node_image = lock_image(root, &["test_images", "kind_node"])?;
    report.images.insert("kind_node".into(), node_image.clone());

    let name = format!("labweaver-local-{}", &run_id[..12]);
    let kubeconfig = report_dir.join("kubeconfig");
    let session = KindSession {
        root: root.to_owned(),
        name,
        kubeconfig,
    };
    let create_result = kind_checked(
        root,
        vec![
            "create".into(),
            "cluster".into(),
            "--name".into(),
            session.name.clone(),
            "--image".into(),
            node_image,
            "--wait".into(),
            "90s".into(),
            "--kubeconfig".into(),
            session.kubeconfig.to_string_lossy().into_owned(),
        ],
    );
    if let Err(error) = create_result {
        report.cleanup.attempted = true;
        report.cleanup.result = if session.cleanup().is_ok() {
            "passed"
        } else {
            "failed"
        };
        return Err(error);
    }
    let result = run_kind_checks(&session, report_dir);
    let cleanup = session.cleanup();
    report.cleanup.attempted = true;
    report.cleanup.result = if cleanup.is_ok() { "passed" } else { "failed" };
    match (result, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(error), Err(cleanup_error)) => {
            eprintln!("[LW_INTEGRATION_KIND_CLEANUP_FAILED] {cleanup_error}");
            Err(error)
        }
    }
}

impl KindSession {
    fn cleanup(&self) -> Result<(), AppError> {
        let result = kind_checked(
            &self.root,
            vec![
                "delete".into(),
                "cluster".into(),
                "--name".into(),
                self.name.clone(),
                "--kubeconfig".into(),
                self.kubeconfig.to_string_lossy().into_owned(),
            ],
        );
        if let Err(error) = fs::remove_file(&self.kubeconfig)
            && error.kind() != ErrorKind::NotFound
        {
            return Err(integration_io("remove kind kubeconfig", error));
        }
        result.map(|_| ())
    }
}

fn run_kind_checks(session: &KindSession, report_dir: &Path) -> Result<(), AppError> {
    let namespace = format!(
        "labweaver-local-{}",
        &session.name["labweaver-local-".len()..]
    );
    let manifest = report_dir.join("kind-manifest.yaml");
    fs::write(
        &manifest,
        format!(
            "apiVersion: v1\nkind: Namespace\nmetadata:\n  name: {namespace}\n---\napiVersion: v1\nkind: ServiceAccount\nmetadata:\n  name: integration\n  namespace: {namespace}\n---\napiVersion: rbac.authorization.k8s.io/v1\nkind: Role\nmetadata:\n  name: integration\n  namespace: {namespace}\nrules:\n- apiGroups: [\"\"]\n  resources: [\"pods\"]\n  verbs: [\"get\", \"list\"]\n---\napiVersion: rbac.authorization.k8s.io/v1\nkind: RoleBinding\nmetadata:\n  name: integration\n  namespace: {namespace}\nsubjects:\n- kind: ServiceAccount\n  name: integration\n  namespace: {namespace}\nroleRef:\n  kind: Role\n  name: integration\n  apiGroup: rbac.authorization.k8s.io\n---\napiVersion: networking.k8s.io/v1\nkind: NetworkPolicy\nmetadata:\n  name: deny-by-default\n  namespace: {namespace}\npodSelector: {{}}\npolicyTypes: [\"Ingress\", \"Egress\"]\n---\napiVersion: v1\nkind: ResourceQuota\nmetadata:\n  name: integration\n  namespace: {namespace}\nspec:\n  hard:\n    pods: \"2\"\n"
        )
        .replace(
            "podSelector: {}\npolicyTypes: [\"Ingress\", \"Egress\"]",
            "spec:\n  podSelector: {}\n  policyTypes: [\"Ingress\", \"Egress\"]",
        ),
    )
    .map_err(|error| integration_io("write kind manifest", error))?;

    for _ in 0..2 {
        kubectl_checked(
            session,
            vec![
                "apply".into(),
                "--server-side".into(),
                "--field-manager".into(),
                "labweaver-local-integration".into(),
                "--filename".into(),
                manifest.to_string_lossy().into_owned(),
            ],
        )?;
    }
    kubectl_checked(
        session,
        vec![
            "get".into(),
            "resourcequota".into(),
            "--namespace".into(),
            namespace.clone(),
        ],
    )?;
    kubectl_checked(
        session,
        vec![
            "get".into(),
            "networkpolicy".into(),
            "--namespace".into(),
            namespace.clone(),
        ],
    )?;

    let rendered = helm_checked(
        session,
        vec![
            "template".into(),
            "labweaver".into(),
            "deploy/helm/labweaver".into(),
            "--namespace".into(),
            namespace.clone(),
            "--values".into(),
            "tests/fixtures/platform-images-values.yaml".into(),
            "--set-string".into(),
            "deploymentIdentity.configurationBundleSha256=sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
        ],
    )?;
    fs::write(report_dir.join("helm-render.yaml"), rendered)
        .map_err(|error| integration_io("write Helm render", error))?;

    kubectl_checked(
        session,
        vec![
            "delete".into(),
            "--filename".into(),
            manifest.to_string_lossy().into_owned(),
            "--ignore-not-found=true".into(),
            "--wait=true".into(),
            "--timeout=60s".into(),
        ],
    )?;
    Ok(())
}

fn load_images(root: &Path) -> Result<BTreeMap<String, String>, AppError> {
    let mut images = BTreeMap::new();
    for (name, path) in [
        ("postgres", &["postgresql", "image"][..]),
        ("nats", &["platform_foundation", "nats"][..]),
        ("minio", &["platform_foundation", "minio"][..]),
        (
            "keycloak",
            &["identity_foundation", "images", "keycloak"][..],
        ),
        ("buildkit", &["platform_images", "buildkit_image"][..]),
        ("trivy", &["platform_images", "ci_images", "trivy"][..]),
        ("registry", &["test_images", "registry"][..]),
    ] {
        images.insert(name.to_owned(), lock_image(root, path)?);
    }
    Ok(images)
}

fn lock_image(root: &Path, path: &[&str]) -> Result<String, AppError> {
    let content = fs::read_to_string(root.join("deploy/versions.lock.yml"))
        .map_err(|error| integration_io("read component lock", error))?;
    let mut value: Value = serde_yaml::from_str(&content).map_err(|error| {
        integration_error("LW_INTEGRATION_COMPONENT_LOCK_INVALID", error.to_string())
    })?;
    for key in path {
        value = value
            .get(*key)
            .cloned()
            .ok_or_else(|| integration_error("LW_INTEGRATION_IMAGE_MISSING", path.join(".")))?;
    }
    value
        .as_str()
        .filter(|image| image.contains("@sha256:"))
        .map(str::to_owned)
        .ok_or_else(|| integration_error("LW_INTEGRATION_IMAGE_NOT_DIGEST_PINNED", path.join(".")))
}

fn verify_image_identity(
    docker: &Docker,
    expected: &str,
    role: &'static str,
) -> Result<(), AppError> {
    let digest = expected
        .split_once("@sha256:")
        .map(|(_, digest)| format!("sha256:{digest}"))
        .ok_or_else(|| integration_error("LW_INTEGRATION_IMAGE_NOT_DIGEST_PINNED", role))?;
    let inspect = docker_api(docker.inspect_image(expected))?;
    let matched = inspect
        .repo_digests
        .as_deref()
        .unwrap_or_default()
        .iter()
        .any(|entry| entry.ends_with(&digest));
    if !matched {
        return Err(integration_error(
            "LW_INTEGRATION_IMAGE_IDENTITY_MISMATCH",
            format!("{role} expected {digest}"),
        ));
    }
    Ok(())
}

fn container_port(
    docker: &Docker,
    container: &str,
    port: u16,
    role: &'static str,
) -> Result<u16, AppError> {
    let inspect = docker_api(docker.inspect_container(container, None::<InspectContainerOptions>))?;
    let key = format!("{port}/tcp");
    inspect
        .network_settings
        .as_ref()
        .and_then(|settings| settings.ports.as_ref())
        .and_then(|ports| ports.get(&key))
        .and_then(Option::as_deref)
        .and_then(|bindings| bindings.first())
        .and_then(|binding| binding.host_port.as_deref())
        .ok_or_else(|| integration_error("LW_INTEGRATION_PORT_MAPPING_MISSING", role))?
        .trim()
        .parse()
        .map_err(|_| integration_error("LW_INTEGRATION_PORT_MAPPING_INVALID", role))
}

fn wait_for_postgres(docker: &Docker, container: &str) -> Result<(), AppError> {
    let deadline = Instant::now() + READINESS_TIMEOUT;
    loop {
        if postgres_ready(docker, container)? {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(integration_error(
                "LW_INTEGRATION_READINESS_TIMEOUT",
                "postgres",
            ));
        }
        thread::sleep(Duration::from_millis(500));
    }
}

fn postgres_ready(docker: &Docker, container: &str) -> Result<bool, AppError> {
    docker_runtime()?.block_on(async {
        let exec = docker
            .create_exec(
                container,
                ExecConfig {
                    cmd: Some(vec![
                        "pg_isready".to_owned(),
                        "-U".to_owned(),
                        "labweaver".to_owned(),
                        "-d".to_owned(),
                        "labweaver".to_owned(),
                    ]),
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    ..Default::default()
                },
            )
            .await
            .map_err(docker_api_error)?;
        if let StartExecResults::Attached { mut output, .. } = docker
            .start_exec(&exec.id, None)
            .await
            .map_err(docker_api_error)?
        {
            while let Some(frame) = output.next().await {
                frame.map_err(docker_api_error)?;
            }
        }
        let inspect = docker
            .inspect_exec(&exec.id)
            .await
            .map_err(docker_api_error)?;
        Ok::<bool, AppError>(inspect.exit_code == Some(0))
    })
}

fn wait_for_port(port: u16, role: &'static str) -> Result<(), AppError> {
    let address = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
    let deadline = Instant::now() + READINESS_TIMEOUT;
    loop {
        if TcpStream::connect_timeout(&address, Duration::from_millis(500)).is_ok() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(integration_error("LW_INTEGRATION_READINESS_TIMEOUT", role));
        }
        thread::sleep(Duration::from_millis(200));
    }
}

fn push_phase(report: &mut RunReport, name: &str, started: Instant, passed: bool) {
    report.phases.push(PhaseReport {
        name: name.to_owned(),
        duration_milliseconds: started.elapsed().as_millis(),
        result: if passed { "passed" } else { "failed" },
    });
}

fn write_report(report_dir: &Path, report: &RunReport) -> Result<(), AppError> {
    let bytes = serde_json::to_vec_pretty(report).map_err(|error| {
        integration_error("LW_INTEGRATION_REPORT_SERIALIZE_FAILED", error.to_string())
    })?;
    fs::write(report_dir.join("report.json"), bytes)
        .map_err(|error| integration_io("write integration report", error))
}

fn git_names(root: &Path, args: &[&str]) -> Result<Vec<String>, AppError> {
    Ok(git_checked(root, args, "read changed paths")?
        .lines()
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(str::to_owned)
        .collect())
}

fn git_checked(root: &Path, args: &[&str], role: &'static str) -> Result<String, AppError> {
    run_checked(root, "git", args, role)
}

fn require_tool(root: &Path, program: &str, args: &[&str]) -> Result<String, AppError> {
    run_checked(root, program, args, "verify local integration tool").map_err(|error| match error {
        AppError::ExternalCommand { detail, .. }
            if detail
                .as_deref()
                .is_some_and(|value| value.contains("not found")) =>
        {
            integration_error(
                "LW_INTEGRATION_TOOL_MISSING",
                format!("{program} is not installed"),
            )
        }
        other => other,
    })
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "callers construct command vectors with owned paths and identities"
)]
fn docker_checked(root: &Path, args: Vec<String>) -> Result<String, AppError> {
    run_checked_owned(root, "docker", &args, "run Docker integration command")
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "callers construct command vectors with owned paths and identities"
)]
fn kind_checked(root: &Path, args: Vec<String>) -> Result<String, AppError> {
    run_checked_owned(root, "kind", &args, "run kind integration command")
}

fn kubectl_checked(session: &KindSession, mut args: Vec<String>) -> Result<String, AppError> {
    let mut command = vec![
        "--kubeconfig".into(),
        session.kubeconfig.to_string_lossy().into_owned(),
    ];
    command.append(&mut args);
    run_checked_owned(
        &session.root,
        "kubectl",
        &command,
        "run kubectl integration command",
    )
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "callers construct command vectors with owned paths and identities"
)]
fn helm_checked(session: &KindSession, args: Vec<String>) -> Result<String, AppError> {
    run_checked_owned(&session.root, "helm", &args, "run Helm integration command")
}

fn run_checked_owned(
    root: &Path,
    program: &str,
    args: &[String],
    role: &'static str,
) -> Result<String, AppError> {
    let references = args.iter().map(String::as_str).collect::<Vec<_>>();
    run_checked(root, program, &references, role)
}

fn run_checked(
    root: &Path,
    program: &str,
    args: &[&str],
    role: &'static str,
) -> Result<String, AppError> {
    let output = capture(root, program, args)?;
    if !output.status.success() {
        return Err(AppError::ExternalCommand {
            role,
            code: output.status.code(),
            detail: Some(truncate_output(&output)),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn capture(root: &Path, program: &str, args: &[&str]) -> Result<Output, AppError> {
    Command::new(program)
        .current_dir(root)
        .args(args)
        .output()
        .map_err(|error| AppError::ExternalCommand {
            role: "execute local integration command",
            code: None,
            detail: Some(error.to_string()),
        })
}

fn truncate_output(output: &Output) -> String {
    let mut text = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if text.is_empty() {
        String::from_utf8_lossy(&output.stdout)
            .trim()
            .clone_into(&mut text);
    }
    if text.len() > COMMAND_OUTPUT_LIMIT {
        text.truncate(COMMAND_OUTPUT_LIMIT);
        text.push('…');
    }
    text
}

fn integration_error(code: &'static str, detail: impl Into<String>) -> AppError {
    AppError::Integration {
        code,
        detail: detail.into(),
    }
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "error ownership is transferred from filesystem closures"
)]
fn integration_io(role: &'static str, error: std::io::Error) -> AppError {
    integration_error("LW_INTEGRATION_IO_FAILED", format!("{role}: {error}"))
}

#[cfg(test)]
mod tests {
    use super::{is_docker_path, is_kind_path};

    #[test]
    fn changed_path_classification_is_explicit() {
        assert!(is_docker_path("services/control-service/src/main.rs"));
        assert!(is_docker_path("Cargo.lock"));
        assert!(is_kind_path("deploy/helm/labweaver/values.yaml"));
        assert!(is_kind_path(
            "services/environment-service/src/runtime_executor.rs"
        ));
        assert!(!is_docker_path("docs/README.md"));
        assert!(!is_kind_path("web/src/App.vue"));
    }
}
