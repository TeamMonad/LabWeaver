//! Evaluation-owned immutable submission collection.

pub mod ansible_probe;
pub mod ansible_probe_executor;
pub mod ansible_probe_job;
pub mod ansible_probe_worker;
pub mod api;
pub mod collector;
pub mod command_store;
mod control_helpers;
pub mod control_plane;
pub mod coordinator;
pub mod freeze;
pub mod freeze_store;
pub mod oj;
pub mod oj_executor;
pub mod oj_job;
pub mod oj_worker;
pub mod outbox;
pub mod process;
pub mod ssh_source;
pub mod worker;

pub use ansible_probe_worker::{AnsibleProbeWorkerError, run_ansible_probe_worker};
pub use api::{EvaluationApiState, evaluation_api_router, serve_evaluation_plain};
pub use collector::{
    CollectError, CollectorLimits, FrozenArchive, PreflightReport, PvcSnapshotSource,
    SnapshotCollector, SnapshotSource, SnapshotTransport, SourceEntry, SourceKind, SourceMetadata,
};
pub use command_store::{
    FreezeCommandAccept, FreezeCommandStoreError, PgFreezeCommandStore, SubmissionFreezeCommand,
};
pub use control_plane::{
    EvaluationControlStoreError, EvaluationReleaseReservation, EvaluationRunReservation,
    EvaluationStepLease, PgEvaluationControlStore,
};
pub use coordinator::{FreezeCoordinator, FreezeCoordinatorConfiguration, FreezeCoordinatorError};
pub use freeze::{FreezeRequest, FreezeService, FreezeServiceError};
pub use freeze_store::{BeginFreeze, FreezeLease, PgFreezeStore};
pub use oj_worker::{OjWorkerError, run_oj_case_exec, run_oj_compile_exec, run_oj_worker};
pub use outbox::{EvaluationOutboxDispatcher, EvaluationOutboxError};
pub use process::{EvaluationProcessError, run_evaluation_service};
pub use ssh_source::{SshSnapshotConfig, SshSnapshotSource};
pub use worker::{FreezeWorkerError, run_freeze_worker};
