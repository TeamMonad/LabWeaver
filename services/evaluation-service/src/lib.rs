//! Evaluation-owned immutable submission collection.

pub mod collector;
pub mod freeze;
pub mod freeze_store;
pub mod ssh_source;
pub mod worker;

pub use collector::{
    CollectError, CollectorLimits, FrozenArchive, PreflightReport, PvcSnapshotSource,
    SnapshotCollector, SnapshotSource, SnapshotTransport, SourceEntry, SourceKind, SourceMetadata,
};
pub use freeze::{FreezeRequest, FreezeService, FreezeServiceError};
pub use freeze_store::{BeginFreeze, FreezeLease, PgFreezeStore};
pub use ssh_source::{SshSnapshotConfig, SshSnapshotSource};
pub use worker::{FreezeWorkerError, run_freeze_worker};
