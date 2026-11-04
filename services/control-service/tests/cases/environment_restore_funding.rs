//! Real production `PostgreSQL` roles, Control publication, Resource pricing/ledger and Environment caller.
#![allow(
    clippy::too_many_lines,
    reason = "keep each restore transaction invariant together"
)]
use super::super::membership_governance_cases::production_roles::ProductionRoles;
use super::*;
use contracts::environment::{
    EnvironmentDiskRetention, EnvironmentDiskState, EnvironmentRetainedStorageUsage,
    ObservedEnvironmentState, ResourceWorkCleanupStatus,
};
use contracts::resource::credits::CreditAdminOperation;
use contracts::resource::environment_funding::{
    EnvironmentCreditGrant, RestoreEnvironmentFundingRequest,
};
use contracts::resource::{
    CapacityClaim, CapacityClaimState, FixedDecimal, Money, ResourceApproval, ResourceBillingUnit,
    ResourceRequest, ResourceRequestState, ResourceTarget,
};
use resource_service::{
    ApprovalPolicy,
    credits::PgCreditStore,
    store::{PendingAllocation, PgResourceStore, ProvisioningCapacityClaim},
};

#[path = "environment_restore_http.rs"]
mod http_cases;

const OWNER: &str = "synthetic-environment";
struct Fixture {
    roles: ProductionRoles,
    store: PgResourceStore,
    request: ResourceRequest,
    approval: ResourceApproval,
    item: ProvisioningCapacityClaim,
    status: ResourceWorkCleanupStatus,
    original: EnvironmentCreditGrant,
    input: RestoreEnvironmentFundingRequest,
    projection: contracts::events::ReleasePublished,
}
impl Fixture {
    async fn new() -> TestResult<Self> {
        let roles = ProductionRoles::start().await?;
        let mut publication =
            ManualPublication::new(&roles.control, RuntimeKind::Container).await?;
        let store = PgResourceStore::new(roles.resource.clone())
            .with_environment_credit_owner(OWNER.into());
        let now = store.current_time().await?;
        publication.now = now;
        publication
            .request
            .manual
            .as_mut()
            .ok_or("manual spec")?
            .spec
            .retention
            .retain_until = Some(UtcTimestamp::from_utc(
            now.get() + time::Duration::hours(2),
        )?);
        let actor = publication.project.owner_actor_id;
        sqlx::query("INSERT INTO access.actors(actor_id,issuer,subject_sha256,username,display_name) VALUES($1,'https://synthetic.invalid',$2,'restore-payer','restore-payer')")
            .bind(actor.as_uuid()).bind(Sha256Digest::of_bytes(actor.as_uuid().as_bytes()).to_string()).execute(&roles.provisioner).await?;
        let release = publication
            .publish(actor, "restore-publish", &publication.request)
            .await?;
        let policy = control_service::platform_configuration::update(
            &roles.control,
            actor,
            &contracts::platform_configuration::UpdatePlatformConfigurationRequest {
                expected_revision: 0,
                configuration:
                    contracts::platform_configuration::PlatformConfigurationDesired::default(),
            },
            &IdempotencyKey::parse("restore-policy")?,
            now,
        )
        .await?;
        super::super::runtime_authorization_cases::verified(
            &roles.provisioner,
            policy,
            now,
            "restore-policy",
        )
        .await?;
        let resources = contracts::resource::WorkloadResources {
            cpu_millicores: 1000,
            memory_bytes: 1_073_741_824,
            storage_bytes: 10_737_418_240,
            gpu: None,
        };
        let request = ResourceRequest {
            id: contracts::ResourceRequestId::new(),
            generation: 1,
            request_key: "restore-funding".into(),
            requester_id: actor,
            project_id: publication.project.id,
            course_id: None,
            target: ResourceTarget::Environment {
                environment_id: EnvironmentId::new(),
                release_id: release.id,
                release_version: release.version,
            },
            requested_resources: resources.clone(),
            requested_duration_seconds: 60,
            state: ResourceRequestState::Reviewing,
            revision: Revision::new(1)?,
            created_at: now,
            updated_at: now,
            diagnostic_code: None,
        };
        let binding =
            resource_service::runtime_authorization::resolve_binding(&roles.resource, &request)
                .await?;
        let approval = ResourceApproval {
            runtime_authorization: Some(binding),
            id: contracts::ResourceApprovalId::new(),
            request_id: request.id,
            request_revision: request.revision,
            approver_id: actor,
            provider_binding: "container-primary-v1".into(),
            approved_resources: resources.clone(),
            approved_duration_seconds: 60,
            reason: "synthetic restore approval".into(),
            valid_until: UtcTimestamp::from_utc(now.get() + time::Duration::hours(1))?,
            created_at: now,
        };
        store
            .create("restore-create", &request, "restore-create")
            .await?;
        let allocation = PendingAllocation {
            claim: CapacityClaim {
                id: contracts::CapacityClaimId::new(),
                request_id: request.id,
                approval_id: approval.id,
                provider_binding: approval.provider_binding.clone(),
                workload_resources: resources.clone(),
                quota_resources: resources,
                gpu_allocation: None,
                state: CapacityClaimState::Reserved,
                revision: Revision::new(1)?,
                updated_at: Some(now),
            },
            lease_id: contracts::LeaseId::new(),
        };
        store
            .approve(
                "restore-approve",
                request.id,
                &approval,
                &allocation,
                ApprovalPolicy {
                    min_duration_seconds: 1,
                    max_duration_seconds: 3600,
                },
                "restore-approve",
            )
            .await?;
        for (index, unit) in [
            ResourceBillingUnit::CpuMillicoreSecond,
            ResourceBillingUnit::MemoryByteSecond,
            ResourceBillingUnit::StorageByteSecond,
        ]
        .into_iter()
        .enumerate()
        {
            store
                .create_rate(
                    &format!("restore-price-{index}"),
                    &contracts::http::CreateResourceRateRequest {
                        unit,
                        unit_quantity: if index == 0 { 1000 } else { 1 },
                        gpu_class: None,
                        gpu_mode: None,
                        builder_binding: None,
                        model_binding: None,
                        max_preauthorization: None,
                        unit_price: Money {
                            currency: "CREDITS".into(),
                            amount: FixedDecimal::parse(if index == 0 {
                                "0.010000"
                            } else {
                                "0.000000"
                            })?,
                        },
                        effective_from: UtcTimestamp::from_utc(
                            now.get() - time::Duration::hours(1),
                        )?,
                        effective_until: None,
                    },
                )
                .await?;
        }
        PgCreditStore::new(roles.resource.clone())
            .admin(
                "restore-allowance",
                actor,
                &[contracts::PlatformRole::PlatformAdmin],
                &CreditAdminOperation::Grant {
                    project_id: request.project_id,
                    amount: FixedDecimal::parse("100.000000")?,
                    reason: "synthetic restore allowance".into(),
                },
            )
            .await?;
        let shell = store
            .claim_next_capacity_shell()
            .await?
            .ok_or("capacity shell")?;
        let from = store.current_time().await?;
        let lease = store
            .activate_lease(
                allocation.lease_id,
                Revision::new(1)?,
                from,
                UtcTimestamp::from_utc(from.get() + time::Duration::seconds(60))?,
                actor,
                "restore-activate",
            )
            .await?;
        let claim = store
            .mark_capacity_shell_ready(
                shell.claim.id,
                shell.claim.revision,
                "synthetic-restore",
                "synthetic-namespace",
                "synthetic-quota",
            )
            .await?;
        let claim = store
            .mark_capacity_handed_off(claim.id, claim.revision, lease.id, lease.revision)
            .await?;
        let original = store.environment_credit_grant(OWNER, lease.id).await?;
        let item = ProvisioningCapacityClaim {
            claim,
            request: store.load(request.id).await?,
            lease: lease.clone(),
            lease_synced_revision: Some(lease.revision),
        };
        let retired = store.current_time().await?;
        let environment_id = request
            .target
            .environment_id()
            .ok_or("environment target")?;
        let status = ResourceWorkCleanupStatus {
            version: 1,
            environment_id,
            project_id: request.project_id,
            course_id: None,
            owner_actor_id: actor,
            lease_id: lease.id,
            lease_revision: lease.revision,
            capacity_binding: item.claim.id.to_string(),
            revision: Revision::new(3)?,
            observed_state: ObservedEnvironmentState::Deleted,
            cleanup_complete: false,
            generation: 2,
            compute_retired: true,
            storage_purged: false,
            disk_retention: Some(EnvironmentDiskRetention {
                state: EnvironmentDiskState::Retained,
                started_at: retired,
                expires_at: UtcTimestamp::from_utc(retired.get() + time::Duration::days(7))?,
                retired_generation: 2,
                compute_retired_at: Some(retired),
                restored_generation: None,
                reactivated_at: None,
                purged_at: None,
            }),
            retained_storage_usage: Some(EnvironmentRetainedStorageUsage {
                started_at: retired,
                measured_until: retired,
                storage_bytes: request.requested_resources.storage_bytes,
                closed: false,
            }),
            diagnostic_code: None,
        };
        store
            .consume_environment_cleanup_status(&item, &status)
            .await?;
        let input = RestoreEnvironmentFundingRequest {
            lease_id: lease.id,
            environment_id,
            project_id: request.project_id,
            requester_id: actor,
            restored_generation: 3,
        };
        Ok(Self {
            roles,
            store,
            request,
            approval,
            item,
            status,
            original,
            input,
            projection: contracts::events::ReleasePublished {
                release,
                environment_spec: publication
                    .request
                    .manual
                    .as_ref()
                    .ok_or("manual spec")?
                    .spec
                    .clone(),
            },
        })
    }
    async fn admit(&self, key: &str) -> TestResult<EnvironmentCreditGrant> {
        Ok(self
            .store
            .restore_environment_funding(key, OWNER, &self.input)
            .await?)
    }
    async fn holds(&self) -> TestResult<i64> {
        Ok(
            sqlx::query_scalar("SELECT count(*) FROM resource.credit_holds WHERE request_id=$1")
                .bind(self.request.id.as_uuid())
                .fetch_one(&self.roles.resource)
                .await?,
        )
    }
    async fn old_hold(&self) -> TestResult<serde_json::Value> {
        Ok(
            sqlx::query_scalar("SELECT to_jsonb(h) FROM resource.credit_holds h WHERE hold_id=$1")
                .bind(self.original.hold.id)
                .fetch_one(&self.roles.resource)
                .await?,
        )
    }
    async fn released(&self) -> TestResult<bool> {
        Ok(sqlx::query_scalar(
            "SELECT compute_released FROM resource.capacity_claims WHERE claim_id=$1",
        )
        .bind(self.item.claim.id.as_uuid())
        .fetch_one(&self.roles.resource)
        .await?)
    }
}

#[tokio::test]
async fn restore_funding_creates_new_hold_and_only_matching_receipt_clears_compute() -> TestResult {
    let mut f = Box::pin(Fixture::new()).await?;
    let old = f.old_hold().await?;
    let funded = f.admit("restore-funded").await?;
    assert_ne!(
        funded.hold.id, f.original.hold.id,
        "restore must create a new hold, never reuse the old hold"
    );
    assert_eq!(
        funded.estimate.total.amount,
        FixedDecimal::parse("0.600000")?
    );
    assert_eq!(f.holds().await?, 2);
    assert!(
        f.released().await?,
        "funding alone cannot clear compute-released"
    );
    assert_eq!(
        f.old_hold().await?,
        old,
        "retired hold must remain untouched"
    );
    assert!(
        f.store
            .environment_credit_grant(OWNER, f.input.lease_id)
            .await?
            .can_authorize_execution
    );
    let at = f.store.current_time().await?;
    f.status.revision = Revision::new(5)?;
    f.status.generation = 4;
    f.status.compute_retired = false;
    f.status.observed_state = ObservedEnvironmentState::Ready;
    let disk = f.status.disk_retention.as_mut().ok_or("retained disk")?;
    disk.state = EnvironmentDiskState::Restored;
    disk.restored_generation = Some(3);
    disk.reactivated_at = Some(at);
    let usage = f
        .status
        .retained_storage_usage
        .as_mut()
        .ok_or("retained usage")?;
    usage.measured_until = at;
    usage.closed = true;
    // A real but old lease revision receipt must not lift the new funding fence.
    f.store
        .consume_environment_cleanup_status(&f.item, &f.status)
        .await?;
    assert!(
        f.released().await?,
        "old revision receipt cannot clear compute-released"
    );
    f.item.lease = f.store.load_lease(f.input.lease_id).await?;
    f.store
        .mark_lease_synced(f.item.claim.id, funded.lease_revision)
        .await?;
    f.item.lease_synced_revision = Some(funded.lease_revision);
    f.status.lease_revision = funded.lease_revision;
    f.status.revision = Revision::new(6)?;
    f.store
        .consume_environment_cleanup_status(&f.item, &f.status)
        .await?;
    assert!(
        !f.released().await?,
        "matching revision and generation must clear compute-released"
    );
    let closed: (bool, time::OffsetDateTime) = sqlx::query_as(
        "SELECT closed,closed_at FROM resource.retained_storage_intervals WHERE lease_id=$1",
    )
    .bind(f.input.lease_id.as_uuid())
    .fetch_one(&f.roles.resource)
    .await?;
    assert_eq!(closed, (true, at.get()));
    let costs: i64 =
        sqlx::query_scalar("SELECT count(*) FROM resource.retained_storage_platform_costs")
            .fetch_one(&f.roles.resource)
            .await?;
    f.store
        .consume_environment_cleanup_status(&f.item, &f.status)
        .await?;
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM resource.retained_storage_platform_costs"
        )
        .fetch_one(&f.roles.resource)
        .await?,
        costs,
        "reactivation closes and prices retained storage exactly once"
    );
    assert_eq!(f.old_hold().await?, old);
    Ok(())
}

#[tokio::test]
async fn restore_funding_expired_original_approval_rejects_without_hold() -> TestResult {
    let f = Box::pin(Fixture::new()).await?;
    let mut expired = f.approval.clone();
    expired.valid_until =
        UtcTimestamp::from_utc(expired.created_at.get() + time::Duration::milliseconds(1))?;
    sqlx::query(
        "UPDATE resource.resource_approvals SET valid_until=$2,contract=$3 WHERE approval_id=$1",
    )
    .bind(expired.id.as_uuid())
    .bind(expired.valid_until.get())
    .bind(serde_json::to_value(expired)?)
    .execute(&f.roles.provisioner)
    .await?;
    let result = f
        .store
        .restore_environment_funding("restore-expired", OWNER, &f.input)
        .await;
    assert!(
        matches!(
            result,
            Err(resource_service::store::ResourceStoreError::ApprovalInvalid)
        ),
        "expired original approval must reject restore: {result:?}"
    );
    assert_eq!(f.holds().await?, 1);
    assert!(f.released().await?);
    Ok(())
}

#[tokio::test]
async fn restore_funding_revoked_membership_rejects_and_replay_rechecks_scope() -> TestResult {
    let f = Box::pin(Fixture::new()).await?;
    sqlx::query("UPDATE control.project_memberships SET state='revoked',revision=revision+1 WHERE project_id=$1 AND actor_id=$2")
        .bind(f.request.project_id.as_uuid()).bind(f.request.requester_id.as_uuid()).execute(&f.roles.control).await?;
    let result = f
        .store
        .restore_environment_funding("restore-revoked", OWNER, &f.input)
        .await;
    assert!(
        matches!(
            result,
            Err(resource_service::store::ResourceStoreError::RuntimeBinding(
                resource_service::runtime_authorization::RuntimeBindingError::ScopeDenied
            ))
        ),
        "current revoked membership must reject restore: {result:?}"
    );
    assert_eq!(f.holds().await?, 1);
    sqlx::query("UPDATE control.project_memberships SET state='active',revision=revision+1 WHERE project_id=$1 AND actor_id=$2")
        .bind(f.request.project_id.as_uuid()).bind(f.request.requester_id.as_uuid()).execute(&f.roles.control).await?;
    f.admit("restore-revoked").await?;
    sqlx::query("UPDATE control.project_memberships SET state='revoked',revision=revision+1 WHERE project_id=$1 AND actor_id=$2")
        .bind(f.request.project_id.as_uuid()).bind(f.request.requester_id.as_uuid()).execute(&f.roles.control).await?;
    assert!(
        f.admit("restore-revoked").await.is_err(),
        "replay must recheck current membership"
    );
    assert_eq!(f.holds().await?, 2);
    Ok(())
}

#[tokio::test]
async fn restore_funding_insufficient_credits_rolls_back_all_admission_state() -> TestResult {
    let f = Box::pin(Fixture::new()).await?;
    PgCreditStore::new(f.roles.resource.clone())
        .admin(
            "restore-drain",
            f.request.requester_id,
            &[contracts::PlatformRole::PlatformAdmin],
            &CreditAdminOperation::Adjust {
                project_id: f.request.project_id,
                amount: FixedDecimal::parse("-99.400000")?,
                reason: "synthetic no available restore credits".into(),
            },
        )
        .await?;
    let old = f.old_hold().await?;
    let lease = f.store.load_lease(f.input.lease_id).await?;
    let result = f
        .store
        .restore_environment_funding("restore-insufficient", OWNER, &f.input)
        .await;
    assert!(
        matches!(
            result,
            Err(resource_service::store::ResourceStoreError::Credits(
                resource_service::credits::CreditError::Insufficient
            ))
        ),
        "insufficient credits must reject restore: {result:?}"
    );
    assert_eq!(f.holds().await?, 1);
    assert_eq!(f.store.load_lease(f.input.lease_id).await?, lease);
    assert_eq!(f.old_hold().await?, old);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM resource.environment_restore_funding")
            .fetch_one(&f.roles.resource)
            .await?,
        0
    );
    Ok(())
}

#[tokio::test]
async fn restore_funding_replay_concurrent_duplicate_and_changed_body_are_fenced() -> TestResult {
    let f = Box::pin(Fixture::new()).await?;
    let (a, b) = tokio::join!(f.admit("restore-concurrent"), f.admit("restore-concurrent"));
    let a = a?;
    assert_eq!(a, b?);
    assert_eq!(a, f.admit("restore-concurrent").await?);
    assert_eq!(a, f.admit("restore-new-key").await?);
    assert_eq!(
        f.holds().await?,
        2,
        "concurrent retries must create exactly one new hold"
    );
    let mut changed = f.input.clone();
    changed.restored_generation += 1;
    let error = f
        .store
        .restore_environment_funding("restore-concurrent", OWNER, &changed)
        .await;
    assert!(matches!(
        error,
        Err(resource_service::store::ResourceStoreError::Credits(
            resource_service::credits::CreditError::Conflict
        ))
    ));
    Ok(())
}

#[tokio::test]
async fn restore_funding_uses_current_rate_not_original_rate_snapshot() -> TestResult {
    let f = Box::pin(Fixture::new()).await?;
    let old = f.old_hold().await?;
    let cpu = &f
        .original
        .estimate
        .lines
        .iter()
        .find(|line| line.unit == ResourceBillingUnit::CpuMillicoreSecond)
        .ok_or("cpu rate")?;
    let boundary =
        UtcTimestamp::from_utc(f.store.current_time().await?.get() + time::Duration::seconds(1))?;
    f.store
        .end_rate(
            cpu.rate_id,
            "restore-end-rate",
            &contracts::http::EndResourceRateRequest {
                effective_until: boundary,
            },
        )
        .await?;
    let rate = f
        .store
        .create_rate(
            "restore-new-rate",
            &contracts::http::CreateResourceRateRequest {
                unit: ResourceBillingUnit::CpuMillicoreSecond,
                unit_quantity: 1000,
                gpu_class: None,
                gpu_mode: None,
                builder_binding: None,
                model_binding: None,
                max_preauthorization: None,
                unit_price: Money {
                    currency: "CREDITS".into(),
                    amount: FixedDecimal::parse("0.020000")?,
                },
                effective_from: boundary,
                effective_until: None,
            },
        )
        .await?;
    let delay = boundary.get() - f.store.current_time().await?.get();
    if delay.is_positive() {
        tokio::time::sleep(std::time::Duration::try_from(delay)?).await;
    }
    let restored = f.admit("restore-repriced").await?;
    assert_eq!(
        restored.estimate.total.amount,
        FixedDecimal::parse("1.200000")?
    );
    assert!(
        restored
            .estimate
            .lines
            .iter()
            .any(|line| line.rate_id == rate.id)
    );
    assert_eq!(f.old_hold().await?, old);
    Ok(())
}
