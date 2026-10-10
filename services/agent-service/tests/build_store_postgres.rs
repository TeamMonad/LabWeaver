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
use contracts::events::{AgentBuildRequested, CloudEvent, EVENT_CONTRACTS, SPEC_VERSION, subjects};
use contracts::http::{
    IdempotencyKey, InternalAgentBuildCancellationRequest, InternalAgentBuildState,
    InternalAgentBuildStatusQuery,
};
use contracts::supply_chain::{BuildNetworkPolicy, BuildRequest};
use contracts::{
    ActorId, ArtifactId, ArtifactRef, BuildRequestId, CandidateId, CourseId, EventId, ProjectId,
    Revision, Sequence, UtcTimestamp,
};
use persistence_sqlx::Sha256Digest;
use sqlx::postgres::PgPoolOptions;
use testcontainers::{ImageExt, runners::AsyncRunner};
use testcontainers_modules::postgres::Postgres;

mod support;
use support::apply_agent_migrations;

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
            robot_subject: format!(
                "robot$project-{}+runtime-puller",
                command.request.project_id
            ),
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

    async fn import_candidate(
        &self,
        _context: &BuildProviderRequestContext,
        command: &AgentBuildRequested,
        identity: BuildIdentity,
    ) -> Result<BuiltCandidate, BuildProviderFailure> {
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
    apply_agent_migrations(&pool).await?;

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
                project_id: command.request.project_id,
                course_id: command.request.course_id,
            },
        )
        .await?;
    assert_eq!(running.state, InternalAgentBuildState::Running);
    assert_eq!(running.revision, revision(2)?);
    let cancellation = InternalAgentBuildCancellationRequest {
        project_id: command.request.project_id,
        course_id: command.request.course_id,
        build_request_id: command.request.id,
        expected_state: running.state,
        expected_revision: running.revision,
        actor_id: ActorId::new(),
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
            "robot$project-{}+runtime-puller",
            successful_command.request.project_id
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
        _cancellation: &tokio_util::sync::CancellationToken,
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
                ..
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
    apply_agent_migrations(&pool).await?;
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
        BuildProviderStage::Import => BuildExecutorRequest::Import {
            command: command.clone(),
            identity: BuildIdentity(Sha256Digest::of_bytes(
                command.request.id.as_uuid().as_bytes(),
            )),
        },
        BuildProviderStage::Cleanup => BuildExecutorRequest::Cleanup {
            build_request_id: command.request.id,
            identity: BuildIdentity(persistence_sqlx::Sha256Digest::of_bytes(
                command.request.id.as_uuid().as_bytes(),
            )),
            timeout_milliseconds: 120_000,
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
    let project_id = ProjectId::new();
    let course_id = CourseId::new();
    let candidate_id = CandidateId::new();
    let request = BuildRequest {
        id: BuildRequestId::new(),
        project_id,
        course_id: Some(course_id),
        candidate_id,
        candidate_revision: revision(1)?,
        builder_binding: "buildkit-primary-v1".to_owned(),
        source: contracts::supply_chain::BuildSource::Dockerfile {
            context: artifact_ref("application/vnd.oci.image.layer.v1.tar+gzip"),
            context_object_key: "build-contexts/context.tar.gz".to_owned(),
            dockerfile_path: "Dockerfile".to_owned(),
        },
        output_repository: format!(
            "harbor.internal/labweaver-system/course-{course_id}-{candidate_id}"
        ),
        network: BuildNetworkPolicy::DenyAll,
        max_duration_milliseconds: 2_000,
        max_cpu_millicores: 2_000,
        max_memory_bytes: 2_147_483_648,
        created_at: now(),
    };
    let idempotency_key = format!("build:{}", request.id);
    Ok(AgentBuildRequested {
        request,
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
        project_id: command.request.project_id,
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

#[derive(Clone)]
struct CancellableBuildExecutor {
    started: tokio_util::sync::CancellationToken,
    stopped: tokio_util::sync::CancellationToken,
    confirmed: bool,
}

#[async_trait]
impl BuildExecutorBackend for CancellableBuildExecutor {
    async fn execute(
        &self,
        context: &BuildProviderRequestContext,
        request: &BuildExecutorRequest,
        cancellation: &tokio_util::sync::CancellationToken,
    ) -> BuildExecutorResponse {
        match request {
            BuildExecutorRequest::Build { .. } => {
                self.started.cancel();
                cancellation.cancelled().await;
                self.stopped.cancel();
                BuildExecutorResponse::Failed {
                    failure: BuildProviderFailure {
                        code: if self.confirmed {
                            BuildProviderFailureCode::Cancelled
                        } else {
                            BuildProviderFailureCode::ExecutionUnknown
                        },
                        retryable: false,
                    },
                }
            }
            BuildExecutorRequest::Cleanup {
                build_request_id,
                identity,
                ..
            } => {
                assert!(
                    self.stopped.is_cancelled(),
                    "cleanup must follow joined execution"
                );
                BuildExecutorResponse::Cleaned {
                    build_request_id: *build_request_id,
                    build_identity: *identity,
                }
            }
            _ => unreachable!("unused external boundary"),
        }
    }
}

#[tokio::test]
async fn executor_cleanup_signals_exact_active_build_and_unknown_stop_keeps_fence_closed()
-> Result<(), Box<dyn std::error::Error>> {
    let container = Postgres::default().with_tag("17.5-alpine").start().await?;
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&format!(
            "postgres://postgres:postgres@127.0.0.1:{}/postgres",
            container.get_host_port_ipv4(5432).await?
        ))
        .await?;
    apply_agent_migrations(&pool).await?;
    for confirmed in [true, false] {
        let command = build_command()?;
        let deadline = add_time(database_now(&pool).await?, time::Duration::seconds(10))?;
        let lease = uuid::Uuid::new_v4();
        let started = tokio_util::sync::CancellationToken::new();
        let stopped = tokio_util::sync::CancellationToken::new();
        let executor = Arc::new(FencedBuildExecutor::new(
            PgBuildExecutorFenceStore::new(pool.clone()),
            CancellableBuildExecutor {
                started: started.clone(),
                stopped: stopped.clone(),
                confirmed,
            },
        ));
        let build =
            build_executor_envelope(&command, 1, lease, BuildProviderStage::Build, deadline);
        let active = executor.clone();
        let task = tokio::spawn(async move { active.execute(build).await });
        started.cancelled().await;
        let cleanup = executor
            .execute(build_executor_envelope(
                &command,
                1,
                lease,
                BuildProviderStage::Cleanup,
                deadline,
            ))
            .await;
        assert!(stopped.is_cancelled());
        task.await??;
        if confirmed {
            assert!(matches!(
                cleanup?.response,
                BuildExecutorResponse::Cleaned { .. }
            ));
        } else {
            assert!(matches!(cleanup, Err(BuildExecutorFenceError::InProgress)));
            // Deadline expiry never authorizes a newer producer over an unknown operation.
            sqlx::query("UPDATE agent.build_executor_fences SET deadline_at=clock_timestamp()-interval '1 second' WHERE build_request_id=$1")
                .bind(command.request.id.as_uuid()).execute(&pool).await?;
            let later = executor
                .execute(build_executor_envelope(
                    &command,
                    2,
                    uuid::Uuid::new_v4(),
                    BuildProviderStage::Build,
                    deadline,
                ))
                .await;
            assert!(matches!(later, Err(BuildExecutorFenceError::InProgress)));
            let (generation, response): (i32, Option<serde_json::Value>) = sqlx::query_as("SELECT highest_generation,last_response FROM agent.build_executor_fences WHERE build_request_id=$1")
                .bind(command.request.id.as_uuid()).fetch_one(&pool).await?;
            assert_eq!(generation, 1);
            assert!(response.is_none());
        }
    }
    Ok(())
}

#[derive(Clone)]
struct RecoveryBuildExecutor {
    pool: sqlx::PgPool,
    cleanups: Arc<AtomicUsize>,
}

#[async_trait]
impl BuildExecutorBackend for RecoveryBuildExecutor {
    async fn execute(
        &self,
        _: &BuildProviderRequestContext,
        request: &BuildExecutorRequest,
        _: &tokio_util::sync::CancellationToken,
    ) -> BuildExecutorResponse {
        match request {
            BuildExecutorRequest::Build { command, identity } => BuildExecutorResponse::Built {
                candidate: BuiltCandidate {
                    build_request_id: command.request.id,
                    build_identity: *identity,
                    repository: command.request.output_repository.clone(),
                    digest: digest(),
                },
            },
            BuildExecutorRequest::Cleanup {
                build_request_id,
                identity,
                ..
            } => {
                self.cleanups.fetch_add(1, Ordering::SeqCst);
                sqlx::query("UPDATE agent.build_executor_artifacts SET cleaned_at=clock_timestamp() WHERE build_request_id=$1 AND build_identity=$2")
                    .bind(build_request_id.as_uuid()).bind(identity.0.to_string()).execute(&self.pool).await.expect("external registry adapter completion");
                BuildExecutorResponse::Cleaned {
                    build_request_id: *build_request_id,
                    build_identity: *identity,
                }
            }
            _ => unreachable!("unused registry boundary"),
        }
    }
}

struct RecoveryProvider<B> {
    executor: FencedBuildExecutor<B>,
    command: AgentBuildRequested,
}

#[async_trait]
impl<B: BuildExecutorBackend> BuildSupplyChainProvider for RecoveryProvider<B> {
    fn builder_binding(&self) -> &str {
        "buildkit-primary-v1"
    }
    fn registry_binding(&self) -> &str {
        "harbor-primary-v1"
    }
    async fn ensure_private_project(
        &self,
        _: &BuildProviderRequestContext,
        _: &AgentBuildRequested,
        _: BuildIdentity,
    ) -> Result<PrivateRegistryProject, BuildProviderFailure> {
        unreachable!("cleanup must not rebuild")
    }
    async fn build_candidate(
        &self,
        _: &BuildProviderRequestContext,
        _: &AgentBuildRequested,
        _: BuildIdentity,
    ) -> Result<BuiltCandidate, BuildProviderFailure> {
        unreachable!("cleanup must not rebuild")
    }
    async fn import_candidate(
        &self,
        _: &BuildProviderRequestContext,
        _: &AgentBuildRequested,
        _: BuildIdentity,
    ) -> Result<BuiltCandidate, BuildProviderFailure> {
        unreachable!("cleanup must not import")
    }
    async fn publish_immutable(
        &self,
        _: &BuildProviderRequestContext,
        _: &BuiltCandidate,
    ) -> Result<PublishedImage, BuildProviderFailure> {
        unreachable!("failed command must not publish")
    }
    async fn cleanup_candidate(
        &self,
        context: &BuildProviderRequestContext,
        build_request_id: BuildRequestId,
        identity: BuildIdentity,
    ) -> Result<(), BuildProviderFailure> {
        assert_eq!(build_request_id, self.command.request.id);
        assert_eq!(
            identity.0,
            Sha256Digest::of_bytes(build_request_id.as_uuid().as_bytes())
        );
        let response = self
            .executor
            .execute(build_executor_envelope(
                &self.command,
                context.fence_generation,
                context.lease_token,
                BuildProviderStage::Cleanup,
                context.deadline_at,
            ))
            .await
            .expect("exact terminal cleanup admission");
        assert!(matches!(
            response.response,
            BuildExecutorResponse::Cleaned { .. }
        ));
        Ok(())
    }
}

#[tokio::test]
async fn terminal_late_build_cleanup_preserves_failure_and_replays_completed_cleanup()
-> Result<(), Box<dyn std::error::Error>> {
    let container = Postgres::default().with_tag("17.5-alpine").start().await?;
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&format!(
            "postgres://postgres:postgres@127.0.0.1:{}/postgres",
            container.get_host_port_ipv4(5432).await?
        ))
        .await?;
    apply_agent_migrations(&pool).await?;
    let command = build_command()?;
    let store = PgBuildStore::new(pool.clone());
    store
        .accept_command("agent-build-command-v1", &command_event(command.clone())?)
        .await?;
    sqlx::query("UPDATE agent.build_commands SET state='failed',attempt=1,diagnostic_code='LW_AGENT_BUILD_CLEANUP_FAILED',retryable=false,cleanup_verified=false,completed_at=clock_timestamp() WHERE build_request_id=$1")
        .bind(command.request.id.as_uuid()).execute(&pool).await?;
    let identity = Sha256Digest::of_bytes(command.request.id.as_uuid().as_bytes());
    sqlx::query("INSERT INTO agent.build_executor_artifacts(build_request_id,build_identity,repository,project_name,repository_name,candidate_tag,digest) VALUES($1,$2,$3,'labweaver-system',$6,$4,$5)")
        .bind(command.request.id.as_uuid()).bind(identity.to_string()).bind(&command.request.output_repository)
        .bind(format!("candidate-{}", &identity.to_string()[..24])).bind(digest()).bind(command.request.output_repository.rsplit('/').next().ok_or("repository")?).execute(&pool).await?;
    let cleanups = Arc::new(AtomicUsize::new(0));
    let executor = FencedBuildExecutor::new(
        PgBuildExecutorFenceStore::new(pool.clone()),
        RecoveryBuildExecutor {
            pool: pool.clone(),
            cleanups: cleanups.clone(),
        },
    );
    let deadline = add_time(
        database_now(&pool).await?,
        time::Duration::milliseconds(500),
    )?;
    executor
        .execute(build_executor_envelope(
            &command,
            1,
            uuid::Uuid::new_v4(),
            BuildProviderStage::Build,
            deadline,
        ))
        .await?;
    tokio::time::sleep(Duration::from_millis(550)).await;
    let worker = BuildWorker::new(
        store,
        BuildPipeline::new(
            RecoveryProvider {
                executor,
                command: command.clone(),
            },
            policy()?,
        )?,
        "cleanup-recovery".to_owned(),
        Duration::from_secs(1),
        Duration::from_millis(10),
        2,
    )?;
    worker.run_once(now()).await?;
    let (state, diagnostic, cleaned): (String, String, bool) = sqlx::query_as("SELECT state,diagnostic_code,cleanup_verified FROM agent.build_commands WHERE build_request_id=$1")
        .bind(command.request.id.as_uuid()).fetch_one(&pool).await?;
    assert_eq!(state, "failed");
    assert_eq!(diagnostic, "LW_AGENT_BUILD_CLEANUP_FAILED");
    assert!(cleaned);
    assert_eq!(cleanups.load(Ordering::SeqCst), 1);
    // Simulate interruption after executor completion but before authority flag delivery.
    sqlx::query("UPDATE agent.build_commands SET cleanup_verified=false,next_attempt_at=clock_timestamp() WHERE build_request_id=$1")
        .bind(command.request.id.as_uuid()).execute(&pool).await?;
    worker.run_once(now()).await?;
    assert_eq!(
        cleanups.load(Ordering::SeqCst),
        1,
        "canonical cleanup replay has no second side effect"
    );
    let cleaned: bool = sqlx::query_scalar(
        "SELECT cleanup_verified FROM agent.build_commands WHERE build_request_id=$1",
    )
    .bind(command.request.id.as_uuid())
    .fetch_one(&pool)
    .await?;
    assert!(cleaned);
    Ok(())
}

struct RegistryTlsListener {
    listener: tokio::net::TcpListener,
    acceptor: tokio_rustls::TlsAcceptor,
}

#[derive(Clone)]
struct RetryCleanupExecutor {
    pool: sqlx::PgPool,
    builds: Arc<AtomicUsize>,
    cleanups: Arc<AtomicUsize>,
    fail_cleanup_once: Arc<AtomicBool>,
    block_retry: bool,
    retry_entered: Arc<tokio::sync::Notify>,
    retry_continue: Arc<tokio::sync::Notify>,
}

impl RetryCleanupExecutor {
    fn new(pool: sqlx::PgPool) -> Self {
        Self {
            pool,
            builds: Arc::new(AtomicUsize::new(0)),
            cleanups: Arc::new(AtomicUsize::new(0)),
            fail_cleanup_once: Arc::new(AtomicBool::new(true)),
            block_retry: false,
            retry_entered: Arc::new(tokio::sync::Notify::new()),
            retry_continue: Arc::new(tokio::sync::Notify::new()),
        }
    }
}

#[async_trait]
impl BuildExecutorBackend for RetryCleanupExecutor {
    async fn execute(
        &self,
        _: &BuildProviderRequestContext,
        request: &BuildExecutorRequest,
        _: &tokio_util::sync::CancellationToken,
    ) -> BuildExecutorResponse {
        match request {
            BuildExecutorRequest::Build { .. } => {
                self.builds.fetch_add(1, Ordering::SeqCst);
                BuildExecutorResponse::Failed {
                    failure: BuildProviderFailure {
                        code: BuildProviderFailureCode::Unavailable,
                        retryable: true,
                    },
                }
            }
            BuildExecutorRequest::Cleanup {
                build_request_id,
                identity,
                ..
            } => {
                self.cleanups.fetch_add(1, Ordering::SeqCst);
                if self.fail_cleanup_once.swap(false, Ordering::SeqCst) {
                    return BuildExecutorResponse::Failed {
                        failure: BuildProviderFailure {
                            code: BuildProviderFailureCode::Unavailable,
                            retryable: true,
                        },
                    };
                }
                if self.block_retry {
                    self.retry_entered.notify_one();
                    self.retry_continue.notified().await;
                }
                sqlx::query("UPDATE agent.build_executor_artifacts SET cleaned_at=clock_timestamp() WHERE build_request_id=$1 AND build_identity=$2")
                    .bind(build_request_id.as_uuid()).bind(identity.0.to_string()).execute(&self.pool).await.expect("registry cleanup boundary");
                BuildExecutorResponse::Cleaned {
                    build_request_id: *build_request_id,
                    build_identity: *identity,
                }
            }
            _ => unreachable!("terminal cleanup must not import or publish"),
        }
    }
}

async fn terminal_cleanup_fixture(
    pool: &sqlx::PgPool,
    artifact: bool,
    cancelled: bool,
) -> Result<
    (
        AgentBuildRequested,
        BuildExecutorRequestEnvelope,
        RetryCleanupExecutor,
    ),
    Box<dyn std::error::Error>,
> {
    use agent_service::build_pipeline::{BuildFailureCode, BuildPipelineError};
    let command = build_command()?;
    let store = PgBuildStore::new(pool.clone());
    store
        .accept_command("agent-build-command-v1", &command_event(command.clone())?)
        .await?;
    let lease = store
        .claim_due("original-build", Duration::from_secs(10))
        .await?
        .ok_or("lease")?;
    let token: uuid::Uuid = sqlx::query_scalar(
        "SELECT lease_token FROM agent.build_commands WHERE build_request_id=$1",
    )
    .bind(command.request.id.as_uuid())
    .fetch_one(pool)
    .await?;
    let deadline = add_time(database_now(pool).await?, time::Duration::seconds(2))?;
    let backend = RetryCleanupExecutor::new(pool.clone());
    let executor = FencedBuildExecutor::new(
        PgBuildExecutorFenceStore::new(pool.clone()),
        backend.clone(),
    );
    executor
        .execute(build_executor_envelope(
            &command,
            lease.attempt,
            token,
            BuildProviderStage::Build,
            deadline,
        ))
        .await?;
    if artifact {
        let identity = Sha256Digest::of_bytes(command.request.id.as_uuid().as_bytes());
        sqlx::query("INSERT INTO agent.build_executor_artifacts(build_request_id,build_identity,repository,project_name,repository_name,candidate_tag,digest) VALUES($1,$2,$3,'labweaver-system',$6,$4,$5)")
            .bind(command.request.id.as_uuid()).bind(identity.to_string()).bind(&command.request.output_repository)
            .bind(format!("candidate-{}", &identity.to_string()[..24])).bind(digest())
            .bind(command.request.output_repository.rsplit('/').next().ok_or("repository")?).execute(pool).await?;
    }
    let cleanup = build_executor_envelope(
        &command,
        lease.attempt,
        token,
        BuildProviderStage::Cleanup,
        deadline,
    );
    assert!(matches!(
        executor.execute(cleanup.clone()).await?.response,
        BuildExecutorResponse::Failed { .. }
    ));
    store
        .fail(
            &lease,
            BuildPipelineError {
                code: if cancelled {
                    BuildFailureCode::Cancelled
                } else {
                    BuildFailureCode::Provider(BuildProviderFailureCode::Unavailable)
                },
                retryable: false,
                cleanup_verified: false,
            },
            "original-failure",
        )
        .await?;
    Ok((command, cleanup, backend))
}

fn terminal_cleanup_worker(
    pool: &sqlx::PgPool,
    command: &AgentBuildRequested,
    backend: RetryCleanupExecutor,
) -> Result<BuildWorker<RecoveryProvider<RetryCleanupExecutor>>, Box<dyn std::error::Error>> {
    Ok(BuildWorker::new(
        PgBuildStore::new(pool.clone()),
        BuildPipeline::new(
            RecoveryProvider {
                executor: FencedBuildExecutor::new(
                    PgBuildExecutorFenceStore::new(pool.clone()),
                    backend,
                ),
                command: command.clone(),
            },
            policy()?,
        )?,
        "cleanup-recovery".to_owned(),
        Duration::from_secs(1),
        Duration::from_millis(10),
        2,
    )?)
}

async fn wait_for_compute_deadline(
    pool: &sqlx::PgPool,
    deadline: UtcTimestamp,
) -> Result<(), Box<dyn std::error::Error>> {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if database_now(pool).await? >= deadline {
                return Ok::<_, Box<dyn std::error::Error>>(());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await??;
    Ok(())
}

#[tokio::test]
async fn terminal_cleanup_retry_recovers_failed_and_cancelled_with_or_without_artifact()
-> Result<(), Box<dyn std::error::Error>> {
    let container = Postgres::default().with_tag("17.5-alpine").start().await?;
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&format!(
            "postgres://postgres:postgres@127.0.0.1:{}/postgres",
            container.get_host_port_ipv4(5432).await?
        ))
        .await?;
    apply_agent_migrations(&pool).await?;
    for (artifact, cancelled) in [(false, false), (true, false), (false, true), (true, true)] {
        let (command, cleanup, backend) =
            terminal_cleanup_fixture(&pool, artifact, cancelled).await?;
        let events_before: Vec<serde_json::Value> = sqlx::query_scalar("SELECT payload FROM agent.outbox_events WHERE aggregate_id=$1 ORDER BY aggregate_sequence")
            .bind(command.request.id.as_uuid()).fetch_all(&pool).await?;
        assert_eq!(events_before.len(), 1);
        wait_for_compute_deadline(&pool, cleanup.context.deadline_at).await?;
        let worker = terminal_cleanup_worker(&pool, &command, backend.clone())?;
        assert!(matches!(
            worker.run_once(now()).await?,
            BuildWorkerOutcome::Idle
        ));
        let (state, diagnostic, verified, attempt): (String, String, bool, i32) = sqlx::query_as(
            "SELECT state,diagnostic_code,cleanup_verified,attempt FROM agent.build_commands WHERE build_request_id=$1"
        ).bind(command.request.id.as_uuid()).fetch_one(&pool).await?;
        assert_eq!(state, if cancelled { "cancelled" } else { "failed" });
        assert_eq!(
            diagnostic,
            if cancelled {
                "LW_AGENT_BUILD_CANCELLED"
            } else {
                "LW_AGENT_BUILD_PROVIDER_UNAVAILABLE"
            }
        );
        assert!(verified);
        assert_eq!(attempt, 1);
        let (generation, token, deadline, stage_id): (i32, uuid::Uuid, time::OffsetDateTime, String) = sqlx::query_as(
            "SELECT highest_generation,lease_token,deadline_at,last_request_id FROM agent.build_executor_fences WHERE build_request_id=$1"
        ).bind(command.request.id.as_uuid()).fetch_one(&pool).await?;
        assert_eq!(
            (generation, token, deadline, stage_id),
            (
                1,
                cleanup.context.lease_token,
                cleanup.context.deadline_at.get(),
                cleanup.context.stage_request_id.to_string()
            )
        );
        let cleaned_artifacts: i64 = sqlx::query_scalar("SELECT count(*) FROM agent.build_executor_artifacts WHERE build_request_id=$1 AND cleaned_at IS NOT NULL")
            .bind(command.request.id.as_uuid()).fetch_one(&pool).await?;
        assert_eq!(cleaned_artifacts, i64::from(artifact));
        let events_after: Vec<serde_json::Value> = sqlx::query_scalar("SELECT payload FROM agent.outbox_events WHERE aggregate_id=$1 ORDER BY aggregate_sequence")
            .bind(command.request.id.as_uuid()).fetch_all(&pool).await?;
        assert_eq!(
            events_before, events_after,
            "no completion or replacement failure event"
        );
        worker.run_once(now()).await?;
        assert_eq!(backend.builds.load(Ordering::SeqCst), 1);
        assert_eq!(backend.cleanups.load(Ordering::SeqCst), 2);
    }
    Ok(())
}

#[tokio::test]
async fn terminal_cleanup_retry_serializes_and_rejects_changed_fences_after_deadline()
-> Result<(), Box<dyn std::error::Error>> {
    let container = Postgres::default().with_tag("17.5-alpine").start().await?;
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&format!(
            "postgres://postgres:postgres@127.0.0.1:{}/postgres",
            container.get_host_port_ipv4(5432).await?
        ))
        .await?;
    apply_agent_migrations(&pool).await?;
    let (command, cleanup, mut backend) = terminal_cleanup_fixture(&pool, false, false).await?;
    backend.block_retry = true;
    wait_for_compute_deadline(&pool, cleanup.context.deadline_at).await?;
    let executor = Arc::new(FencedBuildExecutor::new(
        PgBuildExecutorFenceStore::new(pool.clone()),
        backend.clone(),
    ));
    let execution = tokio::spawn({
        let executor = executor.clone();
        let cleanup = cleanup.clone();
        async move { executor.execute(cleanup).await }
    });
    tokio::time::timeout(Duration::from_secs(5), backend.retry_entered.notified()).await?;
    let other = FencedBuildExecutor::new(
        PgBuildExecutorFenceStore::new(pool.clone()),
        backend.clone(),
    );
    assert!(matches!(
        other.execute(cleanup.clone()).await,
        Err(BuildExecutorFenceError::InProgress)
    ));
    backend.retry_continue.notify_one();
    assert!(matches!(
        execution.await??.response,
        BuildExecutorResponse::Cleaned { .. }
    ));
    assert!(matches!(
        other.execute(cleanup.clone()).await?.response,
        BuildExecutorResponse::Cleaned { .. }
    ));
    let mut changed = cleanup.clone();
    changed.context.deadline_at =
        add_time(cleanup.context.deadline_at, time::Duration::seconds(1))?;
    assert!(matches!(
        other.execute(changed).await,
        Err(BuildExecutorFenceError::IdentityMismatch)
    ));
    let changed = build_executor_envelope(
        &command,
        1,
        cleanup.context.lease_token,
        BuildProviderStage::Cleanup,
        add_time(cleanup.context.deadline_at, time::Duration::seconds(1))?,
    );
    assert!(matches!(
        other.execute(changed).await,
        Err(BuildExecutorFenceError::IdentityMismatch)
    ));
    let changed = build_executor_envelope(
        &command,
        1,
        uuid::Uuid::new_v4(),
        BuildProviderStage::Cleanup,
        cleanup.context.deadline_at,
    );
    assert!(matches!(
        other.execute(changed).await,
        Err(BuildExecutorFenceError::StaleGeneration)
    ));
    let changed = build_executor_envelope(
        &command,
        2,
        uuid::Uuid::new_v4(),
        BuildProviderStage::Cleanup,
        cleanup.context.deadline_at,
    );
    assert!(matches!(
        other.execute(changed).await,
        Err(BuildExecutorFenceError::IdentityMismatch)
    ));
    let build = build_executor_envelope(
        &command,
        1,
        cleanup.context.lease_token,
        BuildProviderStage::Build,
        cleanup.context.deadline_at,
    );
    assert!(matches!(
        other.execute(build).await,
        Err(BuildExecutorFenceError::DeadlineExceeded)
    ));
    assert_eq!(backend.builds.load(Ordering::SeqCst), 1);
    assert_eq!(backend.cleanups.load(Ordering::SeqCst), 2);
    Ok(())
}

#[tokio::test]
async fn terminal_cleanup_retry_missing_invalid_or_nonretryable_receipts_stay_unverified()
-> Result<(), Box<dyn std::error::Error>> {
    let container = Postgres::default().with_tag("17.5-alpine").start().await?;
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&format!(
            "postgres://postgres:postgres@127.0.0.1:{}/postgres",
            container.get_host_port_ipv4(5432).await?
        ))
        .await?;
    apply_agent_migrations(&pool).await?;
    let (command, cleanup, backend) = terminal_cleanup_fixture(&pool, false, false).await?;
    let identity = Sha256Digest::of_bytes(command.request.id.as_uuid().as_bytes());
    let worker = terminal_cleanup_worker(&pool, &command, backend.clone())?;
    let invalid_receipts = [
        None,
        Some(
            serde_json::json!({"status":"failed","failure":{"code":"unavailable","retryable":false}}),
        ),
        Some(
            serde_json::json!({"status":"failed","failure":{"code":"execution_unknown","retryable":true}}),
        ),
        Some(
            serde_json::json!({"status":"failed","failure":{"code":"unavailable","retryable":true},"unexpected":true}),
        ),
        Some(
            serde_json::json!({"status":"cleaned","buildRequestId":BuildRequestId::new(),"buildIdentity":identity.to_string()}),
        ),
    ];
    for (index, receipt) in invalid_receipts.into_iter().enumerate() {
        sqlx::query(
            "UPDATE agent.build_executor_fences SET last_response=$2 WHERE build_request_id=$1",
        )
        .bind(command.request.id.as_uuid())
        .bind(receipt)
        .execute(&pool)
        .await?;
        sqlx::query("UPDATE agent.build_commands SET next_attempt_at=clock_timestamp() WHERE build_request_id=$1")
            .bind(command.request.id.as_uuid()).execute(&pool).await?;
        let outcome = worker.run_once(now()).await;
        if index >= 3 {
            assert!(
                outcome.is_err(),
                "malformed or wrong identity receipt must fail closed"
            );
        } else {
            assert!(matches!(outcome?, BuildWorkerOutcome::Idle));
        }
        let verified: bool = sqlx::query_scalar(
            "SELECT cleanup_verified FROM agent.build_commands WHERE build_request_id=$1",
        )
        .bind(command.request.id.as_uuid())
        .fetch_one(&pool)
        .await?;
        assert!(!verified);
        assert_eq!(backend.cleanups.load(Ordering::SeqCst), 1);
        assert_eq!(backend.builds.load(Ordering::SeqCst), 1);
    }
    // A plausible Built receipt cannot establish an artifact's digest without its row.
    let built = serde_json::to_value(BuildExecutorResponse::Built {
        candidate: BuiltCandidate {
            build_request_id: command.request.id,
            build_identity: BuildIdentity(identity),
            repository: command.request.output_repository.clone(),
            digest: digest(),
        },
    })?;
    sqlx::query("UPDATE agent.build_executor_fences SET last_stage='build',last_stage_rank=2,tombstone_generation=NULL,last_response=$2 WHERE build_request_id=$1")
        .bind(command.request.id.as_uuid()).bind(built).execute(&pool).await?;
    assert!(worker.run_once(now()).await.is_err());
    assert_eq!(backend.cleanups.load(Ordering::SeqCst), 1);
    // Existing metadata must be independently marked cleaned; a receipt alone is insufficient.
    let (other, _, other_backend) = terminal_cleanup_fixture(&pool, true, false).await?;
    let cleaned = serde_json::to_value(BuildExecutorResponse::Cleaned {
        build_request_id: other.request.id,
        build_identity: BuildIdentity(Sha256Digest::of_bytes(
            other.request.id.as_uuid().as_bytes(),
        )),
    })?;
    sqlx::query(
        "UPDATE agent.build_executor_fences SET last_response=$2 WHERE build_request_id=$1",
    )
    .bind(other.request.id.as_uuid())
    .bind(cleaned)
    .execute(&pool)
    .await?;
    // Exclude the intentionally malformed first request from this fixture's next reservation.
    sqlx::query("UPDATE agent.build_commands SET next_attempt_at=clock_timestamp()+interval '1 hour' WHERE build_request_id=$1")
        .bind(command.request.id.as_uuid()).execute(&pool).await?;
    assert!(
        terminal_cleanup_worker(&pool, &other, other_backend)?
            .run_once(now())
            .await
            .is_err()
    );
    let verified: bool = sqlx::query_scalar(
        "SELECT cleanup_verified FROM agent.build_commands WHERE build_request_id=$1",
    )
    .bind(other.request.id.as_uuid())
    .fetch_one(&pool)
    .await?;
    assert!(!verified);
    Ok(())
}

impl axum::serve::Listener for RegistryTlsListener {
    type Io = tokio_rustls::server::TlsStream<tokio::net::TcpStream>;
    type Addr = std::net::SocketAddr;
    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            let (stream, address) = self.listener.accept().await.expect("test TLS listener");
            if let Ok(stream) = self.acceptor.accept(stream).await {
                return (stream, address);
            }
        }
    }
    fn local_addr(&self) -> std::io::Result<Self::Addr> {
        self.listener.local_addr()
    }
}

#[tokio::test]
async fn stopped_export_without_artifact_row_must_verify_its_real_owned_tag()
-> Result<(), Box<dyn std::error::Error>> {
    use agent_service::build_executor::{ProductionBuildExecutor, ProductionBuildExecutorConfig};
    let container = Postgres::default().with_tag("17.5-alpine").start().await?;
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&format!(
            "postgres://postgres:postgres@127.0.0.1:{}/postgres",
            container.get_host_port_ipv4(5432).await?
        ))
        .await?;
    apply_agent_migrations(&pool).await?;
    let tls = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()])?;
    let key = rustls::pki_types::PrivateKeyDer::Pkcs8(tls.signing_key.serialize_der().into());
    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![tls.cert.der().clone()], key)?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let host = format!("localhost:{port}");
    let mut command = build_command()?;
    command.request.output_repository = format!("{host}/labweaver-system/image");
    let tag = format!(
        "candidate-{}",
        &Sha256Digest::of_bytes(command.request.id.as_uuid().as_bytes()).to_string()[..24]
    );
    let observed_deletes = Arc::new(AtomicUsize::new(0));
    let delete_count = observed_deletes.clone();
    let expected_tag = Arc::new(std::sync::Mutex::new(tag.clone()));
    let requested_tag = expected_tag.clone();
    let manifest_mode = Arc::new(AtomicUsize::new(0));
    let mode = manifest_mode.clone();
    let manifest_calls = Arc::new(AtomicUsize::new(0));
    let calls = manifest_calls.clone();
    let router = axum::Router::new()
        .route("/service/token", axum::routing::get(|axum::extract::Query(query): axum::extract::Query<std::collections::HashMap<String, String>>| async move {
            assert_eq!(query.get("scope").map(String::as_str), Some("repository:labweaver-system/image:pull"));
            axum::Json(serde_json::json!({"token":"test-token"}))
        }))
        .route("/v2/labweaver-system/image/manifests/{tag}", axum::routing::get(move |axum::extract::Path(tag): axum::extract::Path<String>, headers: axum::http::HeaderMap| {
            assert_eq!(tag, *requested_tag.lock().expect("expected owned candidate"));
            assert_eq!(headers.get(axum::http::header::AUTHORIZATION).expect("pull token").to_str().expect("bearer text"), "Bearer test-token");
            let accepted: Vec<_> = headers.get(axum::http::header::ACCEPT).expect("manifest Accept").to_str().expect("Accept text").split(',').map(str::trim).collect();
            assert_eq!(accepted.len(), 4);
            for media_type in ["application/vnd.oci.image.manifest.v1+json", "application/vnd.docker.distribution.manifest.v2+json", "application/vnd.oci.image.index.v1+json", "application/vnd.docker.distribution.manifest.list.v2+json"] { assert!(accepted.contains(&media_type)); }
            let mode = mode.load(Ordering::SeqCst);
            calls.fetch_add(1, Ordering::SeqCst);
            async move {
                use axum::response::IntoResponse as _;
                if mode == 0 { return ([("content-type", "application/vnd.oci.image.manifest.v1+json")], "{}").into_response(); }
                let errors = match mode {
                    1 => serde_json::json!([{"code":"DENIED"}]),
                    2 => serde_json::json!([{"code":"OTHER"}]),
                    4 => serde_json::json!([{"code":"NOT_FOUND"}]),
                    5 => serde_json::json!([]),
                    _ => serde_json::json!([{"code":"NOT_FOUND"},{"code":"DENIED"}]),
                };
                (axum::http::StatusCode::NOT_FOUND, axum::Json(serde_json::json!({"errors": errors}))).into_response()
            }
        }))
        .route("/api/v2.0/projects/labweaver-system/repositories/image/artifacts/{reference}/tags/{tag}", axum::routing::delete(move |axum::extract::Path((reference, deleted_tag)): axum::extract::Path<(String,String)>| {
            let tag = tag.clone(); let count = delete_count.clone(); async move {
                assert_eq!(reference, tag); assert_eq!(deleted_tag, tag);
                count.fetch_add(1, Ordering::SeqCst);
                axum::http::StatusCode::INTERNAL_SERVER_ERROR
            }
        }));
    let shutdown = tokio_util::sync::CancellationToken::new();
    let serve_shutdown = shutdown.clone();
    let server = tokio::spawn(async move {
        axum::serve(
            RegistryTlsListener {
                listener,
                acceptor: tokio_rustls::TlsAcceptor::from(Arc::new(config)),
            },
            router,
        )
        .with_graceful_shutdown(serve_shutdown.cancelled_owned())
        .await
    });
    let directory = tempfile::tempdir()?;
    let ca = directory.path().join("ca.pem");
    std::fs::write(&ca, tls.cert.pem())?;
    let username = directory.path().join("username");
    std::fs::write(&username, "test-user")?;
    let password = directory.path().join("password");
    std::fs::write(&password, "test-password")?;
    let objects = Arc::new(
        artifact_store::S3ImmutableObjectStore::new(
            artifact_store::S3StoreConfig {
                binding: "test-objects".to_owned(),
                endpoint: "https://localhost:1/".parse()?,
                bucket: "test".to_owned(),
                region: "test".to_owned(),
                object_prefix: "test".to_owned(),
                upload_ttl_seconds: 60,
                max_object_bytes: 1024,
                force_path_style: true,
                ca_bundle_file: None,
            },
            artifact_store::S3Credential {
                access_key_id: "test-access".to_owned(),
                secret_access_key: "test-secret".to_owned(),
                session_token: None,
            },
        )
        .await?,
    );
    let backend = ProductionBuildExecutor::new(
        ProductionBuildExecutorConfig {
            buildctl_path: directory.path().join("buildctl"),
            buildkit_address: "tcp://localhost:1".to_owned(),
            buildkit_ca_file: ca.clone(),
            buildkit_client_certificate_file: ca.clone(),
            buildkit_client_private_key_file: ca.clone(),
            docker_config_directory: directory.path().join("docker"),
            work_directory: directory.path().join("work"),
            max_unpacked_context_bytes: 1024,
            harbor_api: format!("https://{host}/").parse()?,
            harbor_registry: host,
            harbor_ca_file: ca,
            harbor_username_file: username,
            harbor_password_file: password,
            project_storage_quota_bytes: 1024,
            robot_subject: "test-robot".to_owned(),
            service_image: String::new(),
        },
        pool.clone(),
        objects,
    )
    .expect("validated production executor fixture");
    let prelaunch = build_executor_envelope(
        &command,
        1,
        uuid::Uuid::new_v4(),
        BuildProviderStage::Build,
        add_time(database_now(&pool).await?, time::Duration::seconds(5))?,
    );
    let cancellation = tokio_util::sync::CancellationToken::new();
    cancellation.cancel();
    assert!(
        matches!(backend.execute(&prelaunch.context, &prelaunch.request, &cancellation).await,
        BuildExecutorResponse::Failed { failure } if failure.code == BuildProviderFailureCode::Cancelled),
        "cancelled pre-build read never reaches unavailable object storage or solve"
    );
    PgBuildStore::new(pool.clone())
        .accept_command("agent-build-command-v1", &command_event(command.clone())?)
        .await?;
    let deadline = add_time(database_now(&pool).await?, time::Duration::seconds(5))?;
    let lease = uuid::Uuid::new_v4();
    let first = FencedBuildExecutor::new(
        PgBuildExecutorFenceStore::new(pool.clone()),
        CountingBuildExecutor {
            calls: Arc::new(AtomicUsize::new(0)),
        },
    );
    first
        .execute(build_executor_envelope(
            &command,
            1,
            lease,
            BuildProviderStage::Build,
            deadline,
        ))
        .await?;
    let production =
        FencedBuildExecutor::new(PgBuildExecutorFenceStore::new(pool.clone()), backend);
    let response = production
        .execute(build_executor_envelope(
            &command,
            1,
            lease,
            BuildProviderStage::Cleanup,
            deadline,
        ))
        .await?;
    assert!(
        matches!(response.response, BuildExecutorResponse::Failed { failure } if failure.code == BuildProviderFailureCode::Unavailable)
    );
    assert_eq!(
        observed_deletes.load(Ordering::SeqCst),
        1,
        "missing artifact row did not conceal the actual tag"
    );
    let artifacts: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM agent.build_executor_artifacts WHERE build_request_id=$1",
    )
    .bind(command.request.id.as_uuid())
    .fetch_one(&pool)
    .await?;
    assert_eq!(artifacts, 0);
    for mode in [1, 2, 3, 4, 5, 6] {
        manifest_mode.store(mode, Ordering::SeqCst);
        let before = manifest_calls.load(Ordering::SeqCst);
        let mut next = build_command()?;
        next.request.output_repository = command.request.output_repository.clone();
        *expected_tag.lock().expect("expected owned candidate") = format!(
            "candidate-{}",
            &Sha256Digest::of_bytes(next.request.id.as_uuid().as_bytes()).to_string()[..24]
        );
        let stage = if mode == 3 {
            next.request.source = contracts::supply_chain::BuildSource::ExportedOci {
                image: contracts::supply_chain::ExportedOciImage {
                    layout: artifact_ref("application/vnd.oci.image.layout.v1.tar+gzip"),
                    layout_object_key: "exports/image.tar.gz".to_owned(),
                },
            };
            BuildProviderStage::Import
        } else {
            BuildProviderStage::Build
        };
        PgBuildStore::new(pool.clone())
            .accept_command("agent-build-command-v1", &command_event(next.clone())?)
            .await?;
        let lease = uuid::Uuid::new_v4();
        let deadline = add_time(database_now(&pool).await?, time::Duration::seconds(5))?;
        first
            .execute(build_executor_envelope(&next, 1, lease, stage, deadline))
            .await?;
        if mode == 3 {
            let identity = Sha256Digest::of_bytes(next.request.id.as_uuid().as_bytes());
            sqlx::query("INSERT INTO agent.build_executor_artifacts(build_request_id,build_identity,repository,project_name,repository_name,candidate_tag,digest) VALUES($1,$2,$3,'labweaver-system','image',$4,$5)")
                .bind(next.request.id.as_uuid()).bind(identity.to_string()).bind(&next.request.output_repository)
                .bind(format!("candidate-{}", &identity.to_string()[..24])).bind(digest()).execute(&pool).await?;
        }
        let response = production
            .execute(build_executor_envelope(
                &next,
                1,
                lease,
                BuildProviderStage::Cleanup,
                deadline,
            ))
            .await?;
        if mode == 3 {
            assert!(matches!(
                response.response,
                BuildExecutorResponse::Cleaned { .. }
            ));
            assert_eq!(
                manifest_calls.load(Ordering::SeqCst),
                before,
                "digest-only import never queries or deletes a mutable tag"
            );
            let cleaned: bool = sqlx::query_scalar("SELECT cleaned_at IS NOT NULL FROM agent.build_executor_artifacts WHERE build_request_id=$1").bind(next.request.id.as_uuid()).fetch_one(&pool).await?;
            assert!(cleaned);
        } else if mode == 4 {
            assert!(matches!(
                response.response,
                BuildExecutorResponse::Cleaned { .. }
            ));
            let (artifacts, receipt): (i64, serde_json::Value) = sqlx::query_as("SELECT (SELECT count(*) FROM agent.build_executor_artifacts WHERE build_request_id=$1),last_response FROM agent.build_executor_fences WHERE build_request_id=$1")
                .bind(next.request.id.as_uuid()).fetch_one(&pool).await?;
            assert_eq!(
                artifacts, 0,
                "absence verification never fabricates an artifact"
            );
            assert_eq!(
                receipt,
                serde_json::to_value(&response.response)?,
                "same fenced cleanup persists its actual Cleaned response"
            );
        } else {
            assert!(
                matches!(response.response, BuildExecutorResponse::Failed { failure } if failure.code == BuildProviderFailureCode::Unavailable),
                "unproven manifest response cannot establish absence"
            );
        }
        assert_eq!(observed_deletes.load(Ordering::SeqCst), 1);
    }
    shutdown.cancel();
    server.await??;
    Ok(())
}

#[tokio::test]
async fn cancellation_and_completion_use_one_terminal_fence()
-> Result<(), Box<dyn std::error::Error>> {
    use agent_service::build_pipeline::{BuildCancellation, BuildExecutionFence};
    let container = Postgres::default().with_tag("17.5-alpine").start().await?;
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&format!(
            "postgres://postgres:postgres@127.0.0.1:{}/postgres",
            container.get_host_port_ipv4(5432).await?
        ))
        .await?;
    apply_agent_migrations(&pool).await?;
    let store = PgBuildStore::new(pool.clone());
    // External registry completion is represented by the existing executor fixture. Its
    // actual fenced cleanup persists the receipt; a pipeline return alone is insufficient.
    for (cancel_first, receipt) in [(true, true), (true, false), (false, true)] {
        let command = build_command()?;
        store
            .accept_command("agent-build-command-v1", &command_event(command.clone())?)
            .await?;
        let lease = store
            .claim_due("completion-race", Duration::from_secs(30))
            .await?
            .ok_or("no lease")?;
        let token: uuid::Uuid = sqlx::query_scalar(
            "SELECT lease_token FROM agent.build_commands WHERE build_request_id=$1",
        )
        .bind(command.request.id.as_uuid())
        .fetch_one(&pool)
        .await?;
        let started = database_now(&pool).await?;
        let deadline = add_time(
            started,
            time::Duration::milliseconds(command.request.max_duration_milliseconds as i64),
        )?;
        let output = BuildPipeline::new(
            SlowProvider {
                cleanup_called: Arc::new(AtomicBool::new(false)),
                build_delay: Duration::ZERO,
                fail_build: false,
            },
            policy()?,
        )?
        .execute(
            &command,
            started,
            BuildExecutionFence::new(lease.attempt, token, deadline)?,
            &BuildCancellation::new(),
        )
        .await?;
        if receipt {
            let identity = output.build_identity.0.to_string();
            sqlx::query("INSERT INTO agent.build_executor_artifacts(build_request_id,build_identity,repository,project_name,repository_name,candidate_tag,digest) VALUES($1,$2,$3,'labweaver-system','race',$4,$5)")
                .bind(command.request.id.as_uuid()).bind(&identity).bind(&command.request.output_repository)
                .bind(format!("candidate-{}", &identity[..24])).bind(digest()).execute(&pool).await?;
            let executor = FencedBuildExecutor::new(
                PgBuildExecutorFenceStore::new(pool.clone()),
                RecoveryBuildExecutor {
                    pool: pool.clone(),
                    cleanups: Arc::new(AtomicUsize::new(0)),
                },
            );
            executor
                .execute(build_executor_envelope(
                    &command,
                    lease.attempt,
                    token,
                    BuildProviderStage::Build,
                    deadline,
                ))
                .await?;
            executor
                .execute(build_executor_envelope(
                    &command,
                    lease.attempt,
                    token,
                    BuildProviderStage::Cleanup,
                    deadline,
                ))
                .await?;
        }
        let query = InternalAgentBuildStatusQuery {
            project_id: command.request.project_id,
            course_id: command.request.course_id,
        };
        let running = store.load_status(command.request.id, &query).await?;
        let cancel = InternalAgentBuildCancellationRequest {
            project_id: query.project_id,
            course_id: query.course_id,
            build_request_id: command.request.id,
            expected_state: running.state,
            expected_revision: running.revision,
            actor_id: ActorId::new(),
            requested_at: database_now(&pool).await?,
        };
        let key = IdempotencyKey::parse(&format!("race:{}", command.request.id))?;
        if cancel_first {
            store.request_cancellation(&cancel, &key).await?;
        }
        assert_eq!(
            store
                .complete(&lease, &output, started, "completion-race")
                .await?,
            !cancel_first
        );
        if !cancel_first {
            assert!(matches!(
                store.request_cancellation(&cancel, &key).await,
                Err(agent_service::build_store::BuildStoreError::StateConflict)
            ));
        }
        let terminal = store.load_status(command.request.id, &query).await?;
        assert_eq!(
            terminal.state,
            if cancel_first {
                InternalAgentBuildState::Cancelled
            } else {
                InternalAgentBuildState::Succeeded
            }
        );
        assert_eq!(
            terminal.cleanup_verified,
            if cancel_first { Some(receipt) } else { None }
        );
        let (attempt, released): (i32,bool) = sqlx::query_as("SELECT attempt,lease_token IS NULL AND worker_id IS NULL AND lease_expires_at IS NULL FROM agent.build_commands WHERE build_request_id=$1")
            .bind(command.request.id.as_uuid()).fetch_one(&pool).await?;
        assert_eq!(attempt, lease.attempt as i32);
        assert!(released);
        if receipt {
            let fence: (i32,uuid::Uuid,time::OffsetDateTime) = sqlx::query_as("SELECT highest_generation,lease_token,deadline_at FROM agent.build_executor_fences WHERE build_request_id=$1")
                .bind(command.request.id.as_uuid()).fetch_one(&pool).await?;
            assert_eq!(fence, (attempt, token, deadline.get()));
        }
        let artifacts: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM agent.image_artifacts WHERE build_request_id=$1",
        )
        .bind(command.request.id.as_uuid())
        .fetch_one(&pool)
        .await?;
        assert_eq!(artifacts, i64::from(!cancel_first));
        let completed: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM agent.outbox_events WHERE aggregate_id=$1 AND subject=$2",
        )
        .bind(command.request.id.as_uuid())
        .bind(subjects::AGENT_BUILD_COMPLETED)
        .fetch_one(&pool)
        .await?;
        assert_eq!(completed, i64::from(!cancel_first));
        let failed: Vec<serde_json::Value> = sqlx::query_scalar(
            "SELECT payload FROM agent.outbox_events WHERE aggregate_id=$1 AND subject=$2",
        )
        .bind(command.request.id.as_uuid())
        .bind(subjects::AGENT_BUILD_FAILED)
        .fetch_all(&pool)
        .await?;
        assert_eq!(failed.len(), usize::from(cancel_first));
        if cancel_first {
            assert_eq!(
                failed[0]["data"]["diagnosticCode"],
                "LW_AGENT_BUILD_CANCELLED"
            );
        }
        assert!(
            store
                .claim_due("no-replay", Duration::from_secs(30))
                .await?
                .is_none()
        );
    }
    Ok(())
}
