//! Real `PostgreSQL` migration and concurrency evidence for the Issue #48 control plane.

use std::collections::BTreeSet;
use std::sync::Arc;

use artifact_store::{ImmutableObjectStore, ObjectStoreError, PresignedUpload, VerifiedObject};
use async_trait::async_trait;
use contracts::authoring::{AgentTrackKind, CandidateDecision};
use contracts::http::{
    CandidateDecisionRequest, CreateProblemPackageUploadRequest, IdempotencyKey,
    ProblemPackageUploadFile,
};
use contracts::supply_chain::{
    EnvironmentTemplateRelease, ImageArtifact, ImagePolicyEvaluation, SigstoreEvidence,
    VulnerabilitySummary,
};
use contracts::{
    ActorId, ApprovalId, ArtifactId, ArtifactRef, BuildRequestId, CandidateId, CourseId,
    ImageArtifactId, PolicyId, ReleaseId, Revision, Sha256Digest, UtcTimestamp,
};
use control_service::{ControlConfig, ControlError, ControlService};
use sqlx::{Row, postgres::PgPoolOptions};
use testcontainers::{ImageExt, runners::AsyncRunner};
use testcontainers_modules::postgres::Postgres;
use uuid::Uuid;

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
    let control = format!(
        "CREATE SCHEMA control; SET search_path TO control;\n{}\n{}",
        include_str!("../../../migrations/control/0001_initial.sql"),
        include_str!("../../../migrations/control/0002_control_plane.sql")
    );
    sqlx::raw_sql(&control).execute(&pool).await?;
    let agent = format!(
        "CREATE SCHEMA agent; SET search_path TO agent;\n{}\n{}\n{}",
        include_str!("../../../migrations/agent/0001_initial.sql"),
        include_str!("../../../migrations/agent/0002_track_leases.sql"),
        include_str!("../../../migrations/agent/0003_control_dispatch.sql")
    );
    sqlx::raw_sql(&agent).execute(&pool).await?;

    let upload_id = Uuid::now_v7();
    let course_id = Uuid::now_v7();
    let invalid = sqlx::query(
        "INSERT INTO control.problem_package_upload_sessions \
         (upload_id,course_id,revision,state,retention_policy_revision,expires_at) \
         VALUES ($1,$2,1,'completing',1,now()+interval '1 hour')",
    )
    .bind(upload_id)
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
    let request = upload_request()?;
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
    let mut declared_files = request.files.clone();
    declared_files.sort_by(|left, right| left.path.cmp(&right.path));
    let manifest = Sha256Digest::of_canonical(&declared_files)?;
    let complete_key = IdempotencyKey::parse("issue-48-complete-upload")?;
    let package = service
        .complete_upload(
            course,
            session.id,
            manifest,
            session.revision,
            &complete_key,
            now,
        )
        .await?;
    package.validate()?;
    let mut replays = Vec::new();
    for _ in 0..10 {
        let service = service.clone();
        let key = complete_key.clone();
        replays.push(tokio::spawn(async move {
            service
                .complete_upload(course, session.id, manifest, session.revision, &key, now)
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
            &request,
            &IdempotencyKey::parse("issue-48-recovery-create")?,
            now,
        )
        .await?;
    let recovery_key = IdempotencyKey::parse("issue-48-recovery-complete")?;
    let recovery_request_hash = Sha256Digest::of_canonical(&serde_json::json!({
        "courseId": recovery_course,
        "uploadId": recovery_session.id,
        "manifestSha256": manifest,
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
            manifest,
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
    let failing_session = failing_service
        .create_upload(
            contracts::CourseId::new(),
            &request,
            &IdempotencyKey::parse("issue-48-failing-create")?,
            now,
        )
        .await?;
    assert!(
        failing_service
            .complete_upload(
                failing_session.course_id,
                failing_session.id,
                manifest,
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
    let migrations = format!(
        "CREATE SCHEMA control; SET search_path TO control;\n{}\n{}",
        include_str!("../../../migrations/control/0001_initial.sql"),
        include_str!("../../../migrations/control/0002_control_plane.sql")
    );
    sqlx::raw_sql(&migrations).execute(&pool).await?;
    let config = control_config()?;
    let evaluation_schema = config.evaluation_schema_sha256;
    let service = ControlService::new(
        pool.clone(),
        Arc::new(FixtureObjects { fail_second: false }),
        config,
    )?;
    let course_id = CourseId::new();
    let policy_id = PolicyId::new();
    let candidate_id = CandidateId::new();
    let candidate_sha256 = Sha256Digest::of_bytes(b"evaluation-candidate");
    sqlx::query(
        "INSERT INTO control.course_llm_policies \
         (policy_id,course_id,revision,contract_sha256,contract,activated_at) \
         VALUES ($1,$2,1,$3,'{}'::jsonb,now())",
    )
    .bind(policy_id.as_uuid())
    .bind(course_id.as_uuid())
    .bind(Sha256Digest::of_bytes(b"policy").to_string())
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO control.candidates \
         (candidate_id,candidate_kind,course_id,revision,state,content_sha256,contract, \
          policy_revision,schema_sha256,projected_event_id) \
         VALUES ($1,'evaluation',$2,1,'validated',$3,'{}'::jsonb,1,$4,$5)",
    )
    .bind(candidate_id.as_uuid())
    .bind(course_id.as_uuid())
    .bind(candidate_sha256.to_string())
    .bind(evaluation_schema.to_string())
    .bind(Uuid::now_v7())
    .execute(&pool)
    .await?;
    let request = CandidateDecisionRequest {
        candidate_revision: Revision::new(1)?,
        candidate_sha256,
        policy_revision: Revision::new(1)?,
        schema_sha256: evaluation_schema,
        trust_revision: Revision::new(1)?,
        decision: CandidateDecision::Approved,
        reason: "reviewed evaluation candidate".to_owned(),
    };
    let result = service
        .decide_candidate(
            course_id,
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

    let release = release_fixture(course_id)?;
    sqlx::query(
        "INSERT INTO control.environment_template_releases \
         (release_id,course_id,version,environment_candidate_id,candidate_revision, \
          spec_sha256,image_artifact_id,contract,published_at) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)",
    )
    .bind(release.id.as_uuid())
    .bind(course_id.as_uuid())
    .bind(i64::try_from(release.version)?)
    .bind(release.candidate_id.as_uuid())
    .bind(i64::try_from(release.candidate_revision.get())?)
    .bind(release.environment_spec_sha256.to_string())
    .bind(release.image_policy_evaluation.artifact_id.as_uuid())
    .bind(serde_json::to_value(&release)?)
    .bind(release.published_at.get())
    .execute(&pool)
    .await?;
    let withdrawal = service
        .withdraw_release(
            course_id,
            release.id,
            release.version,
            ActorId::new(),
            "SECURITY_REVOKED",
            &IdempotencyKey::parse("withdraw-release")?,
            "2026-07-16T09:00:00.000Z".parse()?,
            "trace-withdraw-release",
        )
        .await?;
    let view = service.release(course_id, release.id).await?;
    assert_eq!(view.release, release);
    assert_eq!(view.withdrawal, Some(withdrawal.clone()));
    let listed = service.releases(course_id, 0, 10).await?;
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].withdrawal, Some(withdrawal));
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            "SELECT subject FROM control.outbox_events WHERE aggregate_id=$1",
        )
        .bind(release.id.as_uuid())
        .fetch_one(&pool)
        .await?,
        contracts::events::subjects::ENVIRONMENT_TEMPLATE_RELEASE_WITHDRAWN
    );
    Ok(())
}

fn release_fixture(
    course_id: CourseId,
) -> Result<EnvironmentTemplateRelease, Box<dyn std::error::Error>> {
    let published_at = "2026-07-16T08:00:00.000Z".parse::<UtcTimestamp>()?;
    let valid_until = "2026-07-16T10:00:00.000Z".parse::<UtcTimestamp>()?;
    let artifact_sha256 = Sha256Digest::of_bytes(b"container-image");
    let trust_bundle_sha256 = Sha256Digest::of_bytes(b"trust-bundle");
    let candidate_id = CandidateId::new();
    let candidate_sha256 = Sha256Digest::of_bytes(b"environment-spec");
    let artifact_id = ImageArtifactId::new();
    let artifact_ref = |media_type: &str| ArtifactRef {
        artifact_id: ArtifactId::new(),
        store_binding: "artifact-store-v1".to_owned(),
        object_version: "version-1".to_owned(),
        sha256: Sha256Digest::of_bytes(media_type.as_bytes()),
        size_bytes: 1,
        media_type: media_type.to_owned(),
    };
    let signature = SigstoreEvidence {
        trust_bundle_sha256,
        fulcio_issuer: "https://issuer.invalid".to_owned(),
        certificate_subject: "spiffe://labweaver/image-builder".to_owned(),
        certificate_sha256: Sha256Digest::of_bytes(b"certificate"),
        signature_sha256: Sha256Digest::of_bytes(b"signature"),
        rekor_log_id: "rekor-v1".to_owned(),
        rekor_log_index: 1,
        rekor_inclusion_proof_sha256: Sha256Digest::of_bytes(b"rekor-proof"),
        ct_log_id: "ct-v1".to_owned(),
        sct_sha256: Sha256Digest::of_bytes(b"sct"),
        verified_at: published_at,
    };
    Ok(EnvironmentTemplateRelease {
        id: ReleaseId::new(),
        course_id,
        version: 1,
        candidate_id,
        candidate_revision: Revision::new(1)?,
        environment_spec_sha256: candidate_sha256,
        runtime_kind: contracts::authoring::RuntimeKind::Container,
        approval: contracts::authoring::CandidateApproval {
            id: ApprovalId::new(),
            candidate_id,
            candidate_revision: Revision::new(1)?,
            candidate_sha256,
            policy_revision: Revision::new(1)?,
            schema_sha256: Sha256Digest::of_bytes(b"environment"),
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
            immutable_tag: "release-1".to_owned(),
            digest: format!("sha256:{artifact_sha256}"),
            sbom: artifact_ref("application/spdx+json"),
            provenance: artifact_ref("application/vnd.in-toto+json"),
            signature,
        },
        image_policy_evaluation: ImagePolicyEvaluation {
            artifact_id,
            artifact_sha256,
            policy_id: PolicyId::new(),
            policy_revision: Revision::new(1)?,
            scanner_name: "trivy".to_owned(),
            scanner_version: "1.0.0".to_owned(),
            scanner_database_sha256: Sha256Digest::of_bytes(b"scanner-db"),
            vulnerabilities: VulnerabilitySummary {
                unknown: 0,
                low: 0,
                medium: 0,
                high: 1,
                critical: 0,
            },
            trust_bundle_sha256,
            expected_fulcio_issuer: "https://issuer.invalid".to_owned(),
            expected_certificate_subject: "spiffe://labweaver/image-builder".to_owned(),
            require_rekor_inclusion: true,
            require_ct_sct: true,
            evaluated_at: published_at,
            max_evidence_age_milliseconds: 7_200_000,
            valid_until,
            passed: true,
        },
        published_by: ActorId::new(),
        published_at,
    })
}

fn upload_request() -> Result<CreateProblemPackageUploadRequest, Box<dyn std::error::Error>> {
    Ok(CreateProblemPackageUploadRequest {
        files: vec![
            ProblemPackageUploadFile {
                path: "statement.md".to_owned(),
                size_bytes: 9,
                sha256: Sha256Digest::of_bytes(b"statement"),
                media_type: "text/markdown".to_owned(),
            },
            ProblemPackageUploadFile {
                path: "starter/main.rs".to_owned(),
                size_bytes: 7,
                sha256: Sha256Digest::of_bytes(b"starter"),
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
        trust_bundle_sha256: Sha256Digest::of_bytes(b"trust-bundle"),
        fulcio_issuer: "https://issuer.invalid".to_owned(),
        certificate_subject: "spiffe://labweaver/image-builder".to_owned(),
        environment_schema_sha256: Sha256Digest::of_bytes(b"environment"),
        evaluation_schema_sha256: Sha256Digest::of_bytes(b"evaluation"),
    })
}

struct FixtureObjects {
    fail_second: bool,
}

#[async_trait]
impl ImmutableObjectStore for FixtureObjects {
    async fn presign_upload(
        &self,
        key: &str,
        _: u64,
        _: Sha256Digest,
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
        key: &str,
        version: &str,
        size: u64,
        sha256: Sha256Digest,
        media_type: &str,
    ) -> Result<VerifiedObject, ObjectStoreError> {
        Ok(verified(key, version, size, sha256, media_type))
    }

    async fn freeze_current(
        &self,
        key: &str,
        size: u64,
        sha256: Sha256Digest,
        media_type: &str,
    ) -> Result<VerifiedObject, ObjectStoreError> {
        if self.fail_second && key.ends_with("00001") {
            return Err(ObjectStoreError::ObjectIdentityMismatch);
        }
        Ok(verified(key, "version-1", size, sha256, media_type))
    }

    async fn delete_orphan(&self, _: &str, _: &str) -> Result<(), ObjectStoreError> {
        Ok(())
    }
}

fn verified(
    _: &str,
    version: &str,
    size: u64,
    sha256: Sha256Digest,
    media_type: &str,
) -> VerifiedObject {
    VerifiedObject {
        reference: ArtifactRef {
            artifact_id: ArtifactId::new(),
            store_binding: "fixture-v1".to_owned(),
            object_version: version.to_owned(),
            sha256,
            size_bytes: size,
            media_type: media_type.to_owned(),
        },
        bytes: Vec::new(),
    }
}
