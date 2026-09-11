//! Durable Environment lifecycle, reconciliation, and owner-resolution implementation.
#![allow(
    missing_docs,
    clippy::missing_errors_doc,
    reason = "the public contract crate and focused contract document own field-level wire documentation"
)]

mod api;
mod container_provider;
mod freeze_binding;
mod kubevirt_console_executor;
mod kubevirt_provider;
mod lifecycle;
mod messaging;
mod metering;
mod outbox;
mod process;
mod reconciler;
mod resolver;
mod runtime;
mod runtime_executor;
mod store;
mod terminal_bridge;
mod terminal_executor;
mod work_admission_client;
mod work_execution;

#[path = "../../http_transport.rs"]
pub mod http_transport;

pub use api::{EnvironmentApiState, environment_api_router, with_service_auth};
pub use container_provider::{
    CONTAINER_BACKEND_PROTOCOL_VERSION, ContainerApplyObservation, ContainerBackendFence,
    ContainerExecutorBackend, ContainerExecutorFenceError, ContainerExecutorRequest,
    ContainerExecutorRequestEnvelope, ContainerExecutorResponse, ContainerExecutorResponseEnvelope,
    ContainerProvider, ContainerProviderBackend, ContainerProviderConfiguration,
    ContainerReleasePolicy, ContainerReleaseResolver, ContainerResource, ContainerResourcePlan,
    ContainerWorkspaceAccessMode, FencedContainerExecutor, NatsContainerExecutorServer,
    NatsContainerProviderBackend, PgContainerExecutorFenceStore, PgReleaseProjectionStore,
    ReleaseProjectionDecision, ReleaseProjectionError, ResolvedContainerRelease,
};
pub use freeze_binding::{
    FreezeBindingConfiguration, FreezeBindingError, FreezeBindingService,
    VmFreezeBindingConfiguration,
};
pub use kubevirt_console_executor::{
    KubeVirtConsoleExecutorServer, KubeVirtConsoleExecutorServerConfig,
    KubeVirtConsoleExecutorServerError, KubeVirtConsoleKubernetesConfiguration,
};
pub use kubevirt_provider::{
    FencedKubeVirtExecutor, KUBEVIRT_BACKEND_PROTOCOL_VERSION, KubeVirtBackendFence,
    KubeVirtCleanupPlan, KubeVirtExecutorBackend, KubeVirtExecutorFenceError,
    KubeVirtExecutorRequest, KubeVirtExecutorRequestEnvelope, KubeVirtExecutorResponse,
    KubeVirtExecutorResponseEnvelope, KubeVirtObservationStore, KubeVirtObservationStoreError,
    KubeVirtProvider, KubeVirtProviderBackend, KubeVirtProviderConfiguration, KubeVirtResource,
    KubeVirtResourceBudget, KubeVirtResourcePlan, KubeVirtRunningObservation, KubeVirtSshBootstrap,
    KubeVirtStoppedObservation, KubeVirtStorageBinding, NatsKubeVirtExecutorServer,
    NatsKubeVirtProviderBackend, PgKubeVirtExecutorFenceStore, PgKubeVirtObservationStore,
};
pub use lifecycle::{
    LifecycleCommand, LifecycleError, apply_provider_failure, apply_provider_observation,
    apply_retry, begin_timeout_cleanup, plan_command, plan_command_authorized,
};
pub use messaging::{
    CommandConsumeOutcome, JetStreamCommandConsumer, JetStreamEventPublisher,
    JetStreamReleaseConsumer, LifecycleCommandMessage, NatsAccessRevoker, NatsEnvironmentProvider,
    NatsMessagingError, NatsResourceLeaseVerifier, connect_nats_mtls,
};
pub use outbox::{
    EnvironmentEventPublisher, OutboxDispatchError, OutboxDispatchOutcome, OutboxDispatcher,
    PublishFailure,
};
pub use process::{EnvironmentProcessRuntime, EnvironmentProcessRuntimeError};
pub use reconciler::{
    EnvironmentProvider, ProviderFailure, ProviderFailureCode, ProviderObservation,
    ProviderRegistry, ReconcileAction, ReconcileError, ReconcileWorker, ReconcileWorkerError,
    ReconcileWorkerOutcome, Reconciler, next_action,
};
pub use resolver::{
    OwnerResolver, OwnerResolverError, authorize_endpoint_eligibility, authorize_owner_resolution,
    owner_resolver_router,
};
pub use runtime::{OwnerResolverRuntime, OwnerResolverRuntimeError};
pub use runtime_executor::{KubernetesContainerExecutor, RuntimeExecutorConfiguration};
pub use store::{
    EnvironmentInventoryFilter, EnvironmentInventoryPage, EnvironmentOperationPage,
    EnvironmentStoreError, InboundCommandDecision, InboundLifecycleCommand, LeasedEnvironment,
    PgEnvironmentStore, StoredEnvironmentInventory, StoredEnvironmentOperation,
};
pub use terminal_bridge::{TerminalBridgeError, TerminalExecutorGateway, terminal_bridge_router};
pub use terminal_executor::{
    TerminalExecutorServer, TerminalExecutorServerConfig, TerminalExecutorServerError,
};
pub use work_admission_client::{
    WorkAdmissionClient, WorkAdmissionClientConfiguration, WorkAdmissionClientError,
    WorkAdmissionResolver, required_scope_for_diagnostics as work_admission_scope_for_diagnostics,
};
pub use work_execution::{
    ContainerWorkExecutionBackend, ContainerWorkExecutionService, ContainerWorkExecutionTarget,
    KubernetesWorkExecutionBackend, WorkExecutionError, WorkExecutionOutcome,
};
