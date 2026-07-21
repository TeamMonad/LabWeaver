//! Real `PostgreSQL` and `JetStream` projection replay, outage and gap coverage.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use artifact_store::{ImmutableObjectStore, ObjectStoreError, PresignedUpload, VerifiedObject};
use async_trait::async_trait;
use contracts::authoring::{
    AgentAttempt, AgentAttemptState, AgentRun, AgentRunState, AgentTrack, AgentTrackKind, LlmUsage,
    RuntimeKind,
};
use contracts::events::{AgentRunEvent, CloudEvent, DATA_SCHEMA_BASE, SPEC_VERSION, subjects};
use contracts::http::InternalAgentRunOutcome;
use contracts::supply_chain::BuildNetworkPolicy;
use contracts::{
    AgentRunId, CourseId, EventId, PolicyId, ProblemPackageId, Revision, Sequence, Sha256Digest,
    UtcTimestamp,
};
use control_service::clients::DownstreamError;
use control_service::messaging::{AgentAuthority, AgentRunConsumer};
use control_service::{ContainerBuildPolicy, ControlConfig, ControlService};
use sqlx::postgres::PgPoolOptions;
use testcontainers::core::{IntoContainerPort, WaitFor};
use testcontainers::{GenericImage, ImageExt, runners::AsyncRunner};
use testcontainers_modules::postgres::Postgres;

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one durable consumer identity proves replay, restart, outage and gap ordering"
)]
async fn control_projection_is_transactional_across_duplicate_restart_outage_and_gap()
-> Result<(), Box<dyn std::error::Error>> {
    let postgres = Postgres::default().with_tag("17.5-alpine").start().await?;
    let pool = PgPoolOptions::new()
        .max_connections(8)
        .connect(&format!(
            "postgres://postgres:postgres@127.0.0.1:{}/postgres",
            postgres.get_host_port_ipv4(5432).await?
        ))
        .await?;
    sqlx::raw_sql(&format!(
        "CREATE SCHEMA control; SET search_path TO control;\n{}",
        include_str!("../../../migrations/control/0001_sprint2_baseline.sql")
    ))
    .execute(&pool)
    .await?;
    let service = ControlService::new(pool.clone(), Arc::new(UnusedObjects), config()?)?;

    let nats = GenericImage::new("nats", "2.11.8-alpine")
        .with_exposed_port(4222.tcp())
        .with_wait_for(WaitFor::message_on_stderr("Server is ready"))
        .with_cmd(["-js"])
        .start()
        .await?;
    let client = async_nats::connect(format!(
        "nats://127.0.0.1:{}",
        nats.get_host_port_ipv4(4222).await?
    ))
    .await?;
    let context = async_nats::jetstream::new(client.clone());
    context
        .create_stream(async_nats::jetstream::stream::Config {
            name: "AGENT_RUNS".to_owned(),
            subjects: vec!["labweaver.agent.>".to_owned()],
            ..Default::default()
        })
        .await?
        .create_consumer(async_nats::jetstream::consumer::pull::Config {
            durable_name: Some("control-agent-projection-v1".to_owned()),
            filter_subject: "labweaver.agent.run.>".to_owned(),
            ack_policy: async_nats::jetstream::consumer::AckPolicy::Explicit,
            ..Default::default()
        })
        .await?;

    let requested = requested_run()?;
    let authority = FakeAuthority {
        unavailable: AtomicBool::new(false),
        requested: requested.clone(),
        outcome: failed_outcome(&requested)?,
    };
    let event1 = event(
        &requested,
        EventId::new(),
        1,
        subjects::AGENT_RUN_REQUESTED,
        "requested",
        None,
    )?;
    publish(&context, &event1).await?;
    let mut consumer = AgentRunConsumer::bind(
        client.clone(),
        "AGENT_RUNS",
        "control-agent-projection-v1",
        "labweaver.agent.quarantine.control_agent_run.v1",
    )
    .await?;
    consumer.process_next(&service, &authority).await?;
    assert_projection(&pool, requested.id, 1, 1, 1).await?;

    publish(&context, &event1).await?;
    consumer.process_next(&service, &authority).await?;
    assert_projection(&pool, requested.id, 1, 1, 1).await?;
    drop(consumer);

    let failed = &authority.outcome.run;
    let event2 = event(
        failed,
        EventId::new(),
        2,
        subjects::AGENT_RUN_FAILED,
        "failed",
        Some("LW_AGENT_RUNTIME_FAILED"),
    )?;
    publish(&context, &event2).await?;
    authority.unavailable.store(true, Ordering::SeqCst);
    let mut consumer = AgentRunConsumer::bind(
        client,
        "AGENT_RUNS",
        "control-agent-projection-v1",
        "labweaver.agent.quarantine.control_agent_run.v1",
    )
    .await?;
    consumer.process_next(&service, &authority).await?;
    assert_projection(&pool, requested.id, 1, 1, 1).await?;
    authority.unavailable.store(false, Ordering::SeqCst);
    tokio::time::timeout(
        Duration::from_secs(5),
        consumer.process_next(&service, &authority),
    )
    .await??;
    assert_projection(&pool, requested.id, 2, 2, 2).await?;

    let gap = event(
        failed,
        EventId::new(),
        4,
        subjects::AGENT_RUN_FAILED,
        "failed",
        Some("LW_AGENT_RUNTIME_FAILED"),
    )?;
    publish(&context, &gap).await?;
    consumer.process_next(&service, &authority).await?;
    assert_projection(&pool, requested.id, 2, 2, 2).await?;
    Ok(())
}

async fn publish(
    context: &async_nats::jetstream::Context,
    event: &CloudEvent<AgentRunEvent>,
) -> Result<(), Box<dyn std::error::Error>> {
    context
        .publish(event.subject.clone(), serde_json::to_vec(event)?.into())
        .await?
        .await?;
    Ok(())
}

async fn assert_projection(
    pool: &sqlx::PgPool,
    run_id: AgentRunId,
    revision: i64,
    watermark: i64,
    sse_count: i64,
) -> Result<(), sqlx::Error> {
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT revision FROM control.agent_run_projections WHERE run_id=$1",
        )
        .bind(run_id.as_uuid())
        .fetch_one(pool)
        .await?,
        revision
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT last_sequence FROM control.inbox_watermarks \
             WHERE consumer='control_agent_run_projection_v1' AND aggregate_id=$1",
        )
        .bind(run_id.as_uuid())
        .fetch_one(pool)
        .await?,
        watermark
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM control.sse_events")
            .fetch_one(pool)
            .await?,
        sse_count
    );
    Ok(())
}

fn requested_run() -> Result<AgentRun, Box<dyn std::error::Error>> {
    let run = AgentRun {
        id: AgentRunId::new(),
        course_id: CourseId::new(),
        package_id: ProblemPackageId::new(),
        policy_id: PolicyId::new(),
        policy_revision: Revision::new(1)?,
        requested_runtime: RuntimeKind::Container,
        state: AgentRunState::Requested,
        revision: Revision::new(1)?,
        tracks: vec![
            AgentTrack {
                kind: AgentTrackKind::Environment,
                attempts: Vec::new(),
                candidate_id: None,
            },
            AgentTrack {
                kind: AgentTrackKind::Evaluation,
                attempts: Vec::new(),
                candidate_id: None,
            },
        ],
    };
    run.validate()?;
    Ok(run)
}

fn failed_outcome(
    requested: &AgentRun,
) -> Result<InternalAgentRunOutcome, Box<dyn std::error::Error>> {
    let mut run = requested.clone();
    run.state = AgentRunState::Failed;
    run.revision = Revision::new(2)?;
    for track in &mut run.tracks {
        track.attempts.push(AgentAttempt {
            number: 1,
            state: AgentAttemptState::Failed,
            input_sha256: Sha256Digest::of_bytes(b"input"),
            output_sha256: None,
            checkpoint: None,
            usage: LlmUsage {
                input_tokens: 0,
                output_tokens: 0,
                requests: 0,
                cost_microusd: 0,
            },
            usage_observed: false,
            diagnostic_code: Some("LW_AGENT_RUNTIME_FAILED".to_owned()),
        });
    }
    run.validate()?;
    let outcome_sha256 = Sha256Digest::of_canonical(&serde_json::json!({
        "run": run,
        "environmentCandidate": null,
        "evaluationCandidate": null,
    }))?;
    let outcome = InternalAgentRunOutcome {
        run,
        environment_candidate: None,
        evaluation_candidate: None,
        outcome_sha256,
    };
    outcome.validate()?;
    Ok(outcome)
}

fn event(
    run: &AgentRun,
    event_id: EventId,
    sequence: u64,
    subject: &str,
    state: &str,
    diagnostic: Option<&str>,
) -> Result<CloudEvent<AgentRunEvent>, Box<dyn std::error::Error>> {
    let schema = if subject == subjects::AGENT_RUN_REQUESTED {
        "agent-run-requested"
    } else {
        "agent-run-failed"
    };
    Ok(CloudEvent {
        specversion: SPEC_VERSION.to_owned(),
        id: event_id,
        source: "urn:labweaver:agent-service".to_owned(),
        event_type: subject.to_owned(),
        subject: subject.to_owned(),
        time: "2026-07-15T08:00:00.000Z".parse::<UtcTimestamp>()?,
        datacontenttype: "application/json".to_owned(),
        dataschema: format!("{DATA_SCHEMA_BASE}/{schema}.schema.json"),
        course_id: run.course_id,
        aggregate_revision: run.revision,
        aggregate_sequence: Sequence(sequence),
        trace_id: "issue-48-control-consumer".to_owned(),
        data: AgentRunEvent {
            run_id: run.id,
            attempt: u64::from(sequence > 1),
            state: state.to_owned(),
            diagnostic_code: diagnostic.map(str::to_owned),
        },
    })
}

struct FakeAuthority {
    unavailable: AtomicBool,
    requested: AgentRun,
    outcome: InternalAgentRunOutcome,
}

#[async_trait]
impl AgentAuthority for FakeAuthority {
    async fn get(&self, _: AgentRunId) -> Result<AgentRun, DownstreamError> {
        if self.unavailable.load(Ordering::SeqCst) {
            Err(DownstreamError::Unavailable)
        } else {
            Ok(self.requested.clone())
        }
    }

    async fn outcome(&self, _: AgentRunId) -> Result<InternalAgentRunOutcome, DownstreamError> {
        if self.unavailable.load(Ordering::SeqCst) {
            Err(DownstreamError::Unavailable)
        } else {
            Ok(self.outcome.clone())
        }
    }
}

struct UnusedObjects;

#[async_trait]
impl ImmutableObjectStore for UnusedObjects {
    async fn presign_upload(
        &self,
        _: &str,
        _: u64,
        _: Sha256Digest,
        _: &str,
        _: UtcTimestamp,
    ) -> Result<PresignedUpload, ObjectStoreError> {
        Err(ObjectStoreError::ObjectUnavailable)
    }

    async fn read_verified(
        &self,
        _: &str,
        _: &str,
        _: u64,
        _: Sha256Digest,
        _: &str,
    ) -> Result<VerifiedObject, ObjectStoreError> {
        Err(ObjectStoreError::ObjectUnavailable)
    }

    async fn freeze_current(
        &self,
        _: &str,
        _: u64,
        _: Sha256Digest,
        _: &str,
    ) -> Result<VerifiedObject, ObjectStoreError> {
        Err(ObjectStoreError::ObjectUnavailable)
    }

    async fn delete_orphan(&self, _: &str, _: &str) -> Result<(), ObjectStoreError> {
        Err(ObjectStoreError::DeleteFailed)
    }
}

fn config() -> Result<ControlConfig, Box<dyn std::error::Error>> {
    Ok(ControlConfig {
        package_object_prefix: "problem-packages".to_owned(),
        upload_ttl_seconds: 900,
        completion_lease_seconds: 300,
        max_package_files: 100,
        max_package_bytes: 1_048_576,
        retention_policy_id: PolicyId::new(),
        retention_seconds: 86_400,
        sse_retention_seconds: 3_600,
        trust_revision: Revision::new(1)?,
        image_policy_id: PolicyId::new(),
        image_policy_revision: Revision::new(1)?,
        environment_schema_sha256: Sha256Digest::of_bytes(b"environment"),
        evaluation_schema_sha256: Sha256Digest::of_bytes(b"evaluation"),
        container_build: ContainerBuildPolicy {
            builder_binding: "buildkit-primary-v1".to_owned(),
            output_repository_prefix: "harbor.internal/labweaver-system".to_owned(),
            dockerfile_path: "Dockerfile".to_owned(),
            network: BuildNetworkPolicy::DenyAll,
            max_duration_milliseconds: 600_000,
            max_cpu_millicores: 2_000,
            max_memory_bytes: 2_147_483_648,
        },
        virtual_machine_base: control_service::VirtualMachineBasePolicy {
            provider_binding: "kubevirt-primary-v1".to_owned(),
            storage_class_binding: "vm-rwo-primary-v1".to_owned(),
            artifact_id: contracts::ImageArtifactId::new(),
            base_disk: contracts::supply_chain::VirtualMachineBaseDisk {
                binding: "ubuntu-24.04-v1".to_owned(),
                source_registry_digest: concat!(
                    "docker://quay.io/containerdisks/ubuntu@",
                    "sha256:d28194a16351320fa9a093e18233033508a745566eb8ba3b309c32924bf155a5"
                )
                .to_owned(),
                disk_sha256: Sha256Digest::of_bytes(b"vm-disk"),
                capacity_bytes: 10_737_418_240,
            },
            format: contracts::supply_chain::VirtualMachineDiskFormat::Qcow2,
        },
    })
}
