//! Real `PostgreSQL` proof for build lease heartbeat, live cancellation, cleanup, and Outbox.
#![allow(
    clippy::expect_used,
    clippy::too_many_lines,
    reason = "one live database test keeps the complete lease and uses fixed validated fixtures"
)]

use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use agent_service::build_pipeline::{
    BuildIdentity, BuildPipeline, BuildPipelinePolicy, BuildProviderFailure,
    BuildSupplyChainProvider, BuiltCandidate, PrivateRegistryProject, PublishedImage, ScanEvidence,
};
use agent_service::build_store::{
    BuildCommandDecision, BuildWorker, BuildWorkerOutcome, PgBuildStore,
};
use async_trait::async_trait;
use contracts::authoring::{CandidateApproval, CandidateDecision};
use contracts::events::{
    AgentBuildRequestedV2, CloudEvent, EVENT_CONTRACTS, SPEC_VERSION, subjects,
};
use contracts::supply_chain::{
    BuildNetworkPolicy, BuildRequest, SigstoreEvidence, VulnerabilitySummary,
};
use contracts::{
    ActorId, ApprovalId, ArtifactId, ArtifactRef, BuildRequestId, CandidateId, CourseId, EventId,
    PolicyId, Revision, Sequence, Sha256Digest, UtcTimestamp,
};
use sqlx::postgres::PgPoolOptions;
use testcontainers::{ImageExt, runners::AsyncRunner};
use testcontainers_modules::postgres::Postgres;

#[derive(Clone)]
struct SlowProvider {
    cleanup_called: Arc<AtomicBool>,
    build_delay: Duration,
}

#[async_trait]
impl BuildSupplyChainProvider for SlowProvider {
    fn builder_binding(&self) -> &'static str {
        "buildkit-primary-v1"
    }

    fn scanner_binding(&self) -> &'static str {
        "trivy-primary-v1"
    }

    fn signer_binding(&self) -> &'static str {
        "sigstore-private-v1"
    }

    fn registry_binding(&self) -> &'static str {
        "harbor-primary-v1"
    }

    async fn ensure_private_project(
        &self,
        command: &AgentBuildRequestedV2,
        identity: BuildIdentity,
    ) -> Result<PrivateRegistryProject, BuildProviderFailure> {
        Ok(PrivateRegistryProject {
            build_request_id: command.request.id,
            build_identity: identity,
            repository_prefix: format!("harbor.internal/course-{}", command.request.course_id),
            private: true,
            storage_quota_bytes: 10 * 1024 * 1024 * 1024,
            robot_subject: format!("robot$course-{}+runtime-puller", command.request.course_id),
        })
    }

    async fn build_candidate(
        &self,
        command: &AgentBuildRequestedV2,
        identity: BuildIdentity,
    ) -> Result<BuiltCandidate, BuildProviderFailure> {
        if !self.build_delay.is_zero() {
            tokio::time::sleep(self.build_delay).await;
        }
        Ok(BuiltCandidate {
            build_request_id: command.request.id,
            build_identity: identity,
            repository: command.request.output_repository.clone(),
            digest: digest(),
            sbom: artifact_ref("application/spdx+json"),
            provenance: artifact_ref("application/vnd.in-toto+json"),
        })
    }

    async fn scan_candidate(
        &self,
        candidate: &BuiltCandidate,
    ) -> Result<ScanEvidence, BuildProviderFailure> {
        Ok(ScanEvidence {
            build_identity: candidate.build_identity,
            digest: candidate.digest.clone(),
            scanner_name: "trivy".to_owned(),
            scanner_version: "0.58.0".to_owned(),
            scanner_database_sha256: Sha256Digest::of_bytes(b"trivy-db"),
            vulnerabilities: VulnerabilitySummary {
                unknown: 0,
                low: 0,
                medium: 0,
                high: 0,
                critical: 0,
            },
        })
    }

    async fn sign_and_verify(
        &self,
        _candidate: &BuiltCandidate,
    ) -> Result<SigstoreEvidence, BuildProviderFailure> {
        Ok(SigstoreEvidence {
            trust_bundle_sha256: Sha256Digest::of_bytes(b"trust-bundle"),
            fulcio_issuer: "https://fulcio.internal".to_owned(),
            certificate_subject: "spiffe://labweaver/image-builder".to_owned(),
            certificate_sha256: Sha256Digest::of_bytes(b"certificate"),
            signature_sha256: Sha256Digest::of_bytes(b"signature"),
            rekor_log_id: "rekor-private-v1".to_owned(),
            rekor_log_index: 7,
            rekor_inclusion_proof_sha256: Sha256Digest::of_bytes(b"rekor-proof"),
            ct_log_id: "ct-private-v1".to_owned(),
            sct_sha256: Sha256Digest::of_bytes(b"sct"),
            verified_at: now(),
        })
    }

    async fn publish_immutable(
        &self,
        candidate: &BuiltCandidate,
    ) -> Result<PublishedImage, BuildProviderFailure> {
        Ok(PublishedImage {
            build_identity: candidate.build_identity,
            digest: candidate.digest.clone(),
            immutable_tag: "build-cancel-test".to_owned(),
        })
    }

    async fn cleanup_candidate(
        &self,
        _build_request_id: BuildRequestId,
        _identity: BuildIdentity,
    ) -> Result<(), BuildProviderFailure> {
        self.cleanup_called.store(true, Ordering::Release);
        Ok(())
    }
}

#[tokio::test]
async fn heartbeat_observes_live_cancellation_and_commits_one_terminal_event()
-> Result<(), Box<dyn std::error::Error>> {
    let container = Postgres::default().with_tag("17.5-alpine").start().await?;
    let url = format!(
        "postgres://postgres:postgres@127.0.0.1:{}/postgres",
        container.get_host_port_ipv4(5432).await?
    );
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&url)
        .await?;
    let migrations = format!(
        "CREATE SCHEMA agent; SET search_path TO agent;\n{}\n{}\n{}\n{}",
        include_str!("../../../migrations/agent/0001_initial.sql"),
        include_str!("../../../migrations/agent/0002_track_leases.sql"),
        include_str!("../../../migrations/agent/0003_control_dispatch.sql"),
        include_str!("../../../migrations/agent/0004_build_pipeline.sql")
    );
    sqlx::raw_sql(&migrations).execute(&pool).await?;

    let command = build_command()?;
    let event = command_event(command.clone())?;
    let store = PgBuildStore::new(pool.clone());
    assert_eq!(
        store
            .accept_command("agent-build-command-v2", &event)
            .await?,
        BuildCommandDecision::Accepted
    );

    let cleanup_called = Arc::new(AtomicBool::new(false));
    let pipeline = BuildPipeline::new(
        SlowProvider {
            cleanup_called: cleanup_called.clone(),
            build_delay: Duration::from_secs(3),
        },
        policy()?,
    )?;
    let lease_duration = Duration::from_millis(120);
    let worker = BuildWorker::new(
        store.clone(),
        pipeline,
        "build-worker-test".to_owned(),
        lease_duration,
        Duration::from_millis(10),
        2,
    )?;
    let worker_task = tokio::spawn(async move { worker.run_once(now()).await });

    wait_until_running(&pool, command.request.id).await?;
    tokio::time::sleep(lease_duration * 3).await;
    let lease_current: bool = sqlx::query_scalar(
        "SELECT lease_expires_at>clock_timestamp() FROM agent.build_commands WHERE build_request_id=$1",
    )
    .bind(command.request.id.as_uuid())
    .fetch_one(&pool)
    .await?;
    assert!(lease_current, "heartbeat must keep the exact lease current");
    assert!(store.request_cancellation(command.request.id).await?);

    let outcome = tokio::time::timeout(Duration::from_secs(2), worker_task).await???;
    assert!(matches!(
        outcome,
        BuildWorkerOutcome::Failed {
            build_request_id,
            diagnostic_code: "LW_AGENT_BUILD_CANCELLED"
        } if build_request_id == command.request.id
    ));
    assert!(cleanup_called.load(Ordering::Acquire));
    let (state, diagnostic, cleanup_verified): (String, String, bool) = sqlx::query_as(
        "SELECT state,diagnostic_code,cleanup_verified FROM agent.build_commands WHERE build_request_id=$1",
    )
    .bind(command.request.id.as_uuid())
    .fetch_one(&pool)
    .await?;
    assert_eq!(state, "cancelled");
    assert_eq!(diagnostic, "LW_AGENT_BUILD_CANCELLED");
    assert!(cleanup_verified);
    let terminal_events: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM agent.outbox_events WHERE aggregate_id=$1 AND subject=$2",
    )
    .bind(command.request.id.as_uuid())
    .bind(subjects::AGENT_BUILD_FAILED_V2)
    .fetch_one(&pool)
    .await?;
    assert_eq!(terminal_events, 1);

    let successful_command = build_command()?;
    assert_eq!(
        store
            .accept_command(
                "agent-build-command-v2",
                &command_event(successful_command.clone())?,
            )
            .await?,
        BuildCommandDecision::Accepted
    );
    let successful_worker = BuildWorker::new(
        store,
        BuildPipeline::new(
            SlowProvider {
                cleanup_called: Arc::new(AtomicBool::new(false)),
                build_delay: Duration::ZERO,
            },
            policy()?,
        )?,
        "build-worker-success".to_owned(),
        Duration::from_secs(1),
        Duration::from_millis(10),
        2,
    )?;
    assert!(matches!(
        successful_worker.run_once(now()).await?,
        BuildWorkerOutcome::Completed { build_request_id }
            if build_request_id == successful_command.request.id
    ));
    let (state, robot_subject, completed_events): (String, String, i64) = sqlx::query_as(
        "SELECT c.state,a.registry_project_evidence->>'robotSubject', \
         (SELECT count(*) FROM agent.outbox_events o WHERE o.aggregate_id=c.build_request_id AND o.subject=$2) \
         FROM agent.build_commands c JOIN agent.image_artifacts a USING (build_request_id) \
         WHERE c.build_request_id=$1",
    )
    .bind(successful_command.request.id.as_uuid())
    .bind(subjects::AGENT_BUILD_COMPLETED_V2)
    .fetch_one(&pool)
    .await?;
    assert_eq!(state, "succeeded");
    assert_eq!(
        robot_subject,
        format!(
            "robot$course-{}+runtime-puller",
            successful_command.request.course_id
        )
    );
    assert_eq!(completed_events, 1);
    Ok(())
}

async fn wait_until_running(
    pool: &sqlx::PgPool,
    build_request_id: BuildRequestId,
) -> Result<(), Box<dyn std::error::Error>> {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let state: Option<String> = sqlx::query_scalar(
                "SELECT state FROM agent.build_commands WHERE build_request_id=$1",
            )
            .bind(build_request_id.as_uuid())
            .fetch_optional(pool)
            .await?;
            if state.as_deref() == Some("running") {
                return Ok::<_, sqlx::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await??;
    Ok(())
}

fn build_command() -> Result<AgentBuildRequestedV2, Box<dyn std::error::Error>> {
    let course_id = CourseId::new();
    let candidate_id = CandidateId::new();
    let candidate_sha256 = Sha256Digest::of_bytes(b"environment-spec");
    let approval_id = ApprovalId::new();
    let request = BuildRequest {
        id: BuildRequestId::new(),
        course_id,
        candidate_id,
        candidate_revision: revision(1)?,
        candidate_sha256,
        approval_id,
        builder_binding: "buildkit-primary-v1".to_owned(),
        context: artifact_ref("application/vnd.oci.image.layer.v1.tar+gzip"),
        dockerfile_path: "Dockerfile".to_owned(),
        base_image_digest: format!("sha256:{}", "c".repeat(64)),
        output_repository: format!("harbor.internal/course-{course_id}/{candidate_id}"),
        network: BuildNetworkPolicy::DenyAll,
        max_duration_milliseconds: 2_000,
        max_cpu_millicores: 2_000,
        max_memory_bytes: 2_147_483_648,
        created_at: now(),
    };
    let approval = CandidateApproval {
        id: approval_id,
        candidate_id,
        candidate_revision: revision(1)?,
        candidate_sha256,
        policy_revision: revision(1)?,
        schema_sha256: Sha256Digest::of_bytes(b"schema"),
        trust_revision: revision(1)?,
        actor_id: ActorId::new(),
        decision: CandidateDecision::Approved,
        reason: "reviewed".to_owned(),
        decided_at: now(),
    };
    let idempotency_key = format!("approval:{approval_id}");
    let command_sha256 = Sha256Digest::of_canonical(&serde_json::json!({
        "request":request,
        "approval":approval,
        "idempotencyKey":idempotency_key,
    }))?;
    Ok(AgentBuildRequestedV2 {
        request,
        approval,
        idempotency_key,
        command_sha256,
    })
}

fn command_event(
    command: AgentBuildRequestedV2,
) -> Result<CloudEvent<AgentBuildRequestedV2>, Box<dyn std::error::Error>> {
    let contract = EVENT_CONTRACTS
        .iter()
        .copied()
        .find(|contract| contract.subject == subjects::AGENT_BUILD_REQUESTED_V2)
        .ok_or("missing v2 build contract")?;
    Ok(CloudEvent {
        specversion: SPEC_VERSION.to_owned(),
        id: EventId::new(),
        source: contract.source().to_owned(),
        event_type: contract.event_type.to_owned(),
        subject: contract.subject.to_owned(),
        time: now(),
        datacontenttype: "application/json".to_owned(),
        dataschema: contract.data_schema(),
        course_id: command.request.course_id,
        aggregate_revision: revision(1)?,
        aggregate_sequence: Sequence(1),
        trace_id: format!("build:{}", command.request.id),
        data: command,
    })
}

fn policy() -> Result<BuildPipelinePolicy, Box<dyn std::error::Error>> {
    Ok(BuildPipelinePolicy {
        builder_binding: "buildkit-primary-v1".to_owned(),
        scanner_binding: "trivy-primary-v1".to_owned(),
        signer_binding: "sigstore-private-v1".to_owned(),
        registry_binding: "harbor-primary-v1".to_owned(),
        policy_id: PolicyId::new(),
        policy_revision: revision(1)?,
        scanner_name: "trivy".to_owned(),
        scanner_version: "0.58.0".to_owned(),
        scanner_database_sha256: Sha256Digest::of_bytes(b"trivy-db"),
        trust_bundle_sha256: Sha256Digest::of_bytes(b"trust-bundle"),
        expected_fulcio_issuer: "https://fulcio.internal".to_owned(),
        expected_certificate_subject: "spiffe://labweaver/image-builder".to_owned(),
        registry_robot_name: "runtime-puller".to_owned(),
        evidence_ttl_milliseconds: 3_600_000,
        stage_timeout: Duration::from_secs(1),
    })
}

fn artifact_ref(media_type: &str) -> ArtifactRef {
    ArtifactRef {
        artifact_id: ArtifactId::new(),
        store_binding: "minio-artifacts-v1".to_owned(),
        object_version: "version-1".to_owned(),
        sha256: Sha256Digest::of_bytes(media_type.as_bytes()),
        size_bytes: 1,
        media_type: media_type.to_owned(),
    }
}

fn digest() -> String {
    format!("sha256:{}", "a".repeat(64))
}

fn revision(value: u64) -> Result<Revision, Box<dyn std::error::Error>> {
    Ok(Revision::new(value)?)
}

fn now() -> UtcTimestamp {
    UtcTimestamp::from_str("2026-07-16T08:00:00.000Z").expect("fixed timestamp is valid")
}
