//! Real Resource admission and approval fences against the persisted course calendar.
use super::{
    super::{PublicationCase, authoring_publication_fixture},
    TestResult,
};
use contracts::resource::{ResourceApproval, ResourceRequest};
use resource_service::{
    ApprovalPolicy,
    runtime_authorization::{RuntimeBindingError, resolve_binding},
    store::{PendingAllocation, PgResourceStore, ResourceStoreError},
};
use sqlx::{PgPool, postgres::PgPoolOptions};

#[tokio::test]
async fn course_calendar_runtime_start_boundaries_preserve_lease_usage_and_charges() -> TestResult {
    Box::pin(authoring_publication_fixture(
        PublicationCase::Publication,
        None,
    ))
    .await
}

#[allow(clippy::too_many_lines)]
pub(super) async fn assert_calendar_boundaries(
    pool: &PgPool,
    request: &ResourceRequest,
    approval: &ResourceApproval,
    allocation: &PendingAllocation,
) -> TestResult {
    let course = request.course_id.ok_or("course required")?;
    let (release, _) = request
        .target
        .release()
        .ok_or("environment release required")?;
    sqlx::raw_sql("DO $$ BEGIN IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname='lw_resource_runtime') THEN CREATE ROLE lw_resource_runtime; END IF; END $$; GRANT USAGE ON SCHEMA control,access,resource TO lw_resource_runtime; GRANT SELECT ON ALL TABLES IN SCHEMA control,access,resource TO lw_resource_runtime; GRANT INSERT,UPDATE ON ALL TABLES IN SCHEMA resource TO lw_resource_runtime;")
        .execute(pool).await?;
    let runtime = PgPoolOptions::new()
        .max_connections(2)
        .after_connect(|connection, _| {
            Box::pin(async move {
                sqlx::query("SET ROLE lw_resource_runtime")
                    .execute(connection)
                    .await?;
                Ok(())
            })
        })
        .connect_with((*pool.connect_options()).clone())
        .await?;
    sqlx::query("INSERT INTO control.course_experiments(experiment_id,course_id,release_id,policy,revision,contract) VALUES($1,$2,$3,'individual',1,'{}')")
        .bind(uuid::Uuid::now_v7()).bind(course.as_uuid()).bind(release.as_uuid()).execute(pool).await?;
    sqlx::query("INSERT INTO control.course_calendars(course_id,revision,open_at,due_at,time_zone,late_policy,late_until,contract) VALUES($1,1,clock_timestamp()-interval '1 hour',clock_timestamp()+interval '1 hour','Asia/Shanghai','hard_close',NULL,'{}')")
        .bind(course.as_uuid()).execute(pool).await?;
    let store = PgResourceStore::new(runtime.clone());
    for (label, open_hours, due_hours, policy, late_hours, expected) in [
        (
            "before_open",
            1,
            2,
            "hard_close",
            None,
            Some("LW_COURSE_EXPERIMENT_NOT_OPEN"),
        ),
        ("open", -1, 1, "hard_close", None, None),
        ("late", -2, -1, "allow_with_mark", Some(1), None),
        (
            "after_late_until",
            -3,
            -2,
            "allow_with_mark",
            Some(-1),
            Some("LW_COURSE_EXPERIMENT_CLOSED"),
        ),
        (
            "hard_close",
            -2,
            -1,
            "hard_close",
            None,
            Some("LW_COURSE_EXPERIMENT_CLOSED"),
        ),
    ] {
        sqlx::query("UPDATE control.course_calendars SET open_at=clock_timestamp()+make_interval(hours=>$2),due_at=clock_timestamp()+make_interval(hours=>$3),late_policy=$4,late_until=CASE WHEN $5::integer IS NULL THEN NULL ELSE clock_timestamp()+make_interval(hours=>$5) END WHERE course_id=$1")
            .bind(course.as_uuid()).bind(open_hours).bind(due_hours).bind(policy).bind(late_hours).execute(pool).await?;
        let before = side_effects(pool).await?;
        let resolved = resolve_binding(&runtime, request).await;
        if let Some(diagnostic) = expected {
            assert_eq!(
                resolved.err().map(|error| error.to_string()).as_deref(),
                Some(diagnostic),
                "{label}: calendar must reject runtime start"
            );
            let rejected = store
                .approve(
                    &format!("calendar-{label}"),
                    request.id,
                    approval,
                    allocation,
                    ApprovalPolicy {
                        min_duration_seconds: 60,
                        max_duration_seconds: 3600,
                    },
                    "calendar-runtime",
                )
                .await;
            assert!(
                matches!(
                    &rejected,
                    Err(ResourceStoreError::RuntimeBinding(
                        RuntimeBindingError::CourseExperimentNotOpen
                            | RuntimeBindingError::CourseExperimentClosed
                    ))
                ),
                "{label}: cached runtime authorization must not bypass the approval calendar fence: {rejected:?}"
            );
            assert_eq!(
                side_effects(pool).await?,
                before,
                "{label}: rejected start must not create leases, usage, charges, approvals, claims, or credit holds"
            );
        } else {
            let binding = resolved?;
            assert_eq!(
                binding.request_id, request.id,
                "{label}: in-window start is authorized"
            );
        }
    }
    sqlx::query("DELETE FROM control.course_calendars WHERE course_id=$1")
        .bind(course.as_uuid())
        .execute(pool)
        .await?;
    sqlx::query("DELETE FROM control.course_experiments WHERE course_id=$1")
        .bind(course.as_uuid())
        .execute(pool)
        .await?;
    runtime.close().await;
    Ok(())
}

async fn side_effects(pool: &PgPool) -> TestResult<(i64, i64, i64, i64, i64, i64)> {
    Ok(sqlx::query_as("SELECT (SELECT count(*) FROM resource.resource_leases),(SELECT count(*) FROM resource.resource_usage_records),(SELECT count(*) FROM resource.resource_charges),(SELECT count(*) FROM resource.resource_approvals),(SELECT count(*) FROM resource.capacity_claims),(SELECT count(*) FROM resource.credit_holds)")
        .fetch_one(pool).await?)
}
