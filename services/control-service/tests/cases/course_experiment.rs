use uuid::Uuid;

#[path = "../support/production_roles.rs"]
mod production_roles;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

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
    let member_actor = Uuid::now_v7();
    sqlx::query("INSERT INTO control.course_experiments(experiment_id,course_id,release_id,policy,revision,contract) VALUES($1,$2,$3,'group',1,'{}')")
        .bind(experiment_id).bind(course_id).bind(Uuid::now_v7()).execute(&pool).await?;
    sqlx::query("INSERT INTO control.experiment_groups(group_id,experiment_id,payer_project_id,payer_actor_id,payer_consent,revision,contract) VALUES($1,$2,$3,$4,true,1,'{}')")
        .bind(group_id).bind(experiment_id).bind(payer_project).bind(payer_actor).execute(&pool).await?;
    sqlx::query("INSERT INTO control.experiment_group_members(group_id,actor_id,revision,state) VALUES($1,$2,1,'active')")
        .bind(group_id).bind(member_actor).execute(&pool).await?;
    sqlx::query("UPDATE control.experiment_group_members SET state='revoked',revision=2 WHERE group_id=$1 AND actor_id=$2")
        .bind(group_id).bind(member_actor).execute(&pool).await?;
    let error = sqlx::query("UPDATE control.experiment_groups SET payer_project_id=$2 WHERE group_id=$1")
        .bind(group_id).bind(replacement_project).execute(&pool).await.expect_err("payer replacement must be rejected");
    assert!(error.to_string().contains("LW_COURSE_EXPERIMENT_PAYER_IMMUTABLE"));
    let (project, actor, state): (Uuid, Uuid, bool) = sqlx::query_as("SELECT payer_project_id,payer_actor_id,payer_consent FROM control.experiment_groups WHERE group_id=$1")
        .bind(group_id).fetch_one(&pool).await?;
    assert_eq!(project, payer_project);
    assert_eq!(actor, payer_actor);
    assert!(state);
    let membership_state: String = sqlx::query_scalar("SELECT state FROM control.experiment_group_members WHERE group_id=$1 AND actor_id=$2")
        .bind(group_id).bind(member_actor).fetch_one(&pool).await?;
    assert_eq!(membership_state, "revoked");
    Ok(())
}

