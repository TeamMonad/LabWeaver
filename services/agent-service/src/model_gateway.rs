use axum::{
    Router,
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::post,
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use contracts::{
    ProjectId,
    resource::model_costing::{
        ModelCostAdmissionRequest, ModelCostSettlementRequest, ModelTokenQuantities,
        ModelUsageMeasurement,
    },
};
use futures_util::StreamExt;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
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

const TOKEN_VERSION: &str = "lw1";

/// A signed, short-lived capability for one model run attempt.
#[derive(Clone, Debug)]
pub struct ScopedTokenIssuer {
    key: Arc<[u8]>,
    ttl: Duration,
}

#[derive(Debug, Deserialize, Serialize)]
struct TokenClaims {
    v: String,
    project_id: String,
    agent_run_id: Uuid,
    attempt: u32,
    expires_at: u64,
}

impl ScopedTokenIssuer {
    /// Creates a signer. HMAC is used rather than local registration so every Agent replica can
    /// validate a capability without shared process state or a new database table.
    pub fn new(key: impl Into<Vec<u8>>, ttl: Duration) -> Option<Self> {
        let key = key.into();
        if key.len() < 32 || ttl.is_zero() {
            return None;
        }
        Some(Self {
            key: Arc::from(key),
            ttl,
        })
    }

    /// Issues a capability scoped to exactly one project, run and attempt.
    #[must_use]
    pub fn issue(&self, project_id: ProjectId, agent_run_id: Uuid, attempt: u32) -> String {
        let expires_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            .saturating_add(self.ttl.as_secs());
        let claims = TokenClaims {
            v: TOKEN_VERSION.to_owned(),
            project_id: project_id.to_string(),
            agent_run_id,
            attempt,
            expires_at,
        };
        let payload = serde_json::to_vec(&claims).unwrap_or_default();
        let encoded = URL_SAFE_NO_PAD.encode(payload);
        let signature = hmac_sha256(&self.key, encoded.as_bytes());
        format!("{encoded}.{}", URL_SAFE_NO_PAD.encode(signature))
    }

    fn validate(&self, token: &str) -> Result<TokenClaims, TokenValidationError> {
        let (encoded, signature) = token
            .split_once('.')
            .ok_or(TokenValidationError::Malformed)?;
        let supplied = URL_SAFE_NO_PAD
            .decode(signature)
            .map_err(|_| TokenValidationError::Malformed)?;
        let expected = hmac_sha256(&self.key, encoded.as_bytes());
        if !constant_time_eq(&supplied, &expected) {
            return Err(TokenValidationError::Signature);
        }
        let claims: TokenClaims = serde_json::from_slice(
            &URL_SAFE_NO_PAD
                .decode(encoded)
                .map_err(|_| TokenValidationError::Malformed)?,
        )
        .map_err(|_| TokenValidationError::Malformed)?;
        if claims.v != TOKEN_VERSION {
            return Err(TokenValidationError::Malformed);
        }
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        if now >= claims.expires_at {
            return Err(TokenValidationError::Expired);
        }
        if claims.project_id.parse::<ProjectId>().is_err() {
            return Err(TokenValidationError::Malformed);
        }
        Ok(claims)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TokenValidationError {
    Malformed,
    Signature,
    Expired,
}

/// Runtime values shared by both Agent model execution paths.
#[derive(Clone, Debug)]
pub struct ModelGatewayRuntime {
    pub base_url: Url,
    pub issuer: ScopedTokenIssuer,
}

impl ModelGatewayRuntime {
    #[must_use]
    pub fn environment(
        &self,
        project_id: ProjectId,
        agent_run_id: Uuid,
        attempt: u32,
    ) -> BTreeMap<String, String> {
        let mut environment = BTreeMap::new();
        environment.insert("ANTHROPIC_BASE_URL".to_owned(), self.base_url.to_string());
        environment.insert(
            "ANTHROPIC_API_KEY".to_owned(),
            self.issuer.issue(project_id, agent_run_id, attempt),
        );
        environment
    }
}

#[derive(Clone)]
pub struct GatewayState {
    pub upstream: Url,
    pub upstream_key: String,
    /// Legacy single-capability fields remain private compatibility for unit callers. Production
    /// always supplies `issuer` and never accepts a provider token from a worker.
    pub token: String,
    pub expires_at: SystemTime,
    pub project_id: ProjectId,
    pub agent_run_id: Uuid,
    pub attempt: u32,
    pub resource: ResourceClient,
    pub sequence: Arc<AtomicU64>,
    pub http: reqwest::Client,
    pub issuer: Option<ScopedTokenIssuer>,
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

#[allow(
    clippy::manual_let_else,
    clippy::single_match_else,
    clippy::too_many_lines
)]
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
    let token = auth.strip_prefix("Bearer ").unwrap_or_default();
    let (project_id, agent_run_id, attempt) = if let Some(issuer) = &state.issuer {
        let claims = match issuer.validate(token) {
            Ok(claims) => claims,
            Err(TokenValidationError::Expired) => {
                return error_response(
                    StatusCode::UNAUTHORIZED,
                    "LW_MODEL_GATEWAY_TOKEN_EXPIRED",
                    "gateway token expired",
                );
            }
            Err(_) => {
                return error_response(
                    StatusCode::FORBIDDEN,
                    "LW_MODEL_GATEWAY_TOKEN_SCOPE",
                    "gateway token scope denied",
                );
            }
        };
        let project_id = match claims.project_id.parse::<ProjectId>() {
            Ok(value) => value,
            Err(_) => {
                return error_response(
                    StatusCode::FORBIDDEN,
                    "LW_MODEL_GATEWAY_TOKEN_SCOPE",
                    "gateway token scope denied",
                );
            }
        };
        (project_id, claims.agent_run_id, claims.attempt)
    } else {
        if token != state.token {
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
        (state.project_id, state.agent_run_id, state.attempt)
    };
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
    let call_id = headers
        .get("x-labweaver-call-id")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| Uuid::parse_str(value).ok())
        .unwrap_or_else(|| {
            let sequence = state
                .sequence
                .fetch_add(1, Ordering::AcqRel)
                .saturating_add(1);
            deterministic_uuid(&[
                agent_run_id.as_bytes(),
                &attempt.to_be_bytes(),
                &sequence.to_be_bytes(),
            ])
        });
    let admission = match state
        .resource
        .admit_model_cost(
            &call_id.to_string(),
            &ModelCostAdmissionRequest {
                call_id,
                project_id,
                model_binding: model.clone(),
            },
        )
        .await
    {
        Ok(value) => value,
        Err(
            task_execution::resource::ResourceClientError::Denied
            | task_execution::resource::ResourceClientError::Rejected,
        ) => {
            return error_response(
                StatusCode::PAYMENT_REQUIRED,
                "LW_CREDITS_INSUFFICIENT",
                "model Credits admission rejected",
            );
        }
        Err(_) => {
            return error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "LW_MODEL_ADMISSION_UNAVAILABLE",
                "model Credits admission unavailable",
            );
        }
    };
    let upstream_url = state
        .upstream
        .join("v1/messages")
        .unwrap_or_else(|_| state.upstream.clone());
    let mut request = state
        .http
        .post(upstream_url)
        .header(header::CONTENT_TYPE, "application/json")
        .header(
            header::AUTHORIZATION,
            format!("Bearer {}", state.upstream_key),
        )
        .body(body);
    if let Some(value) = headers.get("anthropic-version") {
        request = request.header("anthropic-version", value);
    }
    let upstream = match request.send().await {
        Ok(value) => value,
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
    let status = upstream.status();
    let content_type = upstream.headers().get(header::CONTENT_TYPE).cloned();
    if content_type
        .as_ref()
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.contains("text/event-stream"))
    {
        let stream = gateway_stream(
            upstream.bytes_stream(),
            state.clone(),
            admission.hold.id,
            call_id,
            model.clone(),
        );
        let mut builder = Response::builder().status(status);
        if let Some(value) = content_type {
            builder = builder.header(header::CONTENT_TYPE, value);
        }
        return builder
            .body(axum::body::Body::from_stream(stream))
            .unwrap_or_else(|_| {
                error_response(
                    StatusCode::BAD_GATEWAY,
                    "LW_MODEL_GATEWAY_RESPONSE_INVALID",
                    "invalid model response",
                )
            });
    }
    let bytes = match upstream.bytes().await {
        Ok(value) => value,
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
    let measurement = extract_usage(
        &bytes,
        content_type
            .as_ref()
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default(),
    )
    .map_or_else(
        || ModelUsageMeasurement::Unknown {
            reason: "upstream response did not contain usage".into(),
        },
        |quantities| ModelUsageMeasurement::Known { quantities },
    );
    let _ = settle(&state, admission.hold.id, call_id, &model, measurement).await;
    let mut builder = Response::builder().status(status);
    if let Some(value) = content_type {
        builder = builder.header(header::CONTENT_TYPE, value);
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

fn gateway_stream<S>(
    stream: S,
    state: Arc<GatewayState>,
    hold_id: Uuid,
    call_id: Uuid,
    model: String,
) -> impl futures_util::Stream<Item = Result<Bytes, std::io::Error>>
where
    S: futures_util::Stream<Item = Result<Bytes, reqwest::Error>> + Unpin + Send + 'static,
{
    futures_util::stream::unfold(
        (stream, Vec::new(), state, hold_id, call_id, model),
        |(mut stream, mut body, state, hold_id, call_id, model)| async move {
            match stream.next().await {
                Some(Ok(chunk)) => {
                    body.extend_from_slice(&chunk);
                    Some((Ok(chunk), (stream, body, state, hold_id, call_id, model)))
                }
                Some(Err(_)) => {
                    let _ = settle_unknown(&state, hold_id, call_id, &model, "stream interrupted")
                        .await;
                    None
                }
                None => {
                    let measurement = extract_usage(&body, "text/event-stream").map_or_else(
                        || ModelUsageMeasurement::Unknown {
                            reason: "stream ended without usage".into(),
                        },
                        |quantities| ModelUsageMeasurement::Known { quantities },
                    );
                    let _ = settle(&state, hold_id, call_id, &model, measurement).await;
                    None
                }
            }
        },
    )
}

async fn settle(
    state: &GatewayState,
    hold_id: Uuid,
    call_id: Uuid,
    model: &str,
    measurement: ModelUsageMeasurement,
) -> Result<(), task_execution::resource::ResourceClientError> {
    state
        .resource
        .settle_model_cost(
            hold_id,
            &ModelCostSettlementRequest {
                hold_id,
                event_id: deterministic_event_id(&[call_id.as_bytes(), b"settle"]),
                call_id,
                model_binding: model.to_owned(),
                measurement,
            },
        )
        .await
        .map(|_| ())
}
async fn settle_unknown(
    state: &GatewayState,
    hold_id: Uuid,
    call_id: Uuid,
    model: &str,
    reason: &str,
) -> Result<(), task_execution::resource::ResourceClientError> {
    settle(
        state,
        hold_id,
        call_id,
        model,
        ModelUsageMeasurement::Unknown {
            reason: reason.to_owned(),
        },
    )
    .await
}

fn extract_usage(body: &[u8], content_type: &str) -> Option<ModelTokenQuantities> {
    if content_type.contains("text/event-stream") {
        return extract_sse_usage(body);
    }
    let value: Value = serde_json::from_slice(body).ok()?;
    Some(usage_value(value.get("usage")?))
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
fn usage_value(value: &Value) -> ModelTokenQuantities {
    merge_usage(ModelTokenQuantities::default(), value)
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

fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    let mut block = [0_u8; 64];
    if key.len() > block.len() {
        block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36_u8; 64];
    let mut opad = [0x5c_u8; 64];
    for index in 0..64 {
        ipad[index] ^= block[index];
        opad[index] ^= block[index];
    }
    let mut inner = Sha256::new();
    inner.update(ipad);
    inner.update(message);
    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(inner.finalize());
    outer.finalize().into()
}
fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |diff, (a, b)| diff | (a ^ b))
        == 0
}

#[must_use]
pub fn token_expiry(ttl: Duration) -> SystemTime {
    SystemTime::now().checked_add(ttl).unwrap_or(UNIX_EPOCH)
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::duration_suboptimal_units
)]
mod tests {
    use super::*;
    #[test]
    fn scoped_tokens_are_bound_and_signed() {
        let project = ProjectId::new();
        let issuer = ScopedTokenIssuer::new(vec![7; 32], Duration::from_secs(300)).unwrap();
        let token = issuer.issue(project, Uuid::new_v4(), 3);
        let claims = issuer.validate(&token).unwrap();
        assert_eq!(claims.project_id, project.to_string());
        assert_eq!(claims.attempt, 3);
        let mut tampered = token.into_bytes();
        tampered[3] = b'X';
        assert!(
            issuer
                .validate(std::str::from_utf8(&tampered).unwrap())
                .is_err()
        );
    }
    #[test]
    fn usage_aggregates_all_four_dimensions() {
        let usage = extract_usage(br#"{"usage":{"input_tokens":1,"output_tokens":2,"cache_read_input_tokens":3,"cache_creation_input_tokens":4}}"#, "application/json").unwrap();
        assert_eq!(usage.input_tokens, 1);
        assert_eq!(usage.output_tokens, 2);
        assert_eq!(usage.cache_read_tokens, 3);
        assert_eq!(usage.cache_write_tokens, 4);
    }
}
