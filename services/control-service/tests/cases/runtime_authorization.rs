//! Real Control and Resource approval bindings with synthetic executor observations.
use super::{apply_domain_migrations, verified_teacher_fixture};
use contracts::authoring::{AuthoringApproval, EnvironmentCandidate};
use contracts::http::IdempotencyKey;
use contracts::platform_configuration::{
    ApplyPlatformConfigurationRequest, LinuxCapability, PlatformConfigurationApplyStage as Stage,
    PlatformConfigurationDesired, PlatformConfigurationStatus,
    RegisterPlatformConfigurationBundleRequest, ReportPlatformConfigurationApplyRequest,
    RuntimePreauthorization, RuntimePreauthorizationScope, UpdatePlatformConfigurationRequest,
};
use contracts::resource::{
    CapacityClaim, CapacityClaimState, ResourceApproval, ResourceRequest, ResourceRequestState,
    ResourceTarget, WorkloadResources,
};
use contracts::runtime_authorization::{
    RuntimeAdmissionBasis, RuntimeAuthorizationError, RuntimePolicyUse, check_policy_use,
};
use contracts::supply_chain::EnvironmentTemplateRelease;
use contracts::{
    ActorId, AuthenticatedActor, CapacityClaimId, CourseId, EnvironmentId, LeaseId, PlatformRole,
    ResourceApprovalId, ResourceRequestId, Revision, UtcTimestamp,
};
use control_service::{ControlError, ControlService, platform_configuration as configuration};
use persistence_sqlx::{Domain, Sha256Digest};
use resource_service::{
    ApprovalPolicy,
    runtime_authorization::{
        RuntimeBindingError, current_environment_authorization, resolve_binding,
    },
    store::{PendingAllocation, PgResourceStore},
};
use sqlx::PgPool;
use std::collections::BTreeSet;
type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

#[path = "runtime_authorization_lease.rs"]
mod lease_cases;

async fn progress(
    pool: &PgPool,
    state: PlatformConfigurationStatus,
    apply_stage: Stage,
    now: UtcTimestamp,
    key: &str,
) -> TestResult<PlatformConfigurationStatus> {
    let apply = state.latest_apply.as_ref().ok_or("no apply")?;
    let desired = configuration::get_version(pool, apply.desired_revision).await?;
    let loaded = synthetic_loaded(&desired, &apply.encrypted_bundle_sha256)?;
    Ok(configuration::report(
        pool,
        Some(&loaded),
        "synthetic-runtime-policy-executor",
        apply.id,
        &ReportPlatformConfigurationApplyRequest {
            expected_revision: apply.revision,
            stage: apply_stage,
            failure_reason: None,
            observed_configuration_sha256: (apply_stage == Stage::Succeeded)
                .then_some(desired.configuration_sha256),
            observed_encrypted_bundle_sha256: (apply_stage == Stage::Succeeded)
                .then_some(apply.encrypted_bundle_sha256.clone()),
            control_replica_observations: None,
        },
        &IdempotencyKey::parse(key)?,
        now,
    )
    .await?)
}

fn synthetic_loaded(
    version: &contracts::platform_configuration::PlatformConfigurationVersion,
    encrypted_bundle_sha256: &str,
) -> TestResult<control_service::platform_configuration::LoadedConfiguration> {
    let directory = tempfile::tempdir()?;
    let version_file = directory.path().join("version.json");
    let source_file = directory.path().join("source.json");
    std::fs::write(&version_file, serde_json::to_vec(version)?)?;
    std::fs::write(
        &source_file,
        serde_json::to_vec(&serde_json::json!({
            "apiVersion":"deploy.labweaver.io/loaded-source/v1",
            "desiredRevision":version.revision,
            "configurationSha256":version.configuration_sha256,
            "encryptedBundleSha256":encrypted_bundle_sha256,
            "installation":{"namespace":"synthetic-control", "releaseName":"synthetic"}
        }))?,
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for file in [&version_file, &source_file] {
            std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o600))?;
        }
    }
    Ok(
        control_service::platform_configuration::LoadedConfiguration::load(
            &version_file,
            &source_file,
        )?,
    )
}

fn encrypted_fixture() -> TestResult<Vec<u8>> {
    let temporary = tempfile::tempdir()?;
    let identity = temporary.path().join("synthetic-identity");
    let archive = temporary.path().join("synthetic.age");
    let generated = std::process::Command::new("age-keygen")
        .arg("-o")
        .arg(&identity)
        .output()?;
    if !generated.status.success() {
        return Err("synthetic age key generation failed".into());
    }
    let recipient = std::process::Command::new("age-keygen")
        .arg("-y")
        .arg(&identity)
        .output()?;
    if !recipient.status.success() {
        return Err("synthetic recipient lookup failed".into());
    }
    let plaintext = temporary.path().join("synthetic-plaintext");
    std::fs::write(&plaintext, b"synthetic-only runtime admission settings")?;
    let encrypted = std::process::Command::new("age")
        .arg("-r")
        .arg(String::from_utf8(recipient.stdout)?.trim())
        .arg("-o")
        .arg(&archive)
        .arg(&plaintext)
        .output()?;
    if !encrypted.status.success() {
        return Err("synthetic age encryption failed".into());
    }
    Ok(std::fs::read(archive)?)
}

async fn begin(
    pool: &PgPool,
    state: PlatformConfigurationStatus,
    now: UtcTimestamp,
    key: &str,
) -> TestResult<PlatformConfigurationStatus> {
    let desired = state.desired.as_ref().ok_or("no desired")?;
    let cipher = encrypted_fixture()?;
    let state = configuration::register_bundle(
        pool,
        ActorId::new(),
        desired.revision,
        &RegisterPlatformConfigurationBundleRequest {
            expected_revision: state.revision,
            configuration_sha256: desired.configuration_sha256.clone(),
        },
        configuration::BundleCiphertext {
            digest: Sha256Digest::of_bytes(&cipher),
            bytes: u64::try_from(cipher.len())?,
        },
        &IdempotencyKey::parse(&format!("{key}-registered-bundle"))?,
        now,
    )
    .await?;
    Ok(configuration::request_apply(
        pool,
        ActorId::new(),
        &ApplyPlatformConfigurationRequest {
            expected_revision: state.revision,
            desired_revision: state.desired.as_ref().ok_or("no desired")?.revision,
        },
        &IdempotencyKey::parse(key)?,
        now,
    )
    .await?)
}

async fn verified(
    pool: &PgPool,
    state: PlatformConfigurationStatus,
    now: UtcTimestamp,
    prefix: &str,
) -> TestResult<PlatformConfigurationStatus> {
    let mut state = begin(pool, state, now, &format!("{prefix}-apply")).await?;
    for (index, stage) in [
        Stage::Validating,
        Stage::Applying,
        Stage::Verifying,
        Stage::Succeeded,
    ]
    .into_iter()
    .enumerate()
    {
        state = progress(pool, state, stage, now, &format!("{prefix}-phase-{index}")).await?;
    }
    Ok(state)
}

// Keep the ordered apply/approval scenario together so each observed state is checked.
#[allow(clippy::too_many_lines)]
pub(super) async fn assert_bindings_after_publication(
    pool: &PgPool,
    control: &ControlService,
    approval: &AuthoringApproval,
    candidate: &EnvironmentCandidate,
    release: &EnvironmentTemplateRelease,
    now: UtcTimestamp,
) -> TestResult {
    sqlx::query("CREATE ROLE lw_control_runtime NOLOGIN")
        .execute(pool)
        .await?;
    apply_domain_migrations(pool, Domain::Access).await?;
    apply_domain_migrations(pool, Domain::Resource).await?;
    let student = ActorId::new();
    sqlx::query("INSERT INTO access.actors(actor_id,issuer,subject_sha256,username,display_name) VALUES($1,'https://synthetic.invalid',$2,'payer','payer')").bind(student.as_uuid()).bind(Sha256Digest::of_bytes(student.as_uuid().as_bytes()).to_string()).execute(pool).await?;
    let course_id = release.course_id.ok_or("course required")?;
    sqlx::query("INSERT INTO control.course_memberships(course_id,actor_id,role,object_role,state,revision) VALUES($1,$2,'student','participant','active',1)").bind(course_id.as_uuid()).bind(student.as_uuid()).execute(pool).await?;
    let payer = control
        .create_project(
            student,
            PlatformRole::Student,
            &contracts::CreateProjectRequest {
                name: "runtime-paying-project".to_owned(),
                description: None,
                course_id: None,
            },
            &IdempotencyKey::parse("runtime-payer")?,
            now,
        )
        .await?;
    assert_ne!(payer.id, release.project_id);
    let resources = WorkloadResources {
        cpu_millicores: 1000,
        memory_bytes: 1_073_741_824,
        storage_bytes: 10_737_418_240,
        gpu: None,
    };
    let request = ResourceRequest {
        id: ResourceRequestId::new(),
        generation: 1,
        request_key: "runtime-binding".to_owned(),
        requester_id: student,
        project_id: payer.id,
        course_id: Some(course_id),
        target: ResourceTarget::Environment {
            environment_id: EnvironmentId::new(),
            release_id: release.id,
            release_version: release.version,
        },
        requested_resources: resources.clone(),
        requested_duration_seconds: 600,
        state: ResourceRequestState::Reviewing,
        revision: Revision::new(1)?,
        created_at: now,
        updated_at: now,
        diagnostic_code: None,
    };
    assert!(matches!(
        resolve_binding(pool, &request).await,
        Err(RuntimeBindingError::Authorization(
            RuntimeAuthorizationError::PolicyNotVerified
        ))
    ));
    let first = configuration::update(
        pool,
        ActorId::new(),
        &UpdatePlatformConfigurationRequest {
            expected_revision: 0,
            configuration: PlatformConfigurationDesired::default(),
        },
        &IdempotencyKey::parse("runtime-initial-draft")?,
        now,
    )
    .await?;
    assert!(matches!(
        resolve_binding(pool, &request).await,
        Err(RuntimeBindingError::Authorization(
            RuntimeAuthorizationError::PolicyNotVerified
        ))
    ));
    let mut settings = PlatformConfigurationDesired::default();
    settings.runtime_policy.allowed_capabilities = BTreeSet::from([LinuxCapability::NetAdmin]);
    settings.runtime_policy.preauthorizations = vec![RuntimePreauthorization {
        scope: RuntimePreauthorizationScope::Course { course_id },
        capabilities: BTreeSet::from([LinuxCapability::NetAdmin]),
        privileged: false,
    }];
    let second = configuration::update(
        pool,
        ActorId::new(),
        &UpdatePlatformConfigurationRequest {
            expected_revision: first.revision,
            configuration: settings.clone(),
        },
        &IdempotencyKey::parse("runtime-course-draft")?,
        now,
    )
    .await?;
    assert!(matches!(
        resolve_binding(pool, &request).await,
        Err(RuntimeBindingError::Authorization(
            RuntimeAuthorizationError::PolicyNotVerified
        ))
    ));
    let applied = verified(pool, second, now, "runtime-first").await?;
    let binding = resolve_binding(pool, &request).await?;
    assert_eq!(binding.basis, RuntimeAdmissionBasis::CoursePreauthorization);
    assert_eq!(
        binding.permissions.scope,
        RuntimePreauthorizationScope::Course { course_id }
    );
    assert!(
        binding.permissions.capabilities.is_empty(),
        "current EnvironmentSpec has no extra capability declaration"
    );
    assert_eq!(
        binding.configuration.authoring_approval_id,
        Some(approval.id)
    );
    assert_eq!(
        binding.configuration.configuration_sha256,
        Sha256Digest::of_canonical(&candidate.spec)?.to_string()
    );
    assert_ne!(
        binding.platform_configuration_revision, candidate.policy_revision,
        "LLM revision cannot substitute for applied runtime policy"
    );
    let store = PgResourceStore::new(pool.clone());
    store
        .create("runtime-create", &request, "runtime-create")
        .await?;
    let resource_approval = ResourceApproval {
        runtime_authorization: Some(binding.clone()),
        id: ResourceApprovalId::new(),
        request_id: request.id,
        request_revision: request.revision,
        approver_id: ActorId::new(),
        provider_binding: "kubernetes-standard".to_owned(),
        approved_resources: resources.clone(),
        approved_duration_seconds: 600,
        reason: "approve immutable configuration".to_owned(),
        valid_until: UtcTimestamp::from_utc(now.get() + time::Duration::hours(1))?,
        created_at: now,
    };
    let allocation = PendingAllocation {
        claim: CapacityClaim {
            id: CapacityClaimId::new(),
            request_id: request.id,
            approval_id: resource_approval.id,
            provider_binding: resource_approval.provider_binding.clone(),
            workload_resources: resources.clone(),
            quota_resources: resources,
            gpu_allocation: None,
            state: CapacityClaimState::Reserved,
            revision: Revision::new(1)?,
            updated_at: Some(now),
        },
        lease_id: LeaseId::new(),
    };
    store
        .approve(
            "runtime-approve",
            request.id,
            &resource_approval,
            &allocation,
            ApprovalPolicy {
                min_duration_seconds: 60,
                max_duration_seconds: 3600,
            },
            "runtime-approve",
        )
        .await?;
    let stored: serde_json::Value =
        sqlx::query_scalar("SELECT contract FROM resource.resource_approvals WHERE approval_id=$1")
            .bind(resource_approval.id.as_uuid())
            .fetch_one(pool)
            .await?;
    assert_eq!(
        serde_json::from_value::<ResourceApproval>(stored)?.runtime_authorization,
        Some(binding.clone())
    );
    let allocated = store.load(request.id).await?;
    assert_eq!(
        resource_service::runtime_authorization::current_authorization(pool, &allocated).await?,
        binding
    );
    let allocated = Box::pin(lease_cases::assert_original_lease_chain(
        pool,
        &store,
        &resource_approval,
        &allocation,
    ))
    .await?;
    let mut obsolete_request = allocated.clone();
    obsolete_request.generation += 1;
    assert!(matches!(
        resource_service::runtime_authorization::current_authorization(pool, &obsolete_request)
            .await,
        Err(RuntimeBindingError::Authorization(
            RuntimeAuthorizationError::ConfigurationMismatch
        ))
    ));
    sqlx::query("UPDATE control.course_memberships SET state='revoked',revision=2 WHERE course_id=$1 AND actor_id=$2")
        .bind(course_id.as_uuid()).bind(student.as_uuid()).execute(pool).await?;
    assert!(matches!(
        resolve_binding(pool, &request).await,
        Err(RuntimeBindingError::ScopeDenied)
    ));
    assert!(matches!(
        resource_service::runtime_authorization::current_authorization(pool, &allocated).await,
        Err(RuntimeBindingError::ScopeDenied)
    ));
    assert!(matches!(
        current_environment_authorization(pool, request.id, Some(allocation.lease_id)).await,
        Err(RuntimeBindingError::ScopeDenied)
    ));
    sqlx::query("UPDATE control.course_memberships SET state='active',revision=3 WHERE course_id=$1 AND actor_id=$2")
        .bind(course_id.as_uuid()).bind(student.as_uuid()).execute(pool).await?;
    settings.runtime_policy.preauthorizations.insert(
        0,
        RuntimePreauthorization {
            scope: RuntimePreauthorizationScope::Project {
                project_id: payer.id,
            },
            capabilities: BTreeSet::new(),
            privileged: false,
        },
    );
    let changed = configuration::update(
        pool,
        ActorId::new(),
        &UpdatePlatformConfigurationRequest {
            expected_revision: applied.revision,
            configuration: settings,
        },
        &IdempotencyKey::parse("runtime-next-draft")?,
        now,
    )
    .await?;
    binding.validate_current(&request, &binding.configuration, &changed)?;
    assert_eq!(
        resolve_binding(pool, &request)
            .await?
            .platform_configuration_revision,
        binding.platform_configuration_revision,
        "desired is not effective"
    );
    let newly_applied = verified(pool, changed, now, "runtime-second").await?;
    assert_eq!(
        binding.validate_current(&request, &binding.configuration, &newly_applied),
        Err(RuntimeAuthorizationError::PolicyChanged)
    );
    assert!(matches!(
        resource_service::runtime_authorization::current_authorization(pool, &allocated).await,
        Err(RuntimeBindingError::Authorization(
            RuntimeAuthorizationError::PolicyChanged
        ))
    ));
    assert!(matches!(
        current_environment_authorization(pool, request.id, Some(allocation.lease_id)).await,
        Err(RuntimeBindingError::Authorization(
            RuntimeAuthorizationError::PolicyChanged
        ))
    ));
    let current_binding = resolve_binding(pool, &request).await?;
    assert_eq!(
        current_binding.basis,
        RuntimeAdmissionBasis::ProjectPreauthorization
    );
    assert_ne!(
        current_binding.platform_configuration_revision,
        binding.platform_configuration_revision
    );
    let mut changed_configuration = binding.configuration.clone();
    let mut changed_spec = candidate.spec.clone();
    changed_spec.name.push_str("-changed");
    changed_configuration.configuration_sha256 =
        Sha256Digest::of_canonical(&changed_spec)?.to_string();
    assert_eq!(
        current_binding.validate_current(&request, &changed_configuration, &newly_applied),
        Err(RuntimeAuthorizationError::ConfigurationMismatch)
    );
    let mut unrelated = request.clone();
    unrelated.target = ResourceTarget::Task {
        task_run_id: contracts::TaskRunId::new(),
    };
    assert!(matches!(
        resolve_binding(pool, &unrelated).await,
        Err(RuntimeBindingError::Authorization(
            RuntimeAuthorizationError::TargetNotEnvironment
        ))
    ));
    let mut cross = request.clone();
    cross.project_id = contracts::ProjectId::new();
    assert!(matches!(
        resolve_binding(pool, &cross).await,
        Err(RuntimeBindingError::ScopeDenied)
    ));
    let state = begin(
        pool,
        configuration::get(pool).await?,
        now,
        "runtime-uncertain-apply",
    )
    .await?;
    let state = progress(
        pool,
        state,
        Stage::Validating,
        now,
        "runtime-uncertain-validate",
    )
    .await?;
    let state = progress(
        pool,
        state,
        Stage::Applying,
        now,
        "runtime-uncertain-applying",
    )
    .await?;
    assert!(matches!(
        resolve_binding(pool, &request).await,
        Err(RuntimeBindingError::Authorization(
            RuntimeAuthorizationError::PolicyNotVerified
        ))
    ));
    assert!(check_policy_use(&state, RuntimePolicyUse::StopOrRecovery).is_ok());
    assert!(check_policy_use(&state, RuntimePolicyUse::Admission).is_err());
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM resource.resource_approvals")
            .fetch_one(pool)
            .await?,
        1,
        "policy changes neither create another approval nor stop the existing request"
    );
    assert_eq!(
        store.load(request.id).await?.state,
        ResourceRequestState::Active
    );
    assert!(matches!(
        current_environment_authorization(pool, request.id, Some(allocation.lease_id)).await,
        Err(RuntimeBindingError::Authorization(
            RuntimeAuthorizationError::PolicyNotVerified
        ))
    ));
    let lease = store.load_lease(allocation.lease_id).await?;
    store
        .revoke_lease(
            "runtime-lease-revoke",
            lease.id,
            lease.revision,
            "synthetic revoke".to_owned(),
            resource_approval.approver_id,
            "runtime-lease-revoke",
        )
        .await?;
    assert!(matches!(
        current_environment_authorization(pool, request.id, Some(allocation.lease_id)).await,
        Err(RuntimeBindingError::LeaseNotActive)
    ));
    Ok(())
}

pub(super) async fn assert_assistant_cannot_approve(
    control: &ControlService,
    pool: &PgPool,
    project_id: contracts::ProjectId,
    request: &contracts::http::CompleteAuthoringApprovalRequest,
    now: UtcTimestamp,
) -> TestResult {
    let assistant = ActorId::new();
    let course_id = request.course_id.ok_or("course")?;
    sqlx::query("INSERT INTO control.course_memberships(course_id,actor_id,role,object_role,state,revision) VALUES($1,$2,'student','teaching_assistant','active',1)").bind(course_id.as_uuid()).bind(assistant.as_uuid()).execute(pool).await?;
    for roles in [
        vec![PlatformRole::Teacher],
        vec![
            PlatformRole::Teacher,
            PlatformRole::Student,
            PlatformRole::PlatformAdmin,
        ],
        vec![PlatformRole::PlatformAdmin],
    ] {
        let actor = AuthenticatedActor {
            actor_id: assistant,
            roles,
            expires_at: UtcTimestamp::from_utc(now.get() + time::Duration::hours(1))?,
        };
        assert!(matches!(
            control
                .complete_authoring_approval(
                    project_id,
                    request,
                    &actor,
                    &IdempotencyKey::parse("assistant-no-final-approval")?,
                    now,
                    "assistant-denied",
                    &[]
                )
                .await,
            Err(ControlError::TeacherApprovalDenied)
        ));
    }
    let teacher = verified_teacher_fixture(pool, ActorId::new(), CourseId::new(), now).await?;
    assert!(matches!(
        control
            .complete_authoring_approval(
                project_id,
                request,
                &teacher,
                &IdempotencyKey::parse("wrong-course-no-approval")?,
                now,
                "wrong-course",
                &[]
            )
            .await,
        Err(ControlError::TeacherApprovalDenied)
    ));
    Ok(())
}
