//! Deterministic JSON Schema and OpenAPI generation from Rust-owned contracts.

use std::collections::BTreeMap;

use schemars::{Schema, schema_for};
use serde_json::{Value, json};

use crate::events::{self, CloudEvent};
use crate::http::{
    ApiSurface, Method, MutationContract, OPERATIONS, OperationScopeKind, operation_authorization,
};

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
        "schemas/contracts/v1/course-llm-egress-policy.schema.json",
        crate::authoring::CourseLlmEgressPolicy
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
        "schemas/contracts/v1/image-policy-evaluation.schema.json",
        crate::supply_chain::ImagePolicyEvaluation
    );
    document!(
        "schemas/contracts/v1/environment-template-release.schema.json",
        crate::supply_chain::EnvironmentTemplateRelease
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
        "schemas/contracts/v1/http/environment-endpoint-eligibility-request.schema.json",
        crate::environment::EnvironmentEndpointEligibilityRequest
    );
    document!(
        "schemas/contracts/v1/environment-endpoint-eligibility.schema.json",
        crate::environment::EnvironmentEndpointEligibility
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
        "schemas/contracts/v1/http/candidate-decision-request.schema.json",
        crate::http::CandidateDecisionRequest
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
        "schemas/contracts/v1/events/submission-freeze-requested.schema.json",
        CloudEvent<events::SubmissionFreezeRequested>
    );
    document!(
        "schemas/contracts/v1/events/submission-frozen.schema.json",
        CloudEvent<events::SubmissionFrozen>
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
            Method::Delete => "delete",
        };
        let mut parameters = path_parameters(operation.path);
        if matches!(
            operation.operation_id,
            "listEnvironmentTemplateReleases" | "listSshPublicKeys"
        ) {
            parameters.push(json!({"name":"cursor","in":"query","required":false,"schema":{"type":"string","minLength":1,"maxLength":512}}));
            parameters.push(json!({"name":"limit","in":"query","required":false,"schema":{"type":"integer","minimum":1,"maximum":100,"default":50}}));
        }
        parameters.extend(environment_management_parameters(operation.operation_id));
        if operation.operation_id == "streamCourseEvents" {
            parameters.push(json!({"name":"courseId","in":"query","required":true,"schema":{"type":"string","format":"uuid"}}));
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
        if operation.security == crate::http::Security::BffSession {
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
                (ApiSurface::GatewayInternal, crate::http::Security::ServiceMtls) => json!({"serviceMtls": []}),
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
            "appendEnvironmentCandidateDecision" | "appendEvaluationCandidateDecision"
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
        let authorization = operation_authorization(operation.operation_id).ok_or_else(|| {
            GenerationError::Contract("operation authorization metadata is missing".to_owned())
        })?;
        operation_json["x-labweaver-allowed-roles"] = json!(authorization.allowed_roles);
        operation_json["x-labweaver-scope"] = json!(match authorization.scope {
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
        json!({"serviceMtls": {"type":"mutualTLS","description":"Deployment-controlled service identity; never exposed to browser clients."}})
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
                ,"InternalCreateAgentRunRequest": contract_ref("http/internal-create-agent-run-request")
                ,"InternalAgentRunMutationRequest": contract_ref("http/internal-agent-run-mutation-request")
                ,"InternalAgentBuildCancellationRequest": contract_ref("http/internal-agent-build-cancellation-request")
                ,"InternalAgentBuildCancellationResult": contract_ref("http/internal-agent-build-cancellation-result")
                ,"InternalAgentBuildStatusQuery": contract_ref("http/internal-agent-build-status-query")
                ,"InternalAgentRunOutcome": contract_ref("http/internal-agent-run-outcome")
                ,"InternalImageArtifactResolution": contract_ref("http/internal-image-artifact-resolution")
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
                json!({"post":{"operationId":"decideAuthorization","summary":"Evaluate an actor session and exact resource scope for an mTLS caller","security":[{"serviceMtls":[]}],"requestBody":{"required":true,"content":{"application/json":{"schema":{"$ref":"#/components/schemas/AuthorizationDecisionRequest"}}}},"responses":{"200":{"description":"Expiry-bounded authorization decision","content":{"application/json":{"schema":{"$ref":"#/components/schemas/AuthorizationDecision"}}}},"403":{"$ref":"#/components/responses/Problem"},"503":{"$ref":"#/components/responses/Problem"}}}}),
            );
            paths.insert(
                "/internal/v1/agent-runs".to_owned(),
                json!({"post":{"operationId":"createInternalAgentRun","summary":"Reserve an Agent-owned run from a Control-verified immutable package and policy","security":[{"serviceMtls":[]}],"parameters":[{"name":"Idempotency-Key","in":"header","required":true,"schema":{"type":"string","minLength":16,"maxLength":128}}],"requestBody":{"required":true,"content":{"application/json":{"schema":{"$ref":"#/components/schemas/InternalCreateAgentRunRequest"}}}},"responses":{"202":{"description":"AgentRun accepted","content":{"application/json":{"schema":{"$ref":"./agent-run.schema.json"}}}},"409":{"$ref":"#/components/responses/Problem"},"503":{"$ref":"#/components/responses/Problem"}}}}),
            );
            paths.insert(
                "/internal/v1/agent-runs/{runId}".to_owned(),
                json!({"get":{"operationId":"getInternalAgentRun","summary":"Read the authoritative Agent-owned run","security":[{"serviceMtls":[]}],"parameters":[{"name":"runId","in":"path","required":true,"schema":{"type":"string","format":"uuid"}}],"responses":{"200":{"description":"Authoritative AgentRun","content":{"application/json":{"schema":{"$ref":"./agent-run.schema.json"}}}},"404":{"$ref":"#/components/responses/Problem"},"503":{"$ref":"#/components/responses/Problem"}}}}),
            );
            paths.insert(
                "/internal/v1/agent-runs/{runId}/cancel".to_owned(),
                json!({"post":{"operationId":"cancelInternalAgentRun","summary":"Request cancellation at an exact AgentRun revision","security":[{"serviceMtls":[]}],"parameters":[{"name":"runId","in":"path","required":true,"schema":{"type":"string","format":"uuid"}},{"name":"Idempotency-Key","in":"header","required":true,"schema":{"type":"string","minLength":16,"maxLength":128}}],"requestBody":{"required":true,"content":{"application/json":{"schema":{"$ref":"#/components/schemas/InternalAgentRunMutationRequest"}}}},"responses":{"200":{"description":"Updated authoritative AgentRun","content":{"application/json":{"schema":{"$ref":"./agent-run.schema.json"}}}},"409":{"$ref":"#/components/responses/Problem"},"503":{"$ref":"#/components/responses/Problem"}}}}),
            );
            paths.insert(
                "/internal/v1/build-requests/{buildRequestId}/cancel".to_owned(),
                json!({"post":{"operationId":"cancelInternalAgentBuild","summary":"Request one actor-attributed build cancellation at an exact course, command hash, state and revision","security":[{"serviceMtls":[]}],"parameters":[{"name":"buildRequestId","in":"path","required":true,"schema":{"type":"string","format":"uuid"}},{"name":"Idempotency-Key","in":"header","required":true,"schema":{"type":"string","minLength":16,"maxLength":128}}],"requestBody":{"required":true,"content":{"application/json":{"schema":{"$ref":"#/components/schemas/InternalAgentBuildCancellationRequest"}}}},"responses":{"202":{"description":"Durable cancellation requested","content":{"application/json":{"schema":{"$ref":"#/components/schemas/InternalAgentBuildCancellationResult"}}}},"403":{"$ref":"#/components/responses/Problem"},"409":{"$ref":"#/components/responses/Problem"},"422":{"$ref":"#/components/responses/Problem"},"503":{"$ref":"#/components/responses/Problem"}}}}),
            );
            paths.insert(
                "/internal/v1/build-requests/{buildRequestId}".to_owned(),
                json!({"get":{"operationId":"getInternalAgentBuild","summary":"Read the Agent-owned build state and revision for an exact course and command hash","security":[{"serviceMtls":[]}],"parameters":[{"name":"buildRequestId","in":"path","required":true,"schema":{"type":"string","format":"uuid"}},{"name":"courseId","in":"query","required":true,"schema":{"type":"string","format":"uuid"}},{"name":"commandSha256","in":"query","required":true,"schema":{"type":"string","pattern":"^[0-9a-f]{64}$"}}],"responses":{"200":{"description":"Authoritative build status","content":{"application/json":{"schema":{"$ref":"#/components/schemas/InternalAgentBuildCancellationResult"}}}},"403":{"$ref":"#/components/responses/Problem"},"404":{"$ref":"#/components/responses/Problem"},"503":{"$ref":"#/components/responses/Problem"}}}}),
            );
            paths.insert(
                "/internal/v1/agent-runs/{runId}/tracks/{track}/retry".to_owned(),
                json!({"post":{"operationId":"retryInternalAgentRunTrack","summary":"Retry one failed AgentRun track at an exact revision","security":[{"serviceMtls":[]}],"parameters":[{"name":"runId","in":"path","required":true,"schema":{"type":"string","format":"uuid"}},{"name":"track","in":"path","required":true,"schema":{"type":"string","enum":["environment","evaluation"]}},{"name":"Idempotency-Key","in":"header","required":true,"schema":{"type":"string","minLength":16,"maxLength":128}}],"requestBody":{"required":true,"content":{"application/json":{"schema":{"$ref":"#/components/schemas/InternalAgentRunMutationRequest"}}}},"responses":{"200":{"description":"Updated authoritative AgentRun","content":{"application/json":{"schema":{"$ref":"./agent-run.schema.json"}}}},"409":{"$ref":"#/components/responses/Problem"},"503":{"$ref":"#/components/responses/Problem"}}}}),
            );
            paths.insert(
                "/internal/v1/agent-runs/{runId}/outcome".to_owned(),
                json!({"get":{"operationId":"getInternalAgentRunOutcome","summary":"Resolve the authoritative run and retained candidate checkpoints","security":[{"serviceMtls":[]}],"parameters":[{"name":"runId","in":"path","required":true,"schema":{"type":"string","format":"uuid"}}],"responses":{"200":{"description":"Authoritative outcome","content":{"application/json":{"schema":{"$ref":"#/components/schemas/InternalAgentRunOutcome"}}}},"404":{"$ref":"#/components/responses/Problem"},"503":{"$ref":"#/components/responses/Problem"}}}}),
            );
            paths.insert(
                "/internal/v1/image-artifacts/{artifactId}".to_owned(),
                json!({"get":{"operationId":"resolveInternalImageArtifact","summary":"Resolve one Agent-owned verified artifact identity","security":[{"serviceMtls":[]}],"parameters":[{"name":"artifactId","in":"path","required":true,"schema":{"type":"string","format":"uuid"}}],"responses":{"200":{"description":"Authoritative artifact resolution","content":{"application/json":{"schema":{"$ref":"#/components/schemas/InternalImageArtifactResolution"}}}},"404":{"$ref":"#/components/responses/Problem"},"503":{"$ref":"#/components/responses/Problem"}}}}),
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
            json!({"name":"courseId","in":"query","required":true,"schema":{"type":"string","format":"uuid"}}),
            json!({"name":"projectId","in":"query","required":false,"schema":{"type":"string","format":"uuid"}}),
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
            json!({"name":"state","in":"query","required":false,"schema":{"type":"string","enum":["accepted","running","cancelling","succeeded","failed","cancelled","timed_out"]}}),
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
        "streamCourseEvents" => vec![
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
        "createProblemPackageUpload" => "http/create-problem-package-upload-request",
        "completeProblemPackageUpload" => "http/complete-problem-package-upload-request",
        "createCourseLlmPolicy" => "course-llm-egress-policy",
        "createAgentRun" => "http/create-agent-run-request",
        "appendEnvironmentCandidateDecision" | "appendEvaluationCandidateDecision" => {
            "http/candidate-decision-request"
        }
        "createEnvironmentTemplateRelease" => "http/create-environment-template-release-request",
        "withdrawEnvironmentTemplateRelease" => {
            "http/withdraw-environment-template-release-request"
        }
        "createEnvironment" => "http/create-environment-request",
        "freezeSubmission" => "http/freeze-submission-request",
        "createSshPublicKey" => "http/create-ssh-public-key-request",
        "createAccessGrant" => "http/create-access-grant-request",
        "revokeAccessGrant" => "http/revoke-access-grant-request",
        "renewAccessGrant" => "http/renew-access-grant-request",
        "issueConsoleCapability" => "http/issue-console-capability-request",
        "authorizeSsh" => "ssh-authorization-request",
        "resolveEnvironmentOwner" => "http/environment-owner-resolution-request",
        "resolveEndpointEligibility" => "http/environment-endpoint-eligibility-request",
        "createGatewaySession" => "create-gateway-session-request",
        "heartbeatGatewaySession" => "heartbeat-gateway-session-request",
        "closeGatewaySession" => "close-gateway-session-request",
        _ => return None,
    };
    Some(contract_ref(name))
}

fn response_schema(operation_id: &str) -> Option<Value> {
    let schema = match operation_id {
        "createProblemPackageUpload" => contract_ref("http/problem-package-upload-session"),
        "getProblemPackage" | "completeProblemPackageUpload" => contract_ref("problem-package"),
        "createCourseLlmPolicy" | "getActiveCourseLlmPolicy" => {
            contract_ref("course-llm-egress-policy")
        }
        "createAgentRun" | "getAgentRun" | "cancelAgentRun" | "retryAgentRunTrack" => {
            contract_ref("agent-run")
        }
        "getEnvironmentCandidate" => contract_ref("http/environment-candidate-view"),
        "getEvaluationCandidate" => contract_ref("http/evaluation-candidate-view"),
        "appendEnvironmentCandidateDecision" | "appendEvaluationCandidateDecision" => {
            contract_ref("candidate-approval")
        }
        "getEnvironmentTemplateRelease" => contract_ref("http/environment-template-release-view"),
        "listEnvironmentTemplateReleases" => {
            json!({"type":"object","required":["items"],"properties":{"items":{"type":"array","items":contract_ref("http/environment-template-release-view")},"nextCursor":{"type":["string","null"]}}})
        }
        "getEnvironment" => contract_ref("environment-instance"),
        "listEnvironments" => contract_ref("http/environment-summary-page"),
        "getEnvironmentOperation" => contract_ref("environment-operation-snapshot"),
        "listEnvironmentOperations" => contract_ref("http/environment-operation-page"),
        "listEnvironmentAccessGrants" => contract_ref("http/environment-access-grant-page"),
        "streamCourseEvents" => contract_ref("http/environment-management-event"),
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
            } else {
                json!({"$ref":"#/components/schemas/OperationAccepted"})
            }
        }
        _ => return None,
    };
    Some(schema)
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
        let media_type = if operation_id == "streamCourseEvents" {
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
        "streamCourseEvents" => &[400, 401, 403, 409, 410, 429, 500, 503],
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
            "/api/v1/courses/{courseId}/agent-runs",
            "/api/v1/courses/{courseId}/agent-runs/{runId}/cancel",
            "/api/v1/courses/{courseId}/agent-runs/{runId}/tracks/{track}/retry",
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
        assert!(internal.contains("mutualTLS"));
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
