//! PostgreSQL authority for Resource requests and administrator decisions.

#![allow(
    clippy::doc_markdown,
    reason = "SQL identifiers and protocol terms are intentionally preserved in persistence docs"
)]

use contracts::environment::{
    EnvironmentLeaseAuthorization, EnvironmentLeaseState, EnvironmentLeaseVerificationRequest,
    EnvironmentLeaseVerificationResponse,
};
use contracts::events::{
    CloudEvent, EVENT_CONTRACTS, ResourceLeaseChanged, ResourceRequestChanged, subjects,
};
use contracts::http::{
    CreateResourceRateRequest, IdempotencyKey, RecordResourceUsageRequest, TaskResourceStatus,
    UpsertResourceBudgetRequest,
};
use contracts::resource::{
    CapacityClaim, CapacityClaimState, FixedDecimal, GpuAllocation, GpuAllocationMode,
    GpuCatalogEntry, Money, ResourceApproval, ResourceBillingUnit, ResourceBudget, ResourceCharge,
    ResourceChargeLine, ResourceLease, ResourceLeaseState, ResourceRate, ResourceRequest,
    ResourceRequestState, ResourceTarget, ResourceUsageKind, ResourceUsageRecord, UsageMeasurement,
    UsageSettlementState,
};
use contracts::{
    BudgetId, ChargeId, EventId, GpuCatalogEntryId, LeaseId, ProjectId, RateId, ResourceRequestId,
    Revision, Sequence, TaskRunId, UsageRecordId, UtcTimestamp,
};
use persistence_sqlx::Sha256Digest; // internal persistence hash, not contract hash
use persistence_sqlx::{
    Domain, IdempotencyDecision, IdempotencyStore, OutboxStore, PersistenceError,
};
use rust_decimal::{Decimal, RoundingStrategy};
use serde_json::Value;
use sqlx::{PgPool, Postgres, Row, Transaction};
use std::cmp::{max, min};
use std::str::FromStr;

use crate::{ApprovalPolicy, LifecycleError, ResourceLifecycle};

const REQUEST_SUBMITTED_SUBJECT: &str = subjects::RESOURCE_REQUEST_SUBMITTED;
const REQUEST_APPROVED_SUBJECT: &str = subjects::RESOURCE_REQUEST_APPROVED;
const REQUEST_REJECTED_SUBJECT: &str = subjects::RESOURCE_REQUEST_REJECTED;
const REQUEST_CANCELLED_SUBJECT: &str = subjects::RESOURCE_REQUEST_CANCELLED;
const REQUEST_STATE_CHANGED_SUBJECT: &str = subjects::RESOURCE_REQUEST_STATE_CHANGED;
const LEASE_ACTIVATED_SUBJECT: &str = subjects::RESOURCE_LEASE_ACTIVATED;
const LEASE_RENEWED_SUBJECT: &str = subjects::RESOURCE_LEASE_RENEWED;
const LEASE_REVOKED_SUBJECT: &str = subjects::RESOURCE_LEASE_REVOKED;
const LEASE_EXPIRING_SUBJECT: &str = subjects::RESOURCE_LEASE_EXPIRING;
const LEASE_EXPIRED_SUBJECT: &str = subjects::RESOURCE_LEASE_EXPIRED;
const GPU_ADMISSION_LOCK: &str = "gpu-admission";

/// Deterministic capacity plan created before provider side effects are scheduled.
#[derive(Clone, Debug)]
pub struct PendingAllocation {
    pub claim: CapacityClaim,
    pub lease_id: LeaseId,
}

/// A capacity shell exclusively leased to one reconciler attempt.
#[derive(Clone, Debug)]
pub struct ProvisioningCapacityClaim {
    pub claim: CapacityClaim,
    pub request: ResourceRequest,
    pub lease: ResourceLease,
    /// The last Lease revision acknowledged by the Environment owner.
    ///
    /// PostgreSQL stores zero until the handoff or a later lease sync has been
    /// acknowledged. Zero is represented as `None` so callers cannot put it
    /// on a cleanup request and accidentally treat it as a valid fence.
    pub lease_synced_revision: Option<Revision>,
}

/// A GPU reservation that is still authoritative in Resource.
///
/// The capacity observer uses this projection to subtract only pods that can be
/// tied to the exact durable target, namespace, and GPU allocation. Labels alone
/// are never sufficient to classify provider occupancy as Resource-owned.
#[derive(Clone, Debug)]
pub(crate) struct ActiveGpuReservation {
    pub claim_id: contracts::CapacityClaimId,
    pub entry_id: GpuCatalogEntryId,
    pub units: u32,
    pub allocation_binding: String,
    pub namespace_name: Option<String>,
    pub target: ResourceTarget,
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
            || self.claim.workload_resources != approval.approved_resources
            || self.claim.state != CapacityClaimState::Reserved
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

struct UsageAuthority<'a> {
    caller: &'a auth::ServiceIdentity,
    environment_service_client_id: &'a str,
    evaluation_service_client_id: &'a str,
}

impl PgResourceStore {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub(crate) fn pool(&self) -> PgPool {
        self.pool.clone()
    }

    /// Exposes the same pool-backed authority to the HTTP boundary; no second repository is
    /// created, so HTTP and background workers share the transaction and migration checks.
    #[must_use]
    pub fn for_http(&self) -> Self {
        self.clone()
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
        // The response identity and timestamps are server-owned.  Hash only the
        // caller's intent so a retry with the same key can replay the first
        // durable request even when the client rebuilt its JSON object.
        let hash = Sha256Digest::of_canonical(&(
            request.requester_id,
            request.project_id,
            request.course_id,
            &request.request_key,
            &request.target,
            &request.requested_resources,
            request.requested_duration_seconds,
        ))
        .map_err(|_| ResourceStoreError::Wire)?;
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

    /// Loads the request projection for an Evaluation-owned one-shot task before a capacity
    /// claim exists. The indexed target columns are checked against the decoded contract so a
    /// malformed or cross-target snapshot cannot be returned as task state.
    pub async fn load_task_request(
        &self,
        task_run_id: TaskRunId,
    ) -> Result<ResourceRequest, ResourceStoreError> {
        let rows = sqlx::query(
            "SELECT contract FROM resource.resource_requests
             WHERE task_run_id=$1 AND target_kind='task'
             ORDER BY created_at, request_id",
        )
        .bind(task_run_id.as_uuid())
        .fetch_all(&self.pool)
        .await?;
        if rows.len() > 1 {
            return Err(ResourceStoreError::CapacityReadbackInvalid);
        }
        let row = rows
            .into_iter()
            .next()
            .ok_or(ResourceStoreError::NotFound)?;
        let request = decode_request(row.try_get("contract")?)?;
        if !matches!(
            request.target,
            ResourceTarget::Task { task_run_id: id } if id == task_run_id
        ) {
            return Err(ResourceStoreError::ScopeConflict);
        }
        Ok(request)
    }

    /// Lists every request for a platform administrator. The HTTP boundary must enforce the
    /// global administrator policy before invoking this method.
    pub async fn list_all_requests(&self) -> Result<Vec<ResourceRequest>, ResourceStoreError> {
        let rows = sqlx::query(
            "SELECT contract FROM resource.resource_requests \
             ORDER BY created_at, request_id",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| decode_request(row.try_get("contract")?))
            .collect()
    }

    /// Lists every lease for a platform administrator. The HTTP boundary must enforce the
    /// global administrator policy before invoking this method.
    pub async fn list_all_leases(&self) -> Result<Vec<ResourceLease>, ResourceStoreError> {
        let rows = sqlx::query(
            "SELECT contract FROM resource.resource_leases \
             ORDER BY created_at, lease_id",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| decode_lease(row.try_get("contract")?))
            .collect()
    }

    /// Lists only requests owned by the authenticated actor. The database predicate is part of
    /// the authority boundary; callers cannot fetch another actor's projections by filtering the
    /// returned JSON in memory.
    pub async fn list_owned(
        &self,
        actor_id: contracts::ActorId,
        project_id: ProjectId,
        course_id: Option<contracts::CourseId>,
    ) -> Result<Vec<ResourceRequest>, ResourceStoreError> {
        let rows = sqlx::query(
            "SELECT contract FROM resource.resource_requests \
             WHERE requester_id=$1 AND project_id=$2 \
               AND ($3::uuid IS NULL OR course_id=$3) \
             ORDER BY created_at, request_id",
        )
        .bind(actor_id.as_uuid())
        .bind(project_id.as_uuid())
        .bind(course_id.map(contracts::CourseId::as_uuid))
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| decode_request(row.try_get("contract")?))
            .collect()
    }

    /// Lists all requests in one course for an Access-authorized platform
    /// administrator. The caller must enforce the course authorization before
    /// invoking this method.
    pub async fn list_for_project(
        &self,
        project_id: ProjectId,
        course_id: Option<contracts::CourseId>,
    ) -> Result<Vec<ResourceRequest>, ResourceStoreError> {
        let rows = sqlx::query(
            "SELECT contract FROM resource.resource_requests \
             WHERE project_id=$1 AND ($2::uuid IS NULL OR course_id=$2) \
             ORDER BY created_at, request_id",
        )
        .bind(project_id.as_uuid())
        .bind(course_id.map(contracts::CourseId::as_uuid))
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| decode_request(row.try_get("contract")?))
            .collect()
    }

    /// Lists leases owned by one actor in one course without in-memory scope
    /// filtering.
    pub async fn list_owned_leases(
        &self,
        actor_id: contracts::ActorId,
        project_id: ProjectId,
        course_id: Option<contracts::CourseId>,
    ) -> Result<Vec<ResourceLease>, ResourceStoreError> {
        let rows = sqlx::query(
            "SELECT l.contract FROM resource.resource_leases l \
             JOIN resource.resource_requests r ON r.request_id=l.request_id \
             WHERE r.project_id=$1 AND r.requester_id=$2 \
               AND ($3::uuid IS NULL OR r.course_id=$3) \
             ORDER BY l.created_at,l.lease_id",
        )
        .bind(project_id.as_uuid())
        .bind(actor_id.as_uuid())
        .bind(course_id.map(contracts::CourseId::as_uuid))
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| decode_lease(row.try_get("contract")?))
            .collect()
    }

    /// Lists all leases in one course for an Access-authorized administrator.
    pub async fn list_leases_for_project(
        &self,
        project_id: ProjectId,
        course_id: Option<contracts::CourseId>,
    ) -> Result<Vec<ResourceLease>, ResourceStoreError> {
        let rows = sqlx::query(
            "SELECT l.contract FROM resource.resource_leases l \
             JOIN resource.resource_requests r ON r.request_id=l.request_id \
             WHERE r.project_id=$1 AND ($2::uuid IS NULL OR r.course_id=$2) \
             ORDER BY l.created_at,l.lease_id",
        )
        .bind(project_id.as_uuid())
        .bind(course_id.map(contracts::CourseId::as_uuid))
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| decode_lease(row.try_get("contract")?))
            .collect()
    }

    /// Records one immutable provider meter interval.
    ///
    /// The request row is locked before checking the interval fence. This serializes retries
    /// for one resource request while allowing compute and retained-storage meters to overlap.
    /// A transport retry with the same source event is an exact replay; a new event that overlaps
    /// the same request and meter kind is rejected instead of producing a duplicate charge.
    pub async fn record_usage(
        &self,
        input: &RecordResourceUsageRequest,
        observed_at: UtcTimestamp,
    ) -> Result<ResourceUsageRecord, ResourceStoreError> {
        self.record_usage_with_authority(input, observed_at, None)
            .await
    }

    /// Records usage submitted by an authenticated internal meter or Evaluation owner.
    ///
    /// Internal delivery must name a live Resource lease. The request, project, target kind, and
    /// lease relationship are read from PostgreSQL while the request row is locked, so a trusted
    /// service cannot manufacture a project or target scope in its JSON payload.
    pub async fn record_usage_internal(
        &self,
        input: &RecordResourceUsageRequest,
        observed_at: UtcTimestamp,
        caller: &auth::ServiceIdentity,
        environment_service_client_id: &str,
        evaluation_service_client_id: &str,
    ) -> Result<ResourceUsageRecord, ResourceStoreError> {
        let authority = UsageAuthority {
            caller,
            environment_service_client_id,
            evaluation_service_client_id,
        };
        self.record_usage_with_authority(input, observed_at, Some(&authority))
            .await
    }

    #[allow(clippy::too_many_lines)]
    async fn record_usage_with_authority(
        &self,
        input: &RecordResourceUsageRequest,
        observed_at: UtcTimestamp,
        authority: Option<&UsageAuthority<'_>>,
    ) -> Result<ResourceUsageRecord, ResourceStoreError> {
        let settlement = match input.measurement {
            UsageMeasurement::Known { .. } => UsageSettlementState::Pending,
            UsageMeasurement::Unknown { .. } => UsageSettlementState::Unsettled,
        };
        let usage = ResourceUsageRecord {
            id: UsageRecordId::new(),
            project_id: input.project_id,
            course_id: input.course_id,
            kind: input.kind,
            request_id: input.request_id,
            lease_id: input.lease_id,
            source_event_id: input.source_event_id,
            measured_from: input.measured_from,
            measured_until: input.measured_until,
            measurement: input.measurement.clone(),
            settlement,
            observed_at,
        };
        usage.validate().map_err(ResourceStoreError::Contract)?;

        let mut transaction = self.pool.begin().await?;
        let request_scope = sqlx::query(
            "SELECT project_id, course_id, target_kind, task_run_id
             FROM resource.resource_requests
              WHERE request_id=$1 FOR UPDATE",
        )
        .bind(usage.request_id.as_uuid())
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or(ResourceStoreError::NotFound)?;
        let request_project: uuid::Uuid = request_scope.try_get("project_id")?;
        let request_course: Option<uuid::Uuid> = request_scope.try_get("course_id")?;
        let target_kind: String = request_scope.try_get("target_kind")?;
        let task_run_id: Option<uuid::Uuid> = request_scope.try_get("task_run_id")?;
        if request_project != usage.project_id.as_uuid()
            || request_course != usage.course_id.map(contracts::CourseId::as_uuid)
        {
            return Err(ResourceStoreError::ScopeConflict);
        }
        if let Some(authority) = authority {
            let expected_client_id = match target_kind.as_str() {
                "environment" => authority.environment_service_client_id,
                "task" => authority.evaluation_service_client_id,
                _ => return Err(ResourceStoreError::ScopeConflict),
            };
            if authority.caller.client_id != expected_client_id {
                tracing::warn!(
                    event = "resource.usage.authority_mismatch",
                    request_id = %usage.request_id,
                    target_kind,
                    caller_client_id = authority.caller.client_id,
                    diagnostic_code = "LW_AUTH_USAGE_OWNER_MISMATCH",
                );
                return Err(ResourceStoreError::ScopeConflict);
            }
            if usage.lease_id.is_none()
                || !matches!(target_kind.as_str(), "environment" | "task")
                || (target_kind == "task" && task_run_id.is_none())
                || (target_kind == "environment" && task_run_id.is_some())
            {
                return Err(ResourceStoreError::ScopeConflict);
            }
        }
        if let Some(lease_id) = usage.lease_id {
            let lease_row = sqlx::query(
                "SELECT request_id, state FROM resource.resource_leases WHERE lease_id=$1",
            )
            .bind(lease_id.as_uuid())
            .fetch_optional(&mut *transaction)
            .await?
            .ok_or(ResourceStoreError::LeaseNotFound)?;
            let lease_request: uuid::Uuid = lease_row.try_get("request_id")?;
            let lease_state: String = lease_row.try_get("state")?;
            if lease_request != usage.request_id.as_uuid() {
                return Err(ResourceStoreError::ScopeConflict);
            }
            if authority.is_some()
                && !matches!(
                    lease_state.as_str(),
                    "active" | "expiring" | "expired" | "revoked"
                )
            {
                return Err(ResourceStoreError::ScopeConflict);
            }
        }

        if let Some(row) = sqlx::query(
            "SELECT contract FROM resource.resource_usage_records WHERE source_event_id=$1",
        )
        .bind(usage.source_event_id.as_uuid())
        .fetch_optional(&mut *transaction)
        .await?
        {
            let existing = decode_usage(row.try_get("contract")?)?;
            if same_usage_intent(&existing, &usage) {
                transaction.commit().await?;
                return Ok(existing);
            }
            return Err(ResourceStoreError::UsageConflict);
        }

        let kind = wire(usage.kind)?;
        let overlaps: bool = sqlx::query_scalar(
            "SELECT EXISTS(
                 SELECT 1 FROM resource.resource_usage_records
                 WHERE request_id=$1 AND kind=$2
                   AND measured_from < $4 AND measured_until > $3
             )",
        )
        .bind(usage.request_id.as_uuid())
        .bind(&kind)
        .bind(usage.measured_from.get())
        .bind(usage.measured_until.get())
        .fetch_one(&mut *transaction)
        .await?;
        if overlaps {
            return Err(ResourceStoreError::UsageOverlap);
        }

        let (measurement_state, cpu, memory, storage, gpu, unknown_reason) =
            match &usage.measurement {
                UsageMeasurement::Known { quantities } => (
                    "known",
                    Some(quantities.cpu_millicore_seconds.to_string()),
                    Some(quantities.memory_byte_seconds.to_string()),
                    Some(quantities.storage_byte_seconds.to_string()),
                    Some(quantities.gpu_unit_seconds.to_string()),
                    None,
                ),
                UsageMeasurement::Unknown { reason } => {
                    ("unknown", None, None, None, None, Some(reason.as_str()))
                }
            };
        sqlx::query(
            "INSERT INTO resource.resource_usage_records
             (usage_record_id,project_id,course_id,request_id,lease_id,source_event_id,kind,
              measured_from,measured_until,measurement_state,cpu_millicore_seconds,
              memory_byte_seconds,storage_byte_seconds,gpu_unit_seconds,unknown_reason,
              settlement,observed_at,contract)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11::numeric,$12::numeric,$13::numeric,
                     $14::numeric,$15,$16,$17,$18)",
        )
        .bind(usage.id.as_uuid())
        .bind(usage.project_id.as_uuid())
        .bind(usage.course_id.map(contracts::CourseId::as_uuid))
        .bind(usage.request_id.as_uuid())
        .bind(usage.lease_id.map(LeaseId::as_uuid))
        .bind(usage.source_event_id.as_uuid())
        .bind(kind)
        .bind(usage.measured_from.get())
        .bind(usage.measured_until.get())
        .bind(measurement_state)
        .bind(cpu)
        .bind(memory)
        .bind(storage)
        .bind(gpu)
        .bind(unknown_reason)
        .bind(wire(usage.settlement)?)
        .bind(usage.observed_at.get())
        .bind(serde_json::to_value(&usage)?)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(usage)
    }

    /// Attempts one durable pending settlement. Missing rate configuration leaves the usage row
    /// pending so a later rate publication can settle it; no zero-value charge is synthesized.
    pub async fn settle_pending_usage_once(&self) -> Result<bool, ResourceStoreError> {
        let rows = sqlx::query(
            "SELECT usage_record_id, contract
             FROM resource.resource_usage_records
             WHERE settlement='pending' AND settlement_next_attempt_at <= clock_timestamp()
             ORDER BY observed_at, usage_record_id
             LIMIT 16",
        )
        .fetch_all(&self.pool)
        .await?;
        if rows.is_empty() {
            return Ok(false);
        }
        let mut did_work = false;
        for row in rows {
            let usage_record_id: uuid::Uuid = row.try_get("usage_record_id")?;
            let usage = match decode_usage(row.try_get("contract")?) {
                Ok(usage) => usage,
                Err(error) => {
                    self.mark_settlement_retry(
                        usage_record_id,
                        "LW_RESOURCE_SETTLEMENT_CONTRACT_INVALID",
                    )
                    .await?;
                    let safe_detail = resource_error_safe_detail(&error);
                    tracing::warn!(
                        resource_id = %usage_record_id,
                        event = "resource.settlement.contract_invalid",
                        diagnostic_code = "LW_RESOURCE_SETTLEMENT_CONTRACT_INVALID",
                        error_kind = resource_error_kind(&error),
                        failure_stage = "settlement.decode_usage",
                        safe_detail = safe_detail.as_str(),
                        retryable = false,
                    );
                    did_work = true;
                    continue;
                }
            };
            match self.settle_usage(usage.id).await {
                Ok(_) => did_work = true,
                Err(error) => {
                    let diagnostic = if matches!(error, ResourceStoreError::RateUnconfigured) {
                        "LW_RESOURCE_RATE_UNCONFIGURED"
                    } else {
                        "LW_RESOURCE_SETTLEMENT_RETRY"
                    };
                    self.mark_settlement_retry(usage_record_id, diagnostic)
                        .await?;
                    let safe_detail = resource_error_safe_detail(&error);
                    tracing::warn!(
                        resource_id = %usage.id,
                        event = "resource.settlement.failed",
                        diagnostic_code = diagnostic,
                        error_kind = resource_error_kind(&error),
                        failure_stage = "settlement.calculate_charge",
                        safe_detail = safe_detail.as_str(),
                        retryable = true,
                    );
                    did_work = true;
                }
            }
        }
        Ok(did_work)
    }

    async fn mark_settlement_retry(
        &self,
        usage_record_id: uuid::Uuid,
        diagnostic_code: &str,
    ) -> Result<(), ResourceStoreError> {
        // The retry delay is capped before the numeric value is cast to integer. The attempt
        // counter saturates at INT_MAX because it is durable metadata, while the delay remains
        // unchanged after the exponent reaches six and the 60-second cap.
        sqlx::query(
            "UPDATE resource.resource_usage_records
             SET settlement_attempts=LEAST(settlement_attempts, 2147483646) + 1,
                 settlement_next_attempt_at=clock_timestamp()
                   + make_interval(secs => LEAST(60, power(2, LEAST(settlement_attempts, 6)))::integer),
                 settlement_diagnostic_code=$2
             WHERE usage_record_id=$1 AND settlement='pending'",
        )
        .bind(usage_record_id)
        .bind(diagnostic_code)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Settles one known usage observation against the rate windows that cover
    /// its complete interval.  The usage row, charge uniqueness check, and
    /// project spend update share one transaction, so a worker retry cannot
    /// create a second base charge or advance spend twice.  Unknown provider
    /// measurements remain explicitly unsettled and produce no charge.
    pub async fn settle_usage(
        &self,
        usage_id: UsageRecordId,
    ) -> Result<Option<ResourceCharge>, ResourceStoreError> {
        let mut transaction = self.pool.begin().await?;
        let row = sqlx::query(
            "SELECT contract FROM resource.resource_usage_records
             WHERE usage_record_id=$1 FOR UPDATE",
        )
        .bind(usage_id.as_uuid())
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or(ResourceStoreError::UsageNotFound)?;
        let usage = decode_usage(row.try_get("contract")?)?;

        if let Some(row) = sqlx::query(
            "SELECT contract FROM resource.resource_charges
             WHERE usage_record_id=$1 AND adjustment_of IS NULL
             ORDER BY created_at, charge_id LIMIT 1",
        )
        .bind(usage.id.as_uuid())
        .fetch_optional(&mut *transaction)
        .await?
        {
            let charge = decode_charge(row.try_get("contract")?)?;
            if usage.settlement != UsageSettlementState::Settled {
                update_usage_settlement(&mut transaction, &usage, UsageSettlementState::Settled)
                    .await?;
            }
            transaction.commit().await?;
            return Ok(Some(charge));
        }

        if matches!(usage.measurement, UsageMeasurement::Unknown { .. }) {
            transaction.commit().await?;
            return Ok(None);
        }

        let Some(mut charge) = calculate_charge(&mut transaction, &usage).await? else {
            update_usage_settlement(&mut transaction, &usage, UsageSettlementState::Settled)
                .await?;
            transaction.commit().await?;
            return Ok(None);
        };
        charge.created_at = database_now(&mut transaction).await?;
        charge.validate().map_err(ResourceStoreError::Contract)?;
        insert_charge(&mut transaction, &charge).await?;
        update_usage_settlement(&mut transaction, &usage, UsageSettlementState::Settled).await?;
        apply_budget_delta(&mut transaction, usage.project_id, &charge.total).await?;
        transaction.commit().await?;
        Ok(Some(charge))
    }

    /// Lists every immutable rate version, newest revisions first.
    pub async fn list_rates(&self) -> Result<Vec<ResourceRate>, ResourceStoreError> {
        let rows = sqlx::query(
            "SELECT contract FROM resource.resource_rates
             ORDER BY effective_from DESC, revision DESC, rate_id",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| decode_rate(row.try_get("contract")?))
            .collect()
    }

    /// Creates one immutable rate version. Overlapping effective windows for the same dimension
    /// are rejected so a charge never depends on an arbitrary row ordering.
    #[allow(clippy::too_many_lines)]
    pub async fn create_rate(
        &self,
        idempotency_key: &str,
        input: &CreateResourceRateRequest,
    ) -> Result<ResourceRate, ResourceStoreError> {
        input.unit_price.validate()?;
        let gpu_unit = input.unit == ResourceBillingUnit::GpuUnitSecond;
        if gpu_unit != (input.gpu_class.is_some() && input.gpu_mode.is_some())
            || input.gpu_class.is_some() != input.gpu_mode.is_some()
        {
            return Err(ResourceStoreError::RateDimensionInvalid);
        }
        if input
            .effective_until
            .is_some_and(|until| until <= input.effective_from)
        {
            return Err(ResourceStoreError::Contract(
                contracts::resource::ResourceError::InvalidRate,
            ));
        }
        IdempotencyKey::parse(idempotency_key).map_err(|_| ResourceStoreError::IdempotencyKey)?;
        let hash = Sha256Digest::of_canonical(input).map_err(|_| ResourceStoreError::Wire)?;
        let mut transaction = self.pool.begin().await?;
        let result = match IdempotencyStore::reserve(
            &mut transaction,
            Domain::Resource,
            "create_resource_rate",
            idempotency_key,
            hash,
        )
        .await?
        {
            IdempotencyDecision::Replay(value) => decode_rate(value)?,
            IdempotencyDecision::Conflict => return Err(ResourceStoreError::IdempotencyConflict),
            IdempotencyDecision::InProgress => {
                return Err(ResourceStoreError::IdempotencyInProgress);
            }
            IdempotencyDecision::Reserved => {
                // Serialize revision assignment and interval checks for one
                // rate dimension before creating the server-owned rate ID.
                sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('resource_rates', 0))")
                    .execute(&mut *transaction)
                    .await?;
                let unit = wire(input.unit)?;
                let gpu_mode = input.gpu_mode.map(wire).transpose()?;
                let revision_i64: i64 = sqlx::query_scalar(
                    "SELECT COALESCE(MAX(revision),0)::bigint + 1
                     FROM resource.resource_rates
                     WHERE unit=$1 AND gpu_class IS NOT DISTINCT FROM $2
                       AND gpu_mode IS NOT DISTINCT FROM $3",
                )
                .bind(&unit)
                .bind(input.gpu_class.as_deref())
                .bind(gpu_mode.as_deref())
                .fetch_one(&mut *transaction)
                .await?;
                let revision = Revision::new(u64::try_from(revision_i64)?)?;
                // An open rate is the current version for this dimension. Publishing a later
                // version closes that interval at the successor boundary. This is serialized
                // with the insert so a meter never observes two open prices for one dimension.
                let previous_open = sqlx::query(
                    "SELECT contract
                     FROM resource.resource_rates
                     WHERE unit=$1 AND gpu_class IS NOT DISTINCT FROM $2
                       AND gpu_mode IS NOT DISTINCT FROM $3
                       AND effective_from < $4 AND effective_until IS NULL
                     ORDER BY effective_from DESC, revision DESC
                     LIMIT 1 FOR UPDATE",
                )
                .bind(&unit)
                .bind(input.gpu_class.as_deref())
                .bind(gpu_mode.as_deref())
                .bind(input.effective_from.get())
                .fetch_optional(&mut *transaction)
                .await?;
                let previous_open = previous_open
                    .map(|row| decode_rate(row.try_get("contract")?))
                    .transpose()?;
                if let Some(previous) = &previous_open {
                    let settled_after_boundary: bool = sqlx::query_scalar(
                        "SELECT EXISTS(
                             SELECT 1
                             FROM resource.resource_usage_records u
                             JOIN resource.resource_charges c
                               ON c.usage_record_id=u.usage_record_id
                              AND c.adjustment_of IS NULL
                             WHERE u.settlement='settled'
                               AND u.measured_until > $1
                               AND EXISTS (
                                   SELECT 1
                                   FROM jsonb_array_elements(c.lines) line
                                   WHERE line->>'rateId'=$2
                               )
                         )",
                    )
                    .bind(input.effective_from.get())
                    .bind(previous.id.to_string())
                    .fetch_one(&mut *transaction)
                    .await?;
                    if settled_after_boundary {
                        return Err(ResourceStoreError::RateSettledConflict);
                    }
                }
                let overlap: bool = sqlx::query_scalar(
                    "SELECT EXISTS(
                         SELECT 1 FROM resource.resource_rates
                         WHERE unit=$1 AND gpu_class IS NOT DISTINCT FROM $2
                           AND gpu_mode IS NOT DISTINCT FROM $3
                            AND effective_from < COALESCE($5, 'infinity'::timestamptz)
                            AND COALESCE(effective_until, 'infinity'::timestamptz) > $4
                            AND ($6::uuid IS NULL OR rate_id <> $6)
                      )",
                )
                .bind(&unit)
                .bind(input.gpu_class.as_deref())
                .bind(gpu_mode.as_deref())
                .bind(input.effective_from.get())
                .bind(input.effective_until.map(UtcTimestamp::get))
                .bind(previous_open.as_ref().map(|rate| rate.id.as_uuid()))
                .fetch_one(&mut *transaction)
                .await?;
                if overlap {
                    return Err(ResourceStoreError::RateOverlap);
                }
                if let Some(previous) = previous_open {
                    let mut closed = previous.clone();
                    closed.effective_until = Some(input.effective_from);
                    closed.validate().map_err(ResourceStoreError::Contract)?;
                    sqlx::query(
                        "UPDATE resource.resource_rates
                         SET effective_until=$2, contract=$3
                         WHERE rate_id=$1 AND effective_until IS NULL",
                    )
                    .bind(closed.id.as_uuid())
                    .bind(input.effective_from.get())
                    .bind(serde_json::to_value(&closed)?)
                    .execute(&mut *transaction)
                    .await?;
                }
                let rate = ResourceRate {
                    id: RateId::new(),
                    revision,
                    unit: input.unit,
                    unit_quantity: input.unit_quantity,
                    gpu_class: input.gpu_class.clone(),
                    gpu_mode: input.gpu_mode,
                    unit_price: input.unit_price.clone(),
                    effective_from: input.effective_from,
                    effective_until: input.effective_until,
                };
                rate.validate().map_err(ResourceStoreError::Contract)?;
                sqlx::query(
                    "INSERT INTO resource.resource_rates
                     (rate_id,revision,unit,unit_quantity,gpu_class,gpu_mode,currency,unit_price,effective_from,effective_until,contract)
                     VALUES ($1,$2,$3,$4::numeric,$5,$6,$7,$8,$9,$10,$11)",
                )
                .bind(rate.id.as_uuid())
                .bind(i64::try_from(rate.revision.get())?)
                .bind(unit)
                .bind(rate.unit_quantity.to_string())
                .bind(rate.gpu_class.as_deref())
                .bind(gpu_mode)
                .bind(&rate.unit_price.currency)
                .bind(rate.unit_price.amount.as_str())
                .bind(rate.effective_from.get())
                .bind(rate.effective_until.map(UtcTimestamp::get))
                .bind(serde_json::to_value(&rate)?)
                .execute(&mut *transaction)
                .await?;
                let value = serde_json::to_value(&rate)?;
                IdempotencyStore::complete(
                    &mut transaction,
                    Domain::Resource,
                    "create_resource_rate",
                    idempotency_key,
                    &value,
                )
                .await?;
                rate
            }
        };
        transaction.commit().await?;
        Ok(result)
    }

    /// Lists the active and inactive GPU catalog revisions.
    pub async fn list_gpu_catalog(&self) -> Result<Vec<GpuCatalogEntry>, ResourceStoreError> {
        let rows = sqlx::query(
            "SELECT contract FROM resource.gpu_catalog_entries
             ORDER BY class, mode, revision DESC, entry_id",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| decode_gpu_catalog(row.try_get("contract")?))
            .collect()
    }

    /// Lists the durable GPU reservations that still own capacity.
    ///
    /// A reservation remains authoritative until Resource commits its release. The
    /// query therefore includes the in-flight expiry/release states, while the
    /// contract and SQL projections are checked together before they are exposed to
    /// the provider observer. No provider object is treated as owned from a label
    /// without a matching row returned here.
    pub(crate) async fn list_active_gpu_reservations(
        &self,
    ) -> Result<Vec<ActiveGpuReservation>, ResourceStoreError> {
        let rows = sqlx::query(
            "SELECT r.contract AS request_contract,
                    c.contract AS claim_contract,
                    l.contract AS lease_contract,
                    c.namespace_name,
                    reservation.claim_id AS reservation_claim_id,
                    reservation.entry_id AS reservation_entry_id,
                    reservation.units AS reservation_units,
                    catalog.provider_binding AS catalog_provider_binding,
                    catalog.allocation_binding AS catalog_allocation_binding
             FROM resource.gpu_capacity_reservations reservation
             JOIN resource.capacity_claims c ON c.claim_id=reservation.claim_id
             JOIN resource.resource_requests r ON r.request_id=c.request_id
             JOIN resource.resource_leases l ON l.claim_id=c.claim_id
             JOIN resource.gpu_catalog_entries catalog ON catalog.entry_id=reservation.entry_id
             WHERE reservation.state='reserved'
               AND c.state IN ('reserved','provisioning','ready','handed_off','releasing','blocked')
               AND r.state IN ('allocating','active','expiring')
               AND l.state IN ('allocating','active','expiring')
             ORDER BY reservation.reservation_id",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                let request = decode_request(row.try_get("request_contract")?)?;
                let claim = decode_claim(row.try_get("claim_contract")?)?;
                let lease = decode_lease(row.try_get("lease_contract")?)?;
                let reservation_claim_id: uuid::Uuid = row.try_get("reservation_claim_id")?;
                let reservation_entry_id: uuid::Uuid = row.try_get("reservation_entry_id")?;
                let reservation_units: i32 = row.try_get("reservation_units")?;
                let provider_binding: String = row.try_get("catalog_provider_binding")?;
                let allocation_binding: String = row.try_get("catalog_allocation_binding")?;
                let namespace_name: Option<String> = row.try_get("namespace_name")?;
                let allocation = claim
                    .gpu_allocation
                    .as_ref()
                    .ok_or(ResourceStoreError::GpuAllocationMissing)?;
                let units = u32::try_from(reservation_units)?;
                if claim.id.as_uuid() != reservation_claim_id
                    || claim.request_id != request.id
                    || lease.request_id != request.id
                    || lease.claim_id != claim.id
                    || allocation.entry_id.as_uuid() != reservation_entry_id
                    || allocation.count != units
                    || allocation.provider_binding != provider_binding
                    || allocation.allocation_binding != allocation_binding
                    || namespace_name
                        .as_deref()
                        .is_some_and(|value| !valid_namespace_name(value))
                    || !matches!(
                        claim.state,
                        CapacityClaimState::Reserved
                            | CapacityClaimState::Provisioning
                            | CapacityClaimState::Ready
                            | CapacityClaimState::HandedOff
                            | CapacityClaimState::Releasing
                            | CapacityClaimState::Blocked
                    )
                    || !matches!(
                        request.state,
                        ResourceRequestState::Allocating
                            | ResourceRequestState::Active
                            | ResourceRequestState::Expiring
                    )
                    || !matches!(
                        lease.state,
                        ResourceLeaseState::Allocating
                            | ResourceLeaseState::Active
                            | ResourceLeaseState::Expiring
                    )
                {
                    return Err(ResourceStoreError::CapacityReadbackInvalid);
                }
                Ok(ActiveGpuReservation {
                    claim_id: claim.id,
                    entry_id: allocation.entry_id,
                    units,
                    allocation_binding,
                    namespace_name,
                    target: request.target,
                })
            })
            .collect()
    }

    /// Adds one immutable GPU catalog revision. Fresh capacity observations are recorded
    /// separately by the trusted provider meter; catalog capacity alone never grants a GPU.
    #[allow(clippy::too_many_lines)]
    pub async fn create_gpu_catalog_entry(
        &self,
        idempotency_key: &str,
        entry: &GpuCatalogEntry,
    ) -> Result<GpuCatalogEntry, ResourceStoreError> {
        entry.validate().map_err(ResourceStoreError::Contract)?;
        IdempotencyKey::parse(idempotency_key).map_err(|_| ResourceStoreError::IdempotencyKey)?;
        let hash = Sha256Digest::of_canonical(entry).map_err(|_| ResourceStoreError::Wire)?;
        let mut transaction = self.pool.begin().await?;
        let result = match IdempotencyStore::reserve(
            &mut transaction,
            Domain::Resource,
            "create_gpu_catalog_entry",
            idempotency_key,
            hash,
        )
        .await?
        {
            IdempotencyDecision::Replay(value) => decode_gpu_catalog(value)?,
            IdempotencyDecision::Conflict => return Err(ResourceStoreError::IdempotencyConflict),
            IdempotencyDecision::InProgress => {
                return Err(ResourceStoreError::IdempotencyInProgress);
            }
            IdempotencyDecision::Reserved => {
                lock_gpu_admission(&mut transaction).await?;
                let mode = wire(entry.mode)?;
                let current_revision: Option<i64> = sqlx::query_scalar(
                    "SELECT MAX(revision)::bigint FROM resource.gpu_catalog_entries
                     WHERE class=$1",
                )
                .bind(&entry.class)
                .fetch_one(&mut *transaction)
                .await?;
                if current_revision.is_some_and(|revision| {
                    u64::try_from(revision)
                        .ok()
                        .is_some_and(|revision| revision >= entry.revision.get())
                }) {
                    return Err(ResourceStoreError::GpuCatalogRevisionConflict);
                }
                let mode_collision: bool = sqlx::query_scalar(
                    "SELECT EXISTS(
                         SELECT 1
                         FROM resource.gpu_catalog_entries
                         WHERE active
                           AND allocation_binding=$1
                           AND class<>$2
                           AND mode<>$3
                      )",
                )
                .bind(&entry.allocation_binding)
                .bind(&entry.class)
                .bind(&mode)
                .fetch_one(&mut *transaction)
                .await?;
                if mode_collision {
                    return Err(ResourceStoreError::GpuCatalogModeCollision);
                }
                let pool_collision: bool = sqlx::query_scalar(
                    "SELECT EXISTS(
                         SELECT 1
                         FROM resource.gpu_catalog_entries
                         WHERE active
                           AND allocation_binding=$1
                           AND class<>$2
                           AND mode=$3
                     )",
                )
                .bind(&entry.allocation_binding)
                .bind(&entry.class)
                .bind(&mode)
                .fetch_one(&mut *transaction)
                .await?;
                if pool_collision {
                    return Err(ResourceStoreError::GpuCatalogPoolCollision);
                }
                let current_active = sqlx::query(
                    "SELECT mode,provider_binding,allocation_binding
                     FROM resource.gpu_catalog_entries
                     WHERE class=$1 AND active
                     FOR UPDATE",
                )
                .bind(&entry.class)
                .fetch_optional(&mut *transaction)
                .await?;
                if let Some(current_active) = current_active {
                    let current_mode: String = current_active.try_get("mode")?;
                    let current_provider: String = current_active.try_get("provider_binding")?;
                    let current_allocation: String =
                        current_active.try_get("allocation_binding")?;
                    let mapping_changed = current_mode != mode
                        || current_provider != entry.provider_binding
                        || current_allocation != entry.allocation_binding;
                    if mapping_changed {
                        let has_live_reservation: bool = sqlx::query_scalar(
                            "SELECT EXISTS(
                                 SELECT 1
                                 FROM resource.gpu_capacity_reservations reservation
                                 JOIN resource.gpu_catalog_entries catalog
                                   ON catalog.entry_id=reservation.entry_id
                                 WHERE catalog.class=$1
                                   AND reservation.state='reserved'
                             )",
                        )
                        .bind(&entry.class)
                        .fetch_one(&mut *transaction)
                        .await?;
                        if has_live_reservation {
                            return Err(ResourceStoreError::GpuCatalogMappingConflict);
                        }
                    }
                }
                if entry.active {
                    // The active class identifies one complete catalog product;
                    // changing its revision retires the previous product and
                    // keeps its durable JSON projection in sync with SQL.
                    sqlx::query(
                        "UPDATE resource.gpu_catalog_entries
                         SET active=false, updated_at=clock_timestamp(),
                             contract=jsonb_set(contract, '{active}', 'false'::jsonb, true)
                         WHERE class=$1 AND active",
                    )
                    .bind(&entry.class)
                    .execute(&mut *transaction)
                    .await?;
                }
                sqlx::query(
                    "INSERT INTO resource.gpu_catalog_entries
                      (entry_id,class,mode,provider_binding,capacity_units,allocation_binding,revision,active,contract)
                      VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)",
                )
                .bind(entry.id.as_uuid())
                .bind(&entry.class)
                .bind(&mode)
                .bind(&entry.provider_binding)
                .bind(i32::try_from(entry.capacity_units)?)
                .bind(&entry.allocation_binding)
                .bind(i64::try_from(entry.revision.get())?)
                .bind(entry.active)
                .bind(serde_json::to_value(entry)?)
                .execute(&mut *transaction)
                .await?;
                let value = serde_json::to_value(entry)?;
                IdempotencyStore::complete(
                    &mut transaction,
                    Domain::Resource,
                    "create_gpu_catalog_entry",
                    idempotency_key,
                    &value,
                )
                .await?;
                entry.clone()
            }
        };
        transaction.commit().await?;
        Ok(result)
    }

    /// Records capacity available to this platform before Resource's own durable reservations.
    /// The resolver subtracts each active reservation exactly once while holding the catalog row.
    pub async fn record_gpu_capacity_observation(
        &self,
        entry_id: GpuCatalogEntryId,
        available_units: u32,
        source_binding: &str,
        observed_at: UtcTimestamp,
        valid_until: UtcTimestamp,
    ) -> Result<(), ResourceStoreError> {
        if source_binding.trim().is_empty()
            || source_binding.len() > 256
            || valid_until <= observed_at
        {
            return Err(ResourceStoreError::GpuObservationInvalid);
        }
        let capacity: i64 = sqlx::query_scalar(
            "SELECT capacity_units::bigint FROM resource.gpu_catalog_entries WHERE entry_id=$1",
        )
        .bind(entry_id.as_uuid())
        .fetch_optional(&self.pool)
        .await?
        .ok_or(ResourceStoreError::GpuCatalogMissing)?;
        if i64::from(available_units) > capacity {
            return Err(ResourceStoreError::GpuObservationInvalid);
        }
        sqlx::query(
            "INSERT INTO resource.gpu_capacity_observations
             (observation_id,entry_id,available_units,source_binding,observed_at,valid_until)
             VALUES ($1,$2,$3,$4,$5,$6)
             ON CONFLICT (entry_id, observed_at) DO UPDATE
             SET available_units=EXCLUDED.available_units,
                 source_binding=EXCLUDED.source_binding,
                 valid_until=EXCLUDED.valid_until",
        )
        .bind(uuid::Uuid::now_v7())
        .bind(entry_id.as_uuid())
        .bind(i32::try_from(available_units)?)
        .bind(source_binding)
        .bind(observed_at.get())
        .bind(valid_until.get())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Loads the single project budget, failing closed when no budget is configured.
    pub async fn get_budget(
        &self,
        project_id: ProjectId,
    ) -> Result<ResourceBudget, ResourceStoreError> {
        let row = sqlx::query("SELECT contract FROM resource.resource_budgets WHERE project_id=$1")
            .bind(project_id.as_uuid())
            .fetch_optional(&self.pool)
            .await?
            .ok_or(ResourceStoreError::BudgetNotFound)?;
        decode_budget(row.try_get("contract")?)
    }

    /// Upserts a project budget under a row lock and preserves calculated spend.
    #[allow(clippy::too_many_lines)]
    pub async fn upsert_budget(
        &self,
        idempotency_key: &str,
        input: &UpsertResourceBudgetRequest,
        now: UtcTimestamp,
    ) -> Result<ResourceBudget, ResourceStoreError> {
        input.limit.validate()?;
        input.warning_at.validate()?;
        if input.limit.currency != input.warning_at.currency
            || input.warning_at.amount.scaled() < 0
            || input.limit.amount.scaled() < 0
        {
            return Err(ResourceStoreError::Contract(
                contracts::resource::ResourceError::InvalidBudget,
            ));
        }
        IdempotencyKey::parse(idempotency_key).map_err(|_| ResourceStoreError::IdempotencyKey)?;
        let hash = Sha256Digest::of_canonical(input).map_err(|_| ResourceStoreError::Wire)?;
        let mut transaction = self.pool.begin().await?;
        let budget = match IdempotencyStore::reserve(
            &mut transaction,
            Domain::Resource,
            "upsert_resource_budget",
            idempotency_key,
            hash,
        )
        .await?
        {
            IdempotencyDecision::Replay(value) => decode_budget(value)?,
            IdempotencyDecision::Conflict => return Err(ResourceStoreError::IdempotencyConflict),
            IdempotencyDecision::InProgress => {
                return Err(ResourceStoreError::IdempotencyInProgress);
            }
            IdempotencyDecision::Reserved => {
                let existing = sqlx::query(
                    "SELECT contract FROM resource.resource_budgets WHERE project_id=$1 FOR UPDATE",
                )
                .bind(input.project_id.as_uuid())
                .fetch_optional(&mut *transaction)
                .await?;
                let budget = if let Some(row) = existing {
                    let current = decode_budget(row.try_get("contract")?)?;
                    let revision = Revision::new(
                        current
                            .revision
                            .get()
                            .checked_add(1)
                            .ok_or(ResourceStoreError::RevisionOverflow)?,
                    )?;
                    ResourceBudget {
                        id: current.id,
                        project_id: input.project_id,
                        course_id: input.course_id,
                        limit: input.limit.clone(),
                        warning_at: input.warning_at.clone(),
                        spent: current.spent,
                        revision,
                        updated_at: now,
                    }
                } else {
                    ResourceBudget {
                        id: BudgetId::new(),
                        project_id: input.project_id,
                        course_id: input.course_id,
                        limit: input.limit.clone(),
                        warning_at: input.warning_at.clone(),
                        spent: Money {
                            currency: input.limit.currency.clone(),
                            amount: contracts::resource::FixedDecimal::zero(),
                        },
                        revision: Revision::new(1)?,
                        updated_at: now,
                    }
                };
                budget.validate().map_err(ResourceStoreError::Contract)?;
                sqlx::query(
                    "INSERT INTO resource.resource_budgets
                     (budget_id,project_id,course_id,currency,limit_amount,warning_amount,spent_amount,
                      revision,updated_at,contract)
                     VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)
                     ON CONFLICT (project_id) DO UPDATE SET course_id=EXCLUDED.course_id,
                       currency=EXCLUDED.currency,limit_amount=EXCLUDED.limit_amount,
                       warning_amount=EXCLUDED.warning_amount,spent_amount=EXCLUDED.spent_amount,
                       revision=EXCLUDED.revision,updated_at=EXCLUDED.updated_at,contract=EXCLUDED.contract",
                )
                .bind(budget.id.as_uuid())
                .bind(budget.project_id.as_uuid())
                .bind(budget.course_id.map(contracts::CourseId::as_uuid))
                .bind(&budget.limit.currency)
                .bind(budget.limit.amount.as_str())
                .bind(budget.warning_at.amount.as_str())
                .bind(budget.spent.amount.as_str())
                .bind(i64::try_from(budget.revision.get())?)
                .bind(budget.updated_at.get())
                .bind(serde_json::to_value(&budget)?)
                .execute(&mut *transaction)
                .await?;
                let value = serde_json::to_value(&budget)?;
                IdempotencyStore::complete(
                    &mut transaction,
                    Domain::Resource,
                    "upsert_resource_budget",
                    idempotency_key,
                    &value,
                )
                .await?;
                budget
            }
        };
        transaction.commit().await?;
        Ok(budget)
    }

    /// Lists calculated charges for one project in creation order.
    pub async fn list_charges(
        &self,
        project_id: ProjectId,
    ) -> Result<Vec<ResourceCharge>, ResourceStoreError> {
        let rows = sqlx::query(
            "SELECT contract FROM resource.resource_charges
             WHERE project_id=$1 ORDER BY created_at, charge_id",
        )
        .bind(project_id.as_uuid())
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| decode_charge(row.try_get("contract")?))
            .collect()
    }

    /// Appends an administrator adjustment without mutating the original charge.
    #[allow(clippy::too_many_arguments)]
    pub async fn create_adjustment(
        &self,
        idempotency_key: &str,
        project_id: ProjectId,
        charge_id: ChargeId,
        amount: Money,
        reason: String,
        actor: contracts::ActorId,
        created_at: UtcTimestamp,
    ) -> Result<ResourceCharge, ResourceStoreError> {
        if reason.trim().is_empty() || reason.chars().count() > 500 {
            return Err(ResourceStoreError::AdjustmentInvalid);
        }
        amount.validate()?;
        IdempotencyKey::parse(idempotency_key).map_err(|_| ResourceStoreError::IdempotencyKey)?;
        let mut transaction = self.pool.begin().await?;
        let original_row = sqlx::query(
            "SELECT contract FROM resource.resource_charges
             WHERE charge_id=$1 AND project_id=$2 FOR UPDATE",
        )
        .bind(charge_id.as_uuid())
        .bind(project_id.as_uuid())
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or(ResourceStoreError::ChargeNotFound)?;
        let original = decode_charge(original_row.try_get("contract")?)?;
        if original.total.currency != amount.currency {
            return Err(ResourceStoreError::AdjustmentInvalid);
        }
        let original_line = original
            .lines
            .first()
            .ok_or(ResourceStoreError::AdjustmentInvalid)?;
        let hash = Sha256Digest::of_canonical(&(project_id, charge_id, &amount, &reason, actor))
            .map_err(|_| ResourceStoreError::Wire)?;
        let result = match IdempotencyStore::reserve(
            &mut transaction,
            Domain::Resource,
            "create_resource_charge_adjustment",
            idempotency_key,
            hash,
        )
        .await?
        {
            IdempotencyDecision::Replay(value) => decode_charge(value)?,
            IdempotencyDecision::Conflict => return Err(ResourceStoreError::IdempotencyConflict),
            IdempotencyDecision::InProgress => {
                return Err(ResourceStoreError::IdempotencyInProgress);
            }
            IdempotencyDecision::Reserved => {
                let adjustment = ResourceCharge {
                    id: ChargeId::new(),
                    usage_record_id: original.usage_record_id,
                    project_id,
                    course_id: original.course_id,
                    lines: vec![contracts::resource::ResourceChargeLine {
                        rate_id: original_line.rate_id,
                        rate_revision: original_line.rate_revision,
                        unit: original_line.unit,
                        quantity: 1,
                        unit_quantity: 1,
                        unit_price: amount.clone(),
                        amount: amount.clone(),
                    }],
                    total: amount.clone(),
                    settlement: UsageSettlementState::Settled,
                    created_at,
                    adjustment_of: Some(original.id),
                    adjustment_reason: Some(reason.clone()),
                    adjusted_by: Some(actor),
                    diagnostic_code: None,
                };
                adjustment
                    .validate()
                    .map_err(ResourceStoreError::Contract)?;
                insert_charge(&mut transaction, &adjustment).await?;
                apply_budget_delta(&mut transaction, project_id, &adjustment.total).await?;
                let value = serde_json::to_value(&adjustment)?;
                IdempotencyStore::complete(
                    &mut transaction,
                    Domain::Resource,
                    "create_resource_charge_adjustment",
                    idempotency_key,
                    &value,
                )
                .await?;
                adjustment
            }
        };
        transaction.commit().await?;
        Ok(result)
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
        let ResourceTarget::Environment { environment_id, .. } = request.target else {
            return Ok(inactive_lease_response(EnvironmentLeaseState::Revoked));
        };
        if lease.state != ResourceLeaseState::Active
            || lease
                .expires_at
                .is_none_or(|expires_at| expires_at <= authority_now)
            || verification.environment_id != environment_id
            || verification.project_id != request.project_id
            || verification.course_id != request.course_id
            || verification.owner_actor_id != request.requester_id
            || verification.capacity_binding != claim.id.to_string()
        {
            return Ok(inactive_lease_response(state));
        }
        let authorization = EnvironmentLeaseAuthorization {
            resource_request_id: request.id,
            lease_id: lease.id,
            lease_revision: lease.revision,
            environment_id,
            project_id: request.project_id,
            course_id: request.course_id,
            owner_actor_id: request.requester_id,
            capacity_binding: claim.id.to_string(),
            approved_resources: claim.workload_resources.clone(),
            gpu_allocation: claim.gpu_allocation.clone(),
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
        // Approval, claim, and lease IDs are generated for the resulting
        // records.  The idempotency fingerprint is the stable administrative
        // decision and selected allocation intent, so transport retries replay
        // the original response instead of conflicting on fresh UUIDs.
        let hash = Sha256Digest::of_canonical(&(
            request_id,
            request.project_id,
            request.course_id,
            approval.request_revision,
            approval.approver_id,
            &approval.provider_binding,
            &approval.approved_resources,
            approval.approved_duration_seconds,
            &approval.reason,
            &allocation.claim.quota_resources,
        ))
        .map_err(|_| ResourceStoreError::Wire)?;
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
                let mut claim = allocation.claim.clone();
                claim.gpu_allocation = resolve_gpu_allocation(
                    &mut transaction,
                    approval.provider_binding.as_str(),
                    approval.approved_resources.gpu.as_ref(),
                )
                .await?;
                let resolved_allocation = PendingAllocation {
                    claim,
                    lease_id: allocation.lease_id,
                };
                let lease = resolved_allocation.validate(&request, approval, now)?;
                insert_approval(&mut transaction, approval).await?;
                insert_claim(&mut transaction, &resolved_allocation.claim).await?;
                if let Some(gpu_allocation) = &resolved_allocation.claim.gpu_allocation {
                    insert_gpu_reservation(
                        &mut transaction,
                        resolved_allocation.claim.id,
                        gpu_allocation,
                    )
                    .await?;
                }
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

    /// Performs a revision-fenced terminal request mutation with an idempotent transaction.
    pub async fn reject_or_cancel(
        &self,
        idempotency_key: &str,
        request_id: ResourceRequestId,
        expected_revision: contracts::Revision,
        terminal: ResourceRequestState,
        actor: contracts::ActorId,
        trace_id: &str,
    ) -> Result<ResourceRequest, ResourceStoreError> {
        IdempotencyKey::parse(idempotency_key).map_err(|_| ResourceStoreError::IdempotencyKey)?;
        validate_trace(trace_id)?;
        if !matches!(
            terminal,
            ResourceRequestState::Rejected | ResourceRequestState::Cancelled
        ) {
            return Err(ResourceStoreError::InvalidCreateState);
        }
        let mut transaction = self.pool.begin().await?;
        let request = load_locked(&mut transaction, request_id).await?;
        let hash = Sha256Digest::of_canonical(&(
            request_id,
            request.project_id,
            request.course_id,
            expected_revision,
            terminal,
            actor,
        ))
        .map_err(|_| ResourceStoreError::Wire)?;
        let result = match IdempotencyStore::reserve(
            &mut transaction,
            Domain::Resource,
            "terminal_resource_request",
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
                let next = ResourceLifecycle::reject_or_cancel(
                    &request,
                    expected_revision,
                    terminal,
                    now,
                )?;
                update_request(&mut transaction, &request, &next).await?;
                insert_transition(
                    &mut transaction,
                    &next,
                    next.revision.get(),
                    Some(request.state),
                    Some(actor),
                    trace_id,
                )
                .await?;
                let subject = match terminal {
                    ResourceRequestState::Rejected => REQUEST_REJECTED_SUBJECT,
                    ResourceRequestState::Cancelled => REQUEST_CANCELLED_SUBJECT,
                    _ => unreachable!("terminal state was validated above"),
                };
                enqueue_request_event(&mut transaction, &next, subject, trace_id).await?;
                let value = serde_json::to_value(&next)?;
                IdempotencyStore::complete(
                    &mut transaction,
                    Domain::Resource,
                    "terminal_resource_request",
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

    pub async fn retry(
        &self,
        idempotency_key: &str,
        request_id: ResourceRequestId,
        expected_revision: contracts::Revision,
        actor: contracts::ActorId,
        trace_id: &str,
    ) -> Result<ResourceRequest, ResourceStoreError> {
        IdempotencyKey::parse(idempotency_key).map_err(|_| ResourceStoreError::IdempotencyKey)?;
        validate_trace(trace_id)?;
        let mut transaction = self.pool.begin().await?;
        let request = load_locked(&mut transaction, request_id).await?;
        let hash = Sha256Digest::of_canonical(&(
            request_id,
            request.project_id,
            request.course_id,
            expected_revision,
            actor,
            "retry",
        ))
        .map_err(|_| ResourceStoreError::Wire)?;
        let result = match IdempotencyStore::reserve(
            &mut transaction,
            Domain::Resource,
            "retry_resource_request",
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
                let next = ResourceLifecycle::retry(&request, expected_revision, now)?;
                update_request(&mut transaction, &request, &next).await?;
                insert_transition(
                    &mut transaction,
                    &next,
                    next.revision.get(),
                    Some(request.state),
                    Some(actor),
                    trace_id,
                )
                .await?;
                enqueue_request_event(
                    &mut transaction,
                    &next,
                    REQUEST_STATE_CHANGED_SUBJECT,
                    trace_id,
                )
                .await?;
                let value = serde_json::to_value(&next)?;
                IdempotencyStore::complete(
                    &mut transaction,
                    Domain::Resource,
                    "retry_resource_request",
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

    /// Claims the durable reservation for a one-shot Evaluation task.
    ///
    /// Resource workers only reconcile Environment targets. Evaluation must therefore claim its
    /// own Task reservation through this transaction before it can acknowledge the handoff. The
    /// task identity, request, claim, and lease are all checked while their rows are locked;
    /// retries after the first claim return the current state without advancing the revision.
    pub async fn claim_task_resource(
        &self,
        task_run_id: TaskRunId,
        trace_id: &str,
    ) -> Result<TaskResourceStatus, ResourceStoreError> {
        validate_trace(trace_id)?;
        let mut transaction = self.pool.begin().await?;
        let row = sqlx::query(
            "SELECT r.contract AS request_contract,c.contract AS claim_contract,
                    l.contract AS lease_contract,c.namespace_name AS execution_namespace
             FROM resource.resource_requests r
             JOIN resource.capacity_claims c ON c.request_id=r.request_id
             JOIN resource.resource_leases l ON l.claim_id=c.claim_id
             WHERE r.task_run_id=$1 AND r.target_kind='task'
             FOR UPDATE OF r,c,l",
        )
        .bind(task_run_id.as_uuid())
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or(ResourceStoreError::NotFound)?;
        let request = decode_request(row.try_get("request_contract")?)?;
        let claim = decode_claim(row.try_get("claim_contract")?)?;
        let lease = decode_lease(row.try_get("lease_contract")?)?;
        let execution_namespace: Option<String> = row.try_get("execution_namespace")?;
        if !matches!(request.target, ResourceTarget::Task { task_run_id: id } if id == task_run_id)
            || claim.request_id != request.id
            || lease.request_id != request.id
            || lease.claim_id != claim.id
        {
            return Err(ResourceStoreError::ScopeConflict);
        }
        let claim = match claim.state {
            CapacityClaimState::Reserved => {
                if lease.state != ResourceLeaseState::Allocating {
                    return Err(ResourceStoreError::CapacityClaimStateConflict);
                }
                let next = transition_claim(&claim, CapacityClaimState::Provisioning)?;
                update_claim(&mut transaction, &claim, &next, None, None, None).await?;
                next
            }
            CapacityClaimState::Provisioning
            | CapacityClaimState::Ready
            | CapacityClaimState::HandedOff => claim,
            CapacityClaimState::Releasing
            | CapacityClaimState::Released
            | CapacityClaimState::Blocked => {
                return Err(ResourceStoreError::CapacityClaimStateConflict);
            }
        };
        transaction.commit().await?;
        Ok(task_resource_status(
            request,
            claim,
            lease,
            execution_namespace,
        ))
    }

    /// Atomically activates and hands off a one-shot Task resource.
    ///
    /// The owner supplies both current revisions. Resource computes the lease window using the
    /// database clock, advances the request and lease together, and only then records
    /// `HandedOff`. A stale Evaluation worker cannot acknowledge a newer reservation.
    #[allow(clippy::too_many_lines)]
    pub async fn acknowledge_task_resource(
        &self,
        task_run_id: TaskRunId,
        expected_claim_revision: Revision,
        expected_lease_revision: Revision,
        execution_namespace: &str,
        trace_id: &str,
    ) -> Result<TaskResourceStatus, ResourceStoreError> {
        if !valid_namespace_name(execution_namespace) {
            return Err(ResourceStoreError::InvalidNamespace);
        }
        validate_trace(trace_id)?;
        let mut transaction = self.pool.begin().await?;
        let row = sqlx::query(
            "SELECT r.contract AS request_contract,c.contract AS claim_contract,
                    l.contract AS lease_contract,c.namespace_name AS execution_namespace,
                    a.approval_id,a.request_id AS approval_request_id,
                    a.approved_duration_seconds,a.valid_until AS approval_valid_until
             FROM resource.resource_requests r
             JOIN resource.capacity_claims c ON c.request_id=r.request_id
             JOIN resource.resource_leases l ON l.claim_id=c.claim_id
             JOIN resource.resource_approvals a ON a.approval_id=c.approval_id
             WHERE r.task_run_id=$1 AND r.target_kind='task'
             FOR UPDATE OF r,c,l",
        )
        .bind(task_run_id.as_uuid())
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or(ResourceStoreError::NotFound)?;
        let request = decode_request(row.try_get("request_contract")?)?;
        let claim = decode_claim(row.try_get("claim_contract")?)?;
        let lease = decode_lease(row.try_get("lease_contract")?)?;
        let current_namespace: Option<String> = row.try_get("execution_namespace")?;
        let approval_id: uuid::Uuid = row.try_get("approval_id")?;
        let approval_request_id: uuid::Uuid = row.try_get("approval_request_id")?;
        let approved_duration_seconds: i64 = row.try_get("approved_duration_seconds")?;
        let approval_valid_until: time::OffsetDateTime = row.try_get("approval_valid_until")?;
        if !matches!(request.target, ResourceTarget::Task { task_run_id: id } if id == task_run_id)
            || claim.request_id != request.id
            || lease.request_id != request.id
            || lease.claim_id != claim.id
            || approval_id != claim.approval_id.as_uuid()
            || approval_request_id != request.id.as_uuid()
            || claim.state != CapacityClaimState::Provisioning
            || request.state != ResourceRequestState::Allocating
            || claim.revision != expected_claim_revision
            || lease.revision != expected_lease_revision
            || lease.state != ResourceLeaseState::Allocating
            || current_namespace
                .as_deref()
                .is_some_and(|value| value != execution_namespace)
        {
            return Err(ResourceStoreError::CapacityClaimStateConflict);
        }
        let now = database_now(&mut transaction).await?;
        if approved_duration_seconds <= 0 || approval_valid_until <= now.get() {
            return Err(ResourceStoreError::ApprovalInvalid);
        }
        let duration = approved_duration_seconds;
        let expires_at = UtcTimestamp::from_utc(now.get() + time::Duration::seconds(duration))
            .map_err(|_| ResourceStoreError::CapacityReadbackInvalid)?;
        let next_lease =
            ResourceLifecycle::activate_lease(&lease, expected_lease_revision, now, expires_at)?;
        let next_request = ResourceLifecycle::activate(&request, request.revision, now)?;
        let next_claim = transition_claim(&claim, CapacityClaimState::HandedOff)?;
        update_claim(
            &mut transaction,
            &claim,
            &next_claim,
            Some(execution_namespace),
            None,
            None,
        )
        .await?;
        update_lease(&mut transaction, &lease, &next_lease).await?;
        update_request(&mut transaction, &request, &next_request).await?;
        insert_transition(
            &mut transaction,
            &next_request,
            next_request.revision.get(),
            Some(request.state),
            None,
            trace_id,
        )
        .await?;
        enqueue_lease_event(
            &mut transaction,
            &next_lease,
            &next_request,
            LEASE_ACTIVATED_SUBJECT,
            trace_id,
        )
        .await?;
        enqueue_request_event(
            &mut transaction,
            &next_request,
            REQUEST_STATE_CHANGED_SUBJECT,
            trace_id,
        )
        .await?;
        transaction.commit().await?;
        Ok(task_resource_status(
            next_request,
            next_claim,
            next_lease,
            Some(execution_namespace.to_owned()),
        ))
    }

    /// Reads the complete authoritative state for one Task target.
    pub async fn load_task_resource(
        &self,
        task_run_id: TaskRunId,
    ) -> Result<TaskResourceStatus, ResourceStoreError> {
        let row = sqlx::query(
            "SELECT r.contract AS request_contract,c.contract AS claim_contract,
                    l.contract AS lease_contract,c.namespace_name AS execution_namespace
             FROM resource.resource_requests r
             JOIN resource.capacity_claims c ON c.request_id=r.request_id
             JOIN resource.resource_leases l ON l.claim_id=c.claim_id
             WHERE r.task_run_id=$1 AND r.target_kind='task'",
        )
        .bind(task_run_id.as_uuid())
        .fetch_optional(&self.pool)
        .await?
        .ok_or(ResourceStoreError::NotFound)?;
        let request = decode_request(row.try_get("request_contract")?)?;
        let claim = decode_claim(row.try_get("claim_contract")?)?;
        let lease = decode_lease(row.try_get("lease_contract")?)?;
        let execution_namespace: Option<String> = row.try_get("execution_namespace")?;
        if !matches!(request.target, ResourceTarget::Task { task_run_id: id } if id == task_run_id)
            || claim.request_id != request.id
            || lease.request_id != request.id
            || lease.claim_id != claim.id
        {
            return Err(ResourceStoreError::ScopeConflict);
        }
        Ok(task_resource_status(
            request,
            claim,
            lease,
            execution_namespace,
        ))
    }

    /// Releases a Task reservation after the owner has stopped using it.
    ///
    /// Task resources have no Environment object requiring an external cleanup readback. The
    /// whole pre-handoff or post-handoff cleanup is therefore committed under the same row locks,
    /// while the two caller revisions still fence stale owner requests.
    #[allow(clippy::too_many_lines)]
    pub async fn release_task_resource(
        &self,
        task_run_id: TaskRunId,
        expected_claim_revision: Revision,
        expected_lease_revision: Revision,
        trace_id: &str,
    ) -> Result<TaskResourceStatus, ResourceStoreError> {
        validate_trace(trace_id)?;
        let mut transaction = self.pool.begin().await?;
        let row = sqlx::query(
            "SELECT r.contract AS request_contract,c.contract AS claim_contract,
                    l.contract AS lease_contract,c.namespace_name AS execution_namespace
             FROM resource.resource_requests r
             JOIN resource.capacity_claims c ON c.request_id=r.request_id
             JOIN resource.resource_leases l ON l.claim_id=c.claim_id
             WHERE r.task_run_id=$1 AND r.target_kind='task'
             FOR UPDATE OF r,c,l",
        )
        .bind(task_run_id.as_uuid())
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or(ResourceStoreError::NotFound)?;
        let request = decode_request(row.try_get("request_contract")?)?;
        let claim = decode_claim(row.try_get("claim_contract")?)?;
        let lease = decode_lease(row.try_get("lease_contract")?)?;
        let execution_namespace: Option<String> = row.try_get("execution_namespace")?;
        if !matches!(request.target, ResourceTarget::Task { task_run_id: id } if id == task_run_id)
            || claim.request_id != request.id
            || lease.request_id != request.id
            || lease.claim_id != claim.id
            || claim.revision != expected_claim_revision
            || lease.revision != expected_lease_revision
            || !matches!(
                claim.state,
                CapacityClaimState::Reserved
                    | CapacityClaimState::Provisioning
                    | CapacityClaimState::HandedOff
            )
            || !matches!(
                lease.state,
                ResourceLeaseState::Allocating | ResourceLeaseState::Active
            )
            || !matches!(
                request.state,
                ResourceRequestState::Allocating | ResourceRequestState::Active
            )
        {
            return Err(ResourceStoreError::CapacityClaimStateConflict);
        }
        let now = database_now(&mut transaction).await?;
        let expiring_lease = ResourceLifecycle::begin_lease_expiry(
            &lease,
            expected_lease_revision,
            now,
            Some("task_owner_release".to_owned()),
        )?;
        let expired_lease = ResourceLifecycle::complete_lease_expiry(
            &expiring_lease,
            expiring_lease.revision,
            now,
        )?;
        let expiring_request = ResourceLifecycle::begin_expiry(&request, request.revision, now)?;
        let expired_request =
            ResourceLifecycle::complete_expiry(&expiring_request, expiring_request.revision, now)?;
        let releasing_claim = transition_claim(&claim, CapacityClaimState::Releasing)?;
        let released_claim = transition_claim(&releasing_claim, CapacityClaimState::Released)?;
        update_claim(&mut transaction, &claim, &released_claim, None, None, None).await?;
        sqlx::query(
            "UPDATE resource.gpu_capacity_reservations
             SET state='released', released_at=clock_timestamp()
             WHERE claim_id=$1 AND state='reserved'",
        )
        .bind(claim.id.as_uuid())
        .execute(&mut *transaction)
        .await?;
        update_lease(&mut transaction, &lease, &expired_lease).await?;
        update_request(&mut transaction, &request, &expired_request).await?;
        insert_transition(
            &mut transaction,
            &expired_request,
            expired_request.revision.get(),
            Some(request.state),
            None,
            trace_id,
        )
        .await?;
        enqueue_lease_event(
            &mut transaction,
            &expired_lease,
            &expired_request,
            LEASE_EXPIRED_SUBJECT,
            trace_id,
        )
        .await?;
        enqueue_request_event(
            &mut transaction,
            &expired_request,
            REQUEST_STATE_CHANGED_SUBJECT,
            trace_id,
        )
        .await?;
        transaction.commit().await?;
        Ok(task_resource_status(
            expired_request,
            released_claim,
            expired_lease,
            execution_namespace,
        ))
    }

    /// Applies an administrator renewal with an exact revision and idempotency fence.
    pub async fn renew_lease(
        &self,
        idempotency_key: &str,
        lease_id: LeaseId,
        expected_revision: contracts::Revision,
        expires_at: UtcTimestamp,
        trace_id: &str,
    ) -> Result<ResourceLease, ResourceStoreError> {
        IdempotencyKey::parse(idempotency_key).map_err(|_| ResourceStoreError::IdempotencyKey)?;
        validate_trace(trace_id)?;
        let mut transaction = self.pool.begin().await?;
        let lease = load_locked_lease(&mut transaction, lease_id).await?;
        let hash = Sha256Digest::of_canonical(&(lease_id, expected_revision, expires_at))
            .map_err(|_| ResourceStoreError::Wire)?;
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
                let request = load_locked(&mut transaction, lease.request_id).await?;
                update_lease(&mut transaction, &lease, &next).await?;
                enqueue_lease_event(
                    &mut transaction,
                    &next,
                    &request,
                    LEASE_RENEWED_SUBJECT,
                    trace_id,
                )
                .await?;
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

    /// Revokes a Lease with an exact revision and idempotency fence.
    pub async fn revoke_lease(
        &self,
        idempotency_key: &str,
        lease_id: LeaseId,
        expected_revision: contracts::Revision,
        reason: String,
        actor: contracts::ActorId,
        trace_id: &str,
    ) -> Result<ResourceLease, ResourceStoreError> {
        IdempotencyKey::parse(idempotency_key).map_err(|_| ResourceStoreError::IdempotencyKey)?;
        validate_trace(trace_id)?;
        let mut transaction = self.pool.begin().await?;
        let lease = load_locked_lease(&mut transaction, lease_id).await?;
        let hash = Sha256Digest::of_canonical(&(lease_id, expected_revision, &reason, actor))
            .map_err(|_| ResourceStoreError::Wire)?;
        let result = match IdempotencyStore::reserve(
            &mut transaction,
            Domain::Resource,
            "revoke_resource_lease",
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
                let next = ResourceLifecycle::revoke_lease(&lease, expected_revision, now, reason)?;
                let request = load_locked(&mut transaction, lease.request_id).await?;
                let next_request =
                    ResourceLifecycle::begin_expiry(&request, request.revision, now)?;
                update_lease(&mut transaction, &lease, &next).await?;
                update_request(&mut transaction, &request, &next_request).await?;
                insert_transition(
                    &mut transaction,
                    &next_request,
                    next_request.revision.get(),
                    Some(request.state),
                    Some(actor),
                    trace_id,
                )
                .await?;
                enqueue_lease_event(
                    &mut transaction,
                    &next,
                    &next_request,
                    LEASE_REVOKED_SUBJECT,
                    trace_id,
                )
                .await?;
                enqueue_request_event(
                    &mut transaction,
                    &next_request,
                    REQUEST_STATE_CHANGED_SUBJECT,
                    trace_id,
                )
                .await?;
                let value = serde_json::to_value(&next)?;
                IdempotencyStore::complete(
                    &mut transaction,
                    Domain::Resource,
                    "revoke_resource_lease",
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
        actor: contracts::ActorId,
        trace_id: &str,
    ) -> Result<ResourceLease, ResourceStoreError> {
        validate_trace(trace_id)?;
        let mut transaction = self.pool.begin().await?;
        let lease = load_locked_lease(&mut transaction, lease_id).await?;
        let request = load_locked(&mut transaction, lease.request_id).await?;
        let next =
            ResourceLifecycle::activate_lease(&lease, expected_revision, active_from, expires_at)?;
        let next_request = ResourceLifecycle::activate(&request, request.revision, active_from)?;
        update_lease(&mut transaction, &lease, &next).await?;
        update_request(&mut transaction, &request, &next_request).await?;
        insert_transition(
            &mut transaction,
            &next_request,
            next_request.revision.get(),
            Some(ResourceRequestState::Allocating),
            Some(actor),
            trace_id,
        )
        .await?;
        enqueue_lease_event(
            &mut transaction,
            &next,
            &next_request,
            LEASE_ACTIVATED_SUBJECT,
            trace_id,
        )
        .await?;
        enqueue_request_event(
            &mut transaction,
            &next_request,
            REQUEST_STATE_CHANGED_SUBJECT,
            trace_id,
        )
        .await?;
        transaction.commit().await?;
        Ok(next)
    }

    /// Starts the fail-closed expiry saga. Capacity remains reserved until cleanup readback.
    pub async fn begin_lease_expiry(
        &self,
        lease_id: LeaseId,
        expected_revision: contracts::Revision,
        reason: Option<String>,
        actor: contracts::ActorId,
        trace_id: &str,
    ) -> Result<ResourceLease, ResourceStoreError> {
        validate_trace(trace_id)?;
        let mut transaction = self.pool.begin().await?;
        let lease = load_locked_lease(&mut transaction, lease_id).await?;
        let request = load_locked(&mut transaction, lease.request_id).await?;
        let now = database_now(&mut transaction).await?;
        let next = ResourceLifecycle::begin_lease_expiry(&lease, expected_revision, now, reason)?;
        let next_request = ResourceLifecycle::begin_expiry(&request, request.revision, now)?;
        update_lease(&mut transaction, &lease, &next).await?;
        update_request(&mut transaction, &request, &next_request).await?;
        insert_transition(
            &mut transaction,
            &next_request,
            next_request.revision.get(),
            Some(request.state),
            Some(actor),
            trace_id,
        )
        .await?;
        enqueue_lease_event(
            &mut transaction,
            &next,
            &next_request,
            LEASE_EXPIRING_SUBJECT,
            trace_id,
        )
        .await?;
        enqueue_request_event(
            &mut transaction,
            &next_request,
            REQUEST_STATE_CHANGED_SUBJECT,
            trace_id,
        )
        .await?;
        transaction.commit().await?;
        Ok(next)
    }

    /// Marks a Lease expired after Environment cleanup and exact capacity release readback.
    pub async fn complete_lease_expiry(
        &self,
        lease_id: LeaseId,
        expected_revision: contracts::Revision,
        actor: contracts::ActorId,
        trace_id: &str,
    ) -> Result<ResourceLease, ResourceStoreError> {
        validate_trace(trace_id)?;
        let mut transaction = self.pool.begin().await?;
        let lease = load_locked_lease(&mut transaction, lease_id).await?;
        let request = load_locked(&mut transaction, lease.request_id).await?;
        let now = database_now(&mut transaction).await?;
        let next = ResourceLifecycle::complete_lease_expiry(&lease, expected_revision, now)?;
        let next_request = ResourceLifecycle::complete_expiry(&request, request.revision, now)?;
        update_lease(&mut transaction, &lease, &next).await?;
        update_request(&mut transaction, &request, &next_request).await?;
        insert_transition(
            &mut transaction,
            &next_request,
            next_request.revision.get(),
            Some(ResourceRequestState::Expiring),
            Some(actor),
            trace_id,
        )
        .await?;
        enqueue_lease_event(
            &mut transaction,
            &next,
            &next_request,
            LEASE_EXPIRED_SUBJECT,
            trace_id,
        )
        .await?;
        enqueue_request_event(
            &mut transaction,
            &next_request,
            REQUEST_STATE_CHANGED_SUBJECT,
            trace_id,
        )
        .await?;
        transaction.commit().await?;
        Ok(next)
    }

    /// Claims one Environment capacity allocation with `SKIP LOCKED`.
    ///
    /// Resource does not claim one-shot Task capacity in this worker. Evaluation owns that
    /// boundary and obtains a durable claim through the internal Task API after its Job identity
    /// exists. This prevents a worker from inventing an Environment or acknowledging a task on
    /// behalf of its owner.
    pub async fn claim_next_capacity_shell(
        &self,
    ) -> Result<Option<ProvisioningCapacityClaim>, ResourceStoreError> {
        let mut transaction = self.pool.begin().await?;
        let row = sqlx::query(
            "SELECT c.contract AS claim_contract,r.contract AS request_contract,l.contract AS lease_contract,c.lease_synced_revision AS lease_synced_revision \
             FROM resource.capacity_claims c \
             JOIN resource.resource_requests r ON r.request_id=c.request_id \
             JOIN resource.resource_leases l ON l.claim_id=c.claim_id \
             WHERE r.target_kind='environment' \
               AND ((c.state='reserved' AND l.state='allocating') \
                    OR (c.state='provisioning' \
                        AND c.updated_at <= clock_timestamp() - interval '1 minute' \
                        AND l.state IN ('allocating','active'))) \
             ORDER BY c.updated_at,c.created_at \
             FOR UPDATE OF c,l SKIP LOCKED LIMIT 1",
        )
        .fetch_optional(&mut *transaction)
        .await?;
        let Some(row) = row else {
            transaction.commit().await?;
            return Ok(None);
        };
        let claim = decode_claim(row.try_get("claim_contract")?)?;
        let request = decode_request(row.try_get("request_contract")?)?;
        let lease = decode_lease(row.try_get("lease_contract")?)?;
        if !matches!(
            claim.state,
            CapacityClaimState::Reserved | CapacityClaimState::Provisioning
        ) || !matches!(
            lease.state,
            ResourceLeaseState::Allocating | ResourceLeaseState::Active
        ) {
            return Err(ResourceStoreError::CapacityClaimStateConflict);
        }
        let next = if claim.state == CapacityClaimState::Provisioning {
            // A stale worker may still hold the previous revision. Requeue through the explicit
            // Reserved state so the recovery advances the fence twice and invalidates that
            // worker before another attempt receives the shell.
            let requeued = transition_claim(&claim, CapacityClaimState::Reserved)?;
            let next = transition_claim(&requeued, CapacityClaimState::Provisioning)?;
            tracing::warn!(
                event = "resource.capacity.provisioning_recovered",
                diagnostic_code = "LW_RESOURCE_CAPACITY_PROVISIONING_RECOVERED",
                claim_id = %claim.id,
                previous_revision = claim.revision.get(),
                recovered_revision = next.revision.get()
            );
            update_claim(&mut transaction, &claim, &next, None, None, None).await?;
            next
        } else {
            let next = transition_claim(&claim, CapacityClaimState::Provisioning)?;
            update_claim(&mut transaction, &claim, &next, None, None, None).await?;
            next
        };
        transaction.commit().await?;
        Ok(Some(ProvisioningCapacityClaim {
            claim: next,
            request,
            lease,
            lease_synced_revision: decode_lease_synced_revision(&row)?,
        }))
    }

    /// Reloads a provisioning claim after lease activation or while recovering a handoff.
    pub async fn refresh_provisioning_claim(
        &self,
        claim_id: contracts::CapacityClaimId,
    ) -> Result<Option<ProvisioningCapacityClaim>, ResourceStoreError> {
        let row = sqlx::query(
            "SELECT c.contract AS claim_contract,r.contract AS request_contract,l.contract AS lease_contract,c.lease_synced_revision AS lease_synced_revision \
             FROM resource.capacity_claims c \
             JOIN resource.resource_requests r ON r.request_id=c.request_id \
             JOIN resource.resource_leases l ON l.claim_id=c.claim_id \
             WHERE c.claim_id=$1 AND c.state='provisioning'",
        )
        .bind(claim_id.as_uuid())
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        Ok(Some(ProvisioningCapacityClaim {
            claim: decode_claim(row.try_get("claim_contract")?)?,
            request: decode_request(row.try_get("request_contract")?)?,
            lease: decode_lease(row.try_get("lease_contract")?)?,
            lease_synced_revision: decode_lease_synced_revision(&row)?,
        }))
    }

    /// Reads one Environment claim whose active Lease still needs its owner handoff.
    pub async fn next_provisioning_capacity_handoff(
        &self,
    ) -> Result<Option<ProvisioningCapacityClaim>, ResourceStoreError> {
        let row = sqlx::query(
            "SELECT c.contract AS claim_contract,r.contract AS request_contract,l.contract AS lease_contract,c.lease_synced_revision AS lease_synced_revision \
             FROM resource.capacity_claims c \
             JOIN resource.resource_requests r ON r.request_id=c.request_id \
             JOIN resource.resource_leases l ON l.claim_id=c.claim_id \
             WHERE c.state='provisioning' AND l.state='active' AND r.target_kind='environment' \
             ORDER BY c.updated_at,c.claim_id LIMIT 1",
        )
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        Ok(Some(ProvisioningCapacityClaim {
            claim: decode_claim(row.try_get("claim_contract")?)?,
            request: decode_request(row.try_get("request_contract")?)?,
            lease: decode_lease(row.try_get("lease_contract")?)?,
            lease_synced_revision: decode_lease_synced_revision(&row)?,
        }))
    }

    /// Records exact Kubernetes UIDs only after the Provider has read back its claim fence.
    pub async fn mark_capacity_shell_ready(
        &self,
        claim_id: contracts::CapacityClaimId,
        expected_revision: contracts::Revision,
        namespace: &str,
        namespace_uid: &str,
        quota_uid: &str,
    ) -> Result<CapacityClaim, ResourceStoreError> {
        if !valid_namespace_name(namespace) || namespace_uid.is_empty() || quota_uid.is_empty() {
            return Err(ResourceStoreError::CapacityReadbackInvalid);
        }
        let mut transaction = self.pool.begin().await?;
        let claim = load_locked_claim(&mut transaction, claim_id).await?;
        if claim.revision != expected_revision || claim.state != CapacityClaimState::Provisioning {
            return Err(ResourceStoreError::CapacityClaimStateConflict);
        }
        let next = transition_claim(&claim, CapacityClaimState::Ready)?;
        update_claim(
            &mut transaction,
            &claim,
            &next,
            Some(namespace),
            Some(namespace_uid),
            Some(quota_uid),
        )
        .await?;
        transaction.commit().await?;
        Ok(next)
    }

    /// Reads one provider-ready shell. Repeated reads are intentionally safe: the Environment
    /// handoff uses a request/revision-derived idempotency key and is acknowledged before this
    /// authoritative state advances to `handed_off`.
    pub async fn next_ready_capacity_handoff(
        &self,
    ) -> Result<Option<ProvisioningCapacityClaim>, ResourceStoreError> {
        let row = sqlx::query(
            "SELECT c.contract AS claim_contract,r.contract AS request_contract,l.contract AS lease_contract,c.lease_synced_revision AS lease_synced_revision FROM resource.capacity_claims c JOIN resource.resource_requests r ON r.request_id=c.request_id JOIN resource.resource_leases l ON l.claim_id=c.claim_id WHERE c.state='ready' ORDER BY c.updated_at LIMIT 1",
        )
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        Ok(Some(ProvisioningCapacityClaim {
            claim: decode_claim(row.try_get("claim_contract")?)?,
            request: decode_request(row.try_get("request_contract")?)?,
            lease: decode_lease(row.try_get("lease_contract")?)?,
            lease_synced_revision: decode_lease_synced_revision(&row)?,
        }))
    }

    /// Reloads a ready claim by id for handoff. Returns `None` if the claim was already
    /// consumed by another reconciler cycle (avoids spurious crash on `.ok_or()`).
    pub async fn refresh_ready_claim(
        &self,
        claim_id: contracts::CapacityClaimId,
    ) -> Result<Option<ProvisioningCapacityClaim>, ResourceStoreError> {
        let row = sqlx::query(
            "SELECT c.contract AS claim_contract,r.contract AS request_contract,l.contract AS lease_contract,c.lease_synced_revision AS lease_synced_revision FROM resource.capacity_claims c JOIN resource.resource_requests r ON r.request_id=c.request_id JOIN resource.resource_leases l ON l.claim_id=c.claim_id WHERE c.claim_id=$1 AND c.state='ready'",
        )
        .bind(claim_id.as_uuid())
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        Ok(Some(ProvisioningCapacityClaim {
            claim: decode_claim(row.try_get("claim_contract")?)?,
            request: decode_request(row.try_get("request_contract")?)?,
            lease: decode_lease(row.try_get("lease_contract")?)?,
            lease_synced_revision: decode_lease_synced_revision(&row)?,
        }))
    }

    /// Records Environment ownership transfer only after its owner durably accepts the
    /// idempotent handoff. The lease revision sent in that handoff is the initial owner fence;
    /// it is retained even when Resource advances the lease while this transaction is waiting.
    pub async fn mark_capacity_handed_off(
        &self,
        claim_id: contracts::CapacityClaimId,
        expected_revision: contracts::Revision,
        lease_id: LeaseId,
        lease_revision: contracts::Revision,
    ) -> Result<CapacityClaim, ResourceStoreError> {
        let mut transaction = self.pool.begin().await?;
        let claim = load_locked_claim(&mut transaction, claim_id).await?;
        let lease = load_locked_lease(&mut transaction, lease_id).await?;
        if claim.revision != expected_revision
            || !matches!(
                claim.state,
                CapacityClaimState::Provisioning | CapacityClaimState::Ready
            )
            || lease.claim_id != claim.id
            || lease.revision < lease_revision
            || !matches!(
                lease.state,
                ResourceLeaseState::Active | ResourceLeaseState::Expiring
            )
        {
            return Err(ResourceStoreError::CapacityClaimStateConflict);
        }
        let next = transition_claim(&claim, CapacityClaimState::HandedOff)?;
        let changed = sqlx::query(
            "UPDATE resource.capacity_claims \
             SET state=$3,revision=$4,lease_synced_revision=$5,updated_at=clock_timestamp(),contract=$6 \
             WHERE claim_id=$1 AND revision=$2 AND state IN ('provisioning','ready') \
               AND lease_synced_revision < $5",
        )
        .bind(claim.id.as_uuid())
        .bind(i64::try_from(claim.revision.get())?)
        .bind(wire(next.state)? )
        .bind(i64::try_from(next.revision.get())?)
        .bind(i64::try_from(lease_revision.get())?)
        .bind(serde_json::to_value(&next)? )
        .execute(&mut *transaction)
        .await?;
        if changed.rows_affected() != 1 {
            return Err(ResourceStoreError::RevisionConflict);
        }
        transaction.commit().await?;
        Ok(next)
    }

    /// Returns one active handed-off Lease whose current revision has not yet
    /// been acknowledged by Environment.
    pub async fn next_unsynced_active_lease(
        &self,
    ) -> Result<Option<ProvisioningCapacityClaim>, ResourceStoreError> {
        let row = sqlx::query(
            "SELECT c.contract AS claim_contract,r.contract AS request_contract,l.contract AS lease_contract,c.lease_synced_revision AS lease_synced_revision \
             FROM resource.capacity_claims c \
             JOIN resource.resource_requests r ON r.request_id=c.request_id \
             JOIN resource.resource_leases l ON l.claim_id=c.claim_id \
             WHERE c.state='handed_off' AND l.state='active' \
               AND c.lease_synced_revision < l.revision \
             ORDER BY l.updated_at,l.lease_id LIMIT 1",
        )
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        Ok(Some(ProvisioningCapacityClaim {
            claim: decode_claim(row.try_get("claim_contract")?)?,
            request: decode_request(row.try_get("request_contract")?)?,
            lease: decode_lease(row.try_get("lease_contract")?)?,
            lease_synced_revision: decode_lease_synced_revision(&row)?,
        }))
    }

    /// Records the exact Lease revision acknowledged by Environment. An `expiring` Lease is
    /// accepted here because a successful owner sync may race Resource's revoke transition.
    pub async fn mark_lease_synced(
        &self,
        claim_id: contracts::CapacityClaimId,
        lease_revision: contracts::Revision,
    ) -> Result<(), ResourceStoreError> {
        let changed = sqlx::query(
            "UPDATE resource.capacity_claims c SET lease_synced_revision=$2,updated_at=clock_timestamp() \
             FROM resource.resource_leases l \
             WHERE c.claim_id=$1 AND c.state='handed_off' AND l.claim_id=c.claim_id \
               AND l.state IN ('active','expiring') AND l.revision >= $2 \
               AND c.lease_synced_revision < $2",
        )
        .bind(claim_id.as_uuid())
        .bind(i64::try_from(lease_revision.get())?)
        .execute(&self.pool)
        .await?;
        if changed.rows_affected() != 1 {
            return Err(ResourceStoreError::RevisionConflict);
        }
        Ok(())
    }

    /// Claims one due or explicitly revoked handed-off Lease. Natural expiry
    /// first enters the same durable `expiring` state used by revocation.
    pub async fn next_lease_cleanup(
        &self,
        actor: contracts::ActorId,
    ) -> Result<Option<ProvisioningCapacityClaim>, ResourceStoreError> {
        let mut transaction = self.pool.begin().await?;
        let row = sqlx::query(
            "SELECT c.contract AS claim_contract,r.contract AS request_contract,l.contract AS lease_contract,c.lease_synced_revision AS lease_synced_revision \
             FROM resource.capacity_claims c \
             JOIN resource.resource_requests r ON r.request_id=c.request_id \
             JOIN resource.resource_leases l ON l.claim_id=c.claim_id \
             WHERE r.target_kind='environment' \
               AND c.state IN ('reserved','provisioning','blocked','handed_off','releasing') \
               AND (l.state='expiring' OR (l.state='active' AND l.expires_at<=clock_timestamp())) \
               AND NOT EXISTS (\
                 SELECT 1 FROM resource.capacity_attempts a \
                 WHERE a.claim_id=c.claim_id \
                   AND a.step IN ('expire_environment','release_capacity') \
                   AND a.state='failed' \
               )\
               AND NOT EXISTS (\
                 SELECT 1 FROM resource.capacity_attempts a \
                 WHERE a.claim_id=c.claim_id \
                   AND a.step IN ('expire_environment','release_capacity') \
                   AND a.state='retry' \
                   AND a.next_attempt_at>clock_timestamp() \
               )\
             ORDER BY l.expires_at,l.updated_at FOR UPDATE OF c,l SKIP LOCKED LIMIT 1",
        )
        .fetch_optional(&mut *transaction)
        .await?;
        let Some(row) = row else {
            transaction.commit().await?;
            return Ok(None);
        };
        let claim = decode_claim(row.try_get("claim_contract")?)?;
        let request = decode_request(row.try_get("request_contract")?)?;
        let lease = decode_lease(row.try_get("lease_contract")?)?;
        let lease = if lease.state == ResourceLeaseState::Active {
            let now = database_now(&mut transaction).await?;
            let next = ResourceLifecycle::begin_lease_expiry(&lease, lease.revision, now, None)?;
            let next_request = ResourceLifecycle::begin_expiry(&request, request.revision, now)?;
            update_lease(&mut transaction, &lease, &next).await?;
            update_request(&mut transaction, &request, &next_request).await?;
            insert_transition(
                &mut transaction,
                &next_request,
                next_request.revision.get(),
                Some(request.state),
                Some(actor),
                &format!("resource-lease-expire-{}", lease.id),
            )
            .await?;
            let trace_id = format!("resource-lease-expire-{}", lease.id);
            enqueue_lease_event(
                &mut transaction,
                &next,
                &next_request,
                LEASE_EXPIRING_SUBJECT,
                &trace_id,
            )
            .await?;
            enqueue_request_event(
                &mut transaction,
                &next_request,
                REQUEST_STATE_CHANGED_SUBJECT,
                &trace_id,
            )
            .await?;
            next
        } else {
            lease
        };
        transaction.commit().await?;
        Ok(Some(ProvisioningCapacityClaim {
            claim,
            request,
            lease,
            lease_synced_revision: decode_lease_synced_revision(&row)?,
        }))
    }

    pub async fn mark_capacity_releasing(
        &self,
        claim_id: contracts::CapacityClaimId,
        expected_revision: contracts::Revision,
    ) -> Result<CapacityClaim, ResourceStoreError> {
        let mut transaction = self.pool.begin().await?;
        let claim = load_locked_claim(&mut transaction, claim_id).await?;
        if claim.revision != expected_revision
            || !matches!(
                claim.state,
                CapacityClaimState::Reserved
                    | CapacityClaimState::Provisioning
                    | CapacityClaimState::Blocked
                    | CapacityClaimState::HandedOff
            )
        {
            return Err(ResourceStoreError::CapacityClaimStateConflict);
        }
        let next = transition_claim(&claim, CapacityClaimState::Releasing)?;
        update_claim(&mut transaction, &claim, &next, None, None, None).await?;
        transaction.commit().await?;
        Ok(next)
    }

    /// Completes the exact claim and Lease only after Environment and Provider
    /// absence readback have both succeeded.
    pub async fn complete_capacity_release(
        &self,
        claim_id: contracts::CapacityClaimId,
        expected_claim_revision: contracts::Revision,
        lease_id: LeaseId,
        expected_lease_revision: contracts::Revision,
        actor: contracts::ActorId,
        trace_id: &str,
    ) -> Result<ResourceLease, ResourceStoreError> {
        validate_trace(trace_id)?;
        let mut transaction = self.pool.begin().await?;
        let claim = load_locked_claim(&mut transaction, claim_id).await?;
        let lease = load_locked_lease(&mut transaction, lease_id).await?;
        let request = load_locked(&mut transaction, lease.request_id).await?;
        if claim.revision != expected_claim_revision
            || claim.state != CapacityClaimState::Releasing
            || lease.revision != expected_lease_revision
            || lease.state != ResourceLeaseState::Expiring
            || lease.claim_id != claim.id
        {
            return Err(ResourceStoreError::CapacityClaimStateConflict);
        }
        let next_claim = transition_claim(&claim, CapacityClaimState::Released)?;
        let now = database_now(&mut transaction).await?;
        let next_lease = ResourceLifecycle::complete_lease_expiry(&lease, lease.revision, now)?;
        let next_request = ResourceLifecycle::complete_expiry(&request, request.revision, now)?;
        update_claim(&mut transaction, &claim, &next_claim, None, None, None).await?;
        sqlx::query(
            "UPDATE resource.gpu_capacity_reservations \
             SET state='released', released_at=clock_timestamp() \
             WHERE claim_id=$1 AND state='reserved'",
        )
        .bind(claim.id.as_uuid())
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            "UPDATE resource.capacity_claims SET last_diagnostic_code=NULL WHERE claim_id=$1",
        )
        .bind(claim.id.as_uuid())
        .execute(&mut *transaction)
        .await?;
        update_lease(&mut transaction, &lease, &next_lease).await?;
        update_request(&mut transaction, &request, &next_request).await?;
        insert_transition(
            &mut transaction,
            &next_request,
            next_request.revision.get(),
            Some(request.state),
            Some(actor),
            trace_id,
        )
        .await?;
        enqueue_lease_event(
            &mut transaction,
            &next_lease,
            &next_request,
            LEASE_EXPIRED_SUBJECT,
            trace_id,
        )
        .await?;
        enqueue_request_event(
            &mut transaction,
            &next_request,
            REQUEST_STATE_CHANGED_SUBJECT,
            trace_id,
        )
        .await?;
        transaction.commit().await?;
        Ok(next_lease)
    }

    /// Completes a claim that never crossed the Environment handoff boundary.
    ///
    /// No external Environment cleanup readback is required here because the claim is only
    /// eligible while it is still pre-handoff. Once a claim is `handed_off`, callers must use
    /// `complete_capacity_release` after the exact Environment cleanup status fence.
    pub async fn complete_pre_handoff_release(
        &self,
        claim_id: contracts::CapacityClaimId,
        expected_claim_revision: contracts::Revision,
        lease_id: LeaseId,
        expected_lease_revision: contracts::Revision,
        actor: contracts::ActorId,
        trace_id: &str,
    ) -> Result<ResourceLease, ResourceStoreError> {
        validate_trace(trace_id)?;
        let mut transaction = self.pool.begin().await?;
        let claim = load_locked_claim(&mut transaction, claim_id).await?;
        let lease = load_locked_lease(&mut transaction, lease_id).await?;
        let request = load_locked(&mut transaction, lease.request_id).await?;
        if claim.revision != expected_claim_revision
            || claim.state != CapacityClaimState::Releasing
            || lease.revision != expected_lease_revision
            || lease.state != ResourceLeaseState::Expiring
            || lease.claim_id != claim.id
            || !matches!(request.target, ResourceTarget::Environment { .. })
        {
            return Err(ResourceStoreError::CapacityClaimStateConflict);
        }
        let next_claim = transition_claim(&claim, CapacityClaimState::Released)?;
        let now = database_now(&mut transaction).await?;
        let next_lease = ResourceLifecycle::complete_lease_expiry(&lease, lease.revision, now)?;
        let next_request = ResourceLifecycle::complete_expiry(&request, request.revision, now)?;
        update_claim(&mut transaction, &claim, &next_claim, None, None, None).await?;
        sqlx::query(
            "UPDATE resource.gpu_capacity_reservations \
             SET state='released', released_at=clock_timestamp() \
             WHERE claim_id=$1 AND state='reserved'",
        )
        .bind(claim.id.as_uuid())
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            "UPDATE resource.capacity_claims SET last_diagnostic_code=NULL WHERE claim_id=$1",
        )
        .bind(claim.id.as_uuid())
        .execute(&mut *transaction)
        .await?;
        update_lease(&mut transaction, &lease, &next_lease).await?;
        update_request(&mut transaction, &request, &next_request).await?;
        insert_transition(
            &mut transaction,
            &next_request,
            next_request.revision.get(),
            Some(request.state),
            Some(actor),
            trace_id,
        )
        .await?;
        enqueue_lease_event(
            &mut transaction,
            &next_lease,
            &next_request,
            LEASE_EXPIRED_SUBJECT,
            trace_id,
        )
        .await?;
        enqueue_request_event(
            &mut transaction,
            &next_request,
            REQUEST_STATE_CHANGED_SUBJECT,
            trace_id,
        )
        .await?;
        transaction.commit().await?;
        Ok(next_lease)
    }

    /// Persists a sanitized reconciliation failure without releasing capacity
    /// or moving the Lease to a false terminal state.
    pub async fn record_reconciliation_failure(
        &self,
        claim_id: contracts::CapacityClaimId,
        step: &'static str,
        diagnostic_code: &str,
    ) -> Result<(), ResourceStoreError> {
        if !matches!(step, "expire_environment" | "release_capacity")
            || !valid_diagnostic(diagnostic_code)
        {
            return Err(ResourceStoreError::DiagnosticInvalid);
        }
        let mut transaction = self.pool.begin().await?;
        let state: String = sqlx::query_scalar(
            "SELECT state FROM resource.capacity_claims WHERE claim_id=$1 FOR UPDATE",
        )
        .bind(claim_id.as_uuid())
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or(ResourceStoreError::CapacityClaimNotFound)?;
        if !matches!(state.as_str(), "handed_off" | "releasing") {
            return Err(ResourceStoreError::CapacityClaimStateConflict);
        }
        let attempt: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint + 1 FROM resource.capacity_attempts \
             WHERE claim_id=$1 AND step=$2",
        )
        .bind(claim_id.as_uuid())
        .bind(step)
        .fetch_one(&mut *transaction)
        .await?;
        sqlx::query(
            "INSERT INTO resource.capacity_attempts \
             (claim_id,attempt,step,state,next_attempt_at,diagnostic_code) \
             VALUES ($1,$2,$3,$4,clock_timestamp() + make_interval(secs => $6::double precision * 5),$5)",
        )
        .bind(claim_id.as_uuid())
        .bind(attempt)
        .bind(step)
        .bind(if attempt >= 3 { "failed" } else { "retry" })
        .bind(diagnostic_code)
        .bind(i32::try_from(attempt.min(12)).map_err(|_| ResourceStoreError::RevisionOverflow)?)
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            "UPDATE resource.capacity_claims SET last_diagnostic_code=$2,updated_at=clock_timestamp() \
             WHERE claim_id=$1",
        )
        .bind(claim_id.as_uuid())
        .bind(diagnostic_code)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(())
    }

    /// Bounds transient handoff failures. After three attempts the claim is retained as
    /// `blocked` for an explicit administrator recovery instead of retrying indefinitely.
    pub async fn retry_or_block_capacity_handoff(
        &self,
        claim_id: contracts::CapacityClaimId,
        expected_revision: contracts::Revision,
        diagnostic_code: &str,
    ) -> Result<CapacityClaim, ResourceStoreError> {
        if !valid_diagnostic(diagnostic_code) {
            return Err(ResourceStoreError::DiagnosticInvalid);
        }
        let mut transaction = self.pool.begin().await?;
        let claim = load_locked_claim(&mut transaction, claim_id).await?;
        if claim.revision != expected_revision
            || !matches!(
                claim.state,
                CapacityClaimState::Provisioning | CapacityClaimState::Ready
            )
        {
            return Err(ResourceStoreError::CapacityClaimStateConflict);
        }
        let attempt: i64 = sqlx::query_scalar("SELECT count(*)::bigint + 1 FROM resource.capacity_attempts WHERE claim_id=$1 AND step='handoff_environment'")
            .bind(claim_id.as_uuid()).fetch_one(&mut *transaction).await?;
        sqlx::query("INSERT INTO resource.capacity_attempts (claim_id,attempt,step,state,diagnostic_code) VALUES ($1,$2,'handoff_environment',$3,$4)")
            .bind(claim_id.as_uuid()).bind(attempt).bind(if attempt >= 3 { "failed" } else { "retry" }).bind(diagnostic_code)
            .execute(&mut *transaction).await?;
        if attempt >= 3 {
            let next = transition_claim(&claim, CapacityClaimState::Blocked)?;
            update_claim(&mut transaction, &claim, &next, None, None, None).await?;
            sqlx::query(
                "UPDATE resource.capacity_claims SET last_diagnostic_code=$2 WHERE claim_id=$1",
            )
            .bind(claim_id.as_uuid())
            .bind(diagnostic_code)
            .execute(&mut *transaction)
            .await?;
            transaction.commit().await?;
            return Ok(next);
        }
        transaction.commit().await?;
        Ok(claim)
    }

    /// Retains a failed claim for explicit administrator retry; it never silently re-enters admission.
    pub async fn mark_capacity_shell_blocked(
        &self,
        claim_id: contracts::CapacityClaimId,
        expected_revision: contracts::Revision,
        diagnostic_code: &str,
    ) -> Result<CapacityClaim, ResourceStoreError> {
        if !valid_diagnostic(diagnostic_code) {
            return Err(ResourceStoreError::DiagnosticInvalid);
        }
        let mut transaction = self.pool.begin().await?;
        let claim = load_locked_claim(&mut transaction, claim_id).await?;
        if claim.revision != expected_revision || claim.state != CapacityClaimState::Provisioning {
            return Err(ResourceStoreError::CapacityClaimStateConflict);
        }
        let next = transition_claim(&claim, CapacityClaimState::Blocked)?;
        update_claim(&mut transaction, &claim, &next, None, None, None).await?;
        sqlx::query(
            "UPDATE resource.capacity_claims SET last_diagnostic_code=$2 WHERE claim_id=$1",
        )
        .bind(claim_id.as_uuid())
        .bind(diagnostic_code)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(next)
    }

    /// Records a bounded provider failure. The first two failures return the exact claim to
    /// `reserved`; the third becomes durable `blocked` and needs an administrator retry.
    pub async fn retry_or_block_capacity_shell(
        &self,
        claim_id: contracts::CapacityClaimId,
        expected_revision: contracts::Revision,
        diagnostic_code: &str,
    ) -> Result<CapacityClaim, ResourceStoreError> {
        if !valid_diagnostic(diagnostic_code) {
            return Err(ResourceStoreError::DiagnosticInvalid);
        }
        let mut transaction = self.pool.begin().await?;
        let claim = load_locked_claim(&mut transaction, claim_id).await?;
        if claim.revision != expected_revision || claim.state != CapacityClaimState::Provisioning {
            return Err(ResourceStoreError::CapacityClaimStateConflict);
        }
        let attempt: i64 = sqlx::query_scalar("SELECT count(*)::bigint + 1 FROM resource.capacity_attempts WHERE claim_id=$1 AND step='provision_quota'")
            .bind(claim_id.as_uuid()).fetch_one(&mut *transaction).await?;
        sqlx::query("INSERT INTO resource.capacity_attempts (claim_id,attempt,step,state,diagnostic_code) VALUES ($1,$2,'provision_quota','retry',$3)")
            .bind(claim_id.as_uuid()).bind(attempt).bind(diagnostic_code).execute(&mut *transaction).await?;
        let target = if attempt >= 3 {
            CapacityClaimState::Blocked
        } else {
            CapacityClaimState::Reserved
        };
        let next = transition_claim(&claim, target)?;
        update_claim(&mut transaction, &claim, &next, None, None, None).await?;
        sqlx::query(
            "UPDATE resource.capacity_claims SET last_diagnostic_code=$2 WHERE claim_id=$1",
        )
        .bind(claim_id.as_uuid())
        .bind(diagnostic_code)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(next)
    }
}

async fn calculate_charge(
    transaction: &mut Transaction<'_, Postgres>,
    usage: &ResourceUsageRecord,
) -> Result<Option<ResourceCharge>, ResourceStoreError> {
    let UsageMeasurement::Known { quantities } = &usage.measurement else {
        return Ok(None);
    };
    let mut lines = Vec::new();
    let mut currency: Option<String> = None;

    if usage.kind == ResourceUsageKind::Compute {
        if quantities.cpu_millicore_seconds > 0 {
            append_rate_segments(
                transaction,
                usage,
                ResourceBillingUnit::CpuMillicoreSecond,
                quantities.cpu_millicore_seconds,
                None,
                None,
                &mut lines,
                &mut currency,
            )
            .await?;
        }
        if quantities.memory_byte_seconds > 0 {
            append_rate_segments(
                transaction,
                usage,
                ResourceBillingUnit::MemoryByteSecond,
                quantities.memory_byte_seconds,
                None,
                None,
                &mut lines,
                &mut currency,
            )
            .await?;
        }
        if quantities.gpu_unit_seconds > 0 {
            let allocation = load_usage_gpu_allocation(transaction, usage).await?;
            append_rate_segments(
                transaction,
                usage,
                ResourceBillingUnit::GpuUnitSecond,
                quantities.gpu_unit_seconds,
                Some(&allocation.class),
                Some(allocation.mode),
                &mut lines,
                &mut currency,
            )
            .await?;
        }
    } else if quantities.storage_byte_seconds > 0 {
        append_rate_segments(
            transaction,
            usage,
            ResourceBillingUnit::StorageByteSecond,
            quantities.storage_byte_seconds,
            None,
            None,
            &mut lines,
            &mut currency,
        )
        .await?;
    }

    if lines.is_empty() {
        return Ok(None);
    }
    let currency = currency.ok_or(ResourceStoreError::RateCurrencyConflict)?;
    let total_scaled = lines.iter().try_fold(0_i128, |total, line| {
        total
            .checked_add(line.amount.amount.scaled())
            .ok_or(ResourceStoreError::NumericOverflow)
    })?;
    let total = Money {
        currency,
        amount: FixedDecimal::from_scaled(total_scaled)?,
    };
    Ok(Some(ResourceCharge {
        id: ChargeId::new(),
        usage_record_id: usage.id,
        project_id: usage.project_id,
        course_id: usage.course_id,
        lines,
        total,
        settlement: UsageSettlementState::Settled,
        // The settle transaction replaces this with the database timestamp.
        created_at: usage.observed_at,
        adjustment_of: None,
        adjustment_reason: None,
        adjusted_by: None,
        diagnostic_code: None,
    }))
}

#[allow(clippy::too_many_arguments)]
async fn append_rate_segments(
    transaction: &mut Transaction<'_, Postgres>,
    usage: &ResourceUsageRecord,
    unit: ResourceBillingUnit,
    quantity: u64,
    gpu_class: Option<&str>,
    gpu_mode: Option<GpuAllocationMode>,
    lines: &mut Vec<ResourceChargeLine>,
    currency: &mut Option<String>,
) -> Result<(), ResourceStoreError> {
    let segments = select_rate_segments(transaction, unit, gpu_class, gpu_mode, usage).await?;
    let total_millis =
        (usage.measured_until.get() - usage.measured_from.get()).whole_milliseconds();
    if total_millis <= 0 {
        return Err(ResourceStoreError::NumericOverflow);
    }
    let total_millis =
        u128::try_from(total_millis).map_err(|_| ResourceStoreError::NumericOverflow)?;
    let mut allocated = 0_u64;
    for (index, (rate, segment_millis)) in segments.iter().enumerate() {
        let segment_quantity = if index + 1 == segments.len() {
            quantity
                .checked_sub(allocated)
                .ok_or(ResourceStoreError::NumericOverflow)?
        } else {
            let product = u128::from(quantity)
                .checked_mul(*segment_millis)
                .ok_or(ResourceStoreError::NumericOverflow)?;
            u64::try_from(product / total_millis)
                .map_err(|_| ResourceStoreError::NumericOverflow)?
        };
        allocated = allocated
            .checked_add(segment_quantity)
            .ok_or(ResourceStoreError::NumericOverflow)?;
        append_charge_line(unit, segment_quantity, rate, lines, currency)?;
    }
    if allocated != quantity {
        return Err(ResourceStoreError::NumericOverflow);
    }
    Ok(())
}

fn append_charge_line(
    unit: ResourceBillingUnit,
    quantity: u64,
    rate: &ResourceRate,
    lines: &mut Vec<ResourceChargeLine>,
    currency: &mut Option<String>,
) -> Result<(), ResourceStoreError> {
    if quantity == 0 {
        return Ok(());
    }
    if let Some(expected) = currency.as_ref()
        && expected != &rate.unit_price.currency
    {
        return Err(ResourceStoreError::RateCurrencyConflict);
    }
    *currency = Some(rate.unit_price.currency.clone());
    let amount = multiply_money(&rate.unit_price, quantity, rate.unit_quantity)?;
    lines.push(ResourceChargeLine {
        rate_id: rate.id,
        rate_revision: rate.revision,
        unit,
        quantity,
        unit_quantity: rate.unit_quantity,
        unit_price: rate.unit_price.clone(),
        amount,
    });
    Ok(())
}

async fn select_rate_segments(
    transaction: &mut Transaction<'_, Postgres>,
    unit: ResourceBillingUnit,
    gpu_class: Option<&str>,
    gpu_mode: Option<GpuAllocationMode>,
    usage: &ResourceUsageRecord,
) -> Result<Vec<(ResourceRate, u128)>, ResourceStoreError> {
    let unit = wire(unit)?;
    let gpu_mode = gpu_mode.map(wire).transpose()?;
    let rows = sqlx::query(
        "SELECT contract FROM resource.resource_rates
         WHERE unit=$1 AND gpu_class IS NOT DISTINCT FROM $2
           AND gpu_mode IS NOT DISTINCT FROM $3
           AND effective_from < $4
           AND (effective_until IS NULL OR effective_until > $5)
         ORDER BY effective_from ASC, revision ASC, rate_id ASC",
    )
    .bind(&unit)
    .bind(gpu_class)
    .bind(gpu_mode.as_deref())
    .bind(usage.measured_until.get())
    .bind(usage.measured_from.get())
    .fetch_all(&mut **transaction)
    .await?;
    if rows.is_empty() {
        return Err(ResourceStoreError::RateUnconfigured);
    }
    let from = usage.measured_from.get();
    let until = usage.measured_until.get();
    let mut cursor = from;
    let mut segments = Vec::with_capacity(rows.len());
    for row in rows {
        let rate = decode_rate(row.try_get("contract")?)?;
        let segment_from = max(from, rate.effective_from.get());
        let segment_until = min(until, rate.effective_until.map_or(until, UtcTimestamp::get));
        if segment_until <= segment_from {
            continue;
        }
        if segment_from < cursor {
            return Err(ResourceStoreError::RateOverlap);
        }
        if segment_from > cursor {
            return Err(ResourceStoreError::RateUnconfigured);
        }
        let duration = (segment_until - segment_from).whole_milliseconds();
        let duration = u128::try_from(duration).map_err(|_| ResourceStoreError::NumericOverflow)?;
        if duration == 0 {
            return Err(ResourceStoreError::NumericOverflow);
        }
        segments.push((rate, duration));
        cursor = segment_until;
    }
    if cursor != until {
        return Err(ResourceStoreError::RateUnconfigured);
    }
    Ok(segments)
}

async fn load_usage_gpu_allocation(
    transaction: &mut Transaction<'_, Postgres>,
    usage: &ResourceUsageRecord,
) -> Result<GpuAllocation, ResourceStoreError> {
    let row = if let Some(lease_id) = usage.lease_id {
        sqlx::query(
            "SELECT c.contract FROM resource.capacity_claims c
             JOIN resource.resource_leases l ON l.claim_id=c.claim_id
             WHERE l.lease_id=$1 AND c.request_id=$2",
        )
        .bind(lease_id.as_uuid())
        .bind(usage.request_id.as_uuid())
        .fetch_optional(&mut **transaction)
        .await?
    } else {
        sqlx::query(
            "SELECT contract FROM resource.capacity_claims
             WHERE request_id=$1 ORDER BY created_at DESC, claim_id DESC LIMIT 1",
        )
        .bind(usage.request_id.as_uuid())
        .fetch_optional(&mut **transaction)
        .await?
    };
    let Some(row) = row else {
        return Err(ResourceStoreError::GpuAllocationMissing);
    };
    decode_claim(row.try_get("contract")?)?
        .gpu_allocation
        .ok_or(ResourceStoreError::GpuAllocationMissing)
}

fn multiply_money(
    unit_price: &Money,
    quantity: u64,
    unit_quantity: u64,
) -> Result<Money, ResourceStoreError> {
    if unit_quantity == 0 {
        return Err(ResourceStoreError::NumericOverflow);
    }
    let price = Decimal::from_str(unit_price.amount.as_str())
        .map_err(|_| ResourceStoreError::NumericOverflow)?;
    let amount = Decimal::from(quantity)
        .checked_mul(price)
        .ok_or(ResourceStoreError::NumericOverflow)?
        .checked_div(Decimal::from(unit_quantity))
        .ok_or(ResourceStoreError::NumericOverflow)?
        .round_dp_with_strategy(6, RoundingStrategy::MidpointNearestEven);
    Ok(Money {
        currency: unit_price.currency.clone(),
        amount: FixedDecimal::parse(&format!("{amount:.6}"))?,
    })
}

async fn update_usage_settlement(
    transaction: &mut Transaction<'_, Postgres>,
    usage: &ResourceUsageRecord,
    settlement: UsageSettlementState,
) -> Result<(), ResourceStoreError> {
    let mut next = usage.clone();
    next.settlement = settlement;
    next.validate().map_err(ResourceStoreError::Contract)?;
    let changed = sqlx::query(
        "UPDATE resource.resource_usage_records
         SET settlement=$2, contract=$3 WHERE usage_record_id=$1",
    )
    .bind(usage.id.as_uuid())
    .bind(wire(settlement)?)
    .bind(serde_json::to_value(&next)?)
    .execute(&mut **transaction)
    .await?;
    if changed.rows_affected() != 1 {
        return Err(ResourceStoreError::UsageNotFound);
    }
    Ok(())
}

async fn apply_budget_delta(
    transaction: &mut Transaction<'_, Postgres>,
    project_id: ProjectId,
    delta: &Money,
) -> Result<(), ResourceStoreError> {
    let Some(row) = sqlx::query(
        "SELECT contract FROM resource.resource_budgets WHERE project_id=$1 FOR UPDATE",
    )
    .bind(project_id.as_uuid())
    .fetch_optional(&mut **transaction)
    .await?
    else {
        // Budgets are optional; the charge remains authoritative even when a
        // project has not configured a reminder threshold.
        return Ok(());
    };
    let current = decode_budget(row.try_get("contract")?)?;
    if current.limit.currency != delta.currency {
        return Err(ResourceStoreError::BudgetCurrencyConflict);
    }
    let spent_scaled = current
        .spent
        .amount
        .scaled()
        .checked_add(delta.amount.scaled())
        .ok_or(ResourceStoreError::NumericOverflow)?;
    if spent_scaled < 0 {
        return Err(ResourceStoreError::BudgetSpendInvalid);
    }
    let next = ResourceBudget {
        id: current.id,
        project_id: current.project_id,
        course_id: current.course_id,
        limit: current.limit,
        warning_at: current.warning_at,
        spent: Money {
            currency: delta.currency.clone(),
            amount: FixedDecimal::from_scaled(spent_scaled)?,
        },
        revision: Revision::new(
            current
                .revision
                .get()
                .checked_add(1)
                .ok_or(ResourceStoreError::RevisionOverflow)?,
        )?,
        updated_at: database_now(transaction).await?,
    };
    next.validate().map_err(ResourceStoreError::Contract)?;
    sqlx::query(
        "UPDATE resource.resource_budgets
         SET spent_amount=$2, revision=$3, updated_at=$4, contract=$5
         WHERE project_id=$1",
    )
    .bind(project_id.as_uuid())
    .bind(next.spent.amount.as_str())
    .bind(i64::try_from(next.revision.get())?)
    .bind(next.updated_at.get())
    .bind(serde_json::to_value(&next)?)
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

async fn insert_request(
    transaction: &mut Transaction<'_, Postgres>,
    request: &ResourceRequest,
) -> Result<(), ResourceStoreError> {
    let gpu = request.requested_resources.gpu.as_ref();
    let (target_kind, task_run_id, environment_id, release_id, release_version) =
        match request.target {
            ResourceTarget::Environment {
                environment_id,
                release_id,
                release_version,
            } => (
                "environment",
                None,
                Some(environment_id.as_uuid()),
                Some(release_id.as_uuid()),
                Some(i64::try_from(release_version)?),
            ),
            ResourceTarget::Task { task_run_id } => {
                ("task", Some(task_run_id.as_uuid()), None, None, None)
            }
        };
    sqlx::query(
        "INSERT INTO resource.resource_requests (request_id,generation,request_key,requester_id,course_id,project_id,target_kind,task_run_id,environment_id,release_id,release_version,requested_cpu_millicores,requested_memory_bytes,requested_storage_bytes,gpu_class,gpu_count,requested_duration_seconds,state,revision,diagnostic_code,created_at,updated_at,contract) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21,$22,$23)",
    )
    .bind(request.id.as_uuid()).bind(i64::try_from(request.generation)?).bind(&request.request_key)
    .bind(request.requester_id.as_uuid()).bind(request.course_id.map(contracts::CourseId::as_uuid)).bind(request.project_id.as_uuid())
    .bind(target_kind).bind(task_run_id).bind(environment_id).bind(release_id).bind(release_version)
    .bind(i32::try_from(request.requested_resources.cpu_millicores)?).bind(i64::try_from(request.requested_resources.memory_bytes)?).bind(i64::try_from(request.requested_resources.storage_bytes)?)
    .bind(gpu.map(|value| value.class.as_str())).bind(gpu.map(|value| i32::try_from(value.count)).transpose()?)
    .bind(i64::try_from(request.requested_duration_seconds)?).bind(wire(request.state)?).bind(i64::try_from(request.revision.get())?).bind(&request.diagnostic_code)
    .bind(request.created_at.get()).bind(request.updated_at.get()).bind(serde_json::to_value(request)?)
    .execute(&mut **transaction).await?;
    Ok(())
}

/// Resolves a GPU request against the active Resource catalog while holding
/// the candidate catalog rows.  The reservation is inserted in the same
/// transaction as the approval, so concurrent approvals cannot oversubscribe
/// an observed device pool.
async fn lock_gpu_admission(
    transaction: &mut Transaction<'_, Postgres>,
) -> Result<(), ResourceStoreError> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(GPU_ADMISSION_LOCK)
        .execute(&mut **transaction)
        .await?;
    Ok(())
}

async fn resolve_gpu_allocation(
    transaction: &mut Transaction<'_, Postgres>,
    provider_binding: &str,
    request: Option<&contracts::resource::GpuRequest>,
) -> Result<Option<GpuAllocation>, ResourceStoreError> {
    let Some(request) = request else {
        return Ok(None);
    };
    lock_gpu_admission(transaction).await?;
    let mut rows = sqlx::query(
        "SELECT entry_id,class,mode,provider_binding,capacity_units,allocation_binding,revision \
         FROM resource.gpu_catalog_entries \
         WHERE class=$1 AND provider_binding=$2 AND active \
         ORDER BY revision DESC, entry_id FOR UPDATE",
    )
    .bind(&request.class)
    .bind(provider_binding)
    .fetch_all(&mut **transaction)
    .await?;
    if rows.len() > 1 {
        tracing::error!(
            class = %request.class,
            provider_binding,
            active_catalog_entries = rows.len(),
            event = "resource.gpu_catalog.ambiguous",
            diagnostic_code = "LW_RESOURCE_GPU_CATALOG_AMBIGUOUS",
        );
        return Err(ResourceStoreError::GpuCatalogAmbiguous);
    }
    let Some(row) = rows.pop() else {
        return Err(ResourceStoreError::GpuCatalogMissing);
    };
    let entry_id: uuid::Uuid = row.try_get("entry_id")?;
    let mode: String = row.try_get("mode")?;
    let mode = serde_json::from_value::<GpuAllocationMode>(Value::String(mode))
        .map_err(|_| ResourceStoreError::GpuCatalogInvalid)?;
    let catalog_revision: i64 = row.try_get("revision")?;
    let class: String = row.try_get("class")?;
    let provider: String = row.try_get("provider_binding")?;
    let allocation_binding: String = row.try_get("allocation_binding")?;
    let capacity = i64::from(row.try_get::<i32, _>("capacity_units")?);
    let available: Option<i64> = sqlx::query_scalar(
        "SELECT available_units::bigint FROM resource.gpu_capacity_observations \
         WHERE entry_id=$1 AND valid_until > clock_timestamp() \
         ORDER BY observed_at DESC LIMIT 1",
    )
    .bind(entry_id)
    .fetch_optional(&mut **transaction)
    .await?;
    // A catalog entry without a fresh provider observation cannot authorize a
    // claim.  The catalog capacity is a policy limit, never a substitute for
    // current provider inventory.
    let Some(available) = available else {
        return Err(ResourceStoreError::GpuObservationStale);
    };
    let reserved: i64 = sqlx::query_scalar(
        "SELECT COALESCE(sum(reservation.units),0)::bigint
         FROM resource.gpu_capacity_reservations reservation
         JOIN resource.gpu_catalog_entries catalog
           ON catalog.entry_id=reservation.entry_id
         WHERE reservation.state='reserved'
           AND catalog.allocation_binding=$1",
    )
    .bind(&allocation_binding)
    .fetch_one(&mut **transaction)
    .await?;
    let requested = i64::from(request.count);
    if available.min(capacity).saturating_sub(reserved) < requested {
        return Err(ResourceStoreError::GpuCapacityExhausted);
    }
    let catalog_revision = Revision::new(u64::try_from(catalog_revision)?)?;
    let allocation = GpuAllocation {
        entry_id: GpuCatalogEntryId::from_str(&entry_id.to_string())
            .map_err(|_| ResourceStoreError::GpuCatalogInvalid)?,
        class,
        count: request.count,
        mode,
        provider_binding: provider,
        allocation_binding,
        catalog_revision,
    };
    allocation
        .validate()
        .map_err(|_| ResourceStoreError::GpuCatalogInvalid)?;
    Ok(Some(allocation))
}

async fn insert_gpu_reservation(
    transaction: &mut Transaction<'_, Postgres>,
    claim_id: contracts::CapacityClaimId,
    allocation: &GpuAllocation,
) -> Result<(), ResourceStoreError> {
    sqlx::query(
        "INSERT INTO resource.gpu_capacity_reservations
         (reservation_id,claim_id,entry_id,units,state)
         VALUES ($1,$2,$3,$4,'reserved')",
    )
    .bind(uuid::Uuid::now_v7())
    .bind(claim_id.as_uuid())
    .bind(allocation.entry_id.as_uuid())
    .bind(i32::try_from(allocation.count)?)
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

async fn insert_approval(
    transaction: &mut Transaction<'_, Postgres>,
    approval: &ResourceApproval,
) -> Result<(), ResourceStoreError> {
    let gpu = approval.approved_resources.gpu.as_ref();
    sqlx::query("INSERT INTO resource.resource_approvals (approval_id,request_id,request_revision,approver_id,provider_binding,approved_cpu_millicores,approved_memory_bytes,approved_storage_bytes,approved_gpu_class,approved_gpu_count,approved_duration_seconds,reason,valid_until,created_at,contract) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15)")
        .bind(approval.id.as_uuid()).bind(approval.request_id.as_uuid()).bind(i64::try_from(approval.request_revision.get())?).bind(approval.approver_id.as_uuid()).bind(&approval.provider_binding)
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
    let gpu_allocation = claim.gpu_allocation.as_ref();
    sqlx::query("INSERT INTO resource.capacity_claims (claim_id,request_id,approval_id,provider_binding,state,revision,created_at,updated_at,workload_cpu_millicores,workload_memory_bytes,workload_storage_bytes,workload_gpu_class,workload_gpu_count,quota_cpu_millicores,quota_memory_bytes,quota_storage_bytes,quota_gpu_class,quota_gpu_count,gpu_catalog_entry_id,gpu_mode,gpu_allocation_binding,contract) VALUES ($1,$2,$3,$4,$5,$6,clock_timestamp(),clock_timestamp(),$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20)")
        .bind(claim.id.as_uuid()).bind(claim.request_id.as_uuid()).bind(claim.approval_id.as_uuid()).bind(&claim.provider_binding).bind(wire(claim.state)?).bind(i64::try_from(claim.revision.get())?)
        .bind(i32::try_from(claim.workload_resources.cpu_millicores)?).bind(i64::try_from(claim.workload_resources.memory_bytes)?).bind(i64::try_from(claim.workload_resources.storage_bytes)?)
        .bind(workload_gpu.map(|value| value.class.as_str())).bind(workload_gpu.map(|value| i32::try_from(value.count)).transpose()?)
        .bind(i32::try_from(claim.quota_resources.cpu_millicores)?).bind(i64::try_from(claim.quota_resources.memory_bytes)?).bind(i64::try_from(claim.quota_resources.storage_bytes)?)
        .bind(quota_gpu.map(|value| value.class.as_str())).bind(quota_gpu.map(|value| i32::try_from(value.count)).transpose()?)
        .bind(gpu_allocation.map(|value| value.entry_id.as_uuid()))
        .bind(gpu_allocation.map(|value| wire(value.mode)).transpose()?)
        .bind(gpu_allocation.map(|value| value.allocation_binding.as_str()))
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

async fn insert_charge(
    transaction: &mut Transaction<'_, Postgres>,
    charge: &ResourceCharge,
) -> Result<(), ResourceStoreError> {
    sqlx::query(
        "INSERT INTO resource.resource_charges
         (charge_id,usage_record_id,project_id,course_id,lines,currency,total_amount,
          settlement,adjustment_of,adjustment_reason,adjusted_by,diagnostic_code,
          created_at,contract)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14)",
    )
    .bind(charge.id.as_uuid())
    .bind(charge.usage_record_id.as_uuid())
    .bind(charge.project_id.as_uuid())
    .bind(charge.course_id.map(contracts::CourseId::as_uuid))
    .bind(serde_json::to_value(&charge.lines)?)
    .bind(&charge.total.currency)
    .bind(charge.total.amount.as_str())
    .bind(wire(charge.settlement)?)
    .bind(charge.adjustment_of.map(ChargeId::as_uuid))
    .bind(charge.adjustment_reason.as_deref())
    .bind(charge.adjusted_by.map(contracts::ActorId::as_uuid))
    .bind(charge.diagnostic_code.as_deref())
    .bind(charge.created_at.get())
    .bind(serde_json::to_value(charge)?)
    .execute(&mut **transaction)
    .await?;
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

async fn load_locked_claim(
    transaction: &mut Transaction<'_, Postgres>,
    claim_id: contracts::CapacityClaimId,
) -> Result<CapacityClaim, ResourceStoreError> {
    let row =
        sqlx::query("SELECT contract FROM resource.capacity_claims WHERE claim_id=$1 FOR UPDATE")
            .bind(claim_id.as_uuid())
            .fetch_optional(&mut **transaction)
            .await?
            .ok_or(ResourceStoreError::CapacityClaimNotFound)?;
    decode_claim(row.try_get("contract")?)
}

async fn update_claim(
    transaction: &mut Transaction<'_, Postgres>,
    current: &CapacityClaim,
    next: &CapacityClaim,
    namespace: Option<&str>,
    namespace_uid: Option<&str>,
    quota_uid: Option<&str>,
) -> Result<(), ResourceStoreError> {
    let changed = sqlx::query("UPDATE resource.capacity_claims SET state=$3,revision=$4,namespace_name=COALESCE($5,namespace_name),namespace_uid=COALESCE($6,namespace_uid),quota_uid=COALESCE($7,quota_uid),updated_at=clock_timestamp(),contract=$8 WHERE claim_id=$1 AND revision=$2")
        .bind(current.id.as_uuid()).bind(i64::try_from(current.revision.get())?).bind(wire(next.state)?).bind(i64::try_from(next.revision.get())?)
        .bind(namespace).bind(namespace_uid).bind(quota_uid).bind(serde_json::to_value(next)?)
        .execute(&mut **transaction).await?;
    if changed.rows_affected() != 1 {
        return Err(ResourceStoreError::RevisionConflict);
    }
    Ok(())
}

fn transition_claim(
    claim: &CapacityClaim,
    state: CapacityClaimState,
) -> Result<CapacityClaim, ResourceStoreError> {
    let valid = matches!(
        (claim.state, state),
        (
            CapacityClaimState::Reserved,
            CapacityClaimState::Provisioning
        ) | (
            CapacityClaimState::Provisioning,
            CapacityClaimState::Reserved
                | CapacityClaimState::Ready
                | CapacityClaimState::Blocked
                | CapacityClaimState::HandedOff
        ) | (
            CapacityClaimState::Ready,
            CapacityClaimState::HandedOff | CapacityClaimState::Blocked
        ) | (
            CapacityClaimState::Reserved
                | CapacityClaimState::Provisioning
                | CapacityClaimState::Blocked
                | CapacityClaimState::HandedOff,
            CapacityClaimState::Releasing
        ) | (
            CapacityClaimState::Releasing,
            CapacityClaimState::Released | CapacityClaimState::Blocked
        )
    );
    if !valid {
        return Err(ResourceStoreError::CapacityClaimStateConflict);
    }
    let mut next = claim.clone();
    next.state = state;
    next.revision = contracts::Revision::new(
        claim
            .revision
            .get()
            .checked_add(1)
            .ok_or(ResourceStoreError::RevisionOverflow)?,
    )?;
    Ok(next)
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
    let contract = event_contract(subject)?;
    let event_id = EventId::new();
    let payload = serde_json::to_value(CloudEvent {
        specversion: contracts::events::SPEC_VERSION.into(),
        id: event_id,
        source: contract.source().into(),
        event_type: contract.event_type.into(),
        subject: contract.subject.into(),
        time: request.updated_at,
        datacontenttype: "application/json".into(),
        dataschema: contract.data_schema(),
        project_id: request.project_id,
        course_id: request.course_id,
        aggregate_revision: request.revision,
        aggregate_sequence: Sequence(request.revision.get()),
        trace_id: trace_id.into(),
        data: ResourceRequestChanged {
            request: request.clone(),
        },
    })?;
    let hash = Sha256Digest::of_canonical(&payload).map_err(|_| ResourceStoreError::Wire)?;
    OutboxStore::enqueue(
        transaction,
        Domain::Resource,
        event_id.as_uuid(),
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

async fn enqueue_lease_event(
    transaction: &mut Transaction<'_, Postgres>,
    lease: &ResourceLease,
    request: &ResourceRequest,
    subject: &str,
    trace_id: &str,
) -> Result<(), ResourceStoreError> {
    let contract = event_contract(subject)?;
    let event_id = EventId::new();
    let payload = serde_json::to_value(CloudEvent {
        specversion: contracts::events::SPEC_VERSION.into(),
        id: event_id,
        source: contract.source().into(),
        event_type: contract.event_type.into(),
        subject: contract.subject.into(),
        time: lease.updated_at,
        datacontenttype: "application/json".into(),
        dataschema: contract.data_schema(),
        project_id: request.project_id,
        course_id: request.course_id,
        aggregate_revision: lease.revision,
        aggregate_sequence: Sequence(lease.revision.get()),
        trace_id: trace_id.into(),
        data: ResourceLeaseChanged {
            lease: lease.clone(),
            request: request.clone(),
        },
    })?;
    let hash = Sha256Digest::of_canonical(&payload).map_err(|_| ResourceStoreError::Wire)?;
    OutboxStore::enqueue(
        transaction,
        Domain::Resource,
        event_id.as_uuid(),
        contract.subject,
        contract.event_type,
        lease.id.as_uuid(),
        lease.revision.get(),
        &payload,
        hash,
    )
    .await?;
    Ok(())
}

fn event_contract(subject: &str) -> Result<contracts::events::EventContract, ResourceStoreError> {
    EVENT_CONTRACTS
        .iter()
        .copied()
        .find(|contract| contract.subject == subject)
        .ok_or(ResourceStoreError::EventContractInvalid)
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

fn task_resource_status(
    request: ResourceRequest,
    claim: CapacityClaim,
    lease: ResourceLease,
    execution_namespace: Option<String>,
) -> TaskResourceStatus {
    let cleanup_confirmed = request.state.is_terminal()
        && claim.state == CapacityClaimState::Released
        && matches!(
            lease.state,
            ResourceLeaseState::Expired | ResourceLeaseState::Revoked
        );
    TaskResourceStatus {
        task_run_id: match &request.target {
            ResourceTarget::Task { task_run_id } => *task_run_id,
            ResourceTarget::Environment { .. } => {
                // All callers validate the Task target before constructing the public status.
                // Keep this branch explicit so a future caller cannot accidentally fabricate the
                // task identity from an unrelated Environment request.
                unreachable!("task resource status requires a Task target")
            }
        },
        project_id: request.project_id,
        owner_id: request.requester_id,
        execution_namespace,
        claim_revision: claim.revision,
        lease_revision: lease.revision,
        cleanup_confirmed,
        request,
        claim,
        lease,
    }
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

fn decode_lease_synced_revision(
    row: &sqlx::postgres::PgRow,
) -> Result<Option<Revision>, ResourceStoreError> {
    let value: i64 = row.try_get("lease_synced_revision")?;
    let value = u64::try_from(value)?;
    if value == 0 {
        Ok(None)
    } else {
        Ok(Some(Revision::new(value)?))
    }
}

fn decode_usage(value: Value) -> Result<ResourceUsageRecord, ResourceStoreError> {
    let usage: ResourceUsageRecord = serde_json::from_value(value)?;
    usage.validate()?;
    Ok(usage)
}

fn same_usage_intent(left: &ResourceUsageRecord, right: &ResourceUsageRecord) -> bool {
    left.project_id == right.project_id
        && left.course_id == right.course_id
        && left.kind == right.kind
        && left.request_id == right.request_id
        && left.lease_id == right.lease_id
        && left.source_event_id == right.source_event_id
        && left.measured_from == right.measured_from
        && left.measured_until == right.measured_until
        && left.measurement == right.measurement
}
fn decode_rate(value: Value) -> Result<ResourceRate, ResourceStoreError> {
    let rate: ResourceRate = serde_json::from_value(value)?;
    rate.validate()?;
    Ok(rate)
}
fn decode_gpu_catalog(value: Value) -> Result<GpuCatalogEntry, ResourceStoreError> {
    let entry: GpuCatalogEntry = serde_json::from_value(value)?;
    entry.validate()?;
    Ok(entry)
}
fn decode_budget(value: Value) -> Result<ResourceBudget, ResourceStoreError> {
    let budget: ResourceBudget = serde_json::from_value(value)?;
    budget.validate()?;
    Ok(budget)
}
fn decode_charge(value: Value) -> Result<ResourceCharge, ResourceStoreError> {
    let charge: ResourceCharge = serde_json::from_value(value)?;
    charge.validate()?;
    Ok(charge)
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
fn valid_namespace_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 63
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && !value.starts_with('-')
        && !value.ends_with('-')
}
fn valid_diagnostic(value: &str) -> bool {
    value.len() <= 128
        && value.starts_with("LW_")
        && value
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
}

pub(crate) fn resource_error_kind(error: &ResourceStoreError) -> &'static str {
    match error {
        ResourceStoreError::Database(_) => "database",
        ResourceStoreError::Persistence(_) => "persistence",
        ResourceStoreError::Serialization(_) => "serialization",
        ResourceStoreError::Contract(_) => "contract",
        ResourceStoreError::Lifecycle(_) => "lifecycle",
        ResourceStoreError::Foundation(_) => "foundation",
        ResourceStoreError::RateUnconfigured => "configuration",
        _ => "resource",
    }
}

pub(crate) fn resource_error_safe_detail(error: &ResourceStoreError) -> String {
    match error {
        ResourceStoreError::Database(error) => safe_sqlstate_detail(error),
        ResourceStoreError::RateUnconfigured => "rate_unconfigured".to_owned(),
        ResourceStoreError::Contract(_) => "contract_invalid".to_owned(),
        ResourceStoreError::Lifecycle(_) => "lifecycle_failed".to_owned(),
        ResourceStoreError::Foundation(_) => "foundation_invalid".to_owned(),
        ResourceStoreError::Persistence(_) => "persistence_failed".to_owned(),
        ResourceStoreError::Serialization(_) => "serialization_failed".to_owned(),
        ResourceStoreError::Numeric(_) | ResourceStoreError::NumericOverflow => {
            "numeric_overflow".to_owned()
        }
        _ => "resource_operation_failed".to_owned(),
    }
}

pub(crate) fn safe_sqlstate_detail(error: &sqlx::Error) -> String {
    let code = error
        .as_database_error()
        .and_then(sqlx::error::DatabaseError::code);
    safe_sqlstate_value(code.as_deref())
}

fn safe_sqlstate_value(code: Option<&str>) -> String {
    let Some(code) = code
        .filter(|code| code.len() == 5)
        .filter(|code| code.bytes().all(|byte| byte.is_ascii_alphanumeric()))
    else {
        return "redacted_unclassified".to_owned();
    };
    format!("sqlstate_{}", code.to_ascii_lowercase())
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
    #[error("LW_RESOURCE_GPU_CATALOG_MISSING")]
    GpuCatalogMissing,
    #[error("LW_RESOURCE_GPU_CATALOG_AMBIGUOUS")]
    GpuCatalogAmbiguous,
    #[error("LW_RESOURCE_GPU_CATALOG_INVALID")]
    GpuCatalogInvalid,
    #[error("LW_RESOURCE_GPU_CAPACITY_EXHAUSTED")]
    GpuCapacityExhausted,
    #[error("LW_RESOURCE_LEASE_WINDOW_MISSING")]
    LeaseWindowMissing,
    #[error("LW_RESOURCE_LEASE_NOT_FOUND")]
    LeaseNotFound,
    #[error("LW_RESOURCE_SCOPE_CONFLICT")]
    ScopeConflict,
    #[error("LW_RESOURCE_USAGE_CONFLICT")]
    UsageConflict,
    #[error("LW_RESOURCE_USAGE_NOT_FOUND")]
    UsageNotFound,
    #[error("LW_RESOURCE_USAGE_OVERLAP")]
    UsageOverlap,
    #[error("LW_RESOURCE_RATE_DIMENSION_INVALID")]
    RateDimensionInvalid,
    #[error("LW_RESOURCE_RATE_OVERLAP")]
    RateOverlap,
    #[error("LW_RESOURCE_RATE_SETTLED_CONFLICT")]
    RateSettledConflict,
    #[error("LW_RESOURCE_RATE_UNCONFIGURED")]
    RateUnconfigured,
    #[error("LW_RESOURCE_RATE_CURRENCY_CONFLICT")]
    RateCurrencyConflict,
    #[error("LW_RESOURCE_GPU_OBSERVATION_INVALID")]
    GpuObservationInvalid,
    #[error("LW_RESOURCE_GPU_OBSERVATION_STALE")]
    GpuObservationStale,
    #[error("LW_RESOURCE_GPU_ALLOCATION_MISSING")]
    GpuAllocationMissing,
    #[error("LW_RESOURCE_GPU_CATALOG_REVISION_CONFLICT")]
    GpuCatalogRevisionConflict,
    #[error("LW_RESOURCE_GPU_CATALOG_MODE_COLLISION")]
    GpuCatalogModeCollision,
    #[error("LW_RESOURCE_GPU_CATALOG_POOL_COLLISION")]
    GpuCatalogPoolCollision,
    #[error("LW_RESOURCE_GPU_CATALOG_MAPPING_CONFLICT")]
    GpuCatalogMappingConflict,
    #[error("LW_RESOURCE_BUDGET_NOT_FOUND")]
    BudgetNotFound,
    #[error("LW_RESOURCE_BUDGET_CURRENCY_CONFLICT")]
    BudgetCurrencyConflict,
    #[error("LW_RESOURCE_BUDGET_SPEND_INVALID")]
    BudgetSpendInvalid,
    #[error("LW_RESOURCE_CHARGE_NOT_FOUND")]
    ChargeNotFound,
    #[error("LW_RESOURCE_ADJUSTMENT_INVALID")]
    AdjustmentInvalid,
    #[error("LW_RESOURCE_CAPACITY_CLAIM_NOT_FOUND")]
    CapacityClaimNotFound,
    #[error("LW_RESOURCE_CAPACITY_CLAIM_STATE_CONFLICT")]
    CapacityClaimStateConflict,
    #[error("LW_RESOURCE_CAPACITY_READBACK_INVALID")]
    CapacityReadbackInvalid,
    #[error("LW_RESOURCE_DIAGNOSTIC_INVALID")]
    DiagnosticInvalid,
    #[error("LW_RESOURCE_REVISION_OVERFLOW")]
    RevisionOverflow,
    #[error("LW_RESOURCE_TRACE_INVALID")]
    Trace,
    #[error("LW_RESOURCE_EXECUTION_NAMESPACE_INVALID")]
    InvalidNamespace,
    #[error("LW_RESOURCE_APPROVAL_INVALID")]
    ApprovalInvalid,
    #[error("LW_RESOURCE_WIRE_INVALID")]
    Wire,
    #[error("LW_RESOURCE_EVENT_CONTRACT_INVALID")]
    EventContractInvalid,
    #[error("LW_RESOURCE_NUMERIC_OVERFLOW")]
    Numeric(#[from] std::num::TryFromIntError),
    #[error("LW_RESOURCE_NUMERIC_OVERFLOW")]
    NumericOverflow,
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

#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "fixed test values are valid by construction"
)]
mod tests {
    use contracts::resource::{CapacityClaim, CapacityClaimState, WorkloadResources};
    use contracts::{CapacityClaimId, ResourceApprovalId, ResourceRequestId, Revision};

    use super::{safe_sqlstate_value, transition_claim};

    #[test]
    fn blocked_pre_handoff_claim_can_enter_release_readback() {
        let claim = CapacityClaim {
            id: CapacityClaimId::new(),
            request_id: ResourceRequestId::new(),
            approval_id: ResourceApprovalId::new(),
            provider_binding: "kubernetes-standard".into(),
            workload_resources: WorkloadResources {
                cpu_millicores: 500,
                memory_bytes: 512 * 1024 * 1024,
                storage_bytes: 1024 * 1024 * 1024,
                gpu: None,
            },
            quota_resources: WorkloadResources {
                cpu_millicores: 500,
                memory_bytes: 512 * 1024 * 1024,
                storage_bytes: 1024 * 1024 * 1024,
                gpu: None,
            },
            gpu_allocation: None,
            state: CapacityClaimState::Blocked,
            revision: Revision::new(4).expect("fixed revision"),
        };
        let releasing = transition_claim(&claim, CapacityClaimState::Releasing);
        assert!(releasing.is_ok());
        assert_eq!(
            releasing.ok().map(|value| value.state),
            Some(CapacityClaimState::Releasing)
        );
    }

    #[test]
    fn safe_sqlstate_detail_keeps_database_context_without_error_text() {
        assert_eq!(safe_sqlstate_value(Some("22003")), "sqlstate_22003");
        assert_eq!(safe_sqlstate_value(Some("XX000")), "sqlstate_xx000");
        assert_eq!(
            safe_sqlstate_value(Some("database error with secret")),
            "redacted_unclassified"
        );
        assert_eq!(safe_sqlstate_value(None), "redacted_unclassified");
    }
}
