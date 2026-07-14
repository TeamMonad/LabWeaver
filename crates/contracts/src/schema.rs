//! Deterministic JSON Schema and OpenAPI generation from Rust-owned contracts.

use std::collections::BTreeMap;

use schemars::{Schema, schema_for};
use serde_json::{Value, json};

use crate::events::{self, CloudEvent};
use crate::http::{ApiSurface, Method, MutationContract, OPERATIONS};

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
        "schemas/contracts/v1/private-sigstore-workload-identity.schema.json",
        crate::supply_chain::WorkloadIdentityPolicy
    );
    document!(
        "schemas/contracts/v1/private-sigstore-trust-bundle.schema.json",
        crate::supply_chain::PrivateSigstoreTrustBundle
    );
    document!(
        "schemas/contracts/v1/private-sigstore-testflight-report.schema.json",
        crate::supply_chain::PrivateSigstoreTestFlightReport
    );
    document!(
        "schemas/contracts/v1/environment-instance.schema.json",
        crate::environment::EnvironmentInstance
    );
    document!(
        "schemas/contracts/v1/environment-endpoint.schema.json",
        crate::environment::EnvironmentEndpoint
    );
    document!(
        "schemas/contracts/v1/access-grant.schema.json",
        crate::access::AccessGrant
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
        "schemas/contracts/v1/http/create-environment-template-release-request.schema.json",
        crate::http::CreateEnvironmentTemplateReleaseRequest
    );
    document!(
        "schemas/contracts/v1/http/create-environment-request.schema.json",
        crate::http::CreateEnvironmentRequest
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
        "schemas/contracts/v1/events/agent-run-requested.schema.json",
        CloudEvent<events::AgentRunEvent>
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
        CloudEvent<events::AgentBuildRequested>
    );
    document!(
        "schemas/contracts/v1/events/agent-build-failed.schema.json",
        CloudEvent<events::AgentBuildRequested>
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
        "schemas/contracts/v1/events/access-grant-created.schema.json",
        CloudEvent<events::AccessGrantChanged>
    );
    document!(
        "schemas/contracts/v1/events/access-grant-revoked.schema.json",
        CloudEvent<events::AccessGrantChanged>
    );
    document!(
        "schemas/contracts/v1/events/submission-freeze-requested.schema.json",
        CloudEvent<events::SubmissionFrozen>
    );
    document!(
        "schemas/contracts/v1/events/submission-frozen.schema.json",
        CloudEvent<events::SubmissionFrozen>
    );
    document!(
        "schemas/contracts/v1/events/lab-release-approved.schema.json",
        CloudEvent<events::ReleasePublished>
    );
    document!(
        "schemas/contracts/v1/events/environment-template-release-published.schema.json",
        CloudEvent<events::ReleasePublished>
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
        if operation.operation_id == "streamCourseEvents" {
            parameters.push(json!({"name":"courseId","in":"query","required":true,"schema":{"type":"string","format":"uuid"}}));
            parameters.push(json!({"name":"after","in":"query","required":false,"schema":{"type":"integer","minimum":0}}));
            parameters.push(json!({"name":"Last-Event-ID","in":"header","required":false,"schema":{"type":"integer","minimum":0}}));
        }
        if operation.mutation != MutationContract::None {
            parameters.push(header_parameter("Idempotency-Key", true));
        }
        if operation.mutation == MutationContract::IdempotentRevisioned {
            parameters.push(header_parameter("If-Match", true));
        }
        let responses = operation_responses(
            operation.success_status,
            response_schema(operation.operation_id),
        );
        let mut operation_json = json!({
            "operationId": operation.operation_id,
            "summary": operation.operation_id,
            "description": format!("Permission: {}. Timeout: {} ms. Cancellable: {}. Retryable: {}. v1 permits additive endpoints and optional response fields only.", operation.permission, operation.timeout_milliseconds, operation.cancellable, operation.retryable),
            "security": [if surface == ApiSurface::Public { json!({"oidc": [operation.permission]}) } else { json!({"serviceMtls": []}) }],
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
        if let Some(schema) = request_schema(operation.operation_id) {
            operation_json["requestBody"] =
                json!({"required":true,"content":{"application/json":{"schema":schema}}});
        }
        let entry = paths
            .entry(operation.path.to_owned())
            .or_insert_with(|| json!({}));
        entry[method] = operation_json;
    }
    let security_schemes = if surface == ApiSurface::Public {
        json!({"oidc": {"type":"oauth2","flows":{"authorizationCode":{"authorizationUrl":"/oidc/authorize","tokenUrl":"/oidc/token","scopes":{}}}}})
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
            },
            "responses": {"Problem": {"description":"RFC 9457 problem detail","content":{"application/problem+json":{"schema":{"$ref":"#/components/schemas/ProblemDetails"}}}}}
        }
    });
    let document: utoipa::openapi::OpenApi = serde_json::from_value(value)?;
    Ok(serde_json::to_value(document)?)
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
        "createEnvironment" => "http/create-environment-request",
        "freezeSubmission" => "http/freeze-submission-request",
        "createSshPublicKey" => "http/create-ssh-public-key-request",
        "createAccessGrant" => "http/create-access-grant-request",
        "revokeAccessGrant" => "http/revoke-access-grant-request",
        "authorizeSsh" => "ssh-authorization-request",
        "createGatewaySession" | "heartbeatGatewaySession" | "closeGatewaySession" => {
            "gateway-session"
        }
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
        "getAgentRun" => contract_ref("agent-run"),
        "getEnvironmentCandidate" => contract_ref("environment-candidate"),
        "getEvaluationCandidate" => contract_ref("evaluation-candidate"),
        "appendEnvironmentCandidateDecision" | "appendEvaluationCandidateDecision" => {
            contract_ref("candidate-approval")
        }
        "getEnvironmentTemplateRelease" => contract_ref("environment-template-release"),
        "listEnvironmentTemplateReleases" => {
            json!({"type":"object","required":["items"],"properties":{"items":{"type":"array","items":contract_ref("environment-template-release")},"nextCursor":{"type":["string","null"]}}})
        }
        "getEnvironment" => contract_ref("environment-instance"),
        "listEnvironmentEndpoints" => {
            json!({"type":"object","required":["items"],"properties":{"items":{"type":"array","items":contract_ref("environment-endpoint")}}})
        }
        "getFrozenSubmission" => contract_ref("frozen-submission"),
        "listSshPublicKeys" => {
            json!({"type":"object","required":["items"],"properties":{"items":{"type":"array","items":contract_ref("ssh-public-key")},"nextCursor":{"type":["string","null"]}}})
        }
        "createSshPublicKey" => contract_ref("ssh-public-key"),
        "createAccessGrant" | "getAccessGrant" => contract_ref("access-grant"),
        "authorizeSsh" => contract_ref("ssh-authorization"),
        "createGatewaySession" => contract_ref("gateway-session"),
        id if [
            "createAgentRun",
            "cancelAgentRun",
            "retryAgentRunTrack",
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
        ]
        .contains(&id) =>
        {
            json!({"$ref":"#/components/schemas/OperationAccepted"})
        }
        _ => return None,
    };
    Some(schema)
}

fn operation_responses(success_status: u16, response_schema: Option<Value>) -> Value {
    let mut responses = serde_json::Map::new();
    let mut success = json!({"description":"Successful response","headers":{"ETag":{"schema":{"type":"string","pattern":"^\\\"rev-[1-9][0-9]*\\\"$"}}}});
    if let Some(schema) = response_schema {
        success["content"] = json!({"application/json":{"schema":schema}});
    }
    responses.insert(success_status.to_string(), success);
    for code in [400_u16, 401, 403, 404, 409, 410, 412, 422, 429, 500, 503] {
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
        assert!(internal.contains("mutualTLS"));
        Ok(())
    }
}
