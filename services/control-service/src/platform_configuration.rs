//! Transactional managed configuration, revision history and deployment observations.

use contracts::http::IdempotencyKey;
use contracts::platform_configuration::{
    ApplyPlatformConfigurationRequest, PlatformConfigurationAppliedState as AppliedState,
    PlatformConfigurationApply, PlatformConfigurationApplyStage as Stage,
    PlatformConfigurationFailureReason, PlatformConfigurationNextStep as Next,
    PlatformConfigurationStatus, PlatformConfigurationVersion,
    ReportPlatformConfigurationApplyRequest, UpdatePlatformConfigurationRequest,
    valid_configuration_sha256,
};
use contracts::{ActorId, OperationId, Revision, UtcTimestamp};
use persistence_sqlx::{Domain, IdempotencyDecision, IdempotencyStore, Sha256Digest};
use serde_json::{Value, json};
use sqlx::{PgPool, Postgres, Row, Transaction};

type Result<T> = std::result::Result<T, ConfigurationError>;

#[derive(Debug, thiserror::Error)]
/// Stable configuration diagnostics. Input contents and executor logs are never included.
pub enum ConfigurationError {
    /// A request or digest has an invalid shape.
    #[error("LW_PLATFORM_CONFIGURATION_INVALID")]
    Invalid,
    /// Typed policy validation rejected the settings.
    #[error(transparent)]
    Contract(#[from] contracts::platform_configuration::PlatformConfigurationContractError),
    /// The aggregate or desired version changed.
    #[error("LW_PLATFORM_CONFIGURATION_REVISION_CONFLICT")]
    RevisionConflict,
    /// An earlier application still owns the execution window.
    #[error("LW_PLATFORM_CONFIGURATION_APPLY_IN_PROGRESS")]
    ApplyInProgress,
    /// An attempt fence, owner or transition is invalid.
    #[error("LW_PLATFORM_CONFIGURATION_APPLY_CONFLICT")]
    ApplyConflict,
    /// The executor observed a different immutable configuration or Bundle.
    #[error("LW_PLATFORM_CONFIGURATION_OBSERVATION_MISMATCH")]
    ObservationMismatch,
    /// No requested immutable record exists.
    #[error("LW_PLATFORM_CONFIGURATION_NOT_FOUND")]
    NotFound,
    /// Persistence failed without exposing SQL or payloads.
    #[error("LW_PLATFORM_CONFIGURATION_PERSISTENCE_FAILED")]
    Persistence,
    /// A key was reused for a different actor or request.
    #[error("LW_IDEMPOTENCY_CONFLICT")]
    IdempotencyConflict,
    /// An identical mutation has not committed yet.
    #[error("LW_OPERATION_IN_PROGRESS")]
    OperationInProgress,
}

/// Reads facts without creating or applying configuration.
pub async fn get(pool: &PgPool) -> Result<PlatformConfigurationStatus> {
    let row = sqlx::query(
        "SELECT revision,contract FROM control.platform_configuration_state WHERE singleton=true",
    )
    .fetch_one(pool)
    .await
    .map_err(|_| ConfigurationError::Persistence)?;
    decode_state(&row)
}

/// Reads one immutable desired version, including superseded versions.
pub async fn get_version(
    pool: &PgPool,
    revision: Revision,
) -> Result<PlatformConfigurationVersion> {
    let value: Value = sqlx::query_scalar(
        "SELECT contract FROM control.platform_configuration_versions WHERE revision=$1",
    )
    .bind(number(revision.get())?)
    .fetch_optional(pool)
    .await
    .map_err(|_| ConfigurationError::Persistence)?
    .ok_or(ConfigurationError::NotFound)?;
    serde_json::from_value(value).map_err(|_| ConfigurationError::Persistence)
}

/// Reads one attempt even after retries replace the latest attempt.
pub async fn get_apply(pool: &PgPool, id: OperationId) -> Result<PlatformConfigurationApply> {
    let value: Value = sqlx::query_scalar(
        "SELECT contract FROM control.platform_configuration_applies WHERE apply_id=$1",
    )
    .bind(id.as_uuid())
    .fetch_optional(pool)
    .await
    .map_err(|_| ConfigurationError::Persistence)?
    .ok_or(ConfigurationError::NotFound)?;
    serde_json::from_value(value).map_err(|_| ConfigurationError::Persistence)
}

/// Both JSON forms and advanced YAML reach this one conditional mutation.
pub async fn update(
    pool: &PgPool,
    actor: ActorId,
    request: &UpdatePlatformConfigurationRequest,
    key: &IdempotencyKey,
    now: UtcTimestamp,
) -> Result<PlatformConfigurationStatus> {
    request
        .configuration
        .validate()
        .map_err(ConfigurationError::Contract)?;
    let mut tx = pool
        .begin()
        .await
        .map_err(|_| ConfigurationError::Persistence)?;
    let operation = "control_platform_configuration_update_v1";
    if let Some(replay) = reserve(
        &mut tx,
        operation,
        key,
        &json!({"actor":actor,"request":request}),
    )
    .await?
    {
        return Ok(replay);
    }
    let mut state = lock_state(&mut tx).await?;
    if state.revision != request.expected_revision {
        return Err(ConfigurationError::RevisionConflict);
    }
    advance_state(&mut state)?;
    let desired = PlatformConfigurationVersion {
        revision: revision(state.revision)?,
        configuration: request.configuration.clone(),
        configuration_sha256: Sha256Digest::of_canonical(&request.configuration)
            .map_err(|_| ConfigurationError::Invalid)?
            .to_string(),
        created_by: actor,
        created_at: now,
    };
    sqlx::query(
        "INSERT INTO control.platform_configuration_versions(revision,contract) VALUES($1,$2)",
    )
    .bind(number(desired.revision.get())?)
    .bind(encode(&desired)?)
    .execute(&mut *tx)
    .await
    .map_err(|_| ConfigurationError::Persistence)?;
    state.desired = Some(desired);
    state.refresh_impact();
    finish(tx, state, operation, key).await
}

/// Records intent. It never runs a command or changes the applied snapshot.
pub async fn request_apply(
    pool: &PgPool,
    actor: ActorId,
    request: &ApplyPlatformConfigurationRequest,
    key: &IdempotencyKey,
    now: UtcTimestamp,
) -> Result<PlatformConfigurationStatus> {
    if !valid_configuration_sha256(&request.encrypted_bundle_sha256) {
        return Err(ConfigurationError::Invalid);
    }
    let mut tx = pool
        .begin()
        .await
        .map_err(|_| ConfigurationError::Persistence)?;
    let operation = "control_platform_configuration_apply_v1";
    if let Some(replay) = reserve(
        &mut tx,
        operation,
        key,
        &json!({"actor":actor,"request":request}),
    )
    .await?
    {
        return Ok(replay);
    }
    let mut state = lock_state(&mut tx).await?;
    if state.revision != request.expected_revision
        || state.desired.as_ref().map(|v| v.revision) != Some(request.desired_revision)
    {
        return Err(ConfigurationError::RevisionConflict);
    }
    if state
        .latest_apply
        .as_ref()
        .is_some_and(|v| !v.stage.terminal())
    {
        return Err(ConfigurationError::ApplyInProgress);
    }
    let apply = PlatformConfigurationApply {
        id: OperationId::new(),
        previous_apply_id: state.latest_apply.as_ref().map(|v| v.id),
        revision: revision(1)?,
        desired_revision: request.desired_revision,
        encrypted_bundle_sha256: request.encrypted_bundle_sha256.clone(),
        stage: Stage::Requested,
        failed_stage: None,
        failure_reason: None,
        next_step: if state.applied_state == AppliedState::Unverified {
            Next::ReconcileExternalResult
        } else {
            Next::ExecuteApprovedDeployment
        },
        requested_by: actor,
        requested_at: now,
        executor_client_id: None,
        updated_at: now,
    };
    sqlx::query("INSERT INTO control.platform_configuration_applies(apply_id,desired_revision,revision,contract) VALUES($1,$2,1,$3)")
        .bind(apply.id.as_uuid()).bind(number(apply.desired_revision.get())?).bind(encode(&apply)?).execute(&mut *tx).await.map_err(|_| ConfigurationError::Persistence)?;
    advance_state(&mut state)?;
    state.latest_apply = Some(apply);
    finish(tx, state, operation, key).await
}

/// Consumes a verified deployment-service observation. The caller must enforce its JWT permission.
pub async fn report(
    pool: &PgPool,
    executor: &str,
    id: OperationId,
    request: &ReportPlatformConfigurationApplyRequest,
    key: &IdempotencyKey,
    now: UtcTimestamp,
) -> Result<PlatformConfigurationStatus> {
    if executor.is_empty() || executor.len() > 128 || executor.chars().any(char::is_control) {
        return Err(ConfigurationError::Invalid);
    }
    let mut tx = pool
        .begin()
        .await
        .map_err(|_| ConfigurationError::Persistence)?;
    let operation = "control_platform_configuration_report_v1";
    if let Some(replay) = reserve(
        &mut tx,
        operation,
        key,
        &json!({"executor":executor,"id":id,"request":request}),
    )
    .await?
    {
        return Ok(replay);
    }
    let mut state = lock_state(&mut tx).await?;
    let apply = state
        .latest_apply
        .as_mut()
        .ok_or(ConfigurationError::NotFound)?;
    if apply.id != id
        || apply.revision != request.expected_revision
        || apply.stage.terminal()
        || apply
            .executor_client_id
            .as_deref()
            .is_some_and(|v| v != executor)
        || now < apply.updated_at
    {
        return Err(ConfigurationError::ApplyConflict);
    }
    let previous_stage = apply.stage;
    let allowed = matches!(
        (previous_stage, request.stage),
        (Stage::Requested, Stage::Validating | Stage::Failed)
            | (Stage::Validating, Stage::Applying | Stage::Failed)
            | (Stage::Applying, Stage::Verifying | Stage::Failed)
            | (Stage::Verifying, Stage::Succeeded | Stage::Failed)
    );
    if !allowed {
        return Err(ConfigurationError::ApplyConflict);
    }
    if (request.stage == Stage::Failed) != request.failure_reason.is_some() {
        return Err(ConfigurationError::Invalid);
    }
    if request.stage == Stage::Succeeded {
        let value: Value = sqlx::query_scalar(
            "SELECT contract FROM control.platform_configuration_versions WHERE revision=$1",
        )
        .bind(number(apply.desired_revision.get())?)
        .fetch_one(&mut *tx)
        .await
        .map_err(|_| ConfigurationError::Persistence)?;
        let desired: PlatformConfigurationVersion =
            serde_json::from_value(value).map_err(|_| ConfigurationError::Persistence)?;
        if request.observed_configuration_sha256.as_deref()
            != Some(desired.configuration_sha256.as_str())
            || request.observed_encrypted_bundle_sha256.as_deref()
                != Some(apply.encrypted_bundle_sha256.as_str())
        {
            return Err(ConfigurationError::ObservationMismatch);
        }
        state.applied = Some(desired);
        state.applied_state = AppliedState::Verified;
        state.applied_at = Some(now);
        state.applied_bundle_sha256 = Some(apply.encrypted_bundle_sha256.clone());
    } else if request.observed_configuration_sha256.is_some()
        || request.observed_encrypted_bundle_sha256.is_some()
    {
        return Err(ConfigurationError::Invalid);
    }
    if request.stage == Stage::Applying
        || (request.stage == Stage::Failed
            && (matches!(previous_stage, Stage::Applying | Stage::Verifying)
                || request.failure_reason
                    == Some(PlatformConfigurationFailureReason::ExternalResultUnknown)))
    {
        state.applied_state = AppliedState::Unverified;
    }
    apply.stage = request.stage;
    apply.revision = revision(
        apply
            .revision
            .get()
            .checked_add(1)
            .ok_or(ConfigurationError::Invalid)?,
    )?;
    apply.executor_client_id = Some(executor.to_owned());
    apply.updated_at = now;
    apply.failed_stage = (request.stage == Stage::Failed).then_some(previous_stage);
    apply.failure_reason = request.failure_reason;
    apply.next_step = match request.stage {
        Stage::Succeeded => Next::None,
        Stage::Failed
            if matches!(previous_stage, Stage::Requested | Stage::Validating)
                && state.applied_state != AppliedState::Unverified
                && request.failure_reason
                    != Some(PlatformConfigurationFailureReason::ExternalResultUnknown) =>
        {
            Next::FixConfigurationAndRetry
        }
        Stage::Failed => Next::ReconcileExternalResult,
        _ => Next::WaitForExecutor,
    };
    sqlx::query("UPDATE control.platform_configuration_applies SET revision=$2,contract=$3 WHERE apply_id=$1")
        .bind(id.as_uuid()).bind(number(apply.revision.get())?).bind(encode(apply)?).execute(&mut *tx).await.map_err(|_| ConfigurationError::Persistence)?;
    advance_state(&mut state)?;
    state.refresh_impact();
    finish(tx, state, operation, key).await
}

fn number(value: u64) -> Result<i64> {
    i64::try_from(value).map_err(|_| ConfigurationError::Invalid)
}
fn revision(value: u64) -> Result<Revision> {
    Revision::new(value).map_err(|_| ConfigurationError::Invalid)
}
fn encode<T: serde::Serialize>(value: &T) -> Result<Value> {
    serde_json::to_value(value).map_err(|_| ConfigurationError::Invalid)
}
fn advance_state(state: &mut PlatformConfigurationStatus) -> Result<()> {
    state.revision = state
        .revision
        .checked_add(1)
        .filter(|v| *v <= i64::MAX.unsigned_abs())
        .ok_or(ConfigurationError::Invalid)?;
    Ok(())
}
fn decode_state(row: &sqlx::postgres::PgRow) -> Result<PlatformConfigurationStatus> {
    let value: Value = row
        .try_get("contract")
        .map_err(|_| ConfigurationError::Persistence)?;
    let state: PlatformConfigurationStatus =
        serde_json::from_value(value).map_err(|_| ConfigurationError::Persistence)?;
    if number(state.revision)?
        != row
            .try_get::<i64, _>("revision")
            .map_err(|_| ConfigurationError::Persistence)?
    {
        return Err(ConfigurationError::Persistence);
    }
    Ok(state)
}
async fn lock_state(tx: &mut Transaction<'_, Postgres>) -> Result<PlatformConfigurationStatus> {
    let row = sqlx::query("SELECT revision,contract FROM control.platform_configuration_state WHERE singleton=true FOR UPDATE")
        .fetch_one(&mut **tx).await.map_err(|_| ConfigurationError::Persistence)?;
    decode_state(&row)
}
async fn reserve(
    tx: &mut Transaction<'_, Postgres>,
    operation: &str,
    key: &IdempotencyKey,
    request: &Value,
) -> Result<Option<PlatformConfigurationStatus>> {
    let hash = Sha256Digest::of_canonical(request).map_err(|_| ConfigurationError::Invalid)?;
    match IdempotencyStore::reserve(tx, Domain::Control, operation, key.as_str(), hash)
        .await
        .map_err(|_| ConfigurationError::Persistence)?
    {
        IdempotencyDecision::Reserved => Ok(None),
        IdempotencyDecision::Replay(value) => Ok(Some(
            serde_json::from_value(value).map_err(|_| ConfigurationError::Persistence)?,
        )),
        IdempotencyDecision::Conflict => Err(ConfigurationError::IdempotencyConflict),
        IdempotencyDecision::InProgress => Err(ConfigurationError::OperationInProgress),
    }
}
async fn finish(
    mut tx: Transaction<'_, Postgres>,
    state: PlatformConfigurationStatus,
    operation: &str,
    key: &IdempotencyKey,
) -> Result<PlatformConfigurationStatus> {
    let contract = encode(&state)?;
    sqlx::query("UPDATE control.platform_configuration_state SET revision=$1,contract=$2 WHERE singleton=true")
        .bind(number(state.revision)?).bind(&contract).execute(&mut *tx).await.map_err(|_| ConfigurationError::Persistence)?;
    IdempotencyStore::complete(&mut tx, Domain::Control, operation, key.as_str(), &contract)
        .await
        .map_err(|_| ConfigurationError::Persistence)?;
    tx.commit()
        .await
        .map_err(|_| ConfigurationError::Persistence)?;
    Ok(state)
}
