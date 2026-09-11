//! Deterministic JSON Schema and OpenAPI generation from Rust-owned contracts.

use std::collections::BTreeMap;

use schemars::{Schema, schema_for};
use serde_json::{Value, json};

use crate::events::{self, CloudEvent};
use crate::http::{ApiSurface, Method, MutationContract, OPERATIONS, OperationScopeKind};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GeneratedArtifact {
    pub relative_path: String,
    pub bytes: Vec<u8>,
}

pub fn generate_all() -> Result<Vec<GeneratedArtifact>, GenerationError> {
    let mut output = Vec::new();
    macro_rules! document {
        ($path:literal, $type:ty) => {
            output.push(schema_artifact($path, schema_for!($type))?);
        };
    }
    document!(
        "schemas/contracts/v1/problem-package.schema.json",
        crate::authoring::ProblemPackage
    );
    document!(
        "schemas/contracts/v1/project-llm-egress-policy.schema.json",
        crate::authoring::ProjectLlmEgressPolicy
    );
    document!(
        "schemas/contracts/v1/project.schema.json",
        crate::project::Project
    );
    document!(
        "schemas/contracts/v1/agent-run.schema.json",
        crate::authoring::AgentRun
    );
    document!(
        "schemas/contracts/v1/environment-candidate.schema.json",
        crate::authoring::EnvironmentCandidate
    );
    document!(
        "schemas/contracts/v1/evaluation-candidate.schema.json",
        crate::authoring::EvaluationCandidate
    );
    document!(
        "schemas/contracts/v1/candidate-approval.schema.json",
        crate::authoring::CandidateApproval
    );
    document!(
        "schemas/contracts/v1/authoring-approval.schema.json",
        crate::authoring::AuthoringApproval
    );
    document!(
        "schemas/contracts/v1/authoring-approval-publication-status.schema.json",
        crate::authoring::AuthoringApprovalPublicationStatus
    );
    document!(
        "schemas/contracts/v1/environment-spec.schema.json",
        crate::authoring::EnvironmentSpec
    );
    output.push(json_artifact(
        "schemas/contracts/v1/evaluation-spec.schema.json",
        crate::evaluation::evaluation_spec_schema()
            .map_err(|error| GenerationError::Contract(error.to_string()))?,
    )?);
    output.push(json_artifact(
        "schemas/contracts/v1/goal-review.schema.json",
        crate::evaluation::goal_review_schema()
            .map_err(|error| GenerationError::Contract(error.to_string()))?,
    )?);
    document!(
        "schemas/contracts/v1/evaluation-release.schema.json",
        crate::evaluation::EvaluationRelease
    );
    document!(
        "schemas/contracts/v1/evaluation-run.schema.json",
        crate::evaluation::EvaluationRun
    );
    document!(
        "schemas/contracts/v1/student-evaluation-result.schema.json",
        crate::evaluation::StudentEvaluationResult
    );
    document!(
        "schemas/contracts/v1/evaluation-step-run.schema.json",
        crate::evaluation::EvaluationStepRun
    );
    document!(
        "schemas/contracts/v1/evaluation-runtime-identity.schema.json",
        crate::evaluation::EvaluationRuntimeIdentity
    );
    document!(
        "schemas/contracts/v1/evaluation-execution-binding.schema.json",
        crate::evaluation::EvaluationExecutionBinding
    );
    document!(
        "schemas/contracts/v1/approved-program-profile.schema.json",
        crate::evaluation::ApprovedProgramProfile
    );
    document!(
        "schemas/contracts/v1/evaluation-run-identity.schema.json",
        crate::evaluation::EvaluationRunIdentity
    );
    document!(
        "schemas/contracts/v1/evaluation-step-completion.schema.json",
        crate::evaluation::EvaluationStepCompletion
    );
    document!(
        "schemas/contracts/v1/submission-manifest.schema.json",
        crate::submission::SubmissionManifest
    );
    document!(
        "schemas/contracts/v1/frozen-submission.schema.json",
        crate::submission::FrozenSubmission
    );
    document!(
        "schemas/contracts/v1/internal/environment-freeze-binding-request.schema.json",
        crate::submission::EnvironmentFreezeBindingRequest
    );
    document!(
        "schemas/contracts/v1/internal/environment-freeze-binding.schema.json",
        crate::submission::EnvironmentFreezeBinding
    );
    document!(
        "schemas/contracts/v1/build-request.schema.json",
        crate::supply_chain::BuildRequest
    );
    document!(
        "schemas/contracts/v1/image-artifact.schema.json",
        crate::supply_chain::ImageArtifact
    );
    document!(
        "schemas/contracts/v1/environment-template-release.schema.json",
        crate::supply_chain::EnvironmentTemplateRelease
    );
    document!(
        "schemas/contracts/v1/release-withdrawal.schema.json",
        crate::supply_chain::ReleaseWithdrawal
    );
    document!(
        "schemas/contracts/v1/http/environment-template-release-view.schema.json",
        crate::supply_chain::EnvironmentTemplateReleaseView
    );
    document!(
        "schemas/contracts/v1/environment-instance.schema.json",
        crate::environment::EnvironmentInstance
    );
    document!(
        "schemas/contracts/v1/internal/environment-execution-binding-request.schema.json",
        crate::environment::EnvironmentExecutionBindingRequest
    );
    document!(
        "schemas/contracts/v1/internal/environment-execution-binding.schema.json",
        crate::environment::EnvironmentExecutionBinding
    );
    document!(
        "schemas/contracts/v1/environment-summary.schema.json",
        crate::environment::EnvironmentSummary
    );
    document!(
        "schemas/contracts/v1/environment-operation-snapshot.schema.json",
        crate::environment::EnvironmentOperationSnapshot
    );
    document!(
        "schemas/contracts/v1/http/environment-summary-page.schema.json",
        crate::http::SnapshotPage<crate::environment::EnvironmentSummary>
    );
    document!(
        "schemas/contracts/v1/http/environment-operation-page.schema.json",
        crate::http::SnapshotPage<crate::environment::EnvironmentOperationSnapshot>
    );
    document!(
        "schemas/contracts/v1/environment-create-spec.schema.json",
        crate::environment::EnvironmentCreateSpec
    );
    document!(
        "schemas/contracts/v1/environment-reset-target.schema.json",
        crate::environment::EnvironmentResetTarget
    );
    document!(
        "schemas/contracts/v1/http/reset-environment-request.schema.json",
        crate::http::ResetEnvironmentRequest
    );
    document!(
        "schemas/contracts/v1/environment-lease-verification-request.schema.json",
        crate::environment::EnvironmentLeaseVerificationRequest
    );
    document!(
        "schemas/contracts/v1/environment-lease-verification-response.schema.json",
        crate::environment::EnvironmentLeaseVerificationResponse
    );
    document!(
        "schemas/contracts/v1/resource-work-handoff.schema.json",
        crate::environment::ResourceWorkHandoff
    );
    document!(
        "schemas/contracts/v1/resource-work-lease-update.schema.json",
        crate::environment::ResourceWorkLeaseUpdate
    );
    document!(
        "schemas/contracts/v1/resource-work-cleanup.schema.json",
        crate::environment::ResourceWorkCleanup
    );
    document!(
        "schemas/contracts/v1/resource-work-cleanup-status.schema.json",
        crate::environment::ResourceWorkCleanupStatus
    );
    document!(
        "schemas/contracts/v1/resource-request.schema.json",
        crate::resource::ResourceRequest
    );
    document!(
        "schemas/contracts/v1/resource-approval.schema.json",
        crate::resource::ResourceApproval
    );
    document!(
        "schemas/contracts/v1/capacity-claim.schema.json",
        crate::resource::CapacityClaim
    );
    document!(
        "schemas/contracts/v1/resource-lease.schema.json",
        crate::resource::ResourceLease
    );
    document!(
        "schemas/contracts/v1/resource-lease-authorization.schema.json",
        crate::resource::ResourceLeaseAuthorization
    );
    document!(
        "schemas/contracts/v1/gpu-catalog-entry.schema.json",
        crate::resource::GpuCatalogEntry
    );
    document!(
        "schemas/contracts/v1/resource-rate.schema.json",
        crate::resource::ResourceRate
    );
    document!(
        "schemas/contracts/v1/resource-usage-record.schema.json",
        crate::resource::ResourceUsageRecord
    );
    document!(
        "schemas/contracts/v1/resource-charge.schema.json",
        crate::resource::ResourceCharge
    );
    document!(
        "schemas/contracts/v1/resource-budget.schema.json",
        crate::resource::ResourceBudget
    );
    document!(
        "schemas/contracts/v1/environment-endpoint.schema.json",
        crate::environment::EnvironmentEndpoint
    );
    document!(
        "schemas/contracts/v1/http/environment-owner-resolution-request.schema.json",
        crate::environment::EnvironmentOwnerResolutionRequest
    );
    document!(
        "schemas/contracts/v1/environment-owner-resolution.schema.json",
        crate::environment::EnvironmentOwnerResolution
    );
    document!(
        "schemas/contracts/v1/http/environment-work-configuration-target-query.schema.json",
        crate::environment::EnvironmentWorkConfigurationTargetQuery
    );
    document!(
        "schemas/contracts/v1/environment-work-configuration-target.schema.json",
        crate::environment::EnvironmentWorkConfigurationTarget
    );
    document!(
        "schemas/contracts/v1/http/environment-endpoint-eligibility-request.schema.json",
        crate::environment::EnvironmentEndpointEligibilityRequest
    );
    document!(
        "schemas/contracts/v1/environment-endpoint-eligibility.schema.json",
        crate::environment::EnvironmentEndpointEligibility
    );
    document!(
        "schemas/contracts/v1/internal/environment-console-eligibility-request.schema.json",
        crate::environment::EnvironmentConsoleEligibilityRequest
    );
    document!(
        "schemas/contracts/v1/internal/environment-console-eligibility.schema.json",
        crate::environment::EnvironmentConsoleEligibility
    );
    document!(
        "schemas/contracts/v1/environment-owner-resolver-client-config.schema.json",
        crate::environment::EnvironmentOwnerResolverClientConfig
    );
    document!(
        "schemas/contracts/v1/access-grant.schema.json",
        crate::access::AccessGrant
    );
    document!(
        "schemas/contracts/v1/access-grant-snapshot.schema.json",
        crate::access::AccessGrantSnapshot
    );
    document!(
        "schemas/contracts/v1/console-capability-availability.schema.json",
        crate::access::ConsoleCapabilityAvailability
    );
    document!(
        "schemas/contracts/v1/console-capability.schema.json",
        crate::access::ConsoleCapability
    );
    document!(
        "schemas/contracts/v1/console-client-control.schema.json",
        crate::access::ConsoleClientControl
    );
    document!(
        "schemas/contracts/v1/console-session.schema.json",
        crate::access::ConsoleSession
    );
    document!(
        "schemas/contracts/v1/terminal-spec.schema.json",
        crate::authoring::TerminalSpec
    );
    document!(
        "schemas/contracts/v1/http/environment-access-grant-page.schema.json",
        crate::http::SnapshotPage<crate::access::AccessGrantSnapshot>
    );
    document!(
        "schemas/contracts/v1/endpoint-grant.schema.json",
        crate::access::EndpointGrant
    );
    document!(
        "schemas/contracts/v1/ssh-public-key.schema.json",
        crate::access::SshPublicKey
    );
    document!(
        "schemas/contracts/v1/ssh-authorization-request.schema.json",
        crate::access::SshAuthorizationRequest
    );
    document!(
        "schemas/contracts/v1/ssh-authorization.schema.json",
        crate::access::SshAuthorization
    );
    document!(
        "schemas/contracts/v1/gateway-session.schema.json",
        crate::access::GatewaySession
    );
    document!(
        "schemas/contracts/v1/create-gateway-session-request.schema.json",
        crate::access::CreateGatewaySessionRequest
    );
    document!(
        "schemas/contracts/v1/heartbeat-gateway-session-request.schema.json",
        crate::access::HeartbeatGatewaySessionRequest
    );
    document!(
        "schemas/contracts/v1/close-gateway-session-request.schema.json",
        crate::access::CloseGatewaySessionRequest
    );
    document!(
        "schemas/contracts/v1/authenticated-actor.schema.json",
        crate::auth::AuthenticatedActor
    );
    document!(
        "schemas/contracts/v1/auth-session.schema.json",
        crate::auth::AuthSession
    );
    document!(
        "schemas/contracts/v1/csrf-token-response.schema.json",
        crate::auth::CsrfTokenResponse
    );
    document!(
        "schemas/contracts/v1/course-membership.schema.json",
        crate::auth::CourseMembership
    );
    document!(
        "schemas/contracts/v1/project-membership.schema.json",
        crate::auth::ProjectMembership
    );
    document!(
        "schemas/contracts/v1/authorization-decision.schema.json",
        crate::auth::AuthorizationDecision
    );
    document!(
        "schemas/contracts/v1/authorization-decision-request.schema.json",
        crate::auth::AuthorizationDecisionRequest
    );
    document!(
        "schemas/contracts/v1/http/internal-create-agent-run-request.schema.json",
        crate::http::InternalCreateAgentRunRequest
    );
    document!(
        "schemas/contracts/v1/http/internal-agent-run-mutation-request.schema.json",
        crate::http::InternalAgentRunMutationRequest
    );
    document!(
        "schemas/contracts/v1/http/internal-agent-llm-review-request.schema.json",
        crate::http::InternalAgentLlmReviewRequest
    );
    document!(
        "schemas/contracts/v1/http/internal-agent-llm-review-receipt.schema.json",
        crate::http::InternalAgentLlmReviewReceipt
    );
    document!(
        "schemas/contracts/v1/http/agent-llm-review-query.schema.json",
        crate::http::AgentLlmReviewQuery
    );
    document!(
        "schemas/contracts/v1/http/generated-artifact-query.schema.json",
        crate::http::GeneratedArtifactQuery
    );
    document!(
        "schemas/contracts/v1/http/generated-artifact-record.schema.json",
        crate::http::GeneratedArtifactRecord
    );
    document!(
        "schemas/contracts/v1/http/approve-work-configuration-request.schema.json",
        crate::http::ApproveWorkConfigurationRequest
    );
    document!(
        "schemas/contracts/v1/http/internal-approve-work-configuration-request.schema.json",
        crate::http::InternalApproveWorkConfigurationRequest
    );
    document!(
        "schemas/contracts/v1/http/internal-agent-build-cancellation-request.schema.json",
        crate::http::InternalAgentBuildCancellationRequest
    );
    document!(
        "schemas/contracts/v1/http/internal-agent-build-cancellation-result.schema.json",
        crate::http::InternalAgentBuildCancellationResult
    );
    document!(
        "schemas/contracts/v1/http/internal-agent-build-status-query.schema.json",
        crate::http::InternalAgentBuildStatusQuery
    );
    document!(
        "schemas/contracts/v1/http/internal-agent-run-outcome.schema.json",
        crate::http::InternalAgentRunOutcome
    );
    document!(
        "schemas/contracts/v1/http/internal-image-artifact-resolution.schema.json",
        crate::http::InternalImageArtifactResolution
    );
    document!(
        "schemas/contracts/v1/http/authoring-publication-admission-binding.schema.json",
        crate::http::AuthoringPublicationAdmissionBinding
    );
    document!(
        "schemas/contracts/v1/http/authoring-publication-admission-query.schema.json",
        crate::http::AuthoringPublicationAdmissionQuery
    );
    document!(
        "schemas/contracts/v1/http/work-configuration-admission-binding.schema.json",
        crate::http::WorkConfigurationAdmissionBinding
    );
    document!(
        "schemas/contracts/v1/http/work-configuration-admission-query.schema.json",
        crate::http::WorkConfigurationAdmissionQuery
    );
    document!(
        "schemas/contracts/v1/http/container-work-execution-request.schema.json",
        crate::http::ContainerWorkExecutionRequest
    );
    document!(
        "schemas/contracts/v1/http/container-work-execution-query.schema.json",
        crate::http::ContainerWorkExecutionQuery
    );
    document!(
        "schemas/contracts/v1/http/agent-work-execution-intent-metadata.schema.json",
        crate::http::AgentWorkExecutionIntentMetadata
    );
    document!(
        "schemas/contracts/v1/http/agent-work-execution-intent-query.schema.json",
        crate::http::AgentWorkExecutionIntentQuery
    );
    document!(
        "schemas/contracts/v1/http/container-work-execution-receipt.schema.json",
        crate::http::ContainerWorkExecutionReceipt
    );
    document!(
        "schemas/contracts/v1/http/internal-publish-evaluation-release-request.schema.json",
        crate::http::InternalPublishEvaluationReleaseRequest
    );
    document!(
        "schemas/contracts/v1/http/internal-withdraw-evaluation-release-request.schema.json",
        crate::http::InternalWithdrawEvaluationReleaseRequest
    );
    document!(
        "schemas/contracts/v1/http/create-evaluation-release-request.schema.json",
        crate::http::CreateEvaluationReleaseRequest
    );
    document!(
        "schemas/contracts/v1/http/withdraw-evaluation-release-request.schema.json",
        crate::http::WithdrawEvaluationReleaseRequest
    );
    document!(
        "schemas/contracts/v1/http/internal-create-evaluation-run-request.schema.json",
        crate::http::InternalCreateEvaluationRunRequest
    );
    document!(
        "schemas/contracts/v1/http/internal-evaluation-run-mutation-request.schema.json",
        crate::http::InternalEvaluationRunMutationRequest
    );
    document!(
        "schemas/contracts/v1/http/internal-complete-evaluation-step-request.schema.json",
        crate::http::InternalCompleteEvaluationStepRequest
    );
    document!(
        "schemas/contracts/v1/problem-details.schema.json",
        crate::ProblemDetails
    );
    document!(
        "schemas/contracts/v1/http/create-problem-package-upload-request.schema.json",
        crate::http::CreateProblemPackageUploadRequest
    );
    document!(
        "schemas/contracts/v1/http/problem-package-upload-session.schema.json",
        crate::http::ProblemPackageUploadSession
    );
    document!(
        "schemas/contracts/v1/http/complete-problem-package-upload-request.schema.json",
        crate::http::CompleteProblemPackageUploadRequest
    );
    document!(
        "schemas/contracts/v1/http/create-agent-run-request.schema.json",
        crate::http::CreateAgentRunRequest
    );
    document!(
        "schemas/contracts/v1/http/create-project-request.schema.json",
        crate::project::CreateProjectRequest
    );
    document!(
        "schemas/contracts/v1/http/update-project-request.schema.json",
        crate::project::UpdateProjectRequest
    );
    document!(
        "schemas/contracts/v1/http/add-project-membership-request.schema.json",
        crate::http::AddProjectMembershipRequest
    );
    document!(
        "schemas/contracts/v1/http/remove-project-membership-request.schema.json",
        crate::http::RemoveProjectMembershipRequest
    );
    document!(
        "schemas/contracts/v1/http/create-work-configuration-run-request.schema.json",
        crate::http::CreateWorkConfigurationRunRequest
    );
    document!(
        "schemas/contracts/v1/http/work-configuration-plan-view.schema.json",
        crate::http::WorkConfigurationPlanView
    );
    document!(
        "schemas/contracts/v1/http/candidate-decision-request.schema.json",
        crate::http::CandidateDecisionRequest
    );
    document!(
        "schemas/contracts/v1/http/complete-authoring-approval-request.schema.json",
        crate::http::CompleteAuthoringApprovalRequest
    );
    document!(
        "schemas/contracts/v1/http/environment-candidate-view.schema.json",
        crate::http::EnvironmentCandidateView
    );
    document!(
        "schemas/contracts/v1/http/evaluation-candidate-view.schema.json",
        crate::http::EvaluationCandidateView
    );
    document!(
        "schemas/contracts/v1/http/create-environment-template-release-request.schema.json",
        crate::http::CreateEnvironmentTemplateReleaseRequest
    );
    document!(
        "schemas/contracts/v1/http/withdraw-environment-template-release-request.schema.json",
        crate::http::WithdrawEnvironmentTemplateReleaseRequest
    );
    document!(
        "schemas/contracts/v1/http/create-environment-request.schema.json",
        crate::http::CreateEnvironmentRequest
    );
    document!(
        "schemas/contracts/v1/http/create-resource-request.schema.json",
        crate::http::CreateResourceRequest
    );
    document!(
        "schemas/contracts/v1/http/create-task-resource-target.schema.json",
        crate::http::CreateTaskResourceTarget
    );
    document!(
        "schemas/contracts/v1/http/internal-create-task-resource-request.schema.json",
        crate::http::InternalCreateTaskResourceRequest
    );
    document!(
        "schemas/contracts/v1/http/record-resource-usage-request.schema.json",
        crate::http::RecordResourceUsageRequest
    );
    document!(
        "schemas/contracts/v1/http/task-resource-status.schema.json",
        crate::http::TaskResourceStatus
    );
    document!(
        "schemas/contracts/v1/http/acknowledge-task-resource-request.schema.json",
        crate::http::AcknowledgeTaskResourceRequest
    );
    document!(
        "schemas/contracts/v1/http/release-task-resource-request.schema.json",
        crate::http::ReleaseTaskResourceRequest
    );
    document!(
        "schemas/contracts/v1/http/create-resource-rate-request.schema.json",
        crate::http::CreateResourceRateRequest
    );
    document!(
        "schemas/contracts/v1/http/upsert-resource-budget-request.schema.json",
        crate::http::UpsertResourceBudgetRequest
    );
    document!(
        "schemas/contracts/v1/http/create-resource-adjustment-request.schema.json",
        crate::http::CreateResourceAdjustmentRequest
    );
    document!(
        "schemas/contracts/v1/http/approve-resource-request.schema.json",
        crate::http::ApproveResourceRequest
    );
    document!(
        "schemas/contracts/v1/http/resource-request-mutation.schema.json",
        crate::http::ResourceRequestMutation
    );
    document!(
        "schemas/contracts/v1/http/renew-resource-lease.schema.json",
        crate::http::RenewResourceLease
    );
    document!(
        "schemas/contracts/v1/http/resource-operation-accepted.schema.json",
        crate::http::ResourceOperationAccepted
    );
    document!(
        "schemas/contracts/v1/http/environment-operation-accepted.schema.json",
        crate::http::EnvironmentOperationAccepted
    );
    document!(
        "schemas/contracts/v1/http/environment-inventory-query.schema.json",
        crate::http::EnvironmentInventoryQuery
    );
    document!(
        "schemas/contracts/v1/http/environment-operation-list-query.schema.json",
        crate::http::EnvironmentOperationListQuery
    );
    document!(
        "schemas/contracts/v1/http/environment-access-grant-list-query.schema.json",
        crate::http::EnvironmentAccessGrantListQuery
    );
    document!(
        "schemas/contracts/v1/http/environment-management-event.schema.json",
        crate::http::SseEvent<crate::http::EnvironmentManagementStreamEvent>
    );
    document!(
        "schemas/contracts/v1/http/freeze-submission-request.schema.json",
        crate::http::FreezeSubmissionRequest
    );
    document!(
        "schemas/contracts/v1/http/create-ssh-public-key-request.schema.json",
        crate::http::CreateSshPublicKeyRequest
    );
    document!(
        "schemas/contracts/v1/http/create-access-grant-request.schema.json",
        crate::http::CreateAccessGrantRequest
    );
    document!(
        "schemas/contracts/v1/http/revoke-access-grant-request.schema.json",
        crate::http::RevokeAccessGrantRequest
    );
    document!(
        "schemas/contracts/v1/http/renew-access-grant-request.schema.json",
        crate::http::RenewAccessGrantRequest
    );
    document!(
        "schemas/contracts/v1/http/issue-console-capability-request.schema.json",
        crate::http::IssueConsoleCapabilityRequest
    );

    document!(
        "schemas/contracts/v1/events/agent-run-requested.schema.json",
        CloudEvent<events::AgentRunEvent>
    );
    document!(
        "schemas/contracts/v1/events/resource-request-submitted.schema.json",
        CloudEvent<events::ResourceRequestChanged>
    );
    document!(
        "schemas/contracts/v1/events/resource-request-approved.schema.json",
        CloudEvent<events::ResourceRequestChanged>
    );
    document!(
        "schemas/contracts/v1/events/resource-request-rejected.schema.json",
        CloudEvent<events::ResourceRequestChanged>
    );
    document!(
        "schemas/contracts/v1/events/resource-request-cancelled.schema.json",
        CloudEvent<events::ResourceRequestChanged>
    );
    document!(
        "schemas/contracts/v1/events/resource-request-state-changed.schema.json",
        CloudEvent<events::ResourceRequestChanged>
    );
    document!(
        "schemas/contracts/v1/events/resource-lease-activated.schema.json",
        CloudEvent<events::ResourceLeaseChanged>
    );
    document!(
        "schemas/contracts/v1/events/resource-lease-renewed.schema.json",
        CloudEvent<events::ResourceLeaseChanged>
    );
    document!(
        "schemas/contracts/v1/events/resource-lease-revoked.schema.json",
        CloudEvent<events::ResourceLeaseChanged>
    );
    document!(
        "schemas/contracts/v1/events/resource-lease-expiring.schema.json",
        CloudEvent<events::ResourceLeaseChanged>
    );
    document!(
        "schemas/contracts/v1/events/resource-lease-expired.schema.json",
        CloudEvent<events::ResourceLeaseChanged>
    );
    document!(
        "schemas/contracts/v1/events/agent-run-completed.schema.json",
        CloudEvent<events::AgentRunEvent>
    );
    document!(
        "schemas/contracts/v1/events/agent-run-failed.schema.json",
        CloudEvent<events::AgentRunEvent>
    );
    document!(
        "schemas/contracts/v1/events/agent-build-requested.schema.json",
        CloudEvent<events::AgentBuildRequested>
    );
    document!(
        "schemas/contracts/v1/events/agent-build-completed.schema.json",
        CloudEvent<events::AgentBuildCompleted>
    );
    document!(
        "schemas/contracts/v1/events/agent-build-failed.schema.json",
        CloudEvent<events::AgentBuildFailed>
    );
    document!(
        "schemas/contracts/v1/events/environment-provision-requested.schema.json",
        CloudEvent<events::EnvironmentEvent>
    );
    document!(
        "schemas/contracts/v1/events/environment-ready.schema.json",
        CloudEvent<events::EnvironmentEvent>
    );
    document!(
        "schemas/contracts/v1/events/environment-failed.schema.json",
        CloudEvent<events::EnvironmentEvent>
    );
    document!(
        "schemas/contracts/v1/events/environment-delete-requested.schema.json",
        CloudEvent<events::EnvironmentEvent>
    );
    document!(
        "schemas/contracts/v1/events/environment-operation-accepted.schema.json",
        CloudEvent<events::EnvironmentEvent>
    );
    document!(
        "schemas/contracts/v1/events/environment-state-changed.schema.json",
        CloudEvent<events::EnvironmentEvent>
    );
    document!(
        "schemas/contracts/v1/events/environment-lifecycle-requested.schema.json",
        CloudEvent<crate::environment::EnvironmentLifecycleCommandData>
    );
    document!(
        "schemas/contracts/v1/events/access-grant-created.schema.json",
        CloudEvent<events::AccessGrantChanged>
    );
    document!(
        "schemas/contracts/v1/events/access-grant-activated.schema.json",
        CloudEvent<events::AccessGrantChanged>
    );
    document!(
        "schemas/contracts/v1/events/access-grant-denied.schema.json",
        CloudEvent<events::AccessGrantChanged>
    );
    document!(
        "schemas/contracts/v1/events/access-grant-expired.schema.json",
        CloudEvent<events::AccessGrantChanged>
    );
    document!(
        "schemas/contracts/v1/events/access-grant-revoked.schema.json",
        CloudEvent<events::AccessGrantChanged>
    );
    document!(
        "schemas/contracts/v1/events/access-ssh-key-revoked.schema.json",
        CloudEvent<events::SshPublicKeyRevoked>
    );
    document!(
        "schemas/contracts/v1/events/access-session-termination-requested.schema.json",
        CloudEvent<events::GatewaySessionChanged>
    );
    document!(
        "schemas/contracts/v1/events/access-session-closed.schema.json",
        CloudEvent<events::GatewaySessionChanged>
    );
    document!(
        "schemas/contracts/v1/events/access-session-termination-overdue.schema.json",
        CloudEvent<events::GatewaySessionChanged>
    );
    document!(
        "schemas/contracts/v1/events/access-console-session-state-changed.schema.json",
        CloudEvent<events::ConsoleSessionChanged>
    );
    document!(
        "schemas/contracts/v1/events/submission-freeze-requested.schema.json",
        CloudEvent<events::SubmissionFreezeRequested>
    );
    document!(
        "schemas/contracts/v1/events/submission-frozen.schema.json",
        CloudEvent<events::SubmissionFrozen>
    );
    document!(
        "schemas/contracts/v1/events/evaluation-release-published.schema.json",
        CloudEvent<events::EvaluationReleasePublished>
    );
    document!(
        "schemas/contracts/v1/events/evaluation-run-requested.schema.json",
        CloudEvent<events::EvaluationRunEvent>
    );
    document!(
        "schemas/contracts/v1/events/evaluation-run-state-changed.schema.json",
        CloudEvent<events::EvaluationRunEvent>
    );
    document!(
        "schemas/contracts/v1/events/evaluation-step-run-state-changed.schema.json",
        CloudEvent<events::EvaluationStepRunEvent>
    );
    document!(
        "schemas/contracts/v1/events/lab-release-approved.schema.json",
        CloudEvent<events::LabReleaseApproved>
    );
    document!(
        "schemas/contracts/v1/events/environment-template-release-published.schema.json",
        CloudEvent<events::ReleasePublished>
    );
    document!(
        "schemas/contracts/v1/events/environment-template-release-withdrawn.schema.json",
        CloudEvent<events::ReleaseWithdrawn>
    );
    document!(
        "schemas/contracts/v1/events/authoring-approval-completed.schema.json",
        CloudEvent<events::AuthoringApprovalCompleted>
    );

    output.push(json_artifact(
        "schemas/openapi/labweaver-public.v1.json",
        openapi(ApiSurface::Public)?,
    )?);
    output.push(json_artifact(
        "schemas/openapi/labweaver-gateway-internal.v1.json",
        openapi(ApiSurface::GatewayInternal)?,
    )?);
    output.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    Ok(output)
}

fn schema_artifact(path: &str, schema: Schema) -> Result<GeneratedArtifact, GenerationError> {
    json_artifact(path, serde_json::to_value(schema)?)
}

fn json_artifact(path: &str, value: Value) -> Result<GeneratedArtifact, GenerationError> {
    let mut bytes = serde_json::to_vec_pretty(&value)?;
    bytes.push(b'\n');
    Ok(GeneratedArtifact {
        relative_path: path.to_owned(),
        bytes,
    })
}

fn openapi(surface: ApiSurface) -> Result<Value, GenerationError> {
    let title = match surface {
        ApiSurface::Public => "LabWeaver Public API",
        ApiSurface::GatewayInternal => "LabWeaver Gateway Internal API",
    };
    let mut paths: BTreeMap<String, Value> = BTreeMap::new();
    for operation in OPERATIONS
        .iter()
        .filter(|operation| operation.surface == surface)
    {
        let method = match operation.method {
            Method::Get => "get",
            Method::Post => "post",
            Method::Put => "put",
            Method::Patch => "patch",
            Method::Delete => "delete",
        };
        let mut parameters = path_parameters(operation.path);
        if matches!(
            operation.operation_id,
            "listEnvironmentTemplateReleases"
                | "listEvaluationReleases"
                | "listOwnEvaluationResults"
                | "listOwnProjectEvaluationResults"
                | "listSshPublicKeys"
        ) {
            parameters.push(json!({"name":"cursor","in":"query","required":false,"schema":{"type":"string","minLength":1,"maxLength":512}}));
            parameters.push(json!({"name":"limit","in":"query","required":false,"schema":{"type":"integer","minimum":1,"maximum":100,"default":50}}));
        }
        if matches!(
            operation.operation_id,
            "listEnvironmentTemplateReleases" | "getEnvironmentTemplateRelease"
        ) {
            parameters.push(json!({"name":"courseId","in":"query","required":false,"schema":{"type":["string","null"],"format":"uuid"}}));
        }
        if matches!(
            operation.operation_id,
            "listProjectResourceRequests" | "listProjectResourceLeases"
        ) {
            parameters.push(json!({"name":"courseId","in":"query","required":false,"schema":{"type":"string","format":"uuid"}}));
        }
        if operation.operation_id == "getInternalAuthoringPublicationAdmission" {
            parameters.extend([
                json!({"name":"projectId","in":"query","required":true,"schema":{"type":"string","format":"uuid"}}),
                json!({"name":"courseId","in":"query","required":false,"schema":{"type":["string","null"],"format":"uuid"}}),
                json!({"name":"approvalRevision","in":"query","required":true,"schema":{"type":"integer","minimum":1}}),
                json!({"name":"evaluationReleaseId","in":"query","required":true,"schema":{"type":"string","format":"uuid"}}),
            ]);
        }
        if operation.operation_id == "getInternalGeneratedArtifact" {
            parameters.extend([
                json!({"name":"projectId","in":"query","required":true,"schema":{"type":"string","format":"uuid"}}),
                json!({"name":"courseId","in":"query","required":false,"schema":{"type":["string","null"],"format":"uuid"}}),
                json!({"name":"packageId","in":"query","required":true,"schema":{"type":"string","format":"uuid"}}),
                json!({"name":"packageRevision","in":"query","required":true,"schema":{"type":"integer","minimum":1}}),
            ]);
        }
        if operation.operation_id == "resolveEnvironmentWorkConfigurationTarget" {
            parameters.extend([
                json!({"name":"projectId","in":"query","required":true,"schema":{"type":"string","format":"uuid"}}),
                json!({"name":"courseId","in":"query","required":false,"schema":{"type":["string","null"],"format":"uuid"}}),
                json!({"name":"actorId","in":"query","required":true,"schema":{"type":"string","format":"uuid"}}),
                json!({"name":"expectedRevision","in":"query","required":true,"schema":{"type":"integer","minimum":1}}),
            ]);
        }
        parameters.extend(environment_management_parameters(operation.operation_id));
        if operation.operation_id == "streamProjectEvents" {
            let stream_cursor_schema = json!({
                "type":"string",
                "format":"uint64-decimal",
                "pattern":crate::STREAM_SEQUENCE_PATTERN,
                "maxLength":crate::STREAM_SEQUENCE_MAX_LENGTH
            });
            parameters.push(json!({"name":"after","in":"query","required":false,"schema":stream_cursor_schema.clone()}));
            parameters.push(json!({"name":"Last-Event-ID","in":"header","required":false,"schema":stream_cursor_schema}));
        }
        if operation.mutation != MutationContract::None {
            parameters.push(header_parameter("Idempotency-Key", true));
        }
        if operation.mutation == MutationContract::IdempotentRevisioned {
            parameters.push(header_parameter("If-Match", true));
        }
        // The BFF interceptor obtains and attaches these headers for browser
        // mutations. Read operations authenticate through the session cookie
        // but do not participate in the CSRF protocol, so keeping the headers
        // off their generated options prevents callers from fabricating them.
        if operation.security == crate::http::Security::BffSession
            && operation.mutation != MutationContract::None
        {
            parameters.push(json!({"name":"Origin","in":"header","required":true,"schema":{"type":"string","format":"uri"}}));
            parameters.push(json!({"name":"X-CSRF-Token","in":"header","required":true,"schema":{"type":"string","minLength":43,"maxLength":43}}));
        }
        let responses = operation_responses(
            operation.operation_id,
            operation.success_status,
            response_schema(operation.operation_id),
        );
        let mut operation_json = json!({
            "operationId": operation.operation_id,
            "summary": operation.operation_id,
            "description": format!("Permission: {}. Timeout: {} ms. Cancellable: {}. Retryable: {}. v1 permits additive endpoints and optional response fields only.", operation.permission, operation.timeout_milliseconds, operation.cancellable, operation.retryable),
            "security": [match (surface, operation.security) {
                (ApiSurface::Public, crate::http::Security::Oidc) => json!({"oidc": [operation.permission]}),
                (ApiSurface::Public, crate::http::Security::BffSession) => json!({"bffSession": []}),
                (ApiSurface::GatewayInternal, crate::http::Security::ServiceJwt) => json!({"serviceJwt": [operation.permission]}),
                _ => return Err(GenerationError::Contract("operation surface and security metadata disagree".to_owned())),
            }],
            "parameters": parameters,
            "responses": responses,
            "x-labweaver-permission": operation.permission,
            "x-labweaver-idempotency": format!("{:?}", operation.mutation),
            "x-labweaver-timeout-ms": operation.timeout_milliseconds,
            "x-labweaver-cancellable": operation.cancellable,
            "x-labweaver-retryable": operation.retryable
            ,"x-labweaver-problem-content-type":"application/problem+json"
            ,"x-labweaver-errors":["LW_CONTRACT_DOCUMENT_INVALID","LW_ACCESS_DENIED","LW_IDEMPOTENCY_CONFLICT","LW_REVISION_CONFLICT"]
        });
        if operation.operation_id == "resolveEnvironmentOwner" {
            operation_json["x-labweaver-errors"] = json!([
                "LW_CONTRACT_DOCUMENT_INVALID",
                "LW_ENV_OWNER_CALLER_UNTRUSTED",
                "LW_ENV_OWNER_SCOPE_MISMATCH",
                "LW_ENV_OWNER_UNAVAILABLE",
                "LW_ENV_OWNER_RESOLVER_UNAVAILABLE",
                "LW_ENV_OWNER_CLOCK_INVALID"
            ]);
        }
        if matches!(
            operation.operation_id,
            "appendProjectEnvironmentCandidateDecision"
                | "appendProjectEvaluationCandidateDecision"
        ) {
            operation_json["x-labweaver-errors"] = json!([
                "LW_CONTRACT_DOCUMENT_INVALID",
                "LW_ACCESS_DENIED",
                "LW_IDEMPOTENCY_CONFLICT",
                "LW_REVISION_CONFLICT",
                "LW_CANDIDATE_KIND_MISMATCH"
            ]);
        }
        if let Some(errors) = console_operation_errors(operation.operation_id) {
            operation_json["x-labweaver-errors"] = errors;
        }
        if operation.operation_id == "issueConsoleCapability" {
            operation_json["x-labweaver-console-handoff-cookie"] = json!({
                "name": "__Secure-labweaver_console_handoff",
                "secure": true,
                "httpOnly": true,
                "sameSite": "Strict",
                "path": "connectionLocator",
                "maxAgeSeconds": 30,
                "oneTime": true
            });
        }
        operation_json["x-labweaver-allowed-roles"] = json!(operation.allowed_roles);
        operation_json["x-labweaver-scope"] = json!(match operation.scope {
            OperationScopeKind::Global => "global",
            OperationScopeKind::Course => "course",
            OperationScopeKind::Project => "project",
            OperationScopeKind::Environment => "environment",
            OperationScopeKind::Service => "service",
        });
        if let Some(errors) = environment_management_errors(operation.operation_id) {
            operation_json["x-labweaver-errors"] = errors;
        }
        if let Some(schema) = request_schema(operation.operation_id) {
            operation_json["requestBody"] =
                json!({"required":true,"content":{"application/json":{"schema":schema}}});
        }
        let entry = paths
            .entry(operation.path.to_owned())
            .or_insert_with(|| json!({}));
        entry[method] = operation_json;
    }
    add_auth_paths(surface, &mut paths);
    let security_schemes = if surface == ApiSurface::Public {
        json!({
            "oidc": {"type":"oauth2","flows":{"authorizationCode":{"authorizationUrl":"/auth/login","tokenUrl":"/auth/callback","scopes":{}}}},
            "bffSession": {"type":"apiKey","in":"cookie","name":"__Host-labweaver_session"},
            "bearerJwt": {"type":"http","scheme":"bearer","bearerFormat":"JWT"}
        })
    } else {
        json!({"serviceJwt": {"type":"http","scheme":"bearer","bearerFormat":"JWT","description":"Short-lived service-account JWT validated against the configured issuer, audience, signature, expiry, and route permission. Service-to-service transport is protected by TLS."}})
    };
    let value = json!({
        "openapi": "3.1.0",
        "info": {"title": title, "version": "1.0.0", "description": "Generated from the contracts Rust crate. Runtime implementation is outside this artifact."},
        "servers": [{"url": if surface == ApiSurface::Public { "/" } else { "https://gateway-control.internal" }}],
        "paths": paths,
        "components": {
            "securitySchemes": security_schemes,
            "schemas": {
                "ProblemDetails": {
                    "type":"object",
                    "required":["type","title","status","detail","instance","diagnosticCode","requestId","retryable"],
                    "properties":{
                        "type":{"type":"string","format":"uri-reference"},"title":{"type":"string"},"status":{"type":"integer","minimum":400,"maximum":599},"detail":{"type":"string"},"instance":{"type":"string","format":"uri-reference"},"diagnosticCode":{"type":"string","pattern":"^LW_[A-Z0-9_]+$"},"requestId":{"type":"string"},"traceId":{"type":"string"},"retryable":{"type":"boolean"},"violations":{"type":"array","items":{"type":"object","additionalProperties":false,"required":["field","code","message"],"properties":{"field":{"type":"string"},"code":{"type":"string","pattern":"^LW_[A-Z0-9_]+$"},"message":{"type":"string"}}}}
                    }
                },
                "OperationAccepted": {"type":"object","additionalProperties":false,"required":["operationId","revision","statusUrl"],"properties":{"operationId":{"type":"string","format":"uuid"},"revision":{"type":"integer","minimum":1},"statusUrl":{"type":"string","format":"uri-reference"}}}
                ,"AuthSession": contract_ref("auth-session")
                ,"CsrfTokenResponse": contract_ref("csrf-token-response")
                ,"AuthorizationDecisionRequest": contract_ref("authorization-decision-request")
                ,"AuthorizationDecision": contract_ref("authorization-decision")
                ,"EnvironmentWorkConfigurationTarget": contract_ref("environment-work-configuration-target")
                ,"EnvironmentWorkConfigurationTargetQuery": contract_ref("http/environment-work-configuration-target-query")
                 ,"InternalCreateAgentRunRequest": contract_ref("http/internal-create-agent-run-request")
                 ,"InternalAgentRunMutationRequest": contract_ref("http/internal-agent-run-mutation-request")
                 ,"InternalAgentLlmReviewRequest": contract_ref("http/internal-agent-llm-review-request")
                 ,"InternalAgentLlmReviewReceipt": contract_ref("http/internal-agent-llm-review-receipt")
                 ,"AgentLlmReviewQuery": contract_ref("http/agent-llm-review-query")
                 ,"GeneratedArtifactRecord": contract_ref("http/generated-artifact-record")
                ,"InternalApproveWorkConfigurationRequest": contract_ref("http/internal-approve-work-configuration-request")
                ,"InternalAgentBuildCancellationRequest": contract_ref("http/internal-agent-build-cancellation-request")
                ,"InternalAgentBuildCancellationResult": contract_ref("http/internal-agent-build-cancellation-result")
                ,"InternalAgentBuildStatusQuery": contract_ref("http/internal-agent-build-status-query")
                ,"InternalAgentRunOutcome": contract_ref("http/internal-agent-run-outcome")
                ,"InternalImageArtifactResolution": contract_ref("http/internal-image-artifact-resolution")
                ,"AuthoringPublicationAdmissionBinding": contract_ref("http/authoring-publication-admission-binding")
                ,"AuthoringPublicationAdmissionQuery": contract_ref("http/authoring-publication-admission-query")
                ,"WorkConfigurationAdmissionBinding": contract_ref("http/work-configuration-admission-binding")
                ,"WorkConfigurationAdmissionQuery": contract_ref("http/work-configuration-admission-query")
                 ,"ContainerWorkExecutionRequest": contract_ref("http/container-work-execution-request")
                 ,"ContainerWorkExecutionQuery": contract_ref("http/container-work-execution-query")
                 ,"ContainerWorkExecutionReceipt": contract_ref("http/container-work-execution-receipt")
                 ,"AgentWorkExecutionIntentMetadata": contract_ref("http/agent-work-execution-intent-metadata")
                 ,"AgentWorkExecutionIntentQuery": contract_ref("http/agent-work-execution-intent-query")
                ,"EvaluationRelease": contract_ref("evaluation-release")
                ,"EvaluationRun": contract_ref("evaluation-run")
                ,"StudentEvaluationResult": contract_ref("student-evaluation-result")
                ,"InternalPublishEvaluationReleaseRequest": contract_ref("http/internal-publish-evaluation-release-request")
                ,"InternalWithdrawEvaluationReleaseRequest": contract_ref("http/internal-withdraw-evaluation-release-request")
                ,"InternalCreateEvaluationRunRequest": contract_ref("http/internal-create-evaluation-run-request")
                ,"InternalEvaluationRunMutationRequest": contract_ref("http/internal-evaluation-run-mutation-request")
                ,"InternalCompleteEvaluationStepRequest": contract_ref("http/internal-complete-evaluation-step-request")
                ,"EnvironmentExecutionBindingRequest": contract_ref("internal/environment-execution-binding-request")
                ,"EnvironmentExecutionBinding": contract_ref("internal/environment-execution-binding")
            },
            "responses": {"Problem": {"description":"RFC 9457 problem detail","content":{"application/problem+json":{"schema":{"$ref":"#/components/schemas/ProblemDetails"}}}}}
        }
    });
    let document: utoipa::openapi::OpenApi = serde_json::from_value(value)?;
    Ok(serde_json::to_value(document)?)
}

fn add_auth_paths(surface: ApiSurface, paths: &mut BTreeMap<String, Value>) {
    match surface {
        ApiSurface::Public => {
            paths.insert(
                "/auth/login".to_owned(),
                json!({"get":{"operationId":"beginOidcLogin","summary":"Begin OIDC Authorization Code + PKCE login","security":[],"parameters":[{"name":"return_to","in":"query","required":false,"schema":{"type":"string"}}],"responses":{"302":{"description":"Redirect to the configured OIDC provider"},"503":{"$ref":"#/components/responses/Problem"}}}}),
            );
            paths.insert(
                "/auth/callback".to_owned(),
                json!({"get":{"operationId":"completeOidcLogin","summary":"Complete the one-time OIDC callback","security":[],"parameters":[{"name":"code","in":"query","required":true,"schema":{"type":"string","minLength":1}},{"name":"state","in":"query","required":true,"schema":{"type":"string","minLength":1}}],"responses":{"302":{"description":"Session established and redirected to the allowlisted return URL"},"401":{"$ref":"#/components/responses/Problem"},"503":{"$ref":"#/components/responses/Problem"}}}}),
            );
            paths.insert(
                "/auth/backchannel-logout".to_owned(),
                json!({"post":{"operationId":"consumeOidcBackchannelLogout","summary":"Consume a signed, replay-protected OIDC back-channel logout token","security":[],"requestBody":{"required":true,"content":{"application/x-www-form-urlencoded":{"schema":{"type":"object","additionalProperties":false,"required":["logout_token"],"properties":{"logout_token":{"type":"string","minLength":1}}}}}},"responses":{"204":{"description":"Matching sessions revoked"},"403":{"$ref":"#/components/responses/Problem"},"503":{"$ref":"#/components/responses/Problem"}}}}),
            );
            paths.insert(
                "/auth/logout".to_owned(),
                json!({"post":{"operationId":"logoutBrowserSession","summary":"Revoke the BFF session and begin provider logout","security":[{"bffSession":[]}],"parameters":[{"name":"Origin","in":"header","required":true,"schema":{"type":"string","format":"uri"}},{"name":"X-CSRF-Token","in":"header","required":true,"schema":{"type":"string","minLength":43,"maxLength":43}}],"responses":{"302":{"description":"Session revoked and redirected to provider logout"},"403":{"$ref":"#/components/responses/Problem"},"503":{"$ref":"#/components/responses/Problem"}}}}),
            );
            paths.insert(
                "/api/v1/auth/session".to_owned(),
                json!({"get":{"operationId":"getAuthSession","summary":"Return the safe actor and current authoritative scopes","security":[{"bffSession":[]},{"bearerJwt":[]}],"responses":{"200":{"description":"Current authentication session","content":{"application/json":{"schema":{"$ref":"#/components/schemas/AuthSession"}}}},"401":{"$ref":"#/components/responses/Problem"},"503":{"$ref":"#/components/responses/Problem"}}}}),
            );
            paths.insert(
                "/api/v1/auth/csrf".to_owned(),
                json!({"get":{"operationId":"issueCsrfToken","summary":"Issue a synchronizer token for the current BFF session","security":[{"bffSession":[]}],"responses":{"200":{"description":"Short-lived synchronizer token","content":{"application/json":{"schema":{"$ref":"#/components/schemas/CsrfTokenResponse"}}}},"401":{"$ref":"#/components/responses/Problem"},"503":{"$ref":"#/components/responses/Problem"}}}}),
            );
        }
        ApiSurface::GatewayInternal => {
            paths.insert(
                "/internal/v1/auth/decision".to_owned(),
                json!({"post":{"operationId":"decideAuthorization","summary":"Evaluate an actor session and exact resource scope for a service JWT caller","security": internal_security("authorization:decide"),"requestBody":{"required":true,"content":{"application/json":{"schema":{"$ref":"#/components/schemas/AuthorizationDecisionRequest"}}}},"responses":{"200":{"description":"Expiry-bounded authorization decision","content":{"application/json":{"schema":{"$ref":"#/components/schemas/AuthorizationDecision"}}}},"403":{"$ref":"#/components/responses/Problem"},"503":{"$ref":"#/components/responses/Problem"}}}}),
            );
            paths.insert(
                "/internal/v1/agent-runs".to_owned(),
                json!({"post":{"operationId":"createInternalAgentRun","summary":"Reserve an Agent-owned run from a Control-verified immutable package and policy","security": internal_security("agent_run:create"),"parameters":[{"name":"Idempotency-Key","in":"header","required":true,"schema":{"type":"string","minLength":16,"maxLength":128}}],"requestBody":{"required":true,"content":{"application/json":{"schema":{"$ref":"#/components/schemas/InternalCreateAgentRunRequest"}}}},"responses":{"202":{"description":"AgentRun accepted","content":{"application/json":{"schema":{"$ref":"./agent-run.schema.json"}}}},"409":{"$ref":"#/components/responses/Problem"},"503":{"$ref":"#/components/responses/Problem"}}}}),
            );
            paths.insert(
                "/internal/v1/agent-runs/{runId}/work-configuration/approve".to_owned(),
                json!({"post":{"operationId":"approveInternalWorkConfigurationRun","summary":"Bind one exact Work configuration preauthorization to an AgentRun awaiting approval","security": internal_security("agent_run:approve"),"parameters":[{"name":"runId","in":"path","required":true,"schema":{"type":"string","format":"uuid"}},{"name":"Idempotency-Key","in":"header","required":true,"schema":{"type":"string","minLength":16,"maxLength":128}}],"requestBody":{"required":true,"content":{"application/json":{"schema":{"$ref":"#/components/schemas/InternalApproveWorkConfigurationRequest"}}}},"responses":{"202":{"description":"AgentRun accepted for Work configuration execution","content":{"application/json":{"schema":{"$ref":"./agent-run.schema.json"}}}},"409":{"$ref":"#/components/responses/Problem"},"422":{"$ref":"#/components/responses/Problem"},"503":{"$ref":"#/components/responses/Problem"}}}}),
            );
            paths.insert(
                "/internal/v1/agent-runs/{runId}".to_owned(),
                json!({"get":{"operationId":"getInternalAgentRun","summary":"Read the authoritative Agent-owned run","security": internal_security("agent_run:read"),"parameters":[{"name":"runId","in":"path","required":true,"schema":{"type":"string","format":"uuid"}}],"responses":{"200":{"description":"Authoritative AgentRun","content":{"application/json":{"schema":{"$ref":"./agent-run.schema.json"}}}},"404":{"$ref":"#/components/responses/Problem"},"503":{"$ref":"#/components/responses/Problem"}}}}),
            );
            paths.insert(
                "/internal/v1/agent-runs/{runId}/cancel".to_owned(),
                json!({"post":{"operationId":"cancelInternalAgentRun","summary":"Request cancellation at an exact AgentRun revision","security": internal_security("agent_run:cancel"),"parameters":[{"name":"runId","in":"path","required":true,"schema":{"type":"string","format":"uuid"}},{"name":"Idempotency-Key","in":"header","required":true,"schema":{"type":"string","minLength":16,"maxLength":128}}],"requestBody":{"required":true,"content":{"application/json":{"schema":{"$ref":"#/components/schemas/InternalAgentRunMutationRequest"}}}},"responses":{"200":{"description":"Updated authoritative AgentRun","content":{"application/json":{"schema":{"$ref":"./agent-run.schema.json"}}}},"409":{"$ref":"#/components/responses/Problem"},"503":{"$ref":"#/components/responses/Problem"}}}}),
            );
            paths.insert(
                "/internal/v1/build-requests/{buildRequestId}/cancel".to_owned(),
                json!({"post":{"operationId":"cancelInternalAgentBuild","summary":"Request one actor-attributed build cancellation at an exact course, state and revision","security": internal_security("agent_build:cancel"),"parameters":[{"name":"buildRequestId","in":"path","required":true,"schema":{"type":"string","format":"uuid"}},{"name":"Idempotency-Key","in":"header","required":true,"schema":{"type":"string","minLength":16,"maxLength":128}}],"requestBody":{"required":true,"content":{"application/json":{"schema":{"$ref":"#/components/schemas/InternalAgentBuildCancellationRequest"}}}},"responses":{"202":{"description":"Durable cancellation requested","content":{"application/json":{"schema":{"$ref":"#/components/schemas/InternalAgentBuildCancellationResult"}}}},"403":{"$ref":"#/components/responses/Problem"},"409":{"$ref":"#/components/responses/Problem"},"422":{"$ref":"#/components/responses/Problem"},"503":{"$ref":"#/components/responses/Problem"}}}}),
            );
            paths.insert(
                "/internal/v1/llm-reviews".to_owned(),
                json!({"post":{"operationId":"createInternalAgentLlmReview","summary":"Queue one bounded advisory Agent LLM review","security": internal_security("agent.llm_review.create"),"parameters":[{"name":"Idempotency-Key","in":"header","required":true,"schema":{"type":"string","minLength":16,"maxLength":128}}],"requestBody":{"required":true,"content":{"application/json":{"schema":{"$ref":"#/components/schemas/InternalAgentLlmReviewRequest"}}}},"responses":{"202":{"description":"LLM review queued","content":{"application/json":{"schema":{"$ref":"#/components/schemas/InternalAgentLlmReviewReceipt"}}}},"409":{"$ref":"#/components/responses/Problem"},"422":{"$ref":"#/components/responses/Problem"},"503":{"$ref":"#/components/responses/Problem"}}}}),
            );
            paths.insert(
                "/internal/v1/llm-reviews/{taskRunId}".to_owned(),
                json!({"get":{"operationId":"getInternalAgentLlmReview","summary":"Read one Agent LLM review receipt","security": internal_security("agent.llm_review.read"),"parameters":[{"name":"taskRunId","in":"path","required":true,"schema":{"type":"string","format":"uuid"}},{"name":"projectId","in":"query","required":true,"schema":{"type":"string","format":"uuid"}},{"name":"courseId","in":"query","required":false,"schema":{"type":["string","null"],"format":"uuid"}}],"responses":{"200":{"description":"LLM review receipt","content":{"application/json":{"schema":{"$ref":"#/components/schemas/InternalAgentLlmReviewReceipt"}}}},"403":{"$ref":"#/components/responses/Problem"},"404":{"$ref":"#/components/responses/Problem"},"503":{"$ref":"#/components/responses/Problem"}}}}),
            );
            paths.insert(
                "/internal/v1/llm-reviews/{taskRunId}/cancel".to_owned(),
                json!({"post":{"operationId":"cancelInternalAgentLlmReview","summary":"Persist cancellation for one Agent LLM review","security": internal_security("agent.llm_review.cancel"),"parameters":[{"name":"taskRunId","in":"path","required":true,"schema":{"type":"string","format":"uuid"}},{"name":"projectId","in":"query","required":true,"schema":{"type":"string","format":"uuid"}},{"name":"courseId","in":"query","required":false,"schema":{"type":["string","null"],"format":"uuid"}},{"name":"Idempotency-Key","in":"header","required":true,"schema":{"type":"string","minLength":16,"maxLength":128}}],"responses":{"200":{"description":"Updated LLM review receipt","content":{"application/json":{"schema":{"$ref":"#/components/schemas/InternalAgentLlmReviewReceipt"}}}},"403":{"$ref":"#/components/responses/Problem"},"404":{"$ref":"#/components/responses/Problem"},"409":{"$ref":"#/components/responses/Problem"},"503":{"$ref":"#/components/responses/Problem"}}}}),
            );
            paths.insert(
                "/internal/v1/build-requests/{buildRequestId}".to_owned(),
                json!({"get":{"operationId":"getInternalAgentBuild","summary":"Read the Agent-owned build state and revision for an exact course","security": internal_security("agent_build:read"),"parameters":[{"name":"buildRequestId","in":"path","required":true,"schema":{"type":"string","format":"uuid"}},{"name":"courseId","in":"query","required":true,"schema":{"type":"string","format":"uuid"}}],"responses":{"200":{"description":"Authoritative build status","content":{"application/json":{"schema":{"$ref":"#/components/schemas/InternalAgentBuildCancellationResult"}}}},"403":{"$ref":"#/components/responses/Problem"},"404":{"$ref":"#/components/responses/Problem"},"503":{"$ref":"#/components/responses/Problem"}}}}),
            );
            paths.insert(
                "/internal/v1/agent-runs/{runId}/tracks/{track}/retry".to_owned(),
                json!({"post":{"operationId":"retryInternalAgentRunTrack","summary":"Retry one failed AgentRun track at an exact revision","security": internal_security("agent_run:retry"),"parameters":[{"name":"runId","in":"path","required":true,"schema":{"type":"string","format":"uuid"}},{"name":"track","in":"path","required":true,"schema":{"type":"string","enum":["environment","evaluation"]}},{"name":"Idempotency-Key","in":"header","required":true,"schema":{"type":"string","minLength":16,"maxLength":128}}],"requestBody":{"required":true,"content":{"application/json":{"schema":{"$ref":"#/components/schemas/InternalAgentRunMutationRequest"}}}},"responses":{"200":{"description":"Updated authoritative AgentRun","content":{"application/json":{"schema":{"$ref":"./agent-run.schema.json"}}}},"409":{"$ref":"#/components/responses/Problem"},"503":{"$ref":"#/components/responses/Problem"}}}}),
            );
            paths.insert(
                "/internal/v1/agent-runs/{runId}/outcome".to_owned(),
                json!({"get":{"operationId":"getInternalAgentRunOutcome","summary":"Resolve the authoritative run and retained candidate checkpoints","security": internal_security("agent_run:outcome"),"parameters":[{"name":"runId","in":"path","required":true,"schema":{"type":"string","format":"uuid"}}],"responses":{"200":{"description":"Authoritative outcome","content":{"application/json":{"schema":{"$ref":"#/components/schemas/InternalAgentRunOutcome"}}}},"404":{"$ref":"#/components/responses/Problem"},"503":{"$ref":"#/components/responses/Problem"}}}}),
            );
            paths.insert(
                "/internal/v1/agent-runs/{runId}/work-execution-intent".to_owned(),
                json!({"get":{"operationId":"getInternalAgentWorkExecutionIntent","summary":"Read persisted VM Work execution metadata","security": internal_security("agent.control.invoke"),"parameters":[{"name":"runId","in":"path","required":true,"schema":{"type":"string","format":"uuid"}},{"name":"projectId","in":"query","required":true,"schema":{"type":"string","format":"uuid"}},{"name":"courseId","in":"query","required":false,"schema":{"type":["string","null"],"format":"uuid"}},{"name":"executionId","in":"query","required":true,"schema":{"type":"string","format":"uuid"}}],"responses":{"200":{"description":"Persisted VM Work execution metadata","content":{"application/json":{"schema":{"$ref":"#/components/schemas/AgentWorkExecutionIntentMetadata"}}}},"403":{"$ref":"#/components/responses/Problem"},"404":{"$ref":"#/components/responses/Problem"},"409":{"$ref":"#/components/responses/Problem"},"503":{"$ref":"#/components/responses/Problem"}}}}),
            );
            paths.insert(
                "/internal/v1/image-artifacts/{artifactId}".to_owned(),
                json!({"get":{"operationId":"resolveInternalImageArtifact","summary":"Resolve one Agent-owned verified artifact identity","security": internal_security("agent_artifact:read"),"parameters":[{"name":"artifactId","in":"path","required":true,"schema":{"type":"string","format":"uuid"}}],"responses":{"200":{"description":"Authoritative artifact resolution","content":{"application/json":{"schema":{"$ref":"#/components/schemas/InternalImageArtifactResolution"}}}},"404":{"$ref":"#/components/responses/Problem"},"503":{"$ref":"#/components/responses/Problem"}}}}),
            );
            paths.insert(
                "/internal/v1/evaluation-releases".to_owned(),
                json!({
                    "post":{"operationId":"publishInternalEvaluationRelease","summary":"Publish one Control-approved immutable EvaluationSpec release","security": internal_security("evaluation_release:publish"),"parameters":[{"name":"Idempotency-Key","in":"header","required":true,"schema":{"type":"string","minLength":16,"maxLength":128}}],"requestBody":{"required":true,"content":{"application/json":{"schema":{"$ref":"#/components/schemas/InternalPublishEvaluationReleaseRequest"}}}},"responses":{"201":{"description":"EvaluationRelease created","content":{"application/json":{"schema":{"$ref":"#/components/schemas/EvaluationRelease"}}}},"409":{"$ref":"#/components/responses/Problem"},"422":{"$ref":"#/components/responses/Problem"},"503":{"$ref":"#/components/responses/Problem"}}},
                    "get":{"operationId":"listInternalEvaluationReleases","summary":"List authoritative Evaluation releases for one course","security": internal_security("evaluation_release:read"),"parameters":[{"name":"x-labweaver-course-id","in":"header","required":true,"schema":{"type":"string","format":"uuid"}},{"name":"limit","in":"query","required":false,"schema":{"type":"integer","minimum":1,"maximum":100,"default":50}}],"responses":{"200":{"description":"Evaluation releases","content":{"application/json":{"schema":{"type":"array","items":{"$ref":"#/components/schemas/EvaluationRelease"}}}}},"403":{"$ref":"#/components/responses/Problem"},"503":{"$ref":"#/components/responses/Problem"}}}
                }),
            );
            paths.insert(
                "/internal/v1/evaluation-releases/{releaseId}".to_owned(),
                json!({"get":{"operationId":"getInternalEvaluationRelease","summary":"Read one authoritative Evaluation release","security": internal_security("evaluation_release:read_one"),"parameters":[{"name":"releaseId","in":"path","required":true,"schema":{"type":"string","format":"uuid"}}],"responses":{"200":{"description":"Authoritative EvaluationRelease","content":{"application/json":{"schema":{"$ref":"#/components/schemas/EvaluationRelease"}}}},"404":{"$ref":"#/components/responses/Problem"},"503":{"$ref":"#/components/responses/Problem"}}}}),
            );
            paths.insert(
                "/internal/v1/evaluation-releases/{releaseId}/withdraw".to_owned(),
                json!({"post":{"operationId":"withdrawInternalEvaluationRelease","summary":"Withdraw one Evaluation release at an exact revision","security": internal_security("evaluation_release:withdraw"),"parameters":[{"name":"releaseId","in":"path","required":true,"schema":{"type":"string","format":"uuid"}},{"name":"Idempotency-Key","in":"header","required":true,"schema":{"type":"string","minLength":16,"maxLength":128}},{"name":"If-Match","in":"header","required":true,"schema":{"type":"string"}}],"requestBody":{"required":true,"content":{"application/json":{"schema":{"$ref":"#/components/schemas/InternalWithdrawEvaluationReleaseRequest"}}}},"responses":{"200":{"description":"Withdrawn EvaluationRelease","content":{"application/json":{"schema":{"$ref":"#/components/schemas/EvaluationRelease"}}}},"409":{"$ref":"#/components/responses/Problem"},"412":{"$ref":"#/components/responses/Problem"},"503":{"$ref":"#/components/responses/Problem"}}}}),
            );
            paths.insert(
                "/internal/v1/evaluation-runs".to_owned(),
                json!({"post":{"operationId":"createInternalEvaluationRun","summary":"Reserve one EvaluationRun from an active release and immutable FrozenSubmission","security": internal_security("evaluation_run:create"),"parameters":[{"name":"Idempotency-Key","in":"header","required":true,"schema":{"type":"string","minLength":16,"maxLength":128}}],"requestBody":{"required":true,"content":{"application/json":{"schema":{"$ref":"#/components/schemas/InternalCreateEvaluationRunRequest"}}}},"responses":{"202":{"description":"EvaluationRun accepted","content":{"application/json":{"schema":{"$ref":"#/components/schemas/EvaluationRun"}}}},"409":{"$ref":"#/components/responses/Problem"},"422":{"$ref":"#/components/responses/Problem"},"503":{"$ref":"#/components/responses/Problem"}}}}),
            );
            paths.insert(
                "/internal/v1/evaluation-runs/{runId}".to_owned(),
                json!({"get":{"operationId":"getInternalEvaluationRun","summary":"Read one authoritative EvaluationRun","security": internal_security("evaluation_run:read"),"parameters":[{"name":"runId","in":"path","required":true,"schema":{"type":"string","format":"uuid"}}],"responses":{"200":{"description":"Authoritative EvaluationRun","content":{"application/json":{"schema":{"$ref":"#/components/schemas/EvaluationRun"}}}},"404":{"$ref":"#/components/responses/Problem"},"503":{"$ref":"#/components/responses/Problem"}}}}),
            );
            paths.insert(
                "/internal/v1/evaluation-runs/{runId}/cancel".to_owned(),
                json!({"post":{"operationId":"cancelInternalEvaluationRun","summary":"Request cancellation at an exact EvaluationRun revision","security": internal_security("evaluation_run:cancel"),"parameters":[{"name":"runId","in":"path","required":true,"schema":{"type":"string","format":"uuid"}},{"name":"Idempotency-Key","in":"header","required":true,"schema":{"type":"string","minLength":16,"maxLength":128}}],"requestBody":{"required":true,"content":{"application/json":{"schema":{"$ref":"#/components/schemas/InternalEvaluationRunMutationRequest"}}}},"responses":{"200":{"description":"Updated authoritative EvaluationRun","content":{"application/json":{"schema":{"$ref":"#/components/schemas/EvaluationRun"}}}},"409":{"$ref":"#/components/responses/Problem"},"422":{"$ref":"#/components/responses/Problem"},"503":{"$ref":"#/components/responses/Problem"}}}}),
            );
            paths.insert(
                "/internal/v1/evaluation-runs/{runId}/steps/{stepRunId}/retry".to_owned(),
                json!({"post":{"operationId":"retryInternalEvaluationStep","summary":"Retry one failed or cancelled StepRun at an exact EvaluationRun revision","security": internal_security("evaluation_step:retry"),"parameters":[{"name":"runId","in":"path","required":true,"schema":{"type":"string","format":"uuid"}},{"name":"stepRunId","in":"path","required":true,"schema":{"type":"string","format":"uuid"}},{"name":"Idempotency-Key","in":"header","required":true,"schema":{"type":"string","minLength":16,"maxLength":128}}],"requestBody":{"required":true,"content":{"application/json":{"schema":{"$ref":"#/components/schemas/InternalEvaluationRunMutationRequest"}}}},"responses":{"200":{"description":"Updated authoritative EvaluationRun","content":{"application/json":{"schema":{"$ref":"#/components/schemas/EvaluationRun"}}}},"409":{"$ref":"#/components/responses/Problem"},"422":{"$ref":"#/components/responses/Problem"},"503":{"$ref":"#/components/responses/Problem"}}}}),
            );
            paths.insert(
                "/internal/v1/evaluation-runs/{runId}/steps/{stepRunId}/cleanup".to_owned(),
                json!({"post":{"operationId":"verifyInternalEvaluationStepCleanup","summary":"Verify cleanup for one failed or cancelled StepRun at an exact EvaluationRun revision","security": internal_security("evaluation_step:cleanup"),"parameters":[{"name":"runId","in":"path","required":true,"schema":{"type":"string","format":"uuid"}},{"name":"stepRunId","in":"path","required":true,"schema":{"type":"string","format":"uuid"}},{"name":"Idempotency-Key","in":"header","required":true,"schema":{"type":"string","minLength":16,"maxLength":128}}],"requestBody":{"required":true,"content":{"application/json":{"schema":{"$ref":"#/components/schemas/InternalEvaluationRunMutationRequest"}}}},"responses":{"200":{"description":"Updated authoritative EvaluationRun","content":{"application/json":{"schema":{"$ref":"#/components/schemas/EvaluationRun"}}}},"409":{"$ref":"#/components/responses/Problem"},"422":{"$ref":"#/components/responses/Problem"},"503":{"$ref":"#/components/responses/Problem"}}}}),
            );
            paths.insert(
                "/internal/v1/evaluation-runs/{runId}/steps/{stepRunId}/complete".to_owned(),
                json!({"post":{"operationId":"completeInternalEvaluationStep","summary":"Complete one fenced StepRun attempt with hash-only evidence","security": internal_security("evaluation_step:complete"),"requestBody":{"required":true,"content":{"application/json":{"schema":{"$ref":"#/components/schemas/InternalCompleteEvaluationStepRequest"}}}},"parameters":[{"name":"runId","in":"path","required":true,"schema":{"type":"string","format":"uuid"}},{"name":"stepRunId","in":"path","required":true,"schema":{"type":"string","format":"uuid"}}],"responses":{"200":{"description":"Updated authoritative EvaluationRun","content":{"application/json":{"schema":{"$ref":"#/components/schemas/EvaluationRun"}}}},"409":{"$ref":"#/components/responses/Problem"},"422":{"$ref":"#/components/responses/Problem"},"503":{"$ref":"#/components/responses/Problem"}}}}),
            );
        }
    }
}

fn path_parameters(path: &str) -> Vec<Value> {
    path.split('{')
        .skip(1)
        .filter_map(|fragment| {
            fragment.split_once('}').map(|(name, _)| {
        json!({"name":name,"in":"path","required":true,"schema":{"type":"string","minLength":1}})
    })
        })
        .collect()
}

fn header_parameter(name: &str, required: bool) -> Value {
    json!({"name":name,"in":"header","required":required,"schema":{"type":"string"}})
}

fn environment_management_parameters(operation_id: &str) -> Vec<Value> {
    let cursor = || json!({"name":"cursor","in":"query","required":false,"schema":{"type":"string","minLength":1,"maxLength":512,"pattern":"^[A-Za-z0-9_.~-]+$"}});
    let limit = || json!({"name":"limit","in":"query","required":false,"schema":{"type":"integer","minimum":1,"maximum":100,"default":50}});
    match operation_id {
        "listEnvironments" => vec![
            json!({"name":"projectId","in":"query","required":true,"schema":{"type":"string","format":"uuid"}}),
            json!({"name":"courseId","in":"query","required":false,"schema":{"type":"string","format":"uuid"}}),
            json!({"name":"runtimeKind","in":"query","required":false,"schema":{"type":"string","enum":["container","virtual_machine"]}}),
            json!({"name":"class","in":"query","required":false,"schema":{"type":"string","enum":["experiment","work"]}}),
            json!({"name":"desiredState","in":"query","required":false,"schema":{"type":"string","enum":["running","stopped","deleted"]}}),
            json!({"name":"observedState","in":"query","required":false,"schema":{"type":"string","enum":["requested","validating","building","provisioning","ready","stopping","stopped","updating","expiring","deleting","deleted","failed"]}}),
            json!({"name":"releaseId","in":"query","required":false,"schema":{"type":"string","format":"uuid"}}),
            cursor(),
            limit(),
        ],
        "listEnvironmentOperations" => vec![
            json!({"name":"kind","in":"query","required":false,"schema":{"type":"string","enum":["create","start","stop","restart","reset","retry","cancel","recover","expire","delete","cleanup","freeze"]}}),
            json!({"name":"state","in":"query","required":false,"schema":{"type":"string","enum":["accepted","running","cancelling","succeeded","failed","cancelled"]}}),
            cursor(),
            limit(),
        ],
        "listEnvironmentAccessGrants" => vec![
            json!({"name":"state","in":"query","required":false,"schema":{"type":"string","enum":["requested","active","denied","expired","revoked"]}}),
            json!({"name":"endpointId","in":"query","required":false,"schema":{"type":"string","format":"uuid"}}),
            json!({"name":"includeTerminal","in":"query","required":false,"schema":{"type":"boolean","default":false}}),
            cursor(),
            limit(),
        ],
        _ => Vec::new(),
    }
}

fn environment_management_errors(operation_id: &str) -> Option<Value> {
    let errors = match operation_id {
        "listEnvironments" | "listEnvironmentOperations" => vec![
            "LW_CONTRACT_DOCUMENT_INVALID",
            "LW_HTTP_UNAUTHENTICATED",
            "LW_ACCESS_DENIED",
            "LW_ENVIRONMENT_SCOPE_REQUIRED",
            "LW_ENVIRONMENT_SCOPE_MISMATCH",
            "LW_ENVIRONMENT_CURSOR_INVALID",
            "LW_ENVIRONMENT_CURSOR_EXPIRED",
            "LW_ENVIRONMENT_PROVIDER_UNAVAILABLE",
            "LW_HTTP_RATE_LIMITED",
            "LW_HTTP_SERVICE_UNAVAILABLE",
            "LW_HTTP_INTERNAL",
        ],
        "streamProjectEvents" => vec![
            "LW_CONTRACT_DOCUMENT_INVALID",
            "LW_HTTP_UNAUTHENTICATED",
            "LW_ACCESS_DENIED",
            "LW_SSE_CURSOR_CONFLICT",
            "LW_SSE_CURSOR_EXPIRED",
            "LW_SSE_CURSOR_GAP",
            "LW_HTTP_RATE_LIMITED",
            "LW_HTTP_SERVICE_UNAVAILABLE",
            "LW_HTTP_INTERNAL",
        ],
        "getEnvironmentOperation" => vec![
            "LW_CONTRACT_DOCUMENT_INVALID",
            "LW_HTTP_UNAUTHENTICATED",
            "LW_ACCESS_DENIED",
            "LW_ENVIRONMENT_SCOPE_MISMATCH",
            "LW_ENVIRONMENT_OPERATION_NOT_FOUND",
            "LW_ENVIRONMENT_OPERATION_STATE_CONFLICT",
            "LW_ENVIRONMENT_PROVIDER_UNAVAILABLE",
            "LW_HTTP_RATE_LIMITED",
            "LW_HTTP_SERVICE_UNAVAILABLE",
            "LW_HTTP_INTERNAL",
        ],
        "listEnvironmentAccessGrants" => vec![
            "LW_CONTRACT_DOCUMENT_INVALID",
            "LW_HTTP_UNAUTHENTICATED",
            "LW_ACCESS_DENIED",
            "LW_ACCESS_GRANT_CURSOR_INVALID",
            "LW_ACCESS_GRANT_CURSOR_EXPIRED",
            "LW_ACCESS_GRANT_SNAPSHOT_CONFLICT",
            "LW_HTTP_RATE_LIMITED",
            "LW_HTTP_SERVICE_UNAVAILABLE",
            "LW_HTTP_INTERNAL",
        ],
        _ => return None,
    };
    Some(json!(errors))
}

fn console_operation_errors(operation_id: &str) -> Option<Value> {
    let errors = match operation_id {
        "listConsoleCapabilities" => vec![
            "LW_HTTP_UNAUTHENTICATED",
            "LW_CONSOLE_CAPABILITY_DENIED",
            "LW_CONSOLE_CAPABILITY_EXPIRED",
            "LW_CONSOLE_REVISION_CONFLICT",
            "LW_CONSOLE_LEASE_INVALID",
            "LW_CONSOLE_ENVIRONMENT_NOT_READY",
            "LW_CONSOLE_UPSTREAM_UNAVAILABLE",
        ],
        "issueConsoleCapability" => vec![
            "LW_HTTP_UNAUTHENTICATED",
            "LW_CONSOLE_CAPABILITY_DENIED",
            "LW_CONSOLE_CAPABILITY_EXPIRED",
            "LW_CONSOLE_REVISION_CONFLICT",
            "LW_CONSOLE_LEASE_INVALID",
            "LW_CONSOLE_ENVIRONMENT_NOT_READY",
            "LW_CONSOLE_SUBPROTOCOL_MISMATCH",
            "LW_CONSOLE_UPSTREAM_UNAVAILABLE",
        ],
        _ => return None,
    };
    Some(json!(errors))
}

fn contract_ref(name: &str) -> Value {
    json!({"$ref": format!("../contracts/v1/{name}.schema.json")})
}

fn request_schema(operation_id: &str) -> Option<Value> {
    let name = match operation_id {
        "createProject" => "http/create-project-request",
        "updateProject" => "http/update-project-request",
        "addProjectMembership" => "http/add-project-membership-request",
        "removeProjectMembership" => "http/remove-project-membership-request",
        "createProjectProblemPackageUpload" => "http/create-problem-package-upload-request",
        "completeProjectProblemPackageUpload" => "http/complete-problem-package-upload-request",
        "createProjectLlmPolicy" => "project-llm-egress-policy",
        "createProjectAgentRun" => "http/create-agent-run-request",
        "createInternalAgentLlmReview" => "http/internal-agent-llm-review-request",
        "createProjectWorkConfigurationRun" => "http/create-work-configuration-run-request",
        "approveProjectWorkConfigurationRun" => "http/approve-work-configuration-request",
        "completeProjectAuthoringApproval" => "http/complete-authoring-approval-request",
        "createTaskResourceRequest" => "http/internal-create-task-resource-request",
        "appendProjectEnvironmentCandidateDecision"
        | "appendProjectEvaluationCandidateDecision" => "http/candidate-decision-request",
        "createEnvironmentTemplateRelease" => "http/create-environment-template-release-request",
        "createEvaluationRelease" => "http/create-evaluation-release-request",
        "withdrawEvaluationRelease" => "http/withdraw-evaluation-release-request",
        "withdrawEnvironmentTemplateRelease" => {
            "http/withdraw-environment-template-release-request"
        }
        "createEnvironment" => "http/create-environment-request",
        "resetEnvironment" => "http/reset-environment-request",
        "createResourceRequest" | "createProjectResourceRequest" => "http/create-resource-request",
        "createResourceGpuCatalogEntry" => "gpu-catalog-entry",
        "createResourceRate" => "http/create-resource-rate-request",
        "upsertProjectResourceBudget" => "http/upsert-resource-budget-request",
        "createProjectResourceChargeAdjustment" => "http/create-resource-adjustment-request",
        "recordResourceUsage" | "recordInternalResourceUsage" => {
            "http/record-resource-usage-request"
        }
        "acknowledgeTaskResource" => "http/acknowledge-task-resource-request",
        "releaseTaskResource" => "http/release-task-resource-request",
        "cancelTaskResource"
        | "cancelResourceRequest"
        | "rejectResourceRequest"
        | "retryResourceRequest"
        | "revokeResourceLease" => "http/resource-request-mutation",
        "approveResourceRequest" | "resizeAndApproveResourceRequest" => {
            "http/approve-resource-request"
        }
        "renewResourceLease" => "http/renew-resource-lease",
        "freezeSubmission" => "http/freeze-submission-request",
        "createSshPublicKey" => "http/create-ssh-public-key-request",
        "createAccessGrant" => "http/create-access-grant-request",
        "revokeAccessGrant" => "http/revoke-access-grant-request",
        "renewAccessGrant" => "http/renew-access-grant-request",
        "issueConsoleCapability" => "http/issue-console-capability-request",
        "authorizeSsh" => "ssh-authorization-request",
        "resolveEnvironmentOwner" => "http/environment-owner-resolution-request",
        "resolveEndpointEligibility" => "http/environment-endpoint-eligibility-request",
        "resolveEnvironmentEvaluationExecutionBinding"
        | "resolveEnvironmentWorkExecutionBinding" => {
            "internal/environment-execution-binding-request"
        }
        "createGatewaySession" => "create-gateway-session-request",
        "heartbeatGatewaySession" => "heartbeat-gateway-session-request",
        "closeGatewaySession" => "close-gateway-session-request",
        _ => return None,
    };
    Some(contract_ref(name))
}

fn response_schema(operation_id: &str) -> Option<Value> {
    let schema = match operation_id {
        "createProject" | "getProject" | "updateProject" | "archiveProject" => {
            contract_ref("project")
        }
        "listProjects" => json!({
            "type":"array",
            "items":contract_ref("project")
        }),
        "listProjectMemberships" => json!({
            "type":"array",
            "items":contract_ref("project-membership")
        }),
        "addProjectMembership" | "removeProjectMembership" => contract_ref("project-membership"),
        "createProjectProblemPackageUpload" => contract_ref("http/problem-package-upload-session"),
        "getProjectProblemPackage" | "completeProjectProblemPackageUpload" => {
            contract_ref("problem-package")
        }
        "createProjectLlmPolicy"
        | "getActiveProjectLlmPolicy"
        | "getInternalProjectLlmEgressPolicy" => contract_ref("project-llm-egress-policy"),
        "createProjectAgentRun"
        | "createProjectWorkConfigurationRun"
        | "approveProjectWorkConfigurationRun"
        | "getProjectAgentRun"
        | "cancelProjectAgentRun"
        | "retryProjectAgentRunTrack" => contract_ref("agent-run"),
        "createInternalAgentLlmReview"
        | "getInternalAgentLlmReview"
        | "cancelInternalAgentLlmReview" => contract_ref("http/internal-agent-llm-review-receipt"),
        "getInternalAgentWorkExecutionIntent" => {
            contract_ref("http/agent-work-execution-intent-metadata")
        }
        "getProjectWorkConfigurationPlan" => contract_ref("http/work-configuration-plan-view"),
        "completeProjectAuthoringApproval" => contract_ref("authoring-approval"),
        "getProjectAuthoringApproval" => contract_ref("authoring-approval-publication-status"),
        "getProjectEnvironmentCandidate" => contract_ref("http/environment-candidate-view"),
        "getProjectEvaluationCandidate" => contract_ref("http/evaluation-candidate-view"),
        "createEvaluationRelease" | "getEvaluationRelease" | "withdrawEvaluationRelease" => {
            contract_ref("evaluation-release")
        }
        "listEvaluationReleases" => {
            json!({"type":"object","additionalProperties":false,"required":["items"],"properties":{"items":{"type":"array","items":contract_ref("evaluation-release")} ,"nextCursor":{"type":["string","null"]}}})
        }
        "getOwnEvaluationResult" | "getOwnProjectEvaluationResult" => {
            contract_ref("student-evaluation-result")
        }
        "listOwnEvaluationResults" | "listOwnProjectEvaluationResults" => {
            json!({"type":"object","additionalProperties":false,"required":["items"],"properties":{"items":{"type":"array","items":contract_ref("student-evaluation-result")} ,"nextCursor":{"type":["string","null"]}}})
        }
        "appendProjectEnvironmentCandidateDecision"
        | "appendProjectEvaluationCandidateDecision" => contract_ref("candidate-approval"),
        "getEnvironmentTemplateRelease" => contract_ref("http/environment-template-release-view"),
        "withdrawEnvironmentTemplateRelease" => contract_ref("release-withdrawal"),
        "listEnvironmentTemplateReleases" => {
            json!({"type":"object","required":["items"],"properties":{"items":{"type":"array","items":contract_ref("http/environment-template-release-view")},"nextCursor":{"type":["string","null"]}}})
        }
        "getEnvironment" => contract_ref("environment-instance"),
        "getInternalAuthoringPublicationAdmission" => {
            contract_ref("http/authoring-publication-admission-binding")
        }
        "getInternalGeneratedArtifact" => contract_ref("http/generated-artifact-record"),
        "resolveEnvironmentWorkConfigurationTarget" => {
            contract_ref("environment-work-configuration-target")
        }
        "recordResourceUsage" | "recordInternalResourceUsage" => {
            contract_ref("resource-usage-record")
        }
        "claimTaskResource"
        | "acknowledgeTaskResource"
        | "getTaskResource"
        | "releaseTaskResource" => contract_ref("http/task-resource-status"),
        "getResourceRequest"
        | "cancelTaskResource"
        | "createTaskResourceRequest"
        | "getTaskResourceRequest" => contract_ref("resource-request"),
        "listResourceRequests" | "listProjectResourceRequests" => {
            json!({"type":"array","items":contract_ref("resource-request")})
        }
        "listResourceGpuCatalog" => {
            json!({"type":"array","items":contract_ref("gpu-catalog-entry")})
        }
        "createResourceGpuCatalogEntry" => contract_ref("gpu-catalog-entry"),
        "listResourceRates" => {
            json!({"type":"array","items":contract_ref("resource-rate")})
        }
        "createResourceRate" => contract_ref("resource-rate"),
        "getProjectResourceBudget" | "upsertProjectResourceBudget" => {
            contract_ref("resource-budget")
        }
        "listProjectResourceCharges" => {
            json!({"type":"array","items":contract_ref("resource-charge")})
        }
        "createProjectResourceChargeAdjustment" => contract_ref("resource-charge"),
        "getResourceLease" | "renewResourceLease" | "revokeResourceLease" => {
            contract_ref("resource-lease")
        }
        "listResourceLeases" | "listProjectResourceLeases" => {
            json!({"type":"array","items":contract_ref("resource-lease")})
        }
        "listEnvironments" => contract_ref("http/environment-summary-page"),
        "getEnvironmentOperation" => contract_ref("environment-operation-snapshot"),
        "listEnvironmentOperations" => contract_ref("http/environment-operation-page"),
        "listEnvironmentAccessGrants" => contract_ref("http/environment-access-grant-page"),
        "streamProjectEvents" => contract_ref("http/environment-management-event"),
        "listEnvironmentEndpoints" => {
            json!({"type":"object","required":["items"],"properties":{"items":{"type":"array","items":contract_ref("environment-endpoint")}}})
        }
        "getFrozenSubmission" => contract_ref("frozen-submission"),
        "listSshPublicKeys" => {
            json!({"type":"object","required":["items"],"properties":{"items":{"type":"array","items":contract_ref("ssh-public-key")},"nextCursor":{"type":["string","null"]}}})
        }
        "createSshPublicKey" => contract_ref("ssh-public-key"),
        "createAccessGrant" | "getAccessGrant" | "renewAccessGrant" => contract_ref("access-grant"),
        "listConsoleCapabilities" => contract_ref("console-capability-availability"),
        "issueConsoleCapability" => contract_ref("console-capability"),
        "authorizeSsh" => contract_ref("ssh-authorization"),
        "resolveEnvironmentOwner" => contract_ref("environment-owner-resolution"),
        "resolveEndpointEligibility" => contract_ref("environment-endpoint-eligibility"),
        "resolveEnvironmentEvaluationExecutionBinding"
        | "resolveEnvironmentWorkExecutionBinding" => {
            contract_ref("internal/environment-execution-binding")
        }
        "createGatewaySession" | "heartbeatGatewaySession" | "closeGatewaySession" => {
            contract_ref("gateway-session")
        }
        id if [
            "createEnvironmentTemplateRelease",
            "createEnvironment",
            "startEnvironment",
            "stopEnvironment",
            "restartEnvironment",
            "resetEnvironment",
            "retryEnvironment",
            "cancelEnvironmentOperation",
            "recoverEnvironment",
            "deleteEnvironment",
            "freezeSubmission",
            "createResourceRequest",
            "createProjectResourceRequest",
            "approveResourceRequest",
            "resizeAndApproveResourceRequest",
            "cancelResourceRequest",
            "rejectResourceRequest",
            "retryResourceRequest",
            "revokeAccessGrant",
            "renewAccessGrant",
        ]
        .contains(&id) =>
        {
            if [
                "createEnvironment",
                "startEnvironment",
                "stopEnvironment",
                "restartEnvironment",
                "resetEnvironment",
                "retryEnvironment",
                "cancelEnvironmentOperation",
                "recoverEnvironment",
                "deleteEnvironment",
            ]
            .contains(&id)
            {
                contract_ref("http/environment-operation-accepted")
            } else if [
                "createResourceRequest",
                "createProjectResourceRequest",
                "approveResourceRequest",
                "resizeAndApproveResourceRequest",
                "cancelResourceRequest",
                "rejectResourceRequest",
                "retryResourceRequest",
            ]
            .contains(&id)
            {
                contract_ref("http/resource-operation-accepted")
            } else {
                json!({"$ref":"#/components/schemas/OperationAccepted"})
            }
        }
        _ => return None,
    };
    Some(schema)
}

fn internal_security(permission: &str) -> Value {
    json!([{"serviceJwt":[permission]}])
}

fn operation_responses(
    operation_id: &str,
    success_status: u16,
    response_schema: Option<Value>,
) -> Value {
    let mut responses = serde_json::Map::new();
    let mut success = json!({"description":"Successful response","headers":{"ETag":{"schema":{"type":"string","pattern":"^\\\"rev-[1-9][0-9]*\\\"$"}}}});
    if operation_id == "issueConsoleCapability" {
        success["headers"]["Set-Cookie"] = json!({
            "description": "Exactly one __Secure-labweaver_console_handoff cookie. It MUST be Secure, HttpOnly, SameSite=Strict, have Max-Age=30, and use the returned connectionLocator as its exact Path. Its value is the one-time secret and is never present in a response body, URL, SDK, or log.",
            "schema": {"type":"string"}
        });
    }
    if let Some(schema) = response_schema {
        let media_type = if operation_id == "streamProjectEvents" {
            "text/event-stream"
        } else {
            "application/json"
        };
        success["content"] = json!({media_type:{"schema":schema}});
    }
    responses.insert(success_status.to_string(), success);
    let error_statuses: &[u16] = match operation_id {
        "listConsoleCapabilities" => &[401, 403, 404, 412, 422, 429, 503],
        "issueConsoleCapability" => &[401, 403, 404, 409, 412, 422, 429, 503],
        "listEnvironments" | "listEnvironmentOperations" | "listEnvironmentAccessGrants" => {
            &[400, 401, 403, 409, 410, 422, 429, 500, 503]
        }
        "getEnvironmentOperation" => &[400, 401, 403, 404, 409, 429, 500, 503],
        "streamProjectEvents" => &[400, 401, 403, 409, 410, 429, 500, 503],
        _ => &[400, 401, 403, 404, 409, 410, 412, 422, 429, 500, 503],
    };
    for code in error_statuses {
        responses.insert(
            code.to_string(),
            json!({"$ref":"#/components/responses/Problem"}),
        );
    }
    Value::Object(responses)
}

#[derive(Debug, thiserror::Error)]
pub enum GenerationError {
    #[error("contract artifact serialization failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("contract schema generation failed: {0}")]
    Contract(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn generation_is_byte_deterministic_and_surfaces_are_isolated()
    -> Result<(), Box<dyn std::error::Error>> {
        assert_eq!(generate_all()?, generate_all()?);
        let generated = generate_all()?;
        let public = generated
            .iter()
            .find(|item| item.relative_path.ends_with("public.v1.json"))
            .ok_or("public OpenAPI was not generated")?;
        let internal = generated
            .iter()
            .find(|item| item.relative_path.ends_with("internal.v1.json"))
            .ok_or("internal OpenAPI was not generated")?;
        let public = String::from_utf8_lossy(&public.bytes);
        let internal = String::from_utf8_lossy(&internal.bytes);
        assert!(!public.contains("/internal/v1"));
        assert!(!internal.contains("/api/v1"));
        assert!(public.contains("/auth/login"));
        assert!(public.contains("/auth/backchannel-logout"));
        assert!(public.contains("#/components/schemas/AuthSession"));
        assert!(public.contains("__Host-labweaver_session"));
        let public_document: Value = serde_json::from_str(&public)?;
        let console_path = "/api/v1/access-grants/{grantId}/console-capabilities"
            .replace('~', "~0")
            .replace('/', "~1");
        let issue_console = format!("/paths/{console_path}/post");
        assert_eq!(
            public_document.pointer(&format!("{issue_console}/security")),
            Some(&json!([{"bffSession": []}])),
            "console issuance must not accept an OIDC bearer projection"
        );
        let parameters = public_document
            .pointer(&format!("{issue_console}/parameters"))
            .and_then(Value::as_array)
            .ok_or("console issuance parameters are missing")?;
        assert!(parameters.iter().any(|parameter| parameter == &json!({"name":"Origin","in":"header","required":true,"schema":{"type":"string","format":"uri"}})));
        assert!(parameters.iter().any(|parameter| parameter == &json!({"name":"X-CSRF-Token","in":"header","required":true,"schema":{"type":"string","minLength":43,"maxLength":43}})));
        assert_eq!(
            public_document.pointer(&format!(
                "{issue_console}/x-labweaver-console-handoff-cookie/path"
            )),
            Some(&json!("connectionLocator"))
        );
        assert_eq!(
            public_document.pointer(&format!(
                "{issue_console}/responses/201/headers/Set-Cookie/schema/type"
            )),
            Some(&json!("string"))
        );
        let issue_errors = public_document
            .pointer(&format!("{issue_console}/x-labweaver-errors"))
            .and_then(Value::as_array)
            .ok_or("console issuance diagnostics are missing")?;
        for diagnostic in [
            "LW_CONSOLE_CAPABILITY_DENIED",
            "LW_CONSOLE_CAPABILITY_EXPIRED",
            "LW_CONSOLE_REVISION_CONFLICT",
            "LW_CONSOLE_LEASE_INVALID",
            "LW_CONSOLE_ENVIRONMENT_NOT_READY",
            "LW_CONSOLE_SUBPROTOCOL_MISMATCH",
            "LW_CONSOLE_UPSTREAM_UNAVAILABLE",
        ] {
            assert!(
                issue_errors.contains(&json!(diagnostic)),
                "missing {diagnostic}"
            );
        }
        for path in [
            "/api/v1/projects/{projectId}/agent-runs",
            "/api/v1/projects/{projectId}/agent-runs/{runId}/cancel",
            "/api/v1/projects/{projectId}/agent-runs/{runId}/tracks/{track}/retry",
        ] {
            assert_eq!(
                public_document.pointer(&format!(
                    "/paths/{}/post/responses/202/content/application~1json/schema/$ref",
                    path.replace('~', "~0").replace('/', "~1")
                )),
                Some(&json!("../contracts/v1/agent-run.schema.json")),
                "{path} must return the AgentRun body implemented by Control Service"
            );
        }
        assert!(internal.contains("/internal/v1/auth/decision"));
        assert!(internal.contains("AuthorizationDecisionRequest"));
        assert!(internal.contains("serviceJwt"));
        assert!(!internal.contains("serviceMtls"));
        assert!(!internal.contains("mutualTLS"));
        let release_view = generated
            .iter()
            .find(|item| {
                item.relative_path
                    .ends_with("environment-template-release-view.schema.json")
            })
            .ok_or("release view schema was not generated")?;
        let release_view: Value = serde_json::from_slice(&release_view.bytes)?;
        let properties = release_view
            .get("properties")
            .and_then(Value::as_object)
            .ok_or("release view properties are missing")?;
        assert!(properties.contains_key("id"));
        assert!(properties.contains_key("withdrawal"));
        assert!(!properties.contains_key("release"));
        Ok(())
    }
}
