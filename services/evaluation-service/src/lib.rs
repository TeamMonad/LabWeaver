//! Evaluation-owned immutable submission collection.

pub mod api;
pub mod collector;
pub mod command_store;
pub mod freeze;
pub mod freeze_store;
pub mod outbox;
pub mod process;
pub mod ssh_source;
pub mod worker;

pub use api::{EvaluationApiState, evaluation_api_router, serve_evaluation_mtls};
pub use collector::{
    CollectError, CollectorLimits, FrozenArchive, PreflightReport, PvcSnapshotSource,
    SnapshotCollector, SnapshotSource, SnapshotTransport, SourceEntry, SourceKind, SourceMetadata,
};
pub use command_store::{
    FreezeCommandAccept, FreezeCommandStoreError, PgFreezeCommandStore, SubmissionFreezeCommand,
};
pub use freeze::{FreezeRequest, FreezeService, FreezeServiceError};
pub use freeze_store::{BeginFreeze, FreezeLease, PgFreezeStore};
pub use outbox::{EvaluationOutboxDispatcher, EvaluationOutboxError};
pub use process::{EvaluationProcessError, run_evaluation_service};
pub use ssh_source::{SshSnapshotConfig, SshSnapshotSource};
pub use worker::{FreezeWorkerError, run_freeze_worker};
