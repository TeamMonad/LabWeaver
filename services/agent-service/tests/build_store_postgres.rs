//! Real `PostgreSQL` proof for build lease heartbeat, live cancellation, cleanup, and Outbox.
#![allow(
    unused_imports,
    clippy::all,
    clippy::pedantic,
    dead_code,
    unused,
    clippy::expect_used,
    clippy::too_many_lines,
    reason = "one live database test keeps the complete lease and uses fixed validated fixtures"
)]

use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use agent_service::build_pipeline::{
    BUILD_EXECUTOR_PROTOCOL_VERSION, BuildIdentity, BuildPipeline, BuildPipelinePolicy,
    BuildProviderFailure, BuildProviderFailureCode, BuildProviderRequestContext,
    BuildProviderStage, BuildSupplyChainProvider, BuiltCandidate, PrivateRegistryProject,
    PublishedImage,
};
use agent_service::build_provider::{
    BuildExecutorBackend, BuildExecutorFenceError, BuildExecutorRequest,
    BuildExecutorRequestEnvelope, BuildExecutorResponse, FencedBuildExecutor,
    PgBuildExecutorFenceStore,
};
use agent_service::build_store::{
    BuildCommandDecision, BuildWorker, BuildWorkerOutcome, PgBuildStore,
};
use async_trait::async_trait;
use contracts::authoring::{CandidateApproval, CandidateDecision};
use contracts::events::{AgentBuildRequested, CloudEvent, EVENT_CONTRACTS, SPEC_VERSION, subjects};
use contracts::http::{
    IdempotencyKey, InternalAgentBuildCancellationRequest, InternalAgentBuildState,
    InternalAgentBuildStatusQuery,
};
use contracts::supply_chain::{BuildNetworkPolicy, BuildRequest};
use contracts::{
    ActorId, ApprovalId, ArtifactId, ArtifactRef, BuildRequestId, CandidateId, CourseId, EventId,
    Revision, Sequence, UtcTimestamp,
};
use persistence_sqlx::Sha256Digest;
use sqlx::postgres::PgPoolOptions;
use testcontainers::{ImageExt, runners::AsyncRunner};
use testcontainers_modules::postgres::Postgres;

#[derive(Clone)]
struct SlowProvider {
    cleanup_called: Arc<AtomicBool>,
    build_delay: Duration,
    fail_build: bool,
}

#[async_trait]
impl BuildSupplyChainProvider for SlowProvider {
    fn builder_binding(&self) -> &'static str {
        "buildkit-primary-v1"
    }

    fn registry_binding(&self) -> &'static str {
        "harbor-primary-v1"
    }

    async fn ensure_private_project(
        &self,
        _context: &BuildProviderRequestContext,
        command: &AgentBuildRequested,
        identity: BuildIdentity,
    ) -> Result<PrivateRegistryProject, BuildProviderFailure> {
        Ok(PrivateRegistryProject {
            build_request_id: command.request.id,
            build_identity: identity,
            repository_prefix: "harbor.internal/labweaver-system".to_owned(),
            private: true,
            storage_quota_bytes: 10 * 1024 * 1024 * 1024,
            robot_subject: format!("robot$course-{}+runtime-puller", command.request.course_id),
        })
    }

    async fn build_candidate(
        &self,
        _context: &BuildProviderRequestContext,
        command: &AgentBuildRequested,
        identity: BuildIdentity,
    ) -> Result<BuiltCandidate, BuildProviderFailure> {
        if !self.build_delay.is_zero() {
            tokio::time::sleep(self.build_delay).await;
        }
        if self.fail_build {
            return Err(BuildProviderFailure {
                code: BuildProviderFailureCode::Unavailable,
                retryable: true,
            });
        }
        Ok(BuiltCandidate {
            build_request_id: command.request.id,
            build_identity: identity,
            repository: command.request.output_repository.clone(),
            digest: digest(),
        })
    }

    async fn publish_immutable(
        &self,
        _context: &BuildProviderRequestContext,
        candidate: &BuiltCandidate,
    ) -> Result<PublishedImage, BuildProviderFailure> {
        Ok(PublishedImage {
            build_identity: candidate.build_identity,
            digest: candidate.digest.clone(),
        })
    }

    async fn cleanup_candidate(
        &self,
        _context: &BuildProviderRequestContext,
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
    let baseline = format!(
        "CREATE SCHEMA agent; SET search_path TO agent;\n{}\n{}",
        include_str!("../../../migrations/agent/0001_platform_baseline.sql"),
        include_str!("../../../migrations/agent/0002_allow_content_addressed_image_reuse.sql")
    );
    sqlx::raw_sql(&baseline).execute(&pool).await?;

    let command = build_command()?;
    let event = command_event(command.clone())?;
    let store = PgBuildStore::new(pool.clone());
    assert_eq!(
        store
            .accept_command("agent-build-command-v1", &event)
            .await?,
        BuildCommandDecision::Accepted
    );

    let cleanup_called = Arc::new(AtomicBool::new(false));
    let pipeline = BuildPipeline::new(
        SlowProvider {
            cleanup_called: cleanup_called.clone(),
            build_delay: Duration::from_secs(3),
            fail_build: false,
        },
        policy()?,
    )?;
    // Keep the lease comfortably above normal CI scheduler and PostgreSQL
    // round-trip jitter. The assertion below observes an actual renewal rather
    // than relying on a fixed sleep near the expiry boundary.
    let lease_duration = Duration::from_secs(1);
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
    let initial_lease_expires_at: time::OffsetDateTime = sqlx::query_scalar(
        "SELECT lease_expires_at FROM agent.build_commands WHERE build_request_id=$1",
    )
    .bind(command.request.id.as_uuid())
    .fetch_one(&pool)
    .await?;
    wait_until_lease_renewed(&pool, command.request.id, initial_lease_expires_at).await?;
    let lease_current: bool = sqlx::query_scalar(
        "SELECT lease_expires_at>clock_timestamp() FROM agent.build_commands WHERE build_request_id=$1",
    )
    .bind(command.request.id.as_uuid())
    .fetch_one(&pool)
    .await?;
    assert!(lease_current, "heartbeat must keep the exact lease current");
    let cancellation_requested_at = database_now(&pool).await?;
    let running = store
        .load_status(
            command.request.id,
            &InternalAgentBuildStatusQuery {
                course_id: command.request.course_id,
            },
        )
        .await?;
    assert_eq!(running.state, InternalAgentBuildState::Running);
    assert_eq!(running.revision, revision(2)?);
    let cancellation = InternalAgentBuildCancellationRequest {
        course_id: command.request.course_id,
        build_request_id: command.request.id,
        expected_state: running.state,
        expected_revision: running.revision,
        actor_id: ActorId::new(),
        authority_san_uri: "spiffe://labweaver/control-service".to_owned(),
        requested_at: cancellation_requested_at,
    };
    let cancellation_key = IdempotencyKey::parse(&format!("cancel:{}", command.request.id))?;
    let result = store
        .request_cancellation(&cancellation, &cancellation_key)
        .await?;
    assert!(result.cancellation_requested);
    assert_eq!(result.revision, revision(3)?);
    assert_eq!(
        store
            .request_cancellation(&cancellation, &cancellation_key)
            .await?,
        result
    );

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
    .bind(subjects::AGENT_BUILD_FAILED)
    .fetch_one(&pool)
    .await?;
    assert_eq!(terminal_events, 1);

    let successful_command = build_command()?;
    assert_eq!(
        store
            .accept_command(
                "agent-build-command-v1",
                &command_event(successful_command.clone())?,
            )
            .await?,
        BuildCommandDecision::Accepted
    );
    let successful_worker = BuildWorker::new(
        store.clone(),
        BuildPipeline::new(
            SlowProvider {
                cleanup_called: Arc::new(AtomicBool::new(false)),
                build_delay: Duration::ZERO,
                fail_build: false,
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
    .bind(subjects::AGENT_BUILD_COMPLETED)
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

    let repeated_content_command = build_command()?;
    assert_eq!(
        store
            .accept_command(
                "agent-build-command-v1",
                &command_event(repeated_content_command.clone())?,
            )
            .await?,
        BuildCommandDecision::Accepted
    );
    let repeated_content_worker = BuildWorker::new(
        store.clone(),
        BuildPipeline::new(
            SlowProvider {
                cleanup_called: Arc::new(AtomicBool::new(false)),
                build_delay: Duration::ZERO,
                fail_build: false,
            },
            policy()?,
        )?,
        "build-worker-repeated-content".to_owned(),
        Duration::from_secs(1),
        Duration::from_millis(10),
        2,
    )?;
    assert!(matches!(
        repeated_content_worker.run_once(now()).await?,
        BuildWorkerOutcome::Completed { build_request_id }
            if build_request_id == repeated_content_command.request.id
    ));
    let repeated_digest_artifacts: i64 =
        sqlx::query_scalar("SELECT count(*) FROM agent.image_artifacts WHERE image_digest=$1")
            .bind(digest())
            .fetch_one(&pool)
            .await?;
    assert_eq!(repeated_digest_artifacts, 2);

    let retry_command = build_command()?;
    assert_eq!(
        store
            .accept_command(
                "agent-build-command-v1",
                &command_event(retry_command.clone())?,
            )
            .await?,
        BuildCommandDecision::Accepted
    );
    let retry_worker = BuildWorker::new(
        store,
        BuildPipeline::new(
            SlowProvider {
                cleanup_called: Arc::new(AtomicBool::new(false)),
                build_delay: Duration::from_millis(50),
                fail_build: true,
            },
            policy()?,
        )?,
        "build-worker-retry".to_owned(),
        Duration::from_secs(1),
        Duration::from_millis(10),
        2,
    )?;
    assert!(matches!(
        retry_worker.run_once(now()).await?,
        BuildWorkerOutcome::RetryScheduled { build_request_id, attempt: 1 }
            if build_request_id == retry_command.request.id
    ));
    let (retry_state, retry_is_future): (String, bool) = sqlx::query_as(
        "SELECT state,next_attempt_at>updated_at FROM agent.build_commands WHERE build_request_id=$1",
    )
    .bind(retry_command.request.id.as_uuid())
    .fetch_one(&pool)
    .await?;
    assert_eq!(retry_state, "requested");
    assert!(
        retry_is_future,
        "retry must use the post-provider database clock"
    );
    Ok(())
}

#[derive(Clone)]
struct CountingBuildExecutor {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl BuildExecutorBackend for CountingBuildExecutor {
    async fn execute(
        &self,
        _context: &BuildProviderRequestContext,
        request: &BuildExecutorRequest,
    ) -> BuildExecutorResponse {
        self.calls.fetch_add(1, Ordering::SeqCst);
        match request {
            BuildExecutorRequest::EnsurePrivateProject { command, identity } => {
                BuildExecutorResponse::PrivateProjectReady {
                    project: PrivateRegistryProject {
                        build_request_id: command.request.id,
                        build_identity: *identity,
                        repository_prefix: "harbor.internal/labweaver-system".to_owned(),
                        private: true,
                        storage_quota_bytes: 1,
                        robot_subject: "robot$runtime".to_owned(),
                    },
                }
            }
            BuildExecutorRequest::Cleanup {
                build_request_id,
                identity,
            } => BuildExecutorResponse::Cleaned {
                build_request_id: *build_request_id,
                build_identity: *identity,
            },
            _ => BuildExecutorResponse::Failed {
                failure: BuildProviderFailure {
                    code: BuildProviderFailureCode::Rejected,
                    retryable: false,
                },
            },
        }
    }
}

#[tokio::test]
async fn executor_fence_survives_restart_and_cleanup_dominates_its_generation()
-> Result<(), Box<dyn std::error::Error>> {
    let container = Postgres::default().with_tag("17.5-alpine").start().await?;
    let url = format!(
        "postgres://postgres:postgres@127.0.0.1:{}/postgres",
        container.get_host_port_ipv4(5432).await?
    );
    let pool = PgPoolOptions::new()
        .max_connections(3)
        .connect(&url)
        .await?;
    let migrations = format!(
        "CREATE SCHEMA agent; SET search_path TO agent;\n{}\n{}",
        include_str!("../../../migrations/agent/0001_platform_baseline.sql"),
        include_str!("../../../migrations/agent/0002_allow_content_addressed_image_reuse.sql")
    );
    sqlx::raw_sql(&migrations).execute(&pool).await?;
    let command = build_command()?;
    let deadline = add_time(database_now(&pool).await?, time::Duration::minutes(1))?;
    let calls = Arc::new(AtomicUsize::new(0));
    let lease_one = uuid::Uuid::new_v4();
    let first = build_executor_envelope(
        &command,
        1,
        lease_one,
        BuildProviderStage::EnsurePrivateProject,
        deadline,
    );
    FencedBuildExecutor::new(
        PgBuildExecutorFenceStore::new(pool.clone()),
        CountingBuildExecutor {
            calls: calls.clone(),
        },
    )
    .execute(first.clone())
    .await?;
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // A newly constructed executor replays the persisted response without repeating the effect.
    FencedBuildExecutor::new(
        PgBuildExecutorFenceStore::new(pool.clone()),
        CountingBuildExecutor {
            calls: calls.clone(),
        },
    )
    .execute(first)
    .await?;
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    let lease_two = uuid::Uuid::new_v4();
    let second = build_executor_envelope(
        &command,
        2,
        lease_two,
        BuildProviderStage::EnsurePrivateProject,
        deadline,
    );
    let executor = FencedBuildExecutor::new(
        PgBuildExecutorFenceStore::new(pool.clone()),
        CountingBuildExecutor {
            calls: calls.clone(),
        },
    );
    executor.execute(second).await?;
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    let delayed_cleanup = build_executor_envelope(
        &command,
        1,
        lease_one,
        BuildProviderStage::Cleanup,
        deadline,
    );
    assert!(matches!(
        executor.execute(delayed_cleanup).await,
        Err(BuildExecutorFenceError::StaleGeneration)
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    executor
        .execute(build_executor_envelope(
            &command,
            2,
            lease_two,
            BuildProviderStage::Cleanup,
            deadline,
        ))
        .await?;
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert!(matches!(
        executor
            .execute(build_executor_envelope(
                &command,
                2,
                lease_two,
                BuildProviderStage::Build,
                deadline,
            ))
            .await,
        Err(BuildExecutorFenceError::Tombstoned)
    ));

    // Build cleanup tombstones only the failed attempt; a strictly newer lease may retry.
    executor
        .execute(build_executor_envelope(
            &command,
            3,
            uuid::Uuid::new_v4(),
            BuildProviderStage::EnsurePrivateProject,
            deadline,
        ))
        .await?;
    assert_eq!(calls.load(Ordering::SeqCst), 4);

    let expired_command = build_command()?;
    assert!(matches!(
        executor
            .execute(build_executor_envelope(
                &expired_command,
                1,
                uuid::Uuid::new_v4(),
                BuildProviderStage::EnsurePrivateProject,
                add_time(database_now(&pool).await?, time::Duration::seconds(-1))?,
            ))
            .await,
        Err(BuildExecutorFenceError::DeadlineExceeded)
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 4);
    Ok(())
}

fn build_executor_envelope(
    command: &AgentBuildRequested,
    generation: u32,
    lease_token: uuid::Uuid,
    stage: BuildProviderStage,
    deadline_at: UtcTimestamp,
) -> BuildExecutorRequestEnvelope {
    let request = match stage {
        BuildProviderStage::EnsurePrivateProject => BuildExecutorRequest::EnsurePrivateProject {
            command: command.clone(),
            identity: BuildIdentity(persistence_sqlx::Sha256Digest::of_bytes(
                command.request.id.as_uuid().as_bytes(),
            )),
        },
        BuildProviderStage::Build => BuildExecutorRequest::Build {
            command: command.clone(),
            identity: BuildIdentity(persistence_sqlx::Sha256Digest::of_bytes(
                command.request.id.as_uuid().as_bytes(),
            )),
        },
        BuildProviderStage::Cleanup => BuildExecutorRequest::Cleanup {
            build_request_id: command.request.id,
            identity: BuildIdentity(persistence_sqlx::Sha256Digest::of_bytes(
                command.request.id.as_uuid().as_bytes(),
            )),
        },
        _ => unreachable!("fixture uses only command-bound stages"),
    };
    let stage_request_id = Sha256Digest::of_canonical(&serde_json::json!({
        "protocolVersion": BUILD_EXECUTOR_PROTOCOL_VERSION,
        "buildRequestId": command.request.id,
        "fenceGeneration": generation,
        "leaseToken": lease_token,
        "stage": stage,
        "deadlineAt": deadline_at,
        "request": &request,
    }))
    .expect("executor request identity");
    BuildExecutorRequestEnvelope {
        context: BuildProviderRequestContext {
            protocol_version: BUILD_EXECUTOR_PROTOCOL_VERSION,
            build_request_id: command.request.id,
            fence_generation: generation,
            lease_token,
            stage,
            stage_request_id,
            deadline_at,
        },
        request,
    }
}

fn add_time(
    timestamp: UtcTimestamp,
    duration: time::Duration,
) -> Result<UtcTimestamp, Box<dyn std::error::Error>> {
    Ok(UtcTimestamp::from_utc(timestamp.get() + duration)?)
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

async fn wait_until_lease_renewed(
    pool: &sqlx::PgPool,
    build_request_id: BuildRequestId,
    initial_lease_expires_at: time::OffsetDateTime,
) -> Result<(), Box<dyn std::error::Error>> {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let lease_expires_at: time::OffsetDateTime = sqlx::query_scalar(
                "SELECT lease_expires_at FROM agent.build_commands WHERE build_request_id=$1",
            )
            .bind(build_request_id.as_uuid())
            .fetch_one(pool)
            .await?;
            if lease_expires_at > initial_lease_expires_at {
                return Ok::<_, sqlx::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await??;
    Ok(())
}

async fn database_now(pool: &sqlx::PgPool) -> Result<UtcTimestamp, Box<dyn std::error::Error>> {
    let value: time::OffsetDateTime =
        sqlx::query_scalar("SELECT date_trunc('milliseconds',clock_timestamp())")
            .fetch_one(pool)
            .await?;
    Ok(UtcTimestamp::from_utc(value)?)
}

fn build_command() -> Result<AgentBuildRequested, Box<dyn std::error::Error>> {
    let course_id = CourseId::new();
    let candidate_id = CandidateId::new();
    let approval_id = ApprovalId::new();
    let request = BuildRequest {
        id: BuildRequestId::new(),
        course_id,
        candidate_id,
        candidate_revision: revision(1)?,
        approval_id,
        builder_binding: "buildkit-primary-v1".to_owned(),
        context: artifact_ref("application/vnd.oci.image.layer.v1.tar+gzip"),
        context_object_key: "build-contexts/context.tar.gz".to_owned(),
        dockerfile_path: "Dockerfile".to_owned(),
        base_image_digest: format!("sha256:{}", "c".repeat(64)),
        output_repository: format!(
            "harbor.internal/labweaver-system/course-{course_id}-{candidate_id}"
        ),
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
        policy_revision: revision(1)?,
        trust_revision: revision(1)?,
        actor_id: ActorId::new(),
        decision: CandidateDecision::Approved,
        reason: "reviewed".to_owned(),
        decided_at: now(),
    };
    let idempotency_key = format!("approval:{approval_id}");
    Ok(AgentBuildRequested {
        request,
        approval,
        idempotency_key,
    })
}

fn command_event(
    command: AgentBuildRequested,
) -> Result<CloudEvent<AgentBuildRequested>, Box<dyn std::error::Error>> {
    let contract = EVENT_CONTRACTS
        .iter()
        .copied()
        .find(|contract| contract.subject == subjects::AGENT_BUILD_REQUESTED)
        .ok_or("missing v1 build contract")?;
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
        registry_binding: "harbor-primary-v1".to_owned(),
        registry_robot_name: "runtime-puller".to_owned(),
        stage_timeout: Duration::from_secs(1),
    })
}

fn artifact_ref(media_type: &str) -> ArtifactRef {
    ArtifactRef {
        artifact_id: ArtifactId::new(),
        store_binding: "minio-artifacts-v1".to_owned(),
        object_version: "version-1".to_owned(),
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
