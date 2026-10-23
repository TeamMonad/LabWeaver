//! Model token admission and settlement uses the same Credits hold owner as Resource.
use crate::{
    credits, pricing,
    store::{PgResourceStore, ResourceStoreError},
};
use contracts::{
    ResourceRequestId,
    resource::{
        FixedDecimal, Money, ResourceBillingUnit, ResourceChargeLine,
        credits::{CreditHold, CreditHoldRequest, CreditHoldResolutionRequest},
        model_costing::{
            ModelCostAdmission, ModelCostAdmissionRequest, ModelCostEstimate,
            ModelCostEstimateRequest, ModelTokenQuantities, ModelUsageMeasurement,
        },
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
        if quantity == 0 {
            continue;
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
        request_id: input.request_id,
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
        let row = sqlx::query(
            "SELECT project_id FROM resource.resource_requests WHERE request_id=$1 FOR UPDATE",
        )
        .bind(input.request_id.as_uuid())
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(ResourceStoreError::NotFound)?;
        let project: contracts::ProjectId = row
            .try_get::<uuid::Uuid, _>("project_id")?
            .to_string()
            .parse()
            .map_err(|_| ResourceStoreError::ScopeConflict)?;
        let estimate = estimate_in_transaction(
            &mut tx,
            &ModelCostEstimateRequest {
                request_id: input.request_id,
                model_binding: input.model_binding.clone(),
                quantities: ModelTokenQuantities::default(),
            },
            project,
        )
        .await?;
        let hold = credits::hold_in_transaction(
            &mut tx,
            key,
            owner,
            &CreditHoldRequest {
                project_id: project,
                request_id: input.request_id,
                business_key: "llm-usage".into(),
                amount: estimate.max_preauthorization.clone(),
            },
        )
        .await?;
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

    pub async fn settle_model_cost(
        &self,
        key: &str,
        owner: &str,
        hold_id: uuid::Uuid,
        event_id: contracts::EventId,
        model_binding: &str,
        measurement: &ModelUsageMeasurement,
    ) -> Result<CreditHold, ResourceStoreError> {
        let mut tx = self.pool().begin().await?;
        if let Some(replay) = credits::reserve(
            &mut tx,
            "model_cost_event",
            &event_id.to_string(),
            &(owner, hold_id, model_binding, measurement),
        )
        .await?
        {
            return Ok(serde_json::from_value(replay)?);
        }
        let row =
            sqlx::query("SELECT project_id,request_id FROM resource.credit_holds WHERE hold_id=$1")
                .bind(hold_id)
                .fetch_one(&mut *tx)
                .await?;
        let project: contracts::ProjectId = row
            .try_get::<uuid::Uuid, _>("project_id")?
            .to_string()
            .parse()
            .map_err(|_| ResourceStoreError::ScopeConflict)?;
        let request_id: ResourceRequestId = row
            .try_get::<uuid::Uuid, _>("request_id")?
            .to_string()
            .parse()
            .map_err(|_| ResourceStoreError::ScopeConflict)?;
        let resolution = match measurement {
            ModelUsageMeasurement::Unknown { reason } => CreditHoldResolutionRequest::Unknown {
                event_id: Some(event_id),
                reason: reason.clone(),
            },
            ModelUsageMeasurement::Known { quantities } => {
                let estimate = estimate_in_transaction(
                    &mut tx,
                    &ModelCostEstimateRequest {
                        request_id,
                        model_binding: model_binding.to_owned(),
                        quantities: *quantities,
                    },
                    project,
                )
                .await?;
                CreditHoldResolutionRequest::Settle {
                    event_id,
                    amount: estimate.total.amount,
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
        credits::complete(&mut tx, "model_cost_event", &event_id.to_string(), &hold).await?;
        credits::complete(&mut tx, "model_cost_settle", key, &hold).await?;
        tx.commit().await?;
        Ok(hold)
    }
}
