//! Real `PostgreSQL` migration and concurrency evidence for the Issue #48 control plane.

use std::collections::BTreeSet;
use std::sync::Arc;

use artifact_store::{ImmutableObjectStore, ObjectStoreError, PresignedUpload, VerifiedObject};
use async_trait::async_trait;
use contracts::authoring::{
    AgentAttempt, AgentAttemptState, AgentRun, AgentRunPurpose, AgentRunState, AgentTrack,
    AgentTrackKind, AuthoringApproval, AuthoringApprovalPublicationStatus,
    AuthoringPublicationState, CandidateDecision, EnvironmentCandidate, EnvironmentClass,
    EnvironmentSpec, EvaluationCandidate, LlmUsage, PackageFile, ProblemPackage,
    ProjectLlmEgressPolicy, RuntimeKind, WorkConfigurationPlan,
};
use contracts::evaluation::{
    EVALUATION_RELEASE_SCHEMA_VERSION, EvaluationRelease, EvaluationReleaseState,
    EvaluationRuntimeIdentity, EvaluationSpec,
};
use contracts::events::{AgentBuildRequested, CloudEvent};
use contracts::http::{
    AuthoringPublicationAdmissionQuery, CandidateDecisionRequest, CompleteAuthoringApprovalRequest,
    CreateEnvironmentTemplateReleaseRequest, CreateProblemPackageUploadRequest,
    GeneratedArtifactKind, GeneratedArtifactRecord, IdempotencyKey, ProblemPackageUploadFile,
    WorkConfigurationAdmissionQuery,
};
use contracts::supply_chain::BuildNetworkPolicy;
use contracts::supply_chain::{EnvironmentTemplateRelease, ImageArtifact};
use contracts::{
    ActorId, AgentRunId, ApprovalId, ArtifactId, ArtifactRef, BuildRequestId, CandidateId,
    CourseId, DiagnosticCode, EnvironmentId, EvaluationReleaseId, EventId, ImageArtifactId,
    PolicyId, ProblemPackageId, Project, ProjectId, ProjectState, ReleaseId, RetentionClass,
    RetentionDisposition, RetentionSnapshot, Revision, UtcTimestamp,
};
use control_service::{
    AuthoringPublicationClaim, ContainerBuildPolicy, ControlConfig, ControlError, ControlService,
};
use persistence_sqlx::{Domain, Sha256Digest};
use sqlx::{Row, postgres::PgPoolOptions};
use testcontainers::{ImageExt, runners::AsyncRunner};
use testcontainers_modules::postgres::Postgres;
use uuid::Uuid;

mod support;
use support::apply_domain_migrations;

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn issue_48_migrations_enforce_fencing_and_monotonic_course_sequences()
-> Result<(), Box<dyn std::error::Error>> {
    let container = Postgres::default().with_tag("17.5-alpine").start().await?;
    let url = format!(
        "postgres://postgres:postgres@127.0.0.1:{}/postgres",
        container.get_host_port_ipv4(5432).await?
    );
    let pool = PgPoolOptions::new()
        .max_connections(16)
        .connect(&url)
        .await?;
    apply_domain_migrations(&pool, Domain::Control).await?;
    apply_domain_migrations(&pool, Domain::Agent).await?;

    let upload_id = Uuid::now_v7();
    let course_id = Uuid::now_v7();
    let invalid = sqlx::query(
        "INSERT INTO control.problem_package_upload_sessions \
         (upload_id,project_id,course_id,revision,state,retention_policy_revision,expires_at) \
         VALUES ($1,$2,$3,1,'completing',1,now()+interval '1 hour')",
    )
    .bind(upload_id)
    .bind(Uuid::now_v7())
    .bind(course_id)
    .execute(&pool)
    .await;
    assert!(
        invalid.is_err(),
        "completing sessions must always carry a fencing lease"
    );

    let mut tasks = Vec::new();
    for _ in 0..20 {
        let pool = pool.clone();
        tasks.push(tokio::spawn(async move {
            let mut transaction = pool.begin().await?;
            let sequence = sqlx::query_scalar::<_, i64>(
                "INSERT INTO control.sse_course_cursors(course_id,last_sequence) VALUES ($1,1) \
                 ON CONFLICT(course_id) DO UPDATE SET last_sequence=control.sse_course_cursors.last_sequence+1 \
                 RETURNING last_sequence",
            )
            .bind(course_id)
            .fetch_one(&mut *transaction)
            .await?;
            sqlx::query(
                "INSERT INTO control.sse_events(course_id,sequence,event_type,aggregate_id,aggregate_revision,payload,payload_sha256) \
                 VALUES ($1,$2,'test.v1',$3,1,'{}'::jsonb,$4)",
            )
            .bind(course_id)
            .bind(sequence)
            .bind(Uuid::now_v7())
            .bind("44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a")
            .execute(&mut *transaction)
            .await?;
            transaction.commit().await?;
            Ok::<i64, sqlx::Error>(sequence)
        }));
    }
    let mut sequences = BTreeSet::new();
    for task in tasks {
        sequences.insert(task.await??);
    }
    assert_eq!(sequences, (1_i64..=20).collect());
    let cursor =
        sqlx::query("SELECT last_sequence FROM control.sse_course_cursors WHERE course_id=$1")
            .bind(course_id)
            .fetch_one(&pool)
            .await?
            .try_get::<i64, _>("last_sequence")?;
    assert_eq!(cursor, 20);

    let course = contracts::CourseId::new();
    let request = upload_request(course)?;
    let now = UtcTimestamp::from_utc(
        sqlx::query_scalar("SELECT date_trunc('milliseconds',clock_timestamp())")
            .fetch_one(&pool)
            .await?,
    )?;
    let service = ControlService::new(
        pool.clone(),
        Arc::new(FixtureObjects { fail_second: false }),
        control_config()?,
    )?;
    let create_key = IdempotencyKey::parse("issue-48-create-upload")?;
    let session = service
        .create_upload(course, &request, &create_key, now)
        .await?;
    let complete_key = IdempotencyKey::parse("issue-48-complete-upload")?;
    let package = service
        .complete_upload(course, session.id, session.revision, &complete_key, now)
        .await?;
    package.validate()?;
    let mut replays = Vec::new();
    for _ in 0..10 {
        let service = service.clone();
        let key = complete_key.clone();
        replays.push(tokio::spawn(async move {
            service
                .complete_upload(course, session.id, session.revision, &key, now)
                .await
        }));
    }
    for replay in replays {
        assert_eq!(replay.await??, package);
    }
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM control.problem_packages")
            .fetch_one(&pool)
            .await?,
        1
    );

    let recovery_course = contracts::CourseId::new();
    let recovery_session = service
        .create_upload(
            recovery_course,
            &upload_request(recovery_course)?,
            &IdempotencyKey::parse("issue-48-recovery-create")?,
            now,
        )
        .await?;
    let recovery_key = IdempotencyKey::parse("issue-48-recovery-complete")?;
    let recovery_request_hash = Sha256Digest::of_canonical(&serde_json::json!({
        "projectId": serde_json::Value::Null,
        "courseId": recovery_course,
        "uploadId": recovery_session.id,
        "expectedRevision": recovery_session.revision,
    }))?;
    sqlx::query(
        "INSERT INTO control.idempotency_ledger \
         (operation,idempotency_key,request_sha256,state) VALUES ($1,$2,$3,'in_progress')",
    )
    .bind("control_complete_problem_package_upload_v1")
    .bind(recovery_key.as_str())
    .bind(recovery_request_hash.to_string())
    .execute(&pool)
    .await?;
    sqlx::query(
        "UPDATE control.problem_package_upload_sessions \
         SET state='completing',revision=revision+1,completion_idempotency_key=$2, \
             completion_request_sha256=$3,completion_lease_token=$4, \
             completion_lease_expires_at=now()-interval '1 second' WHERE upload_id=$1",
    )
    .bind(recovery_session.id.as_uuid())
    .bind(recovery_key.as_str())
    .bind(recovery_request_hash.to_string())
    .bind(Uuid::now_v7())
    .execute(&pool)
    .await?;
    let recovered = service
        .complete_upload(
            recovery_course,
            recovery_session.id,
            recovery_session.revision,
            &recovery_key,
            now,
        )
        .await?;
    recovered.validate()?;

    let failing_service = ControlService::new(
        pool.clone(),
        Arc::new(FixtureObjects { fail_second: true }),
        control_config()?,
    )?;
    let failing_course = contracts::CourseId::new();
    let failing_session = failing_service
        .create_upload(
            failing_course,
            &upload_request(failing_course)?,
            &IdempotencyKey::parse("issue-48-failing-create")?,
            now,
        )
        .await?;
    assert!(
        failing_service
            .complete_upload(
                failing_session
                    .course_id
                    .ok_or("upload session missing course")?,
                failing_session.id,
                failing_session.revision,
                &IdempotencyKey::parse("issue-48-failing-complete")?,
                now,
            )
            .await
            .is_err()
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            "SELECT state FROM control.problem_package_upload_sessions WHERE upload_id=$1",
        )
        .bind(failing_session.id.as_uuid())
        .fetch_one(&pool)
        .await?,
        "failed"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM control.object_cleanup_ledger WHERE upload_id=$1",
        )
        .bind(failing_session.id.as_uuid())
        .fetch_one(&pool)
        .await?,
        1
    );
    Ok(())
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn candidate_decision_route_kind_is_bound_before_approval()
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
    apply_domain_migrations(&pool, Domain::Control).await?;
    let config = control_config()?;
    let evaluation_schema = config.evaluation_schema_sha256;
    let environment_schema = config.environment_schema_sha256;
    let service = ControlService::new(
        pool.clone(),
        Arc::new(FixtureObjects { fail_second: false }),
        config,
    )?;
    let project_id = ProjectId::new();
    let project_owner = ActorId::new();
    let project = project_fixture(project_id, project_owner, None)?;
    insert_project(&pool, &project).await?;
    let policy_id = PolicyId::new();
    let candidate_id = CandidateId::new();
    let candidate_sha256 = Sha256Digest::of_bytes(b"evaluation-candidate");
    sqlx::query(
        "INSERT INTO control.project_llm_policies \
         (policy_id,project_id,course_id,revision,contract_sha256,contract,activated_at) \
         VALUES ($1,$2,$3,1,$4,'{}'::jsonb,now())",
    )
    .bind(policy_id.as_uuid())
    .bind(project_id.as_uuid())
    .bind(None::<Uuid>)
    .bind(Sha256Digest::of_bytes(b"policy").to_string())
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO control.candidates \
         (candidate_id,candidate_kind,project_id,course_id,revision,state,content_sha256,contract, \
           policy_revision,schema_sha256,projected_event_id) \
         VALUES ($1,'evaluation',$2,$3,1,'validated',$4,'{}'::jsonb,1,$5,$6)",
    )
    .bind(candidate_id.as_uuid())
    .bind(project_id.as_uuid())
    .bind(None::<Uuid>)
    .bind(candidate_sha256.to_string())
    .bind(evaluation_schema.to_string())
    .bind(Uuid::now_v7())
    .execute(&pool)
    .await?;
    let request = CandidateDecisionRequest {
        candidate_revision: Revision::new(1)?,
        policy_revision: Revision::new(1)?,
        trust_revision: Revision::new(1)?,
        decision: CandidateDecision::Approved,
        reason: "reviewed evaluation candidate".to_owned(),
    };
    let result = service
        .decide_project_candidate(
            project_id,
            candidate_id,
            AgentTrackKind::Environment,
            &request,
            ActorId::new(),
            Revision::new(1)?,
            &IdempotencyKey::parse("candidate-kind-mismatch")?,
            "2026-07-16T08:00:00.000Z".parse()?,
        )
        .await;
    assert!(matches!(result, Err(ControlError::CandidateKindMismatch)));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM control.candidate_approvals")
            .fetch_one(&pool)
            .await?,
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM control.idempotency_ledger")
            .fetch_one(&pool)
            .await?,
        0
    );

    let project_id = ProjectId::new();
    let project_owner = ActorId::new();
    let course_id = CourseId::new();
    let project = project_fixture(project_id, project_owner, Some(course_id))?;
    insert_project(&pool, &project).await?;
    let policy_id = PolicyId::new();
    sqlx::query(
        "INSERT INTO control.project_llm_policies \
         (policy_id,project_id,course_id,revision,contract_sha256,contract,activated_at) \
         VALUES ($1,$2,$3,1,$4,'{}'::jsonb,now())",
    )
    .bind(policy_id.as_uuid())
    .bind(project_id.as_uuid())
    .bind(course_id.as_uuid())
    .bind(Sha256Digest::of_bytes(b"work-policy").to_string())
    .execute(&pool)
    .await?;
    let environment_candidate =
        environment_candidate(project_id, Some(course_id), environment_schema)?;
    let package_id = ProblemPackageId::new();
    let build_context = match &environment_candidate.spec.runtime {
        contracts::authoring::EnvironmentRuntimeSpec::Container { build_context, .. } => {
            build_context
        }
        contracts::authoring::EnvironmentRuntimeSpec::VirtualMachine { .. } => {
            return Err("fixture must be Container".into());
        }
    };
    let upload_id = Uuid::now_v7();
    let foreign_course_id = CourseId::new();
    sqlx::query(
        "INSERT INTO control.problem_package_upload_sessions \
         (upload_id,project_id,course_id,revision,state,retention_policy_revision,expires_at,completed_package_id) \
         VALUES ($1,$2,$3,1,'completed',1,now()+interval '1 hour',$4)",
    )
    .bind(upload_id)
    .bind(environment_candidate.project_id.as_uuid())
    .bind(foreign_course_id.as_uuid())
    .bind(package_id.as_uuid())
    .execute(&pool)
    .await?;
    let package_contract = serde_json::json!({
        "id": package_id,
        "projectId": project_id,
        "courseId": course_id,
        "revision": 1,
        "files": [{"path": "context.tar.gz", "object": build_context}],
        "retention": {
            "policyId": PolicyId::new(),
            "policyRevision": 1,
            "class": "course_material",
            "retainUntil": "2026-12-31T08:00:00.000Z",
            "disposition": "delete"
        },
        "completedAt": "2026-07-16T08:00:00.000Z"
    });
    sqlx::query(
        "INSERT INTO control.problem_packages \
         (package_id,project_id,course_id,revision,manifest_sha256,contract,completed_at) \
         VALUES ($1,$2,$3,1,$4,$5,$6)",
    )
    .bind(package_id.as_uuid())
    .bind(project_id.as_uuid())
    .bind(course_id.as_uuid())
    .bind(Sha256Digest::of_bytes(b"manifest").to_string())
    .bind(package_contract)
    .bind("2026-07-16T08:00:00.000Z".parse::<UtcTimestamp>()?.get())
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO control.problem_package_upload_files \
         (upload_id,ordinal,path,object_key,artifact_id,object_version,size_bytes,sha256,media_type,verified_at) \
         VALUES ($1,0,'context.tar.gz',$2,$3,$4,$5,$6,$7,now())",
    )
    .bind(upload_id)
    .bind(format!("courses/{course_id}/uploads/{upload_id}/context.tar.gz"))
    .bind(build_context.artifact_id.as_uuid())
    .bind(&build_context.object_version)
    .bind(i64::try_from(build_context.size_bytes)?)
    .bind(Sha256Digest::of_bytes(b"context").to_string())
    .bind(&build_context.media_type)
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO control.candidates \
         (candidate_id,candidate_kind,project_id,course_id,run_id,revision,state,content_sha256,contract, \
           policy_revision,schema_sha256,projected_event_id) \
          VALUES ($1,'environment',$2,$3,$4,1,'validated',$5,$6,1,$7,$8)",
    )
    .bind(environment_candidate.id.as_uuid())
    .bind(environment_candidate.project_id.as_uuid())
    .bind(course_id.as_uuid())
    .bind(environment_candidate.run_id.as_uuid())
    .bind(Sha256Digest::of_canonical(&environment_candidate.spec)?.to_string())
    .bind(serde_json::to_value(&environment_candidate)?)
    .bind(environment_schema.to_string())
    .bind(Uuid::now_v7())
    .execute(&pool)
    .await?;
    let run = succeeded_agent_run(
        project_id,
        Some(course_id),
        package_id,
        policy_id,
        environment_candidate.run_id,
        environment_candidate.id,
        CandidateId::new(),
        EnvironmentClass::Work,
    )?;
    let cross_course_context = service
        .project_candidates(
            EventId::new(),
            &run,
            Some(&environment_candidate),
            None,
            None,
        )
        .await;
    assert!(matches!(
        cross_course_context,
        Err(ControlError::PersistenceIdentityMismatch)
    ));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM control.candidate_approvals")
            .fetch_one(&pool)
            .await?,
        0
    );
    sqlx::query(
        "UPDATE control.problem_package_upload_sessions SET course_id=$1 WHERE upload_id=$2",
    )
    .bind(course_id.as_uuid())
    .bind(upload_id)
    .execute(&pool)
    .await?;
    service
        .project_candidates(
            EventId::new(),
            &run,
            Some(&environment_candidate),
            None,
            None,
        )
        .await?;
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM control.container_build_projections \
             WHERE project_id=$1 AND candidate_id=$2 AND candidate_revision=$3",
        )
        .bind(project_id.as_uuid())
        .bind(environment_candidate.id.as_uuid())
        .bind(i64::try_from(environment_candidate.revision.get())?)
        .fetch_one(&pool)
        .await?,
        1
    );
    let environment_decision = CandidateDecisionRequest {
        candidate_revision: environment_candidate.revision,
        policy_revision: environment_candidate.policy_revision,
        trust_revision: Revision::new(1)?,
        decision: CandidateDecision::Approved,
        reason: "reviewed container candidate".to_owned(),
    };
    let approval_result = service
        .decide_project_candidate(
            project_id,
            environment_candidate.id,
            AgentTrackKind::Environment,
            &environment_decision,
            project_owner,
            Revision::new(1)?,
            &IdempotencyKey::parse("approve-container-candidate")?,
            "2026-07-16T08:00:00.000Z".parse()?,
        )
        .await;
    let approval = approval_result?;
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM control.container_build_projections \
             WHERE project_id=$1 AND candidate_id=$2 AND candidate_revision=$3",
        )
        .bind(project_id.as_uuid())
        .bind(environment_candidate.id.as_uuid())
        .bind(i64::try_from(environment_candidate.revision.get())?)
        .fetch_one(&pool)
        .await?,
        1
    );
    let requested_view = service
        .project_environment_candidate_view(project_id, environment_candidate.id)
        .await?;
    assert_eq!(requested_view.candidate, environment_candidate);
    assert_eq!(requested_view.approvals, vec![approval.clone()]);
    let requested_build = requested_view.build.ok_or("missing requested build view")?;
    assert_eq!(
        requested_build.state,
        contracts::http::CandidateBuildState::Requested
    );
    assert!(requested_build.artifact.is_none());
    let payload: serde_json::Value = sqlx::query_scalar(
        "SELECT payload FROM control.outbox_events WHERE subject=$1 AND aggregate_id<>$2",
    )
    .bind(contracts::events::subjects::AGENT_BUILD_REQUESTED)
    .bind(approval.id.as_uuid())
    .fetch_one(&pool)
    .await?;
    let build_event: CloudEvent<AgentBuildRequested> = serde_json::from_value(payload)?;
    build_event.data.validate()?;
    assert_eq!(
        build_event.data.request.candidate_id,
        environment_candidate.id
    );
    assert_eq!(
        build_event.data.request.output_repository,
        format!(
            "harbor.internal/labweaver-system/course-{course_id}-{}",
            environment_candidate.id
        )
    );
    assert_eq!(
        build_event.data.request.context,
        match &environment_candidate.spec.runtime {
            contracts::authoring::EnvironmentRuntimeSpec::Container { build_context, .. } => {
                build_context.clone()
            }
            contracts::authoring::EnvironmentRuntimeSpec::VirtualMachine { .. } => {
                return Err("fixture must be Container".into());
            }
        }
    );

    let mut supply_chain = release_fixture(course_id, environment_candidate.project_id)?;
    supply_chain.candidate_id = environment_candidate.id;
    supply_chain.candidate_revision = environment_candidate.revision;

    supply_chain.approval = approval.clone();
    if let ImageArtifact::Container {
        build_request_id, ..
    } = &mut supply_chain.artifact
    {
        *build_request_id = build_event.data.request.id;
    }
    assert!(matches!(
        service
            .project_artifact(
                EventId::new(),
                environment_candidate.project_id,
                Some(CourseId::new()),
                &supply_chain.artifact,
            )
            .await,
        Err(ControlError::ProjectMismatch)
    ));
    service
        .project_artifact(
            EventId::new(),
            environment_candidate.project_id,
            Some(course_id),
            &supply_chain.artifact,
        )
        .await?;
    let succeeded_view = service
        .environment_candidate_view(course_id, environment_candidate.id)
        .await?;
    let succeeded_build = succeeded_view.build.ok_or("missing succeeded build view")?;
    assert_eq!(
        succeeded_build.state,
        contracts::http::CandidateBuildState::Succeeded
    );
    assert_eq!(
        succeeded_build.artifact,
        Some(supply_chain.artifact.clone())
    );
    let release = service
        .create_project_work_release(
            environment_candidate.project_id,
            &CreateEnvironmentTemplateReleaseRequest {
                project_id: environment_candidate.project_id,
                course_id: Some(course_id),
                candidate_id: environment_candidate.id,
                candidate_revision: environment_candidate.revision,
                runtime_kind: contracts::authoring::RuntimeKind::Container,
                approval_id: approval.id,
            },
            project_owner,
            false,
            &IdempotencyKey::parse("publish-container-release")?,
            "2026-07-16T08:30:00.000Z".parse()?,
            "trace-publish-container-release",
        )
        .await?;
    let withdrawal = service
        .withdraw_project_release(
            environment_candidate.project_id,
            release.id,
            release.version,
            project_owner,
            false,
            "SECURITY_REVOKED",
            &IdempotencyKey::parse("withdraw-release")?,
            "2026-07-16T09:00:00.000Z".parse()?,
            "trace-withdraw-release",
        )
        .await?;
    let view = service
        .project_release(
            environment_candidate.project_id,
            release.id,
            Some(course_id),
            project_owner,
        )
        .await?;
    assert_eq!(view.release, release);
    assert_eq!(view.withdrawal, Some(withdrawal.clone()));
    let listed = service
        .project_releases(
            environment_candidate.project_id,
            Some(course_id),
            0,
            10,
            project_owner,
        )
        .await?;
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].withdrawal, Some(withdrawal));
    for subject in [
        contracts::events::subjects::ENVIRONMENT_TEMPLATE_RELEASE_PUBLISHED,
        contracts::events::subjects::ENVIRONMENT_TEMPLATE_RELEASE_WITHDRAWN,
    ] {
        assert!(
            sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS(SELECT 1 FROM control.outbox_events \
                 WHERE aggregate_id=$1 AND subject=$2)",
            )
            .bind(release.id.as_uuid())
            .bind(subject)
            .fetch_one(&pool)
            .await?
        );
    }

    // A newer candidate projection must not surface the succeeded build that
    // belongs to the previous revision and specification hash.
    let mut current_candidate = environment_candidate.clone();
    current_candidate.revision = Revision::new(2)?;
    current_candidate.spec.name.push_str("-current");
    current_candidate.validate()?;
    let current_spec_sha256 = Sha256Digest::of_canonical(&current_candidate.spec)?;
    sqlx::query(
        "UPDATE control.candidates \
         SET revision=$2,content_sha256=$3,contract=$4 WHERE candidate_id=$1",
    )
    .bind(current_candidate.id.as_uuid())
    .bind(i64::try_from(current_candidate.revision.get())?)
    .bind(current_spec_sha256.to_string())
    .bind(serde_json::to_value(&current_candidate)?)
    .execute(&pool)
    .await?;
    let current_view = service
        .project_environment_candidate_view(project_id, current_candidate.id)
        .await?;
    assert_eq!(current_view.candidate, current_candidate);
    assert!(current_view.build.is_none());
    assert!(current_view.image_artifact.is_none());

    Ok(())
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn generated_container_context_is_bound_to_agent_artifact_metadata()
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
    apply_domain_migrations(&pool, Domain::Control).await?;
    let config = control_config()?;
    let service = ControlService::new(
        pool.clone(),
        Arc::new(FixtureObjects { fail_second: false }),
        config,
    )?;
    let project_id = ProjectId::new();
    let course_id = CourseId::new();
    let owner = ActorId::new();
    insert_project(&pool, &project_fixture(project_id, owner, Some(course_id))?).await?;
    let now: UtcTimestamp = "2026-07-16T08:00:00.000Z".parse()?;
    let environment_schema = Sha256Digest::of_bytes(b"environment");
    let package_id = ProblemPackageId::new();
    let package = ProblemPackage {
        id: package_id,
        project_id,
        course_id: Some(course_id),
        revision: Revision::new(1)?,
        files: vec![PackageFile {
            path: "README.md".to_owned(),
            object: ArtifactRef {
                artifact_id: ArtifactId::new(),
                store_binding: "approved-package-v1".to_owned(),
                object_version: "package-version-1".to_owned(),
                size_bytes: 1,
                media_type: "text/markdown".to_owned(),
            },
        }],
        retention: RetentionSnapshot {
            policy_id: PolicyId::new(),
            policy_revision: Revision::new(1)?,
            class: RetentionClass::CourseMaterial,
            retain_until: "2026-12-31T08:00:00.000Z".parse()?,
            disposition: RetentionDisposition::Delete,
        },
        completed_at: now,
    };
    package.validate()?;
    sqlx::query(
        "INSERT INTO control.problem_packages \
         (package_id,project_id,course_id,revision,manifest_sha256,contract,completed_at) \
         VALUES ($1,$2,$3,$4,$5,$6,$7)",
    )
    .bind(package.id.as_uuid())
    .bind(package.project_id.as_uuid())
    .bind(course_id.as_uuid())
    .bind(i64::try_from(package.revision.get())?)
    .bind(Sha256Digest::of_bytes(b"generated-context-manifest").to_string())
    .bind(serde_json::to_value(&package)?)
    .bind(package.completed_at.get())
    .execute(&pool)
    .await?;
    let environment = environment_candidate(project_id, Some(course_id), environment_schema)?;
    let evaluation = evaluation_candidate(project_id, Some(course_id), environment.run_id, now)?;
    let run = succeeded_agent_run(
        project_id,
        Some(course_id),
        package.id,
        PolicyId::new(),
        environment.run_id,
        environment.id,
        evaluation.id,
        EnvironmentClass::Experiment,
    )?;
    let build_context = match &environment.spec.runtime {
        contracts::authoring::EnvironmentRuntimeSpec::Container { build_context, .. } => {
            build_context.clone()
        }
        contracts::authoring::EnvironmentRuntimeSpec::VirtualMachine { .. } => {
            return Err("fixture must be Container".into());
        }
    };
    let generated = GeneratedArtifactRecord {
        artifact: build_context.clone(),
        project_id,
        course_id: Some(course_id),
        package_id: package.id,
        package_revision: package.revision,
        kind: GeneratedArtifactKind::BuildContext,
        object_key: format!(
            "generated-build-contexts/{project_id}/{}.tar.gz",
            build_context.artifact_id
        ),
        content_sha256: Sha256Digest::of_bytes(b"generated-context").to_string(),
    };
    service
        .project_candidates(
            EventId::new(),
            &run,
            Some(&environment),
            Some(&evaluation),
            Some(&generated),
        )
        .await?;
    service
        .project_candidates(
            EventId::new(),
            &run,
            Some(&environment),
            Some(&evaluation),
            Some(&generated),
        )
        .await?;
    let object_key: String = sqlx::query_scalar(
        "SELECT contract->'request'->>'contextObjectKey' \
         FROM control.container_build_projections WHERE candidate_id=$1",
    )
    .bind(environment.id.as_uuid())
    .fetch_one(&pool)
    .await?;
    assert_eq!(object_key, generated.object_key);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM control.container_build_projections")
            .fetch_one(&pool)
            .await?,
        1
    );
    let mut wrong_revision = generated.clone();
    wrong_revision.package_revision = Revision::new(2)?;
    assert!(matches!(
        service
            .project_candidates(
                EventId::new(),
                &run,
                Some(&environment),
                Some(&evaluation),
                Some(&wrong_revision),
            )
            .await,
        Err(ControlError::PersistenceIdentityMismatch)
    ));
    let mut changed_key = generated.clone();
    changed_key.object_key.push_str("-changed");
    assert!(matches!(
        service
            .project_candidates(
                EventId::new(),
                &run,
                Some(&environment),
                Some(&evaluation),
                Some(&changed_key),
            )
            .await,
        Err(ControlError::ProjectionConflict)
    ));
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM control.outbox_events WHERE subject=$1",
        )
        .bind(contracts::events::subjects::AGENT_BUILD_REQUESTED)
        .fetch_one(&pool)
        .await?,
        1
    );
    let persisted_key: String = sqlx::query_scalar(
        "SELECT contract->'request'->>'contextObjectKey' \
         FROM control.container_build_projections WHERE candidate_id=$1",
    )
    .bind(environment.id.as_uuid())
    .fetch_one(&pool)
    .await?;
    assert_eq!(persisted_key, generated.object_key);
    Ok(())
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn authoring_approval_is_atomic_idempotent_and_publication_gated()
-> Result<(), Box<dyn std::error::Error>> {
    let container = Postgres::default().with_tag("17.5-alpine").start().await?;
    let url = format!(
        "postgres://postgres:postgres@127.0.0.1:{}/postgres",
        container.get_host_port_ipv4(5432).await?
    );
    let pool = PgPoolOptions::new()
        .max_connections(8)
        .connect(&url)
        .await?;
    apply_domain_migrations(&pool, Domain::Control).await?;

    let now = UtcTimestamp::from_utc(
        sqlx::query_scalar("SELECT date_trunc('milliseconds',clock_timestamp())")
            .fetch_one(&pool)
            .await?,
    )?;
    let config = control_config()?;
    let vm_base = config.virtual_machine_base.clone();
    let evaluation_runtime = EvaluationRuntimeIdentity {
        provider_binding: config.evaluation_runtime.provider_binding.clone(),
        runner_image: config.evaluation_runtime.runner_image.clone(),
    };
    let service = ControlService::new(
        pool.clone(),
        Arc::new(FixtureObjects { fail_second: false }),
        config,
    )?;
    let project_id = ProjectId::new();
    let owner = ActorId::new();
    let course_id = CourseId::new();
    insert_project(&pool, &project_fixture(project_id, owner, Some(course_id))?).await?;

    let policy = authoring_policy(project_id, Some(course_id), now)?;
    service
        .activate_project_policy(
            project_id,
            policy.clone(),
            &IdempotencyKey::parse("authoring-approval-policy")?,
        )
        .await?;
    let upload = service
        .create_project_upload(
            project_id,
            &authoring_upload_request(project_id, Some(course_id))?,
            &IdempotencyKey::parse("authoring-approval-upload")?,
            now,
        )
        .await?;
    let package = service
        .complete_project_upload(
            project_id,
            upload.id,
            upload.revision,
            &IdempotencyKey::parse("authoring-approval-package")?,
            now,
        )
        .await?;
    package.validate()?;

    let environment = vm_environment_candidate(
        project_id,
        Some(course_id),
        &vm_base,
        EnvironmentClass::Experiment,
        now,
    )?;
    let evaluation = evaluation_candidate(project_id, Some(course_id), environment.run_id, now)?;
    let run = succeeded_agent_run(
        project_id,
        Some(course_id),
        package.id,
        policy.id,
        environment.run_id,
        environment.id,
        evaluation.id,
        EnvironmentClass::Experiment,
    )?;
    service
        .project_candidates(
            EventId::new(),
            &run,
            Some(&environment),
            Some(&evaluation),
            None,
        )
        .await?;

    let image_artifact = ImageArtifact::VirtualMachine {
        id: vm_base.artifact_id,
        base_disk: vm_base.base_disk.clone(),
        format: vm_base.format,
    };
    let request = CompleteAuthoringApprovalRequest {
        project_id,
        course_id: Some(course_id),
        package_id: package.id,
        package_revision: package.revision,
        environment_candidate_id: environment.id,
        environment_candidate_revision: environment.revision,
        evaluation_candidate_id: evaluation.id,
        evaluation_candidate_revision: evaluation.revision,
        image_artifact: image_artifact.clone(),
        reason: "teacher approved the complete package".to_owned(),
    };
    let approval_key = IdempotencyKey::parse("authoring-approval-complete")?;
    let approval = service
        .complete_authoring_approval(
            project_id,
            &request,
            owner,
            &approval_key,
            now,
            "trace-authoring-approval",
        )
        .await?;
    assert_eq!(approval.package_id, package.id);
    assert_eq!(approval.environment_candidate_id, environment.id);
    assert_eq!(approval.evaluation_candidate_id, evaluation.id);
    assert_eq!(approval.image_artifact, image_artifact);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM control.authoring_approvals")
            .fetch_one(&pool)
            .await?,
        1
    );
    let pending = service
        .authoring_approval_publication_status(project_id, approval.id)
        .await?;
    assert_eq!(pending.status, AuthoringPublicationState::Pending);
    assert!(pending.environment_release_id.is_none());
    assert!(pending.evaluation_release_id.is_none());

    let replay = service
        .complete_authoring_approval(
            project_id,
            &request,
            owner,
            &approval_key,
            now,
            "trace-authoring-approval",
        )
        .await?;
    assert_eq!(replay, approval);
    let mut changed_request = request.clone();
    changed_request.reason = "different immutable decision".to_owned();
    assert!(matches!(
        service
            .complete_authoring_approval(
                project_id,
                &changed_request,
                owner,
                &approval_key,
                now,
                "trace-authoring-approval",
            )
            .await,
        Err(ControlError::IdempotencyConflict)
    ));
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM control.authoring_approval_publications"
        )
        .fetch_one(&pool)
        .await?,
        1
    );

    let publishing = match service.claim_authoring_publication(&approval, now).await? {
        AuthoringPublicationClaim::Claimed(value) => value,
        AuthoringPublicationClaim::AlreadyReady(_) => {
            return Err("fresh authoring approval was already ready".into());
        }
    };
    assert_eq!(publishing.status, AuthoringPublicationState::Publishing);
    let admission_query = AuthoringPublicationAdmissionQuery {
        project_id,
        course_id: Some(course_id),
        approval_revision: approval.revision,
        evaluation_release_id: EvaluationReleaseId::new(),
    };
    assert!(matches!(
        service
            .authoring_publication_admission(approval.id, &admission_query)
            .await,
        Err(ControlError::OperationInProgress)
    ));

    let environment_release = service
        .publish_authoring_environment_release(
            &approval,
            now,
            "trace-authoring-environment-release",
        )
        .await?;
    assert!(matches!(
        service
            .authoring_publication_admission(approval.id, &admission_query)
            .await,
        Err(ControlError::OperationInProgress)
    ));

    let evaluation_release = EvaluationRelease {
        schema_version: EVALUATION_RELEASE_SCHEMA_VERSION.to_owned(),
        id: EvaluationReleaseId::new(),
        project_id,
        course_id: Some(course_id),
        candidate_id: evaluation.id,
        candidate_revision: evaluation.revision,
        approval_id: approval.id,
        approval_revision: approval.revision,
        evaluation_spec: evaluation.spec.clone(),
        runtime_identity: evaluation_runtime,
        state: EvaluationReleaseState::Active,
        revision: Revision::new(1)?,
        published_by: owner,
        published_at: now,
        withdrawn_at: None,
        withdrawal_diagnostic_code: None,
    };
    evaluation_release.validate()?;
    service
        .complete_authoring_publication(
            approval.id,
            project_id,
            &environment_release,
            &evaluation_release,
            now,
        )
        .await?;
    let ready = service
        .authoring_approval_publication_status(project_id, approval.id)
        .await?;
    assert_eq!(ready.status, AuthoringPublicationState::Ready);
    assert_eq!(ready.environment_release_id, Some(environment_release.id));
    assert_eq!(ready.evaluation_release_id, Some(evaluation_release.id));
    assert_eq!(
        ready.evaluation_release_revision,
        Some(evaluation_release.revision)
    );

    let ready_query = AuthoringPublicationAdmissionQuery {
        evaluation_release_id: evaluation_release.id,
        ..admission_query
    };
    let admission = service
        .authoring_publication_admission(approval.id, &ready_query)
        .await?;
    assert_eq!(admission.environment_release_id, environment_release.id);
    assert_eq!(admission.evaluation_release_id, evaluation_release.id);
    service
        .complete_authoring_publication(
            approval.id,
            project_id,
            &environment_release,
            &evaluation_release,
            now,
        )
        .await?;
    Ok(())
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn authoring_publication_failure_is_durable_and_not_admissible()
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
    apply_domain_migrations(&pool, Domain::Control).await?;

    let now = UtcTimestamp::from_utc(
        sqlx::query_scalar("SELECT date_trunc('milliseconds',clock_timestamp())")
            .fetch_one(&pool)
            .await?,
    )?;
    let config = control_config()?;
    let service = ControlService::new(
        pool.clone(),
        Arc::new(FixtureObjects { fail_second: false }),
        config.clone(),
    )?;
    let project_id = ProjectId::new();
    let course_id = CourseId::new();
    let actor_id = ActorId::new();
    let approval = AuthoringApproval {
        id: ApprovalId::new(),
        project_id,
        course_id: Some(course_id),
        revision: Revision::new(1)?,
        package_id: ProblemPackageId::new(),
        package_revision: Revision::new(1)?,
        environment_candidate_id: CandidateId::new(),
        environment_candidate_revision: Revision::new(1)?,
        evaluation_candidate_id: CandidateId::new(),
        evaluation_candidate_revision: Revision::new(1)?,
        evaluation_runtime_identity: EvaluationRuntimeIdentity {
            provider_binding: config.evaluation_runtime.provider_binding.clone(),
            runner_image: config.evaluation_runtime.runner_image.clone(),
        },
        image_artifact: ImageArtifact::VirtualMachine {
            id: config.virtual_machine_base.artifact_id,
            base_disk: config.virtual_machine_base.base_disk.clone(),
            format: config.virtual_machine_base.format,
        },
        actor_id,
        reason: "fixture approval for durable publication failure".to_owned(),
        approved_at: now,
    };
    approval.validate()?;
    let approval_contract = serde_json::to_value(&approval)?;
    sqlx::query(
        "INSERT INTO control.authoring_approvals \
         (approval_id,project_id,course_id,revision,package_id,package_revision,\
          environment_candidate_id,environment_candidate_revision,evaluation_candidate_id,\
          evaluation_candidate_revision,image_artifact_id,actor_id,reason,contract,approved_at) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15)",
    )
    .bind(approval.id.as_uuid())
    .bind(project_id.as_uuid())
    .bind(Some(course_id.as_uuid()))
    .bind(i64::try_from(approval.revision.get())?)
    .bind(approval.package_id.as_uuid())
    .bind(i64::try_from(approval.package_revision.get())?)
    .bind(approval.environment_candidate_id.as_uuid())
    .bind(i64::try_from(
        approval.environment_candidate_revision.get(),
    )?)
    .bind(approval.evaluation_candidate_id.as_uuid())
    .bind(i64::try_from(approval.evaluation_candidate_revision.get())?)
    .bind(approval.image_artifact.id().as_uuid())
    .bind(actor_id.as_uuid())
    .bind(&approval.reason)
    .bind(approval_contract)
    .bind(now.get())
    .execute(&pool)
    .await?;

    let publication = AuthoringApprovalPublicationStatus {
        approval: approval.clone(),
        status: AuthoringPublicationState::Pending,
        environment_release_id: None,
        evaluation_release_id: None,
        evaluation_release_revision: None,
        diagnostic_code: None,
        updated_at: now,
        revision: Revision::new(1)?,
    };
    let publication_contract = serde_json::to_value(&publication)?;
    sqlx::query(
        "INSERT INTO control.authoring_approval_publications \
         (approval_id,project_id,course_id,state,environment_release_id,evaluation_release_id,\
          evaluation_release_revision,diagnostic_code,updated_at,revision,contract) \
         VALUES ($1,$2,$3,'pending',NULL,NULL,NULL,NULL,$4,$5,$6)",
    )
    .bind(approval.id.as_uuid())
    .bind(project_id.as_uuid())
    .bind(Some(course_id.as_uuid()))
    .bind(now.get())
    .bind(i64::try_from(publication.revision.get())?)
    .bind(publication_contract)
    .execute(&pool)
    .await?;

    let diagnostic = DiagnosticCode::registered("LW_EVALUATION_RELEASE_PUBLISH_UNAVAILABLE");
    service
        .fail_authoring_publication(approval.id, project_id, diagnostic.clone(), now)
        .await?;

    let failed = service
        .authoring_approval_publication_status(project_id, approval.id)
        .await?;
    assert_eq!(failed.status, AuthoringPublicationState::Failed);
    assert_eq!(failed.diagnostic_code, Some(diagnostic.clone()));
    assert!(failed.environment_release_id.is_none());
    assert!(failed.evaluation_release_id.is_none());
    assert!(failed.evaluation_release_revision.is_none());
    assert_eq!(failed.revision, Revision::new(2)?);

    let admission_query = AuthoringPublicationAdmissionQuery {
        project_id,
        course_id: Some(course_id),
        approval_revision: approval.revision,
        evaluation_release_id: EvaluationReleaseId::new(),
    };
    assert!(matches!(
        service
            .authoring_publication_admission(approval.id, &admission_query)
            .await,
        Err(ControlError::ReleaseEvidenceInvalid)
    ));
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM control.sse_project_events \
             WHERE project_id=$1 AND event_type='authoring_approval.publication.ready.v1'",
        )
        .bind(project_id.as_uuid())
        .fetch_one(&pool)
        .await?,
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM control.sse_project_events \
             WHERE project_id=$1 AND event_type='authoring_approval.publication.failed.v1'",
        )
        .bind(project_id.as_uuid())
        .fetch_one(&pool)
        .await?,
        1
    );
    Ok(())
}

#[tokio::test]
async fn private_work_environment_approval_requires_project_owner()
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
    apply_domain_migrations(&pool, Domain::Control).await?;

    let now: UtcTimestamp = "2026-09-08T10:00:00.000Z".parse()?;
    let config = control_config()?;
    let vm_base = config.virtual_machine_base.clone();
    let service = ControlService::new(
        pool.clone(),
        Arc::new(FixtureObjects { fail_second: false }),
        config,
    )?;
    let project_id = ProjectId::new();
    let owner = ActorId::new();
    let outsider = ActorId::new();
    insert_project(&pool, &project_fixture(project_id, owner, None)?).await?;
    let policy = authoring_policy(project_id, None, now)?;
    service
        .activate_project_policy(
            project_id,
            policy.clone(),
            &IdempotencyKey::parse("private-work-policy")?,
        )
        .await?;
    let environment =
        vm_environment_candidate(project_id, None, &vm_base, EnvironmentClass::Work, now)?;
    let evaluation = evaluation_candidate(project_id, None, environment.run_id, now)?;
    let run = succeeded_agent_run(
        project_id,
        None,
        ProblemPackageId::new(),
        policy.id,
        environment.run_id,
        environment.id,
        evaluation.id,
        EnvironmentClass::Work,
    )?;
    service
        .project_candidates(
            EventId::new(),
            &run,
            Some(&environment),
            Some(&evaluation),
            None,
        )
        .await?;
    let request = CandidateDecisionRequest {
        candidate_revision: environment.revision,
        policy_revision: environment.policy_revision,
        trust_revision: Revision::new(1)?,
        decision: CandidateDecision::Approved,
        reason: "owner approved private Work environment".to_owned(),
    };
    assert!(matches!(
        service
            .decide_project_candidate(
                project_id,
                environment.id,
                AgentTrackKind::Environment,
                &request,
                outsider,
                Revision::new(1)?,
                &IdempotencyKey::parse("private-work-outsider")?,
                now,
            )
            .await,
        Err(ControlError::ProjectGovernanceDenied)
    ));
    let approval = service
        .decide_project_candidate(
            project_id,
            environment.id,
            AgentTrackKind::Environment,
            &request,
            owner,
            Revision::new(1)?,
            &IdempotencyKey::parse("private-work-owner")?,
            now,
        )
        .await?;
    assert_eq!(approval.actor_id, owner);
    Ok(())
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn work_configuration_admission_reads_live_access_memberships()
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
    apply_domain_migrations(&pool, Domain::Control).await?;
    sqlx::query(
        "DO $$ BEGIN
             IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'lw_control_runtime') THEN
                 CREATE ROLE lw_control_runtime NOLOGIN;
             END IF;
         END $$",
    )
    .execute(&pool)
    .await?;
    apply_domain_migrations(&pool, Domain::Access).await?;

    let project_id = ProjectId::new();
    let course_id = CourseId::new();
    let actor_id = ActorId::new();
    let environment_id = EnvironmentId::new();
    let environment_revision = Revision::new(1)?;
    insert_project(
        &pool,
        &project_fixture(project_id, actor_id, Some(course_id))?,
    )
    .await?;
    sqlx::query("INSERT INTO access.actors (actor_id,issuer,subject_sha256) VALUES ($1,$2,$3)")
        .bind(actor_id.as_uuid())
        .bind("https://issuer.test")
        .bind("a".repeat(64))
        .execute(&pool)
        .await?;
    sqlx::query(
        "INSERT INTO access.course_memberships
         (course_id,actor_id,role,state,revision,expires_at)
         VALUES ($1,$2,'student','active',1,NULL)",
    )
    .bind(course_id.as_uuid())
    .bind(actor_id.as_uuid())
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO access.project_memberships
         (course_id,project_id,actor_id,role,state,revision,expires_at)
         VALUES ($1,$2,$3,'student','active',1,NULL)",
    )
    .bind(course_id.as_uuid())
    .bind(project_id.as_uuid())
    .bind(actor_id.as_uuid())
    .execute(&pool)
    .await?;

    let script_artifact = ArtifactRef {
        artifact_id: ArtifactId::new(),
        store_binding: "test-store".to_owned(),
        object_version: "version-1".to_owned(),
        size_bytes: 12,
        media_type: "text/x-shellscript".to_owned(),
    };
    let plan = WorkConfigurationPlan {
        id: contracts::WorkConfigurationPlanId::new(),
        revision: Revision::new(1)?,
        script_artifact,
        verification_script_artifact: None,
        summary: "apply the approved Work configuration".to_owned(),
        requires_restart: false,
        environment_id,
        environment_revision,
    };
    let run = AgentRun {
        id: AgentRunId::new(),
        project_id,
        course_id: Some(course_id),
        package_id: ProblemPackageId::new(),
        policy_id: PolicyId::new(),
        policy_revision: Revision::new(1)?,
        purpose: AgentRunPurpose::WorkConfiguration {
            environment_id,
            environment_revision,
            actor_id,
            runtime_kind: RuntimeKind::Container,
        },
        state: AgentRunState::Running,
        revision: Revision::new(1)?,
        tracks: vec![AgentTrack {
            kind: AgentTrackKind::WorkConfiguration,
            attempts: vec![AgentAttempt {
                number: 1,
                state: AgentAttemptState::Running,
                checkpoint: None,
                usage: LlmUsage {
                    input_tokens: 1,
                    output_tokens: 1,
                    requests: 1,
                    cost_microusd: 1,
                },
                usage_observed: false,
                diagnostic_code: None,
            }],
            candidate_id: None,
        }],
        plan: Some(plan.clone()),
    };
    run.validate()?;

    let service = ControlService::new(
        pool.clone(),
        Arc::new(FixtureObjects { fail_second: false }),
        control_config()?,
    )?;
    let query = WorkConfigurationAdmissionQuery {
        project_id,
        course_id: Some(course_id),
        environment_id,
        environment_revision,
        actor_id,
        run_revision: run.revision,
        execution_id: None,
    };
    let binding = service
        .work_configuration_admission_with_run(run.id, &query, &run, None)
        .await?;
    assert_eq!(binding.run_id, run.id);
    assert_eq!(binding.project_id, project_id);
    assert_eq!(binding.actor_id, actor_id);
    assert_eq!(binding.plan, Some(plan));
    assert!(binding.preauthorization.is_none());

    sqlx::query(
        "UPDATE access.project_memberships
         SET state = 'revoked', revision = 2
         WHERE project_id = $1 AND actor_id = $2",
    )
    .bind(project_id.as_uuid())
    .bind(actor_id.as_uuid())
    .execute(&pool)
    .await?;
    assert!(matches!(
        service
            .work_configuration_admission_with_run(run.id, &query, &run, None)
            .await,
        Err(ControlError::ProjectMismatch)
    ));
    Ok(())
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn project_release_reads_are_project_scoped_and_course_filtered()
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
    apply_domain_migrations(&pool, Domain::Control).await?;
    let service = ControlService::new(
        pool.clone(),
        Arc::new(FixtureObjects { fail_second: false }),
        control_config()?,
    )?;

    let project_id = ProjectId::new();
    let project_owner = ActorId::new();
    let project_course_id = CourseId::new();
    let project = project_fixture(project_id, project_owner, Some(project_course_id))?;
    insert_project(&pool, &project).await?;
    let environment_schema = Sha256Digest::of_bytes(b"environment");
    let project_candidate =
        environment_candidate(project_id, Some(project_course_id), environment_schema)?;
    let mut project_release = release_fixture(project_course_id, project_id)?;
    project_release.candidate_id = project_candidate.id;
    project_release.agent_run_id = project_candidate.run_id;
    project_release.candidate_revision = project_candidate.revision;
    project_release.approval.candidate_id = project_candidate.id;
    project_release.approval.candidate_revision = project_candidate.revision;
    insert_environment_candidate(&pool, &project_candidate, environment_schema).await?;
    insert_release(&pool, &project_release).await?;

    let project_work_candidate = environment_candidate(project_id, None, environment_schema)?;
    let mut project_work_release = release_fixture(project_course_id, project_id)?;
    project_work_release.candidate_id = project_work_candidate.id;
    project_work_release.agent_run_id = project_work_candidate.run_id;
    project_work_release.candidate_revision = project_work_candidate.revision;
    project_work_release.approval.candidate_id = project_work_candidate.id;
    project_work_release.approval.candidate_revision = project_work_candidate.revision;
    project_work_release.course_id = None;
    insert_environment_candidate(&pool, &project_work_candidate, environment_schema).await?;
    project_work_release.id = ReleaseId::new();
    project_work_release.version = 2;
    insert_release(&pool, &project_work_release).await?;

    let foreign_project = release_fixture(CourseId::new(), ProjectId::new())?;
    insert_release(&pool, &foreign_project).await?;

    let found = service
        .project_release(project_id, project_release.id, None, project_owner)
        .await?;
    assert_eq!(found.release, project_release);
    assert!(found.withdrawal.is_none());
    assert!(matches!(
        service
            .project_release(project_id, foreign_project.id, None, project_owner)
            .await,
        Err(ControlError::NotFound)
    ));
    assert!(matches!(
        service
            .project_release(
                project_id,
                project_work_release.id,
                Some(CourseId::new()),
                project_owner,
            )
            .await,
        Err(ControlError::NotFound)
    ));
    let non_owner = ActorId::new();
    assert!(matches!(
        service
            .project_release(project_id, project_work_release.id, None, non_owner)
            .await,
        Err(ControlError::NotFound)
    ));
    assert!(
        service
            .project_releases(project_id, None, 0, 10, non_owner)
            .await?
            .is_empty()
    );

    let all = service
        .project_releases(project_id, None, 0, 10, project_owner)
        .await?;
    assert_eq!(
        all.iter().map(|view| view.release.id).collect::<Vec<_>>(),
        vec![project_release.id, project_work_release.id]
    );
    let teaching = service
        .project_releases(project_id, Some(project_course_id), 0, 10, project_owner)
        .await?;
    assert_eq!(
        teaching
            .iter()
            .map(|view| view.release.id)
            .collect::<Vec<_>>(),
        vec![project_release.id]
    );
    assert!(
        service
            .project_releases(project_id, Some(CourseId::new()), 0, 10, project_owner)
            .await?
            .is_empty()
    );
    assert!(matches!(
        service
            .project_releases(project_id, None, 0, 0, project_owner)
            .await,
        Err(ControlError::ContractInvalid)
    ));
    assert!(matches!(
        service
            .project_releases(project_id, None, 0, 101, project_owner)
            .await,
        Err(ControlError::ContractInvalid)
    ));
    Ok(())
}

fn project_fixture(
    project_id: ProjectId,
    owner_actor_id: ActorId,
    course_id: Option<CourseId>,
) -> Result<Project, Box<dyn std::error::Error>> {
    let timestamp = "2026-07-16T08:00:00.000Z".parse()?;
    Ok(Project {
        id: project_id,
        owner_actor_id,
        name: "fixture project".to_owned(),
        description: None,
        course_id,
        state: ProjectState::Active,
        revision: Revision::new(1)?,
        created_at: timestamp,
        updated_at: timestamp,
    })
}

async fn insert_project(
    pool: &sqlx::PgPool,
    project: &Project,
) -> Result<(), Box<dyn std::error::Error>> {
    let contract = serde_json::to_value(project)?;
    sqlx::query(
        "INSERT INTO control.projects \
         (project_id,owner_actor_id,name,description,course_id,state,revision,created_at,updated_at,contract) \
         VALUES ($1,$2,$3,$4,$5,'active',$6,$7,$8,$9)",
    )
    .bind(project.id.as_uuid())
    .bind(project.owner_actor_id.as_uuid())
    .bind(&project.name)
    .bind(&project.description)
    .bind(project.course_id.map(CourseId::as_uuid))
    .bind(i64::try_from(project.revision.get())?)
    .bind(project.created_at.get())
    .bind(project.updated_at.get())
    .bind(contract)
    .execute(pool)
    .await?;
    Ok(())
}

async fn insert_environment_candidate(
    pool: &sqlx::PgPool,
    candidate: &EnvironmentCandidate,
    schema_sha256: Sha256Digest,
) -> Result<(), Box<dyn std::error::Error>> {
    sqlx::query(
        "INSERT INTO control.candidates \
         (candidate_id,candidate_kind,project_id,course_id,run_id,revision,state,content_sha256,contract, \
          policy_revision,schema_sha256,projected_event_id) \
         VALUES ($1,'environment',$2,$3,$4,$5,'validated',$6,$7,$8,$9,$10)",
    )
    .bind(candidate.id.as_uuid())
    .bind(candidate.project_id.as_uuid())
    .bind(candidate.course_id.map(CourseId::as_uuid))
    .bind(candidate.run_id.as_uuid())
    .bind(i64::try_from(candidate.revision.get())?)
    .bind(Sha256Digest::of_canonical(&candidate.spec)?.to_string())
    .bind(serde_json::to_value(candidate)?)
    .bind(i64::try_from(candidate.policy_revision.get())?)
    .bind(schema_sha256.to_string())
    .bind(Uuid::now_v7())
    .execute(pool)
    .await?;
    Ok(())
}

fn environment_candidate(
    project_id: ProjectId,
    course_id: Option<CourseId>,
    _schema_sha256: Sha256Digest,
) -> Result<EnvironmentCandidate, Box<dyn std::error::Error>> {
    let spec: EnvironmentSpec = serde_json::from_value(serde_json::json!({
        "apiVersion":"environment.labweaver.io/v1",
        "kind":"EnvironmentSpec",
        "name":"code-server",
        "class":"work",
        "resources":{"cpuMillicores":1000,"memoryBytes":1_073_741_824_u64,"storageBytes":1_073_741_824_u64},
        "network":{"mode":"deny_all"},
        "entries":[{"name":"code-server","protocol":"https","servicePort":8080}],
        "security":{
            "userPolicy":"non_root_required",
            "rootFilesystemPolicy":"read_only_required",
            "privilegeEscalationPolicy":"deny",
            "publicExposurePolicy":"deny",
            "securityProfileBinding":"restricted-v1"
        },
        "runtime":{
            "kind":"container",
            "provider_binding":"container-primary-v1",
            "build_context":{
                "artifactId":ArtifactId::new(),
                "storeBinding":"approved-context-v1",
                "objectVersion":"version-1",
                "sizeBytes":7,
                "mediaType":"application/vnd.oci.image.layer.v1.tar+gzip"
            },
            "service_port":8080
        },
        "retention":{
            "policyId":PolicyId::new(),
            "policyRevision":1,
            "class":"run_evidence",
            "retainUntil":"2026-08-16T08:00:00.000Z",
            "disposition":"delete"
        }
    }))?;
    let _spec_sha256 = Sha256Digest::of_canonical(&spec)?;
    let candidate = EnvironmentCandidate {
        id: CandidateId::new(),
        run_id: AgentRunId::new(),
        project_id,
        course_id,
        revision: Revision::new(1)?,
        spec,
        policy_revision: Revision::new(1)?,
        model: "fixture-provider-v1".to_owned(),
        created_at: "2026-07-16T08:00:00.000Z".parse()?,
    };
    candidate.validate()?;
    Ok(candidate)
}

fn authoring_upload_request(
    project_id: ProjectId,
    course_id: Option<CourseId>,
) -> Result<CreateProblemPackageUploadRequest, Box<dyn std::error::Error>> {
    Ok(CreateProblemPackageUploadRequest {
        project_id,
        course_id,
        files: vec![
            ProblemPackageUploadFile {
                path: "statement.md".to_owned(),
                size_bytes: 9,
                media_type: "text/markdown".to_owned(),
            },
            ProblemPackageUploadFile {
                path: "starter/main.rs".to_owned(),
                size_bytes: 7,
                media_type: "text/plain".to_owned(),
            },
        ],
        retention_policy_revision: Revision::new(1)?,
    })
}

fn authoring_policy(
    project_id: ProjectId,
    course_id: Option<CourseId>,
    now: UtcTimestamp,
) -> Result<ProjectLlmEgressPolicy, Box<dyn std::error::Error>> {
    Ok(serde_json::from_value(serde_json::json!({
        "id": PolicyId::new(),
        "projectId": project_id,
        "courseId": course_id,
        "revision": 1,
        "binding": {
            "runtimeBinding": "claude-code-test",
            "model": "claude-sonnet-4-6-20260601",
            "claudeCodeVersion": "2.1.207",
            "maxInFlightPerWorker": 2
        },
        "budget": {
            "maxInputTokens": 100_000,
            "maxOutputTokens": 16_000,
            "maxRequests": 8,
            "maxCostMicrousd": 2_000_000,
            "timeoutMilliseconds": 120_000,
            "maxTransientRetries": 2,
            "maxSchemaRepairs": 2
        },
        "deniedDataClasses": [
            "secret",
            "token",
            "private_key",
            "personally_identifiable_information",
            "unallowlisted_student_submission"
        ],
        "studentContentMode": "manifest_allowlist_only",
        "activatedAt": now
    }))?)
}

fn vm_environment_candidate(
    project_id: ProjectId,
    course_id: Option<CourseId>,
    base: &control_service::VirtualMachineBasePolicy,
    class: EnvironmentClass,
    now: UtcTimestamp,
) -> Result<EnvironmentCandidate, Box<dyn std::error::Error>> {
    let mut candidate = environment_candidate(
        project_id,
        course_id,
        Sha256Digest::of_bytes(b"environment"),
    )?;
    let mut value = serde_json::to_value(&candidate)?;
    value["spec"]["class"] = serde_json::to_value(class)?;
    value["spec"]["entries"] = serde_json::json!([
        {"name":"ssh","protocol":"ssh","servicePort":22}
    ]);
    value["spec"]["security"]["rootFilesystemPolicy"] = serde_json::json!("mutable_required");
    value["spec"]["runtime"] = serde_json::json!({
        "kind":"virtual_machine",
        "provider_binding":base.provider_binding,
        "base_disk":base.base_disk,
        "storage_class_binding":base.storage_class_binding,
        "ssh_port":22
    });
    value["spec"]["retention"]["retainUntil"] = serde_json::to_value(now)?;
    candidate.spec = serde_json::from_value(value["spec"].clone())?;
    candidate.validate()?;
    Ok(candidate)
}

fn evaluation_candidate(
    project_id: ProjectId,
    course_id: Option<CourseId>,
    run_id: AgentRunId,
    now: UtcTimestamp,
) -> Result<EvaluationCandidate, Box<dyn std::error::Error>> {
    Ok(EvaluationCandidate {
        id: CandidateId::new(),
        run_id,
        project_id,
        course_id,
        revision: Revision::new(1)?,
        spec: EvaluationSpec::from_yaml(include_str!(
            "../../../crates/contracts/tests/fixtures/evaluation/linux/evaluation.yaml"
        ))?,
        policy_revision: Revision::new(1)?,
        model: "fixture-provider-v1".to_owned(),
        created_at: now,
    })
}

#[allow(clippy::too_many_arguments)]
fn succeeded_agent_run(
    project_id: ProjectId,
    course_id: Option<CourseId>,
    package_id: ProblemPackageId,
    policy_id: PolicyId,
    run_id: AgentRunId,
    environment_candidate_id: CandidateId,
    evaluation_candidate_id: CandidateId,
    environment_class: EnvironmentClass,
) -> Result<AgentRun, Box<dyn std::error::Error>> {
    let attempt = AgentAttempt {
        number: 1,
        state: AgentAttemptState::Succeeded,
        checkpoint: None,
        usage: LlmUsage {
            input_tokens: 1,
            output_tokens: 1,
            requests: 1,
            cost_microusd: 1,
        },
        usage_observed: true,
        diagnostic_code: None,
    };
    let run = AgentRun {
        id: run_id,
        project_id,
        course_id,
        package_id,
        policy_id,
        policy_revision: Revision::new(1)?,
        purpose: AgentRunPurpose::Authoring { environment_class },
        state: AgentRunState::Succeeded,
        revision: Revision::new(1)?,
        tracks: vec![
            AgentTrack {
                kind: AgentTrackKind::Environment,
                attempts: vec![attempt.clone()],
                candidate_id: Some(environment_candidate_id),
            },
            AgentTrack {
                kind: AgentTrackKind::Evaluation,
                attempts: vec![attempt],
                candidate_id: Some(evaluation_candidate_id),
            },
        ],
        plan: None,
    };
    run.validate()?;
    Ok(run)
}

async fn insert_release(
    pool: &sqlx::PgPool,
    release: &EnvironmentTemplateRelease,
) -> Result<(), Box<dyn std::error::Error>> {
    sqlx::query(
        "INSERT INTO control.environment_template_releases \
         (release_id,project_id,course_id,version,environment_candidate_id,candidate_revision, \
          spec_sha256,image_artifact_id,contract,published_at) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)",
    )
    .bind(release.id.as_uuid())
    .bind(release.project_id.as_uuid())
    .bind(release.course_id.map(CourseId::as_uuid))
    .bind(i64::try_from(release.version)?)
    .bind(release.candidate_id.as_uuid())
    .bind(i64::try_from(release.candidate_revision.get())?)
    .bind(Sha256Digest::of_bytes(b"environment-spec").to_string())
    .bind(release.artifact.id().as_uuid())
    .bind(serde_json::to_value(release)?)
    .bind(release.published_at.get())
    .execute(pool)
    .await?;
    Ok(())
}

fn release_fixture(
    course_id: CourseId,
    project_id: ProjectId,
) -> Result<EnvironmentTemplateRelease, Box<dyn std::error::Error>> {
    let published_at = "2026-07-16T08:00:00.000Z".parse::<UtcTimestamp>()?;
    let artifact_sha256 = Sha256Digest::of_bytes(b"container-image");
    let candidate_id = CandidateId::new();
    let artifact_id = ImageArtifactId::new();
    Ok(EnvironmentTemplateRelease {
        id: ReleaseId::new(),
        project_id,
        course_id: Some(course_id),
        version: 1,
        candidate_id,
        agent_run_id: AgentRunId::new(),
        candidate_revision: Revision::new(1)?,
        runtime_kind: contracts::authoring::RuntimeKind::Container,
        approval: contracts::authoring::CandidateApproval {
            id: ApprovalId::new(),
            candidate_id,
            candidate_revision: Revision::new(1)?,
            policy_revision: Revision::new(1)?,
            trust_revision: Revision::new(1)?,
            actor_id: ActorId::new(),
            decision: CandidateDecision::Approved,
            reason: "reviewed".to_owned(),
            decided_at: published_at,
        },
        artifact: ImageArtifact::Container {
            id: artifact_id,
            build_request_id: BuildRequestId::new(),
            repository: "registry.invalid/course/environment".to_owned(),
            digest: format!("sha256:{artifact_sha256}"),
        },
        published_by: ActorId::new(),
        published_at,
    })
}

fn upload_request(
    course_id: CourseId,
) -> Result<CreateProblemPackageUploadRequest, Box<dyn std::error::Error>> {
    Ok(CreateProblemPackageUploadRequest {
        project_id: ProjectId::new(),
        course_id: Some(course_id),
        files: vec![
            ProblemPackageUploadFile {
                path: "statement.md".to_owned(),
                size_bytes: 9,
                media_type: "text/markdown".to_owned(),
            },
            ProblemPackageUploadFile {
                path: "starter/main.rs".to_owned(),
                size_bytes: 7,
                media_type: "text/plain".to_owned(),
            },
        ],
        retention_policy_revision: Revision::new(1)?,
    })
}

fn control_config() -> Result<ControlConfig, Box<dyn std::error::Error>> {
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

                capacity_bytes: 10_737_418_240,
            },
            format: contracts::supply_chain::VirtualMachineDiskFormat::Qcow2,
        },
        evaluation_runtime: control_service::EvaluationRuntimePolicy {
            provider_binding: "evaluation-primary-v1".to_owned(),
            runner_image: format!("runner@sha256:{}", "a".repeat(64)),
        },
    })
}

struct FixtureObjects {
    fail_second: bool,
}

#[async_trait]
impl ImmutableObjectStore for FixtureObjects {
    fn binding(&self) -> &'static str {
        "fixture-v1"
    }

    async fn presign_upload(
        &self,
        key: &str,
        _: u64,
        _: &str,
        now: UtcTimestamp,
    ) -> Result<PresignedUpload, ObjectStoreError> {
        Ok(PresignedUpload {
            url: format!("https://minio.invalid/{key}"),
            required_headers: std::collections::BTreeMap::default(),
            expires_at: UtcTimestamp::from_utc(now.get() + time::Duration::seconds(900))
                .map_err(|_| ObjectStoreError::ConfigurationInvalid)?,
        })
    }

    async fn read_verified(
        &self,
        _: &str,
        expected: &ArtifactRef,
    ) -> Result<VerifiedObject, ObjectStoreError> {
        Ok(VerifiedObject {
            reference: expected.clone(),
            bytes: Vec::new(),
        })
    }

    async fn freeze_current(
        &self,
        key: &str,
        size: u64,
        media_type: &str,
    ) -> Result<VerifiedObject, ObjectStoreError> {
        if self.fail_second && key.ends_with("00001") {
            return Err(ObjectStoreError::ObjectIdentityMismatch);
        }
        Ok(verified(key, "version-1", size, media_type))
    }

    async fn delete_orphan(&self, _: &str, _: &str) -> Result<(), ObjectStoreError> {
        Ok(())
    }
}

fn verified(_: &str, version: &str, size: u64, media_type: &str) -> VerifiedObject {
    VerifiedObject {
        reference: ArtifactRef {
            artifact_id: ArtifactId::new(),
            store_binding: "fixture-v1".to_owned(),
            object_version: version.to_owned(),
            size_bytes: size,
            media_type: media_type.to_owned(),
        },
        bytes: Vec::new(),
    }
}
