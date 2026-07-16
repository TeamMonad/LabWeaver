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
    BuildIdentity, BuildProviderFailure, BuildProviderFailureCode, BuildSupplyChainProvider,
    BuiltCandidate, PrivateRegistryProject, PublishedImage, ScanEvidence,
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
        request: ProviderRequest<'_>,
    ) -> Result<ProviderResponse, BuildProviderFailure> {
        let payload = serde_json::to_vec(&request).map_err(|_| output_invalid())?;
        let message = self
            .client
            .request(self.subject.clone(), payload.into())
            .await
            .map_err(|_| unavailable())?;
        if message.payload.len() > MAX_RESPONSE_BYTES {
            return Err(output_invalid());
        }
        serde_json::from_slice(&message.payload).map_err(|_| output_invalid())
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
        command: &AgentBuildRequestedV2,
        identity: BuildIdentity,
    ) -> Result<PrivateRegistryProject, BuildProviderFailure> {
        match self
            .request(ProviderRequest::EnsurePrivateProject { command, identity })
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
        command: &AgentBuildRequestedV2,
        identity: BuildIdentity,
    ) -> Result<BuiltCandidate, BuildProviderFailure> {
        match self
            .request(ProviderRequest::Build { command, identity })
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
        candidate: &BuiltCandidate,
    ) -> Result<ScanEvidence, BuildProviderFailure> {
        match self.request(ProviderRequest::Scan { candidate }).await? {
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
        candidate: &BuiltCandidate,
    ) -> Result<SigstoreEvidence, BuildProviderFailure> {
        match self.request(ProviderRequest::Sign { candidate }).await? {
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
        candidate: &BuiltCandidate,
    ) -> Result<PublishedImage, BuildProviderFailure> {
        match self.request(ProviderRequest::Publish { candidate }).await? {
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
        build_request_id: BuildRequestId,
        identity: BuildIdentity,
    ) -> Result<(), BuildProviderFailure> {
        match self
            .request(ProviderRequest::Cleanup {
                build_request_id,
                identity,
            })
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
#[serde(
    tag = "stage",
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
