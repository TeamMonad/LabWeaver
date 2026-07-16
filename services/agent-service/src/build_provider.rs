//! Typed NATS adapter for the deployment-owned BuildKit/Harbor/Trivy/Sigstore executor.
#![allow(
    missing_docs,
    reason = "the build pipeline trait and v2 event contracts document the integration semantics"
)]

use async_trait::async_trait;
use contracts::BuildRequestId;
use contracts::events::AgentBuildRequestedV2;
use contracts::supply_chain::SigstoreEvidence;
use serde::{Deserialize, Serialize};

use crate::build_pipeline::{
    BuildIdentity, BuildProviderFailure, BuildProviderFailureCode, BuildProviderRequestContext,
    BuildProviderStage, BuildSupplyChainProvider, BuiltCandidate, PrivateRegistryProject,
    PublishedImage, ScanEvidence,
};

const MAX_RESPONSE_BYTES: usize = 1024 * 1024;

/// Exact provider binding and request subject; no fallback subject is permitted.
pub struct NatsBuildSupplyChainProvider {
    client: async_nats::Client,
    subject: String,
    builder_binding: String,
    scanner_binding: String,
    signer_binding: String,
    registry_binding: String,
}

impl NatsBuildSupplyChainProvider {
    pub fn new(
        client: async_nats::Client,
        subject: String,
        builder_binding: String,
        scanner_binding: String,
        signer_binding: String,
        registry_binding: String,
    ) -> Result<Self, BuildProviderFailure> {
        if !valid_subject(&subject)
            || [
                builder_binding.as_str(),
                scanner_binding.as_str(),
                signer_binding.as_str(),
                registry_binding.as_str(),
            ]
            .iter()
            .any(|binding| !valid_token(binding))
        {
            return Err(configuration_failure());
        }
        Ok(Self {
            client,
            subject,
            builder_binding,
            scanner_binding,
            signer_binding,
            registry_binding,
        })
    }

    async fn request(
        &self,
        context: &BuildProviderRequestContext,
        request: ProviderRequest<'_>,
    ) -> Result<ProviderResponse, BuildProviderFailure> {
        if context.stage != request.stage() {
            return Err(identity_mismatch());
        }
        let payload = serde_json::to_vec(&ProviderRequestEnvelope {
            context: *context,
            request,
        })
        .map_err(|_| output_invalid())?;
        let message = self
            .client
            .request(self.subject.clone(), payload.into())
            .await
            .map_err(|_| unavailable())?;
        if message.payload.len() > MAX_RESPONSE_BYTES {
            return Err(output_invalid());
        }
        let response: ProviderResponseEnvelope =
            serde_json::from_slice(&message.payload).map_err(|_| output_invalid())?;
        if response.protocol_version != context.protocol_version
            || response.build_request_id != context.build_request_id
            || response.fence_generation != context.fence_generation
            || response.stage != context.stage
            || response.stage_request_id != context.stage_request_id
        {
            return Err(identity_mismatch());
        }
        Ok(response.response)
    }
}

#[async_trait]
impl BuildSupplyChainProvider for NatsBuildSupplyChainProvider {
    fn builder_binding(&self) -> &str {
        &self.builder_binding
    }

    fn scanner_binding(&self) -> &str {
        &self.scanner_binding
    }

    fn signer_binding(&self) -> &str {
        &self.signer_binding
    }

    fn registry_binding(&self) -> &str {
        &self.registry_binding
    }

    async fn ensure_private_project(
        &self,
        context: &BuildProviderRequestContext,
        command: &AgentBuildRequestedV2,
        identity: BuildIdentity,
    ) -> Result<PrivateRegistryProject, BuildProviderFailure> {
        match self
            .request(
                context,
                ProviderRequest::EnsurePrivateProject { command, identity },
            )
            .await?
        {
            ProviderResponse::PrivateProjectReady { project }
                if project.build_request_id == command.request.id
                    && project.build_identity == identity =>
            {
                Ok(project)
            }
            ProviderResponse::Failed { failure } => Err(failure),
            _ => Err(identity_mismatch()),
        }
    }

    async fn build_candidate(
        &self,
        context: &BuildProviderRequestContext,
        command: &AgentBuildRequestedV2,
        identity: BuildIdentity,
    ) -> Result<BuiltCandidate, BuildProviderFailure> {
        match self
            .request(context, ProviderRequest::Build { command, identity })
            .await?
        {
            ProviderResponse::Built { candidate }
                if candidate.build_request_id == command.request.id
                    && candidate.build_identity == identity =>
            {
                Ok(candidate)
            }
            ProviderResponse::Failed { failure } => Err(failure),
            _ => Err(identity_mismatch()),
        }
    }

    async fn scan_candidate(
        &self,
        context: &BuildProviderRequestContext,
        candidate: &BuiltCandidate,
    ) -> Result<ScanEvidence, BuildProviderFailure> {
        match self
            .request(context, ProviderRequest::Scan { candidate })
            .await?
        {
            ProviderResponse::Scanned { evidence }
                if evidence.build_identity == candidate.build_identity
                    && evidence.digest == candidate.digest =>
            {
                Ok(evidence)
            }
            ProviderResponse::Failed { failure } => Err(failure),
            _ => Err(identity_mismatch()),
        }
    }

    async fn sign_and_verify(
        &self,
        context: &BuildProviderRequestContext,
        candidate: &BuiltCandidate,
    ) -> Result<SigstoreEvidence, BuildProviderFailure> {
        match self
            .request(context, ProviderRequest::Sign { candidate })
            .await?
        {
            ProviderResponse::Signed {
                build_identity,
                digest,
                evidence,
            } if build_identity == candidate.build_identity && digest == candidate.digest => {
                Ok(evidence)
            }
            ProviderResponse::Failed { failure } => Err(failure),
            _ => Err(identity_mismatch()),
        }
    }

    async fn publish_immutable(
        &self,
        context: &BuildProviderRequestContext,
        candidate: &BuiltCandidate,
    ) -> Result<PublishedImage, BuildProviderFailure> {
        match self
            .request(context, ProviderRequest::Publish { candidate })
            .await?
        {
            ProviderResponse::Published { image }
                if image.build_identity == candidate.build_identity
                    && image.digest == candidate.digest =>
            {
                Ok(image)
            }
            ProviderResponse::Failed { failure } => Err(failure),
            _ => Err(identity_mismatch()),
        }
    }

    async fn cleanup_candidate(
        &self,
        context: &BuildProviderRequestContext,
        build_request_id: BuildRequestId,
        identity: BuildIdentity,
    ) -> Result<(), BuildProviderFailure> {
        match self
            .request(
                context,
                ProviderRequest::Cleanup {
                    build_request_id,
                    identity,
                },
            )
            .await?
        {
            ProviderResponse::Cleaned {
                build_request_id: observed_request_id,
                build_identity,
            } if observed_request_id == build_request_id && build_identity == identity => Ok(()),
            ProviderResponse::Failed { failure } => Err(failure),
            _ => Err(identity_mismatch()),
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ProviderRequestEnvelope<'a> {
    #[serde(flatten)]
    context: BuildProviderRequestContext,
    request: ProviderRequest<'a>,
}

#[derive(Serialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
enum ProviderRequest<'a> {
    EnsurePrivateProject {
        command: &'a AgentBuildRequestedV2,
        identity: BuildIdentity,
    },
    Build {
        command: &'a AgentBuildRequestedV2,
        identity: BuildIdentity,
    },
    Scan {
        candidate: &'a BuiltCandidate,
    },
    Sign {
        candidate: &'a BuiltCandidate,
    },
    Publish {
        candidate: &'a BuiltCandidate,
    },
    Cleanup {
        build_request_id: BuildRequestId,
        identity: BuildIdentity,
    },
}

impl ProviderRequest<'_> {
    const fn stage(&self) -> BuildProviderStage {
        match self {
            Self::EnsurePrivateProject { .. } => BuildProviderStage::EnsurePrivateProject,
            Self::Build { .. } => BuildProviderStage::Build,
            Self::Scan { .. } => BuildProviderStage::Scan,
            Self::Sign { .. } => BuildProviderStage::Sign,
            Self::Publish { .. } => BuildProviderStage::Publish,
            Self::Cleanup { .. } => BuildProviderStage::Cleanup,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProviderResponseEnvelope {
    protocol_version: u8,
    build_request_id: BuildRequestId,
    fence_generation: u32,
    stage: BuildProviderStage,
    stage_request_id: contracts::Sha256Digest,
    response: ProviderResponse,
}

#[derive(Deserialize)]
#[serde(
    tag = "status",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
enum ProviderResponse {
    PrivateProjectReady {
        project: PrivateRegistryProject,
    },
    Built {
        candidate: BuiltCandidate,
    },
    Scanned {
        evidence: ScanEvidence,
    },
    Signed {
        build_identity: BuildIdentity,
        digest: String,
        evidence: SigstoreEvidence,
    },
    Published {
        image: PublishedImage,
    },
    Cleaned {
        build_request_id: BuildRequestId,
        build_identity: BuildIdentity,
    },
    Failed {
        failure: BuildProviderFailure,
    },
}

fn valid_token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.:".contains(&byte))
}

fn valid_subject(value: &str) -> bool {
    valid_token(value) && !value.contains('*') && !value.contains('>')
}

const fn configuration_failure() -> BuildProviderFailure {
    BuildProviderFailure {
        code: BuildProviderFailureCode::Rejected,
        retryable: false,
    }
}

const fn unavailable() -> BuildProviderFailure {
    BuildProviderFailure {
        code: BuildProviderFailureCode::Unavailable,
        retryable: true,
    }
}

const fn identity_mismatch() -> BuildProviderFailure {
    BuildProviderFailure {
        code: BuildProviderFailureCode::IdentityMismatch,
        retryable: false,
    }
}

const fn output_invalid() -> BuildProviderFailure {
    BuildProviderFailure {
        code: BuildProviderFailureCode::OutputInvalid,
        retryable: false,
    }
}
