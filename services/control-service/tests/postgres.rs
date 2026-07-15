//! Real `PostgreSQL` migration and concurrency evidence for the Issue #48 control plane.

use std::collections::BTreeSet;
use std::sync::Arc;

use artifact_store::{ImmutableObjectStore, ObjectStoreError, PresignedUpload, VerifiedObject};
use async_trait::async_trait;
use contracts::http::{
    CreateProblemPackageUploadRequest, IdempotencyKey, ProblemPackageUploadFile,
};
use contracts::{ArtifactId, ArtifactRef, PolicyId, Revision, Sha256Digest, UtcTimestamp};
use control_service::{ControlConfig, ControlService};
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
