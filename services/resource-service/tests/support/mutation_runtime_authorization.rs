//! Real Control publication and loaded Bundle for combined Resource HTTP regressions.
//! Agent/image-catalog and OIDC are synthetic external boundaries; approval is never bypassed.
#![allow(clippy::too_many_lines)]

use contracts::{
    ActorId, AgentRunId, CandidateId, EventId, PlatformRole, PolicyId, ProblemPackageId, Revision,
    UtcTimestamp,
    authoring::{AgentRun, EnvironmentCandidate},
    http::IdempotencyKey,
    platform_configuration::{
        ApplyPlatformConfigurationRequest, PlatformConfigurationApplyStage as Stage,
        PlatformConfigurationDesired, RegisterPlatformConfigurationBundleRequest,
        ReportPlatformConfigurationApplyRequest, UpdatePlatformConfigurationRequest,
    },
    resource::{ResourceRequest, ResourceTarget},
};
use control_service::{ControlService, platform_configuration as configuration};
use persistence_sqlx::{Domain, Sha256Digest};
use serde_json::json;
use sqlx::PgPool;
use std::sync::Arc;

#[allow(dead_code, reason = "reuse the existing non-business Bundle fixtures")]
#[path = "../../../control-service/tests/support/platform_configuration_common.rs"]
mod fixture;
#[path = "../../../control-service/tests/support/mod.rs"]
mod migrations;

pub async fn prepare(
    pool: &PgPool,
    request: &mut ResourceRequest,
    now: UtcTimestamp,
) -> Result<String, Box<dyn std::error::Error>> {
    migrations::apply_domain_migrations(pool, Domain::Control).await?;
    sqlx::query("CREATE ROLE lw_control_runtime NOLOGIN")
        .execute(pool)
        .await?;
    migrations::apply_domain_migrations(pool, Domain::Access).await?;
    sqlx::query("INSERT INTO access.actors(actor_id,issuer,subject_sha256,username,display_name) VALUES($1,'https://synthetic.invalid',$2,'mutation-owner','mutation-owner')")
        .bind(request.requester_id.as_uuid()).bind(Sha256Digest::of_bytes(request.requester_id.as_uuid().as_bytes()).to_string()).execute(pool).await?;

    let config = fixture::config()?;
    let base = config.virtual_machine_bases.clone();
    let control = ControlService::new(pool.clone(), Arc::new(fixture::NoObjects), config)?;
    let project = control
        .create_project(
            request.requester_id,
            PlatformRole::PlatformAdmin,
            &contracts::CreateProjectRequest {
                name: "HTTP mutation project".to_owned(),
                description: None,
                course_id: None,
            },
            &IdempotencyKey::parse("mutation-project")?,
            now,
        )
        .await?;
    request.project_id = project.id;
    request.course_id = None;
    let policy = serde_json::from_value(json!({
        "id":PolicyId::new(),"projectId":project.id,"courseId":null,"revision":1,
        "binding":{"runtimeBinding":"claude-code-test","model":"fixture-provider-v1","claudeCodeVersion":"2.1.207","maxInFlightPerWorker":2},
        "budget":{"maxInputTokens":100_000,"maxOutputTokens":16_000,"maxRequests":8,"maxCostMicrousd":2_000_000,"timeoutMilliseconds":120_000,"maxTransientRetries":2,"maxSchemaRepairs":2},
        "deniedDataClasses":["secret","token","private_key","personally_identifiable_information","unallowlisted_student_submission"],
        "studentContentMode":"manifest_allowlist_only","activatedAt":now
    }))?;
    let policy = control
        .activate_project_policy(
            project.id,
            policy,
            &IdempotencyKey::parse("mutation-policy")?,
            None,
        )
        .await?;
    let candidate: EnvironmentCandidate = serde_json::from_value(json!({
        "id":CandidateId::new(),"runId":AgentRunId::new(),"projectId":project.id,"courseId":null,"revision":1,
        "policyRevision":1,"model":"fixture-provider-v1","createdAt":now,
        "spec":{"apiVersion":"environment.labweaver.io/v1","kind":"EnvironmentSpec","name":"http-mutation-work","class":"work",
            "resources":request.requested_resources,"network":{"mode":"deny_all"},"entries":[{"name":"ssh","protocol":"ssh","servicePort":22}],
            "security":{"publicExposurePolicy":"deny","securityProfileBinding":"synthetic-v1","rootFilesystemPolicy":"mutable_required"},
            "runtime":{"kind":"virtual_machine","provider_binding":base.provider_binding,"base_disk":base.bases[0].base_disk,"storage_class_binding":base.storage_class_binding,"ssh_port":22},
            "retention":{"policyId":PolicyId::new(),"policyRevision":1,"class":"run_evidence","retainUntil":UtcTimestamp::from_utc(now.get()+time::Duration::days(1))?,"disposition":"delete"}}
    }))?;
    let run: AgentRun = serde_json::from_value(json!({
        "id":candidate.run_id,"projectId":project.id,"courseId":null,"packageId":ProblemPackageId::new(),"policyId":policy.id,"policyRevision":1,
        "purpose":{"kind":"authoring","environmentClass":"work"},"state":"succeeded","revision":1,"plan":null,
        "tracks":[{"kind":"environment","candidateId":candidate.id,"attempts":[{"number":1,"state":"succeeded","checkpoint":null,
            "usage":{"inputTokens":1,"outputTokens":1,"requests":1,"costMicrousd":1},"usageObserved":true,"diagnosticCode":null}]}]
    }))?;
    control
        .project_candidates(
            EventId::new(),
            &run,
            Some(&candidate),
            None,
            None,
            None,
            None,
        )
        .await?;
    let approval = control
        .decide_project_candidate(
            project.id,
            candidate.id,
            contracts::authoring::AgentTrackKind::Environment,
            &contracts::http::CandidateDecisionRequest {
                candidate_revision: candidate.revision,
                policy_revision: candidate.policy_revision,
                trust_revision: Revision::new(1)?,
                decision: contracts::authoring::CandidateDecision::Approved,
                reason: "approve synthetic external VM catalog".to_owned(),
            },
            request.requester_id,
            Revision::new(1)?,
            &IdempotencyKey::parse("mutation-candidate-approve")?,
            now,
            &[],
        )
        .await?;
    let release = control
        .create_project_work_release(
            project.id,
            &contracts::http::CreateEnvironmentTemplateReleaseRequest {
                project_id: project.id,
                course_id: None,
                candidate_id: candidate.id,
                candidate_revision: candidate.revision,
                runtime_kind: contracts::authoring::RuntimeKind::VirtualMachine,
                approval_id: approval.id,
            },
            request.requester_id,
            false,
            &IdempotencyKey::parse("mutation-publish")?,
            now,
            "mutation-publish",
            &[],
        )
        .await?;
    request.target = ResourceTarget::Environment {
        environment_id: contracts::EnvironmentId::new(),
        release_id: release.id,
        release_version: release.version,
    };

    let desired = configuration::update(
        pool,
        ActorId::new(),
        &UpdatePlatformConfigurationRequest {
            expected_revision: 0,
            configuration: PlatformConfigurationDesired::default(),
        },
        &IdempotencyKey::parse("mutation-platform")?,
        now,
    )
    .await?;
    let version =
        configuration::get_version(pool, desired.desired.as_ref().ok_or("desired")?.revision)
            .await?;
    let directory = tempfile::tempdir()?;
    let (cipher, loaded) = fixture::actual_loaded_bundle(&version, directory.path())?;
    let registered = configuration::register_bundle(
        pool,
        ActorId::new(),
        version.revision,
        &RegisterPlatformConfigurationBundleRequest {
            expected_revision: desired.revision,
            configuration_sha256: version.configuration_sha256.clone(),
        },
        configuration::BundleCiphertext {
            digest: Sha256Digest::of_bytes(&cipher),
            bytes: u64::try_from(cipher.len())?,
        },
        &IdempotencyKey::parse("mutation-bundle")?,
        now,
    )
    .await?;
    let mut status = configuration::request_apply(
        pool,
        ActorId::new(),
        &ApplyPlatformConfigurationRequest {
            expected_revision: registered.revision,
            desired_revision: version.revision,
        },
        &IdempotencyKey::parse("mutation-apply")?,
        now,
    )
    .await?;
    for (index, stage) in [
        Stage::Validating,
        Stage::Applying,
        Stage::Verifying,
        Stage::Succeeded,
    ]
    .into_iter()
    .enumerate()
    {
        let apply = status.latest_apply.as_ref().ok_or("apply")?;
        status = configuration::report(
            pool,
            Some(&loaded),
            "synthetic-runtime-policy-executor",
            apply.id,
            &ReportPlatformConfigurationApplyRequest {
                expected_revision: apply.revision,
                control_replica_observations: None,
                stage,
                failure_reason: None,
                observed_configuration_sha256: (stage == Stage::Succeeded)
                    .then_some(version.configuration_sha256.clone()),
                observed_encrypted_bundle_sha256: (stage == Stage::Succeeded)
                    .then_some(apply.encrypted_bundle_sha256.clone()),
            },
            &IdempotencyKey::parse(&format!("mutation-stage-{index}"))?,
            now,
        )
        .await?;
    }
    let binding = resource_service::runtime_authorization::resolve_binding(pool, request).await?;
    assert_eq!(binding.request_id, request.id);
    assert_eq!(binding.project_id, project.id);
    assert_eq!(binding.configuration.candidate_id, candidate.id);
    Ok(base.provider_binding)
}
