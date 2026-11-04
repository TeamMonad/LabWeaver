use super::{FreezeCoordinator, KubernetesServer, SubmissionFreezeCommand};
use crate::{PgEvaluationControlStore, FreezeWorkerDispatch, FreezeWorkerProfile};
use contracts::{FrozenSubmissionId, submission::SubmissionManifest};
use serde_json::Value;
use sqlx::postgres::PgPoolOptions;
use std::{error::Error, sync::atomic::Ordering};

#[allow(clippy::too_many_lines)]
pub(super) async fn assert_boundaries(
    coordinator: &mut FreezeCoordinator,
    provisioner: &sqlx::PgPool,
    template: &SubmissionFreezeCommand,
    manifest: &SubmissionManifest,
    kubernetes: &KubernetesServer,
) -> Result<(), Box<dyn Error>> {
    let course = template.course_id.ok_or("course required")?;
    let binding = coordinator.binding(template, None).await?;
    let release = binding.environment.release_id;
    sqlx::query("INSERT INTO control.course_experiments(experiment_id,course_id,release_id,policy,revision,contract) VALUES($1,$2,$3,'individual',1,'{}')")
        .bind(uuid::Uuid::now_v7()).bind(course.as_uuid()).bind(release.as_uuid()).execute(provisioner).await?;
    sqlx::query("INSERT INTO control.course_calendars(course_id,revision,open_at,due_at,time_zone,late_policy,late_until,contract) VALUES($1,1,date_trunc('milliseconds',clock_timestamp()-interval '1 hour'),date_trunc('milliseconds',clock_timestamp()+interval '1 hour'),'Asia/Shanghai','hard_close',NULL,'{}')")
        .bind(course.as_uuid()).execute(provisioner).await?;
    let runtime = PgPoolOptions::new().max_connections(4).connect_with(
        (*provisioner.connect_options()).clone().username("lw_evaluation_runtime").password("labweaver-test-only-password")
    ).await?;
    coordinator.evaluation = PgEvaluationControlStore::new(runtime);
    for (label, open_hours, due_hours, policy, late_hours, expected) in [
        ("before_open", 1, 2, "hard_close", None, Some("LW_COURSE_EXPERIMENT_NOT_OPEN")),
        ("open", -1, 1, "hard_close", None, None),
        ("late", -2, -1, "allow_with_mark", Some(1), None),
        ("after_late_until", -3, -2, "allow_with_mark", Some(-1), Some("LW_COURSE_EXPERIMENT_CLOSED")),
        ("hard_close", -2, -1, "hard_close", None, Some("LW_COURSE_EXPERIMENT_CLOSED")),
    ] {
        sqlx::query("UPDATE control.course_calendars SET open_at=date_trunc('milliseconds',clock_timestamp()+make_interval(hours=>$2)),due_at=date_trunc('milliseconds',clock_timestamp()+make_interval(hours=>$3)),late_policy=$4,late_until=CASE WHEN $5::integer IS NULL THEN NULL ELSE date_trunc('milliseconds',clock_timestamp()+make_interval(hours=>$5)) END WHERE course_id=$1")
            .bind(course.as_uuid()).bind(open_hours).bind(due_hours).bind(policy).bind(late_hours).execute(provisioner).await?;
        let mut command = template.clone();
        command.frozen_submission_id = FrozenSubmissionId::new();
        command.operation_id = contracts::OperationId::new();
        command.manifest = manifest.clone();
        command.idempotency_key = format!("calendar-{label}");
        command.worker_dispatch = FreezeWorkerDispatch::Bound {
            namespace: "student".to_owned(), profile: FreezeWorkerProfile::Managed,
        };
        let before: i64 = sqlx::query_scalar("SELECT count(*) FROM evaluation.frozen_submissions").fetch_one(provisioner).await?;
        let writes = kubernetes.patch_count.load(Ordering::Acquire);
        let name = super::super::job_name(&command);
        let result = coordinator.create_resources(&command, "student", &name).await;
        if let Some(diagnostic) = expected {
            assert_eq!(result.err().map(|error| error.to_string()).as_deref(), Some(diagnostic), "{label}: freeze calendar must reject submission");
            let after: i64 = sqlx::query_scalar("SELECT count(*) FROM evaluation.frozen_submissions").fetch_one(provisioner).await?;
            assert_eq!(after, before, "{label}: rejected freeze must not create FrozenSubmission");
            assert_eq!(kubernetes.patch_count.load(Ordering::Acquire), writes, "{label}: rejected freeze creates no Kubernetes resource");
        } else {
            result?;
            assert!(kubernetes.patch_count.load(Ordering::Acquire) > writes, "{label}: accepted submission dispatches freeze worker");
        }
    }
    Ok(())
}
