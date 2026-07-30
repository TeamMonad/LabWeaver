//! PostgreSQL authority for Resource requests and administrator decisions.

use contracts::environment::{
    EnvironmentLeaseAuthorization, EnvironmentLeaseState, EnvironmentLeaseVerificationRequest,
    EnvironmentLeaseVerificationResponse,
};
use contracts::http::IdempotencyKey;
use contracts::resource::{
    CapacityClaim, ResourceApproval, ResourceLease, ResourceLeaseState, ResourceRequest,
    ResourceRequestState,
};
use contracts::{EventId, LeaseId, ResourceRequestId, Sha256Digest, UtcTimestamp};
use persistence_sqlx::{
    Domain, IdempotencyDecision, IdempotencyStore, OutboxStore, PersistenceError,
};
use serde_json::{Value, json};
use sqlx::{PgPool, Postgres, Row, Transaction};

use crate::{ApprovalPolicy, LifecycleError, ResourceLifecycle};

const REQUEST_SUBMITTED_SUBJECT: &str = "labweaver.resource.request.submitted.v1";
const REQUEST_APPROVED_SUBJECT: &str = "labweaver.resource.request.approved.v1";

/// Deterministic capacity plan created before provider side effects are scheduled.
#[derive(Clone, Debug)]
pub struct PendingAllocation {
    pub claim: CapacityClaim,
    pub lease_id: LeaseId,
}

impl PendingAllocation {
    fn validate(
        &self,
        request: &ResourceRequest,
        approval: &ResourceApproval,
        now: UtcTimestamp,
    ) -> Result<ResourceLease, ResourceStoreError> {
        self.claim
            .validate()
            .map_err(ResourceStoreError::Contract)?;
        if self.claim.request_id != request.id
            || self.claim.approval_id != approval.id
            || self.claim.provider_binding != approval.provider_binding
            || self.claim.policy_sha256 != approval.policy_sha256
            || self.claim.workload_resources != approval.approved_resources
        {
            return Err(ResourceStoreError::AllocationMismatch);
        }
        let lease = ResourceLease {
            id: self.lease_id,
            request_id: request.id,
            claim_id: self.claim.id,
            state: ResourceLeaseState::Allocating,
            revision: contracts::Revision::new(1)?,
            active_from: None,
            expires_at: None,
            revoke_reason_code: None,
            created_at: now,
            updated_at: now,
        };
        lease.validate().map_err(ResourceStoreError::Contract)?;
        Ok(lease)
    }
}

/// PostgreSQL-authoritative Resource repository.
#[derive(Clone)]
pub struct PgResourceStore {
    pool: PgPool,
}

impl PgResourceStore {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Uses the database clock for every expiry and approval decision.
    pub async fn current_time(&self) -> Result<UtcTimestamp, ResourceStoreError> {
        let now: time::OffsetDateTime =
            sqlx::query_scalar("SELECT date_trunc('milliseconds', clock_timestamp())")
                .fetch_one(&self.pool)
                .await?;
        UtcTimestamp::from_utc(now).map_err(Into::into)
    }

    /// Persists a new reviewed request, its first transition, idempotency result, and Outbox fact.
    pub async fn create(
        &self,
        idempotency_key: &str,
        request: &ResourceRequest,
        trace_id: &str,
    ) -> Result<ResourceRequest, ResourceStoreError> {
        request.validate().map_err(ResourceStoreError::Contract)?;
        if request.state != ResourceRequestState::Reviewing {
            return Err(ResourceStoreError::InvalidCreateState);
        }
        IdempotencyKey::parse(idempotency_key).map_err(|_| ResourceStoreError::IdempotencyKey)?;
        validate_trace(trace_id)?;
        let mut transaction = self.pool.begin().await?;
        let hash = Sha256Digest::of_canonical(request)?;
        let outcome = match IdempotencyStore::reserve(
            &mut transaction,
            Domain::Resource,
            "create_resource_request",
            idempotency_key,
            hash,
        )
        .await?
        {
            IdempotencyDecision::Replay(value) => decode_request(value)?,
            IdempotencyDecision::Conflict => return Err(ResourceStoreError::IdempotencyConflict),
            IdempotencyDecision::InProgress => {
                return Err(ResourceStoreError::IdempotencyInProgress);
            }
            IdempotencyDecision::Reserved => {
                insert_request(&mut transaction, request).await?;
                insert_transition(
                    &mut transaction,
                    request,
                    1,
                    None,
                    Some(request.requester_id),
                    trace_id,
                )
                .await?;
                enqueue_request_event(
                    &mut transaction,
                    request,
                    REQUEST_SUBMITTED_SUBJECT,
                    trace_id,
                )
                .await?;
                let value = serde_json::to_value(request)?;
                IdempotencyStore::complete(
                    &mut transaction,
                    Domain::Resource,
                    "create_resource_request",
                    idempotency_key,
                    &value,
                )
                .await?;
                request.clone()
            }
        };
        transaction.commit().await?;
        Ok(outcome)
    }

    /// Loads one strict request projection. Empty legacy snapshots are never accepted.
    pub async fn load(
        &self,
        request_id: ResourceRequestId,
    ) -> Result<ResourceRequest, ResourceStoreError> {
        let row =
            sqlx::query("SELECT contract FROM resource.resource_requests WHERE request_id=$1")
                .bind(request_id.as_uuid())
                .fetch_optional(&self.pool)
                .await?
                .ok_or(ResourceStoreError::NotFound)?;
        decode_request(row.try_get("contract")?)
    }

    /// Resolves the exact authorization fence required by Environment before Work creation.
    /// Mismatched scope deliberately receives the same non-active response as an expired Lease.
    pub async fn verify_environment_lease(
        &self,
        verification: &EnvironmentLeaseVerificationRequest,
        authority_now: UtcTimestamp,
    ) -> Result<EnvironmentLeaseVerificationResponse, ResourceStoreError> {
        if verification.version != 1 || verification.capacity_binding.trim().is_empty() {
            return Ok(inactive_lease_response(EnvironmentLeaseState::Revoked));
        }
        let row = sqlx::query(
            "SELECT l.contract AS lease_contract, c.contract AS claim_contract, r.contract AS request_contract FROM resource.resource_leases l JOIN resource.capacity_claims c ON c.claim_id=l.claim_id JOIN resource.resource_requests r ON r.request_id=l.request_id WHERE l.lease_id=$1",
        )
        .bind(verification.lease_id.as_uuid())
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else {
            return Ok(inactive_lease_response(EnvironmentLeaseState::Revoked));
        };
        let lease = decode_lease(row.try_get("lease_contract")?)?;
        let claim = decode_claim(row.try_get("claim_contract")?)?;
        let request = decode_request(row.try_get("request_contract")?)?;
        let state = environment_lease_state(lease.state);
        if lease.state != ResourceLeaseState::Active
            || lease
                .expires_at
                .is_none_or(|expires_at| expires_at <= authority_now)
            || verification.environment_id != request.target.environment_id
            || verification.course_id != request.course_id
            || verification.owner_actor_id != request.requester_id
            || verification.capacity_binding != claim.id.to_string()
        {
            return Ok(inactive_lease_response(state));
        }
        let authorization = EnvironmentLeaseAuthorization {
            lease_id: lease.id,
            lease_revision: lease.revision,
            environment_id: request.target.environment_id,
            course_id: request.course_id,
            owner_actor_id: request.requester_id,
            capacity_binding: claim.id.to_string(),
            active_from: lease
                .active_from
                .ok_or(ResourceStoreError::LeaseWindowMissing)?,
            expires_at: lease
                .expires_at
                .ok_or(ResourceStoreError::LeaseWindowMissing)?,
        };
        Ok(EnvironmentLeaseVerificationResponse {
            version: 1,
            state: EnvironmentLeaseState::Active,
            authorization: Some(authorization),
        })
    }

    /// Appends an approval and transitions a request to allocation in one transaction.
    pub async fn approve(
        &self,
        idempotency_key: &str,
        request_id: ResourceRequestId,
        approval: &ResourceApproval,
        allocation: &PendingAllocation,
        policy: ApprovalPolicy,
        trace_id: &str,
    ) -> Result<ResourceRequest, ResourceStoreError> {
        IdempotencyKey::parse(idempotency_key).map_err(|_| ResourceStoreError::IdempotencyKey)?;
        validate_trace(trace_id)?;
        let mut transaction = self.pool.begin().await?;
        let request = load_locked(&mut transaction, request_id).await?;
        let hash = Sha256Digest::of_canonical(&(approval, &allocation.claim, allocation.lease_id))?;
        let result = match IdempotencyStore::reserve(
            &mut transaction,
            Domain::Resource,
            "approve_resource_request",
            idempotency_key,
            hash,
        )
        .await?
        {
            IdempotencyDecision::Replay(value) => decode_request(value)?,
            IdempotencyDecision::Conflict => return Err(ResourceStoreError::IdempotencyConflict),
            IdempotencyDecision::InProgress => {
                return Err(ResourceStoreError::IdempotencyInProgress);
            }
            IdempotencyDecision::Reserved => {
                let now = database_now(&mut transaction).await?;
                let next = ResourceLifecycle::accept_approval(&request, approval, policy, now)?;
                let lease = allocation.validate(&request, approval, now)?;
                insert_approval(&mut transaction, approval).await?;
                insert_claim(&mut transaction, &allocation.claim).await?;
                insert_lease(&mut transaction, &lease).await?;
                update_request(&mut transaction, &request, &next).await?;
                insert_transition(
                    &mut transaction,
                    &next,
                    next.revision.get(),
                    Some(ResourceRequestState::Reviewing),
                    Some(approval.approver_id),
                    trace_id,
                )
                .await?;
                enqueue_request_event(&mut transaction, &next, REQUEST_APPROVED_SUBJECT, trace_id)
                    .await?;
                let value = serde_json::to_value(&next)?;
                IdempotencyStore::complete(
                    &mut transaction,
                    Domain::Resource,
                    "approve_resource_request",
                    idempotency_key,
                    &value,
                )
                .await?;
                next
            }
        };
        transaction.commit().await?;
        Ok(result)
    }

    /// Loads a Lease projection; malformed snapshots never authorize use.
    pub async fn load_lease(&self, lease_id: LeaseId) -> Result<ResourceLease, ResourceStoreError> {
        let row = sqlx::query("SELECT contract FROM resource.resource_leases WHERE lease_id=$1")
            .bind(lease_id.as_uuid())
            .fetch_optional(&self.pool)
            .await?
            .ok_or(ResourceStoreError::LeaseNotFound)?;
        decode_lease(row.try_get("contract")?)
    }

    /// Applies an administrator renewal with an exact revision and idempotency fence.
    pub async fn renew_lease(
        &self,
        idempotency_key: &str,
        lease_id: LeaseId,
        expected_revision: contracts::Revision,
        expires_at: UtcTimestamp,
    ) -> Result<ResourceLease, ResourceStoreError> {
        IdempotencyKey::parse(idempotency_key).map_err(|_| ResourceStoreError::IdempotencyKey)?;
        let mut transaction = self.pool.begin().await?;
        let lease = load_locked_lease(&mut transaction, lease_id).await?;
        let hash = Sha256Digest::of_canonical(&(lease_id, expected_revision, expires_at))?;
        let result = match IdempotencyStore::reserve(
            &mut transaction,
            Domain::Resource,
            "renew_resource_lease",
            idempotency_key,
            hash,
        )
        .await?
        {
            IdempotencyDecision::Replay(value) => decode_lease(value)?,
            IdempotencyDecision::Conflict => return Err(ResourceStoreError::IdempotencyConflict),
            IdempotencyDecision::InProgress => {
                return Err(ResourceStoreError::IdempotencyInProgress);
            }
            IdempotencyDecision::Reserved => {
                let now = database_now(&mut transaction).await?;
                let next =
                    ResourceLifecycle::renew_lease(&lease, expected_revision, expires_at, now)?;
                update_lease(&mut transaction, &lease, &next).await?;
                let value = serde_json::to_value(&next)?;
                IdempotencyStore::complete(
                    &mut transaction,
                    Domain::Resource,
                    "renew_resource_lease",
                    idempotency_key,
                    &value,
                )
                .await?;
                next
            }
        };
        transaction.commit().await?;
        Ok(result)
    }

    /// Activates a Lease only after the selected capacity provider has read back its exact fence.
    pub async fn activate_lease(
        &self,
        lease_id: LeaseId,
        expected_revision: contracts::Revision,
        active_from: UtcTimestamp,
        expires_at: UtcTimestamp,
    ) -> Result<ResourceLease, ResourceStoreError> {
        let mut transaction = self.pool.begin().await?;
        let lease = load_locked_lease(&mut transaction, lease_id).await?;
        let next =
            ResourceLifecycle::activate_lease(&lease, expected_revision, active_from, expires_at)?;
        update_lease(&mut transaction, &lease, &next).await?;
        transaction.commit().await?;
        Ok(next)
    }

    /// Starts the fail-closed expiry saga. Capacity remains reserved until cleanup readback.
    pub async fn begin_lease_expiry(
        &self,
        lease_id: LeaseId,
        expected_revision: contracts::Revision,
        reason: Option<String>,
    ) -> Result<ResourceLease, ResourceStoreError> {
        let mut transaction = self.pool.begin().await?;
        let lease = load_locked_lease(&mut transaction, lease_id).await?;
        let now = database_now(&mut transaction).await?;
        let next = ResourceLifecycle::begin_lease_expiry(&lease, expected_revision, now, reason)?;
        update_lease(&mut transaction, &lease, &next).await?;
        transaction.commit().await?;
        Ok(next)
    }

    /// Marks a Lease expired after Environment cleanup and exact capacity release readback.
    pub async fn complete_lease_expiry(
        &self,
        lease_id: LeaseId,
        expected_revision: contracts::Revision,
    ) -> Result<ResourceLease, ResourceStoreError> {
        let mut transaction = self.pool.begin().await?;
        let lease = load_locked_lease(&mut transaction, lease_id).await?;
        let now = database_now(&mut transaction).await?;
        let next = ResourceLifecycle::complete_lease_expiry(&lease, expected_revision, now)?;
        update_lease(&mut transaction, &lease, &next).await?;
        transaction.commit().await?;
        Ok(next)
    }
}

async fn insert_request(
    transaction: &mut Transaction<'_, Postgres>,
    request: &ResourceRequest,
) -> Result<(), ResourceStoreError> {
    let gpu = request.requested_resources.gpu.as_ref();
    sqlx::query(
        "INSERT INTO resource.resource_requests (request_id,generation,request_key,requester_id,course_id,project_id,environment_id,release_id,release_version,release_sha256,requested_cpu_millicores,requested_memory_bytes,requested_storage_bytes,gpu_class,gpu_count,requested_duration_seconds,state,revision,diagnostic_code,created_at,updated_at,contract) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21,$22)",
    )
    .bind(request.id.as_uuid()).bind(i64::try_from(request.generation)?).bind(&request.request_key)
    .bind(request.requester_id.as_uuid()).bind(request.course_id.as_uuid()).bind(request.project_id.map(contracts::ProjectId::as_uuid))
    .bind(request.target.environment_id.as_uuid()).bind(request.target.release_id.as_uuid()).bind(i64::try_from(request.target.release_version)?).bind(request.target.release_sha256.to_string())
    .bind(i32::try_from(request.requested_resources.cpu_millicores)?).bind(i64::try_from(request.requested_resources.memory_bytes)?).bind(i64::try_from(request.requested_resources.storage_bytes)?)
    .bind(gpu.map(|value| value.class.as_str())).bind(gpu.map(|value| i32::try_from(value.count)).transpose()?)
    .bind(i64::try_from(request.requested_duration_seconds)?).bind(wire(request.state)?).bind(i64::try_from(request.revision.get())?).bind(&request.diagnostic_code)
    .bind(request.created_at.get()).bind(request.updated_at.get()).bind(serde_json::to_value(request)?)
    .execute(&mut **transaction).await?;
    Ok(())
}

async fn insert_approval(
    transaction: &mut Transaction<'_, Postgres>,
    approval: &ResourceApproval,
) -> Result<(), ResourceStoreError> {
    let gpu = approval.approved_resources.gpu.as_ref();
    sqlx::query("INSERT INTO resource.resource_approvals (approval_id,request_id,request_revision,approver_id,provider_binding,policy_sha256,approved_cpu_millicores,approved_memory_bytes,approved_storage_bytes,approved_gpu_class,approved_gpu_count,approved_duration_seconds,reason,valid_until,created_at,contract) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16)")
        .bind(approval.id.as_uuid()).bind(approval.request_id.as_uuid()).bind(i64::try_from(approval.request_revision.get())?).bind(approval.approver_id.as_uuid()).bind(&approval.provider_binding).bind(approval.policy_sha256.to_string())
        .bind(i32::try_from(approval.approved_resources.cpu_millicores)?).bind(i64::try_from(approval.approved_resources.memory_bytes)?).bind(i64::try_from(approval.approved_resources.storage_bytes)?)
        .bind(gpu.map(|value| value.class.as_str())).bind(gpu.map(|value| i32::try_from(value.count)).transpose()?).bind(i64::try_from(approval.approved_duration_seconds)?)
        .bind(&approval.reason).bind(approval.valid_until.get()).bind(approval.created_at.get()).bind(serde_json::to_value(approval)?)
        .execute(&mut **transaction).await?;
    Ok(())
}

async fn insert_claim(
    transaction: &mut Transaction<'_, Postgres>,
    claim: &CapacityClaim,
) -> Result<(), ResourceStoreError> {
    let workload_gpu = claim.workload_resources.gpu.as_ref();
    let quota_gpu = claim.quota_resources.gpu.as_ref();
    sqlx::query("INSERT INTO resource.capacity_claims (claim_id,request_id,approval_id,provider_binding,policy_sha256,quota_plan_sha256,state,revision,created_at,updated_at,workload_cpu_millicores,workload_memory_bytes,workload_storage_bytes,workload_gpu_class,workload_gpu_count,quota_cpu_millicores,quota_memory_bytes,quota_storage_bytes,quota_gpu_class,quota_gpu_count,contract) VALUES ($1,$2,$3,$4,$5,$6,'reserved',$7,clock_timestamp(),clock_timestamp(),$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18)")
        .bind(claim.id.as_uuid()).bind(claim.request_id.as_uuid()).bind(claim.approval_id.as_uuid()).bind(&claim.provider_binding).bind(claim.policy_sha256.to_string()).bind(claim.quota_plan_sha256.to_string()).bind(i64::try_from(claim.revision.get())?)
        .bind(i32::try_from(claim.workload_resources.cpu_millicores)?).bind(i64::try_from(claim.workload_resources.memory_bytes)?).bind(i64::try_from(claim.workload_resources.storage_bytes)?)
        .bind(workload_gpu.map(|value| value.class.as_str())).bind(workload_gpu.map(|value| i32::try_from(value.count)).transpose()?)
        .bind(i32::try_from(claim.quota_resources.cpu_millicores)?).bind(i64::try_from(claim.quota_resources.memory_bytes)?).bind(i64::try_from(claim.quota_resources.storage_bytes)?)
        .bind(quota_gpu.map(|value| value.class.as_str())).bind(quota_gpu.map(|value| i32::try_from(value.count)).transpose()?)
        .bind(serde_json::to_value(claim)?)
        .execute(&mut **transaction).await?;
    Ok(())
}

async fn insert_lease(
    transaction: &mut Transaction<'_, Postgres>,
    lease: &ResourceLease,
) -> Result<(), ResourceStoreError> {
    sqlx::query("INSERT INTO resource.resource_leases (lease_id,request_id,claim_id,state,revision,active_from,expires_at,revoke_reason_code,created_at,updated_at,contract) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)")
        .bind(lease.id.as_uuid()).bind(lease.request_id.as_uuid()).bind(lease.claim_id.as_uuid()).bind(wire(lease.state)?).bind(i64::try_from(lease.revision.get())?)
        .bind(lease.active_from.map(UtcTimestamp::get)).bind(lease.expires_at.map(UtcTimestamp::get)).bind(&lease.revoke_reason_code).bind(lease.created_at.get()).bind(lease.updated_at.get()).bind(serde_json::to_value(lease)?)
        .execute(&mut **transaction).await?;
    Ok(())
}

async fn update_request(
    transaction: &mut Transaction<'_, Postgres>,
    current: &ResourceRequest,
    next: &ResourceRequest,
) -> Result<(), ResourceStoreError> {
    let changed = sqlx::query("UPDATE resource.resource_requests SET state=$3,revision=$4,diagnostic_code=$5,updated_at=$6,contract=$7 WHERE request_id=$1 AND revision=$2")
        .bind(current.id.as_uuid()).bind(i64::try_from(current.revision.get())?).bind(wire(next.state)?).bind(i64::try_from(next.revision.get())?).bind(&next.diagnostic_code).bind(next.updated_at.get()).bind(serde_json::to_value(next)?)
        .execute(&mut **transaction).await?;
    if changed.rows_affected() != 1 {
        return Err(ResourceStoreError::RevisionConflict);
    }
    Ok(())
}

async fn load_locked(
    transaction: &mut Transaction<'_, Postgres>,
    request_id: ResourceRequestId,
) -> Result<ResourceRequest, ResourceStoreError> {
    let row = sqlx::query(
        "SELECT contract FROM resource.resource_requests WHERE request_id=$1 FOR UPDATE",
    )
    .bind(request_id.as_uuid())
    .fetch_optional(&mut **transaction)
    .await?
    .ok_or(ResourceStoreError::NotFound)?;
    decode_request(row.try_get("contract")?)
}

async fn load_locked_lease(
    transaction: &mut Transaction<'_, Postgres>,
    lease_id: LeaseId,
) -> Result<ResourceLease, ResourceStoreError> {
    let row =
        sqlx::query("SELECT contract FROM resource.resource_leases WHERE lease_id=$1 FOR UPDATE")
            .bind(lease_id.as_uuid())
            .fetch_optional(&mut **transaction)
            .await?
            .ok_or(ResourceStoreError::LeaseNotFound)?;
    decode_lease(row.try_get("contract")?)
}

async fn update_lease(
    transaction: &mut Transaction<'_, Postgres>,
    current: &ResourceLease,
    next: &ResourceLease,
) -> Result<(), ResourceStoreError> {
    let changed = sqlx::query("UPDATE resource.resource_leases SET state=$3,revision=$4,active_from=$5,expires_at=$6,revoke_reason_code=$7,updated_at=$8,contract=$9 WHERE lease_id=$1 AND revision=$2")
        .bind(current.id.as_uuid()).bind(i64::try_from(current.revision.get())?).bind(wire(next.state)?).bind(i64::try_from(next.revision.get())?)
        .bind(next.active_from.map(UtcTimestamp::get)).bind(next.expires_at.map(UtcTimestamp::get)).bind(&next.revoke_reason_code).bind(next.updated_at.get()).bind(serde_json::to_value(next)?)
        .execute(&mut **transaction).await?;
    if changed.rows_affected() != 1 {
        return Err(ResourceStoreError::RevisionConflict);
    }
    Ok(())
}

async fn insert_transition(
    transaction: &mut Transaction<'_, Postgres>,
    request: &ResourceRequest,
    sequence: u64,
    from: Option<ResourceRequestState>,
    actor: Option<contracts::ActorId>,
    trace_id: &str,
) -> Result<(), ResourceStoreError> {
    sqlx::query("INSERT INTO resource.resource_request_transitions (request_id,sequence,from_state,to_state,actor_id,diagnostic_code,trace_id,occurred_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8)")
        .bind(request.id.as_uuid()).bind(i64::try_from(sequence)?).bind(from.map(wire).transpose()?).bind(wire(request.state)?).bind(actor.map(contracts::ActorId::as_uuid)).bind(&request.diagnostic_code).bind(trace_id).bind(request.updated_at.get()).execute(&mut **transaction).await?;
    Ok(())
}

async fn enqueue_request_event(
    transaction: &mut Transaction<'_, Postgres>,
    request: &ResourceRequest,
    subject: &str,
    trace_id: &str,
) -> Result<(), ResourceStoreError> {
    let payload = json!({"request": request, "traceId": trace_id});
    let hash = Sha256Digest::of_canonical(&payload)?;
    OutboxStore::enqueue(
        transaction,
        Domain::Resource,
        EventId::new().as_uuid(),
        subject,
        subject,
        request.id.as_uuid(),
        request.revision.get(),
        &payload,
        hash,
    )
    .await?;
    Ok(())
}

async fn database_now(
    transaction: &mut Transaction<'_, Postgres>,
) -> Result<UtcTimestamp, ResourceStoreError> {
    let now: time::OffsetDateTime =
        sqlx::query_scalar("SELECT date_trunc('milliseconds', clock_timestamp())")
            .fetch_one(&mut **transaction)
            .await?;
    UtcTimestamp::from_utc(now).map_err(Into::into)
}

fn decode_request(value: Value) -> Result<ResourceRequest, ResourceStoreError> {
    let request: ResourceRequest = serde_json::from_value(value)?;
    request.validate()?;
    Ok(request)
}
fn decode_claim(value: Value) -> Result<CapacityClaim, ResourceStoreError> {
    let claim: CapacityClaim = serde_json::from_value(value)?;
    claim.validate()?;
    Ok(claim)
}
fn decode_lease(value: Value) -> Result<ResourceLease, ResourceStoreError> {
    let lease: ResourceLease = serde_json::from_value(value)?;
    lease.validate()?;
    Ok(lease)
}
const fn environment_lease_state(state: ResourceLeaseState) -> EnvironmentLeaseState {
    match state {
        ResourceLeaseState::Active => EnvironmentLeaseState::Active,
        ResourceLeaseState::Allocating | ResourceLeaseState::Expiring => {
            EnvironmentLeaseState::Expiring
        }
        ResourceLeaseState::Expired => EnvironmentLeaseState::Expired,
        ResourceLeaseState::Revoked => EnvironmentLeaseState::Revoked,
    }
}
fn inactive_lease_response(state: EnvironmentLeaseState) -> EnvironmentLeaseVerificationResponse {
    EnvironmentLeaseVerificationResponse {
        version: 1,
        state,
        authorization: None,
    }
}
fn wire<T: serde::Serialize>(value: T) -> Result<String, ResourceStoreError> {
    serde_json::to_value(value)?
        .as_str()
        .map(ToOwned::to_owned)
        .ok_or(ResourceStoreError::Wire)
}
fn validate_trace(value: &str) -> Result<(), ResourceStoreError> {
    if value.is_empty() || value.len() > 128 || value.chars().any(char::is_control) {
        Err(ResourceStoreError::Trace)
    } else {
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ResourceStoreError {
    #[error("LW_RESOURCE_NOT_FOUND")]
    NotFound,
    #[error("LW_RESOURCE_CREATE_STATE_INVALID")]
    InvalidCreateState,
    #[error("LW_IDEMPOTENCY_KEY_INVALID")]
    IdempotencyKey,
    #[error("LW_IDEMPOTENCY_CONFLICT")]
    IdempotencyConflict,
    #[error("LW_IDEMPOTENCY_IN_PROGRESS")]
    IdempotencyInProgress,
    #[error("LW_RESOURCE_REVISION_CONFLICT")]
    RevisionConflict,
    #[error("LW_RESOURCE_ALLOCATION_MISMATCH")]
    AllocationMismatch,
    #[error("LW_RESOURCE_LEASE_WINDOW_MISSING")]
    LeaseWindowMissing,
    #[error("LW_RESOURCE_LEASE_NOT_FOUND")]
    LeaseNotFound,
    #[error("LW_RESOURCE_TRACE_INVALID")]
    Trace,
    #[error("LW_RESOURCE_WIRE_INVALID")]
    Wire,
    #[error("LW_RESOURCE_NUMERIC_OVERFLOW")]
    Numeric(#[from] std::num::TryFromIntError),
    #[error("LW_RESOURCE_CONTRACT_INVALID: {0}")]
    Contract(#[from] contracts::resource::ResourceError),
    #[error("LW_RESOURCE_LIFECYCLE_FAILED: {0}")]
    Lifecycle(#[from] LifecycleError),
    #[error("LW_RESOURCE_FOUNDATION_INVALID: {0}")]
    Foundation(#[from] contracts::foundation::FoundationError),
    #[error("LW_RESOURCE_PERSISTENCE_FAILED: {0}")]
    Persistence(#[from] PersistenceError),
    #[error("LW_RESOURCE_DATABASE_FAILED")]
    Database(#[from] sqlx::Error),
    #[error("LW_RESOURCE_SERIALIZATION_FAILED")]
    Serialization(#[from] serde_json::Error),
}
