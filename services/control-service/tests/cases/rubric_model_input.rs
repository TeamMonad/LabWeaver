//! Real policy transactions; synthetic publication metadata is the external content boundary.
use super::*;
use contracts::http::{
    MaintainRubricModelInputRuleRequest, ModelInputRubricIdentity,
    RubricModelInputRuleAction as Action, RubricModelInputVisibility as Visibility,
};
type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

#[allow(
    clippy::too_many_lines,
    reason = "one transaction sequence covers approval, revocation and replay"
)]
pub async fn assert_rubric_authority(
    pool: &sqlx::PgPool,
    package: &ProblemPackage,
    release: &EvaluationRelease,
    environment_release: &contracts::supply_chain::EnvironmentTemplateRelease,
    case: PublicationCase,
    now: UtcTimestamp,
) -> TestResult {
    let project = package.project_id;
    let course = package.course_id.ok_or("course missing")?;
    let objects = Arc::new(ForbiddenModelInputObjects(
        std::sync::atomic::AtomicUsize::new(0),
    ));
    let service = ControlService::new(pool.clone(), objects.clone(), control_config()?)?;
    let instructor_id = ActorId::new();
    let teacher = verified_teacher_fixture(pool, instructor_id, course, now).await?;
    sqlx::query("INSERT INTO control.project_memberships(project_id,course_id,actor_id,role,object_role,state,revision) VALUES($1,$2,$3,'teacher','editor','active',1)")
        .bind(project.as_uuid()).bind(course.as_uuid()).bind(instructor_id.as_uuid()).execute(pool).await?;
    let file = package.files.first().ok_or("file missing")?;
    let rubric = ModelInputRubricIdentity {
        package_id: package.id,
        package_revision: package.revision,
        path: file.path.clone(),
        artifact: file.object.clone(),
        sha256: "a".repeat(64),
    };
    // #197 owns the producer of PackageFile.sha256. Seed its exact persisted shape at this
    // dependency boundary, without replacing any authorization decision or object read.
    let mut metadata = serde_json::to_value(package)?;
    metadata["files"][0]["sha256"] = serde_json::json!(rubric.sha256);
    sqlx::query(
        "UPDATE control.problem_packages SET contract=$2,manifest_sha256=$3 WHERE package_id=$1",
    )
    .bind(package.id.as_uuid())
    .bind(&metadata)
    .bind(Sha256Digest::of_canonical(&metadata)?.to_string())
    .execute(pool)
    .await?;
    let mut policy = service.active_project_policy(project).await?;
    let mut request = MaintainRubricModelInputRuleRequest {
        evaluation_release_id: release.id,
        evaluation_release_revision: release.revision,
        rubric: rubric.clone(),
        action: approve(),
    };
    let denied = || ControlError::TeacherApprovalDenied.to_string();
    assert_eq!(
        service
            .current_rubric_permission(
                project,
                course,
                release.id,
                release.revision,
                &rubric,
                &policy
            )
            .await
            .denied("unknown rubric")?,
        denied()
    );
    assert_eq!(
        service
            .maintain_rubric_model_input_rule(
                project,
                &teacher,
                &request,
                policy.revision,
                &IdempotencyKey::parse("rubric-unknown")?,
                now
            )
            .await
            .denied("unknown cannot be approved")?,
        denied()
    );
    request.action = Action::SetVisibility {
        visibility: Visibility::CoursePublic,
    };
    for roles in [
        vec![contracts::PlatformRole::Student],
        vec![contracts::PlatformRole::PlatformAdmin],
    ] {
        let mut actor = teacher.clone();
        actor.roles = roles;
        assert_eq!(
            service
                .maintain_rubric_model_input_rule(
                    project,
                    &actor,
                    &request,
                    policy.revision,
                    &IdempotencyKey::parse("rubric-wrong-role")?,
                    now
                )
                .await
                .denied("platform role is not instructor approval")?,
            denied()
        );
    }
    let mut expired = teacher.clone();
    expired.expires_at = now;
    assert_eq!(
        service
            .maintain_rubric_model_input_rule(
                project,
                &expired,
                &request,
                policy.revision,
                &IdempotencyKey::parse("rubric-expired-actor")?,
                now
            )
            .await
            .denied("expired actor")?,
        denied()
    );
    sqlx::query("UPDATE control.course_memberships SET object_role='teaching_assistant' WHERE course_id=$1 AND actor_id=$2").bind(course.as_uuid()).bind(instructor_id.as_uuid()).execute(pool).await?;
    assert_eq!(
        service
            .maintain_rubric_model_input_rule(
                project,
                &teacher,
                &request,
                policy.revision,
                &IdempotencyKey::parse("rubric-assistant")?,
                now
            )
            .await
            .denied("course assistant cannot declare rubric visibility")?,
        denied()
    );
    sqlx::query("UPDATE control.course_memberships SET object_role='instructor' WHERE course_id=$1 AND actor_id=$2").bind(course.as_uuid()).bind(instructor_id.as_uuid()).execute(pool).await?;
    sqlx::query("UPDATE control.project_memberships SET object_role='viewer' WHERE project_id=$1 AND actor_id=$2").bind(project.as_uuid()).bind(instructor_id.as_uuid()).execute(pool).await?;
    assert_eq!(
        service
            .maintain_rubric_model_input_rule(
                project,
                &teacher,
                &request,
                policy.revision,
                &IdempotencyKey::parse("rubric-browse-only")?,
                now
            )
            .await
            .denied("Browse cannot maintain rules")?,
        denied()
    );
    sqlx::query("UPDATE control.project_memberships SET object_role='editor' WHERE project_id=$1 AND actor_id=$2").bind(project.as_uuid()).bind(instructor_id.as_uuid()).execute(pool).await?;

    request.action = Action::SetVisibility {
        visibility: Visibility::CoursePublic,
    };
    let key = IdempotencyKey::parse("rubric-declare-public")?;
    let declared = service
        .maintain_rubric_model_input_rule(project, &teacher, &request, policy.revision, &key, now)
        .await?;
    assert!(declared.rules[0].approval.is_none());
    assert_eq!(
        service
            .maintain_rubric_model_input_rule(
                project,
                &teacher,
                &request,
                policy.revision,
                &key,
                now
            )
            .await?,
        declared
    );
    assert_eq!(
        service
            .current_rubric_permission(
                project,
                course,
                release.id,
                release.revision,
                &rubric,
                &policy
            )
            .await
            .denied("visibility alone is not model approval")?,
        denied()
    );
    assert_eq!(
        service
            .maintain_rubric_model_input_rule(
                project,
                &teacher,
                &request,
                policy.revision,
                &IdempotencyKey::parse("rubric-stale")?,
                now
            )
            .await
            .denied("stale policy")?,
        ControlError::RevisionConflict.to_string()
    );
    policy = service.active_project_policy(project).await?;
    request.action = approve();
    let mut changed = request.clone();
    changed.rubric.sha256 = "b".repeat(64);
    assert_eq!(
        service
            .maintain_rubric_model_input_rule(
                project,
                &teacher,
                &changed,
                policy.revision,
                &IdempotencyKey::parse("rubric-wrong-hash")?,
                now
            )
            .await
            .denied("wrong exact content")?,
        ControlError::ArtifactMismatch.to_string()
    );
    changed = request.clone();
    changed.rubric.artifact.object_version.push_str("-other");
    assert_eq!(
        service
            .maintain_rubric_model_input_rule(
                project,
                &teacher,
                &changed,
                policy.revision,
                &IdempotencyKey::parse("rubric-wrong-object-version")?,
                now
            )
            .await
            .denied("wrong immutable object")?,
        ControlError::ArtifactMismatch.to_string()
    );
    let approved = service
        .maintain_rubric_model_input_rule(
            project,
            &teacher,
            &request,
            policy.revision,
            &IdempotencyKey::parse("rubric-approve")?,
            now,
        )
        .await?;
    let approval = approved.rules[0]
        .approval
        .as_ref()
        .ok_or("approval missing")?;
    assert_eq!(approval.approved_by, instructor_id);
    assert_eq!(
        approval.approver_course_membership_revision,
        Revision::new(1)?
    );
    let permission = service
        .current_rubric_permission(
            project,
            course,
            release.id,
            release.revision,
            &rubric,
            &policy,
        )
        .await?;
    assert_eq!(permission.policy_revision, approved.policy_revision);
    if case == PublicationCase::FrozenSharedInputs {
        assert_shared_frozen_inputs(
            &service,
            pool,
            release,
            environment_release,
            &teacher,
            &rubric,
            now,
        )
        .await?;
        assert_eq!(objects.0.load(std::sync::atomic::Ordering::SeqCst), 0);
        return Ok(());
    }
    let mut other_model = policy.clone();
    "other-model".clone_into(&mut other_model.binding.model);
    assert_eq!(
        service
            .current_rubric_permission(
                project,
                course,
                release.id,
                release.revision,
                &rubric,
                &other_model
            )
            .await
            .denied("approval is target-specific")?,
        denied()
    );
    assert_eq!(
        service
            .current_rubric_permission(
                project,
                course,
                release.id,
                Revision::new(release.revision.get() + 1)?,
                &rubric,
                &policy
            )
            .await
            .denied("release revision differs")?,
        denied()
    );
    let mut other_rubric = rubric.clone();
    other_rubric.artifact.object_version.push_str("-changed");
    assert_eq!(
        service
            .current_rubric_permission(
                project,
                course,
                release.id,
                release.revision,
                &other_rubric,
                &policy
            )
            .await
            .denied("exact rubric differs")?,
        denied()
    );

    for state in ["revoked", "suspended"] {
        sqlx::query(
            "UPDATE control.course_memberships SET state=$3 WHERE course_id=$1 AND actor_id=$2",
        )
        .bind(course.as_uuid())
        .bind(instructor_id.as_uuid())
        .bind(state)
        .execute(pool)
        .await?;
        assert_eq!(
            service
                .current_rubric_permission(
                    project,
                    course,
                    release.id,
                    release.revision,
                    &rubric,
                    &policy
                )
                .await
                .denied("departed instructor")?,
            denied()
        );
        assert_eq!(
            service
                .maintain_rubric_model_input_rule(
                    project,
                    &teacher,
                    &request,
                    approved.policy_revision,
                    &IdempotencyKey::parse("rubric-approve")?,
                    now
                )
                .await
                .denied("replay still checks current authority")?,
            denied()
        );
    }
    sqlx::query("UPDATE control.course_memberships SET state='active',revision=2 WHERE course_id=$1 AND actor_id=$2").bind(course.as_uuid()).bind(instructor_id.as_uuid()).execute(pool).await?;
    assert_eq!(
        service
            .current_rubric_permission(
                project,
                course,
                release.id,
                release.revision,
                &rubric,
                &policy
            )
            .await
            .denied("rejoining must not revive old approval")?,
        denied()
    );
    let renewed = service
        .maintain_rubric_model_input_rule(
            project,
            &teacher,
            &request,
            approved.policy_revision,
            &IdempotencyKey::parse("rubric-renew")?,
            now,
        )
        .await?;
    assert_eq!(
        renewed.rules[0]
            .approval
            .as_ref()
            .ok_or("renewed approval missing")?
            .approver_course_membership_revision,
        Revision::new(2)?
    );
    sqlx::query("UPDATE control.course_memberships SET expires_at=clock_timestamp()-interval '1 second' WHERE course_id=$1 AND actor_id=$2").bind(course.as_uuid()).bind(instructor_id.as_uuid()).execute(pool).await?;
    assert_eq!(
        service
            .current_rubric_permission(
                project,
                course,
                release.id,
                release.revision,
                &rubric,
                &policy
            )
            .await
            .denied("instructor membership expired")?,
        denied()
    );
    sqlx::query(
        "UPDATE control.course_memberships SET expires_at=NULL WHERE course_id=$1 AND actor_id=$2",
    )
    .bind(course.as_uuid())
    .bind(instructor_id.as_uuid())
    .execute(pool)
    .await?;
    request.action = Action::SetVisibility {
        visibility: Visibility::Hidden,
    };
    let hidden = service
        .maintain_rubric_model_input_rule(
            project,
            &teacher,
            &request,
            renewed.policy_revision,
            &IdempotencyKey::parse("rubric-hide")?,
            now,
        )
        .await?;
    assert!(hidden.rules[0].approval.is_none());
    assert_eq!(
        service
            .current_rubric_permission(
                project,
                course,
                release.id,
                release.revision,
                &rubric,
                &policy
            )
            .await
            .denied("hidden must deny")?,
        denied()
    );
    request.action = approve();
    assert_eq!(
        service
            .maintain_rubric_model_input_rule(
                project,
                &teacher,
                &request,
                hidden.policy_revision,
                &IdempotencyKey::parse("rubric-approve-hidden")?,
                now
            )
            .await
            .denied("hidden cannot be approved")?,
        denied()
    );
    request.action = Action::SetVisibility {
        visibility: Visibility::CoursePublic,
    };
    let public = service
        .maintain_rubric_model_input_rule(
            project,
            &teacher,
            &request,
            hidden.policy_revision,
            &IdempotencyKey::parse("rubric-public-again")?,
            now,
        )
        .await?;
    request.action = approve();
    let public = service
        .maintain_rubric_model_input_rule(
            project,
            &teacher,
            &request,
            public.policy_revision,
            &IdempotencyKey::parse("rubric-approved-again")?,
            now,
        )
        .await?;
    request.action = Action::RevokeModelUse;
    let mut racing_approval = request.clone();
    racing_approval.action = approve();
    let approve_key = IdempotencyKey::parse("rubric-race-approve")?;
    let revoke_key = IdempotencyKey::parse("rubric-race-revoke")?;
    let (left, right) = tokio::join!(
        service.maintain_rubric_model_input_rule(
            project,
            &teacher,
            &racing_approval,
            public.policy_revision,
            &approve_key,
            now
        ),
        service.maintain_rubric_model_input_rule(
            project,
            &teacher,
            &request,
            public.policy_revision,
            &revoke_key,
            now
        ),
    );
    let raced = match (left, right) {
        (Ok(value), Err(ControlError::RevisionConflict))
        | (Err(ControlError::RevisionConflict), Ok(value)) => value,
        other => {
            return Err(format!("exactly one concurrent mutation must succeed: {other:?}").into());
        }
    };
    assert_eq!(
        raced.policy_revision.get(),
        public.policy_revision.get() + 1
    );
    let revoked = service
        .maintain_rubric_model_input_rule(
            project,
            &teacher,
            &request,
            raced.policy_revision,
            &IdempotencyKey::parse("rubric-revoke")?,
            now,
        )
        .await?;
    assert!(revoked.rules[0].approval.is_none());
    assert_eq!(
        service
            .current_rubric_permission(
                project,
                course,
                release.id,
                release.revision,
                &rubric,
                &policy
            )
            .await
            .denied("explicit revocation")?,
        denied()
    );
    let view = service
        .project_rubric_model_input_rules(project, &teacher, now)
        .await?;
    assert_eq!(view, revoked);
    let mut replacement = service.active_project_policy(project).await?;
    let before_revision = replacement.revision;
    replacement.id = PolicyId::new();
    service
        .activate_project_policy(
            project,
            replacement,
            &IdempotencyKey::parse("rubric-policy-replacement")?,
            Some(before_revision),
        )
        .await?;
    let preserved = service
        .project_rubric_model_input_rules(project, &teacher, now)
        .await?;
    assert_eq!(preserved.rules, revoked.rules);
    assert!(preserved.policy_revision > revoked.policy_revision);
    assert_eq!(
        service
            .current_rubric_permission(
                project,
                course,
                release.id,
                release.revision,
                &rubric,
                &policy
            )
            .await
            .denied("generic policy update must preserve revocation")?,
        denied()
    );
    assert_eq!(objects.0.load(std::sync::atomic::Ordering::SeqCst), 0);
    Ok(())
}

fn approve() -> Action {
    Action::ApproveModelUse {
        runtime_binding: "claude-code-test".to_owned(),
        model: "fixture-provider-v1".to_owned(),
    }
}

trait DeniedResult<T> {
    fn denied(self, reason: &str) -> TestResult<String>;
}
impl<T: std::fmt::Debug> DeniedResult<T> for Result<T, ControlError> {
    fn denied(self, reason: &str) -> TestResult<String> {
        match self {
            Err(error) => Ok(error.to_string()),
            Ok(value) => Err(format!("unexpected permission: {reason}: {value:?}").into()),
        }
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "real original session, publication and rubric rules authorize each protected task step"
)]
async fn assert_shared_frozen_inputs(
    service: &ControlService,
    pool: &sqlx::PgPool,
    release: &EvaluationRelease,
    environment_release: &contracts::supply_chain::EnvironmentTemplateRelease,
    teacher: &contracts::AuthenticatedActor,
    rubric: &ModelInputRubricIdentity,
    now: UtcTimestamp,
) -> TestResult {
    use base64::Engine as _;
    use contracts::http::{
        CreateFrozenModelInputAuthorizationRequest, ModelInputAuthorizationContext,
        ModelInputAuthorizationSource,
    };
    sqlx::query("CREATE ROLE lw_control_runtime NOLOGIN")
        .execute(pool)
        .await?;
    apply_domain_migrations(pool, Domain::Access).await?;
    sqlx::query("INSERT INTO access.actors(actor_id,issuer,subject_sha256) VALUES($1,'https://fixture-issuer.invalid',$2)")
        .bind(teacher.actor_id.as_uuid()).bind(Sha256Digest::of_bytes(teacher.actor_id.as_uuid().as_bytes()).to_string()).execute(pool).await?;
    let keys = auth::KeyRing::parse(
        "fixture".to_owned(),
        &format!(
            "fixture:{}",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([7u8; 32])
        ),
    )?;
    let session = auth::create_bff_session(
        pool,
        &keys,
        auth::CreateBffSession {
            actor_id: teacher.actor_id.as_uuid(),
            roles: teacher.roles.clone(),
            authorization_revision: 1,
            expires_at: now.get() + time::Duration::hours(1),
            idle_ttl: time::Duration::minutes(30),
            oidc_sid: None,
            logout_hint: "fixture-logout".to_owned(),
        },
        now.get(),
    )
    .await?;
    let original = ModelInputAuthorizationContext {
        session_sha256: Sha256Digest::of_bytes(session.session_id.to_string().as_bytes())
            .to_string(),
        actor_id: teacher.actor_id,
        project_id: release.project_id,
        course_id: release.course_id,
        operation_id: "freezeSubmission".to_owned(),
        valid_until: now,
    };
    let mut evaluation = release.clone();
    let review_step = |id: &str| serde_json::json!({"role":"advisory","id":id,"runner":{"kind":"llm_review","include":["answer.txt"],"rubric":format!("evaluator://{}",rubric.path),"outputMode":"goal_assessment"},"failurePolicy":"continue_advisory"});
    evaluation.evaluation_spec = serde_json::from_value(serde_json::json!({
        "apiVersion":"evaluation.labweaver.io/v1","kind":"EvaluationSpec","metadata":{"name":"shared-advisory-input","version":"1.0.0"},
        "spec":{"submission":{"collector":{"kind":"workspace_snapshot","include":["answer.txt"],"maxBytes":1024},"llmReadable":["answer.txt"]},
        "steps":[review_step("review-a"),review_step("review-b"),{"role":"score","id":"deterministic","runner":{"kind":"file_assertion","requiredFiles":["answer.txt"]},"checker":{"kind":"exit_code","expected":0},"score":{"max":1},"failurePolicy":"stop"}],
        "aggregation":{"kind":"deterministic_sum","maxScore":1,"gates":[]},"review":{"teacherApprovalRequiredForRelease":true,"forceManualWhen":[]}}
    }))?;
    evaluation.validate()?;
    let manifest: contracts::submission::SubmissionManifest = serde_json::from_value(
        serde_json::json!({
            "apiVersion":"evaluation.labweaver.io/v1","kind":"SubmissionManifest","name":"shared-advisory-input","source":"workspace",
            "include":[{"kind":"exactFile","path":"answer.txt"}],"exclude":[],"required":[],"llmReadable":[{"kind":"exactFile","path":"answer.txt"}],
            "maxTotalBytes":1024,"maxFiles":4,"followSymlinks":false
        }),
    )?;
    let frozen_id = contracts::FrozenSubmissionId::new();
    let build_id = match &environment_release.artifact {
        ImageArtifact::Container {
            build_request_id, ..
        } => Some(*build_request_id),
        ImageArtifact::VirtualMachine { .. } => None,
    };
    let mut facts = serde_json::json!({
        "frozenSubmissionId":frozen_id,"projectId":release.project_id,"courseId":release.course_id,"ownerActorId":teacher.actor_id,
        "manifestRevision":1,"manifestSha256":Sha256Digest::of_canonical(&manifest)?.to_string(),"manifest":manifest,
        "archive":{"artifactId":ArtifactId::new(),"storeBinding":"fixture","objectVersion":"archive-v1","sizeBytes":1,"mediaType":"application/gzip"},
        "contentSha256":Sha256Digest::of_bytes(b"x").to_string(),"files":[{"path":"answer.txt","sha256":Sha256Digest::of_bytes(b"x").to_string()}],
        "environment":{"environmentId":EnvironmentId::new(),"environmentRevision":1,"releaseId":environment_release.id,"releaseVersion":environment_release.version,"runtimeKind":environment_release.runtime_kind,"buildRequestId":build_id},
        "evaluationRelease":{"id":release.id,"revision":release.revision,"state":"active"},"inputAuthorizationContext":original
    });
    for step_id in ["review-a", "review-b"] {
        let task_id = contracts::TaskRunId::new();
        facts["taskRunId"] = serde_json::json!(task_id);
        facts["stepId"] = serde_json::json!(step_id);
        let request = CreateFrozenModelInputAuthorizationRequest {
            input_authorization_context: original.clone(),
            task_run_id: task_id,
            parent_lease_sha256: contracts::http::model_input_parent_lease_sha256(
                uuid::Uuid::now_v7(),
            ),
            frozen_submission_id: frozen_id,
            evaluation_release_id: release.id,
            evaluation_release_revision: release.revision,
            selected_paths: vec!["answer.txt".to_owned()],
        };
        let binding = service
            .bind_frozen_model_input(&request, facts.clone(), &evaluation)
            .await?;
        let ModelInputAuthorizationSource::FrozenSubmission(source) = binding.source else {
            return Err("wrong source".into());
        };
        assert_eq!(source.step_id, step_id);
        assert_eq!(source.task_run_id, task_id);
        assert_eq!(source.rubric, *rubric);
        let mut other_task = request.clone();
        other_task.task_run_id = contracts::TaskRunId::new();
        assert!(
            service
                .bind_frozen_model_input(&other_task, facts.clone(), &evaluation)
                .await
                .is_err()
        );
        let mut unknown_step = facts.clone();
        unknown_step["stepId"] = serde_json::json!("deterministic");
        assert!(
            service
                .bind_frozen_model_input(&request, unknown_step, &evaluation)
                .await
                .is_err()
        );
    }
    Ok(())
}
