//! Model token admission and settlement uses the same Credits hold owner as Resource.
use crate::{
    credits, pricing,
    store::{PgResourceStore, ResourceStoreError},
};
use contracts::resource::{
    FixedDecimal, Money, ResourceBillingUnit, ResourceChargeLine,
    credits::{CreditHold, CreditHoldResolutionRequest},
    model_costing::{
        ModelCostAdmission, ModelCostAdmissionRequest, ModelCostEstimate, ModelCostEstimateRequest,
        ModelCostObservation, ModelRateSnapshot, ModelTokenQuantities, ModelUsageMeasurement,
    },
};
use sqlx::{Postgres, Row, Transaction};

const UNITS: [(ResourceBillingUnit, &str); 4] = [
    (ResourceBillingUnit::LlmInputToken, "llm_input_token"),
    (ResourceBillingUnit::LlmOutputToken, "llm_output_token"),
    (
        ResourceBillingUnit::LlmCacheReadToken,
        "llm_cache_read_token",
    ),
    (
        ResourceBillingUnit::LlmCacheWriteToken,
        "llm_cache_write_token",
    ),
];

fn quantities(input: &ModelTokenQuantities) -> [u64; 4] {
    [
        input.input_tokens,
        input.output_tokens,
        input.cache_read_tokens,
        input.cache_write_tokens,
    ]
}

async fn estimate_in_transaction(
    tx: &mut Transaction<'_, Postgres>,
    input: &ModelCostEstimateRequest,
    project_id: contracts::ProjectId,
) -> Result<ModelCostEstimate, ResourceStoreError> {
    if input.model_binding.trim().is_empty() || input.model_binding.len() > 256 {
        return Err(ResourceStoreError::Contract(
            contracts::resource::ResourceError::InvalidRate,
        ));
    }
    let values = quantities(&input.quantities);
    let mut lines = Vec::new();
    let mut total = 0_i128;
    let mut max_hold: Option<FixedDecimal> = None;
    for ((unit, wire), quantity) in UNITS.into_iter().zip(values) {
        let row = sqlx::query("SELECT contract FROM resource.resource_rates WHERE unit=$1 AND model_binding=$2 AND effective_from<=clock_timestamp() AND (effective_until IS NULL OR effective_until>clock_timestamp()) ORDER BY effective_from DESC, revision DESC LIMIT 1")
            .bind(wire)
            .bind(&input.model_binding)
            .fetch_optional(&mut **tx).await?;
        let row = row.ok_or(ResourceStoreError::RateUnconfigured)?;
        let rate: contracts::resource::ResourceRate =
            serde_json::from_value(row.try_get("contract")?)?;
        rate.validate().map_err(ResourceStoreError::Contract)?;
        let configured_max = rate
            .max_preauthorization
            .clone()
            .ok_or(ResourceStoreError::RateUnconfigured)?;
        if let Some(previous) = &max_hold {
            if previous != &configured_max {
                return Err(ResourceStoreError::RateCurrencyConflict);
            }
        } else {
            max_hold = Some(configured_max);
        }
        let amount = pricing::multiply_money(&rate.unit_price, quantity, rate.unit_quantity, 1)?;
        total = total
            .checked_add(amount.amount.scaled())
            .ok_or(ResourceStoreError::NumericOverflow)?;
        lines.push(ResourceChargeLine {
            quantity_divisor: 1,
            rate_id: rate.id,
            rate_revision: rate.revision,
            unit,
            quantity,
            unit_quantity: rate.unit_quantity,
            unit_price: rate.unit_price,
            amount,
        });
    }
    Ok(ModelCostEstimate {
        call_id: input.call_id,
        project_id,
        model_binding: input.model_binding.clone(),
        lines,
        total: Money {
            currency: "CREDITS".into(),
            amount: FixedDecimal::from_scaled(total)?,
        },
        max_preauthorization: max_hold.ok_or(ResourceStoreError::RateUnconfigured)?,
    })
}

impl PgResourceStore {
    pub async fn admit_model_cost(
        &self,
        key: &str,
        owner: &str,
        input: &ModelCostAdmissionRequest,
    ) -> Result<ModelCostAdmission, ResourceStoreError> {
        let mut tx = self.pool().begin().await?;
        if let Some(replay) =
            credits::reserve(&mut tx, "model_cost_admission", key, &(owner, input)).await?
        {
            let original: ModelCostAdmission = serde_json::from_value(replay)?;
            let funded: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM resource.credit_holds WHERE hold_id=$1 AND owner_client_id=$2 AND model_call_id=$3 AND state='unknown' AND remaining >= $4::text::numeric AND reconciliation_due_at>clock_timestamp() AND expired_unknown_at IS NULL)")
                .bind(original.hold.id).bind(owner).bind(input.call_id).bind(original.estimate.max_preauthorization.scaled().to_string())
                .fetch_one(&mut *tx).await?;
            if !funded {
                return Err(credits::CreditError::Insufficient.into());
            }
            return Ok(original);
        }
        let project = input.project_id;
        let estimate = estimate_in_transaction(
            &mut tx,
            &ModelCostEstimateRequest {
                call_id: input.call_id,
                project_id: project,
                model_binding: input.model_binding.clone(),
                quantities: ModelTokenQuantities::default(),
            },
            project,
        )
        .await?;
        let mut hold = credits::model_hold_in_transaction(
            &mut tx,
            key,
            owner,
            project,
            input.call_id,
            &estimate.max_preauthorization,
        )
        .await?;
        hold.model_call_id = Some(input.call_id);
        hold.model_rate_snapshot = Some(
            estimate
                .lines
                .iter()
                .map(
                    |line| contracts::resource::model_costing::ModelRateSnapshot {
                        unit: line.unit,
                        rate_id: line.rate_id,
                        rate_revision: line.rate_revision,
                    },
                )
                .collect(),
        );
        sqlx::query("UPDATE resource.credit_holds SET model_call_id=$2,model_rate_snapshot=$3 WHERE hold_id=$1").bind(hold.id).bind(input.call_id).bind(serde_json::to_value(&hold.model_rate_snapshot)?).execute(&mut *tx).await?;
        credits::require_funded_in_transaction(
            &mut tx,
            owner,
            hold.id,
            &estimate.max_preauthorization,
        )
        .await?;
        let admission_event = contracts::EventId::new();
        let measurement = ModelUsageMeasurement::Unknown {
            reason:
                "admitted model invocation has not supplied a complete terminal usage observation"
                    .into(),
            observed_quantities: None,
        };
        // The finite reconciliation deadline commits with the hold. A lost admission response or
        // killed Agent cannot strand a Pending hold before a second RPC arrives.
        hold = credits::resolve_in_transaction(
            &mut tx,
            owner,
            hold.id,
            &CreditHoldResolutionRequest::Unknown {
                event_id: Some(admission_event),
                reason: "model invocation admitted; terminal usage not yet known".into(),
            },
            None,
            Some(admission_event),
        )
        .await?;
        insert_observation(
            &mut tx,
            &ModelCostObservation {
                hold_id: hold.id,
                call_id: input.call_id,
                event_id: admission_event,
                model_binding: input.model_binding.clone(),
                measurement,
                lines: Vec::new(),
                observed_total: None,
                charged_amount: FixedDecimal::from_scaled(0)?,
            },
        )
        .await?;
        let result = ModelCostAdmission { hold, estimate };
        credits::complete(&mut tx, "model_cost_admission", key, &result).await?;
        tx.commit().await?;
        Ok(result)
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "model settlement carries the authenticated call and event fences"
    )]
    pub async fn settle_model_cost(
        &self,
        _key: &str,
        owner: &str,
        hold_id: uuid::Uuid,
        call_id: uuid::Uuid,
        event_id: contracts::EventId,
        model_binding: &str,
        measurement: &ModelUsageMeasurement,
    ) -> Result<CreditHold, ResourceStoreError> {
        let mut tx = self.pool().begin().await?;
        if let Some(replay) = credits::reserve(
            &mut tx,
            "model_cost_event",
            &format!("{call_id}:{event_id}"),
            &(owner, hold_id, call_id, model_binding, measurement),
        )
        .await?
        {
            return Ok(serde_json::from_value(replay)?);
        }
        let row = sqlx::query("SELECT project_id,model_call_id,model_rate_snapshot FROM resource.credit_holds WHERE hold_id=$1")
            .bind(hold_id).fetch_one(&mut *tx).await?;
        let stored_call: Option<uuid::Uuid> = row.try_get("model_call_id")?;
        if stored_call != Some(call_id) {
            return Err(ResourceStoreError::ScopeConflict);
        }
        let snapshot: Vec<contracts::resource::model_costing::ModelRateSnapshot> =
            serde_json::from_value(
                row.try_get::<Option<serde_json::Value>, _>("model_rate_snapshot")?
                    .ok_or(ResourceStoreError::RateUnconfigured)?,
            )?;
        let observed = match measurement {
            ModelUsageMeasurement::Known { quantities } => Some(quantities),
            ModelUsageMeasurement::Unknown {
                observed_quantities,
                ..
            } => observed_quantities.as_ref(),
        };
        let lines = if let Some(observed) = observed {
            snapshot_lines(&mut tx, &snapshot, model_binding, *observed).await?
        } else {
            Vec::new()
        };
        let total = lines.iter().try_fold(0_i128, |sum, line| {
            sum.checked_add(line.amount.amount.scaled())
                .ok_or(ResourceStoreError::NumericOverflow)
        })?;
        let resolution = match measurement {
            ModelUsageMeasurement::Unknown { reason, .. } => CreditHoldResolutionRequest::Unknown {
                event_id: Some(event_id),
                reason: reason.clone(),
            },
            ModelUsageMeasurement::Known { .. } => CreditHoldResolutionRequest::Settle {
                event_id,
                amount: FixedDecimal::from_scaled(total)?,
                finalize: true,
            },
        };
        let hold = credits::resolve_in_transaction(
            &mut tx,
            owner,
            hold_id,
            &resolution,
            None,
            Some(event_id),
        )
        .await?;
        let charged_scaled: String = sqlx::query_scalar("SELECT COALESCE(sum(spent_delta),0)::text FROM resource.credit_ledger_entries WHERE hold_id=$1 AND source_event_id=$2 AND project_id IS NOT NULL")
            .bind(hold_id).bind(event_id.as_uuid()).fetch_one(&mut *tx).await?;
        insert_observation(
            &mut tx,
            &ModelCostObservation {
                hold_id,
                call_id,
                event_id,
                model_binding: model_binding.to_owned(),
                measurement: measurement.clone(),
                lines,
                observed_total: if observed.is_some() {
                    Some(Money {
                        currency: "CREDITS".into(),
                        amount: FixedDecimal::from_scaled(total)?,
                    })
                } else {
                    None
                },
                charged_amount: FixedDecimal::from_scaled(
                    charged_scaled
                        .parse()
                        .map_err(|_| ResourceStoreError::NumericOverflow)?,
                )?,
            },
        )
        .await?;
        credits::complete(
            &mut tx,
            "model_cost_event",
            &format!("{call_id}:{event_id}"),
            &hold,
        )
        .await?;
        tx.commit().await?;
        Ok(hold)
    }
}

async fn snapshot_lines(
    tx: &mut Transaction<'_, Postgres>,
    snapshot: &[ModelRateSnapshot],
    model: &str,
    usage: ModelTokenQuantities,
) -> Result<Vec<ResourceChargeLine>, ResourceStoreError> {
    let mut lines = Vec::with_capacity(4);
    for ((unit, _), quantity) in UNITS.into_iter().zip(quantities(&usage)) {
        let saved = snapshot
            .iter()
            .find(|entry| entry.unit == unit)
            .ok_or(ResourceStoreError::RateUnconfigured)?;
        let row = sqlx::query(
            "SELECT contract FROM resource.resource_rates WHERE rate_id=$1 AND model_binding=$2",
        )
        .bind(saved.rate_id.as_uuid())
        .bind(model)
        .fetch_one(&mut **tx)
        .await?;
        let rate: contracts::resource::ResourceRate =
            serde_json::from_value(row.try_get("contract")?)?;
        if rate.revision != saved.rate_revision {
            return Err(ResourceStoreError::RateSettledConflict);
        }
        let amount = pricing::multiply_money(&rate.unit_price, quantity, rate.unit_quantity, 1)?;
        lines.push(ResourceChargeLine {
            quantity_divisor: 1,
            rate_id: rate.id,
            rate_revision: rate.revision,
            unit,
            quantity,
            unit_quantity: rate.unit_quantity,
            unit_price: rate.unit_price,
            amount,
        });
    }
    Ok(lines)
}

async fn insert_observation(
    tx: &mut Transaction<'_, Postgres>,
    observation: &ModelCostObservation,
) -> Result<(), ResourceStoreError> {
    sqlx::query("INSERT INTO resource.model_cost_observations(event_id,call_id,hold_id,contract) VALUES($1,$2,$3,$4)")
        .bind(observation.event_id.as_uuid()).bind(observation.call_id).bind(observation.hold_id)
        .bind(serde_json::to_value(observation)?).execute(&mut **tx).await?;
    Ok(())
}
