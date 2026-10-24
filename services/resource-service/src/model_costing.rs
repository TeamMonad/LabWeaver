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
        ModelTokenQuantities, ModelUsageMeasurement,
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
    input
        .quantities
        .validate()
        .map_err(ResourceStoreError::Contract)?;
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
            return Ok(serde_json::from_value(replay)?);
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
        let resolution = match measurement {
            ModelUsageMeasurement::Unknown { reason } => CreditHoldResolutionRequest::Unknown {
                event_id: Some(event_id),
                reason: reason.clone(),
            },
            ModelUsageMeasurement::Known { quantities: usage } => {
                let mut total = 0_i128;
                for ((unit, _), quantity) in UNITS.into_iter().zip(quantities(usage)) {
                    if quantity == 0 {
                        continue;
                    }
                    let saved = snapshot
                        .iter()
                        .find(|entry| entry.unit == unit)
                        .ok_or(ResourceStoreError::RateUnconfigured)?;
                    let rate_row = sqlx::query("SELECT contract FROM resource.resource_rates WHERE rate_id=$1 AND model_binding=$2").bind(saved.rate_id.as_uuid()).bind(model_binding).fetch_one(&mut *tx).await?;
                    let rate: contracts::resource::ResourceRate =
                        serde_json::from_value(rate_row.try_get("contract")?)?;
                    if rate.revision != saved.rate_revision {
                        return Err(ResourceStoreError::RateSettledConflict);
                    }
                    total = total
                        .checked_add(
                            pricing::multiply_money(
                                &rate.unit_price,
                                quantity,
                                rate.unit_quantity,
                                1,
                            )?
                            .amount
                            .scaled(),
                        )
                        .ok_or(ResourceStoreError::NumericOverflow)?;
                }
                CreditHoldResolutionRequest::Settle {
                    event_id,
                    amount: FixedDecimal::from_scaled(total)?,
                    finalize: true,
                }
            }
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
