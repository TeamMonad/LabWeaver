use persistence_sqlx::Sha256Digest; // internal persistence hash, not contract hash
use std::str::FromStr;
use std::time::Duration;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use contracts::authoring::{EnvironmentClass, RuntimeKind};
use contracts::environment::{
    DesiredEnvironmentState, EnvironmentCreateSpec, EnvironmentInstance,
    EnvironmentLeaseAuthorization, EnvironmentOperation, EnvironmentOperationKind,
    EnvironmentOperationSnapshot, ObservedEnvironmentState, OperationState,
    ResourceWorkCleanupStatus,
};
use contracts::events::{
    CloudEvent, EVENT_CONTRACTS, EnvironmentEvent, EventContract, SPEC_VERSION, subjects,
};
use contracts::http::{EnvironmentOperationAccepted, IdempotencyKey, MAX_CURSOR_LENGTH};
use contracts::{
    ActorId, CourseId, DiagnosticCode, EnvironmentId, EventId, OperationId, ProjectId, ReleaseId,
    Revision, Sequence, StreamSequence, UtcTimestamp,
};
use persistence_sqlx::{
    Domain, IdempotencyDecision, IdempotencyStore, InboxDecision, InboxStore, OutboxStore,
    PersistenceError,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sqlx::{PgPool, Postgres, Row, Transaction, postgres::PgRow};
use time::OffsetDateTime;
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
    pub stream_sequence: StreamSequence,
    pub current_operation: Option<EnvironmentOperationSnapshot>,
}

/// One public operation snapshot and its private keyset position.
#[derive(Clone, Debug)]
pub struct StoredEnvironmentOperation {
    pub snapshot: EnvironmentOperationSnapshot,
    cursor_created_at: OffsetDateTime,
}

/// One keyset-paged actor-scoped operation history result.
#[derive(Clone, Debug)]
pub struct EnvironmentOperationPage {
    pub records: Vec<StoredEnvironmentOperation>,
    pub next_cursor: Option<String>,
    pub snapshot_at: UtcTimestamp,
    pub snapshot_sequence: StreamSequence,
}

/// Visibility and state filters for one actor-scoped Environment inventory page.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentInventoryFilter {
    pub project_id: ProjectId,
    pub course_id: Option<CourseId>,
    pub owner_actor_id: ActorId,
    pub runtime_kind: Option<RuntimeKind>,
    pub class: Option<EnvironmentClass>,
    pub desired_state: Option<DesiredEnvironmentState>,
    pub observed_state: Option<ObservedEnvironmentState>,
    pub release_id: Option<ReleaseId>,
}

/// One keyset-paged actor-scoped Environment inventory result.
#[derive(Clone, Debug)]
pub struct EnvironmentInventoryPage {
    pub records: Vec<StoredEnvironmentInventory>,
    pub next_cursor: Option<String>,
    pub snapshot_at: UtcTimestamp,
    pub snapshot_sequence: StreamSequence,
}

/// Immutable delivery metadata for one lifecycle command received from a durable consumer.
#[derive(Clone, Debug)]
pub struct InboundLifecycleCommand {
    pub consumer: String,
    pub event_id: EventId,
    pub project_id: ProjectId,
    pub course_id: Option<CourseId>,
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

    pub(crate) fn pool(&self) -> PgPool {
        self.pool.clone()
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
        lease_authorization: Option<EnvironmentLeaseAuthorization>,
        project_id: ProjectId,
        course_id: Option<CourseId>,
    ) -> Result<EnvironmentOperationAccepted, EnvironmentStoreError> {
        let mut transaction = self.pool.begin().await?;
        let accepted = accept_command_in_transaction(
            &mut transaction,
            idempotency_key,
            command,
            create,
            lease_authorization,
            Some(project_id),
            course_id,
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
                    Some(inbound.project_id),
                    inbound.course_id,
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

    /// Loads one operation from the actor-visible history and projects it through the
    /// Environment lifecycle rules. The operation table is the history authority; the aggregate
    /// contract is used only to evaluate whether this row is still the current operation.
    pub async fn get_operation(
        &self,
        environment_id: EnvironmentId,
        actor_id: ActorId,
        operation_id: OperationId,
    ) -> Result<StoredEnvironmentOperation, EnvironmentStoreError> {
        let mut transaction = self.pool.begin().await?;
        begin_snapshot(&mut transaction).await?;
        let snapshot_at = database_now(&mut transaction).await?;
        let row = sqlx::query(
            "SELECT o.operation_id AS op_id, o.environment_id AS op_environment_id, \
                    o.operation_kind AS op_kind, o.expected_revision AS op_expected_revision, \
                    o.target_generation AS op_target_generation, o.state AS op_state, \
                    o.retry_count AS op_retry_count, o.max_attempts AS op_max_attempts, \
                    date_trunc('milliseconds', o.next_attempt_at) AS op_next_attempt_at, \
                    date_trunc('milliseconds', o.deadline_at) AS op_deadline_at, \
                    o.provider_step AS op_provider_step, o.diagnostic AS op_diagnostic, \
                    o.contract AS op_contract, o.created_at AS op_cursor_created_at, \
                    date_trunc('milliseconds', o.finished_at) AS op_finished_at, \
                    i.contract AS instance_contract \
             FROM environment.environment_operations o \
             JOIN environment.environment_instances i ON i.environment_id=o.environment_id \
             WHERE o.environment_id=$1 AND i.owner_actor_id=$2 AND o.operation_id=$3",
        )
        .bind(environment_id.as_uuid())
        .bind(actor_id.as_uuid())
        .bind(operation_id.as_uuid())
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or(EnvironmentStoreError::OperationNotFound)?;
        let instance: EnvironmentInstance = decode_contract(row.try_get("instance_contract")?)?;
        let record = decode_operation_row(&row)?;
        let current = record.operation.id == instance.operation.id;
        if current {
            ensure_operation_record_matches_instance(&record, &instance)?;
        }
        let stored = StoredEnvironmentOperation {
            snapshot: public_operation_snapshot(&instance, &record, current, snapshot_at)?,
            cursor_created_at: record.cursor_created_at,
        };
        transaction.commit().await?;
        Ok(stored)
    }

    /// Lists one actor's operation history with an opaque cursor bound to the exact environment,
    /// actor, kind, and state scope. The page and its stream position share one repeatable-read
    /// database snapshot.
    #[allow(
        clippy::too_many_lines,
        reason = "the query, public projection, cursor, and stream position share one snapshot transaction"
    )]
    pub async fn list_operations(
        &self,
        environment_id: EnvironmentId,
        actor_id: ActorId,
        kind: Option<EnvironmentOperationKind>,
        state: Option<OperationState>,
        cursor: Option<&str>,
        limit: u16,
    ) -> Result<EnvironmentOperationPage, EnvironmentStoreError> {
        if !(1..=100).contains(&limit) {
            return Err(EnvironmentStoreError::InvalidLimit);
        }
        let scope = OperationCursorScope {
            environment_id,
            actor_id,
            kind,
            state,
        };
        let cursor = cursor
            .map(|value| decode_operation_cursor(value, scope))
            .transpose()?;
        let mut transaction = self.pool.begin().await?;
        begin_snapshot(&mut transaction).await?;
        let snapshot_at = database_now(&mut transaction).await?;
        let rows = sqlx::query(
            "SELECT o.operation_id AS op_id, o.environment_id AS op_environment_id, \
                    o.operation_kind AS op_kind, o.expected_revision AS op_expected_revision, \
                    o.target_generation AS op_target_generation, o.state AS op_state, \
                    o.retry_count AS op_retry_count, o.max_attempts AS op_max_attempts, \
                    date_trunc('milliseconds', o.next_attempt_at) AS op_next_attempt_at, \
                    date_trunc('milliseconds', o.deadline_at) AS op_deadline_at, \
                    o.provider_step AS op_provider_step, o.diagnostic AS op_diagnostic, \
                    o.contract AS op_contract, o.created_at AS op_cursor_created_at, \
                    date_trunc('milliseconds', o.finished_at) AS op_finished_at, \
                    i.contract AS instance_contract \
             FROM environment.environment_operations o \
             JOIN environment.environment_instances i ON i.environment_id=o.environment_id \
             WHERE o.environment_id=$1 AND i.owner_actor_id=$2 \
               AND ($3::text IS NULL OR o.operation_kind=$3) \
               AND ($4::text IS NULL OR o.state=$4) \
               AND ($5::timestamptz IS NULL OR (o.created_at,o.operation_id) < ($5,$6)) \
             ORDER BY o.created_at DESC,o.operation_id DESC LIMIT $7",
        )
        .bind(environment_id.as_uuid())
        .bind(actor_id.as_uuid())
        .bind(kind.map(wire_name).transpose()?)
        .bind(state.map(wire_name).transpose()?)
        .bind(cursor.as_ref().map(|value| value.created_at))
        .bind(cursor.as_ref().map(|value| value.operation_id.as_uuid()))
        .bind(i64::from(limit) + 1)
        .fetch_all(&mut *transaction)
        .await?;
        let mut records = rows
            .into_iter()
            .map(|row| {
                let instance: EnvironmentInstance =
                    decode_contract(row.try_get("instance_contract")?)?;
                let record = decode_operation_row(&row)?;
                let current = record.operation.id == instance.operation.id;
                if current {
                    ensure_operation_record_matches_instance(&record, &instance)?;
                }
                Ok((
                    StoredEnvironmentOperation {
                        snapshot: public_operation_snapshot(
                            &instance,
                            &record,
                            current,
                            snapshot_at,
                        )?,
                        cursor_created_at: record.cursor_created_at,
                    },
                    record.operation.id,
                ))
            })
            .collect::<Result<Vec<_>, EnvironmentStoreError>>()?;
        let has_more = records.len() > usize::from(limit);
        let next_cursor = if has_more {
            let (record, operation_id) = &records[usize::from(limit) - 1];
            Some(encode_operation_cursor(
                scope,
                record.cursor_created_at,
                *operation_id,
            )?)
        } else {
            None
        };
        records.truncate(usize::from(limit));
        let records = records
            .into_iter()
            .map(|(record, _)| record)
            .collect::<Vec<_>>();
        let snapshot_sequence = sqlx::query_scalar::<_, Option<i64>>(
            "SELECT MAX(public_sequence) FROM environment.outbox_events WHERE aggregate_id=$1",
        )
        .bind(environment_id.as_uuid())
        .fetch_one(&mut *transaction)
        .await?
        .map(|value| {
            u64::try_from(value)
                .map(StreamSequence)
                .map_err(|_| EnvironmentStoreError::InvalidDatabaseIdentity)
        })
        .transpose()?
        .unwrap_or(StreamSequence(0));
        transaction.commit().await?;
        Ok(EnvironmentOperationPage {
            records,
            next_cursor,
            snapshot_at,
            snapshot_sequence,
        })
    }

    /// Returns the exact Resource cleanup fence from the authoritative Work aggregate.
    ///
    /// The lease revision is retained in the accepted operation authorization so a
    /// cleanup readback cannot accidentally describe a requester-supplied or newer
    /// claim after the aggregate has entered a destructive lifecycle operation.
    pub async fn load_cleanup_status(
        &self,
        environment_id: EnvironmentId,
    ) -> Result<ResourceWorkCleanupStatus, EnvironmentStoreError> {
        let instance = self.load(environment_id).await?;
        cleanup_status_from_instance(&instance)
    }

    /// Refreshes only the Resource-owned Lease fence of an existing Work
    /// aggregate. Immutable workload bindings and lifecycle intent are
    /// preserved; stale revisions and non-extending updates fail closed.
    pub async fn refresh_work_lease(
        &self,
        environment_id: EnvironmentId,
        authorization: contracts::environment::EnvironmentLeaseAuthorization,
    ) -> Result<EnvironmentInstance, EnvironmentStoreError> {
        authorization.validate()?;
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
        let current_authorization = current
            .operation
            .lease_authorization
            .as_ref()
            .ok_or(EnvironmentStoreError::LeaseAuthorizationInvalid)?;
        if current.class != contracts::authoring::EnvironmentClass::Work
            || current.desired_state == DesiredEnvironmentState::Deleted
            || matches!(
                current.operation.kind,
                EnvironmentOperationKind::Cancel
                    | EnvironmentOperationKind::Expire
                    | EnvironmentOperationKind::Delete
                    | EnvironmentOperationKind::Cleanup
            )
            || current.lease_id != Some(authorization.lease_id)
            || current.project_id != authorization.project_id
            || current.course_id != authorization.course_id
            || current.owner_id != authorization.owner_actor_id
            || current.capacity_binding.as_deref() != Some(authorization.capacity_binding.as_str())
            || current_authorization.lease_id != authorization.lease_id
            || current_authorization.environment_id != authorization.environment_id
            || current_authorization.project_id != authorization.project_id
            || current_authorization.course_id != authorization.course_id
            || current_authorization.owner_actor_id != authorization.owner_actor_id
            || current_authorization.capacity_binding != authorization.capacity_binding
            || current_authorization.resource_request_id != authorization.resource_request_id
            || current_authorization.active_from != authorization.active_from
            || current_authorization.approved_resources != authorization.approved_resources
            || current_authorization.gpu_allocation != authorization.gpu_allocation
        {
            return Err(EnvironmentStoreError::LeaseAuthorizationInvalid);
        }
        current_authorization
            .validate()
            .map_err(|_| EnvironmentStoreError::LeaseAuthorizationInvalid)?;
        if current_authorization == &authorization {
            transaction.commit().await?;
            return Ok(current);
        }
        if authorization.lease_revision <= current_authorization.lease_revision
            || authorization.expires_at <= current.eligibility_expires_at
            || authorization.expires_at <= current_authorization.expires_at
        {
            return Err(EnvironmentStoreError::LeaseAuthorizationInvalid);
        }
        let mut updated = current.clone();
        updated.revision = contracts::Revision::new(current.revision.get().checked_add(1).ok_or(
            EnvironmentStoreError::NumericOverflow("lease refresh revision"),
        )?)?;
        for endpoint in &mut updated.endpoints {
            endpoint.revision = updated.revision;
        }
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
             SET state='running', \
                 contract=jsonb_set(operation.contract, '{state}', '\"running\"'::jsonb, true), \
                 lease_owner=$1, lease_token=$3, \
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
        crate::metering::record_transition(&mut transaction, &stored, updated, occurred_at).await?;
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

    /// Lists one actor's environments using the complete published inventory filter contract.
    ///
    /// The query is keyset-paged in the same order that is exposed by the API. Every page repeats
    /// the project, owner, course, and state predicates, and the cursor carries those predicates
    /// so a cursor cannot be replayed in another visibility scope.
    #[allow(
        clippy::too_many_lines,
        reason = "the inventory query, current-operation projection, and stream position share one snapshot transaction"
    )]
    pub async fn list_owned(
        &self,
        filter: EnvironmentInventoryFilter,
        cursor: Option<&str>,
        limit: u16,
    ) -> Result<EnvironmentInventoryPage, EnvironmentStoreError> {
        if !(1..=100).contains(&limit) {
            return Err(EnvironmentStoreError::InvalidLimit);
        }
        let cursor = cursor
            .map(|value| decode_inventory_cursor(value, filter))
            .transpose()?;
        let mut transaction = self.pool.begin().await?;
        begin_snapshot(&mut transaction).await?;
        let snapshot_at = database_now(&mut transaction).await?;
        let rows = sqlx::query(
            "SELECT i.environment_id, i.contract, \
                    i.created_at AS cursor_created_at, \
                    date_trunc('milliseconds',i.created_at) AS created_at, \
                    date_trunc('milliseconds',i.updated_at) AS updated_at \
             FROM environment.environment_instances i \
             WHERE i.project_id=$1 AND i.owner_actor_id=$2 \
               AND ($3::uuid IS NULL OR i.course_id=$3) \
               AND ($4::text IS NULL OR i.contract->>'class'=$4) \
               AND ($5::text IS NULL OR i.contract->>'runtimeKind'=$5) \
               AND ($6::text IS NULL OR i.desired_state=$6) \
               AND ($7::text IS NULL OR i.observed_state=$7) \
               AND ($8::uuid IS NULL OR i.release_id=$8) \
               AND ($9::timestamptz IS NULL OR (i.created_at,i.environment_id) < ($9,$10)) \
             ORDER BY i.created_at DESC,i.environment_id DESC LIMIT $11",
        )
        .bind(filter.project_id.as_uuid())
        .bind(filter.owner_actor_id.as_uuid())
        .bind(filter.course_id.map(CourseId::as_uuid))
        .bind(filter.class.map(wire_name).transpose()?)
        .bind(filter.runtime_kind.map(wire_name).transpose()?)
        .bind(filter.desired_state.map(wire_name).transpose()?)
        .bind(filter.observed_state.map(wire_name).transpose()?)
        .bind(filter.release_id.map(ReleaseId::as_uuid))
        .bind(cursor.as_ref().map(|value| value.created_at))
        .bind(cursor.as_ref().map(|value| value.environment_id.as_uuid()))
        .bind(i64::from(limit) + 1)
        .fetch_all(&mut *transaction)
        .await?;
        let mut rows = rows
            .into_iter()
            .map(|row| {
                let environment_id: Uuid = row.try_get("environment_id")?;
                let cursor_created_at: OffsetDateTime = row.try_get("cursor_created_at")?;
                Ok((
                    StoredEnvironmentInventory {
                        instance: decode_contract(row.try_get("contract")?)?,
                        created_at: UtcTimestamp::from_utc(row.try_get("created_at")?)?,
                        updated_at: UtcTimestamp::from_utc(row.try_get("updated_at")?)?,
                        stream_sequence: StreamSequence(0),
                        current_operation: None,
                    },
                    cursor_created_at,
                    environment_id,
                ))
            })
            .collect::<Result<Vec<_>, EnvironmentStoreError>>()?;
        let has_more = rows.len() > usize::from(limit);
        let next_cursor = if has_more {
            let (_, created_at, environment_id) = &rows[usize::from(limit) - 1];
            Some(encode_inventory_cursor(
                filter,
                *created_at,
                *environment_id,
            )?)
        } else {
            None
        };
        let environment_ids = rows
            .iter()
            .map(|(_, _, environment_id)| *environment_id)
            .collect::<Vec<_>>();
        if !environment_ids.is_empty() {
            let operation_ids = rows
                .iter()
                .map(|(record, _, _)| record.instance.operation.id.as_uuid())
                .collect::<Vec<_>>();
            let operation_rows = sqlx::query(
                "SELECT o.operation_id AS op_id, o.environment_id AS op_environment_id, \
                        o.operation_kind AS op_kind, o.expected_revision AS op_expected_revision, \
                        o.target_generation AS op_target_generation, o.state AS op_state, \
                        o.retry_count AS op_retry_count, o.max_attempts AS op_max_attempts, \
                        date_trunc('milliseconds', o.next_attempt_at) AS op_next_attempt_at, \
                        date_trunc('milliseconds', o.deadline_at) AS op_deadline_at, \
                        o.provider_step AS op_provider_step, o.diagnostic AS op_diagnostic, \
                        o.contract AS op_contract, o.created_at AS op_cursor_created_at, \
                        date_trunc('milliseconds', o.finished_at) AS op_finished_at \
                 FROM environment.environment_operations o \
                 WHERE o.environment_id = ANY($1::uuid[]) \
                   AND o.operation_id = ANY($2::uuid[])",
            )
            .bind(&environment_ids)
            .bind(&operation_ids)
            .fetch_all(&mut *transaction)
            .await?;
            for (record, _, environment_id) in &mut rows {
                let operation_row = operation_rows.iter().find(|row| {
                    row.try_get::<Uuid, _>("op_environment_id").ok() == Some(*environment_id)
                        && row.try_get::<Uuid, _>("op_id").ok()
                            == Some(record.instance.operation.id.as_uuid())
                });
                let operation_row =
                    operation_row.ok_or(EnvironmentStoreError::InvalidOperationRecord)?;
                let operation = decode_operation_row(operation_row)?;
                ensure_operation_record_matches_instance(&operation, &record.instance)?;
                record.current_operation = Some(public_operation_snapshot(
                    &record.instance,
                    &operation,
                    true,
                    snapshot_at,
                )?);
            }
            let stream_rows = sqlx::query(
                "SELECT aggregate_id, MAX(public_sequence) AS stream_sequence \
                 FROM environment.outbox_events \
                 WHERE aggregate_id = ANY($1::uuid[]) \
                 GROUP BY aggregate_id",
            )
            .bind(&environment_ids)
            .fetch_all(&mut *transaction)
            .await?;
            for (record, _, environment_id) in &mut rows {
                if let Some(stream_row) = stream_rows.iter().find(|row| {
                    row.try_get::<Uuid, _>("aggregate_id").ok() == Some(*environment_id)
                }) {
                    let sequence: i64 = stream_row.try_get("stream_sequence")?;
                    record.stream_sequence = StreamSequence(
                        u64::try_from(sequence)
                            .map_err(|_| EnvironmentStoreError::InvalidDatabaseIdentity)?,
                    );
                }
            }
        }
        let snapshot_sequence = rows
            .iter()
            .map(|(record, _, _)| record.stream_sequence)
            .max_by_key(|sequence| sequence.0)
            .unwrap_or(StreamSequence(0));
        rows.truncate(usize::from(limit));
        let records = rows
            .into_iter()
            .map(|(record, _, _)| record)
            .collect::<Vec<_>>();
        transaction.commit().await?;
        Ok(EnvironmentInventoryPage {
            records,
            next_cursor,
            snapshot_at,
            snapshot_sequence,
        })
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
         (environment_id, project_id, course_id, owner_actor_id, release_id, generation, observed_generation, desired_state, \
          observed_state, provider_binding, lease_id, capacity_binding, revision, \
          terminal_diagnostic, failed_phase, eligibility_expires_at, contract) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17)",
    )
    .bind(instance.id.as_uuid())
    .bind(instance.project_id.as_uuid())
    .bind(instance.course_id.map(CourseId::as_uuid))
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
    crate::metering::initialize(transaction, instance).await?;
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
    project_id: ProjectId,
    course_id: Option<CourseId>,
) -> Result<EnvironmentInstance, EnvironmentStoreError> {
    if command.kind != EnvironmentOperationKind::Create
        || command.expected_revision.get() != 1
        || project_id != spec.project_id
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
                || authorization.project_id != spec.project_id
                || authorization.course_id != spec.course_id
                || authorization.owner_actor_id != spec.owner_actor_id
                || Some(authorization.capacity_binding.as_str()) != spec.capacity_binding.as_deref()
                || authorization.active_from > authority_now
                || authorization.expires_at <= authority_now
            {
                return Err(EnvironmentStoreError::LeaseAuthorizationInvalid);
            }
            authorization.validate()?;
            std::cmp::min(spec.eligibility_expires_at, authorization.expires_at)
        }
    };
    let instance = EnvironmentInstance {
        id: command.environment_id,
        display_label: spec.display_label.clone(),
        project_id: spec.project_id,
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
    project_id: Option<ProjectId>,
    course_id: Option<CourseId>,
) -> Result<EnvironmentOperationAccepted, EnvironmentStoreError> {
    if command.kind == EnvironmentOperationKind::Create {
        let authority_now = database_now(transaction).await?;
        let instance = build_create_instance(
            command,
            create.ok_or(EnvironmentStoreError::CreateSpecRequired)?,
            lease_authorization,
            authority_now,
            project_id.ok_or(EnvironmentStoreError::InboundMetadataInvalid)?,
            course_id,
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
    if let Some(project_id) = project_id
        && (project_id != current.project_id || course_id != current.course_id)
    {
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

fn cleanup_status_from_instance(
    instance: &EnvironmentInstance,
) -> Result<ResourceWorkCleanupStatus, EnvironmentStoreError> {
    if instance.class != contracts::authoring::EnvironmentClass::Work {
        return Err(EnvironmentStoreError::LeaseAuthorizationInvalid);
    }
    let authorization = instance
        .operation
        .lease_authorization
        .as_ref()
        .ok_or(EnvironmentStoreError::LeaseAuthorizationInvalid)?;
    let lease_id = instance
        .lease_id
        .ok_or(EnvironmentStoreError::LeaseAuthorizationInvalid)?;
    let capacity_binding = instance
        .capacity_binding
        .as_ref()
        .ok_or(EnvironmentStoreError::LeaseAuthorizationInvalid)?;
    if authorization.lease_id != lease_id
        || authorization.environment_id != instance.id
        || authorization.project_id != instance.project_id
        || authorization.course_id != instance.course_id
        || authorization.owner_actor_id != instance.owner_id
        || authorization.capacity_binding != *capacity_binding
    {
        return Err(EnvironmentStoreError::LeaseAuthorizationInvalid);
    }
    let status = ResourceWorkCleanupStatus {
        version: 1,
        environment_id: instance.id,
        project_id: instance.project_id,
        course_id: instance.course_id,
        owner_actor_id: instance.owner_id,
        lease_id,
        lease_revision: authorization.lease_revision,
        capacity_binding: capacity_binding.clone(),
        revision: instance.revision,
        observed_state: instance.observed_state,
        cleanup_complete: instance.observed_state == ObservedEnvironmentState::Deleted,
        diagnostic_code: instance.last_diagnostic_code.clone(),
    };
    status.validate()?;
    Ok(status)
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
        project_id: instance.project_id,
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

async fn begin_snapshot(
    transaction: &mut Transaction<'_, Postgres>,
) -> Result<(), EnvironmentStoreError> {
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
        .execute(&mut **transaction)
        .await?;
    Ok(())
}

struct DecodedOperationRow {
    operation: EnvironmentOperation,
    environment_id: EnvironmentId,
    cursor_created_at: OffsetDateTime,
    terminal_at: Option<UtcTimestamp>,
}

/// Checks the intentionally split operation projection without comparing fields that have a
/// documented writer boundary. The operation table owns history and scheduler state. The current
/// aggregate owns the live Resource lease authorization, which may be refreshed after the
/// operation was accepted; claiming a row may also advance the table state before the aggregate
/// has been persisted by the worker.
fn ensure_operation_record_matches_instance(
    record: &DecodedOperationRow,
    instance: &EnvironmentInstance,
) -> Result<(), EnvironmentStoreError> {
    if let Some(reason) = operation_record_mismatch_reason(record, instance) {
        tracing::error!(
            event = "environment.operation_projection_invalid",
            environment_id = %instance.id,
            operation_id = %record.operation.id,
            reason,
            record_state = ?record.operation.state,
            instance_state = ?instance.operation.state,
        );
        return Err(EnvironmentStoreError::InvalidOperationRecord);
    }
    Ok(())
}

fn operation_record_mismatch_reason(
    record: &DecodedOperationRow,
    instance: &EnvironmentInstance,
) -> Option<&'static str> {
    let stored = &record.operation;
    let current = &instance.operation;
    if record.environment_id != instance.id {
        return Some("environment_id");
    }
    if stored.id != current.id {
        return Some("operation_id");
    }
    if stored.kind != current.kind {
        return Some("operation_kind");
    }
    if stored.accepted_revision != current.accepted_revision {
        return Some("accepted_revision");
    }
    if stored.attempt != current.attempt {
        return Some("attempt");
    }
    if stored.provider_step != current.provider_step {
        return Some("provider_step");
    }
    if stored.max_attempts != current.max_attempts {
        return Some("max_attempts");
    }
    if stored.next_attempt_at != current.next_attempt_at {
        return Some("next_attempt_at");
    }
    if stored.actor_id != current.actor_id {
        return Some("actor_id");
    }
    if stored.trace_id != current.trace_id {
        return Some("trace_id");
    }
    if stored.accepted_at != current.accepted_at {
        return Some("accepted_at");
    }
    if stored.deadline_at != current.deadline_at {
        return Some("deadline_at");
    }
    if stored.cleanup_started_at != current.cleanup_started_at {
        return Some("cleanup_started_at");
    }
    if stored.diagnostic_code != current.diagnostic_code {
        return Some("diagnostic_code");
    }
    if stored.preserve_mutable_disk != current.preserve_mutable_disk {
        return Some("preserve_mutable_disk");
    }
    if stored.access_revocation_revision != current.access_revocation_revision {
        return Some("access_revocation_revision");
    }
    if stored.retry_from_phase != current.retry_from_phase {
        return Some("retry_from_phase");
    }
    if stored.reset_target != current.reset_target {
        return Some("reset_target");
    }
    if !operation_states_are_consistent(stored.state, current.state) {
        return Some("state");
    }
    if !lease_authorizations_are_consistent(
        stored.lease_authorization.as_ref(),
        current.lease_authorization.as_ref(),
    ) {
        return Some("lease_authorization");
    }
    None
}

fn operation_states_are_consistent(
    record_state: OperationState,
    instance_state: OperationState,
) -> bool {
    record_state == instance_state
        || record_state == OperationState::Running
            && matches!(
                instance_state,
                OperationState::Accepted | OperationState::Cancelling
            )
}

fn lease_authorizations_are_consistent(
    record: Option<&EnvironmentLeaseAuthorization>,
    current: Option<&EnvironmentLeaseAuthorization>,
) -> bool {
    match (record, current) {
        (None, None) => true,
        (Some(record), Some(current)) if record == current => true,
        (Some(record), Some(current)) => {
            record.resource_request_id == current.resource_request_id
                && record.lease_id == current.lease_id
                && record.environment_id == current.environment_id
                && record.project_id == current.project_id
                && record.course_id == current.course_id
                && record.owner_actor_id == current.owner_actor_id
                && record.capacity_binding == current.capacity_binding
                && record.approved_resources == current.approved_resources
                && record.gpu_allocation == current.gpu_allocation
                && record.active_from == current.active_from
                && current.lease_revision > record.lease_revision
                && current.expires_at > record.expires_at
        }
        _ => false,
    }
}

fn decode_operation_row(row: &PgRow) -> Result<DecodedOperationRow, EnvironmentStoreError> {
    let operation_id = parse_db_id::<OperationId>(row.try_get("op_id")?)?;
    let environment_id = parse_db_id::<EnvironmentId>(row.try_get("op_environment_id")?)?;
    let operation_kind = decode_wire::<EnvironmentOperationKind>(row.try_get("op_kind")?)?;
    let state = decode_wire::<OperationState>(row.try_get("op_state")?)?;
    let expected_revision = revision_from_database(row.try_get("op_expected_revision")?)?;
    let retry_count: i32 = row.try_get("op_retry_count")?;
    let max_attempts: u32 = u32::try_from(row.try_get::<i64, _>("op_max_attempts")?)
        .map_err(|_| EnvironmentStoreError::InvalidOperationRecord)?;
    let provider_step: u32 = u32::try_from(row.try_get::<i64, _>("op_provider_step")?)
        .map_err(|_| EnvironmentStoreError::InvalidOperationRecord)?;
    let target_generation: i64 = row.try_get("op_target_generation")?;
    let next_attempt_at = UtcTimestamp::from_utc(row.try_get("op_next_attempt_at")?)?;
    let deadline_at = UtcTimestamp::from_utc(row.try_get("op_deadline_at")?)?;
    let diagnostic: Option<String> = row.try_get("op_diagnostic")?;
    let operation: EnvironmentOperation = serde_json::from_value(row.try_get("op_contract")?)?;
    let terminal_at = row
        .try_get::<Option<OffsetDateTime>, _>("op_finished_at")?
        .map(UtcTimestamp::from_utc)
        .transpose()?;
    let invalid_reason = if retry_count < 0 {
        Some("retry_count")
    } else if target_generation <= 0 {
        Some("target_generation")
    } else if operation.id != operation_id {
        Some("operation_id")
    } else if operation.kind != operation_kind {
        Some("operation_kind")
    } else if operation.state != state {
        Some("state")
    } else if operation.accepted_revision != expected_revision {
        Some("accepted_revision")
    } else if u32::try_from(retry_count)
        .ok()
        .and_then(|count| count.checked_add(1))
        != Some(operation.attempt)
    {
        Some("attempt")
    } else if operation.max_attempts != max_attempts {
        Some("max_attempts")
    } else if operation.provider_step != provider_step {
        Some("provider_step")
    } else if operation.next_attempt_at != next_attempt_at {
        Some("next_attempt_at")
    } else if operation.deadline_at != deadline_at {
        Some("deadline_at")
    } else if operation.diagnostic_code != diagnostic {
        Some("diagnostic_code")
    } else if operation.state.is_terminal() != terminal_at.is_some() {
        Some("finished_at")
    } else {
        None
    };
    if let Some(reason) = invalid_reason {
        tracing::error!(
            event = "environment.operation_record_invalid",
            environment_id = %environment_id,
            operation_id = %operation_id,
            reason,
            record_state = ?state,
            contract_state = ?operation.state,
        );
        return Err(EnvironmentStoreError::InvalidOperationRecord);
    }
    Ok(DecodedOperationRow {
        operation,
        environment_id,
        cursor_created_at: row.try_get("op_cursor_created_at")?,
        terminal_at,
    })
}

fn public_operation_snapshot(
    instance: &EnvironmentInstance,
    record: &DecodedOperationRow,
    current: bool,
    snapshot_at: UtcTimestamp,
) -> Result<EnvironmentOperationSnapshot, EnvironmentStoreError> {
    let retry_eligible = current
        && record.operation.state == OperationState::Failed
        && record.operation.attempt < record.operation.max_attempts
        && instance.observed_state == ObservedEnvironmentState::Failed
        && instance.failed_phase.is_some()
        && instance.eligibility_expires_at > snapshot_at
        && EnvironmentInstance::ensure_operation_allowed(
            instance.observed_state,
            EnvironmentOperationKind::Retry,
        )
        .is_ok();
    let cancel_eligible = current
        && matches!(
            record.operation.state,
            OperationState::Accepted | OperationState::Running
        )
        && EnvironmentInstance::ensure_operation_allowed(
            instance.observed_state,
            EnvironmentOperationKind::Cancel,
        )
        .is_ok();
    let diagnostic_code = record
        .operation
        .diagnostic_code
        .as_deref()
        .map(DiagnosticCode::parse)
        .transpose()
        .map_err(|_| EnvironmentStoreError::InvalidOperationRecord)?;
    let snapshot = EnvironmentOperationSnapshot {
        environment_id: record.environment_id,
        operation_id: record.operation.id,
        kind: record.operation.kind,
        state: record.operation.state,
        accepted_revision: record.operation.accepted_revision,
        accepted_at: record.operation.accepted_at,
        deadline_at: record.operation.deadline_at,
        cleanup_started_at: record.operation.cleanup_started_at,
        terminal_at: record.terminal_at,
        attempt: record.operation.attempt,
        max_attempts: record.operation.max_attempts,
        retry_eligible,
        cancel_eligible,
        diagnostic_code,
        trace_id: record.operation.trace_id.clone(),
    };
    snapshot
        .validate()
        .map_err(|_| EnvironmentStoreError::InvalidOperationRecord)?;
    Ok(snapshot)
}

fn parse_db_id<T>(value: Uuid) -> Result<T, EnvironmentStoreError>
where
    T: FromStr,
{
    T::from_str(&value.to_string()).map_err(|_| EnvironmentStoreError::InvalidDatabaseIdentity)
}

fn revision_from_database(value: i64) -> Result<Revision, EnvironmentStoreError> {
    Revision::new(u64::try_from(value).map_err(|_| EnvironmentStoreError::InvalidOperationRecord)?)
        .map_err(|_| EnvironmentStoreError::InvalidOperationRecord)
}

fn decode_wire<T: DeserializeOwned>(value: String) -> Result<T, EnvironmentStoreError> {
    serde_json::from_value(Value::String(value)).map_err(|_| EnvironmentStoreError::InvalidWireEnum)
}

const OPERATION_CURSOR_VERSION: u8 = 1;

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct OperationCursorScope {
    environment_id: EnvironmentId,
    actor_id: ActorId,
    kind: Option<EnvironmentOperationKind>,
    state: Option<OperationState>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OperationCursor {
    version: u8,
    created_at: OffsetDateTime,
    operation_id: OperationId,
    environment_id: EnvironmentId,
    actor_id: ActorId,
    kind: Option<EnvironmentOperationKind>,
    state: Option<OperationState>,
    scope_digest: Sha256Digest,
}

fn encode_operation_cursor(
    scope: OperationCursorScope,
    created_at: OffsetDateTime,
    operation_id: OperationId,
) -> Result<String, EnvironmentStoreError> {
    let cursor = OperationCursor {
        version: OPERATION_CURSOR_VERSION,
        created_at,
        operation_id,
        environment_id: scope.environment_id,
        actor_id: scope.actor_id,
        kind: scope.kind,
        state: scope.state,
        scope_digest: operation_scope_digest(scope)?,
    };
    let bytes = serde_json::to_vec(&cursor)?;
    let encoded = URL_SAFE_NO_PAD.encode(bytes);
    if encoded.len() > MAX_CURSOR_LENGTH {
        return Err(EnvironmentStoreError::InvalidOperationCursor);
    }
    Ok(encoded)
}

fn decode_operation_cursor(
    value: &str,
    scope: OperationCursorScope,
) -> Result<OperationCursor, EnvironmentStoreError> {
    if value.is_empty() || value.len() > MAX_CURSOR_LENGTH {
        return Err(EnvironmentStoreError::InvalidOperationCursor);
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| EnvironmentStoreError::InvalidOperationCursor)?;
    if URL_SAFE_NO_PAD.encode(&bytes) != value {
        return Err(EnvironmentStoreError::InvalidOperationCursor);
    }
    let cursor: OperationCursor = serde_json::from_slice(&bytes)
        .map_err(|_| EnvironmentStoreError::InvalidOperationCursor)?;
    if serde_json::to_vec(&cursor).map_err(|_| EnvironmentStoreError::InvalidOperationCursor)?
        != bytes
        || cursor.version != OPERATION_CURSOR_VERSION
        || cursor.environment_id != scope.environment_id
        || cursor.actor_id != scope.actor_id
        || cursor.kind != scope.kind
        || cursor.state != scope.state
        || cursor.scope_digest != operation_scope_digest(scope)?
    {
        return Err(EnvironmentStoreError::InvalidOperationCursor);
    }
    Ok(cursor)
}

fn operation_scope_digest(
    scope: OperationCursorScope,
) -> Result<Sha256Digest, EnvironmentStoreError> {
    Sha256Digest::of_canonical(&scope)
        .map_err(|error| EnvironmentStoreError::Canonical(error.to_string()))
}

const INVENTORY_CURSOR_VERSION: u8 = 1;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct InventoryCursor {
    version: u8,
    created_at: OffsetDateTime,
    environment_id: EnvironmentId,
    filter_digest: Sha256Digest,
}

fn encode_inventory_cursor(
    filter: EnvironmentInventoryFilter,
    created_at: OffsetDateTime,
    environment_id: Uuid,
) -> Result<String, EnvironmentStoreError> {
    let cursor = InventoryCursor {
        version: INVENTORY_CURSOR_VERSION,
        created_at,
        environment_id: serde_json::from_value(Value::String(environment_id.to_string()))
            .map_err(|_| EnvironmentStoreError::InvalidDatabaseIdentity)?,
        filter_digest: inventory_filter_digest(filter)?,
    };
    let bytes = serde_json::to_vec(&cursor)?;
    let encoded = URL_SAFE_NO_PAD.encode(bytes);
    if encoded.len() > MAX_CURSOR_LENGTH {
        return Err(EnvironmentStoreError::InvalidInventoryCursor);
    }
    Ok(encoded)
}

fn decode_inventory_cursor(
    value: &str,
    filter: EnvironmentInventoryFilter,
) -> Result<InventoryCursor, EnvironmentStoreError> {
    if value.is_empty() || value.len() > MAX_CURSOR_LENGTH {
        return Err(EnvironmentStoreError::InvalidInventoryCursor);
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| EnvironmentStoreError::InvalidInventoryCursor)?;
    if URL_SAFE_NO_PAD.encode(&bytes) != value {
        return Err(EnvironmentStoreError::InvalidInventoryCursor);
    }
    let cursor: InventoryCursor = serde_json::from_slice(&bytes)
        .map_err(|_| EnvironmentStoreError::InvalidInventoryCursor)?;
    if serde_json::to_vec(&cursor).map_err(|_| EnvironmentStoreError::InvalidInventoryCursor)?
        != bytes
        || cursor.version != INVENTORY_CURSOR_VERSION
        || cursor.filter_digest != inventory_filter_digest(filter)?
    {
        return Err(EnvironmentStoreError::InvalidInventoryCursor);
    }
    Ok(cursor)
}

fn inventory_filter_digest(
    filter: EnvironmentInventoryFilter,
) -> Result<Sha256Digest, EnvironmentStoreError> {
    Sha256Digest::of_canonical(&filter)
        .map_err(|error| EnvironmentStoreError::Canonical(error.to_string()))
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
        "projectId": instance.project_id,
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
        || duration > Duration::from_mins(5)
    {
        return Err(EnvironmentStoreError::InvalidLease);
    }
    Ok(())
}

fn lease_milliseconds(duration: Duration) -> Result<i64, EnvironmentStoreError> {
    if duration.is_zero()
        || duration > Duration::from_mins(5)
        || !duration.subsec_nanos().is_multiple_of(1_000_000)
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
    #[error("LW_ENVIRONMENT_INVENTORY_CURSOR_INVALID")]
    InvalidInventoryCursor,
    #[error("LW_ENVIRONMENT_OPERATION_CURSOR_INVALID")]
    InvalidOperationCursor,
    #[error("LW_ENVIRONMENT_OPERATION_RECORD_INVALID")]
    InvalidOperationRecord,
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
    #[error("LW_ENVIRONMENT_METERING_INVALID")]
    MeteringInvalid,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn filter() -> EnvironmentInventoryFilter {
        EnvironmentInventoryFilter {
            project_id: ProjectId::new(),
            course_id: Some(CourseId::new()),
            owner_actor_id: ActorId::new(),
            runtime_kind: Some(RuntimeKind::VirtualMachine),
            class: Some(EnvironmentClass::Work),
            desired_state: Some(DesiredEnvironmentState::Stopped),
            observed_state: Some(ObservedEnvironmentState::Ready),
            release_id: Some(ReleaseId::new()),
        }
    }

    #[test]
    fn inventory_cursor_round_trips_exact_scope_filters_and_position()
    -> Result<(), Box<dyn std::error::Error>> {
        let filter = filter();
        let created_at = OffsetDateTime::from_unix_timestamp_nanos(1_751_500_800_123_456_000)?;
        let environment_id = EnvironmentId::new().as_uuid();
        let encoded = encode_inventory_cursor(filter, created_at, environment_id)?;

        assert!(encoded.len() <= MAX_CURSOR_LENGTH);
        assert_eq!(
            decode_inventory_cursor(&encoded, filter)?.created_at,
            created_at
        );
        assert_eq!(
            decode_inventory_cursor(&encoded, filter)?
                .environment_id
                .as_uuid(),
            environment_id
        );
        assert!(matches!(
            decode_inventory_cursor(
                &encoded,
                EnvironmentInventoryFilter {
                    project_id: ProjectId::new(),
                    ..filter
                }
            ),
            Err(EnvironmentStoreError::InvalidInventoryCursor)
        ));
        Ok(())
    }

    #[test]
    fn inventory_cursor_rejects_noncanonical_or_truncated_values()
    -> Result<(), Box<dyn std::error::Error>> {
        let filter = filter();
        assert!(matches!(
            decode_inventory_cursor("not-a-cursor", filter),
            Err(EnvironmentStoreError::InvalidInventoryCursor)
        ));

        let encoded = encode_inventory_cursor(
            filter,
            OffsetDateTime::from_unix_timestamp(1_751_500_800)?,
            EnvironmentId::new().as_uuid(),
        )?;
        assert!(matches!(
            decode_inventory_cursor(&encoded[..encoded.len() - 1], filter),
            Err(EnvironmentStoreError::InvalidInventoryCursor)
        ));
        Ok(())
    }
}
