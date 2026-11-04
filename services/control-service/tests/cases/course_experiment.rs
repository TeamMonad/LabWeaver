use contracts::course_experiments::{
    CreateCourseExperimentRequest, CreateExperimentGroupRequest, ExperimentPolicy,
};
use contracts::http::IdempotencyKey;
use contracts::{
    ActorId, AuthenticatedActor, CourseId, PlatformRole, ProjectId, ReleaseId, UtcTimestamp,
};
use control_service::ControlService;
use persistence_sqlx::Sha256Digest;
use std::sync::Arc;
use uuid::Uuid;
#[allow(dead_code)]
#[allow(clippy::duplicate_mod)]
#[path = "../support/production_roles.rs"]
mod production_roles;
type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

async fn actor(
    pool: &sqlx::PgPool,
    roles: Vec<PlatformRole>,
    name: &str,
) -> TestResult<AuthenticatedActor> {
    let id = ActorId::new();
    sqlx::query("INSERT INTO access.actors(actor_id,issuer,subject_sha256,username,display_name) VALUES($1,'https://synthetic.invalid',$2,$3,$3)")
        .bind(id.as_uuid()).bind(Sha256Digest::of_bytes(id.as_uuid().as_bytes()).to_string()).bind(name).execute(pool).await?;
    let now = UtcTimestamp::from_utc(
        sqlx::query_scalar("SELECT date_trunc('milliseconds',clock_timestamp())")
            .fetch_one(pool)
            .await?,
    )?;
    Ok(AuthenticatedActor {
        actor_id: id,
        roles,
        expires_at: UtcTimestamp::from_utc(now.get() + time::Duration::hours(1))?,
    })
}

async fn service() -> TestResult<(
    production_roles::ProductionRoles,
    ControlService,
    CourseId,
    AuthenticatedActor,
    AuthenticatedActor,
    AuthenticatedActor,
)> {
    let roles = production_roles::ProductionRoles::start().await?;
    let teacher = actor(
        &roles.provisioner,
        vec![PlatformRole::Teacher],
        "experiment-teacher",
    )
    .await?;
    let ta = actor(
        &roles.provisioner,
        vec![PlatformRole::Teacher],
        "experiment-ta",
    )
    .await?;
    let student = actor(
        &roles.provisioner,
        vec![PlatformRole::Student],
        "experiment-student",
    )
    .await?;
    let course = CourseId::new();
    sqlx::query("INSERT INTO control.course_memberships(course_id,actor_id,role,object_role,state,revision) VALUES($1,$2,'teacher','instructor','active',1),($1,$3,'teacher','teaching_assistant','active',1),($1,$4,'student','participant','active',1)")
        .bind(course.as_uuid()).bind(teacher.actor_id.as_uuid()).bind(ta.actor_id.as_uuid()).bind(student.actor_id.as_uuid()).execute(&roles.provisioner).await?;
    let service = ControlService::new(
        roles.control.clone(),
        Arc::new(super::FixtureObjects { fail_second: false }),
        super::control_config()?,
    )?;
    Ok((roles, service, course, teacher, ta, student))
}

async fn course_release(pool: &sqlx::PgPool, course: CourseId) -> TestResult<ReleaseId> {
    let release = ReleaseId::new();
    sqlx::query("INSERT INTO control.environment_template_releases(release_id,project_id,course_id,version,environment_candidate_id,candidate_revision,spec_sha256,contract,published_at) VALUES($1,$2,$3,1,$4,1,$5,'{}'::jsonb,date_trunc('milliseconds',clock_timestamp()))")
        .bind(release.as_uuid()).bind(Uuid::now_v7()).bind(course.as_uuid()).bind(Uuid::now_v7())
        .bind(Sha256Digest::of_bytes(release.as_uuid().as_bytes()).to_string())
        .execute(pool).await?;
    Ok(release)
}

async fn project_role(
    pool: &sqlx::PgPool,
    project: ProjectId,
    actor: &AuthenticatedActor,
    object_role: &str,
) -> TestResult {
    sqlx::query("INSERT INTO control.project_memberships(project_id,actor_id,role,object_role,state,revision) VALUES($1,$2,'student',$3,'active',1) ON CONFLICT (project_id,actor_id) DO UPDATE SET object_role=EXCLUDED.object_role,revision=control.project_memberships.revision+1")
        .bind(project.as_uuid()).bind(actor.actor_id.as_uuid()).bind(object_role).execute(pool).await?;
    Ok(())
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn experiment_create_permissions_and_group_policy_are_enforced() -> TestResult {
    let (roles, service, course, teacher, ta, student) = service().await?;
    let request = CreateCourseExperimentRequest {
        release_id: course_release(&roles.provisioner, course).await?,
        policy: ExperimentPolicy::Group,
    };
    let denied_student = service
        .create_course_experiment(
            &student,
            course,
            &request,
            &IdempotencyKey::parse("student-create")?,
        )
        .await;
    assert!(matches!(
        denied_student,
        Err(control_service::ControlError::CourseGovernanceDenied)
    ));
    let denied_ta = service
        .create_course_experiment(&ta, course, &request, &IdempotencyKey::parse("ta-create")?)
        .await;
    assert!(matches!(
        denied_ta,
        Err(control_service::ControlError::CourseGovernanceDenied)
    ));
    let foreign_release = CreateCourseExperimentRequest {
        release_id: course_release(&roles.provisioner, CourseId::new()).await?,
        policy: ExperimentPolicy::Group,
    };
    assert!(
        matches!(
            service
                .create_course_experiment(
                    &teacher,
                    course,
                    &foreign_release,
                    &IdempotencyKey::parse("foreign-release")?,
                )
                .await,
            Err(control_service::ControlError::NotFound)
        ),
        "another course's release cannot back this course's experiment"
    );
    let experiment = service
        .create_course_experiment(
            &teacher,
            course,
            &request,
            &IdempotencyKey::parse("teacher-create")?,
        )
        .await?;
    let member_project = ProjectId::new();
    let group_request = CreateExperimentGroupRequest {
        payer_project_id: member_project,
        payer_actor_id: student.actor_id,
        member_actor_ids: vec![student.actor_id],
        payer_consent: true,
    };
    assert!(
        matches!(
            service
                .create_experiment_group(
                    &student,
                    course,
                    experiment.id,
                    &group_request,
                    &IdempotencyKey::parse("unrelated-project")?,
                )
                .await,
            Err(control_service::ControlError::CourseGovernanceDenied)
        ),
        "a payer cannot bind a project they do not belong to"
    );
    project_role(&roles.provisioner, member_project, &student, "viewer").await?;
    assert!(
        matches!(
            service
                .create_experiment_group(
                    &student,
                    course,
                    experiment.id,
                    &group_request,
                    &IdempotencyKey::parse("viewer-project")?,
                )
                .await,
            Err(control_service::ControlError::CourseGovernanceDenied)
        ),
        "a project viewer cannot spend its Credits"
    );
    project_role(&roles.provisioner, member_project, &student, "editor").await?;
    assert!(
        matches!(
            service
                .create_experiment_group(
                    &teacher,
                    course,
                    experiment.id,
                    &group_request,
                    &IdempotencyKey::parse("teacher-binds-student")?,
                )
                .await,
            Err(control_service::ControlError::CourseGovernanceDenied)
        ),
        "staff cannot consent to spend a student's project Credits"
    );
    let group = service
        .create_experiment_group(
            &student,
            course,
            experiment.id,
            &group_request,
            &IdempotencyKey::parse("group-create")?,
        )
        .await?;
    assert_eq!(group.group.payer_project_id, member_project);
    assert_eq!(group.group.payer_actor_id, student.actor_id);
    assert!(group.group.payer_consent);
    let other_student = actor(
        &roles.provisioner,
        vec![PlatformRole::Student],
        "experiment-other-student",
    )
    .await?;
    sqlx::query("INSERT INTO control.course_memberships(course_id,actor_id,role,object_role,state,revision) VALUES($1,$2,'student','participant','active',1)").bind(course.as_uuid()).bind(other_student.actor_id.as_uuid()).execute(&roles.provisioner).await?;
    let other_project = ProjectId::new();
    project_role(&roles.provisioner, other_project, &other_student, "owner").await?;
    let second_request = CreateExperimentGroupRequest {
        payer_project_id: other_project,
        payer_actor_id: other_student.actor_id,
        member_actor_ids: vec![other_student.actor_id],
        payer_consent: true,
    };
    service
        .create_experiment_group(
            &other_student,
            course,
            experiment.id,
            &second_request,
            &IdempotencyKey::parse("group-create-2")?,
        )
        .await?;
    assert_eq!(
        service
            .experiment_groups(&student, course, experiment.id)
            .await?
            .len(),
        1
    );
    assert_eq!(
        service
            .experiment_groups(&other_student, course, experiment.id)
            .await?
            .len(),
        1
    );
    assert_eq!(
        service
            .experiment_groups(&teacher, course, experiment.id)
            .await?
            .len(),
        2
    );
    let replay = service
        .create_experiment_group(
            &student,
            course,
            experiment.id,
            &group_request,
            &IdempotencyKey::parse("group-create")?,
        )
        .await?;
    assert_eq!(replay.group.id, group.group.id);
    let conflict = CreateExperimentGroupRequest {
        payer_project_id: ProjectId::new(),
        ..group_request.clone()
    };
    assert!(matches!(
        service
            .create_experiment_group(
                &student,
                course,
                experiment.id,
                &conflict,
                &IdempotencyKey::parse("group-create")?
            )
            .await,
        Err(control_service::ControlError::IdempotencyConflict)
    ));
    let missing_consent = CreateExperimentGroupRequest {
        payer_consent: false,
        ..group_request.clone()
    };
    assert!(matches!(
        service
            .create_experiment_group(
                &student,
                course,
                experiment.id,
                &missing_consent,
                &IdempotencyKey::parse("missing-consent")?
            )
            .await,
        Err(control_service::ControlError::ContractInvalid)
    ));
    let payer_not_member = CreateExperimentGroupRequest {
        member_actor_ids: vec![other_student.actor_id],
        ..group_request
    };
    assert!(matches!(
        service
            .create_experiment_group(
                &student,
                course,
                experiment.id,
                &payer_not_member,
                &IdempotencyKey::parse("payer-not-member")?
            )
            .await,
        Err(control_service::ControlError::ContractInvalid)
    ));
    Ok(())
}

#[tokio::test]
async fn individual_experiment_rejects_groups_and_personal_scope_isolated() -> TestResult {
    let (roles, service, course, teacher, _ta, student) = service().await?;
    let request = CreateCourseExperimentRequest {
        release_id: course_release(&roles.provisioner, course).await?,
        policy: ExperimentPolicy::Individual,
    };
    let experiment = service
        .create_course_experiment(
            &teacher,
            course,
            &request,
            &IdempotencyKey::parse("individual-create")?,
        )
        .await?;
    let payer_project = ProjectId::new();
    project_role(&roles.provisioner, payer_project, &student, "owner").await?;
    let group_request = CreateExperimentGroupRequest {
        payer_project_id: payer_project,
        payer_actor_id: student.actor_id,
        member_actor_ids: vec![student.actor_id],
        payer_consent: true,
    };
    assert!(matches!(
        service
            .create_experiment_group(
                &student,
                course,
                experiment.id,
                &group_request,
                &IdempotencyKey::parse("individual-group")?
            )
            .await,
        Err(control_service::ControlError::ContractInvalid)
    ));
    assert!(matches!(
        service
            .course_experiment(&student, CourseId::new(), experiment.id)
            .await,
        Err(control_service::ControlError::NotFound
            | control_service::ControlError::CourseGovernanceDenied)
    ));
    Ok(())
}

#[tokio::test]
async fn payer_binding_is_immutable_while_membership_changes() -> TestResult {
    let roles = production_roles::ProductionRoles::start().await?;
    let pool = roles.control.clone();
    let course_id = Uuid::now_v7();
    let experiment_id = Uuid::now_v7();
    let group_id = Uuid::now_v7();
    let payer_project = Uuid::now_v7();
    let replacement_project = Uuid::now_v7();
    let payer_actor = Uuid::now_v7();
    sqlx::query("INSERT INTO control.course_experiments(experiment_id,course_id,release_id,policy,revision,contract) VALUES($1,$2,$3,'group',1,'{}')").bind(experiment_id).bind(course_id).bind(Uuid::now_v7()).execute(&pool).await?;
    sqlx::query("INSERT INTO control.experiment_groups(group_id,experiment_id,payer_project_id,payer_actor_id,payer_consent,revision,contract) VALUES($1,$2,$3,$4,true,1,'{}')").bind(group_id).bind(experiment_id).bind(payer_project).bind(payer_actor).execute(&pool).await?;
    sqlx::query("INSERT INTO control.experiment_group_members(group_id,actor_id,revision,state) VALUES($1,$2,1,'active')").bind(group_id).bind(payer_actor).execute(&pool).await?;
    sqlx::query("UPDATE control.experiment_group_members SET state='revoked',revision=2 WHERE group_id=$1 AND actor_id=$2").bind(group_id).bind(payer_actor).execute(&pool).await?;
    let Err(error) = sqlx::query("UPDATE control.experiment_groups SET payer_project_id=$2,payer_actor_id=$3 WHERE group_id=$1").bind(group_id).bind(replacement_project).bind(Uuid::now_v7()).execute(&pool).await else {
        return Err("payer replacement was accepted".into());
    };
    assert!(
        error
            .to_string()
            .contains("LW_COURSE_EXPERIMENT_PAYER_IMMUTABLE")
    );
    let Err(error) = sqlx::query("DELETE FROM control.experiment_groups WHERE group_id=$1")
        .bind(group_id)
        .execute(&pool)
        .await
    else {
        return Err("group deletion (delete-and-recreate payer swap) was accepted".into());
    };
    assert!(
        error
            .to_string()
            .contains("LW_COURSE_EXPERIMENT_PAYER_IMMUTABLE")
    );
    let (project, actor, consent, contract): (Uuid, Uuid, bool, serde_json::Value) = sqlx::query_as("SELECT payer_project_id,payer_actor_id,payer_consent,contract FROM control.experiment_groups WHERE group_id=$1").bind(group_id).fetch_one(&pool).await?;
    assert_eq!(
        (project, actor, consent),
        (payer_project, payer_actor, true)
    );
    assert_eq!(contract, serde_json::json!({}));
    let membership_state: String = sqlx::query_scalar(
        "SELECT state FROM control.experiment_group_members WHERE group_id=$1 AND actor_id=$2",
    )
    .bind(group_id)
    .bind(payer_actor)
    .fetch_one(&pool)
    .await?;
    assert_eq!(membership_state, "revoked");
    Ok(())
}
