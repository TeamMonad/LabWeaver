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
mod outbox;
mod process;
mod reconciler;
mod resolver;
mod runtime;
mod runtime_executor;
mod store;
mod terminal_bridge;
mod terminal_executor;
mod tls;

pub use api::{EnvironmentApiState, environment_api_router};
pub use container_provider::{
    CONTAINER_BACKEND_PROTOCOL_VERSION, ContainerApplyObservation, ContainerBackendFence,
    ContainerExecutorBackend, ContainerExecutorFenceError, ContainerExecutorRequest,
    ContainerExecutorRequestEnvelope, ContainerExecutorResponse, ContainerExecutorResponseEnvelope,
    ContainerProvider, ContainerProviderBackend, ContainerProviderConfiguration,
    ContainerReleasePolicy, ContainerReleaseResolver, ContainerResource, ContainerResourcePlan,
    FencedContainerExecutor, NatsContainerExecutorServer, NatsContainerProviderBackend,
    PgContainerExecutorFenceStore, PgReleaseProjectionStore, ReleaseProjectionDecision,
    ReleaseProjectionError, ResolvedContainerRelease,
};
pub use freeze_binding::{FreezeBindingConfiguration, FreezeBindingError, FreezeBindingService};
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
    OwnerResolver, OwnerResolverError, OwnerResolverPolicy, VerifiedCallerIdentity,
    authorize_endpoint_eligibility, authorize_owner_resolution, owner_resolver_router,
};
pub use runtime::{OwnerResolverRuntime, OwnerResolverRuntimeError};
pub use runtime_executor::{KubernetesContainerExecutor, RuntimeExecutorConfiguration};
pub use store::{
    EnvironmentStoreError, InboundCommandDecision, InboundLifecycleCommand, LeasedEnvironment,
    PgEnvironmentStore, StoredEnvironmentInventory,
};
pub use terminal_bridge::{TerminalBridgeError, TerminalExecutorGateway, terminal_bridge_router};
pub use terminal_executor::{
    TerminalExecutorServer, TerminalExecutorServerConfig, TerminalExecutorServerError,
};
pub use tls::{MtlsConfig, MtlsServerError, serve_owner_resolver_mtls};
