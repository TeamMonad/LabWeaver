//! Deterministic build-pipeline acceptance and failure-path tests.
#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "test fixtures use explicit assertion messages for invalid setup"
)]

use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agent_service::build_pipeline::{
    BUILD_EXECUTOR_PROTOCOL_VERSION, BuildCancellation, BuildExecutionFence, BuildFailureCode,
    BuildIdentity, BuildPipeline, BuildPipelinePolicy, BuildProviderFailure,
    BuildProviderRequestContext, BuildSupplyChainProvider, BuiltCandidate, PrivateRegistryProject,
    PublishedImage, ScanEvidence,
};
use async_trait::async_trait;
use contracts::authoring::{CandidateApproval, CandidateDecision};
use contracts::events::AgentBuildRequested;
use contracts::supply_chain::{
    BuildNetworkPolicy, BuildRequest, ImageArtifact, VulnerabilitySummary,
};
use contracts::{
    ActorId, ApprovalId, ArtifactId, ArtifactRef, BuildRequestId, CandidateId, CourseId, PolicyId,
    Revision, Sha256Digest, UtcTimestamp,
};
use uuid::Uuid;

const DIGEST_HEX: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Call {
    EnsurePrivate,
    Build,
    Scan,
    Publish,
    Cleanup,
}

#[derive(Clone)]
struct FakeProvider {
    calls: Arc<Mutex<Vec<Call>>>,
    contexts: Arc<Mutex<Vec<BuildProviderRequestContext>>>,
    critical: u32,
    high: u32,
    project_private: bool,
    project_quota_bytes: u64,
    robot_name: String,
    published_digest: String,
    build_delay: Duration,
    cleanup_fails: bool,
}

impl Default for FakeProvider {
    fn default() -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            contexts: Arc::new(Mutex::new(Vec::new())),
            critical: 0,
            high: 0,
            project_private: true,
            project_quota_bytes: 10 * 1024 * 1024 * 1024,
            robot_name: "runtime-puller".to_owned(),
            published_digest: digest(),
            build_delay: Duration::ZERO,
            cleanup_fails: false,
        }
    }
}

impl FakeProvider {
    fn record(&self, call: Call, context: &BuildProviderRequestContext) {
        self.calls.lock().expect("call lock").push(call);
        self.contexts.lock().expect("context lock").push(*context);
    }

    fn calls(&self) -> Vec<Call> {
        self.calls.lock().expect("call lock").clone()
    }

    fn contexts(&self) -> Vec<BuildProviderRequestContext> {
        self.contexts.lock().expect("context lock").clone()
    }
}

#[async_trait]
impl BuildSupplyChainProvider for FakeProvider {
    fn builder_binding(&self) -> &'static str {
        "buildkit-primary-v1"
    }

    fn scanner_binding(&self) -> &'static str {
        "trivy-primary-v1"
    }

    fn registry_binding(&self) -> &'static str {
        "harbor-primary-v1"
    }

    async fn ensure_private_project(
        &self,
        context: &BuildProviderRequestContext,
        command: &AgentBuildRequested,
        identity: BuildIdentity,
    ) -> Result<PrivateRegistryProject, BuildProviderFailure> {
        self.record(Call::EnsurePrivate, context);
        Ok(PrivateRegistryProject {
            build_request_id: command.request.id,
            build_identity: identity,
            repository_prefix: format!("harbor.internal/course-{}", command.request.course_id),
            private: self.project_private,
            storage_quota_bytes: self.project_quota_bytes,
            robot_subject: if self.robot_name.is_empty() {
                String::new()
            } else {
                format!(
                    "robot$course-{}+{}",
                    command.request.course_id, self.robot_name
                )
            },
        })
    }

    async fn build_candidate(
        &self,
        context: &BuildProviderRequestContext,
        command: &AgentBuildRequested,
        identity: BuildIdentity,
    ) -> Result<BuiltCandidate, BuildProviderFailure> {
        self.record(Call::Build, context);
        if !self.build_delay.is_zero() {
            tokio::time::sleep(self.build_delay).await;
        }
        Ok(BuiltCandidate {
            build_request_id: command.request.id,
            build_identity: identity,
            repository: command.request.output_repository.clone(),
            digest: digest(),
        })
    }

    async fn scan_candidate(
        &self,
        context: &BuildProviderRequestContext,
        candidate: &BuiltCandidate,
    ) -> Result<ScanEvidence, BuildProviderFailure> {
        self.record(Call::Scan, context);
        Ok(ScanEvidence {
            build_identity: candidate.build_identity,
            digest: candidate.digest.clone(),
            scanner_name: "trivy".to_owned(),
            scanner_version: "0.58.0".to_owned(),
            scanner_database_sha256: Sha256Digest::of_bytes(b"trivy-db"),
            vulnerabilities: VulnerabilitySummary {
                unknown: 0,
                low: 1,
                medium: 2,
                high: self.high,
                critical: self.critical,
            },
        })
    }

    async fn publish_immutable(
        &self,
        context: &BuildProviderRequestContext,
        candidate: &BuiltCandidate,
    ) -> Result<PublishedImage, BuildProviderFailure> {
        self.record(Call::Publish, context);
        Ok(PublishedImage {
            build_identity: candidate.build_identity,
            digest: self.published_digest.clone(),
        })
    }

    async fn cleanup_candidate(
        &self,
        context: &BuildProviderRequestContext,
        _build_request_id: BuildRequestId,
        _identity: BuildIdentity,
    ) -> Result<(), BuildProviderFailure> {
        self.record(Call::Cleanup, context);
        if self.cleanup_fails {
            Err(BuildProviderFailure {
                code: agent_service::build_pipeline::BuildProviderFailureCode::Unavailable,
                retryable: true,
            })
        } else {
            Ok(())
        }
    }
}

#[tokio::test]
async fn successful_build_preserves_digest_identity_and_high_warning() {
    let provider = FakeProvider {
        high: 3,
        ..FakeProvider::default()
    };
    let calls = provider.clone();
    let pipeline = pipeline(provider);
    let command = command(60_000);

    let first = pipeline
        .execute(&command, now(), fence(60_000), &BuildCancellation::new())
        .await
        .expect("build succeeds");
    let second = pipeline
        .execute(&command, now(), fence(60_000), &BuildCancellation::new())
        .await
        .expect("same command succeeds deterministically");

    assert_eq!(first.build_identity, BuildIdentity(command.command_sha256));
    assert!(first.registry_project.private);
    assert!(first.registry_project.storage_quota_bytes > 0);
    assert_eq!(first.build_identity, second.build_identity);
    assert_eq!(
        first.artifact.content_sha256(),
        second.artifact.content_sha256()
    );
    assert_eq!(first.high_severity_warnings, 3);
    assert!(matches!(first.artifact, ImageArtifact::Container { .. }));
    assert!(!calls.calls().contains(&Call::Cleanup));
    let contexts = calls.contexts();
    assert!(contexts.iter().all(|context| {
        context.protocol_version == BUILD_EXECUTOR_PROTOCOL_VERSION
            && context.fence_generation == 1
            && context.lease_token == Uuid::from_u128(1)
    }));
    assert_eq!(
        contexts
            .iter()
            .take(4)
            .map(|context| context.stage_request_id)
            .collect::<std::collections::HashSet<_>>()
            .len(),
        4
    );
}

#[tokio::test]
async fn retry_generation_changes_every_remote_stage_identity() {
    let provider = FakeProvider::default();
    let recorded = provider.clone();
    let pipeline = pipeline(provider);
    let command = command(60_000);
    pipeline
        .execute(
            &command,
            now(),
            fence_with(1, Uuid::from_u128(1), 60_000),
            &BuildCancellation::new(),
        )
        .await
        .expect("first generation succeeds");
    pipeline
        .execute(
            &command,
            now(),
            fence_with(2, Uuid::from_u128(2), 60_000),
            &BuildCancellation::new(),
        )
        .await
        .expect("second generation succeeds");

    let contexts = recorded.contexts();
    assert_eq!(contexts.len(), 8);
    for (first, second) in contexts.iter().take(4).zip(contexts.iter().skip(4)) {
        assert_eq!(first.stage, second.stage);
        assert_eq!(first.build_request_id, second.build_request_id);
        assert_ne!(first.fence_generation, second.fence_generation);
        assert_ne!(first.lease_token, second.lease_token);
        assert_ne!(first.stage_request_id, second.stage_request_id);
    }
}

#[tokio::test]
async fn critical_vulnerability_blocks_publication_then_cleans_up() {
    let provider = FakeProvider {
        critical: 1,
        ..FakeProvider::default()
    };
    let calls = provider.clone();
    let error = pipeline(provider)
        .execute(
            &command(60_000),
            now(),
            fence(60_000),
            &BuildCancellation::new(),
        )
        .await
        .expect_err("Critical must block");

    assert_eq!(error.code, BuildFailureCode::CriticalVulnerability);
    assert!(error.cleanup_verified);
    assert_eq!(
        calls.calls(),
        vec![Call::EnsurePrivate, Call::Build, Call::Scan, Call::Cleanup]
    );
}

#[tokio::test]
async fn private_project_quota_and_robot_are_mandatory() {
    for provider in [
        FakeProvider {
            project_private: false,
            ..FakeProvider::default()
        },
        FakeProvider {
            project_quota_bytes: 0,
            ..FakeProvider::default()
        },
        FakeProvider {
            robot_name: String::new(),
            ..FakeProvider::default()
        },
    ] {
        let calls = provider.clone();
        let error = pipeline(provider)
            .execute(
                &command(60_000),
                now(),
                fence(60_000),
                &BuildCancellation::new(),
            )
            .await
            .expect_err("private project, quota and a bound robot are required");
        assert_eq!(error.code, BuildFailureCode::RegistryProjectInvalid);
        assert_eq!(
            calls.calls(),
            vec![Call::EnsurePrivate, Call::Cleanup],
            "project admission must fail before BuildKit"
        );
    }
}

#[tokio::test]
async fn publication_digest_mismatch_is_fail_closed() {
    let provider = FakeProvider {
        published_digest: format!("sha256:{}", "b".repeat(64)),
        ..FakeProvider::default()
    };
    let calls = provider.clone();
    let error = pipeline(provider)
        .execute(
            &command(60_000),
            now(),
            fence(60_000),
            &BuildCancellation::new(),
        )
        .await
        .expect_err("digest mismatch must block");

    assert_eq!(error.code, BuildFailureCode::PublicationIdentityMismatch);
    assert!(error.cleanup_verified);
    assert_eq!(calls.calls().last(), Some(&Call::Cleanup));
}

#[tokio::test]
async fn request_deadline_cancels_provider_future_and_cleans_up() {
    let provider = FakeProvider {
        build_delay: Duration::from_millis(100),
        ..FakeProvider::default()
    };
    let calls = provider.clone();
    let error = pipeline(provider)
        .execute(&command(10), now(), fence(10), &BuildCancellation::new())
        .await
        .expect_err("request deadline must stop the build");

    assert_eq!(error.code, BuildFailureCode::TimedOut);
    assert!(error.cleanup_verified);
    assert_eq!(calls.calls().last(), Some(&Call::Cleanup));
}

#[tokio::test]
async fn cancellation_cleanup_failure_is_a_terminal_blocker() {
    let provider = FakeProvider {
        cleanup_fails: true,
        ..FakeProvider::default()
    };
    let cancellation = BuildCancellation::new();
    cancellation.cancel();
    let error = pipeline(provider)
        .execute(&command(60_000), now(), fence(60_000), &cancellation)
        .await
        .expect_err("cancelled build must fail");

    assert_eq!(error.code, BuildFailureCode::CleanupFailed);
    assert!(!error.cleanup_verified);
    assert_eq!(error.diagnostic_code(), "LW_AGENT_BUILD_CLEANUP_FAILED");
}

fn pipeline(provider: FakeProvider) -> BuildPipeline<FakeProvider> {
    BuildPipeline::new(
        provider,
        BuildPipelinePolicy {
            builder_binding: "buildkit-primary-v1".to_owned(),
            scanner_binding: "trivy-primary-v1".to_owned(),
            registry_binding: "harbor-primary-v1".to_owned(),
            policy_id: PolicyId::new(),
            policy_revision: revision(1),
            scanner_name: "trivy".to_owned(),
            scanner_version: "0.58.0".to_owned(),
            scanner_database_sha256: Sha256Digest::of_bytes(b"trivy-db"),
            registry_robot_name: "runtime-puller".to_owned(),
            evidence_ttl_milliseconds: 3_600_000,
            stage_timeout: Duration::from_millis(250),
        },
    )
    .expect("valid pipeline")
}

fn fence(max_duration_milliseconds: u64) -> BuildExecutionFence {
    fence_with(1, Uuid::from_u128(1), max_duration_milliseconds)
}

fn fence_with(
    generation: u32,
    lease_token: Uuid,
    max_duration_milliseconds: u64,
) -> BuildExecutionFence {
    let milliseconds = i64::try_from(max_duration_milliseconds).expect("duration fits i64");
    let deadline_at = UtcTimestamp::from_utc(
        now()
            .get()
            .checked_add(time::Duration::milliseconds(milliseconds))
            .expect("deadline fits"),
    )
    .expect("valid deadline");
    BuildExecutionFence::new(generation, lease_token, deadline_at).expect("valid fence")
}

fn command(max_duration_milliseconds: u64) -> AgentBuildRequested {
    let course_id = CourseId::new();
    let candidate_id = CandidateId::new();
    let candidate_sha256 = Sha256Digest::of_bytes(b"environment-spec");
    let approval_id = ApprovalId::new();
    let request = BuildRequest {
        id: BuildRequestId::new(),
        course_id,
        candidate_id,
        candidate_revision: revision(1),
        candidate_sha256,
        approval_id,
        builder_binding: "buildkit-primary-v1".to_owned(),
        context: artifact_ref("application/vnd.oci.image.layer.v1.tar+gzip"),
        context_object_key: "build-contexts/context.tar.gz".to_owned(),
        dockerfile_path: "Dockerfile".to_owned(),
        base_image_digest: format!("sha256:{}", "c".repeat(64)),
        output_repository: format!("harbor.internal/course-{course_id}/{candidate_id}"),
        network: BuildNetworkPolicy::Restricted {
            allowed_registries: vec!["harbor.internal".to_owned()],
        },
        max_duration_milliseconds,
        max_cpu_millicores: 2_000,
        max_memory_bytes: 2_147_483_648,
        created_at: now(),
    };
    let approval = CandidateApproval {
        id: approval_id,
        candidate_id,
        candidate_revision: revision(1),
        candidate_sha256,
        policy_revision: revision(1),
        schema_sha256: Sha256Digest::of_bytes(b"schema"),
        trust_revision: revision(1),
        actor_id: ActorId::new(),
        decision: CandidateDecision::Approved,
        reason: "reviewed".to_owned(),
        decided_at: now(),
    };
    let idempotency_key = format!("approval:{approval_id}");
    let command_sha256 = Sha256Digest::of_canonical(&serde_json::json!({
        "request": &request,
        "approval": &approval,
        "idempotencyKey": &idempotency_key,
    }))
    .expect("canonical command");
    AgentBuildRequested {
        request,
        approval,
        idempotency_key,
        command_sha256,
    }
}

fn artifact_ref(media_type: &str) -> ArtifactRef {
    ArtifactRef {
        artifact_id: ArtifactId::new(),
        store_binding: "artifact-store-v1".to_owned(),
        object_version: "version-1".to_owned(),
        sha256: Sha256Digest::of_bytes(media_type.as_bytes()),
        size_bytes: 128,
        media_type: media_type.to_owned(),
    }
}

fn digest() -> String {
    format!("sha256:{DIGEST_HEX}")
}

fn revision(value: u64) -> Revision {
    Revision::new(value).expect("positive revision")
}

fn timestamp(value: &str) -> UtcTimestamp {
    UtcTimestamp::from_str(value).expect("valid timestamp")
}

fn now() -> UtcTimestamp {
    timestamp("2026-07-16T08:00:00.000Z")
}
