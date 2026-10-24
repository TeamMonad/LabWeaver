//! Agent-owned Anthropic transport: scoped capability, durable call fence, Resource accounting.
use axum::{
    Router,
    body::{Body, Bytes},
    extract::{DefaultBodyLimit, State},
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
    sync::Arc,
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
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ModelGatewayScope {
    Authoring {
        track: contracts::authoring::AgentTrackKind,
        task_run_id: contracts::TaskRunId,
        lease_sha256: String,
    },
    Review {
        lease_sha256: String,
    },
}

#[derive(Deserialize, Serialize)]
pub(crate) struct TokenClaims {
    pub(crate) project_id: ProjectId,
    pub(crate) agent_run_id: Uuid,
    pub(crate) attempt: u32,
    pub(crate) expires_at: u64,
    pub(crate) invocation_id: Uuid,
    pub(crate) scope: ModelGatewayScope,
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
    pub fn issue_for(
        &self,
        project_id: ProjectId,
        agent_run_id: Uuid,
        attempt: u32,
        scope: ModelGatewayScope,
        valid_for: Duration,
    ) -> String {
        let expires_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            .saturating_add(valid_for.min(self.ttl).as_secs());
        let claims = TokenClaims {
            project_id,
            agent_run_id,
            attempt,
            expires_at,
            invocation_id: Uuid::now_v7(),
            scope,
        };
        let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap_or_default());
        let signature = hmac_sha256(&self.key, payload.as_bytes());
        format!("{payload}.{}", URL_SAFE_NO_PAD.encode(signature))
    }
    fn validate(&self, token: &str) -> Result<TokenClaims, &'static str> {
        self.validate_at(
            token,
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        )
    }
    fn validate_at(&self, token: &str, now: u64) -> Result<TokenClaims, &'static str> {
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
        if now >= claims.expires_at {
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
        scope: ModelGatewayScope,
        valid_for: Duration,
    ) -> BTreeMap<String, String> {
        BTreeMap::from([
            ("ANTHROPIC_BASE_URL".into(), self.base_url.to_string()),
            (
                "ANTHROPIC_API_KEY".into(),
                self.issuer
                    .issue_for(project_id, agent_run_id, attempt, scope, valid_for),
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
    pub http: reqwest::Client,
    pub authorization: crate::model_input_authorization::GatewayInputAuthorization,
}
impl GatewayState {
    pub fn router(self) -> Router {
        Router::new()
            .route("/v1/messages", post(messages))
            .route("/authorize", post(authorize_tool))
            .layer(DefaultBodyLimit::max(16 * 1024 * 1024))
            .with_state(Arc::new(self))
    }
    async fn active(&self, claims: &TokenClaims) -> Result<bool, sqlx::Error> {
        let (token, expected) = match &claims.scope {
            ModelGatewayScope::Authoring {
                track,
                task_run_id,
                lease_sha256,
            } => {
                let track = match track {
                    contracts::authoring::AgentTrackKind::Environment => "environment",
                    contracts::authoring::AgentTrackKind::Evaluation => "evaluation",
                    contracts::authoring::AgentTrackKind::WorkConfiguration => "work_configuration",
                };
                let token = sqlx::query_scalar::<_, Uuid>("SELECT t.lease_token FROM agent.agent_runs r JOIN agent.agent_track_work_items t USING(run_id) JOIN agent.authoring_sandbox_attempts s ON s.run_id=t.run_id AND s.track=t.track AND s.attempt_number=t.attempt_number WHERE r.run_id=$1 AND r.project_id=$2 AND r.state='running' AND r.cancellation_requested_at IS NULL AND t.track=$4 AND t.attempt_number=$3 AND t.state='running' AND t.lease_expires_at>clock_timestamp() AND s.task_run_id=$5 AND s.binding IS NOT NULL AND s.state IN ('creating','submitted')")
                    .bind(claims.agent_run_id).bind(claims.project_id.as_uuid()).bind(i64::from(claims.attempt)).bind(track).bind(task_run_id.as_uuid()).fetch_optional(&self.pool).await?;
                (token, lease_sha256)
            }
            ModelGatewayScope::Review { lease_sha256 } => {
                let token = sqlx::query_scalar::<_, Uuid>("SELECT lease_token FROM agent.llm_review_runs WHERE task_run_id=$1 AND project_id=$2 AND attempt=$3 AND state='running' AND lease_expires_at>clock_timestamp()")
                    .bind(claims.agent_run_id).bind(claims.project_id.as_uuid()).bind(i64::from(claims.attempt)).fetch_optional(&self.pool).await?;
                (token, lease_sha256)
            }
        };
        Ok(token.is_some_and(|token| {
            contracts::http::model_input_parent_lease_sha256(token) == *expected
        }))
    }

    async fn authorize(&self, claims: &TokenClaims) -> Result<String, &'static str> {
        if !matches!(self.active(claims).await, Ok(true)) {
            return Err("LW_MODEL_INPUT_AUTHORIZATION_DENIED");
        }
        let model = self.authorization.check(&self.pool, claims).await?;
        if !matches!(self.active(claims).await, Ok(true)) {
            return Err("LW_MODEL_INPUT_AUTHORIZATION_DENIED");
        }
        Ok(model)
    }

    async fn preflight(
        &self,
        claims: &TokenClaims,
        headers: &HeaderMap,
        body: &[u8],
        model: &str,
    ) -> Result<(), &'static str> {
        let expected = self.authorize(claims).await?;
        if expected != model {
            return Err("LW_MODEL_INPUT_AUTHORIZATION_DENIED");
        }
        let input: Value = contracts::parse_strict_json(body)
            .map_err(|_| "LW_MODEL_INPUT_AUTHORIZATION_DENIED")?;
        let token = gateway_token(headers).ok_or("LW_MODEL_INPUT_AUTHORIZATION_DENIED")?;
        self.authorization
            .classify(&input, token, &self.upstream_key)
            .await
    }

    async fn refuse_before_send(
        &self,
        hold_id: Uuid,
        call_id: Uuid,
        model: &str,
        code: &'static str,
    ) -> Response {
        if settle(
            self,
            hold_id,
            call_id,
            model,
            ModelUsageMeasurement::Known {
                quantities: ModelTokenQuantities::default(),
            },
            b"denied-before-send",
        )
        .await
        .is_err()
        {
            return error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "LW_MODEL_SETTLEMENT_PENDING",
                "no model request sent; zero-usage hold release pending reconciliation",
            );
        }
        error_response(
            StatusCode::FORBIDDEN,
            code,
            "no model request sent; hold released without charge",
        )
    }

    async fn call_identity(
        &self,
        claims: &TokenClaims,
        headers: &HeaderMap,
        request_hash: &str,
    ) -> Result<Uuid, &'static str> {
        let project = claims.project_id.as_uuid();
        let attempt = claims.attempt.to_be_bytes();
        let scope: [&[u8]; 3] = [project.as_bytes(), claims.agent_run_id.as_bytes(), &attempt];
        if let Some(value) = headers
            .get("x-labweaver-call-id")
            .or_else(|| headers.get("idempotency-key"))
        {
            let value = value
                .to_str()
                .ok()
                .filter(|id| !id.is_empty() && id.len() <= 256)
                .ok_or("LW_MODEL_CALL_ID_REQUIRED")?;
            return Ok(identity(&[scope[0], scope[1], scope[2], value.as_bytes()]));
        }
        // The production Anthropic SDK labels initial requests with retry-count=0 and transport
        // retries with a positive count. Missing metadata is ambiguous and must not invoke a model.
        let retry = headers
            .get("x-stainless-retry-count")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u32>().ok())
            .ok_or("LW_MODEL_CALL_ID_REQUIRED")?;
        let invocation = claims.invocation_id.to_string();
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|_| "LW_MODEL_CALL_ID_UNAVAILABLE")?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
            .bind(&invocation)
            .execute(&mut *tx)
            .await
            .map_err(|_| "LW_MODEL_CALL_ID_UNAVAILABLE")?;
        let (count, original_key, original_hash): (i64, Option<String>, Option<String>) = sqlx::query_as("SELECT count(*)::bigint,min(idempotency_key),min(request_sha256) FROM agent.idempotency_ledger WHERE operation='model_sdk_invocation' AND idempotency_key LIKE $1")
            .bind(format!("{invocation}:%")).fetch_one(&mut *tx).await.map_err(|_| "LW_MODEL_CALL_ID_UNAVAILABLE")?;
        let key = if retry == 0 {
            // Each fresh SDK invocation gets a distinct durable ordinal, even for identical bodies.
            let ordinal = count.checked_add(1).ok_or("LW_MODEL_CALL_ID_UNAVAILABLE")?;
            let key = format!("{invocation}:{ordinal}");
            sqlx::query("INSERT INTO agent.idempotency_ledger(operation,idempotency_key,request_sha256,state) VALUES('model_sdk_invocation',$1,$2,'in_progress')")
                .bind(&key).bind(request_hash).execute(&mut *tx).await.map_err(|_| "LW_MODEL_CALL_ID_UNAVAILABLE")?;
            key
        } else {
            // Without an explicit per-request ID a retry is safe only if the capability has exactly
            // one original invocation. Never merge by body hash; the digest only rejects mutation.
            if count != 1 || original_hash.as_deref() != Some(request_hash) {
                return Err("LW_MODEL_CALL_REPLAY_AMBIGUOUS");
            }
            original_key.ok_or("LW_MODEL_CALL_REPLAY_AMBIGUOUS")?
        };
        tx.commit()
            .await
            .map_err(|_| "LW_MODEL_CALL_ID_UNAVAILABLE")?;
        Ok(identity(&[scope[0], scope[1], scope[2], key.as_bytes()]))
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
    let expected_model = match state.authorize(&claims).await {
        Ok(model) => model,
        Err(code) => {
            return error_response(
                StatusCode::FORBIDDEN,
                code,
                "original session or model input authorization denied",
            );
        }
    };
    let input: Value = match contracts::parse_strict_json(&body) {
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
    if model != expected_model {
        return error_response(
            StatusCode::FORBIDDEN,
            "LW_MODEL_INPUT_AUTHORIZATION_DENIED",
            "model differs from the original authorized policy",
        );
    }
    if let Err(code) = state
        .authorization
        .classify(&input, token, &state.upstream_key)
        .await
    {
        return error_response(
            StatusCode::FORBIDDEN,
            code,
            "model input contains denied content",
        );
    }
    let request_hash = format!("{:x}", Sha256::digest(&body));
    let call_id = match state.call_identity(&claims, &headers, &request_hash).await {
        Ok(id) => id,
        Err(code) => {
            return error_response(
                StatusCode::CONFLICT,
                code,
                "stable model invocation identity required; ambiguous retries are not forwarded",
            );
        }
    };
    let accepted: Result<bool, _> = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM agent.idempotency_ledger WHERE operation='model_gateway_call' AND idempotency_key=$1)")
        .bind(call_id.to_string()).fetch_one(&state.pool).await;
    match accepted {
        Ok(false) => {}
        Ok(true) => {
            return error_response(
                StatusCode::CONFLICT,
                "LW_MODEL_CALL_OUTCOME_UNKNOWN",
                "original call is already fenced; inspect its persisted observation",
            );
        }
        Err(_) => {
            return error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "LW_MODEL_CALL_ID_UNAVAILABLE",
                "call execution fence unavailable",
            );
        }
    }
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
        let response = forward(
            state,
            headers,
            body,
            model,
            admission.hold.id,
            call_id,
            claims,
        )
        .await;
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
    claims: TokenClaims,
) -> Response {
    // Admission may race revocation, cancellation, or a changed egress policy. No request has
    // left the platform yet, so this path has a known zero observation, never unknown usage.
    let allowed = state.preflight(&claims, &headers, &body, &model).await;
    if let Err(code) = allowed {
        return state
            .refuse_before_send(hold_id, call_id, &model, code)
            .await;
    }
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
        let measurement = observation.measurement(status.is_success());
        if let Err(error) = settle(
            &state,
            hold_id,
            call_id,
            &model,
            measurement,
            b"terminal-observation",
        )
        .await
        {
            tracing::error!(event="agent.model.settlement_pending",error=%error,hold_id=%hold_id,"usage observation could not be reconciled; admission deadline remains authoritative");
        }
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
    input_tokens: Option<u64>,
    start_input: Option<u64>,
    output_tokens: Option<u64>,
    cache_read_tokens: Option<u64>,
    cache_write_tokens: Option<u64>,
    terminal: bool,
    final_output: Option<u64>,
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
                if let Some(data) = self.bytes[..end].strip_prefix(b"data:") {
                    match serde_json::from_slice::<Value>(data) {
                        Ok(value) => self.merge(&value),
                        Err(_) => self.invalid = true,
                    }
                }
                self.bytes.drain(..=end);
            }
        }
    }
    fn merge(&mut self, value: &Value) {
        let kind = value.get("type").and_then(Value::as_str);
        if kind == Some("error") {
            self.invalid = true;
        }
        if self.sse && matches!(kind, Some("message_start" | "message_delta")) {
            self.terminal = false;
        }
        if kind == Some("message_stop") {
            self.terminal = true;
        }
        let stop_reason = if self.sse {
            value
                .get("delta")
                .and_then(|delta| delta.get("stop_reason"))
        } else {
            value.get("stop_reason")
        };
        let finished = matches!(
            stop_reason.and_then(Value::as_str),
            Some(
                "end_turn" | "max_tokens" | "stop_sequence" | "tool_use" | "pause_turn" | "refusal"
            )
        );
        if !self.sse && kind == Some("message") && finished {
            self.terminal = true;
        }
        let usage = value.get("usage").or_else(|| {
            value
                .get("message")
                .and_then(|message| message.get("usage"))
        });
        if let Some(usage) = usage {
            if let Some(tokens) = usage.get("input_tokens").and_then(Value::as_u64) {
                self.input_tokens = Some(tokens);
                if (!self.sse && kind == Some("message"))
                    || (kind == Some("message_start")
                        && value
                            .get("message")
                            .and_then(|message| message.get("type"))
                            .and_then(Value::as_str)
                            == Some("message"))
                {
                    self.start_input = Some(tokens);
                }
            }
            if let Some(tokens) = usage.get("output_tokens").and_then(Value::as_u64) {
                self.output_tokens = Some(tokens);
                if !self.sse || (kind == Some("message_delta") && finished) {
                    self.final_output = Some(tokens);
                }
            }
            if let Some(tokens) = usage.get("cache_read_input_tokens").and_then(Value::as_u64) {
                self.cache_read_tokens = Some(tokens);
            }
            if let Some(tokens) = usage
                .get("cache_creation_input_tokens")
                .and_then(Value::as_u64)
            {
                self.cache_write_tokens = Some(tokens);
            }
        }
    }
    fn measurement(mut self, successful: bool) -> ModelUsageMeasurement {
        if !self.sse
            && let Ok(value) = serde_json::from_slice::<Value>(&self.bytes)
        {
            self.merge(&value);
        }
        let quantities = ModelTokenQuantities {
            input_tokens: self.input_tokens.unwrap_or_default(),
            output_tokens: self.output_tokens.unwrap_or_default(),
            cache_read_tokens: self.cache_read_tokens.unwrap_or_default(),
            cache_write_tokens: self.cache_write_tokens.unwrap_or_default(),
        };
        if successful
            && !self.invalid
            && self.terminal
            && self.final_output.is_some()
            && self.start_input.is_some()
            && self.output_tokens.is_some()
        {
            ModelUsageMeasurement::Known { quantities }
        } else {
            let observed = self.input_tokens.is_some()
                || self.output_tokens.is_some()
                || self.cache_read_tokens.is_some()
                || self.cache_write_tokens.is_some();
            ModelUsageMeasurement::Unknown {
                reason: "model response lacked trustworthy complete terminal usage (failed, interrupted or cancelled)".into(),
                observed_quantities: observed.then_some(quantities),
            }
        }
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
#[cfg(test)]
#[path = "../tests/support/build_billing.rs"]
mod build_billing;
#[cfg(test)]
#[path = "../tests/support/model_authorization.rs"]
mod model_authorization;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn capabilities_follow_execution_deadline_beyond_five_minutes()
    -> Result<(), Box<dyn std::error::Error>> {
        let issuer =
            ScopedTokenIssuer::new(vec![9; 32], Duration::from_hours(24)).ok_or("signer")?;
        let before = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
        let scope = ModelGatewayScope::Review {
            lease_sha256: contracts::http::model_input_parent_lease_sha256(Uuid::now_v7()),
        };
        let token = issuer.issue_for(
            ProjectId::new(),
            Uuid::now_v7(),
            1,
            scope.clone(),
            Duration::from_hours(1),
        );
        let claims = issuer.validate(&token).map_err(std::io::Error::other)?;
        assert!(claims.expires_at >= before + 3600);
        assert!(claims.expires_at <= before + 3601);
        assert!(issuer.validate_at(&token, before + 301).is_ok());
        assert!(issuer.validate_at(&token, claims.expires_at - 1).is_ok());
        assert!(matches!(
            issuer.validate_at(&token, claims.expires_at),
            Err("LW_MODEL_GATEWAY_TOKEN_EXPIRED")
        ));
        // Queue time is deducted by the worker before issuance, so only the remaining execution
        // budget is signed; retrying near the end does not extend the original authorization.
        let queued = issuer.issue_for(
            ProjectId::new(),
            Uuid::now_v7(),
            1,
            scope,
            Duration::from_mins(30),
        );
        let remaining = issuer.validate(&queued).map_err(std::io::Error::other)?;
        assert!(remaining.expires_at <= before + 1801);
        assert!(issuer.validate_at(&queued, before + 900).is_ok());
        Ok(())
    }
}

fn gateway_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .or_else(|| {
            headers
                .get("x-api-key")
                .and_then(|value| value.to_str().ok())
        })
}

async fn authorize_tool(
    State(state): State<Arc<GatewayState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(token) = gateway_token(&headers) else {
        return error_response(
            StatusCode::UNAUTHORIZED,
            "LW_MODEL_GATEWAY_TOKEN_MISSING",
            "gateway token required",
        );
    };
    let claims = match state.issuer.validate(token) {
        Ok(claims) => claims,
        Err(code) => {
            return error_response(StatusCode::UNAUTHORIZED, code, "gateway token rejected");
        }
    };
    match state.authorize(&claims).await {
        Ok(_) => {
            let input: Value = match contracts::parse_strict_json(&body) {
                Ok(input) => input,
                Err(_) => {
                    return error_response(
                        StatusCode::BAD_REQUEST,
                        "LW_MODEL_GATEWAY_REQUEST_INVALID",
                        "tool hook must supply its JSON payload",
                    );
                }
            };
            match state
                .authorization
                .classify(&input, token, &state.upstream_key)
                .await
            {
                Ok(()) => StatusCode::NO_CONTENT.into_response(),
                Err(code) => error_response(
                    StatusCode::FORBIDDEN,
                    code,
                    "tool input or result contains denied content",
                ),
            }
        }
        Err(code) => error_response(
            StatusCode::FORBIDDEN,
            code,
            "original session or model input authorization denied",
        ),
    }
}

#[cfg(test)]
#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "native session, admission, refusal and zero settlement are one financial scenario"
)]
async fn dlp_refusal_after_admission_releases_real_hold_without_charge()
-> Result<(), Box<dyn std::error::Error>> {
    use contracts::resource::{FixedDecimal, credits::CreditHoldState};
    use testcontainers::{ImageExt, runners::AsyncRunner};
    use testcontainers_modules::postgres::Postgres;
    let pg = Postgres::default().with_tag("17.5-alpine").start().await?;
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(12)
        .connect(&format!(
            "postgres://postgres:postgres@127.0.0.1:{}/postgres",
            pg.get_host_port_ipv4(5432).await?
        ))
        .await?;
    let billing = build_billing::BuildBillingFixture::start(pool.clone()).await?;
    model_authorization::apply(&pool, persistence_sqlx::Domain::Agent).await?;
    let authority =
        model_authorization::ModelAuthorizationFixture::start(pool.clone(), &billing).await?;
    billing
        .model_tariffs(authority.project, "fixture-model", true)
        .await?;
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = calls.clone();
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let upstream: Url = format!("http://{}/", listener.local_addr()?).parse()?;
    let server = tokio::spawn(async move {
        let _ = axum::serve(
            listener,
            Router::new().route(
                "/v1/messages",
                post(move || async move {
                    count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    axum::Json(json!({"type":"message","stop_reason":"end_turn"}))
                }),
            ),
        )
        .await;
    });
    let issuer = ScopedTokenIssuer::new(vec![9; 32], Duration::from_hours(24)).ok_or("signer")?;
    let token = issuer.issue_for(
        authority.project,
        authority.run_id.as_uuid(),
        authority.attempt,
        authority.scope.clone(),
        Duration::from_mins(2),
    );
    let claims = issuer.validate(&token).map_err(std::io::Error::other)?;
    let state = Arc::new(GatewayState {
        upstream,
        upstream_key: "provider-key".to_owned(),
        issuer,
        pool: pool.clone(),
        resource: billing.client.clone(),
        http: reqwest::Client::new(),
        authorization: authority.authorization.clone(),
    });
    let call_id = Uuid::now_v7();
    let admission = billing
        .client
        .admit_model_cost(
            &call_id.to_string(),
            &ModelCostAdmissionRequest {
                call_id,
                project_id: authority.project,
                model_binding: "fixture-model".to_owned(),
            },
        )
        .await?;
    assert!(billing.credits.account(authority.project).await?.reserved > FixedDecimal::zero());
    let mut headers = HeaderMap::new();
    headers.insert("x-api-key", token.parse()?);
    let body = serde_json::to_vec(
        &json!({"model":"fixture-model","messages":[{"role":"user","content":{"-----BEGIN PRIVATE KEY-----":"not-a-secret-value"}}]}),
    )?;
    // Exercise the actual final gateway preflight after a real admission, not a mock classifier.
    let response = forward(
        state,
        headers,
        Bytes::from(body),
        "fixture-model".to_owned(),
        admission.hold.id,
        call_id,
        claims,
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    let account = billing.credits.account(authority.project).await?;
    assert_eq!(account.reserved, FixedDecimal::zero());
    assert_eq!(account.spent, FixedDecimal::zero());
    let holds = billing
        .credits
        .holds(authority.project, 1, 100, 0)
        .await?
        .items;
    assert_eq!(holds.len(), 1);
    assert_eq!(holds[0].state, CreditHoldState::Settled);
    assert_eq!(holds[0].remaining, FixedDecimal::zero());
    server.abort();
    Ok(())
}
