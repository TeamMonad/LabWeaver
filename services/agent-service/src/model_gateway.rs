//! Agent-owned Anthropic transport: scoped capability, durable call fence, Resource accounting.
use axum::{
    Router,
    body::{Body, Bytes},
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
use tokio::{net::TcpListener, sync::mpsc};
use uuid::Uuid;

#[derive(Clone)]
pub struct ScopedTokenIssuer {
    key: Arc<[u8]>,
    ttl: Duration,
}
impl std::fmt::Debug for ScopedTokenIssuer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScopedTokenIssuer")
            .field("ttl", &self.ttl)
            .finish_non_exhaustive()
    }
}
#[derive(Deserialize, Serialize)]
struct TokenClaims {
    project_id: ProjectId,
    agent_run_id: Uuid,
    attempt: u32,
    expires_at: u64,
}
impl ScopedTokenIssuer {
    /// HMAC permits verification by every replica; live run/attempt authority is checked in PG.
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
    #[must_use]
    pub fn issue(&self, project_id: ProjectId, agent_run_id: Uuid, attempt: u32) -> String {
        let expires_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            .saturating_add(self.ttl.as_secs());
        let claims = TokenClaims {
            project_id,
            agent_run_id,
            attempt,
            expires_at,
        };
        let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap_or_default());
        let signature = hmac_sha256(&self.key, payload.as_bytes());
        format!("{payload}.{}", URL_SAFE_NO_PAD.encode(signature))
    }
    fn validate(&self, token: &str) -> Result<TokenClaims, &'static str> {
        let (payload, signature) = token
            .split_once('.')
            .ok_or("LW_MODEL_GATEWAY_TOKEN_SCOPE")?;
        let supplied = URL_SAFE_NO_PAD
            .decode(signature)
            .map_err(|_| "LW_MODEL_GATEWAY_TOKEN_SCOPE")?;
        let expected = hmac_sha256(&self.key, payload.as_bytes());
        if supplied.len() != expected.len()
            || supplied
                .iter()
                .zip(expected)
                .fold(0_u8, |diff, (a, b)| diff | (a ^ b))
                != 0
        {
            return Err("LW_MODEL_GATEWAY_TOKEN_SCOPE");
        }
        let bytes = URL_SAFE_NO_PAD
            .decode(payload)
            .map_err(|_| "LW_MODEL_GATEWAY_TOKEN_SCOPE")?;
        let claims: TokenClaims =
            serde_json::from_slice(&bytes).map_err(|_| "LW_MODEL_GATEWAY_TOKEN_SCOPE")?;
        if SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            >= claims.expires_at
        {
            return Err("LW_MODEL_GATEWAY_TOKEN_EXPIRED");
        }
        Ok(claims)
    }
}
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
        BTreeMap::from([
            ("ANTHROPIC_BASE_URL".into(), self.base_url.to_string()),
            (
                "ANTHROPIC_API_KEY".into(),
                self.issuer.issue(project_id, agent_run_id, attempt),
            ),
        ])
    }
}
#[derive(Clone)]
pub struct GatewayState {
    pub upstream: Url,
    pub upstream_key: String,
    pub issuer: ScopedTokenIssuer,
    pub pool: sqlx::PgPool,
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
    async fn active(&self, claims: &TokenClaims) -> Result<bool, sqlx::Error> {
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM agent.llm_review_runs WHERE task_run_id=$1 AND project_id=$2 AND attempt=$3 AND state='running' AND lease_expires_at>clock_timestamp()) OR EXISTS(SELECT 1 FROM agent.agent_runs r JOIN agent.agent_track_work_items t USING(run_id) WHERE r.run_id=$1 AND r.project_id=$2 AND r.state='running' AND r.cancellation_requested_at IS NULL AND t.attempt_number=$3 AND t.state='running' AND t.lease_expires_at>clock_timestamp())")
            .bind(claims.agent_run_id).bind(claims.project_id.as_uuid()).bind(i64::from(claims.attempt)).fetch_one(&self.pool).await
    }
}
pub async fn serve(state: GatewayState, listener: TcpListener) -> Result<(), std::io::Error> {
    axum::serve(listener, state.router()).await
}
fn error_response(status: StatusCode, code: &'static str, message: &str) -> Response {
    (status,axum::Json(json!({"type":"error","error":{"type":"invalid_request_error","message":message},"lwCode":code}))).into_response()
}
fn identity(parts: &[&[u8]]) -> Uuid {
    let mut hash = Sha256::new();
    for part in parts {
        hash.update(part);
        hash.update([0]);
    }
    let mut bytes = [0; 16];
    bytes.copy_from_slice(&hash.finalize()[..16]);
    // EventId wire decoding requires UUIDv7. The payload is deterministic, never a random fallback.
    bytes[6] = (bytes[6] & 0x0f) | 0x70;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes)
}
#[allow(
    clippy::too_many_lines,
    clippy::manual_let_else,
    reason = "admission failures return one sanitized provider error"
)]
async fn messages(
    State(state): State<Arc<GatewayState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .or_else(|| {
            headers
                .get("x-api-key")
                .and_then(|value| value.to_str().ok())
        });
    let Some(token) = token else {
        return error_response(
            StatusCode::UNAUTHORIZED,
            "LW_MODEL_GATEWAY_TOKEN_MISSING",
            "gateway token required",
        );
    };
    let claims = match state.issuer.validate(token) {
        Ok(value) => value,
        Err(code) => {
            return error_response(StatusCode::UNAUTHORIZED, code, "gateway token rejected");
        }
    };
    if !matches!(state.active(&claims).await, Ok(true)) {
        return error_response(
            StatusCode::FORBIDDEN,
            "LW_MODEL_GATEWAY_TOKEN_SCOPE",
            "run attempt is no longer active",
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
    let Some(model) = input
        .get("model")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    else {
        return error_response(
            StatusCode::BAD_REQUEST,
            "LW_MODEL_GATEWAY_MODEL_MISSING",
            "model required",
        );
    };
    let model = model.to_owned();
    let supplied = headers
        .get("x-labweaver-call-id")
        .or_else(|| headers.get("idempotency-key"))
        .and_then(|value| value.to_str().ok());
    let sequence = state.sequence.fetch_add(1, Ordering::AcqRel).to_be_bytes();
    let invocation = Uuid::now_v7();
    let call_id = identity(&[
        claims.project_id.as_uuid().as_bytes(),
        claims.agent_run_id.as_bytes(),
        &claims.attempt.to_be_bytes(),
        supplied.map_or(sequence.as_slice(), str::as_bytes),
        if supplied.is_some() {
            b"retry"
        } else {
            invocation.as_bytes()
        },
    ]);
    let admission = match state
        .resource
        .admit_model_cost(
            &call_id.to_string(),
            &ModelCostAdmissionRequest {
                call_id,
                project_id: claims.project_id,
                model_binding: model.clone(),
            },
        )
        .await
    {
        Ok(value) => value,
        Err(_) => {
            return error_response(
                StatusCode::PAYMENT_REQUIRED,
                "LW_MODEL_ADMISSION_REJECTED",
                "model Credits admission rejected; no upstream request sent",
            );
        }
    };
    // Unknown is persisted before the external side effect. A killed replica or dropped handler
    // cannot leave an unbounded Pending hold: the existing one-hour reconciliation deadline applies.
    if settle(
        &state,
        admission.hold.id,
        call_id,
        &model,
        ModelUsageMeasurement::Unknown {
            reason: "model invocation in progress or outcome uncertain".into(),
        },
        b"unknown",
    )
    .await
    .is_err()
    {
        return error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "LW_MODEL_SETTLEMENT_UNAVAILABLE",
            "model hold could not be recorded",
        );
    }
    let request_hash = format!("{:x}", Sha256::digest(&body));
    let fenced=sqlx::query("INSERT INTO agent.idempotency_ledger(operation,idempotency_key,request_sha256,state) VALUES('model_gateway_call',$1,$2,'in_progress') ON CONFLICT DO NOTHING")
        .bind(call_id.to_string()).bind(&request_hash).execute(&state.pool).await;
    match fenced {
        Ok(result) if result.rows_affected() == 1 => {}
        _ => {
            return error_response(
                StatusCode::CONFLICT,
                "LW_MODEL_CALL_OUTCOME_UNKNOWN",
                "same call already accepted; query original hold instead of replaying upstream",
            );
        }
    }
    let (sender, receiver) = tokio::sync::oneshot::channel();
    // The task owns the irreversible request and settlement, independently of the HTTP handler.
    tokio::spawn(async move {
        let response = forward(state, headers, body, model, admission.hold.id, call_id).await;
        let _ = sender.send(response);
    });
    receiver.await.unwrap_or_else(|_| {
        error_response(
            StatusCode::BAD_GATEWAY,
            "LW_MODEL_USAGE_UNKNOWN",
            "model outcome unknown",
        )
    })
}
async fn forward(
    state: Arc<GatewayState>,
    headers: HeaderMap,
    body: Bytes,
    model: String,
    hold_id: Uuid,
    call_id: Uuid,
) -> Response {
    let Ok(url) = state.upstream.join("v1/messages") else {
        return error_response(
            StatusCode::BAD_GATEWAY,
            "LW_MODEL_UPSTREAM_FAILED",
            "invalid upstream route",
        );
    };
    let mut request = state
        .http
        .post(url)
        .header(header::CONTENT_TYPE, "application/json")
        .bearer_auth(&state.upstream_key)
        .header("x-api-key", &state.upstream_key)
        .body(body);
    for name in ["anthropic-version", "anthropic-beta"] {
        if let Some(value) = headers.get(name) {
            request = request.header(name, value);
        }
    }
    let Ok(upstream) = request.send().await else {
        return error_response(
            StatusCode::BAD_GATEWAY,
            "LW_MODEL_UPSTREAM_FAILED",
            "model upstream failed; hold pending reconciliation",
        );
    };
    let status = upstream.status();
    let content_type = upstream
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("application/json")
        .to_owned();
    let (sender, receiver) = mpsc::channel::<Result<Bytes, std::io::Error>>(8);
    let sse = content_type.contains("text/event-stream");
    tokio::spawn(async move {
        let mut stream = upstream.bytes_stream();
        let mut observation = UsageObservation::new(sse);
        loop {
            tokio::select! {
                biased;
                ()=sender.closed()=>break,
                chunk=stream.next()=>match chunk {
                    Some(Ok(bytes))=>{observation.observe(&bytes);if sender.send(Ok(bytes)).await.is_err(){break;}}
                    Some(Err(_))=>{let _=sender.send(Err(std::io::Error::other("upstream stream interrupted"))).await;break;}
                    None=>break,
                }
            }
        }
        if status.is_success()
            && let Some(quantities) = observation.quantities()
            && let Err(error) = settle(
                &state,
                hold_id,
                call_id,
                &model,
                ModelUsageMeasurement::Known { quantities },
                b"known",
            )
            .await
        {
            tracing::error!(event="agent.model.settlement_pending",error=%error,hold_id=%hold_id,"model usage settlement failed; original Unknown deadline remains authoritative");
        }
        // No terminal usage or error => leave the durably recorded Unknown, never zero.
    });
    let stream = futures_util::stream::unfold(receiver, |mut receiver| async move {
        receiver.recv().await.map(|value| (value, receiver))
    });
    (
        status,
        [(header::CONTENT_TYPE, content_type)],
        Body::from_stream(stream),
    )
        .into_response()
}
async fn settle(
    state: &GatewayState,
    hold_id: Uuid,
    call_id: Uuid,
    model: &str,
    measurement: ModelUsageMeasurement,
    phase: &[u8],
) -> Result<(), task_execution::resource::ResourceClientError> {
    let event_id =
        contracts::EventId::from_str(&identity(&[call_id.as_bytes(), phase]).to_string())
            .map_err(|_| task_execution::resource::ResourceClientError::RequestInvalid)?;
    state
        .resource
        .settle_model_cost(
            hold_id,
            &ModelCostSettlementRequest {
                hold_id,
                call_id,
                event_id,
                model_binding: model.to_owned(),
                measurement,
            },
        )
        .await
        .map(|_| ())
}
#[derive(Default)]
struct UsageObservation {
    sse: bool,
    bytes: Vec<u8>,
    usage: ModelTokenQuantities,
    seen: bool,
    invalid: bool,
}
impl UsageObservation {
    fn new(sse: bool) -> Self {
        Self {
            sse,
            ..Self::default()
        }
    }
    fn observe(&mut self, bytes: &[u8]) {
        if self.invalid {
            return;
        }
        self.bytes.extend_from_slice(bytes);
        if self.bytes.len() > 1024 * 1024 {
            self.invalid = true;
            self.bytes.clear();
            return;
        }
        if self.sse {
            while let Some(end) = self.bytes.iter().position(|byte| *byte == b'\n') {
                let line = &self.bytes[..end];
                if let Some(data) = line.strip_prefix(b"data:")
                    && let Ok(value) = serde_json::from_slice::<Value>(data)
                {
                    self.merge(&value);
                }
                self.bytes.drain(..=end);
            }
        }
    }
    fn merge(&mut self, value: &Value) {
        let usage = value.get("usage").or_else(|| {
            value
                .get("message")
                .and_then(|message| message.get("usage"))
        });
        if let Some(usage) = usage {
            for (field, quantity) in [
                ("input_tokens", &mut self.usage.input_tokens),
                ("output_tokens", &mut self.usage.output_tokens),
                ("cache_read_input_tokens", &mut self.usage.cache_read_tokens),
                (
                    "cache_creation_input_tokens",
                    &mut self.usage.cache_write_tokens,
                ),
            ] {
                if let Some(value) = usage.get(field).and_then(Value::as_u64) {
                    *quantity = value;
                    self.seen = true;
                }
            }
        }
    }
    fn quantities(mut self) -> Option<ModelTokenQuantities> {
        if self.invalid {
            return None;
        }
        if !self.sse
            && let Ok(value) = serde_json::from_slice::<Value>(&self.bytes)
        {
            self.merge(&value);
        }
        self.seen.then_some(self.usage)
    }
}
fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    let mut block = [0_u8; 64];
    if key.len() > 64 {
        block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let mut inner = [0x36; 64];
    let mut outer = [0x5c; 64];
    for index in 0..64 {
        inner[index] ^= block[index];
        outer[index] ^= block[index];
    }
    let mut hash = Sha256::new();
    hash.update(inner);
    hash.update(message);
    let digest = hash.finalize();
    let mut hash = Sha256::new();
    hash.update(outer);
    hash.update(digest);
    hash.finalize().into()
}
