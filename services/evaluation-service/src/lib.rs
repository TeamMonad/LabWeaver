//! Evaluation-owned immutable submission collection.

pub mod agent_client;
pub mod ansible_probe;
pub mod ansible_probe_executor;
pub mod ansible_probe_job;
pub mod ansible_probe_worker;
pub mod api;
pub mod authoring_client;
pub mod collector;
pub mod command_store;
pub mod control_plane;
pub mod coordinator;
pub mod environment_client;
pub mod execution;
pub mod freeze;
pub mod freeze_store;
pub mod kubernetes_runner;
pub mod materializer;
pub mod oj;
pub mod oj_executor;
pub mod oj_job;
pub mod oj_worker;
pub mod outbox;
pub mod process;
pub mod resource_client;
pub mod ssh_source;
pub mod worker;

#[path = "../../http_transport.rs"]
pub mod http_transport;

pub use agent_client::{AgentClient, AgentClientConfiguration, AgentClientError};
pub use ansible_probe_worker::{AnsibleProbeWorkerError, run_ansible_probe_worker};
pub use api::{EvaluationApiState, evaluation_api_router, with_service_auth};
pub use authoring_client::{
    AuthoringAdmissionClient, AuthoringAdmissionClientConfiguration, AuthoringAdmissionClientError,
};
pub use collector::{
    CollectError, CollectorLimits, FrozenArchive, PreflightReport, PvcSnapshotSource,
    SnapshotCollector, SnapshotSource, SnapshotTransport, SourceEntry, SourceKind, SourceMetadata,
};
pub use command_store::{
    FreezeCommandAccept, FreezeCommandStoreError, PgFreezeCommandStore, SubmissionFreezeCommand,
};
pub use control_plane::{
    EVALUATION_EXECUTION_RESOURCES_SCHEMA_VERSION, EvaluationControlStoreError,
    EvaluationExecutionCheckpoint, EvaluationExecutionKind, EvaluationExecutionObjectRef,
    EvaluationExecutionResources, EvaluationReleaseReservation, EvaluationRunReservation,
    EvaluationStepLease, PendingResourceMeterDelivery, PgEvaluationControlStore,
};
pub use coordinator::{FreezeCoordinator, FreezeCoordinatorConfiguration, FreezeCoordinatorError};
pub use environment_client::{
    EnvironmentExecutionBindingClient, EnvironmentExecutionBindingClientConfiguration,
    EnvironmentExecutionBindingClientError, ResolvedEnvironmentExecutionBinding,
};
pub use execution::{
    EvaluationAttemptContext, EvaluationAttemptRunner, EvaluationWorker, ExecutionError,
    ProgramCommandPaths, StepExecutionPlan, TaskResourceError, TaskResourceLifecycle,
    expand_program_argv, plan_deterministic_step,
};
pub use freeze::{FreezeRequest, FreezeService, FreezeServiceError};
pub use freeze_store::{BeginFreeze, FreezeLease, PgFreezeStore};
pub use kubernetes_runner::{
    AnsibleProbeTargetConfiguration, EvaluationExecutionConfiguration, KubernetesEvaluationRunner,
};
pub use materializer::{
    ARTIFACT_MATERIALIZER_SCHEMA_VERSION, DEFAULT_MATERIALIZER_COMMAND_PATH,
    FROZEN_ARCHIVE_MEDIA_TYPE, MaterializeArtifact, MaterializeCommand, MaterializeContent,
    MaterializeDestination, MaterializerError, run_artifact_materializer,
};
pub use oj_worker::{
    OJ_HELPER_FAILURE_EXIT_CODE, OjWorkerError, run_oj_case_exec, run_oj_compile_exec,
    run_oj_worker,
};
pub use outbox::{EvaluationOutboxDispatcher, EvaluationOutboxError};
pub use process::{EvaluationProcessError, run_evaluation_service};
pub use resource_client::{ResourceClient, ResourceClientConfiguration, ResourceClientError};
pub use ssh_source::{SshSnapshotConfig, SshSnapshotSource};
pub use worker::{FreezeWorkerError, run_freeze_worker};
