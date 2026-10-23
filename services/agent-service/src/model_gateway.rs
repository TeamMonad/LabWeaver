use axum::{
    Router,
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::post,
};
use contracts::{
    ProjectId,
    resource::model_costing::{
        ModelCostAdmissionRequest, ModelCostSettlementRequest, ModelTokenQuantities,
        ModelUsageMeasurement,
    },
};
use reqwest::Url;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    str::FromStr,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use task_execution::resource::ResourceClient;
use tokio::net::TcpListener;
use uuid::Uuid;

#[derive(Clone)]
pub struct GatewayState {
    pub upstream: Url,
    pub upstream_key: String,
    pub token: String,
    pub expires_at: SystemTime,
    pub project_id: ProjectId,
    pub agent_run_id: Uuid,
    pub attempt: u32,
    pub resource: ResourceClient,
    pub sequence: Arc<AtomicU64>,
    pub http: reqwest::Client,
}

impl GatewayState {
    pub fn router(self) -> Router {
        Router::new()
            .route("/v1/messages", post(messages))
            .with_state(Arc::new(self))
    }
}

pub async fn serve(state: GatewayState, listener: TcpListener) -> Result<(), std::io::Error> {
    axum::serve(listener, state.router()).await
}

fn error_response(status: StatusCode, code: &'static str, message: &str) -> Response {
    let body = json!({"type":"error","error":{"type":"invalid_request_error","message":message},"lwCode":code});
    (
        status,
        [(header::CONTENT_TYPE, "application/json")],
        axum::Json(body),
    )
        .into_response()
}

fn deterministic_uuid(parts: &[&[u8]]) -> Uuid {
    let mut hash = Sha256::new();
    for part in parts {
        hash.update(part);
        hash.update([0]);
    }
    let bytes: [u8; 16] = hash.finalize()[..16].try_into().unwrap_or([0; 16]);
    Uuid::from_bytes(bytes)
}
fn deterministic_event_id(parts: &[&[u8]]) -> contracts::EventId {
    let uuid = deterministic_uuid(parts);
    contracts::EventId::from_str(&uuid.to_string()).unwrap_or_else(|_| contracts::EventId::new())
}

async fn messages(
    State(state): State<Arc<GatewayState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(auth) = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
    else {
        return error_response(
            StatusCode::UNAUTHORIZED,
            "LW_MODEL_GATEWAY_TOKEN_MISSING",
            "gateway token required",
        );
    };
    if auth.strip_prefix("Bearer ") != Some(state.token.as_str()) {
        return error_response(
            StatusCode::FORBIDDEN,
            "LW_MODEL_GATEWAY_TOKEN_SCOPE",
            "gateway token scope denied",
        );
    }
    if SystemTime::now() > state.expires_at {
        return error_response(
            StatusCode::UNAUTHORIZED,
            "LW_MODEL_GATEWAY_TOKEN_EXPIRED",
            "gateway token expired",
        );
    }
    let input: Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(_) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "LW_MODEL_GATEWAY_REQUEST_INVALID",
                "invalid Messages request",
            );
        }
    };
    let model = input
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    if model.is_empty() {
        return error_response(
            StatusCode::BAD_REQUEST,
            "LW_MODEL_GATEWAY_MODEL_MISSING",
            "model is required",
        );
    }
    let call_seq = state
        .sequence
        .fetch_add(1, Ordering::AcqRel)
        .saturating_add(1);
    let call_id = deterministic_uuid(&[
        state.agent_run_id.as_bytes(),
        &state.attempt.to_be_bytes(),
        &call_seq.to_be_bytes(),
    ]);
    let admission = state
        .resource
        .admit_model_cost(
            &call_id.to_string(),
            &ModelCostAdmissionRequest {
                call_id,
                project_id: state.project_id,
                model_binding: model.clone(),
            },
        )
        .await;
    let admission = match admission {
        Ok(value) => value,
        Err(_) => {
            return error_response(
                StatusCode::PAYMENT_REQUIRED,
                "LW_CREDITS_INSUFFICIENT",
                "model Credits admission rejected",
            );
        }
    };
    let mut upstream_request = state
        .http
        .post(
            state
                .upstream
                .join("v1/messages")
                .unwrap_or(state.upstream.clone()),
        )
        .header(header::CONTENT_TYPE, "application/json")
        .header(
            header::AUTHORIZATION,
            format!("Bearer {}", state.upstream_key),
        )
        .body(body);
    if let Some(value) = headers.get("anthropic-version") {
        upstream_request = upstream_request.header("anthropic-version", value);
    }
    let response = match upstream_request.send().await {
        Ok(response) => response,
        Err(_) => {
            let _ = settle_unknown(
                &state,
                admission.hold.id,
                call_id,
                &model,
                "upstream request failed",
            )
            .await;
            return error_response(
                StatusCode::BAD_GATEWAY,
                "LW_MODEL_UPSTREAM_FAILED",
                "model upstream failed",
            );
        }
    };
    let status = response.status();
    let content_type = response.headers().get(header::CONTENT_TYPE).cloned();
    let bytes = match response.bytes().await {
        Ok(bytes) => bytes,
        Err(_) => {
            let _ = settle_unknown(
                &state,
                admission.hold.id,
                call_id,
                &model,
                "upstream response incomplete",
            )
            .await;
            return error_response(
                StatusCode::BAD_GATEWAY,
                "LW_MODEL_USAGE_UNKNOWN",
                "model usage could not be reconciled",
            );
        }
    };
    let usage = extract_usage(
        &bytes,
        content_type
            .as_ref()
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default(),
    );
    let measurement = usage.map_or_else(
        || ModelUsageMeasurement::Unknown {
            reason: "upstream response did not contain usage".into(),
        },
        |quantities| ModelUsageMeasurement::Known { quantities },
    );
    let settlement = ModelCostSettlementRequest {
        hold_id: admission.hold.id,
        event_id: deterministic_event_id(&[call_id.as_bytes(), b"settle"]),
        call_id,
        model_binding: model,
        measurement,
    };
    let _ = state
        .resource
        .settle_model_cost(admission.hold.id, &settlement)
        .await;
    let mut builder = Response::builder().status(status);
    if let Some(content_type) = content_type {
        builder = builder.header(header::CONTENT_TYPE, content_type);
    }
    builder
        .body(axum::body::Body::from(bytes))
        .unwrap_or_else(|_| {
            error_response(
                StatusCode::BAD_GATEWAY,
                "LW_MODEL_GATEWAY_RESPONSE_INVALID",
                "invalid model response",
            )
        })
}

async fn settle_unknown(
    state: &GatewayState,
    hold_id: uuid::Uuid,
    call_id: Uuid,
    model: &str,
    reason: &str,
) -> Result<(), task_execution::resource::ResourceClientError> {
    state
        .resource
        .settle_model_cost(
            hold_id,
            &ModelCostSettlementRequest {
                hold_id,
                event_id: deterministic_event_id(&[call_id.as_bytes(), b"unknown"]),
                call_id,
                model_binding: model.to_owned(),
                measurement: ModelUsageMeasurement::Unknown {
                    reason: reason.to_owned(),
                },
            },
        )
        .await
        .map(|_| ())
}

fn extract_usage(body: &[u8], content_type: &str) -> Option<ModelTokenQuantities> {
    if content_type.contains("text/event-stream") {
        return extract_sse_usage(body);
    }
    let value: Value = serde_json::from_slice(body).ok()?;
    usage_value(value.get("usage")?)
}

fn extract_sse_usage(body: &[u8]) -> Option<ModelTokenQuantities> {
    let mut result = ModelTokenQuantities::default();
    let mut seen = false;
    for line in String::from_utf8_lossy(body)
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
    {
        let value: Value = serde_json::from_str(line).ok()?;
        if let Some(usage) = value
            .get("message")
            .and_then(|message| message.get("usage"))
            .or_else(|| value.get("usage"))
        {
            result = merge_usage(result, usage);
            seen = true;
        }
    }
    seen.then_some(result)
}

fn usage_value(value: &Value) -> Option<ModelTokenQuantities> {
    Some(merge_usage(ModelTokenQuantities::default(), value))
}
fn merge_usage(mut output: ModelTokenQuantities, value: &Value) -> ModelTokenQuantities {
    output.input_tokens = value
        .get("input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(output.input_tokens);
    output.output_tokens = value
        .get("output_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(output.output_tokens);
    output.cache_read_tokens = value
        .get("cache_read_input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(output.cache_read_tokens);
    output.cache_write_tokens = value
        .get("cache_creation_input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(output.cache_write_tokens);
    output
}

#[must_use]
pub fn token_expiry(ttl: Duration) -> SystemTime {
    SystemTime::now().checked_add(ttl).unwrap_or(UNIX_EPOCH)
}
