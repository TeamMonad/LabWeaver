//! Real Control and Resource approval bindings with synthetic executor observations.
use super::{apply_domain_migrations, verified_teacher_fixture};
use contracts::authoring::EnvironmentCandidate;
use contracts::http::IdempotencyKey;
use contracts::platform_configuration::{
    ApplyPlatformConfigurationRequest, LinuxCapability, PlatformConfigurationApplyStage as Stage,
    PlatformConfigurationDesired, PlatformConfigurationStatus,
    RegisterPlatformConfigurationBundleRequest, ReportPlatformConfigurationApplyRequest,
    UpdatePlatformConfigurationRequest,
};
use contracts::resource::{CapacityClaim, CapacityClaimState, ResourceApproval};
use contracts::resource::{
    ResourceRequest, ResourceRequestState, ResourceTarget, WorkloadResources,
};
use contracts::runtime_authorization::{RuntimeAdmissionBasis, RuntimeAuthorizationError};
use contracts::supply_chain::EnvironmentTemplateRelease;
use contracts::{
    ActorId, AuthenticatedActor, CourseId, EnvironmentId, PlatformRole, ResourceRequestId,
    Revision, UtcTimestamp,
};
use contracts::{CapacityClaimId, LeaseId, ResourceApprovalId};
use control_service::{ControlError, ControlService, platform_configuration as configuration};
use persistence_sqlx::{Domain, Sha256Digest};
use resource_service::runtime_authorization::{RuntimeBindingError, resolve_binding};
use resource_service::{
    ApprovalPolicy,
    store::{PendingAllocation, PgResourceStore},
};
use sqlx::PgPool;
use std::collections::BTreeSet;
type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

#[path = "environment_funded_handoff.rs"]
mod funded_handoff;
#[path = "runtime_authorization_lease.rs"]
mod lease_cases;

#[derive(Clone, Copy)]
pub(super) enum ExecutorFailure {
    Lease,
    Funding,
    ResourceUnavailable,
    Security(ContainerSecurityTampering),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ContainerSecurityTampering {
    HostNetwork,
    HostPid,
    HostIpc,
    HostPath,
    ExtraContainer,
    ServiceAccount,
    ServiceAccountToken,
    PrivilegeEscalation,
    NamespacePsa,
    MissingNetworkPolicy,
    RehashedMissingNetworkPolicy,
    MissingResourceQuota,
    MissingLimitRange,
    EphemeralContainers,
    InitPrivilegeEscalation,
}

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
    candidate: &EnvironmentCandidate,
    release: &EnvironmentTemplateRelease,
    now: UtcTimestamp,
    executor_failure: Option<ExecutorFailure>,
) -> TestResult {
    let work = candidate.spec.class == contracts::authoring::EnvironmentClass::Work;
    if !work {
        sqlx::query("CREATE ROLE lw_control_runtime NOLOGIN")
            .execute(pool)
            .await?;
        apply_domain_migrations(pool, Domain::Access).await?;
    }
    apply_domain_migrations(pool, Domain::Resource).await?;
    let student = if work {
        release.published_by
    } else {
        ActorId::new()
    };
    sqlx::query("INSERT INTO access.actors(actor_id,issuer,subject_sha256,username,display_name) VALUES($1,'https://synthetic.invalid',$2,'payer','payer')").bind(student.as_uuid()).bind(Sha256Digest::of_bytes(student.as_uuid().as_bytes()).to_string()).execute(pool).await?;
    let course_id = release.course_id.ok_or("course required")?;
    if !work {
        sqlx::query("INSERT INTO control.course_memberships(course_id,actor_id,role,object_role,state,revision) VALUES($1,$2,'student','participant','active',1)").bind(course_id.as_uuid()).bind(student.as_uuid()).execute(pool).await?;
    }
    let payer_project_id = if work {
        release.project_id
    } else {
        let payer = control
            .create_project(
                student,
                PlatformRole::Student,
                &contracts::CreateProjectRequest {
                    name: "runtime-paying-project".into(),
                    description: None,
                    course_id: None,
                },
                &IdempotencyKey::parse("runtime-payer")?,
                now,
            )
            .await?;
        assert_ne!(payer.id, release.project_id);
        payer.id
    };
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
        project_id: payer_project_id,
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
    let mut platform = PlatformConfigurationDesired::default();
    if !candidate.spec.security.capabilities.is_empty() {
        platform.runtime_policy.allowed_capabilities = candidate.spec.security.capabilities.clone();
        platform.runtime_policy.preauthorizations.push(
            contracts::platform_configuration::RuntimePreauthorization {
                scope: contracts::platform_configuration::RuntimePreauthorizationScope::Project {
                    project_id: payer_project_id,
                },
                capabilities: candidate.spec.security.capabilities.clone(),
                privileged: false,
            },
        );
    }
    let first = configuration::update(
        pool,
        ActorId::new(),
        &UpdatePlatformConfigurationRequest {
            expected_revision: 0,
            configuration: platform,
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
    let mut unsupported = PlatformConfigurationDesired::default();
    unsupported.runtime_policy.allowed_capabilities = BTreeSet::from([LinuxCapability::SysAdmin]);
    let Err(error) = configuration::update(
        pool,
        ActorId::new(),
        &UpdatePlatformConfigurationRequest {
            expected_revision: first.revision,
            configuration: unsupported,
        },
        &IdempotencyKey::parse("runtime-unsupported-draft")?,
        now,
    )
    .await
    else {
        return Err("Control accepted unsupported runtime capabilities".into());
    };
    assert!(matches!(
        error,
        configuration::ConfigurationError::Contract(
            contracts::platform_configuration::PlatformConfigurationContractError::PolicyUnsupported {
                field: "runtimePolicy.allowedCapabilities"
            }
        )
    ));
    verified(pool, first, now, "runtime-baseline").await?;
    let binding = resolve_binding(pool, &request).await?;
    assert_eq!(
        binding.basis,
        if candidate.spec.security.capabilities.is_empty() {
            RuntimeAdmissionBasis::RestrictedBaseline
        } else {
            RuntimeAdmissionBasis::ProjectPreauthorization
        }
    );
    assert_eq!(
        binding.permissions.capabilities,
        candidate.spec.security.capabilities
    );
    assert!(!binding.permissions.privileged);
    let store = PgResourceStore::new(pool.clone())
        .with_environment_credit_owner("synthetic-environment".into());
    for (index, unit) in [
        contracts::resource::ResourceBillingUnit::CpuMillicoreSecond,
        contracts::resource::ResourceBillingUnit::MemoryByteSecond,
        contracts::resource::ResourceBillingUnit::StorageByteSecond,
    ]
    .into_iter()
    .enumerate()
    {
        store
            .create_rate(
                &format!("runtime-funded-rate-{index}"),
                &contracts::http::CreateResourceRateRequest {
                    builder_binding: None,
                    model_binding: None,
                    max_preauthorization: None,
                    unit,
                    gpu_mode: None,
                    gpu_class: None,
                    unit_quantity: if index == 0 { 1000 } else { 1 },
                    unit_price: contracts::resource::Money {
                        amount: contracts::resource::FixedDecimal::parse(if index == 0 {
                            "0.010000"
                        } else {
                            "0.000000"
                        })?,
                        currency: "CREDITS".into(),
                    },
                    effective_from: UtcTimestamp::from_utc(now.get() - time::Duration::hours(1))?,
                    effective_until: None,
                },
            )
            .await?;
    }
    store
        .create("runtime-create", &request, "runtime-create")
        .await?;
    let resource_approval = ResourceApproval {
        runtime_authorization: Some(binding.clone()),
        id: ResourceApprovalId::new(),
        request_id: request.id,
        request_revision: request.revision,
        approver_id: ActorId::new(),
        provider_binding: match &candidate.spec.runtime {
            contracts::authoring::EnvironmentRuntimeSpec::Container {
                provider_binding, ..
            }
            | contracts::authoring::EnvironmentRuntimeSpec::VirtualMachine {
                provider_binding,
                ..
            } => provider_binding.clone(),
        },
        approved_resources: resources.clone(),
        approved_duration_seconds: 600,
        reason: "approve immutable configuration".to_owned(),
        valid_until: UtcTimestamp::from_utc(now.get() + time::Duration::hours(1))?,
        created_at: now,
    };
    let allocation = PendingAllocation {
        claim: CapacityClaim {
            updated_at: Some(now),
            id: CapacityClaimId::new(),
            request_id: request.id,
            approval_id: resource_approval.id,
            provider_binding: resource_approval.provider_binding.clone(),
            workload_resources: resources.clone(),
            quota_resources: resources,
            gpu_allocation: None,
            state: CapacityClaimState::Reserved,
            revision: Revision::new(1)?,
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
    let _allocated = Box::pin(lease_cases::assert_original_lease_chain(
        pool,
        &store,
        &resource_approval,
        &allocation,
        executor_failure,
    ))
    .await?;
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

pub(super) async fn assert_manual_work_binding(
    pool: &PgPool,
    release: &EnvironmentTemplateRelease,
    owner: ActorId,
    now: UtcTimestamp,
) -> TestResult {
    sqlx::query("CREATE ROLE lw_control_runtime NOLOGIN")
        .execute(pool)
        .await?;
    apply_domain_migrations(pool, Domain::Access).await?;
    apply_domain_migrations(pool, Domain::Resource).await?;
    sqlx::query("INSERT INTO access.actors(actor_id,issuer,subject_sha256,username,display_name) VALUES($1,'https://synthetic.invalid',$2,'manual-owner','manual-owner')")
        .bind(owner.as_uuid()).bind(Sha256Digest::of_bytes(owner.as_uuid().as_bytes()).to_string()).execute(pool).await?;
    let status = configuration::update(
        pool,
        owner,
        &UpdatePlatformConfigurationRequest {
            expected_revision: 0,
            configuration: PlatformConfigurationDesired::default(),
        },
        &IdempotencyKey::parse("manual-runtime-policy")?,
        now,
    )
    .await?;
    verified(pool, status, now, "manual-runtime-policy").await?;
    let spec = &release
        .manual_approval
        .as_ref()
        .ok_or("manual approval required")?
        .spec;
    let request = ResourceRequest {
        id: ResourceRequestId::new(),
        generation: 1,
        request_key: "manual-work-resource".to_owned(),
        requester_id: owner,
        project_id: release.project_id,
        course_id: None,
        target: ResourceTarget::Environment {
            environment_id: EnvironmentId::new(),
            release_id: release.id,
            release_version: release.version,
        },
        requested_resources: WorkloadResources {
            cpu_millicores: spec.resources.cpu_millicores,
            memory_bytes: spec.resources.memory_bytes,
            storage_bytes: spec.resources.storage_bytes,
            gpu: None,
        },
        requested_duration_seconds: 600,
        state: ResourceRequestState::Reviewing,
        revision: Revision::new(1)?,
        created_at: now,
        updated_at: now,
        diagnostic_code: None,
    };
    if !spec.security.capabilities.is_empty() {
        assert_capability_admission(pool, &request, owner, now).await?;
    }
    let binding = resolve_binding(pool, &request).await?;
    assert_eq!(binding.configuration.release_id, release.id);
    assert_eq!(binding.configuration.release_version, release.version);
    assert_eq!(binding.configuration.authoring_approval_id, None);
    assert_eq!(
        binding.configuration.configuration_sha256,
        Sha256Digest::of_canonical(spec)?.to_string()
    );
    assert_eq!(
        binding.basis,
        if spec.security.capabilities.is_empty() {
            RuntimeAdmissionBasis::RestrictedBaseline
        } else {
            RuntimeAdmissionBasis::ProjectPreauthorization
        }
    );
    let mut foreign = request.clone();
    foreign.project_id = contracts::ProjectId::new();
    assert!(matches!(
        resolve_binding(pool, &foreign).await,
        Err(RuntimeBindingError::ScopeDenied)
    ));
    let mut no_membership = request;
    no_membership.requester_id = ActorId::new();
    assert!(matches!(
        resolve_binding(pool, &no_membership).await,
        Err(RuntimeBindingError::ScopeDenied)
    ));
    Ok(())
}

async fn assert_capability_admission(
    pool: &PgPool,
    request: &ResourceRequest,
    owner: ActorId,
    now: UtcTimestamp,
) -> TestResult {
    use contracts::platform_configuration::{
        RuntimePreauthorization, RuntimePreauthorizationScope,
    };
    assert!(matches!(
        resolve_binding(pool, request).await,
        Err(RuntimeBindingError::Authorization(
            RuntimeAuthorizationError::OutsidePlatformRange
        ))
    ));
    let mut configuration = PlatformConfigurationDesired::default();
    configuration.runtime_policy.default_container_uid = 1000;
    configuration
        .runtime_policy
        .allowed_capabilities
        .insert(LinuxCapability::NetAdmin);
    for granted in [false, true] {
        if granted {
            configuration
                .runtime_policy
                .preauthorizations
                .push(RuntimePreauthorization {
                    scope: RuntimePreauthorizationScope::Project {
                        project_id: request.project_id,
                    },
                    capabilities: BTreeSet::from([LinuxCapability::NetAdmin]),
                    privileged: false,
                });
        }
        let state = configuration::get(pool).await?;
        let next = configuration::update(
            pool,
            owner,
            &UpdatePlatformConfigurationRequest {
                expected_revision: state.revision,
                configuration: configuration.clone(),
            },
            &IdempotencyKey::parse(&format!("capability-policy-{granted}"))?,
            now,
        )
        .await?;
        verified(pool, next, now, &format!("capability-policy-{granted}")).await?;
        if !granted {
            assert!(matches!(
                resolve_binding(pool, request).await,
                Err(RuntimeBindingError::Authorization(
                    RuntimeAuthorizationError::ApprovalRequired
                ))
            ));
        }
    }
    let binding = resolve_binding(pool, request).await?;
    assert_eq!(
        binding.permissions.capabilities,
        BTreeSet::from([LinuxCapability::NetAdmin])
    );
    assert!(!binding.permissions.privileged);
    let side_effects: (i64, i64, i64) = sqlx::query_as("SELECT (SELECT count(*) FROM resource.resource_approvals),(SELECT count(*) FROM resource.capacity_claims),(SELECT count(*) FROM resource.credit_holds)")
        .fetch_one(pool).await?;
    assert_eq!(
        side_effects,
        (0, 0, 0),
        "read-only binding resolution cannot allocate, approve or reserve funds"
    );
    Ok(())
}
