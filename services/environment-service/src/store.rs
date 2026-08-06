use std::str::FromStr;
use std::time::Duration;

use contracts::environment::{
    DesiredEnvironmentState, EnvironmentCreateSpec, EnvironmentInstance,
    EnvironmentLeaseAuthorization, EnvironmentOperation, EnvironmentOperationKind,
    ObservedEnvironmentState, OperationState,
};
use contracts::events::{
    CloudEvent, EVENT_CONTRACTS, EnvironmentEvent, EventContract, SPEC_VERSION, subjects,
};
use contracts::http::{EnvironmentOperationAccepted, IdempotencyKey};
use contracts::{
    CourseId, EnvironmentId, EventId, OperationId, Revision, Sequence, Sha256Digest, UtcTimestamp,
};
use persistence_sqlx::{
    Domain, IdempotencyDecision, IdempotencyStore, InboxDecision, InboxStore, OutboxStore,
    PersistenceError,
};
use serde::Serialize;
use serde_json::{Value, json};
use sqlx::{PgPool, Postgres, Row, Transaction};
use uuid::Uuid;

use crate::lifecycle::{LifecycleCommand, LifecycleError, plan_command_authorized};

/// One operation and aggregate reserved by a reconciler worker lease.
#[derive(Clone, Debug)]
pub struct LeasedEnvironment {
    pub instance: EnvironmentInstance,
    pub worker_id: String,
    lease_token: Uuid,
}

/// One actor-scoped inventory record with database and public-stream identity.
#[derive(Clone, Debug)]
pub struct StoredEnvironmentInventory {
    pub instance: EnvironmentInstance,
    pub created_at: UtcTimestamp,
    pub updated_at: UtcTimestamp,
    pub stream_sequence: contracts::StreamSequence,
}

/// Immutable delivery metadata for one lifecycle command received from a durable consumer.
#[derive(Clone, Debug)]
pub struct InboundLifecycleCommand {
    pub consumer: String,
    pub event_id: EventId,
    pub course_id: CourseId,
    pub aggregate_revision: Revision,
    pub aggregate_sequence: Sequence,
    pub idempotency_key: String,
    pub command: LifecycleCommand,
    pub create: Option<EnvironmentCreateSpec>,
    pub lease_authorization: Option<EnvironmentLeaseAuthorization>,
}

/// Durable Inbox decision and, only for the next event, its atomic lifecycle result.
#[derive(Clone, Debug, PartialEq)]
pub enum InboundCommandDecision {
    Applied(EnvironmentOperationAccepted),
    Duplicate,
    Stale,
    Gap,
}

/// PostgreSQL-authoritative Environment repository.
#[derive(Clone)]
pub struct PgEnvironmentStore {
    pool: PgPool,
}

impl PgEnvironmentStore {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Returns the database clock truncated to the contract's millisecond precision.
    pub async fn current_time(&self) -> Result<UtcTimestamp, EnvironmentStoreError> {
        let value: time::OffsetDateTime =
            sqlx::query_scalar("SELECT date_trunc('milliseconds', clock_timestamp())")
                .fetch_one(&self.pool)
                .await?;
        Ok(UtcTimestamp::from_utc(value)?)
    }

    /// Inserts an already validated create aggregate with its operation and Outbox event atomically.
    pub async fn create(
        &self,
        idempotency_key: &str,
        instance: &EnvironmentInstance,
    ) -> Result<EnvironmentOperationAccepted, EnvironmentStoreError> {
        let mut transaction = self.pool.begin().await?;
        let accepted = create_in_transaction(&mut transaction, idempotency_key, instance).await?;
        transaction.commit().await?;
        Ok(accepted)
    }

    /// Accepts a revisioned command, superseding an active operation only for cleanup commands.
    pub async fn accept_command(
        &self,
        idempotency_key: &str,
        command: &LifecycleCommand,
    ) -> Result<EnvironmentOperationAccepted, EnvironmentStoreError> {
        let mut transaction = self.pool.begin().await?;
        let accepted = accept_command_in_transaction(
            &mut transaction,
            idempotency_key,
            command,
            None,
            None,
            None,
        )
        .await?;
        transaction.commit().await?;
        Ok(accepted)
    }

    /// Accepts an authenticated public API command through the same transaction used by NATS.
    pub async fn accept_api_command(
        &self,
        idempotency_key: &str,
        command: &LifecycleCommand,
        create: Option<&EnvironmentCreateSpec>,
        course_id: CourseId,
    ) -> Result<EnvironmentOperationAccepted, EnvironmentStoreError> {
        let mut transaction = self.pool.begin().await?;
        let accepted = accept_command_in_transaction(
            &mut transaction,
            idempotency_key,
            command,
            create,
            None,
            Some(course_id),
        )
        .await?;
        transaction.commit().await?;
        Ok(accepted)
    }

    /// Applies the next durable event and its lifecycle mutation in one transaction.
    pub async fn accept_inbound_command(
        &self,
        inbound: &InboundLifecycleCommand,
    ) -> Result<InboundCommandDecision, EnvironmentStoreError> {
        let payload_hash = canonical_hash(&json!({
            "idempotencyKey": inbound.idempotency_key,
            "command": inbound.command,
            "create": inbound.create,
        }))?;
        if inbound.aggregate_revision != inbound.command.expected_revision {
            return Err(EnvironmentStoreError::InboundMetadataInvalid);
        }
        let mut transaction = self.pool.begin().await?;
        let decision = InboxStore::accept(
            &mut transaction,
            Domain::Environment,
            &inbound.consumer,
            inbound.event_id.as_uuid(),
            inbound.command.environment_id.as_uuid(),
            inbound.aggregate_sequence.0,
            payload_hash,
        )
        .await?;
        let result = match decision {
            InboxDecision::Accepted => InboundCommandDecision::Applied(
                accept_command_in_transaction(
                    &mut transaction,
                    &inbound.idempotency_key,
                    &inbound.command,
                    inbound.create.as_ref(),
                    inbound.lease_authorization.clone(),
                    Some(inbound.course_id),
                )
                .await?,
            ),
            InboxDecision::Duplicate => InboundCommandDecision::Duplicate,
            InboxDecision::Stale => InboundCommandDecision::Stale,
            InboxDecision::Gap => InboundCommandDecision::Gap,
        };
        transaction.commit().await?;
        Ok(result)
    }

    /// Loads one authoritative aggregate; missing and malformed rows fail closed.
    pub async fn load(
        &self,
        environment_id: EnvironmentId,
    ) -> Result<EnvironmentInstance, EnvironmentStoreError> {
        let row = sqlx::query(
            "SELECT contract FROM environment.environment_instances WHERE environment_id=$1",
        )
        .bind(environment_id.as_uuid())
        .fetch_optional(&self.pool)
        .await?
        .ok_or(EnvironmentStoreError::EnvironmentNotFound)?;
        decode_contract(row.try_get("contract")?)
    }

    /// Refreshes only the Resource-owned Lease fence of an existing Work
    /// aggregate. Immutable workload bindings and lifecycle intent are
    /// preserved; stale revisions and non-extending updates fail closed.
    pub async fn refresh_work_lease(
        &self,
        environment_id: EnvironmentId,
        authorization: contracts::environment::EnvironmentLeaseAuthorization,
    ) -> Result<EnvironmentInstance, EnvironmentStoreError> {
        let mut transaction = self.pool.begin().await?;
        let row = sqlx::query(
            "SELECT contract FROM environment.environment_instances \
             WHERE environment_id=$1 FOR UPDATE",
        )
        .bind(environment_id.as_uuid())
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or(EnvironmentStoreError::EnvironmentNotFound)?;
        let current = decode_contract(row.try_get("contract")?)?;
        if current
            .operation
            .lease_authorization
            .as_ref()
            .is_some_and(|value| {
                value.lease_id == authorization.lease_id
                    && value.lease_revision == authorization.lease_revision
                    && value.expires_at == authorization.expires_at
            })
        {
            transaction.commit().await?;
            return Ok(current);
        }
        if current.class != contracts::authoring::EnvironmentClass::Work
            || current.lease_id != Some(authorization.lease_id)
            || current.course_id != authorization.course_id
            || current.owner_id != authorization.owner_actor_id
            || current.capacity_binding.as_deref() != Some(authorization.capacity_binding.as_str())
            || authorization.lease_revision
                <= current
                    .operation
                    .lease_authorization
                    .as_ref()
                    .map_or(contracts::Revision::new(1)?, |value| value.lease_revision)
            || authorization.expires_at <= current.eligibility_expires_at
        {
            return Err(EnvironmentStoreError::LeaseAuthorizationInvalid);
        }
        let mut updated = current.clone();
        updated.revision = contracts::Revision::new(current.revision.get().checked_add(1).ok_or(
            EnvironmentStoreError::NumericOverflow("lease refresh revision"),
        )?)?;
        updated.eligibility_expires_at = authorization.expires_at;
        updated.operation.lease_authorization = Some(authorization);
        update_instance(&mut transaction, &current, &updated).await?;
        enqueue_environment_event(
            &mut transaction,
            &updated,
            subjects::ENVIRONMENT_STATE_CHANGED,
        )
        .await?;
        transaction.commit().await?;
        Ok(updated)
    }

    /// Loads an aggregate and the `PostgreSQL` authority clock from the same statement snapshot.
    pub async fn load_for_owner_resolution(
        &self,
        environment_id: EnvironmentId,
    ) -> Result<(EnvironmentInstance, UtcTimestamp), EnvironmentStoreError> {
        let row = sqlx::query(
            "SELECT contract, date_trunc('milliseconds', clock_timestamp()) AS authority_now \
             FROM environment.environment_instances WHERE environment_id=$1",
        )
        .bind(environment_id.as_uuid())
        .fetch_optional(&self.pool)
        .await?
        .ok_or(EnvironmentStoreError::EnvironmentNotFound)?;
        let instance = decode_contract(row.try_get("contract")?)?;
        let authority_now: time::OffsetDateTime = row.try_get("authority_now")?;
        Ok((instance, UtcTimestamp::from_utc(authority_now)?))
    }

    /// Claims one due operation with `FOR UPDATE SKIP LOCKED`; expired leases are recoverable.
    pub async fn claim_due(
        &self,
        worker_id: &str,
        lease_duration: Duration,
    ) -> Result<Option<LeasedEnvironment>, EnvironmentStoreError> {
        validate_worker(worker_id, lease_duration)?;
        let lease_milliseconds = lease_milliseconds(lease_duration)?;
        let lease_token = Uuid::now_v7();
        let mut transaction = self.pool.begin().await?;
        let row = sqlx::query(
            "WITH candidate AS ( \
               SELECT operation_id FROM environment.environment_operations \
               WHERE state IN ('accepted','running','cancelling') \
                 AND next_attempt_at <= now() \
                 AND (lease_expires_at IS NULL OR lease_expires_at <= now()) \
               ORDER BY created_at, operation_id FOR UPDATE SKIP LOCKED LIMIT 1 \
             ) \
             UPDATE environment.environment_operations operation \
             SET state='running', lease_owner=$1, lease_token=$3, \
                 heartbeat_at=date_trunc('milliseconds', clock_timestamp()), \
                 lease_expires_at=date_trunc('milliseconds', clock_timestamp()) \
                     + ($2 * interval '1 millisecond') \
             FROM candidate WHERE operation.operation_id=candidate.operation_id \
             RETURNING operation.environment_id",
        )
        .bind(worker_id)
        .bind(lease_milliseconds)
        .bind(lease_token)
        .fetch_optional(&mut *transaction)
        .await?;
        let Some(row) = row else {
            transaction.rollback().await?;
            return Ok(None);
        };
        let environment_uuid: Uuid = row.try_get("environment_id")?;
        let environment_id = EnvironmentId::from_str(&environment_uuid.to_string())
            .map_err(|_| EnvironmentStoreError::InvalidDatabaseIdentity)?;
        let instance = load_locked(&mut transaction, environment_id).await?;
        transaction.commit().await?;
        Ok(Some(LeasedEnvironment {
            instance,
            worker_id: worker_id.to_owned(),
            lease_token,
        }))
    }

    /// Renews a held lease without changing business state.
    pub async fn heartbeat(
        &self,
        lease: &LeasedEnvironment,
        lease_duration: Duration,
    ) -> Result<(), EnvironmentStoreError> {
        validate_worker(&lease.worker_id, lease_duration)?;
        let milliseconds = lease_milliseconds(lease_duration)?;
        let result = sqlx::query(
            "UPDATE environment.environment_operations \
             SET heartbeat_at=date_trunc('milliseconds', clock_timestamp()), \
             lease_expires_at=date_trunc('milliseconds', clock_timestamp()) \
                 + ($4 * interval '1 millisecond') \
             WHERE operation_id=$1 AND lease_owner=$2 AND lease_token=$3 \
               AND lease_expires_at > now() \
               AND state='running'",
        )
        .bind(lease.instance.operation.id.as_uuid())
        .bind(&lease.worker_id)
        .bind(lease.lease_token)
        .bind(milliseconds)
        .execute(&self.pool)
        .await?;
        if result.rows_affected() != 1 {
            return Err(EnvironmentStoreError::LeaseLost);
        }
        Ok(())
    }

    /// Persists a reconciler result and state-change Outbox event in one transaction.
    pub async fn save_reconciled(
        &self,
        lease: &LeasedEnvironment,
        updated: &EnvironmentInstance,
    ) -> Result<(), EnvironmentStoreError> {
        let mut transaction = self.pool.begin().await?;
        // Environment then operation is the only row-lock order used by this store.
        let stored = load_locked(&mut transaction, lease.instance.id).await?;
        if stored.revision != lease.instance.revision
            || stored.operation.id != lease.instance.operation.id
            || updated.operation.id != stored.operation.id
        {
            return Err(EnvironmentStoreError::RevisionConflict);
        }
        let row = sqlx::query(
            "SELECT environment_id, lease_owner, lease_token, \
                    lease_expires_at > now() AS lease_current \
             FROM environment.environment_operations WHERE operation_id=$1 FOR UPDATE",
        )
        .bind(lease.instance.operation.id.as_uuid())
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or(EnvironmentStoreError::LeaseLost)?;
        let owner: Option<String> = row.try_get("lease_owner")?;
        let token: Option<Uuid> = row.try_get("lease_token")?;
        let current: Option<bool> = row.try_get("lease_current")?;
        if owner.as_deref() != Some(&lease.worker_id)
            || token != Some(lease.lease_token)
            || current != Some(true)
        {
            return Err(EnvironmentStoreError::LeaseLost);
        }
        update_instance(&mut transaction, &stored, updated).await?;
        let terminal = matches!(
            updated.operation.state,
            OperationState::Succeeded | OperationState::Failed | OperationState::Cancelled
        );
        let state = wire_name(updated.operation.state)?;
        let retry_count = i32::try_from(updated.operation.attempt.saturating_sub(1))
            .map_err(|_| EnvironmentStoreError::NumericOverflow("retry count"))?;
        let result = sqlx::query(
            "UPDATE environment.environment_operations SET state=$4, retry_count=$5, \
             max_attempts=$6, diagnostic=$7, contract=$8, next_attempt_at=$9, \
             deadline_at=$10, target_generation=$11, provider_step=$12, \
             finished_at=CASE WHEN $13 THEN now() ELSE NULL END, \
             lease_owner=NULL, lease_token=NULL, lease_expires_at=NULL, heartbeat_at=NULL \
             WHERE operation_id=$1 AND lease_owner=$2 AND lease_token=$3",
        )
        .bind(updated.operation.id.as_uuid())
        .bind(&lease.worker_id)
        .bind(lease.lease_token)
        .bind(state)
        .bind(retry_count)
        .bind(i64::from(updated.operation.max_attempts))
        .bind(&updated.operation.diagnostic_code)
        .bind(serde_json::to_value(&updated.operation)?)
        .bind(updated.operation.next_attempt_at.get())
        .bind(updated.operation.deadline_at.get())
        .bind(as_i64(updated.generation, "target generation")?)
        .bind(i64::from(updated.operation.provider_step))
        .bind(terminal)
        .execute(&mut *transaction)
        .await?;
        if result.rows_affected() != 1 {
            return Err(EnvironmentStoreError::LeaseLost);
        }
        let occurred_at = database_now(&mut transaction).await?;
        enqueue_environment_event_at(
            &mut transaction,
            updated,
            subjects::ENVIRONMENT_STATE_CHANGED,
            occurred_at,
        )
        .await?;
        transaction.commit().await?;
        Ok(())
    }

    /// Finds expired authoritative records for the scheduler; it performs no implicit mutation.
    pub async fn find_expired(
        &self,
        now: UtcTimestamp,
        limit: u32,
    ) -> Result<Vec<EnvironmentInstance>, EnvironmentStoreError> {
        if !(1..=1_000).contains(&limit) {
            return Err(EnvironmentStoreError::InvalidLimit);
        }
        let rows = sqlx::query(
            "SELECT contract FROM environment.environment_instances \
             WHERE desired_state <> 'deleted' AND eligibility_expires_at <= $1 \
             ORDER BY eligibility_expires_at \
             LIMIT $2",
        )
        .bind(now.get())
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| decode_contract(row.try_get("contract")?))
            .collect()
    }

    /// Lists one actor's environments in one course at a stable database snapshot.
    pub async fn list_owned(
        &self,
        course_id: CourseId,
        owner_actor_id: contracts::ActorId,
        limit: u16,
    ) -> Result<(Vec<StoredEnvironmentInventory>, UtcTimestamp), EnvironmentStoreError> {
        if !(1..=100).contains(&limit) {
            return Err(EnvironmentStoreError::InvalidLimit);
        }
        let mut transaction = self.pool.begin().await?;
        let snapshot_at = database_now(&mut transaction).await?;
        let rows = sqlx::query(
            "SELECT i.contract, \
                    date_trunc('milliseconds',i.created_at) AS created_at, \
                    date_trunc('milliseconds',i.updated_at) AS updated_at, \
                    COALESCE(max(o.public_sequence),1) AS stream_sequence \
             FROM environment.environment_instances i LEFT JOIN environment.outbox_events o ON o.aggregate_id=i.environment_id \
             WHERE i.course_id=$1 AND i.owner_actor_id=$2 \
             GROUP BY i.environment_id,i.contract,i.created_at,i.updated_at \
             ORDER BY i.created_at DESC,i.environment_id DESC LIMIT $3",
        )
        .bind(course_id.as_uuid())
        .bind(owner_actor_id.as_uuid())
        .bind(i64::from(limit))
        .fetch_all(&mut *transaction)
        .await?;
        let records = rows
            .into_iter()
            .map(|row| {
                let sequence: i64 = row.try_get("stream_sequence")?;
                Ok(StoredEnvironmentInventory {
                    instance: decode_contract(row.try_get("contract")?)?,
                    created_at: UtcTimestamp::from_utc(row.try_get("created_at")?)?,
                    updated_at: UtcTimestamp::from_utc(row.try_get("updated_at")?)?,
                    stream_sequence: contracts::StreamSequence(
                        u64::try_from(sequence)
                            .map_err(|_| EnvironmentStoreError::InvalidDatabaseIdentity)?,
                    ),
                })
            })
            .collect::<Result<Vec<_>, EnvironmentStoreError>>()?;
        transaction.commit().await?;
        Ok((records, snapshot_at))
    }
}

async fn create_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    idempotency_key: &str,
    instance: &EnvironmentInstance,
) -> Result<EnvironmentOperationAccepted, EnvironmentStoreError> {
    IdempotencyKey::parse(idempotency_key)
        .map_err(|_| EnvironmentStoreError::InvalidIdempotencyKey)?;
    instance.validate()?;
    if instance.operation.kind != EnvironmentOperationKind::Create
        || instance.operation.state != OperationState::Accepted
        || instance.desired_state != DesiredEnvironmentState::Running
        || instance.observed_state != ObservedEnvironmentState::Requested
        || instance.revision.get() != 1
        || instance.generation != 1
        || instance.observed_generation != 0
        || instance.operation.accepted_revision != instance.revision
        || instance.operation.attempt != 1
        || instance.operation.provider_step != 1
        || instance.operation.next_attempt_at != instance.operation.accepted_at
        || instance.operation.cleanup_started_at.is_some()
        || instance.operation.diagnostic_code.is_some()
        || instance.operation.access_revocation_revision.is_some()
        || instance.operation.retry_from_phase.is_some()
        || instance.operation.reset_target.is_some()
        || instance.operation.preserve_mutable_disk
        || (instance.class == contracts::authoring::EnvironmentClass::Work
            && instance.operation.lease_authorization.is_none())
        || !instance.endpoints.is_empty()
        || instance.last_diagnostic_code.is_some()
        || instance.failed_phase.is_some()
        || instance.cleanup_evidence.is_some()
    {
        return Err(EnvironmentStoreError::InvalidCreateAggregate);
    }
    let request_hash = create_request_hash(instance)?;
    match IdempotencyStore::reserve(
        transaction,
        Domain::Environment,
        "create",
        idempotency_key,
        request_hash,
    )
    .await?
    {
        IdempotencyDecision::Replay(value) => {
            return serde_json::from_value(value).map_err(EnvironmentStoreError::Serialization);
        }
        IdempotencyDecision::Conflict => return Err(EnvironmentStoreError::IdempotencyConflict),
        IdempotencyDecision::InProgress => {
            return Err(EnvironmentStoreError::IdempotencyInProgress);
        }
        IdempotencyDecision::Reserved => {}
    }
    let result = sqlx::query(
        "INSERT INTO environment.environment_instances \
         (environment_id, course_id, owner_actor_id, release_id, generation, observed_generation, desired_state, \
          observed_state, provider_binding, lease_id, capacity_binding, revision, \
          terminal_diagnostic, failed_phase, eligibility_expires_at, contract) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16)",
    )
    .bind(instance.id.as_uuid())
    .bind(instance.course_id.as_uuid())
    .bind(instance.owner_id.as_uuid())
    .bind(instance.release_id.as_uuid())
    .bind(as_i64(instance.generation, "generation")?)
    .bind(as_i64(instance.observed_generation, "observed generation")?)
    .bind(wire_name(instance.desired_state)?)
    .bind(wire_name(instance.observed_state)?)
    .bind(&instance.provider_binding)
    .bind(instance.lease_id.map(contracts::LeaseId::as_uuid))
    .bind(&instance.capacity_binding)
    .bind(as_i64(instance.revision.get(), "revision")?)
    .bind(&instance.last_diagnostic_code)
    .bind(instance.failed_phase.map(wire_name).transpose()?)
    .bind(instance.eligibility_expires_at.get())
    .bind(serde_json::to_value(instance)?)
    .execute(&mut **transaction)
    .await;
    if let Err(error) = result {
        if is_unique_violation(&error) {
            return Err(EnvironmentStoreError::EnvironmentAlreadyExists);
        }
        return Err(error.into());
    }
    insert_operation(transaction, instance).await?;
    enqueue_environment_event(
        transaction,
        instance,
        subjects::ENVIRONMENT_OPERATION_ACCEPTED,
    )
    .await?;
    let accepted = accepted_response(instance);
    IdempotencyStore::complete(
        transaction,
        Domain::Environment,
        "create",
        idempotency_key,
        &serde_json::to_value(&accepted)?,
    )
    .await?;
    Ok(accepted)
}

fn build_create_instance(
    command: &LifecycleCommand,
    spec: &EnvironmentCreateSpec,
    lease_authorization: Option<EnvironmentLeaseAuthorization>,
    authority_now: UtcTimestamp,
    course_id: CourseId,
) -> Result<EnvironmentInstance, EnvironmentStoreError> {
    if command.kind != EnvironmentOperationKind::Create
        || command.expected_revision.get() != 1
        || course_id != spec.course_id
        || spec.release_version == 0
        || spec.provider_binding.trim().is_empty()
        || spec.eligibility_expires_at <= authority_now
        || !(1..=100).contains(&command.max_attempts)
        || command.deadline_at <= command.accepted_at
        || command.deadline_at <= authority_now
        || command.accepted_at > authority_now
        || command.access_revocation_revision.is_some()
        || command.preserve_mutable_disk
        || command.reset_target.is_some()
    {
        return Err(EnvironmentStoreError::InvalidCreateAggregate);
    }
    let eligibility_expires_at = match spec.class {
        contracts::authoring::EnvironmentClass::Experiment => {
            if spec.lease_id.is_some()
                || spec.capacity_binding.is_some()
                || lease_authorization.is_some()
            {
                return Err(EnvironmentStoreError::InvalidCreateAggregate);
            }
            spec.eligibility_expires_at
        }
        contracts::authoring::EnvironmentClass::Work => {
            let authorization = lease_authorization
                .as_ref()
                .ok_or(EnvironmentStoreError::LeaseAuthorizationRequired)?;
            if Some(authorization.lease_id) != spec.lease_id
                || authorization.environment_id != command.environment_id
                || authorization.course_id != spec.course_id
                || authorization.owner_actor_id != spec.owner_actor_id
                || Some(authorization.capacity_binding.as_str()) != spec.capacity_binding.as_deref()
                || authorization.active_from > authority_now
                || authorization.expires_at <= authority_now
            {
                return Err(EnvironmentStoreError::LeaseAuthorizationInvalid);
            }
            std::cmp::min(spec.eligibility_expires_at, authorization.expires_at)
        }
    };
    let instance = EnvironmentInstance {
        id: command.environment_id,
        display_label: spec.display_label.clone(),
        course_id: spec.course_id,
        owner_id: spec.owner_actor_id,
        class: spec.class,
        runtime_kind: spec.runtime_kind,
        release_id: spec.release_id,
        release_version: spec.release_version,
        lease_id: spec.lease_id,
        capacity_binding: spec.capacity_binding.clone(),
        provider_binding: spec.provider_binding.clone(),
        desired_state: DesiredEnvironmentState::Running,
        observed_state: ObservedEnvironmentState::Requested,
        revision: command.expected_revision,
        generation: 1,
        observed_generation: 0,
        operation: EnvironmentOperation {
            id: OperationId::new(),
            kind: EnvironmentOperationKind::Create,
            state: OperationState::Accepted,
            accepted_revision: command.expected_revision,
            attempt: 1,
            provider_step: 1,
            max_attempts: command.max_attempts,
            next_attempt_at: command.accepted_at,
            actor_id: command.actor_id,
            trace_id: command.trace_id.clone(),
            accepted_at: command.accepted_at,
            deadline_at: command.deadline_at,
            cleanup_started_at: None,
            diagnostic_code: None,
            preserve_mutable_disk: false,
            access_revocation_revision: None,
            retry_from_phase: None,
            reset_target: None,
            lease_authorization,
        },
        eligibility_expires_at,
        endpoints: Vec::new(),
        last_diagnostic_code: None,
        failed_phase: None,
        cleanup_evidence: None,
    };
    instance.validate()?;
    Ok(instance)
}

#[allow(
    clippy::too_many_lines,
    reason = "the transaction keeps idempotency, row locking, lifecycle planning, persistence, and Outbox ordering auditable"
)]
async fn accept_command_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    idempotency_key: &str,
    command: &LifecycleCommand,
    create: Option<&EnvironmentCreateSpec>,
    lease_authorization: Option<EnvironmentLeaseAuthorization>,
    course_id: Option<CourseId>,
) -> Result<EnvironmentOperationAccepted, EnvironmentStoreError> {
    if command.kind == EnvironmentOperationKind::Create {
        let authority_now = database_now(transaction).await?;
        let instance = build_create_instance(
            command,
            create.ok_or(EnvironmentStoreError::CreateSpecRequired)?,
            lease_authorization,
            authority_now,
            course_id.ok_or(EnvironmentStoreError::InboundMetadataInvalid)?,
        )?;
        return create_in_transaction(transaction, idempotency_key, &instance).await;
    }
    if create.is_some() {
        return Err(EnvironmentStoreError::CreateSpecUnexpected);
    }
    IdempotencyKey::parse(idempotency_key)
        .map_err(|_| EnvironmentStoreError::InvalidIdempotencyKey)?;
    let operation_name = operation_name(command.kind);
    let request_hash = command_request_hash(command)?;
    match IdempotencyStore::reserve(
        transaction,
        Domain::Environment,
        operation_name,
        idempotency_key,
        request_hash,
    )
    .await?
    {
        IdempotencyDecision::Replay(value) => {
            return serde_json::from_value(value).map_err(EnvironmentStoreError::Serialization);
        }
        IdempotencyDecision::Conflict => {
            return Err(EnvironmentStoreError::IdempotencyConflict);
        }
        IdempotencyDecision::InProgress => {
            return Err(EnvironmentStoreError::IdempotencyInProgress);
        }
        IdempotencyDecision::Reserved => {}
    }
    let current = load_locked(transaction, command.environment_id).await?;
    if course_id.is_some_and(|value| value != current.course_id) {
        return Err(EnvironmentStoreError::InboundMetadataInvalid);
    }
    let authority_now = database_now(transaction).await?;
    let destructive = matches!(
        command.kind,
        EnvironmentOperationKind::Cancel
            | EnvironmentOperationKind::Expire
            | EnvironmentOperationKind::Delete
            | EnvironmentOperationKind::Cleanup
    );
    let superseded_lease_expires_at = if destructive
        && matches!(
            current.operation.state,
            OperationState::Accepted | OperationState::Running | OperationState::Cancelling
        ) {
        let row = sqlx::query(
            "SELECT lease_expires_at FROM environment.environment_operations \
                 WHERE operation_id=$1 AND state IN ('accepted','running','cancelling') \
                 FOR UPDATE",
        )
        .bind(current.operation.id.as_uuid())
        .fetch_optional(&mut **transaction)
        .await?
        .ok_or(EnvironmentStoreError::OperationNotFound)?;
        let lease_expires_at: Option<time::OffsetDateTime> = row.try_get("lease_expires_at")?;
        sqlx::query(
                "UPDATE environment.environment_operations SET state='cancelled', \
                 diagnostic='LW_ENVIRONMENT_OPERATION_SUPERSEDED', finished_at=now(), \
                 contract=jsonb_set(jsonb_set(contract, '{state}', '\"cancelled\"'::jsonb), \
                   '{diagnosticCode}', to_jsonb('LW_ENVIRONMENT_OPERATION_SUPERSEDED'::text), true) \
                 WHERE operation_id=$1 AND state IN ('accepted','running','cancelling')",
            )
            .bind(current.operation.id.as_uuid())
            .execute(&mut **transaction)
            .await?;
        lease_expires_at
    } else {
        None
    };
    let mut planned = plan_command_authorized(
        &current,
        command,
        OperationId::new(),
        lease_authorization,
        authority_now,
    )?;
    if let Some(lease_expires_at) = superseded_lease_expires_at {
        let lease_expires_at = UtcTimestamp::from_utc(lease_expires_at)?;
        if lease_expires_at > planned.operation.next_attempt_at {
            planned.operation.next_attempt_at = lease_expires_at;
            planned.validate()?;
        }
    }
    update_instance(transaction, &current, &planned).await?;
    insert_operation(transaction, &planned).await?;
    enqueue_environment_event(
        transaction,
        &planned,
        subjects::ENVIRONMENT_OPERATION_ACCEPTED,
    )
    .await?;
    let accepted = accepted_response(&planned);
    let result = serde_json::to_value(&accepted)?;
    IdempotencyStore::complete(
        transaction,
        Domain::Environment,
        operation_name,
        idempotency_key,
        &result,
    )
    .await?;
    Ok(accepted)
}

async fn load_locked(
    transaction: &mut Transaction<'_, Postgres>,
    environment_id: EnvironmentId,
) -> Result<EnvironmentInstance, EnvironmentStoreError> {
    let row = sqlx::query(
        "SELECT contract FROM environment.environment_instances \
         WHERE environment_id=$1 FOR UPDATE",
    )
    .bind(environment_id.as_uuid())
    .fetch_optional(&mut **transaction)
    .await?
    .ok_or(EnvironmentStoreError::EnvironmentNotFound)?;
    decode_contract(row.try_get("contract")?)
}

fn decode_contract(value: Value) -> Result<EnvironmentInstance, EnvironmentStoreError> {
    let instance: EnvironmentInstance = serde_json::from_value(value)?;
    instance.validate()?;
    Ok(instance)
}

async fn insert_operation(
    transaction: &mut Transaction<'_, Postgres>,
    instance: &EnvironmentInstance,
) -> Result<(), EnvironmentStoreError> {
    let operation = &instance.operation;
    sqlx::query(
        "INSERT INTO environment.environment_operations \
         (operation_id, environment_id, operation_kind, expected_revision, target_generation, \
          state, retry_count, provider_step, max_attempts, next_attempt_at, deadline_at, \
          diagnostic, contract) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)",
    )
    .bind(operation.id.as_uuid())
    .bind(instance.id.as_uuid())
    .bind(wire_name(operation.kind)?)
    .bind(as_i64(
        operation.accepted_revision.get(),
        "accepted revision",
    )?)
    .bind(as_i64(instance.generation, "target generation")?)
    .bind(wire_name(operation.state)?)
    .bind(
        i32::try_from(operation.attempt.saturating_sub(1))
            .map_err(|_| EnvironmentStoreError::NumericOverflow("retry count"))?,
    )
    .bind(i64::from(operation.provider_step))
    .bind(i64::from(operation.max_attempts))
    .bind(operation.next_attempt_at.get())
    .bind(operation.deadline_at.get())
    .bind(&operation.diagnostic_code)
    .bind(serde_json::to_value(operation)?)
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

async fn update_instance(
    transaction: &mut Transaction<'_, Postgres>,
    current: &EnvironmentInstance,
    updated: &EnvironmentInstance,
) -> Result<(), EnvironmentStoreError> {
    updated.validate()?;
    let result = sqlx::query(
        "UPDATE environment.environment_instances SET generation=$3, observed_generation=$4, \
         desired_state=$5, observed_state=$6, provider_binding=$7, lease_id=$8, \
         capacity_binding=$9, revision=$10, terminal_diagnostic=$11, failed_phase=$12, \
         eligibility_expires_at=$13, contract=$14, updated_at=now() \
         WHERE environment_id=$1 AND revision=$2",
    )
    .bind(current.id.as_uuid())
    .bind(as_i64(current.revision.get(), "expected revision")?)
    .bind(as_i64(updated.generation, "generation")?)
    .bind(as_i64(updated.observed_generation, "observed generation")?)
    .bind(wire_name(updated.desired_state)?)
    .bind(wire_name(updated.observed_state)?)
    .bind(&updated.provider_binding)
    .bind(updated.lease_id.map(contracts::LeaseId::as_uuid))
    .bind(&updated.capacity_binding)
    .bind(as_i64(updated.revision.get(), "revision")?)
    .bind(&updated.last_diagnostic_code)
    .bind(updated.failed_phase.map(wire_name).transpose()?)
    .bind(updated.eligibility_expires_at.get())
    .bind(serde_json::to_value(updated)?)
    .execute(&mut **transaction)
    .await?;
    if result.rows_affected() != 1 {
        return Err(EnvironmentStoreError::RevisionConflict);
    }
    Ok(())
}

async fn enqueue_environment_event(
    transaction: &mut Transaction<'_, Postgres>,
    instance: &EnvironmentInstance,
    subject: &str,
) -> Result<(), EnvironmentStoreError> {
    enqueue_environment_event_at(
        transaction,
        instance,
        subject,
        instance.operation.accepted_at,
    )
    .await
}

async fn enqueue_environment_event_at(
    transaction: &mut Transaction<'_, Postgres>,
    instance: &EnvironmentInstance,
    subject: &str,
    occurred_at: UtcTimestamp,
) -> Result<(), EnvironmentStoreError> {
    let payload = EnvironmentEvent {
        environment_id: instance.id,
        generation: instance.generation,
        state: wire_name(instance.observed_state)?,
        operation_id: Some(instance.operation.id),
        diagnostic_code: instance.last_diagnostic_code.clone(),
    };
    let contract = event_contract(subject)?;
    let event_id = EventId::new();
    let envelope = CloudEvent {
        specversion: SPEC_VERSION.to_owned(),
        id: event_id,
        source: contract.source().to_owned(),
        event_type: contract.event_type.to_owned(),
        subject: contract.subject.to_owned(),
        time: occurred_at,
        datacontenttype: "application/json".to_owned(),
        dataschema: contract.data_schema(),
        course_id: instance.course_id,
        aggregate_revision: instance.revision,
        aggregate_sequence: Sequence(instance.revision.get()),
        trace_id: instance.operation.trace_id.clone(),
        data: payload,
    };
    envelope.validate(contract)?;
    let value = serde_json::to_value(&envelope)?;
    let hash = Sha256Digest::of_canonical(&envelope)
        .map_err(|error| EnvironmentStoreError::Canonical(error.to_string()))?;
    OutboxStore::enqueue(
        transaction,
        Domain::Environment,
        event_id.as_uuid(),
        subject,
        subject,
        instance.id.as_uuid(),
        instance.revision.get(),
        &value,
        hash,
    )
    .await?;
    Ok(())
}

fn event_contract(subject: &str) -> Result<EventContract, EnvironmentStoreError> {
    EVENT_CONTRACTS
        .iter()
        .copied()
        .find(|contract| contract.subject == subject)
        .ok_or(EnvironmentStoreError::EventContractMissing)
}

async fn database_now(
    transaction: &mut Transaction<'_, Postgres>,
) -> Result<UtcTimestamp, EnvironmentStoreError> {
    let value: time::OffsetDateTime =
        sqlx::query_scalar("SELECT date_trunc('milliseconds', clock_timestamp())")
            .fetch_one(&mut **transaction)
            .await?;
    UtcTimestamp::from_utc(value).map_err(Into::into)
}

fn accepted_response(instance: &EnvironmentInstance) -> EnvironmentOperationAccepted {
    EnvironmentOperationAccepted {
        operation_id: instance.operation.id,
        revision: instance.revision,
        status_url: format!(
            "/api/v1/environments/{}/operations/{}",
            instance.id, instance.operation.id
        ),
        environment_id: instance.id,
    }
}

fn create_request_hash(
    instance: &EnvironmentInstance,
) -> Result<Sha256Digest, EnvironmentStoreError> {
    canonical_hash(&json!({
        "courseId": instance.course_id,
        "ownerId": instance.owner_id,
        "class": instance.class,
        "runtimeKind": instance.runtime_kind,
        "releaseId": instance.release_id,
        "releaseVersion": instance.release_version,
        "leaseId": instance.lease_id,
        "capacityBinding": instance.capacity_binding,
        "actorId": instance.operation.actor_id,
        "providerBinding": instance.provider_binding,
        "eligibilityExpiresAt": instance.eligibility_expires_at,
        "traceId": instance.operation.trace_id,
        "acceptedAt": instance.operation.accepted_at,
        "deadlineAt": instance.operation.deadline_at,
        "maxAttempts": instance.operation.max_attempts,
        "leaseAuthorization": instance.operation.lease_authorization,
    }))
}

fn command_request_hash(command: &LifecycleCommand) -> Result<Sha256Digest, EnvironmentStoreError> {
    canonical_hash(&json!({
        "environmentId": command.environment_id,
        "kind": command.kind,
        "expectedRevision": command.expected_revision,
        "actorId": command.actor_id,
        "traceId": command.trace_id,
        "acceptedAt": command.accepted_at,
        "deadlineAt": command.deadline_at,
        "accessRevocationRevision": command.access_revocation_revision,
        "preserveMutableDisk": command.preserve_mutable_disk,
        "maxAttempts": command.max_attempts,
        "resetTarget": command.reset_target,
    }))
}

fn canonical_hash<T: Serialize>(value: &T) -> Result<Sha256Digest, EnvironmentStoreError> {
    Sha256Digest::of_canonical(value)
        .map_err(|error| EnvironmentStoreError::Canonical(error.to_string()))
}

fn wire_name<T: Serialize>(value: T) -> Result<String, EnvironmentStoreError> {
    serde_json::to_value(value)?
        .as_str()
        .map(ToOwned::to_owned)
        .ok_or(EnvironmentStoreError::InvalidWireEnum)
}

const fn operation_name(kind: EnvironmentOperationKind) -> &'static str {
    match kind {
        EnvironmentOperationKind::Create => "create",
        EnvironmentOperationKind::Start => "start",
        EnvironmentOperationKind::Stop => "stop",
        EnvironmentOperationKind::Restart => "restart",
        EnvironmentOperationKind::Reset => "reset",
        EnvironmentOperationKind::Retry => "retry",
        EnvironmentOperationKind::Cancel => "cancel",
        EnvironmentOperationKind::Recover => "recover",
        EnvironmentOperationKind::Expire => "expire",
        EnvironmentOperationKind::Delete => "delete",
        EnvironmentOperationKind::Cleanup => "cleanup",
        EnvironmentOperationKind::Freeze => "freeze",
    }
}

fn as_i64(value: u64, field: &'static str) -> Result<i64, EnvironmentStoreError> {
    i64::try_from(value).map_err(|_| EnvironmentStoreError::NumericOverflow(field))
}

fn validate_worker(worker_id: &str, duration: Duration) -> Result<(), EnvironmentStoreError> {
    if worker_id.is_empty()
        || worker_id.len() > 128
        || !worker_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.:".contains(&byte))
        || duration.is_zero()
        || duration > Duration::from_secs(300)
    {
        return Err(EnvironmentStoreError::InvalidLease);
    }
    Ok(())
}

fn lease_milliseconds(duration: Duration) -> Result<i64, EnvironmentStoreError> {
    if duration.is_zero()
        || duration > Duration::from_secs(300)
        || duration.subsec_nanos() % 1_000_000 != 0
    {
        return Err(EnvironmentStoreError::InvalidLease);
    }
    i64::try_from(duration.as_millis()).map_err(|_| EnvironmentStoreError::InvalidLease)
}

fn is_unique_violation(error: &sqlx::Error) -> bool {
    error
        .as_database_error()
        .and_then(sqlx::error::DatabaseError::code)
        .is_some_and(|code| code == "23505")
}

#[derive(Debug, thiserror::Error)]
pub enum EnvironmentStoreError {
    #[error("LW_ENVIRONMENT_NOT_FOUND")]
    EnvironmentNotFound,
    #[error("LW_ENVIRONMENT_OPERATION_NOT_FOUND")]
    OperationNotFound,
    #[error("LW_ENVIRONMENT_ALREADY_EXISTS")]
    EnvironmentAlreadyExists,
    #[error("LW_ENVIRONMENT_CREATE_AGGREGATE_INVALID")]
    InvalidCreateAggregate,
    #[error("LW_ENVIRONMENT_CREATE_SPEC_REQUIRED")]
    CreateSpecRequired,
    #[error("LW_ENVIRONMENT_CREATE_SPEC_UNEXPECTED")]
    CreateSpecUnexpected,
    #[error("LW_ENVIRONMENT_INBOUND_METADATA_INVALID")]
    InboundMetadataInvalid,
    #[error("LW_ENVIRONMENT_LEASE_AUTHORIZATION_REQUIRED")]
    LeaseAuthorizationRequired,
    #[error("LW_ENVIRONMENT_LEASE_AUTHORIZATION_INVALID")]
    LeaseAuthorizationInvalid,
    #[error("LW_IDEMPOTENCY_KEY_INVALID")]
    InvalidIdempotencyKey,
    #[error("LW_IDEMPOTENCY_CONFLICT")]
    IdempotencyConflict,
    #[error("LW_IDEMPOTENCY_IN_PROGRESS")]
    IdempotencyInProgress,
    #[error("LW_ENVIRONMENT_REVISION_CONFLICT")]
    RevisionConflict,
    #[error("LW_ENVIRONMENT_RECONCILE_LEASE_INVALID")]
    InvalidLease,
    #[error("LW_ENVIRONMENT_RECONCILE_LEASE_LOST")]
    LeaseLost,
    #[error("LW_ENVIRONMENT_EXPIRY_LIMIT_INVALID")]
    InvalidLimit,
    #[error("LW_ENVIRONMENT_DATABASE_IDENTITY_INVALID")]
    InvalidDatabaseIdentity,
    #[error("LW_ENVIRONMENT_NUMERIC_OVERFLOW: {0}")]
    NumericOverflow(&'static str),
    #[error("LW_ENVIRONMENT_WIRE_ENUM_INVALID")]
    InvalidWireEnum,
    #[error("LW_ENVIRONMENT_EVENT_CONTRACT_MISSING")]
    EventContractMissing,
    #[error("LW_ENVIRONMENT_CANONICAL_IDENTITY_FAILED: {0}")]
    Canonical(String),
    #[error("LW_ENVIRONMENT_CONTRACT_INVALID: {0}")]
    Contract(#[from] contracts::environment::EnvironmentError),
    #[error("LW_ENVIRONMENT_EVENT_INVALID: {0}")]
    Event(#[from] contracts::events::EventError),
    #[error("LW_ENVIRONMENT_TIMESTAMP_INVALID: {0}")]
    Timestamp(#[from] contracts::foundation::FoundationError),
    #[error("LW_ENVIRONMENT_LIFECYCLE_FAILED: {0}")]
    Lifecycle(#[from] LifecycleError),
    #[error("LW_ENVIRONMENT_DATABASE_FAILED")]
    Database(#[from] sqlx::Error),
    #[error("LW_ENVIRONMENT_PERSISTENCE_FAILED: {0}")]
    Persistence(#[from] PersistenceError),
    #[error("LW_ENVIRONMENT_SERIALIZATION_FAILED")]
    Serialization(#[from] serde_json::Error),
}

impl EnvironmentStoreError {
    #[must_use]
    pub const fn retryable(&self) -> bool {
        matches!(
            self,
            Self::IdempotencyInProgress
                | Self::Database(_)
                | Self::Persistence(PersistenceError::Database(_))
        )
    }
}
