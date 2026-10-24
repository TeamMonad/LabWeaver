//! Immutable release fixtures shared by `PostgreSQL` and authenticated HTTP integration tests.
use contracts::environment::EnvironmentInstance;
use contracts::events::ReleasePublished;
use contracts::{CourseId, EventId};
use persistence_sqlx::Sha256Digest;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

pub(crate) fn projection(instance: &EnvironmentInstance) -> TestResult<ReleasePublished> {
    let image_id = contracts::PlatformImageId::new();
    let digest = format!("sha256:{}", "a".repeat(64));
    let spec = serde_json::json!({
        "apiVersion":"environment.labweaver.io/v1","kind":"EnvironmentSpec","name":"manual-work","class":"work",
        "resources":instance.approved_resources,"network":{"mode":"deny_all"},
        "entries":[{"name":"app","protocol":"http","servicePort":8080}],
        "security":{"userPolicy":"non_root_required","rootFilesystemPolicy":"read_only_required","privilegeEscalationPolicy":"deny","publicExposurePolicy":"deny","securityProfileBinding":"restricted-v1"},
        "runtime":{"kind":"container","provider_binding":instance.provider_binding,"service_port":8080},
        "retention":{"policyId":contracts::PolicyId::new(),"policyRevision":1,"class":"run_evidence","retainUntil":instance.eligibility_expires_at,"disposition":"delete"}
    });
    Ok(serde_json::from_value(
        serde_json::json!({"environmentSpec":spec,"release":{
            "id":instance.release_id,"projectId":instance.project_id,"courseId":instance.course_id,"version":instance.release_version,
            "candidateId":null,"candidateRevision":null,"agentRunId":null,"approval":null,"runtimeKind":"container",
            "manualApproval":{"templateFamilyId":"manual-upgrade-202","description":"upgrade fixture","spec":spec,"reason":"human reviewed",
                "approvedBy":instance.owner_id,"approvedAt":instance.operation.accepted_at,"trustRevision":1,
                "platformImage":{"catalogId":image_id,"kind":"container","binding":"manual-work","sourceReference":"harbor.internal/labweaver-system/manual:202",
                    "resolvedDigest":digest,"mediaType":"application/vnd.oci.image.manifest.v1+json","sizeBytes":1,"status":"active","trustRevision":1,"repinGeneration":1,
                    "pinnedAt":instance.operation.accepted_at,"updatedAt":instance.operation.accepted_at}},
            "artifact":{"kind":"catalog_container","id":image_id,"catalog_id":image_id,"repin_generation":1,"trust_revision":1,"repository":"harbor.internal/labweaver-system/manual","digest":digest},
            "publishedBy":instance.owner_id,"publishedAt":instance.operation.accepted_at
        }}),
    )?)
}

pub(crate) async fn insert_projection(
    pool: &sqlx::PgPool,
    projection: &ReleasePublished,
) -> TestResult {
    projection.validate()?;
    sqlx::query("INSERT INTO environment.release_projections(release_id,release_version,project_id,course_id,provider_binding,projection_sha256,contract,projected_event_id) VALUES($1,$2,$3,$4,$5,$6,$7,$8)")
        .bind(projection.release.id.as_uuid()).bind(i64::try_from(projection.release.version)?).bind(projection.release.project_id.as_uuid())
        .bind(projection.release.course_id.map(CourseId::as_uuid)).bind("container-primary-v1")
        .bind(Sha256Digest::of_canonical(projection)?.to_string()).bind(serde_json::to_value(projection)?).bind(EventId::new().as_uuid()).execute(pool).await?;
    Ok(())
}
