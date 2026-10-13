//! Real `PostgreSQL` migration and concurrency evidence for the Issue #48 control plane.

use std::collections::BTreeSet;
use std::str::FromStr;
use std::sync::Arc;

use artifact_store::{
    ImmutableObjectStore, ObjectStoreError, PresignedUpload, S3Credential, S3ImmutableObjectStore,
    S3StoreConfig, VerifiedObject,
};
use async_trait::async_trait;
use base64::Engine as _;
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
    EnvironmentPublicationAdmissionQuery, GeneratedArtifactKind, GeneratedArtifactRecord,
    IdempotencyKey, PlatformImageEntry, PlatformImageKind, PlatformImageStatus,
    ProblemPackageRetentionChoice, ProblemPackageUploadFile, WorkConfigurationAdmissionQuery,
};
use contracts::supply_chain::BuildNetworkPolicy;
use contracts::supply_chain::{
    EnvironmentTemplateRelease, ImageArtifact, VirtualMachineBaseDisk, VirtualMachineDiskFormat,
};
use contracts::{
    ActorId, AgentRunId, ApprovalId, ArtifactId, ArtifactRef, BuildRequestId, CandidateId,
    CourseId, DiagnosticCode, EnvironmentId, EvaluationReleaseId, EventId, ImageArtifactId,
    PlatformImageId, PolicyId, ProblemPackageId, Project, ProjectId, ProjectState, ReleaseId,
    RetentionClass, RetentionDisposition, RetentionSnapshot, Revision, UtcTimestamp,
};
use control_service::{
    AuthoringPublicationClaim, ContainerBuildPolicy, ControlConfig, ControlError, ControlService,
};
use persistence_sqlx::{Domain, Sha256Digest};
use sqlx::{Row, postgres::PgPoolOptions};
use testcontainers::core::{IntoContainerPort, WaitFor};
use testcontainers::{GenericImage, ImageExt, runners::AsyncRunner};
use testcontainers_modules::postgres::Postgres;
use uuid::Uuid;

#[path = "cases/membership_governance.rs"]
mod membership_governance_cases;
#[path = "cases/runtime_authorization.rs"]
mod runtime_authorization_cases;
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
    // A lost completion response still resolves to the same upload, even with a new retry key.
    assert_eq!(
        service
            .complete_upload(
                course,
                session.id,
                session.revision,
                &IdempotencyKey::parse("issue-197-completion-new-key")?,
                now
            )
            .await?,
        package
    );
    let status = service
        .project_upload(request.project_id, session.id, now)
        .await?;
    assert_eq!(status.completed_package, Some(package.clone()));
    assert!(status.upload_targets.is_empty());
    assert_eq!(
        package.files[0].sha256,
        Sha256Digest::of_bytes(b"xxxxxxx").to_string()
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM control.problem_packages")
            .fetch_one(&pool)
            .await?,
        1
    );

    let permanent_course = contracts::CourseId::new();
    let mut permanent_request = upload_request(permanent_course)?;
    permanent_request.retention_choice = ProblemPackageRetentionChoice::Permanent;
    let permanent_session = service
        .create_upload(
            permanent_course,
            &permanent_request,
            &IdempotencyKey::parse("issue-48-permanent-create")?,
            now,
        )
        .await?;
    let permanent_package = service
        .complete_upload(
            permanent_course,
            permanent_session.id,
            permanent_session.revision,
            &IdempotencyKey::parse("issue-48-permanent-complete")?,
            now,
        )
        .await?;
    assert!(permanent_package.retention.is_permanent());
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            "SELECT retention_choice FROM control.problem_package_upload_sessions WHERE upload_id=$1",
        )
        .bind(permanent_session.id.as_uuid())
        .fetch_one(&pool)
        .await?,
        "permanent"
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
async fn project_policy_updates_require_current_revision_and_replay_idempotently()
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

    let project_id = ProjectId::new();
    let project = project_fixture(project_id, ActorId::new(), None)?;
    insert_project(&pool, &project).await?;
    let service = ControlService::new(
        pool.clone(),
        Arc::new(FixtureObjects { fail_second: false }),
        control_config()?,
    )?;

    let created = service
        .activate_project_policy(
            project_id,
            authoring_policy(project_id, None, "2026-07-16T08:00:00.000Z".parse()?)?,
            &IdempotencyKey::parse("project-policy-create")?,
            None,
        )
        .await?;
    assert_eq!(created.revision, Revision::new(1)?);

    let update_key = IdempotencyKey::parse("project-policy-update")?;
    let update_policy = authoring_policy(project_id, None, "2026-07-16T08:00:01.000Z".parse()?)?;
    let updated = service
        .activate_project_policy(
            project_id,
            update_policy.clone(),
            &update_key,
            Some(created.revision),
        )
        .await?;
    assert_eq!(updated.revision, Revision::new(2)?);

    let stale = service
        .activate_project_policy(
            project_id,
            authoring_policy(project_id, None, "2026-07-16T08:00:02.000Z".parse()?)?,
            &IdempotencyKey::parse("project-policy-stale")?,
            Some(created.revision),
        )
        .await;
    assert!(matches!(stale, Err(ControlError::RevisionConflict)));

    let missing = service
        .activate_project_policy(
            project_id,
            authoring_policy(project_id, None, "2026-07-16T08:00:03.000Z".parse()?)?,
            &IdempotencyKey::parse("project-policy-missing-fence")?,
            None,
        )
        .await;
    assert!(matches!(missing, Err(ControlError::RevisionConflict)));

    let replay = service
        .activate_project_policy(
            project_id,
            update_policy,
            &update_key,
            Some(created.revision),
        )
        .await?;
    assert_eq!(replay, updated);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM control.project_llm_policies WHERE project_id=$1",
        )
        .bind(project_id.as_uuid())
        .fetch_one(&pool)
        .await?,
        2
    );
    assert_eq!(service.active_project_policy(project_id).await?, updated);
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
            &[],
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
        "files": [{"path": "context.tar.gz", "object": build_context,
                   "sha256": Sha256Digest::of_bytes(b"context").to_string()}],
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
        None,
        EnvironmentClass::Work,
    )?;
    let cross_course_context = service
        .project_candidates(
            EventId::new(),
            &run,
            Some(&environment_candidate),
            None,
            None,
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
            &[],
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
        .project_environment_candidate_view(project_id, environment_candidate.id, &[])
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
        match &build_event.data.request.source {
            contracts::supply_chain::BuildSource::Dockerfile { context, .. } => context,
            contracts::supply_chain::BuildSource::ExportedOci { .. } => {
                return Err("fixture must use the Dockerfile source".into());
            }
        },
        match &environment_candidate.spec.runtime {
            contracts::authoring::EnvironmentRuntimeSpec::Container { build_context, .. } => {
                build_context
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
        .environment_candidate_view(course_id, environment_candidate.id, &[])
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
            &[],
        )
        .await?;
    let work_admission = service
        .environment_publication_admission(
            release.id,
            &EnvironmentPublicationAdmissionQuery {
                project_id: environment_candidate.project_id,
                course_id: Some(course_id),
                environment_release_version: release.version,
            },
        )
        .await?;
    assert_eq!(
        work_admission, None,
        "a validated Work release has an explicit null Evaluation admission"
    );
    assert!(matches!(
        service
            .environment_publication_admission(
                release.id,
                &EnvironmentPublicationAdmissionQuery {
                    project_id: environment_candidate.project_id,
                    course_id: Some(course_id),
                    environment_release_version: release.version + 1,
                },
            )
            .await,
        Err(ControlError::ReleaseNotFound)
    ));
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
    assert!(matches!(
        service
            .environment_publication_admission(
                release.id,
                &EnvironmentPublicationAdmissionQuery {
                    project_id: environment_candidate.project_id,
                    course_id: Some(course_id),
                    environment_release_version: release.version,
                },
            )
            .await,
        Err(ControlError::ReleaseNotFound)
    ));
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
        .project_environment_candidate_view(project_id, current_candidate.id, &[])
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
            sha256: "0".repeat(64),
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
            retain_until: Some("2026-12-31T08:00:00.000Z".parse()?),
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
        Some(evaluation.id),
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
            None,
            Some(&generated),
            None,
        )
        .await?;
    service
        .project_candidates(
            EventId::new(),
            &run,
            Some(&environment),
            Some(&evaluation),
            None,
            Some(&generated),
            None,
        )
        .await?;
    let object_key: String = sqlx::query_scalar(
        "SELECT contract->'request'->'source'->>'contextObjectKey' \
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
                None,
                Some(&wrong_revision),
                None,
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
                None,
                Some(&changed_key),
                None,
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
        "SELECT contract->'request'->'source'->>'contextObjectKey' \
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
async fn exported_sandbox_image_is_enqueued_as_an_import_source()
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
    let course_id = CourseId::new();
    let owner = ActorId::new();
    insert_project(&pool, &project_fixture(project_id, owner, Some(course_id))?).await?;
    let now: UtcTimestamp = "2026-07-16T08:00:00.000Z".parse()?;
    let environment = environment_candidate(
        project_id,
        Some(course_id),
        Sha256Digest::of_bytes(b"exported"),
    )?;
    let evaluation = evaluation_candidate(project_id, Some(course_id), environment.run_id, now)?;
    let run = succeeded_agent_run(
        project_id,
        Some(course_id),
        ProblemPackageId::new(),
        PolicyId::new(),
        environment.run_id,
        environment.id,
        Some(evaluation.id),
        EnvironmentClass::Experiment,
    )?;
    let export = contracts::supply_chain::ExportedOciImage {
        layout: ArtifactRef {
            artifact_id: ArtifactId::new(),
            store_binding: "object-store-v1".to_owned(),
            object_version: "sandbox-export-version-1".to_owned(),
            size_bytes: 4_096,
            media_type: "application/vnd.oci.image.layout.v1+tar".to_owned(),
        },
        layout_object_key: format!("authoring-sandbox/{}/export.tar", environment.run_id),
    };
    service
        .project_candidates(
            EventId::new(),
            &run,
            Some(&environment),
            Some(&evaluation),
            Some(&export),
            None,
            None,
        )
        .await?;
    let source_kind: String = sqlx::query_scalar(
        "SELECT contract->'request'->'source'->>'kind' \
         FROM control.container_build_projections WHERE candidate_id=$1",
    )
    .bind(environment.id.as_uuid())
    .fetch_one(&pool)
    .await?;
    assert_eq!(source_kind, "exported_oci");
    let persisted_key: String = sqlx::query_scalar(
        "SELECT contract->'request'->'source'->'image'->>'layoutObjectKey' \
         FROM control.container_build_projections WHERE candidate_id=$1",
    )
    .bind(environment.id.as_uuid())
    .fetch_one(&pool)
    .await?;
    assert_eq!(persisted_key, export.layout_object_key);
    let object_version: String = sqlx::query_scalar(
        "SELECT contract->'request'->'source'->'image'->'layout'->>'objectVersion' \
         FROM control.container_build_projections WHERE candidate_id=$1",
    )
    .bind(environment.id.as_uuid())
    .fetch_one(&pool)
    .await?;
    assert_eq!(object_version, export.layout.object_version);

    service
        .project_candidates(
            EventId::new(),
            &run,
            Some(&environment),
            Some(&evaluation),
            Some(&export),
            None,
            None,
        )
        .await?;
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM control.outbox_events WHERE subject=$1",
        )
        .bind(contracts::events::subjects::AGENT_BUILD_REQUESTED)
        .fetch_one(&pool)
        .await?,
        1
    );
    let mut changed_key = export.clone();
    changed_key.layout_object_key.push_str("-changed");
    assert!(matches!(
        service
            .project_candidates(
                EventId::new(),
                &run,
                Some(&environment),
                Some(&evaluation),
                Some(&changed_key),
                None,
                None,
            )
            .await,
        Err(ControlError::ProjectionConflict)
    ));
    let build_id: Uuid = sqlx::query_scalar(
        "SELECT build_request_id FROM control.container_build_projections WHERE candidate_id=$1",
    )
    .bind(environment.id.as_uuid())
    .fetch_one(&pool)
    .await?;
    let build_id = BuildRequestId::from_str(&build_id.to_string())?;
    service
        .project_build_failure(
            EventId::new(),
            project_id,
            Some(course_id),
            &contracts::events::AgentBuildFailed {
                build_request_id: build_id,
                diagnostic_code: "LW_AGENT_BUILD_CANCELLED".to_owned(),
                retryable: false,
                cleanup_verified: true,
            },
        )
        .await?;
    // A replayed authoring outcome must not rebuild the cancelled immutable candidate.
    service
        .project_candidates(
            EventId::new(),
            &run,
            Some(&environment),
            Some(&evaluation),
            Some(&export),
            None,
            None,
        )
        .await?;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM control.outbox_events WHERE subject=$1")
            .bind(contracts::events::subjects::AGENT_BUILD_REQUESTED)
            .fetch_one(&pool)
            .await?,
        1
    );
    let artifact = ImageArtifact::Container {
        id: ImageArtifactId::new(),
        build_request_id: build_id,
        repository: "harbor.internal/labweaver-system/late-cancelled-image".to_owned(),
        digest: format!("sha256:{}", "a".repeat(64)),
    };
    assert!(matches!(
        service
            .project_artifact(EventId::new(), project_id, Some(course_id), &artifact)
            .await,
        Err(ControlError::ProjectionConflict)
    ));
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            "SELECT state FROM control.container_build_projections WHERE build_request_id=$1"
        )
        .bind(build_id.as_uuid())
        .fetch_one(&pool)
        .await?,
        "cancelled"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM control.image_artifact_projections WHERE image_artifact_id=$1"
        )
        .bind(match artifact {
            ImageArtifact::Container { id, .. } => id.as_uuid(),
            ImageArtifact::VirtualMachine { .. } => unreachable!(),
        })
        .fetch_one(&pool)
        .await?,
        0
    );
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
    let vm_base = config.virtual_machine_bases.clone();
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
            None,
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
    let evaluation =
        system_facts_evaluation_candidate(project_id, Some(course_id), environment.run_id, now)?;
    let run = succeeded_agent_run(
        project_id,
        Some(course_id),
        package.id,
        policy.id,
        environment.run_id,
        environment.id,
        Some(evaluation.id),
        EnvironmentClass::Experiment,
    )?;
    service
        .project_candidates(
            EventId::new(),
            &run,
            Some(&environment),
            Some(&evaluation),
            None,
            None,
            None,
        )
        .await?;

    let image_artifact = ImageArtifact::VirtualMachine {
        id: vm_base.bases[0].artifact_id,
        base_disk: vm_base.bases[0].base_disk.clone(),
        format: vm_base.bases[0].format,
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
        evaluation_runner_image_artifact: None,
        reason: "teacher approved the complete package".to_owned(),
    };
    let approval_key = IdempotencyKey::parse("authoring-approval-complete")?;
    runtime_authorization_cases::assert_assistant_cannot_approve(
        &service, &pool, project_id, &request, now,
    )
    .await?;
    let approval = service
        .complete_authoring_approval(
            project_id,
            &request,
            &verified_teacher_fixture(&pool, owner, course_id, now).await?,
            &approval_key,
            now,
            "trace-authoring-approval",
            &[],
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
            &verified_teacher_fixture(&pool, owner, course_id, now).await?,
            &approval_key,
            now,
            "trace-authoring-approval",
            &[],
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
                &verified_teacher_fixture(&pool, owner, course_id, now).await?,
                &approval_key,
                now,
                "trace-authoring-approval",
                &[],
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
            &[],
        )
        .await
        .map_err(|error| format!("publish authoring environment release failed: {error:?}"))?;
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
        .await
        .map_err(|error| format!("complete authoring publication failed: {error:?}"))?;
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
    assert!(
        ready.submission_manifest.is_none(),
        "system-facts evaluations must not project a workspace submission manifest"
    );

    Box::pin(
        runtime_authorization_cases::assert_bindings_after_publication(
            &pool,
            &service,
            &approval,
            &environment,
            &environment_release,
            now,
        ),
    )
    .await?;
    let ready_query = AuthoringPublicationAdmissionQuery {
        evaluation_release_id: evaluation_release.id,
        ..admission_query
    };
    let admission = service
        .authoring_publication_admission(approval.id, &ready_query)
        .await?;
    assert_eq!(admission.environment_release_id, environment_release.id);
    assert_eq!(admission.evaluation_release_id, evaluation_release.id);
    let environment_admission = service
        .environment_publication_admission(
            environment_release.id,
            &EnvironmentPublicationAdmissionQuery {
                project_id,
                course_id: Some(course_id),
                environment_release_version: environment_release.version,
            },
        )
        .await
        .map_err(|error| format!("experiment exact admission failed: {error:?}"))?
        .ok_or("Experiment release must return an Evaluation admission")?;
    assert_eq!(
        environment_admission.environment_release_id,
        environment_release.id
    );
    assert_eq!(
        environment_admission.environment_release_version,
        environment_release.version
    );
    assert_eq!(
        environment_admission.evaluation_release_id,
        evaluation_release.id
    );
    assert!(matches!(
        service
            .environment_publication_admission(
                environment_release.id,
                &EnvironmentPublicationAdmissionQuery {
                    project_id,
                    course_id: Some(course_id),
                    environment_release_version: environment_release.version + 1,
                },
            )
            .await,
        Err(ControlError::ReleaseNotFound)
    ));
    service
        .complete_authoring_publication(
            approval.id,
            project_id,
            &environment_release,
            &evaluation_release,
            now,
        )
        .await?;
    service
        .withdraw_project_release(
            project_id,
            environment_release.id,
            environment_release.version,
            owner,
            false,
            "SECURITY_REVOKED",
            &IdempotencyKey::parse("withdraw-authoring-environment-release")?,
            now,
            "trace-withdraw-authoring-environment-release",
        )
        .await?;
    assert!(matches!(
        service
            .environment_publication_admission(
                environment_release.id,
                &EnvironmentPublicationAdmissionQuery {
                    project_id,
                    course_id: Some(course_id),
                    environment_release_version: environment_release.version,
                },
            )
            .await,
        Err(ControlError::ReleaseNotFound)
    ));
    Ok(())
}

/// A VM base an administrator registered in the Agent image catalog is publishable through the
/// deployment policy only while the catalog entry still carries the exact reviewed disk identity.
#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn vm_base_approval_accepts_admin_catalog_identity_and_rejects_descriptor_drift()
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
    let service = ControlService::new(
        pool.clone(),
        Arc::new(FixtureObjects { fail_second: false }),
        config.clone(),
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
            &IdempotencyKey::parse("vm-base-policy")?,
            None,
        )
        .await?;
    let upload = service
        .create_project_upload(
            project_id,
            &authoring_upload_request(project_id, Some(course_id))?,
            &IdempotencyKey::parse("vm-base-upload")?,
            now,
        )
        .await?;
    let package = service
        .complete_project_upload(
            project_id,
            upload.id,
            upload.revision,
            &IdempotencyKey::parse("vm-base-package")?,
            now,
        )
        .await?;

    let catalog_digest = format!("sha256:{}", Sha256Digest::of_bytes(b"catalog-vm-base"));
    let catalog_base = VirtualMachineBaseDisk {
        binding: "rocky-9-v1".to_owned(),
        source_registry_digest: format!(
            "docker://harbor.internal/labweaver-system/rocky-9@{catalog_digest}"
        ),
        capacity_bytes: 21_474_836_480,
    };
    // The candidate declares the catalog base; the deployment policy only owns unrelated statics.
    let mut declared = config.virtual_machine_bases.clone();
    declared.bases[0].base_disk = catalog_base.clone();
    let environment = vm_environment_candidate(
        project_id,
        Some(course_id),
        &declared,
        EnvironmentClass::Experiment,
        now,
    )?;
    let evaluation =
        system_facts_evaluation_candidate(project_id, Some(course_id), environment.run_id, now)?;
    let run = succeeded_agent_run(
        project_id,
        Some(course_id),
        package.id,
        policy.id,
        environment.run_id,
        environment.id,
        Some(evaluation.id),
        EnvironmentClass::Experiment,
    )?;
    service
        .project_candidates(
            EventId::new(),
            &run,
            Some(&environment),
            Some(&evaluation),
            None,
            None,
            None,
        )
        .await?;

    let entry = PlatformImageEntry {
        catalog_id: PlatformImageId::new(),
        kind: PlatformImageKind::VirtualMachine,
        binding: catalog_base.binding.clone(),
        source_reference: "harbor.internal/labweaver-system/rocky-9:1".to_owned(),
        resolved_digest: catalog_digest.clone(),
        media_type: "application/vnd.oci.image.manifest.v1+json".to_owned(),
        size_bytes: catalog_base.capacity_bytes,
        status: PlatformImageStatus::Active,
        trust_revision: 1,
        repin_generation: 1,
        capacity_bytes: Some(catalog_base.capacity_bytes),
        disk_sha256: Some("c".repeat(64)),
        format: Some(VirtualMachineDiskFormat::Qcow2),
        pinned_at: now,
        updated_at: now,
    };
    let catalog_artifact_id: ImageArtifactId = entry.catalog_id.to_string().parse()?;
    let image_artifact = ImageArtifact::VirtualMachine {
        id: catalog_artifact_id,
        base_disk: catalog_base.clone(),
        format: VirtualMachineDiskFormat::Qcow2,
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
        evaluation_runner_image_artifact: None,
        reason: "administrator catalog base reviewed".to_owned(),
    };

    let approval = service
        .complete_authoring_approval(
            project_id,
            &request,
            &verified_teacher_fixture(&pool, owner, course_id, now).await?,
            &IdempotencyKey::parse("vm-base-accept")?,
            now,
            "trace-vm-base-accept",
            std::slice::from_ref(&entry),
        )
        .await?;
    assert_eq!(approval.image_artifact, image_artifact);
    assert_eq!(approval.image_artifact.id(), catalog_artifact_id);

    let mut digest_drift = entry.clone();
    digest_drift.resolved_digest = format!("sha256:{}", Sha256Digest::of_bytes(b"drifted"));
    let mut capacity_drift = entry.clone();
    capacity_drift.capacity_bytes = Some(catalog_base.capacity_bytes + 1);
    let mut format_drift = entry.clone();
    format_drift.format = Some(VirtualMachineDiskFormat::Raw);
    let mut disabled = entry.clone();
    disabled.status = PlatformImageStatus::Disabled;
    let mut inventory_only = entry.clone();
    inventory_only.disk_sha256 = None;
    let mut stale_trust = entry.clone();
    stale_trust.trust_revision += 1;
    let mut repository_drift = entry.clone();
    repository_drift.source_reference = "harbor.internal/other-project/rocky-9:1".to_owned();

    for (index, drifted) in [
        digest_drift,
        capacity_drift,
        format_drift,
        disabled,
        inventory_only,
        stale_trust,
        repository_drift,
    ]
    .into_iter()
    .enumerate()
    {
        assert!(
            matches!(
                service
                    .complete_authoring_approval(
                        project_id,
                        &request,
                        &verified_teacher_fixture(&pool, owner, course_id, now).await?,
                        &IdempotencyKey::parse(&format!("vm-base-reject-{index}"))?,
                        now,
                        "trace-vm-base-reject",
                        std::slice::from_ref(&drifted),
                    )
                    .await,
                Err(ControlError::ArtifactMismatch)
            ),
            "drifted catalog entry {index} must be rejected"
        );
    }

    // A declared artifact that names an id other than the resolved catalog entry is rejected too.
    let mut unknown_id = request.clone();
    unknown_id.image_artifact = ImageArtifact::VirtualMachine {
        id: ImageArtifactId::new(),
        base_disk: catalog_base,
        format: VirtualMachineDiskFormat::Qcow2,
    };
    assert!(matches!(
        service
            .complete_authoring_approval(
                project_id,
                &unknown_id,
                &verified_teacher_fixture(&pool, owner, course_id, now).await?,
                &IdempotencyKey::parse("vm-base-reject-identity")?,
                now,
                "trace-vm-base-reject",
                std::slice::from_ref(&entry),
            )
            .await,
        Err(ControlError::ArtifactMismatch)
    ));
    Ok(())
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn container_experiment_runner_image_is_built_frozen_and_fails_closed()
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
            &IdempotencyKey::parse("runner-policy")?,
            None,
        )
        .await?;
    let upload = service
        .create_project_upload(
            project_id,
            &authoring_upload_request(project_id, Some(course_id))?,
            &IdempotencyKey::parse("runner-upload")?,
            now,
        )
        .await?;
    let package = service
        .complete_project_upload(
            project_id,
            upload.id,
            upload.revision,
            &IdempotencyKey::parse("runner-package")?,
            now,
        )
        .await?;
    package.validate()?;

    let mut environment = environment_candidate(
        project_id,
        Some(course_id),
        Sha256Digest::of_bytes(b"environment"),
    )?;
    let mut environment_value = serde_json::to_value(&environment)?;
    environment_value["spec"]["class"] = serde_json::json!("experiment");
    environment.spec = serde_json::from_value(environment_value["spec"].clone())?;
    environment.validate()?;
    let environment_context = match &environment.spec.runtime {
        contracts::authoring::EnvironmentRuntimeSpec::Container { build_context, .. } => {
            build_context.clone()
        }
        contracts::authoring::EnvironmentRuntimeSpec::VirtualMachine { .. } => {
            return Err("fixture must be Container".into());
        }
    };
    let runner_context = ArtifactRef {
        artifact_id: ArtifactId::new(),
        store_binding: "approved-context-v1".to_owned(),
        object_version: "runner-version-1".to_owned(),
        size_bytes: 11,
        media_type: "application/vnd.oci.image.layer.v1.tar+gzip".to_owned(),
    };
    let mut evaluation =
        evaluation_candidate(project_id, Some(course_id), environment.run_id, now)?;
    evaluation.runner_build_context = Some(runner_context.clone());
    evaluation.validate()?;

    let run = succeeded_agent_run(
        project_id,
        Some(course_id),
        package.id,
        policy.id,
        environment.run_id,
        environment.id,
        Some(evaluation.id),
        EnvironmentClass::Experiment,
    )?;
    let environment_generated = GeneratedArtifactRecord {
        artifact: environment_context.clone(),
        project_id,
        course_id: Some(course_id),
        package_id: package.id,
        package_revision: package.revision,
        kind: GeneratedArtifactKind::BuildContext,
        object_key: format!(
            "generated-build-contexts/{project_id}/{}.tar.gz",
            environment_context.artifact_id
        ),
        content_sha256: Sha256Digest::of_bytes(b"runner-environment-context").to_string(),
    };
    let runner_generated = GeneratedArtifactRecord {
        artifact: runner_context.clone(),
        project_id,
        course_id: Some(course_id),
        package_id: package.id,
        package_revision: package.revision,
        kind: GeneratedArtifactKind::EvaluationRunnerBuildContext,
        object_key: format!(
            "generated-evaluation-runners/{project_id}/{}.tar.gz",
            runner_context.artifact_id
        ),
        content_sha256: Sha256Digest::of_bytes(b"runner-generated-context").to_string(),
    };
    service
        .project_candidates(
            EventId::new(),
            &run,
            Some(&environment),
            Some(&evaluation),
            None,
            Some(&environment_generated),
            Some(&runner_generated),
        )
        .await?;

    let projections: Vec<(String, Uuid)> = sqlx::query_as(
        "SELECT target,build_request_id FROM control.container_build_projections \
         WHERE project_id=$1 ORDER BY target",
    )
    .bind(project_id.as_uuid())
    .fetch_all(&pool)
    .await?;
    assert_eq!(
        projections.len(),
        2,
        "a Container experiment must project both an environment and a runner build"
    );
    let environment_build = projections
        .iter()
        .find(|(target, _)| target == "environment")
        .ok_or("missing environment build projection")?
        .1;
    let runner_build = projections
        .iter()
        .find(|(target, _)| target == "evaluation_runner")
        .ok_or("missing Evaluation runner build projection")?
        .1;

    let repository_prefix = "harbor.internal/labweaver-system";
    let environment_artifact = ImageArtifact::Container {
        id: ImageArtifactId::new(),
        build_request_id: BuildRequestId::from_str(&environment_build.to_string())?,
        repository: format!("{repository_prefix}/course-{course_id}-{}", environment.id),
        digest: format!("sha256:{}", "b".repeat(64)),
    };
    let runner_artifact = ImageArtifact::Container {
        id: ImageArtifactId::new(),
        build_request_id: BuildRequestId::from_str(&runner_build.to_string())?,
        repository: format!(
            "{repository_prefix}/course-{course_id}-{}-evaluation-runner",
            evaluation.id
        ),
        digest: format!("sha256:{}", "c".repeat(64)),
    };
    service
        .project_artifact(
            EventId::new(),
            project_id,
            Some(course_id),
            &environment_artifact,
        )
        .await?;
    service
        .project_artifact(
            EventId::new(),
            project_id,
            Some(course_id),
            &runner_artifact,
        )
        .await?;

    let request = CompleteAuthoringApprovalRequest {
        project_id,
        course_id: Some(course_id),
        package_id: package.id,
        package_revision: package.revision,
        environment_candidate_id: environment.id,
        environment_candidate_revision: environment.revision,
        evaluation_candidate_id: evaluation.id,
        evaluation_candidate_revision: evaluation.revision,
        image_artifact: environment_artifact.clone(),
        evaluation_runner_image_artifact: Some(runner_artifact.clone()),
        reason: "teacher approved the container experiment".to_owned(),
    };
    let mut missing_runner = request.clone();
    missing_runner.evaluation_runner_image_artifact = None;
    assert!(matches!(
        service
            .complete_authoring_approval(
                project_id,
                &missing_runner,
                &verified_teacher_fixture(&pool, owner, course_id, now).await?,
                &IdempotencyKey::parse("runner-approval-missing")?,
                now,
                "trace-runner-approval-missing",
                &[],
            )
            .await,
        Err(ControlError::EvaluationRunnerArtifactRequired)
    ));
    let mut mismatched_runner = request.clone();
    if let Some(ImageArtifact::Container { digest, .. }) =
        &mut mismatched_runner.evaluation_runner_image_artifact
    {
        *digest = format!("sha256:{}", "d".repeat(64));
    }
    assert!(matches!(
        service
            .complete_authoring_approval(
                project_id,
                &mismatched_runner,
                &verified_teacher_fixture(&pool, owner, course_id, now).await?,
                &IdempotencyKey::parse("runner-approval-mismatch")?,
                now,
                "trace-runner-approval-mismatch",
                &[],
            )
            .await,
        Err(ControlError::ArtifactMismatch)
    ));

    let approval = service
        .complete_authoring_approval(
            project_id,
            &request,
            &verified_teacher_fixture(&pool, owner, course_id, now).await?,
            &IdempotencyKey::parse("runner-approval")?,
            now,
            "trace-runner-approval",
            &[],
        )
        .await?;
    assert_eq!(
        approval.evaluation_runner_image_artifact,
        Some(runner_artifact.clone())
    );
    let expected_runner_image = match &runner_artifact {
        ImageArtifact::Container {
            repository, digest, ..
        } => format!("{repository}@{digest}"),
        ImageArtifact::VirtualMachine { .. } => {
            return Err("runner fixture must be a Container artifact".into());
        }
    };
    assert_eq!(
        approval.evaluation_runtime_identity.runner_image,
        expected_runner_image
    );
    let persisted_runner_artifact: Option<Uuid> = sqlx::query_scalar(
        "SELECT evaluation_runner_image_artifact_id FROM control.authoring_approvals \
         WHERE approval_id=$1",
    )
    .bind(approval.id.as_uuid())
    .fetch_one(&pool)
    .await?;
    assert_eq!(
        persisted_runner_artifact,
        Some(runner_artifact.id().as_uuid())
    );

    let view = service
        .project_evaluation_candidate_view(project_id, evaluation.id)
        .await?;
    assert_eq!(view.runner_image_artifact, Some(runner_artifact));
    assert_eq!(
        view.runner_build.map(|build| build.state),
        Some(contracts::http::CandidateBuildState::Succeeded)
    );
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
        teacher_scope: None,
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
            id: config.virtual_machine_bases.bases[0].artifact_id,
            base_disk: config.virtual_machine_bases.bases[0].base_disk.clone(),
            format: config.virtual_machine_bases.bases[0].format,
        },
        evaluation_runner_image_artifact: None,
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
        submission_manifest: None,
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
    let vm_base = config.virtual_machine_bases.clone();
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
            None,
        )
        .await?;
    let environment =
        vm_environment_candidate(project_id, None, &vm_base, EnvironmentClass::Work, now)?;
    let run = succeeded_agent_run(
        project_id,
        None,
        ProblemPackageId::new(),
        policy.id,
        environment.run_id,
        environment.id,
        None,
        EnvironmentClass::Work,
    )?;
    service
        .project_candidates(
            EventId::new(),
            &run,
            Some(&environment),
            None,
            None,
            None,
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
                &[],
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
            &[],
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
        "INSERT INTO control.course_memberships
         (course_id,actor_id,role,state,revision,expires_at)
         VALUES ($1,$2,'student','active',1,NULL)",
    )
    .bind(course_id.as_uuid())
    .bind(actor_id.as_uuid())
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO control.project_memberships
         (course_id,project_id,actor_id,role,object_role,state,revision,expires_at)
         VALUES ($1,$2,$3,'student','editor','active',1,NULL)",
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
        "UPDATE control.project_memberships
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
        retention_choice: contracts::http::ProblemPackageRetentionChoice::Finite,
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
            "model": "fixture-provider-v1",
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
    base: &control_service::VirtualMachineBaseCatalog,
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
        "base_disk":base.bases[0].base_disk,
        "storage_class_binding":base.storage_class_binding,
        "ssh_port":22
    });
    value["spec"]["retention"]["retainUntil"] =
        serde_json::to_value(UtcTimestamp::from_utc(now.get() + time::Duration::days(1))?)?;
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
    evaluation_candidate_from_yaml(
        project_id,
        course_id,
        run_id,
        now,
        include_str!("../../../crates/contracts/tests/fixtures/evaluation/oj/evaluation.yaml"),
    )
}

fn system_facts_evaluation_candidate(
    project_id: ProjectId,
    course_id: Option<CourseId>,
    run_id: AgentRunId,
    now: UtcTimestamp,
) -> Result<EvaluationCandidate, Box<dyn std::error::Error>> {
    evaluation_candidate_from_yaml(
        project_id,
        course_id,
        run_id,
        now,
        include_str!("../../../crates/contracts/tests/fixtures/evaluation/linux/evaluation.yaml"),
    )
}

fn evaluation_candidate_from_yaml(
    project_id: ProjectId,
    course_id: Option<CourseId>,
    run_id: AgentRunId,
    now: UtcTimestamp,
    yaml: &str,
) -> Result<EvaluationCandidate, Box<dyn std::error::Error>> {
    Ok(EvaluationCandidate {
        id: CandidateId::new(),
        run_id,
        project_id,
        course_id,
        revision: Revision::new(1)?,
        spec: EvaluationSpec::from_yaml(yaml)?,
        policy_revision: Revision::new(1)?,
        model: "fixture-provider-v1".to_owned(),
        runner_build_context: None,
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
    evaluation_candidate_id: Option<CandidateId>,
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
    let environment_track = AgentTrack {
        kind: AgentTrackKind::Environment,
        attempts: vec![attempt.clone()],
        candidate_id: Some(environment_candidate_id),
    };
    let tracks = match environment_class {
        EnvironmentClass::Experiment => vec![
            environment_track,
            AgentTrack {
                kind: AgentTrackKind::Evaluation,
                attempts: vec![attempt],
                candidate_id: Some(
                    evaluation_candidate_id.ok_or("Experiment run missing evaluation candidate")?,
                ),
            },
        ],
        EnvironmentClass::Work => vec![environment_track],
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
        tracks,
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
        retention_choice: contracts::http::ProblemPackageRetentionChoice::Finite,
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
            runner_dockerfile_path: "evaluation/Dockerfile".to_owned(),
            network: BuildNetworkPolicy::DenyAll,
            max_duration_milliseconds: 600_000,
            max_cpu_millicores: 2_000,
            max_memory_bytes: 2_147_483_648,
        },
        virtual_machine_bases: control_service::VirtualMachineBaseCatalog {
            provider_binding: "kubevirt-primary-v1".to_owned(),
            storage_class_binding: "vm-rwo-primary-v1".to_owned(),
            max_bases: 8,
            max_capacity_bytes: 137_438_953_472,
            bases: vec![control_service::VirtualMachineBasePolicy {
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
            }],
        },
        evaluation_runtime: control_service::EvaluationRuntimePolicy {
            provider_binding: "evaluation-primary-v1".to_owned(),
            runner_image: format!("runner@sha256:{}", "a".repeat(64)),
        },
        llm_policy_options: contracts::authoring::ProjectLlmPolicyOptions {
            models: vec![contracts::authoring::ProjectLlmPolicyModelOption {
                model: "fixture-provider-v1".to_owned(),
                label: "Fixture model".to_owned(),
            }],
            default_model: "fixture-provider-v1".to_owned(),
            runtime_binding: "claude-code-test".to_owned(),
            claude_code_version: "2.1.207".to_owned(),
            max_in_flight_per_worker: 2,
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
    async fn presign_upload_exact(
        &self,
        key: &str,
        size: u64,
        media_type: &str,
        now: UtcTimestamp,
    ) -> Result<PresignedUpload, ObjectStoreError> {
        self.presign_upload(key, size, media_type, now).await
    }
    async fn presign_upload_exact_until(
        &self,
        key: &str,
        size: u64,
        media_type: &str,
        now: UtcTimestamp,
        expires_at: UtcTimestamp,
    ) -> Result<PresignedUpload, ObjectStoreError> {
        let mut signed = self.presign_upload(key, size, media_type, now).await?;
        signed.expires_at = expires_at;
        Ok(signed)
    }

    async fn read_verified(
        &self,
        _: &str,
        expected: &ArtifactRef,
    ) -> Result<VerifiedObject, ObjectStoreError> {
        Ok(VerifiedObject {
            reference: expected.clone(),
            bytes: vec![
                b'x';
                usize::try_from(expected.size_bytes)
                    .map_err(|_| ObjectStoreError::ObjectTooLarge)?
            ],
        })
    }

    async fn read_verified_file(
        &self,
        key: &str,
        expected: &ArtifactRef,
    ) -> Result<artifact_store::VerifiedObjectFile, ObjectStoreError> {
        let object = self.read_verified(key, expected).await?;
        artifact_store::VerifiedObjectFile::from_bytes(object.reference, &object.bytes)
    }

    async fn freeze_current_file(
        &self,
        key: &str,
        size: u64,
        media_type: &str,
    ) -> Result<artifact_store::VerifiedObjectFile, ObjectStoreError> {
        let object = self.freeze_current(key, size, media_type).await?;
        artifact_store::VerifiedObjectFile::from_bytes(object.reference, &object.bytes)
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

    async fn freeze_current_reference(
        &self,
        key: &str,
        size: u64,
        media_type: &str,
    ) -> Result<ArtifactRef, ObjectStoreError> {
        if self.fail_second && key.ends_with("00001") {
            return Err(ObjectStoreError::ObjectNotFound);
        }
        Ok(self.freeze_current(key, size, media_type).await?.reference)
    }

    async fn presign_upload_until(
        &self,
        key: &str,
        size: u64,
        media_type: &str,
        now: UtcTimestamp,
        expires_at: UtcTimestamp,
    ) -> Result<PresignedUpload, ObjectStoreError> {
        let mut signed = self.presign_upload(key, size, media_type, now).await?;
        signed.expires_at = expires_at;
        Ok(signed)
    }

    async fn list_key_versions(&self, _: &str) -> Result<Vec<String>, ObjectStoreError> {
        Ok(vec!["version-1".to_owned()])
    }

    async fn delete_orphan(&self, _: &str, _: &str) -> Result<(), ObjectStoreError> {
        Ok(())
    }
}

async fn local_minio_image() -> Result<GenericImage, Box<dyn std::error::Error>> {
    let descriptor = std::env::var("LABWEAVER_TEST_MINIO_IMAGE")
        .map_err(|_| "LABWEAVER_TEST_MINIO_IMAGE must name a locally prepared image")?;
    let (name, tag) = descriptor
        .rsplit_once(':')
        .filter(|(name, tag)| !name.is_empty() && !tag.is_empty() && !tag.contains('/'))
        .ok_or("LABWEAVER_TEST_MINIO_IMAGE must be a local name:tag reference")?;
    let docker = testcontainers::bollard::Docker::connect_with_local_defaults()?;
    docker.inspect_image(&descriptor).await?;
    Ok(GenericImage::new(name.to_owned(), tag.to_owned()))
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn material_upload_real_minio_and_postgres_bind_versions_and_cleanup()
-> Result<(), Box<dyn std::error::Error>> {
    let minio = local_minio_image()
        .await?
        .with_exposed_port(9000.tcp())
        .with_wait_for(WaitFor::message_on_stderr("API:"))
        .with_env_var("MINIO_ROOT_USER", "labweaver-test")
        .with_env_var("MINIO_ROOT_PASSWORD", "labweaver-test-secret")
        .with_cmd(["server", "/data"])
        .start()
        .await?;
    let endpoint = format!(
        "http://127.0.0.1:{}",
        minio.get_host_port_ipv4(9000.tcp()).await?
    );
    let objects = S3ImmutableObjectStore::new_for_test(
        S3StoreConfig {
            binding: "minio-control-materials-v1".to_owned(),
            endpoint: endpoint.parse()?,
            bucket: "issue-197-control-materials".to_owned(),
            region: "labweaver-test-1".to_owned(),
            object_prefix: "problem-packages".to_owned(),
            upload_ttl_seconds: 900,
            max_object_bytes: 1_048_576,
            force_path_style: true,
            ca_bundle_file: None,
        },
        S3Credential {
            access_key_id: "labweaver-test".to_owned(),
            secret_access_key: "labweaver-test-secret".to_owned(),
            session_token: None,
        },
    )
    .await?;
    objects.ensure_bucket_for_test().await?;

    let postgres = Postgres::default().with_tag("17.5-alpine").start().await?;
    let pool = PgPoolOptions::new()
        .max_connections(8)
        .connect(&format!(
            "postgres://postgres:postgres@127.0.0.1:{}/postgres",
            postgres.get_host_port_ipv4(5432).await?
        ))
        .await?;
    apply_domain_migrations(&pool, Domain::Control).await?;
    let service = ControlService::new(pool.clone(), Arc::new(objects.clone()), control_config()?)?;
    let course = CourseId::new();
    let request = upload_request(course)?;
    let now = UtcTimestamp::from_utc(
        sqlx::query_scalar("SELECT date_trunc('milliseconds',clock_timestamp())")
            .fetch_one(&pool)
            .await?,
    )?;
    let session = service
        .create_upload(
            course,
            &request,
            &IdempotencyKey::parse("issue-197-real-create")?,
            now,
        )
        .await?;
    let client = reqwest::Client::builder().no_proxy().build()?;
    for target in &session.upload_targets {
        let bytes = match target.path.as_str() {
            "statement.md" => b"statement".as_slice(),
            "starter/main.rs" => b"main.rs".as_slice(),
            path => return Err(format!("unexpected upload target {path}").into()),
        };
        let mut upload = client.put(&target.upload_url);
        for (name, value) in &target.required_headers {
            upload = upload.header(name, value);
        }
        let response = upload.body(bytes.to_vec()).send().await?;
        assert!(
            response.status().is_success(),
            "PUT failed: {}",
            response.status()
        );
    }
    let first_target = session
        .upload_targets
        .first()
        .ok_or("material upload returned no target")?;
    let mut overwrite = client.put(&first_target.upload_url);
    for (name, value) in &first_target.required_headers {
        overwrite = overwrite.header(name, value);
    }
    assert_eq!(
        overwrite.body(b"statement".to_vec()).send().await?.status(),
        reqwest::StatusCode::PRECONDITION_FAILED
    );

    let package = service
        .complete_upload(
            course,
            session.id,
            session.revision,
            &IdempotencyKey::parse("issue-197-real-complete")?,
            now,
        )
        .await?;
    assert_eq!(package.files.len(), 2);
    assert_eq!(
        package.files[0].sha256,
        Sha256Digest::of_bytes(b"main.rs").to_string()
    );
    assert_eq!(
        package.files[1].sha256,
        Sha256Digest::of_bytes(b"statement").to_string()
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM control.problem_package_upload_files \
             WHERE upload_id=$1 AND sha256 IS NOT NULL AND object_version IS NOT NULL",
        )
        .bind(session.id.as_uuid())
        .fetch_one(&pool)
        .await?,
        2
    );
    assert_eq!(
        service
            .complete_upload(
                course,
                session.id,
                session.revision,
                &IdempotencyKey::parse("issue-197-real-complete-retry")?,
                now,
            )
            .await?,
        package
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM control.problem_packages")
            .fetch_one(&pool)
            .await?,
        1
    );

    let cancelled = service
        .create_upload(
            course,
            &request,
            &IdempotencyKey::parse("issue-197-real-cancel-create")?,
            now,
        )
        .await?;
    let cancel_keys: Vec<String> = sqlx::query_scalar(
        "SELECT object_key FROM control.problem_package_upload_files WHERE upload_id=$1 ORDER BY ordinal",
    )
    .bind(cancelled.id.as_uuid())
    .fetch_all(&pool)
    .await?;
    for target in &cancelled.upload_targets {
        let bytes = if target.path == "statement.md" {
            b"statement".as_slice()
        } else {
            b"main.rs".as_slice()
        };
        let mut upload = client.put(&target.upload_url);
        for (name, value) in &target.required_headers {
            upload = upload.header(name, value);
        }
        assert!(
            upload
                .body(bytes.to_vec())
                .send()
                .await?
                .status()
                .is_success()
        );
    }
    let cancelled = service
        .cancel_project_upload(
            request.project_id,
            cancelled.id,
            cancelled.revision,
            &IdempotencyKey::parse("issue-197-real-cancel")?,
            now,
        )
        .await?;
    assert_eq!(
        cancelled.state,
        contracts::http::ProblemPackageUploadState::Cancelled
    );
    sqlx::query(
        "UPDATE control.problem_package_upload_sessions \
         SET expires_at=$2-interval '1 second',cleanup_versions_next_attempt_at=$2-interval '1 second' \
         WHERE upload_id=$1",
    )
    .bind(cancelled.id.as_uuid())
    .bind(now.get())
    .execute(&pool)
    .await?;
    let cleanup_now = UtcTimestamp::from_utc(
        sqlx::query_scalar("SELECT date_trunc('milliseconds',clock_timestamp())")
            .fetch_one(&pool)
            .await?,
    )?;
    service.reconcile_material_uploads(cleanup_now).await?;
    let cleanup_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    for index in 0..2 {
        loop {
            let poll_now = UtcTimestamp::from_utc(
                sqlx::query_scalar("SELECT date_trunc('milliseconds',clock_timestamp())")
                    .fetch_one(&pool)
                    .await?,
            )?;
            match service.cleanup_one_object(poll_now).await? {
                control_service::CleanupOutcome::Deleted => break,
                control_service::CleanupOutcome::Idle
                    if tokio::time::Instant::now() < cleanup_deadline =>
                {
                    tokio::task::yield_now().await;
                }
                outcome => {
                    return Err(
                        format!("cleanup item {index} did not reach Deleted: {outcome:?}").into(),
                    );
                }
            }
        }
    }
    for key in cancel_keys {
        assert!(objects.list_key_versions(&key).await?.is_empty());
    }
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM control.object_cleanup_ledger WHERE upload_id=$1 AND completed_at IS NOT NULL",
        )
        .bind(cancelled.id.as_uuid())
        .fetch_one(&pool)
        .await?,
        2
    );
    Ok(())
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn material_upload_query_cancel_expiry_and_cleanup_are_project_scoped()
-> Result<(), Box<dyn std::error::Error>> {
    use contracts::http::ProblemPackageUploadState;
    let container = Postgres::default().with_tag("17.5-alpine").start().await?;
    let pool = PgPoolOptions::new()
        .connect(&format!(
            "postgres://postgres:postgres@127.0.0.1:{}/postgres",
            container.get_host_port_ipv4(5432).await?,
        ))
        .await?;
    apply_domain_migrations(&pool, Domain::Control).await?;
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
    let course = CourseId::new();
    let request = upload_request(course)?;
    let key = IdempotencyKey::parse("197-create")?;
    let session = service.create_upload(course, &request, &key, now).await?;
    assert_eq!(
        service.create_upload(course, &request, &key, now).await?,
        session
    );
    assert!(matches!(
        service
            .project_upload(ProjectId::new(), session.id, now)
            .await
            .err(),
        Some(ControlError::UploadNotFound)
    ));
    let pending = service
        .project_upload(request.project_id, session.id, now)
        .await?;
    assert_eq!(pending.uploaded_paths.len(), 2);
    assert!(pending.upload_targets.is_empty());
    let interrupted_service = ControlService::new(
        pool.clone(),
        Arc::new(FixtureObjects { fail_second: true }),
        control_config()?,
    )?;
    let resumed = interrupted_service
        .project_upload(
            request.project_id,
            session.id,
            UtcTimestamp::from_utc(now.get() + time::Duration::seconds(30))?,
        )
        .await?;
    assert_eq!(resumed.uploaded_paths, vec!["starter/main.rs"]);
    assert_eq!(resumed.upload_targets.len(), 1);
    assert_eq!(resumed.upload_targets[0].path, "statement.md");
    assert_eq!(resumed.upload_targets[0].expires_at, session.expires_at);
    assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM control.problem_package_upload_files WHERE upload_id=$1 AND sha256 IS NOT NULL")
        .bind(session.id.as_uuid()).fetch_one(&pool).await?,0);
    let cancel_key = IdempotencyKey::parse("197-cancel")?;
    assert!(matches!(
        service
            .cancel_project_upload(
                ProjectId::new(),
                session.id,
                session.revision,
                &cancel_key,
                now
            )
            .await
            .err(),
        Some(ControlError::UploadStateConflict)
    ));
    let cancelled = service
        .cancel_project_upload(
            request.project_id,
            session.id,
            session.revision,
            &cancel_key,
            now,
        )
        .await?;
    assert_eq!(cancelled.state, ProblemPackageUploadState::Cancelled);
    assert_eq!(
        service
            .cancel_project_upload(
                request.project_id,
                session.id,
                session.revision,
                &cancel_key,
                now
            )
            .await?,
        cancelled
    );
    assert!(matches!(
        service
            .complete_upload(
                course,
                session.id,
                session.revision,
                &IdempotencyKey::parse("197-cancelled-complete")?,
                now
            )
            .await
            .err(),
        Some(ControlError::UploadStateConflict)
    ));
    assert!(
        service
            .project_upload(request.project_id, session.id, now)
            .await?
            .upload_targets
            .is_empty()
    );
    service.reconcile_material_uploads(now).await?;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM control.object_cleanup_ledger")
            .fetch_one(&pool)
            .await?,
        0
    );
    sqlx::query("UPDATE control.problem_package_upload_sessions \
        SET expires_at=$2-interval '1 second',cleanup_versions_next_attempt_at=$2-interval '1 second' WHERE upload_id=$1")
        .bind(session.id.as_uuid()).bind(now.get()).execute(&pool).await?;
    service.reconcile_material_uploads(now).await?;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM control.object_cleanup_ledger")
            .fetch_one(&pool)
            .await?,
        2
    );
    let expired = service
        .create_upload(course, &request, &IdempotencyKey::parse("197-expiry")?, now)
        .await?;
    sqlx::query("UPDATE control.problem_package_upload_sessions SET expires_at=$2-interval '1 second' WHERE upload_id=$1")
        .bind(expired.id.as_uuid()).bind(now.get()).execute(&pool).await?;
    let expired = service
        .project_upload(request.project_id, expired.id, now)
        .await?;
    assert_eq!(expired.state, ProblemPackageUploadState::Expired);
    assert!(expired.upload_targets.is_empty());
    Ok(())
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
        bytes: vec![b'x'; usize::try_from(size).unwrap_or_default()],
    }
}

struct RecoveryObjects {
    fail_once: std::sync::atomic::AtomicBool,
    block: std::sync::atomic::AtomicBool,
    started: tokio::sync::Notify,
    resume: tokio::sync::Notify,
}

#[async_trait]
impl ImmutableObjectStore for RecoveryObjects {
    fn binding(&self) -> &'static str {
        "fixture-v1"
    }

    async fn delete_orphan(&self, key: &str, version: &str) -> Result<(), ObjectStoreError> {
        FixtureObjects { fail_second: false }
            .delete_orphan(key, version)
            .await
    }

    async fn presign_upload(
        &self,
        key: &str,
        size: u64,
        media_type: &str,
        now: UtcTimestamp,
    ) -> Result<PresignedUpload, ObjectStoreError> {
        FixtureObjects { fail_second: false }
            .presign_upload(key, size, media_type, now)
            .await
    }
    async fn presign_upload_exact(
        &self,
        key: &str,
        size: u64,
        media_type: &str,
        now: UtcTimestamp,
    ) -> Result<PresignedUpload, ObjectStoreError> {
        self.presign_upload(key, size, media_type, now).await
    }
    async fn presign_upload_exact_until(
        &self,
        key: &str,
        size: u64,
        media_type: &str,
        now: UtcTimestamp,
        expires_at: UtcTimestamp,
    ) -> Result<PresignedUpload, ObjectStoreError> {
        let mut signed = self.presign_upload(key, size, media_type, now).await?;
        signed.expires_at = expires_at;
        Ok(signed)
    }

    async fn read_verified(
        &self,
        key: &str,
        expected: &ArtifactRef,
    ) -> Result<VerifiedObject, ObjectStoreError> {
        FixtureObjects { fail_second: false }
            .read_verified(key, expected)
            .await
    }

    async fn freeze_current(
        &self,
        key: &str,
        size: u64,
        media_type: &str,
    ) -> Result<VerifiedObject, ObjectStoreError> {
        FixtureObjects { fail_second: false }
            .freeze_current(key, size, media_type)
            .await
    }

    async fn freeze_current_file(
        &self,
        key: &str,
        size: u64,
        media_type: &str,
    ) -> Result<artifact_store::VerifiedObjectFile, ObjectStoreError> {
        use std::sync::atomic::Ordering;
        if self.fail_once.swap(false, Ordering::SeqCst) {
            return Err(ObjectStoreError::ObjectUnavailable);
        }
        if self.block.load(Ordering::SeqCst) {
            self.started.notify_one();
            self.resume.notified().await;
        }
        FixtureObjects { fail_second: false }
            .freeze_current_file(key, size, media_type)
            .await
    }
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn transient_completion_recovers_original_intent_and_cancel_fences_an_inflight_read()
-> Result<(), Box<dyn std::error::Error>> {
    use std::sync::atomic::{AtomicBool, Ordering};
    let container = Postgres::default().with_tag("17.5-alpine").start().await?;
    let pool = PgPoolOptions::new()
        .connect(&format!(
            "postgres://postgres:postgres@127.0.0.1:{}/postgres",
            container.get_host_port_ipv4(5432).await?,
        ))
        .await?;
    apply_domain_migrations(&pool, Domain::Control).await?;
    let now = UtcTimestamp::from_utc(
        sqlx::query_scalar("SELECT date_trunc('milliseconds',clock_timestamp())")
            .fetch_one(&pool)
            .await?,
    )?;
    let objects = Arc::new(RecoveryObjects {
        fail_once: AtomicBool::new(true),
        block: AtomicBool::new(false),
        started: tokio::sync::Notify::new(),
        resume: tokio::sync::Notify::new(),
    });
    let service = ControlService::new(pool.clone(), objects.clone(), control_config()?)?;
    let course = CourseId::new();
    let request = upload_request(course)?;
    let session = service
        .create_upload(
            course,
            &request,
            &IdempotencyKey::parse("197-transient-create")?,
            now,
        )
        .await?;
    let key = IdempotencyKey::parse("197-transient-complete")?;
    assert!(matches!(
        service
            .complete_upload(course, session.id, session.revision, &key, now)
            .await,
        Err(ControlError::ObjectStore(
            ObjectStoreError::ObjectUnavailable
        ))
    ));
    let status = service
        .project_upload(request.project_id, session.id, now)
        .await?;
    assert_eq!(
        status.completion_idempotency_key.as_deref(),
        Some(key.as_str())
    );
    assert_eq!(status.completion_expected_revision, Some(session.revision));
    let result = service
        .complete_upload(course, session.id, session.revision, &key, now)
        .await?;
    assert_eq!(
        result.files[0].sha256,
        Sha256Digest::of_bytes(b"xxxxxxx").to_string()
    );

    let cancelled = service
        .create_upload(
            course,
            &request,
            &IdempotencyKey::parse("197-inflight-create")?,
            now,
        )
        .await?;
    objects.block.store(true, Ordering::SeqCst);
    let worker = service.clone();
    let task = tokio::spawn(async move {
        worker
            .complete_upload(
                course,
                cancelled.id,
                cancelled.revision,
                &IdempotencyKey::parse("197-inflight-complete")
                    .map_err(|_| ControlError::ContractInvalid)?,
                now,
            )
            .await
    });
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        objects.started.notified(),
    )
    .await?;
    let status = service
        .project_upload(request.project_id, cancelled.id, now)
        .await?;
    service
        .cancel_project_upload(
            request.project_id,
            cancelled.id,
            status.revision,
            &IdempotencyKey::parse("197-inflight-cancel")?,
            now,
        )
        .await?;
    objects.resume.notify_one();
    assert!(matches!(task.await?, Err(ControlError::OperationLeaseLost)));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM control.problem_packages")
            .fetch_one(&pool)
            .await?,
        1
    );
    Ok(())
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn object_actions_use_control_truth_and_revoke_real_idle_sse()
-> Result<(), Box<dyn std::error::Error>> {
    use auth::object_authorization::authorize_project_action;
    use contracts::object_authorization::{ObjectAction, ProjectObjectRole};
    use futures_util::StreamExt;
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
    sqlx::query("CREATE ROLE lw_control_runtime NOLOGIN")
        .execute(&pool)
        .await?;
    apply_domain_migrations(&pool, Domain::Access).await?;
    let service = ControlService::new(
        pool.clone(),
        Arc::new(FixtureObjects { fail_second: false }),
        control_config()?,
    )?;
    let owner = ActorId::new();
    let member = ActorId::new();
    for actor in [owner, member] {
        sqlx::query("INSERT INTO access.actors(actor_id,issuer,subject_sha256) VALUES($1,'https://synthetic.invalid',$2)")
            .bind(actor.as_uuid()).bind(Sha256Digest::of_bytes(actor.as_uuid().as_bytes()).to_string()).execute(&pool).await?;
    }
    let now = UtcTimestamp::from_utc(time::OffsetDateTime::now_utc().replace_nanosecond(0)?)?;
    let project = service
        .create_project(
            owner,
            contracts::PlatformRole::Student,
            &contracts::CreateProjectRequest {
                name: "synthetic authorization".to_owned(),
                description: None,
                course_id: None,
            },
            &IdempotencyKey::parse("object-project")?,
            now,
        )
        .await
        .map_err(|error| format!("create project: {error:?}"))?;
    let membership = service
        .add_project_membership(
            project.id,
            owner,
            false,
            &contracts::http::ResolvedProjectMembershipRequest {
                actor_id: member,
                role: contracts::PlatformRole::Teacher,
                object_role: ProjectObjectRole::Viewer,
                expires_at: None,
            },
            &IdempotencyKey::parse("object-member")?,
            now,
        )
        .await
        .map_err(|error| format!("add member: {error:?}"))?;
    let edit = contracts::UpdateProjectRequest {
        expected_revision: project.revision,
        name: "shared edit".to_owned(),
        description: None,
    };
    assert!(matches!(
        service
            .update_project(
                project.id,
                member,
                false,
                &edit,
                &IdempotencyKey::parse("viewer-edit")?,
                now
            )
            .await,
        Err(ControlError::ProjectGovernanceDenied)
    ));
    let current = auth::load_membership_snapshot(&pool, member.as_uuid()).await?;
    let context = auth::AuthorizationContext {
        actor: contracts::AuthenticatedActor {
            actor_id: member,
            roles: vec![contracts::PlatformRole::Teacher],
            expires_at: UtcTimestamp::from_utc(
                time::OffsetDateTime::now_utc().replace_nanosecond(0)? + time::Duration::minutes(5),
            )?,
        },
        course_memberships: current.course_memberships,
        project_memberships: current.project_memberships,
        now: time::OffsetDateTime::now_utc().replace_nanosecond(0)?,
    };
    assert!(authorize_project_action(&context, project.id, ObjectAction::Browse).is_ok());
    for action in [
        ObjectAction::Edit,
        ObjectAction::Start,
        ObjectAction::ConfigureAgent,
        ObjectAction::ManageMembers,
        ObjectAction::RequestRuntimePermissions,
    ] {
        assert!(
            authorize_project_action(&context, project.id, action).is_err(),
            "teacher identity cannot increase viewer actions"
        );
    }
    assert!(authorize_project_action(&context, ProjectId::new(), ObjectAction::Browse).is_err());
    assert_eq!(
        sqlx::query_scalar::<_, Option<String>>(
            "SELECT to_regclass('access.project_memberships')::text"
        )
        .fetch_one(&pool)
        .await?,
        None
    );
    let decision = authorize_project_action(&context, project.id, ObjectAction::Browse)?;
    let key_ring = auth::KeyRing::parse(
        "synthetic".to_owned(),
        &format!(
            "synthetic:{}",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([7_u8; 32])
        ),
    )?;
    let bff = auth::create_bff_session(
        &pool,
        &key_ring,
        auth::CreateBffSession {
            actor_id: member.as_uuid(),
            roles: vec![contracts::PlatformRole::Teacher],
            authorization_revision: 1,
            expires_at: time::OffsetDateTime::now_utc() + time::Duration::minutes(10),
            idle_ttl: time::Duration::minutes(5),
            oidc_sid: None,
            logout_hint: "synthetic-logout-hint".to_owned(),
        },
        time::OffsetDateTime::now_utc(),
    )
    .await?;
    let session_id: contracts::BffSessionId = bff.session_id.to_string().parse()?;
    let live = auth::live_authorization::LiveAuthorization::new(
        pool.clone(),
        decision.clone(),
        "streamProjectEvents",
        session_id,
    );
    assert!(live.active().await);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("http://{}/events", listener.local_addr()?);
    let stream_service = service.clone();
    let stream_decision = decision.clone();
    // The route delegates to the exact production Control SSE builder. No test business state exists.
    let router = axum::Router::new().route(
        "/events",
        axum::routing::get(move || {
            let service = stream_service.clone();
            let decision = stream_decision.clone();
            async move {
                control_service::api::event_stream::response(
                    service,
                    decision,
                    session_id,
                    Some(1),
                    256,
                )
                .await
            }
        }),
    );
    let server = tokio::spawn(async move { axum::serve(listener, router).await });
    let response = reqwest::Client::new().get(endpoint).send().await?;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let mut connection = response.bytes_stream();
    let started = std::time::Instant::now();
    service
        .remove_project_membership(
            project.id,
            member,
            owner,
            false,
            &contracts::http::RemoveProjectMembershipRequest {
                expected_revision: membership.revision,
                reason: "synthetic member left".to_owned(),
            },
            &IdempotencyKey::parse("object-remove")?,
            UtcTimestamp::from_utc(time::OffsetDateTime::now_utc().replace_nanosecond(0)?)?,
        )
        .await?;
    assert!(!live.active().await);
    let closed = tokio::time::timeout(std::time::Duration::from_secs(8), connection.next()).await?;
    assert!(
        closed.is_none(),
        "idle real TCP SSE must close and cannot deliver the revocation event to a removed member"
    );
    assert!(
        started.elapsed()
            < std::time::Duration::from_secs(
                contracts::object_authorization::MEMBER_REVOCATION_BOUND_SECONDS
            )
    );
    let snapshot = auth::load_membership_snapshot(&pool, member.as_uuid()).await?;
    let revoked_context = auth::AuthorizationContext {
        project_memberships: snapshot.project_memberships,
        ..context
    };
    assert!(authorize_project_action(&revoked_context, project.id, ObjectAction::Browse).is_err());
    assert_eq!(
        service
            .project_sse_page(
                project.id,
                Some(1),
                256,
                UtcTimestamp::from_utc(time::OffsetDateTime::now_utc().replace_nanosecond(0)?)?
            )
            .await
            .map_err(|error| format!("read post-revoke SSE: {error:?}"))?
            .len(),
        1
    );
    // Rejoining cannot resurrect the old connection or its frozen revision.
    let restored = service
        .add_project_membership(
            project.id,
            owner,
            false,
            &contracts::http::ResolvedProjectMembershipRequest {
                actor_id: member,
                role: contracts::PlatformRole::Teacher,
                object_role: ProjectObjectRole::Editor,
                expires_at: None,
            },
            &IdempotencyKey::parse("object-rejoin")?,
            UtcTimestamp::from_utc(time::OffsetDateTime::now_utc().replace_nanosecond(0)?)?,
        )
        .await?;
    assert!(restored.revision > membership.revision);
    assert!(!live.active().await);
    let edited = service
        .update_project(
            project.id,
            member,
            false,
            &edit,
            &IdempotencyKey::parse("editor-edit")?,
            UtcTimestamp::from_utc(time::OffsetDateTime::now_utc().replace_nanosecond(0)?)?,
        )
        .await?;
    assert_eq!(edited.name, "shared edit");
    assert_eq!(edited.owner_actor_id, owner);
    let snapshot = auth::load_membership_snapshot(&pool, member.as_uuid()).await?;
    let restored_context = auth::AuthorizationContext {
        project_memberships: snapshot.project_memberships,
        ..revoked_context
    };
    let restored_decision =
        authorize_project_action(&restored_context, project.id, ObjectAction::Browse)?;
    let restored_live = auth::live_authorization::LiveAuthorization::new(
        pool.clone(),
        restored_decision.clone(),
        "streamProjectEvents",
        session_id,
    );
    assert!(restored_live.active().await);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("http://{}/events", listener.local_addr()?);
    let stream_service = service.clone();
    let router = axum::Router::new().route(
        "/events",
        axum::routing::get(move || {
            let service = stream_service.clone();
            let decision = restored_decision.clone();
            async move {
                control_service::api::event_stream::response(
                    service,
                    decision,
                    session_id,
                    Some(3),
                    256,
                )
                .await
            }
        }),
    );
    let logout_server = tokio::spawn(async move { axum::serve(listener, router).await });
    let response = reqwest::Client::new().get(endpoint).send().await?;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let mut connection = response.bytes_stream();
    auth::revoke_bff_session(
        &pool,
        bff.session_id,
        "LW_AUTH_SESSION_REVOKED",
        time::OffsetDateTime::now_utc(),
    )
    .await?;
    assert!(!restored_live.active().await);
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(8), connection.next())
            .await?
            .is_none(),
        "BFF logout must close the actual idle SSE without changing membership"
    );
    logout_server.abort();
    server.abort();
    assert!(
        authorize_project_action(&restored_context, project.id, ObjectAction::Archive).is_err(),
        "editor membership must not receive archive"
    );
    assert!(matches!(
        service
            .archive_project(
                project.id,
                member,
                false,
                edited.revision,
                &IdempotencyKey::parse("editor-archive")?,
                UtcTimestamp::from_utc(time::OffsetDateTime::now_utc().replace_nanosecond(0)?)?,
            )
            .await,
        Err(control_service::ControlError::ProjectGovernanceDenied)
    ));
    let owner_archived = service
        .archive_project(
            project.id,
            owner,
            false,
            edited.revision,
            &IdempotencyKey::parse("owner-archive")?,
            UtcTimestamp::from_utc(time::OffsetDateTime::now_utc().replace_nanosecond(0)?)?,
        )
        .await?;
    assert_eq!(owner_archived.state, contracts::ProjectState::Archived);
    let admin_project = service
        .create_project(
            owner,
            contracts::PlatformRole::Student,
            &contracts::CreateProjectRequest {
                name: "admin archive project".to_owned(),
                description: None,
                course_id: None,
            },
            &IdempotencyKey::parse("admin-archive-project")?,
            UtcTimestamp::from_utc(time::OffsetDateTime::now_utc().replace_nanosecond(0)?)?,
        )
        .await?;
    let admin_archived = service
        .archive_project(
            admin_project.id,
            ActorId::new(),
            true,
            admin_project.revision,
            &IdempotencyKey::parse("admin-archive")?,
            UtcTimestamp::from_utc(time::OffsetDateTime::now_utc().replace_nanosecond(0)?)?,
        )
        .await?;
    assert_eq!(admin_archived.state, contracts::ProjectState::Archived);
    Ok(())
}

// Synthetic verified identity and its real Control instructor fact for approval fixtures.
async fn verified_teacher_fixture(
    pool: &sqlx::PgPool,
    actor_id: ActorId,
    course_id: CourseId,
    now: UtcTimestamp,
) -> Result<contracts::AuthenticatedActor, Box<dyn std::error::Error>> {
    sqlx::query("INSERT INTO control.course_memberships (course_id,actor_id,role,object_role,state,revision) VALUES ($1,$2,'teacher','instructor','active',1) ON CONFLICT (course_id,actor_id) DO NOTHING").bind(course_id.as_uuid()).bind(actor_id.as_uuid()).execute(pool).await?;
    Ok(contracts::AuthenticatedActor {
        actor_id,
        roles: vec![contracts::PlatformRole::Teacher],
        expires_at: UtcTimestamp::from_utc(now.get() + time::Duration::hours(1))?,
    })
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one longitudinal test keeps real authorization and capture facts together"
)]
async fn model_input_rechecks_current_session_roles_membership_and_object_scope()
-> Result<(), Box<dyn std::error::Error>> {
    use contracts::http::{
        ModelInputAuthorizationBinding, ModelInputAuthorizationResolveRequest,
        model_input_selection_sha256,
    };
    use sha2::Digest;
    let container = Postgres::default().with_tag("17.5-alpine").start().await?;
    let pool = PgPoolOptions::new()
        .max_connections(8)
        .connect(&format!(
            "postgres://postgres:postgres@127.0.0.1:{}/postgres",
            container.get_host_port_ipv4(5432).await?
        ))
        .await?;
    sqlx::query("CREATE ROLE lw_control_runtime NOLOGIN")
        .execute(&pool)
        .await?;
    apply_domain_migrations(&pool, Domain::Access).await?;
    apply_domain_migrations(&pool, Domain::Control).await?;
    let service = ControlService::new(
        pool.clone(),
        Arc::new(FixtureObjects { fail_second: false }),
        control_config()?,
    )?;
    let actor = ActorId::new();
    let session = contracts::BffSessionId::new();
    sqlx::query("INSERT INTO access.actors(actor_id,issuer,subject_sha256) VALUES($1,'https://synthetic.invalid',$2)").bind(actor.as_uuid()).bind("a".repeat(64)).execute(&pool).await?;
    sqlx::query("INSERT INTO access.bff_sessions(session_id,actor_id,platform_roles,authorization_revision,expires_at,idle_expires_at,encrypted_csrf_token,csrf_encryption_key_id) VALUES($1,$2,ARRAY['student'],1,date_trunc('milliseconds',clock_timestamp())+interval '1 hour',date_trunc('milliseconds',clock_timestamp())+interval '1 hour',$3,'synthetic')").bind(session.as_uuid()).bind(actor.as_uuid()).bind(vec![0u8;32]).execute(&pool).await?;
    let now = UtcTimestamp::from_utc(time::OffsetDateTime::now_utc().replace_nanosecond(0)?)?;
    let project = service
        .create_project(
            actor,
            contracts::PlatformRole::Student,
            &contracts::CreateProjectRequest {
                name: "synthetic model input".to_owned(),
                description: None,
                course_id: None,
            },
            &IdempotencyKey::parse("synthetic-model-project")?,
            now,
        )
        .await?;
    let policy = authoring_policy(project.id, None, now)?;
    service
        .activate_project_policy(
            project.id,
            policy.clone(),
            &IdempotencyKey::parse("synthetic-model-policy")?,
            None,
        )
        .await?;
    let package: ProblemPackage = serde_json::from_value(
        serde_json::json!({"id":contracts::ProblemPackageId::new(),"projectId":project.id,"courseId":null,"revision":1,"files":[{"path":"allowed.md","object":{"artifactId":ArtifactId::new(),"storeBinding":"fixture","objectVersion":"one","sizeBytes":25,"mediaType":"text/plain"}},{"path":"unselected.md","object":{"artifactId":ArtifactId::new(),"storeBinding":"fixture","objectVersion":"two","sizeBytes":1,"mediaType":"text/plain"}}],"retention":{"policyId":PolicyId::new(),"policyRevision":1,"class":"course_material","retainUntil":"2027-12-31T00:00:00.000Z","disposition":"delete"},"completedAt":now}),
    )?;
    sqlx::query("INSERT INTO control.problem_packages(package_id,project_id,course_id,revision,manifest_sha256,contract,completed_at) VALUES($1,$2,NULL,1,$3,$4,$5)").bind(package.id.as_uuid()).bind(project.id.as_uuid()).bind(Sha256Digest::of_canonical(&package.files)?.to_string()).bind(serde_json::to_value(&package)?).bind(now.get()).execute(&pool).await?;
    let paths = vec!["allowed.md".to_owned()];
    let request = ModelInputAuthorizationResolveRequest {
        binding: ModelInputAuthorizationBinding {
            session_sha256: format!("{:x}", sha2::Sha256::digest(session.to_string().as_bytes())),
            actor_id: actor,
            project_id: project.id,
            course_id: None,
            operation_id: "createProjectAgentRun".to_owned(),
            valid_until: UtcTimestamp::from_utc(now.get() + time::Duration::hours(1))?,
            package_id: package.id,
            package_revision: package.revision,
            policy_id: policy.id,
            policy_revision: policy.revision,
            selection_sha256: model_input_selection_sha256(&paths)?,
        },
        selected_paths: paths,
    };
    assert!(
        service
            .resolve_model_input_authorization(&request)
            .await
            .is_ok()
    );
    let mut altered = request.clone();
    altered.binding.project_id = ProjectId::new();
    assert!(
        service
            .resolve_model_input_authorization(&altered)
            .await
            .is_err()
    );
    altered = request.clone();
    altered.selected_paths = vec!["unselected.md".to_owned()];
    assert!(
        service
            .resolve_model_input_authorization(&altered)
            .await
            .is_err()
    );
    sqlx::query("UPDATE control.project_memberships SET object_role='viewer',revision=revision+1 WHERE project_id=$1 AND actor_id=$2").bind(project.id.as_uuid()).bind(actor.as_uuid()).execute(&pool).await?;
    assert!(
        service
            .resolve_model_input_authorization(&request)
            .await
            .is_err()
    );
    // Current session roles, rather than a previously authenticated administrator role, determine permission.
    sqlx::query("UPDATE control.project_memberships SET state='revoked' WHERE project_id=$1 AND actor_id=$2").bind(project.id.as_uuid()).bind(actor.as_uuid()).execute(&pool).await?;
    sqlx::query(
        "UPDATE access.bff_sessions SET platform_roles=ARRAY['platform_admin'] WHERE session_id=$1",
    )
    .bind(session.as_uuid())
    .execute(&pool)
    .await?;
    assert!(
        service
            .resolve_model_input_authorization(&request)
            .await
            .is_ok()
    );
    sqlx::query(
        "UPDATE access.bff_sessions SET platform_roles=ARRAY['student'] WHERE session_id=$1",
    )
    .bind(session.as_uuid())
    .execute(&pool)
    .await?;
    assert!(
        service
            .resolve_model_input_authorization(&request)
            .await
            .is_err()
    );
    sqlx::query(
        "UPDATE control.project_memberships SET state='active' WHERE project_id=$1 AND actor_id=$2",
    )
    .bind(project.id.as_uuid())
    .bind(actor.as_uuid())
    .execute(&pool)
    .await?;
    sqlx::query("UPDATE control.project_memberships SET object_role='owner' WHERE project_id=$1 AND actor_id=$2").bind(project.id.as_uuid()).bind(actor.as_uuid()).execute(&pool).await?;
    sqlx::query(
        "UPDATE access.bff_sessions SET platform_roles=ARRAY['invalid_role'] WHERE session_id=$1",
    )
    .bind(session.as_uuid())
    .execute(&pool)
    .await?;
    assert!(
        service
            .resolve_model_input_authorization(&request)
            .await
            .is_err()
    );
    sqlx::query("UPDATE access.bff_sessions SET platform_roles=ARRAY['student'],revoked_at=now() WHERE session_id=$1").bind(session.as_uuid()).execute(&pool).await?;
    assert!(
        service
            .resolve_model_input_authorization(&request)
            .await
            .is_err()
    );
    sqlx::query("UPDATE access.bff_sessions SET platform_roles=ARRAY['student'],revoked_at=NULL WHERE session_id=$1").bind(session.as_uuid()).execute(&pool).await?;
    #[cfg(unix)]
    {
        use agent_service::claude_code::{
            ClaudeCodeRuntime, ProblemPackageEgressGate, RunCancellation, TokioClaudeCodeProcess,
        };
        use agent_service::run_store::{AgentRunReservation, PostgresAgentRunStore};
        use std::os::unix::fs::PermissionsExt;
        apply_domain_migrations(&pool, Domain::Agent).await?;
        let store = PostgresAgentRunStore::new(pool.clone());
        let command = contracts::http::InternalCreateAgentRunRequest {
            input_authorization: request.binding.clone(),
            project_id: project.id,
            course_id: None,
            actor_id: actor,
            request: contracts::http::InternalAgentRunRequest::Authoring(
                contracts::http::CreateAgentRunRequest {
                    selected_paths: Some(request.selected_paths.clone()),
                    project_id: project.id,
                    course_id: None,
                    package_id: package.id,
                    package_revision: package.revision,
                    policy_id: policy.id,
                    policy_revision: policy.revision,
                    environment_class: EnvironmentClass::Experiment,
                },
            ),
            purpose: AgentRunPurpose::Authoring {
                environment_class: EnvironmentClass::Experiment,
            },
            package: package.clone(),
            object_locators: package
                .files
                .iter()
                .map(|file| {
                    (
                        file.object.artifact_id,
                        format!("synthetic/{}", file.object.artifact_id),
                    )
                })
                .collect(),
            policy: policy.clone(),
            preauthorization: None,
        };
        let AgentRunReservation::Created(run) = store
            .reserve_internal_dispatch(
                &command,
                &IdempotencyKey::parse("model-linked-capture")?,
                now,
                "synthetic-linked",
            )
            .await
            .map_err(|error| format!("model reserve: {error:?}"))?
        else {
            return Err("expected new run".into());
        };
        let selected = vec!["allowed.md".to_owned()];
        let input = ProblemPackageEgressGate::new(
            Arc::new(ModelCaptureReader),
            Arc::new(
                agent_service::classifier::DeterministicEgressClassifier::new(
                    "synthetic".to_owned(),
                    Revision::new(1)?,
                )?,
            ),
        )
        .prepare(&package, &policy, &selected)
        .await?;
        let dispatch = store
            .claim_dispatch(std::time::Duration::from_secs(30))
            .await
            .map_err(|error| format!("claim_dispatch at 9066: {error:?}"))?
            .ok_or("capture dispatch missing")?;
        store
            .bind_prepared_dispatch(&dispatch, input.sha256())
            .await
            .map_err(|error| format!("bind_prepared_dispatch at 9212: {error:?}"))?;
        let track = store
            .claim_track(
                run.id,
                AgentTrackKind::Environment,
                input.sha256(),
                "synthetic-capture",
                std::time::Duration::from_secs(30),
            )
            .await
            .map_err(|error| format!("claim_track at 9489: {error:?}"))?
            .ok_or("environment track missing")?;
        let fixture = tempfile::tempdir()?;
        let binary = fixture.path().join("claude");
        let capture = fixture.path().join("capture.jsonl");
        let output = fixture.path().join("output.jsonl");
        let session_id = Uuid::new_v4().to_string();
        let events = [
            serde_json::json!({"type":"system","subtype":"init","session_id":session_id}),
            serde_json::json!({"type":"assistant","session_id":session_id,"message":{"role":"assistant","content":[{"type":"text","text":"{}"}]}}),
            serde_json::json!({"type":"result","subtype":"success","is_error":false,"session_id":session_id,"num_turns":1,"total_cost_usd":0.001,"usage":{"input_tokens":1,"output_tokens":1},"permission_denials":[]}),
        ];
        std::fs::write(
            &output,
            events
                .iter()
                .map(serde_json::to_string)
                .collect::<Result<Vec<_>, _>>()?
                .join("\n")
                + "\n",
        )?;
        std::fs::write(
            &binary,
            "#!/bin/sh\nif [ \"$2\" = \"--version\" ]; then printf '2.1.207\\n'; exit 0; fi\n/bin/cat >> \"$LABWEAVER_CAPTURE\"\nprintf '\\n' >> \"$LABWEAVER_CAPTURE\"\n/bin/cat \"$LABWEAVER_OUTPUT\"\n",
        )?;
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755))?;
        let process = Arc::new(RevokeAfterCapture {
            inner: TokioClaudeCodeProcess::new(std::collections::BTreeMap::from([
                ("PATH".to_owned(), fixture.path().display().to_string()),
                (
                    "LABWEAVER_CAPTURE".to_owned(),
                    capture.display().to_string(),
                ),
                ("LABWEAVER_OUTPUT".to_owned(), output.display().to_string()),
            ])),
            pool: pool.clone(),
            project_id: project.id,
            actor_id: actor,
        });
        let service = Arc::new(service);
        let authorization = Arc::new(RealModelAuthorization {
            control: service.clone(),
            request: request.clone(),
            store: store.clone(),
            run_id: run.id,
        });
        let runtime = ClaudeCodeRuntime::new(policy.clone(), process.clone())?
            .with_input_authorization(authorization);
        let failure = runtime
            .generate(
                AgentTrackKind::Environment,
                input.clone(),
                RunCancellation::new(),
            )
            .await
            .err()
            .ok_or("current Control membership must stop repair")?;
        assert_eq!(
            failure.diagnostic_code(),
            "LW_MODEL_INPUT_AUTHORIZATION_DENIED"
        );
        let completed = store
            .complete_track(&track, Err(failure.clone()), now, "synthetic-capture")
            .await
            .map_err(|error| format!("complete_track at 12354: {error:?}"))?;
        let prior = completed
            .run
            .tracks
            .iter()
            .find(|track| track.kind == AgentTrackKind::Environment)
            .ok_or("prior environment missing")?
            .attempts[0]
            .clone();
        assert_eq!(prior.usage.requests, 1);
        assert!(prior.usage_observed);
        let evaluation = store
            .claim_track(
                run.id,
                AgentTrackKind::Evaluation,
                input.sha256(),
                "synthetic-capture",
                std::time::Duration::from_secs(30),
            )
            .await
            .map_err(|error| format!("claim_track at 12971: {error:?}"))?
            .ok_or("evaluation track missing")?;
        let denied = runtime
            .generate(
                AgentTrackKind::Evaluation,
                input.clone(),
                RunCancellation::new(),
            )
            .await
            .err()
            .ok_or("revoked before evaluation")?;
        let completed = store
            .complete_track(&evaluation, Err(denied), now, "synthetic-capture")
            .await
            .map_err(|error| format!("complete_track at 13404: {error:?}"))?;
        store
            .retry_track_revisioned(
                project.id,
                None,
                run.id,
                AgentTrackKind::Environment,
                completed.run.revision,
                &IdempotencyKey::parse("model-linked-retry")?,
            )
            .await
            .map_err(|error| format!("retry_track_revisioned at 13712: {error:?}"))?;
        let retry = store
            .claim_dispatch(std::time::Duration::from_secs(30))
            .await
            .map_err(|error| format!("claim_dispatch at 13823: {error:?}"))?
            .ok_or("retry dispatch missing")?;
        let denied = store
            .fail_dispatch_preparation(&retry, "LW_MODEL_INPUT_AUTHORIZATION_DENIED", now)
            .await
            .map_err(|error| format!("fail_dispatch_preparation at 14008: {error:?}"))?;
        let attempts = &denied
            .tracks
            .iter()
            .find(|track| track.kind == AgentTrackKind::Environment)
            .ok_or("retry environment missing")?
            .attempts;
        assert_eq!(attempts.len(), 2);
        assert_eq!(attempts[0], prior);
        assert_eq!(attempts[1].usage.requests, 0);
        assert!(!attempts[1].usage_observed);
        assert_eq!(
            attempts[1].state,
            contracts::authoring::AgentAttemptState::Failed
        );
        // An expired running attempt is preserved by the existing recovery rule, without claiming
        // old provider usage or cleanup. Only the new, blocked preflight has zero requests.
        let AgentRunReservation::Created(expired) = store
            .reserve_internal_dispatch(
                &command,
                &IdempotencyKey::parse("model-linked-expired")?,
                now,
                "synthetic-expired",
            )
            .await?
        else {
            return Err("expected expired fixture".into());
        };
        let dispatch = store
            .claim_dispatch(std::time::Duration::from_secs(30))
            .await
            .map_err(|error| format!("claim_dispatch at 15190: {error:?}"))?
            .ok_or("expired dispatch missing")?;
        store
            .bind_prepared_dispatch(&dispatch, input.sha256())
            .await
            .map_err(|error| format!("bind_prepared_dispatch at 15336: {error:?}"))?;
        let running = store
            .claim_track(
                expired.id,
                AgentTrackKind::Environment,
                input.sha256(),
                "synthetic-expired",
                std::time::Duration::from_secs(30),
            )
            .await
            .map_err(|error| format!("claim_track at 15619: {error:?}"))?
            .ok_or("expired track missing")?;
        let prior = running
            .run
            .tracks
            .iter()
            .find(|track| track.kind == AgentTrackKind::Environment)
            .ok_or("running track missing")?
            .attempts[0]
            .clone();
        sqlx::query("UPDATE agent.agent_track_work_items SET lease_expires_at=now()-interval '1 second' WHERE run_id=$1 AND track='environment'").bind(expired.id.as_uuid()).execute(&pool).await?;
        let dispatch = store
            .claim_dispatch(std::time::Duration::from_secs(30))
            .await
            .map_err(|error| format!("claim_dispatch at 16220: {error:?}"))?
            .ok_or("expired recovery missing")?;
        let denied = store
            .fail_dispatch_preparation(&dispatch, "LW_MODEL_INPUT_AUTHORIZATION_DENIED", now)
            .await
            .map_err(|error| format!("fail_dispatch_preparation at 16410: {error:?}"))?;
        let attempts = &denied
            .tracks
            .iter()
            .find(|track| track.kind == AgentTrackKind::Environment)
            .ok_or("expired environment missing")?
            .attempts;
        assert_eq!(attempts.len(), 2);
        assert_eq!(attempts[0].usage, prior.usage);
        assert_eq!(attempts[0].usage_observed, prior.usage_observed);
        assert_eq!(attempts[0].checkpoint, prior.checkpoint);
        assert_eq!(attempts[1].usage.requests, 0);
        assert!(!attempts[1].usage_observed);
        let sent = std::fs::read_to_string(&capture)?;
        assert_eq!(sent.lines().count(), 1);
        assert!(sent.contains("SYNTHETIC_ALLOWED_CONTENT"));
        assert!(!sent.contains("unselected.md"));
        // Durable cancellation after preparation is re-read before the first actual stdin write.
        sqlx::query("UPDATE control.project_memberships SET object_role='owner' WHERE project_id=$1 AND actor_id=$2").bind(project.id.as_uuid()).bind(actor.as_uuid()).execute(&pool).await?;
        let AgentRunReservation::Created(cancelled) = store
            .reserve_internal_dispatch(
                &command,
                &IdempotencyKey::parse("model-linked-cancel")?,
                now,
                "synthetic-cancel",
            )
            .await
            .map_err(|error| format!("model reserve: {error:?}"))?
        else {
            return Err("expected new run".into());
        };
        let reader = Arc::new(BlockingModelCaptureReader {
            started: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
        });
        let gate = ProblemPackageEgressGate::new(
            reader.clone(),
            Arc::new(
                agent_service::classifier::DeterministicEgressClassifier::new(
                    "synthetic".to_owned(),
                    Revision::new(1)?,
                )?,
            ),
        );
        let cancel_during_read = async {
            reader.started.notified().await;
            let result = store
                .request_cancellation_revisioned(
                    project.id,
                    None,
                    cancelled.id,
                    cancelled.revision,
                    &IdempotencyKey::parse("model-linked-cancel-action")?,
                    now,
                )
                .await;
            reader.release.notify_one();
            result
                .map(|_| ())
                .map_err(Box::<dyn std::error::Error>::from)
        };
        let (prepared, cancel_result) = tokio::join!(
            gate.prepare(&package, &policy, &selected),
            cancel_during_read
        );
        let input = prepared?;
        cancel_result?;
        std::fs::remove_file(&capture)?;
        let runtime = ClaudeCodeRuntime::new(policy, process)?.with_input_authorization(Arc::new(
            RealModelAuthorization {
                control: service.clone(),
                request: request.clone(),
                store: store.clone(),
                run_id: cancelled.id,
            },
        ));
        assert_eq!(
            runtime
                .generate(AgentTrackKind::Environment, input, RunCancellation::new())
                .await
                .err()
                .ok_or("durable cancellation")?
                .diagnostic_code(),
            "LW_CONFLICT"
        );
        assert!(!capture.exists());
        // An old unbound JSON dispatch is rejected individually; a valid following dispatch still claims.
        let AgentRunReservation::Created(legacy) = store
            .reserve_internal_dispatch(
                &command,
                &IdempotencyKey::parse("model-linked-legacy")?,
                now,
                "synthetic-legacy",
            )
            .await?
        else {
            return Err("expected legacy fixture".into());
        };
        sqlx::query(
            "UPDATE agent.agent_run_dispatches SET request=request->'request' WHERE run_id=$1",
        )
        .bind(legacy.id.as_uuid())
        .execute(&pool)
        .await?;
        // Cancelled dispatch may be claimed first; the legacy row is rejected on the next tick.
        let _ = store
            .claim_dispatch(std::time::Duration::from_secs(30))
            .await
            .map_err(|error| format!("claim_dispatch at 20717: {error:?}"))?;
        let _ = store
            .claim_dispatch(std::time::Duration::from_secs(30))
            .await
            .map_err(|error| format!("claim_dispatch at 20824: {error:?}"))?;
        let diagnostic: String = sqlx::query_scalar(
            "SELECT terminal_diagnostic FROM agent.agent_run_dispatches WHERE run_id=$1",
        )
        .bind(legacy.id.as_uuid())
        .fetch_one(&pool)
        .await?;
        assert_eq!(diagnostic, "LW_MODEL_INPUT_AUTHORIZATION_REQUIRED");
        store
            .reserve_internal_dispatch(
                &command,
                &IdempotencyKey::parse("model-linked-following")?,
                now,
                "synthetic-following",
            )
            .await?;
        assert!(
            store
                .claim_dispatch(std::time::Duration::from_secs(30))
                .await
                .map_err(|error| format!("claim_dispatch at 21498: {error:?}"))?
                .is_some()
        );
    }

    Ok(())
}

struct ModelCaptureReader;
#[async_trait]
impl agent_service::claude_code::ProblemPackageReader for ModelCaptureReader {
    async fn read(
        &self,
        _reference: &ArtifactRef,
        _max_bytes: usize,
    ) -> Result<Vec<u8>, agent_service::claude_code::PackageObjectReadError> {
        Ok(b"SYNTHETIC_ALLOWED_CONTENT".to_vec())
    }
}
struct RealModelAuthorization {
    control: Arc<ControlService>,
    request: contracts::http::ModelInputAuthorizationResolveRequest,
    store: agent_service::run_store::PostgresAgentRunStore,
    run_id: AgentRunId,
}
#[async_trait]
impl agent_service::claude_code::ModelInputAuthorization for RealModelAuthorization {
    async fn check(&self) -> Result<(), agent_service::claude_code::ClaudeCodeRuntimeError> {
        use agent_service::claude_code::ClaudeCodeRuntimeError;
        if self
            .store
            .cancellation_requested(self.run_id)
            .await
            .map_err(|_| ClaudeCodeRuntimeError::InputAuthorizationDenied)?
        {
            return Err(ClaudeCodeRuntimeError::Cancelled);
        }
        self.control
            .resolve_model_input_authorization(&self.request)
            .await
            .map_err(|_| ClaudeCodeRuntimeError::InputAuthorizationDenied)?;
        Ok(())
    }
}
struct RevokeAfterCapture {
    inner: agent_service::claude_code::TokioClaudeCodeProcess,
    pool: sqlx::PgPool,
    project_id: ProjectId,
    actor_id: ActorId,
}
#[async_trait]
impl agent_service::claude_code::ClaudeCodeProcess for RevokeAfterCapture {
    async fn version(&self) -> Result<String, agent_service::claude_code::ClaudeCodeProcessError> {
        self.inner.version().await
    }
    async fn execute(
        &self,
        scope: &agent_service::claude_code::ExecutionScope,
        command: agent_service::claude_code::ClaudeCodeCommand,
        cancellation: agent_service::claude_code::RunCancellation,
    ) -> Result<
        agent_service::claude_code::ClaudeCodeProcessOutput,
        agent_service::claude_code::ClaudeCodeProcessError,
    > {
        let result = self.inner.execute(scope, command, cancellation).await?;
        sqlx::query("UPDATE control.project_memberships SET object_role='viewer',revision=revision+1 WHERE project_id=$1 AND actor_id=$2").bind(self.project_id.as_uuid()).bind(self.actor_id.as_uuid()).execute(&self.pool).await.map_err(|_|agent_service::claude_code::ClaudeCodeProcessError::Io)?;
        Ok(result)
    }
    async fn recover_authoring_terminal(
        &self,
        scope: &agent_service::claude_code::AuthoringAttemptScope,
        cancellation: agent_service::claude_code::RunCancellation,
    ) -> Result<
        agent_service::claude_code::ClaudeCodeProcessOutput,
        agent_service::claude_code::ClaudeCodeProcessError,
    > {
        self.inner
            .recover_authoring_terminal(scope, cancellation)
            .await
    }
}

struct BlockingModelCaptureReader {
    started: tokio::sync::Notify,
    release: tokio::sync::Notify,
}
#[async_trait]
impl agent_service::claude_code::ProblemPackageReader for BlockingModelCaptureReader {
    async fn read(
        &self,
        _reference: &ArtifactRef,
        _max_bytes: usize,
    ) -> Result<Vec<u8>, agent_service::claude_code::PackageObjectReadError> {
        self.started.notify_one();
        self.release.notified().await;
        Ok(b"SYNTHETIC_ALLOWED_CONTENT".to_vec())
    }
}

#[cfg(unix)]
#[tokio::test]
async fn model_input_queue_outlives_initial_five_second_decision()
-> Result<(), Box<dyn std::error::Error>> {
    exercise_initial_decision_lifetime(false).await
}
#[cfg(unix)]
#[tokio::test]
async fn model_input_schema_repair_outlives_initial_five_second_decision()
-> Result<(), Box<dyn std::error::Error>> {
    exercise_initial_decision_lifetime(true).await
}

#[cfg(unix)]
struct TtlCaptureProcess {
    inner: agent_service::claude_code::TokioClaudeCodeProcess,
    delay_first_result: bool,
    sends: std::sync::atomic::AtomicUsize,
    recoveries: std::sync::atomic::AtomicUsize,
    timeouts: tokio::sync::Mutex<Vec<std::time::Duration>>,
}
#[cfg(unix)]
#[async_trait]
impl agent_service::claude_code::ClaudeCodeProcess for TtlCaptureProcess {
    async fn version(&self) -> Result<String, agent_service::claude_code::ClaudeCodeProcessError> {
        self.inner.version().await
    }
    async fn execute(
        &self,
        scope: &agent_service::claude_code::ExecutionScope,
        command: agent_service::claude_code::ClaudeCodeCommand,
        cancellation: agent_service::claude_code::RunCancellation,
    ) -> Result<
        agent_service::claude_code::ClaudeCodeProcessOutput,
        agent_service::claude_code::ClaudeCodeProcessError,
    > {
        use std::sync::atomic::Ordering;
        self.timeouts.lock().await.push(command.timeout());
        let number = self.sends.fetch_add(1, Ordering::SeqCst);
        let result = self.inner.execute(scope, command, cancellation).await?;
        if number == 0 && self.delay_first_result {
            tokio::time::sleep(std::time::Duration::from_secs(6)).await;
        }
        Ok(result)
    }
    async fn recover_authoring_terminal(
        &self,
        scope: &agent_service::claude_code::AuthoringAttemptScope,
        cancellation: agent_service::claude_code::RunCancellation,
    ) -> Result<
        agent_service::claude_code::ClaudeCodeProcessOutput,
        agent_service::claude_code::ClaudeCodeProcessError,
    > {
        self.recoveries
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.inner
            .recover_authoring_terminal(scope, cancellation)
            .await
    }
}

#[cfg(unix)]
async fn ttl_attempt_scope(
    pool: &sqlx::PgPool,
    lease: &agent_service::run_store::AgentTrackLease,
    actor_id: ActorId,
    version: &str,
) -> Result<agent_service::claude_code::AuthoringAttemptScope, Box<dyn std::error::Error>> {
    let (started_at,lease_token):(time::OffsetDateTime,Uuid)=sqlx::query_as("SELECT attempt_started_at,lease_token FROM agent.agent_track_work_items WHERE run_id=$1 AND track='environment'").bind(lease.run_id.as_uuid()).fetch_one(pool).await?;
    Ok(agent_service::claude_code::AuthoringAttemptScope {
        run_id: lease.run_id,
        project_id: lease.run.project_id,
        course_id: lease.run.course_id,
        actor_id,
        track: lease.track,
        attempt: lease.attempt,
        execution_generation: 1,
        started_at: UtcTimestamp::from_utc(started_at)?,
        worker_id: lease.worker_id.clone(),
        lease_token,
        trace_id: "synthetic-ttl".to_owned(),
        claude_code_version: version.to_owned(),
    })
}

#[cfg(unix)]
async fn reserve_ttl_attempt(
    store: &agent_service::run_store::PostgresAgentRunStore,
    command: &contracts::http::InternalCreateAgentRunRequest,
    key: &str,
    input_hash: Sha256Digest,
    now: UtcTimestamp,
) -> Result<agent_service::run_store::AgentTrackLease, Box<dyn std::error::Error>> {
    let agent_service::run_store::AgentRunReservation::Created(run) = store
        .reserve_internal_dispatch(command, &IdempotencyKey::parse(key)?, now, "synthetic-ttl")
        .await?
    else {
        return Err("new TTL fixture expected".into());
    };
    let dispatch = store
        .claim_dispatch(std::time::Duration::from_secs(30))
        .await?
        .ok_or("TTL dispatch missing")?;
    assert_eq!(dispatch.run.id, run.id);
    store.bind_prepared_dispatch(&dispatch, input_hash).await?;
    store
        .claim_track(
            run.id,
            AgentTrackKind::Environment,
            input_hash,
            "synthetic-ttl",
            std::time::Duration::from_secs(30),
        )
        .await?
        .ok_or_else(|| "TTL attempt missing".into())
}

#[cfg(unix)]
#[allow(
    clippy::too_many_lines,
    reason = "real session facts, elapsed decision TTL, capture and original task deadline form one bounded fixture"
)]
async fn exercise_initial_decision_lifetime(
    repair: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    use agent_service::claude_code::{
        ClaudeCodeRuntime, ProblemPackageEgressGate, RunCancellation, TokioClaudeCodeProcess,
    };
    use agent_service::run_store::PostgresAgentRunStore;
    use contracts::http::{
        ModelInputAuthorizationBinding, ModelInputAuthorizationResolveRequest,
        model_input_selection_sha256,
    };
    use sha2::Digest;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let container = Postgres::default().with_tag("17.5-alpine").start().await?;
    let pool = PgPoolOptions::new()
        .max_connections(8)
        .connect(&format!(
            "postgres://postgres:postgres@127.0.0.1:{}/postgres",
            container.get_host_port_ipv4(5432).await?
        ))
        .await?;
    sqlx::query("CREATE ROLE lw_control_runtime NOLOGIN")
        .execute(&pool)
        .await?;
    apply_domain_migrations(&pool, Domain::Access).await?;
    apply_domain_migrations(&pool, Domain::Control).await?;
    apply_domain_migrations(&pool, Domain::Agent).await?;
    let config = control_config()?;
    let base = config.virtual_machine_bases.clone();
    let service = Arc::new(ControlService::new(
        pool.clone(),
        Arc::new(FixtureObjects { fail_second: false }),
        config,
    )?);
    let actor = ActorId::new();
    let session = contracts::BffSessionId::new();
    sqlx::query("INSERT INTO access.actors(actor_id,issuer,subject_sha256) VALUES($1,'https://synthetic.invalid',$2)").bind(actor.as_uuid()).bind("c".repeat(64)).execute(&pool).await?;
    sqlx::query("INSERT INTO access.bff_sessions(session_id,actor_id,platform_roles,authorization_revision,expires_at,idle_expires_at,encrypted_csrf_token,csrf_encryption_key_id) VALUES($1,$2,ARRAY['student'],1,date_trunc('milliseconds',now())+interval '1 hour',date_trunc('milliseconds',now())+interval '1 hour',$3,'synthetic')").bind(session.as_uuid()).bind(actor.as_uuid()).bind(vec![0u8;32]).execute(&pool).await?;
    let clock = time::OffsetDateTime::now_utc();
    let now = UtcTimestamp::from_utc(
        clock.replace_nanosecond(clock.nanosecond() / 1_000_000 * 1_000_000)?,
    )?;
    let project = service
        .create_project(
            actor,
            contracts::PlatformRole::Student,
            &contracts::CreateProjectRequest {
                name: "synthetic decision TTL".to_owned(),
                description: None,
                course_id: None,
            },
            &IdempotencyKey::parse("ttl-project")?,
            now,
        )
        .await?;
    let mut policy = authoring_policy(project.id, None, now)?;
    policy.budget.timeout_milliseconds = 20_000;
    service
        .activate_project_policy(
            project.id,
            policy.clone(),
            &IdempotencyKey::parse("ttl-policy")?,
            None,
        )
        .await?;
    let package: ProblemPackage = serde_json::from_value(
        serde_json::json!({"id":contracts::ProblemPackageId::new(),"projectId":project.id,"courseId":null,"revision":1,"files":[{"path":"allowed.md","object":{"artifactId":ArtifactId::new(),"storeBinding":"fixture","objectVersion":"one","sizeBytes":25,"mediaType":"text/plain"}}],"retention":{"policyId":PolicyId::new(),"policyRevision":1,"class":"course_material","retainUntil":"2027-12-31T00:00:00.000Z","disposition":"delete"},"completedAt":now}),
    )?;
    sqlx::query("INSERT INTO control.problem_packages(package_id,project_id,course_id,revision,manifest_sha256,contract,completed_at) VALUES($1,$2,NULL,1,$3,$4,$5)").bind(package.id.as_uuid()).bind(project.id.as_uuid()).bind(Sha256Digest::of_canonical(&package.files)?.to_string()).bind(serde_json::to_value(&package)?).bind(now.get()).execute(&pool).await?;
    let paths = vec!["allowed.md".to_owned()];
    let request = ModelInputAuthorizationResolveRequest {
        binding: ModelInputAuthorizationBinding {
            session_sha256: format!("{:x}", sha2::Sha256::digest(session.to_string().as_bytes())),
            actor_id: actor,
            project_id: project.id,
            course_id: None,
            operation_id: "createProjectAgentRun".to_owned(),
            valid_until: UtcTimestamp::from_utc(now.get() + time::Duration::seconds(5))?,
            package_id: package.id,
            package_revision: package.revision,
            policy_id: policy.id,
            policy_revision: policy.revision,
            selection_sha256: model_input_selection_sha256(&paths)?,
        },
        selected_paths: paths.clone(),
    };
    let command = contracts::http::InternalCreateAgentRunRequest {
        input_authorization: request.binding.clone(),
        project_id: project.id,
        course_id: None,
        actor_id: actor,
        request: contracts::http::InternalAgentRunRequest::Authoring(
            contracts::http::CreateAgentRunRequest {
                selected_paths: Some(paths.clone()),
                project_id: project.id,
                course_id: None,
                package_id: package.id,
                package_revision: package.revision,
                policy_id: policy.id,
                policy_revision: policy.revision,
                environment_class: EnvironmentClass::Work,
            },
        ),
        purpose: AgentRunPurpose::Authoring {
            environment_class: EnvironmentClass::Work,
        },
        package: package.clone(),
        object_locators: package
            .files
            .iter()
            .map(|file| {
                (
                    file.object.artifact_id,
                    format!("synthetic/{}", file.object.artifact_id),
                )
            })
            .collect(),
        policy: policy.clone(),
        preauthorization: None,
    };
    let input = ProblemPackageEgressGate::new(
        Arc::new(ModelCaptureReader),
        Arc::new(
            agent_service::classifier::DeterministicEgressClassifier::new(
                "synthetic".to_owned(),
                Revision::new(1)?,
            )?,
        ),
    )
    .prepare(&package, &policy, &paths)
    .await?;
    let store = PostgresAgentRunStore::new(pool.clone());
    let lease = reserve_ttl_attempt(&store, &command, "ttl-positive", input.sha256(), now).await?;
    let expired_lease =
        reserve_ttl_attempt(&store, &command, "ttl-task-timeout", input.sha256(), now).await?;
    let scope =
        ttl_attempt_scope(&pool, &lease, actor, &policy.binding.claude_code_version).await?;
    let snapshot:(String,String,String,String)=sqlx::query_as("SELECT d.request::text,d.dispatch_sha256,r.input_sha256,d.prepared_input_sha256 FROM agent.agent_run_dispatches d JOIN agent.agent_runs r USING(run_id) WHERE d.run_id=$1").bind(lease.run_id.as_uuid()).fetch_one(&pool).await?;
    let fixture = tempfile::tempdir()?;
    let binary = fixture.path().join("claude");
    let capture = fixture.path().join("capture.jsonl");
    let counter = fixture.path().join("count");
    let bad = fixture.path().join("bad.jsonl");
    let good = fixture.path().join("good.jsonl");
    let candidate = serde_json::to_string(
        &vm_environment_candidate(project.id, None, &base, EnvironmentClass::Work, now)?.spec,
    )?;
    for (output, text) in [(&bad, "{}"), (&good, candidate.as_str())] {
        let session_id = Uuid::new_v4().to_string();
        let events = [
            serde_json::json!({"type":"system","subtype":"init","session_id":session_id}),
            serde_json::json!({"type":"assistant","session_id":session_id,"message":{"role":"assistant","content":[{"type":"text","text":text}]}}),
            serde_json::json!({"type":"result","subtype":"success","is_error":false,"session_id":session_id,"num_turns":1,"total_cost_usd":0.001,"usage":{"input_tokens":1,"output_tokens":1},"permission_denials":[]}),
        ];
        std::fs::write(
            output,
            events
                .iter()
                .map(serde_json::to_string)
                .collect::<Result<Vec<_>, _>>()?
                .join("\n")
                + "\n",
        )?;
    }
    std::fs::write(
        &binary,
        "#!/bin/sh\nif [ \"$2\" = \"--version\" ]; then printf '2.1.207\\n'; exit 0; fi\n/bin/cat >> \"$LABWEAVER_CAPTURE\"\nprintf '\\n' >> \"$LABWEAVER_CAPTURE\"\nif [ -f \"$LABWEAVER_COUNT\" ]; then /bin/cat \"$LABWEAVER_GOOD\"; else printf '1' > \"$LABWEAVER_COUNT\"; /bin/cat \"$LABWEAVER_BAD\"; fi\n",
    )?;
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755))?;
    if !repair {
        std::fs::write(&counter, b"1")?;
    }
    let process = Arc::new(TtlCaptureProcess {
        inner: TokioClaudeCodeProcess::new(std::collections::BTreeMap::from([
            ("PATH".to_owned(), fixture.path().display().to_string()),
            (
                "LABWEAVER_CAPTURE".to_owned(),
                capture.display().to_string(),
            ),
            ("LABWEAVER_COUNT".to_owned(), counter.display().to_string()),
            ("LABWEAVER_BAD".to_owned(), bad.display().to_string()),
            ("LABWEAVER_GOOD".to_owned(), good.display().to_string()),
        ])),
        delay_first_result: repair,
        sends: AtomicUsize::new(0),
        recoveries: AtomicUsize::new(0),
        timeouts: tokio::sync::Mutex::new(Vec::new()),
    });
    let authorization = Arc::new(RealModelAuthorization {
        control: service.clone(),
        request: request.clone(),
        store: store.clone(),
        run_id: lease.run_id,
    });
    let runtime = ClaudeCodeRuntime::new(policy.clone(), process.clone())?
        .with_input_authorization(authorization);
    if !repair {
        tokio::time::sleep(std::time::Duration::from_secs(6)).await;
    }
    let execution = runtime
        .generate_authoring(
            &scope,
            input.clone(),
            RunCancellation::new(),
            EnvironmentClass::Work,
            &[],
        )
        .await.inspect_err(|failure| {
            let captures=if capture.exists() {std::fs::read_to_string(&capture).map_or(usize::MAX,|sent|sent.lines().count())} else {0};
            let elapsed=(time::OffsetDateTime::now_utc()-now.get()).whole_milliseconds();
            eprintln!("initial-decision red evidence: repair={repair}, elapsed_ms={elapsed}, actual_stdin_captures={captures}, diagnostic={}",failure.diagnostic_code());
            assert!(elapsed>5_000);
            assert_eq!(captures,usize::from(repair));
        })?;
    assert!(time::OffsetDateTime::now_utc() > request.binding.valid_until.get());
    let expected = if repair { 2 } else { 1 };
    assert_eq!(execution.audit.usage.requests, expected);
    assert!(execution.audit.usage_observed);
    assert_eq!(
        process.sends.load(Ordering::SeqCst),
        usize::try_from(expected)?
    );
    let sent = std::fs::read_to_string(&capture)?;
    assert_eq!(sent.lines().count(), usize::try_from(expected)?);
    for line in sent.lines() {
        assert!(line.contains("SYNTHETIC_ALLOWED_CONTENT"));
    }
    let timeouts = process.timeouts.lock().await;
    assert!(
        timeouts.iter().all(|timeout| *timeout
            < std::time::Duration::from_millis(policy.budget.timeout_milliseconds))
    );
    if repair {
        assert!(timeouts[1] < timeouts[0]);
    }
    drop(timeouts);
    let completed_at = time::OffsetDateTime::now_utc();
    store
        .complete_track(
            &lease,
            Ok(execution),
            UtcTimestamp::from_utc(
                completed_at
                    .replace_nanosecond(completed_at.nanosecond() / 1_000_000 * 1_000_000)?,
            )?,
            "synthetic-ttl",
        )
        .await?;
    let after:(String,String,String,String)=sqlx::query_as("SELECT d.request::text,d.dispatch_sha256,r.input_sha256,d.prepared_input_sha256 FROM agent.agent_run_dispatches d JOIN agent.agent_runs r USING(run_id) WHERE d.run_id=$1").bind(lease.run_id.as_uuid()).fetch_one(&pool).await?;
    assert_eq!(snapshot, after);
    std::fs::remove_file(&capture)?;
    let denied_lease =
        reserve_ttl_attempt(&store, &command, "ttl-current-denials", input.sha256(), now).await?;
    let denied_scope = ttl_attempt_scope(
        &pool,
        &denied_lease,
        actor,
        &policy.binding.claude_code_version,
    )
    .await?;
    let denied_runtime = ClaudeCodeRuntime::new(policy.clone(), process.clone())?
        .with_input_authorization(Arc::new(RealModelAuthorization {
            control: service.clone(),
            request: request.clone(),
            store: store.clone(),
            run_id: denied_lease.run_id,
        }));
    for update in [
        "expires_at=now()-interval '1 second',idle_expires_at=now()-interval '1 second'",
        "idle_expires_at=now()-interval '1 second'",
        "revoked_at=now()",
    ] {
        sqlx::query(&format!(
            "UPDATE access.bff_sessions SET {update} WHERE session_id=$1"
        ))
        .bind(session.as_uuid())
        .execute(&pool)
        .await?;
        let failure = denied_runtime
            .generate_authoring(
                &denied_scope,
                input.clone(),
                RunCancellation::new(),
                EnvironmentClass::Work,
                &[],
            )
            .await
            .err()
            .ok_or("current original session must deny")?;
        assert_eq!(
            failure.diagnostic_code(),
            "LW_MODEL_INPUT_AUTHORIZATION_DENIED"
        );
        assert!(!capture.exists());
        sqlx::query("UPDATE access.bff_sessions SET expires_at=date_trunc('milliseconds',now())+interval '1 hour',idle_expires_at=date_trunc('milliseconds',now())+interval '1 hour',revoked_at=NULL WHERE session_id=$1").bind(session.as_uuid()).execute(&pool).await?;
    }
    sqlx::query("UPDATE control.project_memberships SET state='revoked' WHERE project_id=$1 AND actor_id=$2").bind(project.id.as_uuid()).bind(actor.as_uuid()).execute(&pool).await?;
    assert_eq!(
        denied_runtime
            .generate_authoring(
                &denied_scope,
                input.clone(),
                RunCancellation::new(),
                EnvironmentClass::Work,
                &[]
            )
            .await
            .err()
            .ok_or("member withdrawal must deny")?
            .diagnostic_code(),
        "LW_MODEL_INPUT_AUTHORIZATION_DENIED"
    );
    assert!(!capture.exists());
    sqlx::query(
        "UPDATE control.project_memberships SET state='active' WHERE project_id=$1 AND actor_id=$2",
    )
    .bind(project.id.as_uuid())
    .bind(actor.as_uuid())
    .execute(&pool)
    .await?;
    store
        .request_cancellation_revisioned(
            project.id,
            None,
            denied_lease.run_id,
            denied_lease.run.revision,
            &IdempotencyKey::parse("ttl-durable-cancel")?,
            now,
        )
        .await?;
    assert_eq!(
        denied_runtime
            .generate_authoring(
                &denied_scope,
                input.clone(),
                RunCancellation::new(),
                EnvironmentClass::Work,
                &[]
            )
            .await
            .err()
            .ok_or("durable cancel must deny")?
            .diagnostic_code(),
        "LW_CONFLICT"
    );
    assert!(!capture.exists());
    let expired_scope = ttl_attempt_scope(
        &pool,
        &expired_lease,
        actor,
        &policy.binding.claude_code_version,
    )
    .await?;
    let deadline = expired_scope.started_at.get()
        + time::Duration::milliseconds(i64::try_from(policy.budget.timeout_milliseconds)?);
    if let Ok(wait) = std::time::Duration::try_from(
        deadline - time::OffsetDateTime::now_utc() + time::Duration::milliseconds(100),
    ) {
        tokio::time::sleep(wait).await;
    }
    let expired_runtime = ClaudeCodeRuntime::new(policy.clone(), process.clone())?
        .with_input_authorization(Arc::new(RealModelAuthorization {
            control: service,
            request,
            store: store.clone(),
            run_id: expired_lease.run_id,
        }));
    let failure = expired_runtime
        .generate_authoring(
            &expired_scope,
            input,
            RunCancellation::new(),
            EnvironmentClass::Work,
            &[],
        )
        .await
        .err()
        .ok_or("original task deadline must stop sending")?;
    assert_eq!(failure.diagnostic_code(), "LW_PROVIDER_TIMEOUT");
    assert!(!capture.exists());
    assert_eq!(
        process.sends.load(Ordering::SeqCst),
        usize::try_from(expected)?
    );
    assert_eq!(process.recoveries.load(Ordering::SeqCst), 1);
    assert_eq!(failure.audit().usage.requests, 0);
    Ok(())
}
