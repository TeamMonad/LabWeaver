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
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ZeroSettlementReconciliation {
    pub succeeded: u64,
    pub failed: u64,
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

    /// Replays only durable, confirmed no-send observations; never infers zero from an unknown call.
    pub async fn reconcile_pending_zero_settlements(
        &self,
    ) -> Result<ZeroSettlementReconciliation, GatewayReconciliationError> {
        self.reconcile_zero_batch(100).await
    }

    async fn reconcile_zero_batch(
        &self,
        limit: u32,
    ) -> Result<ZeroSettlementReconciliation, GatewayReconciliationError> {
        let keys: Vec<String> = sqlx::query_scalar("SELECT idempotency_key FROM agent.idempotency_ledger WHERE operation='model_gateway_call' AND state='in_progress' AND result ? 'knownZero' AND COALESCE((result#>>'{zeroRetry,nextAttemptAt}')::timestamptz,'-infinity'::timestamptz)<=clock_timestamp() ORDER BY (result#>>'{zeroRetry,lastAttemptAt}')::timestamptz NULLS FIRST,created_at,idempotency_key LIMIT $1")
            .bind(i64::from(limit)).fetch_all(&self.pool).await?;
        let mut outcome = ZeroSettlementReconciliation::default();
        for key in keys {
            let result = match Uuid::parse_str(&key) {
                Ok(call_id) => self.reconcile_zero_call(call_id).await,
                Err(_) => Err(GatewayReconciliationError::InvalidObservation),
            };
            match result {
                Ok(true) => outcome.succeeded += 1,
                Ok(false) => {}
                Err(error) => {
                    outcome.failed += 1;
                    tracing::warn!(event="agent.model.zero_settlement_retry",call_id=key.as_str(),error=%error,"confirmed zero observation retained; continuing reconciliation batch");
                    if let Err(error) = self.defer_zero_retry(&key, &error).await {
                        tracing::warn!(event="agent.model.zero_retry_schedule_failed",call_id=key.as_str(),error=%error,"pending zero observation retained; continuing reconciliation batch");
                    }
                }
            }
        }
        Ok(outcome)
    }

    async fn defer_zero_retry(
        &self,
        key: &str,
        error: &GatewayReconciliationError,
    ) -> Result<(), sqlx::Error> {
        // Metadata lives beside, never replaces, the immutable knownZero payload. Backoff is
        // 10/20/40/80/160/300 seconds and capped; unattempted work sorts ahead of retries.
        sqlx::query("UPDATE agent.idempotency_ledger SET result=result||jsonb_build_object('zeroRetry',jsonb_build_object('attempts',LEAST(COALESCE((result#>>'{zeroRetry,attempts}')::integer,0),15)+1,'lastAttemptAt',clock_timestamp(),'nextAttemptAt',clock_timestamp()+LEAST(10.0*power(2.0,LEAST(COALESCE((result#>>'{zeroRetry,attempts}')::integer,0),5)),300.0)*interval '1 second','lastError',$2::text)) WHERE operation='model_gateway_call' AND idempotency_key=$1 AND state='in_progress' AND result ? 'knownZero'")
            .bind(key).bind(error.to_string()).execute(&self.pool).await?;
        Ok(())
    }

    async fn reconcile_zero_call(&self, call_id: Uuid) -> Result<bool, GatewayReconciliationError> {
        use sqlx::Row as _;
        let row = sqlx::query("SELECT state,result FROM agent.idempotency_ledger WHERE operation='model_gateway_call' AND idempotency_key=$1")
            .bind(call_id.to_string()).fetch_optional(&self.pool).await?;
        let Some(row) = row else { return Ok(false) };
        let Some(result) = row.try_get::<Option<Value>, _>("result")? else {
            return Ok(false);
        };
        let Some(observation) = result.get("knownZero") else {
            return Ok(false);
        };
        let request: ModelCostSettlementRequest = serde_json::from_value(observation.clone())
            .map_err(|_| GatewayReconciliationError::InvalidObservation)?;
        if request.call_id != call_id
            || !matches!(&request.measurement, ModelUsageMeasurement::Known {quantities} if quantities == &ModelTokenQuantities::default())
            || request.event_id != zero_settlement_event(call_id)?
        {
            return Err(GatewayReconciliationError::InvalidObservation);
        }
        if row.try_get::<String, _>("state")? == "completed" {
            return Ok(true);
        }
        self.resource
            .settle_model_cost(request.hold_id, &request)
            .await?;
        // A lost Resource response or a failed Agent update replays the same stable eventId.
        sqlx::query("UPDATE agent.idempotency_ledger SET state='completed',completed_at=clock_timestamp() WHERE operation='model_gateway_call' AND idempotency_key=$1 AND state='in_progress' AND result->'knownZero'=$2")
            .bind(call_id.to_string()).bind(observation).execute(&self.pool).await?;
        Ok(true)
    }

    async fn refuse_before_send(
        &self,
        hold_id: Uuid,
        call_id: Uuid,
        model: &str,
        code: &'static str,
    ) -> Response {
        let Ok(event_id) = zero_settlement_event(call_id) else {
            return error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "LW_MODEL_SETTLEMENT_PENDING",
                "no model request sent; zero observation could not be prepared",
            );
        };
        let observation = json!({"knownZero":ModelCostSettlementRequest {
            hold_id, call_id, event_id, model_binding:model.to_owned(),
            measurement:ModelUsageMeasurement::Known {quantities:ModelTokenQuantities::default()},
        }});
        let recorded = sqlx::query("UPDATE agent.idempotency_ledger SET result=COALESCE(result,'{}'::jsonb)||$2 WHERE operation='model_gateway_call' AND idempotency_key=$1 AND state='in_progress' AND (result IS NULL OR result->'knownZero'=$2->'knownZero')")
            .bind(call_id.to_string()).bind(observation).execute(&self.pool).await;
        if !matches!(recorded, Ok(result) if result.rows_affected()==1) {
            return error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "LW_MODEL_SETTLEMENT_PENDING",
                "no model request sent; zero observation persistence failed",
            );
        }
        if self.reconcile_zero_call(call_id).await.is_err() {
            return error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "LW_MODEL_SETTLEMENT_PENDING",
                "no model request sent; durable zero observation awaits reconciliation",
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
    let server = async { axum::serve(listener, state.clone().router()).await };
    let reconciliation = async {
        let mut interval = tokio::time::interval(Duration::from_secs(10));
        loop {
            interval.tick().await;
            if let Err(error) = state.reconcile_pending_zero_settlements().await {
                tracing::warn!(event="agent.model.zero_settlement_pending",diagnostic_code=%error,"durable zero observation retained for reconciliation");
            }
        }
    };
    tokio::select! {
        result = server => result,
        () = reconciliation => Err(std::io::Error::other("model settlement reconciliation ended")),
    }
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
            return match state.reconcile_zero_call(call_id).await {
                Ok(true) => error_response(
                    StatusCode::FORBIDDEN,
                    "LW_MODEL_INPUT_AUTHORIZATION_DENIED",
                    "original call was denied before upstream; its zero observation is settled",
                ),
                Ok(false) => error_response(
                    StatusCode::CONFLICT,
                    "LW_MODEL_CALL_OUTCOME_UNKNOWN",
                    "original call is already fenced; inspect its persisted observation",
                ),
                Err(_) => error_response(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "LW_MODEL_SETTLEMENT_PENDING",
                    "original zero observation awaits reconciliation; no new upstream request sent",
                ),
            };
        }
        Err(_) => {
            return error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "LW_MODEL_CALL_ID_UNAVAILABLE",
                "call execution fence unavailable",
            );
        }
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
async fn failing_project_does_not_block_other_zero_settlements()
-> Result<(), Box<dyn std::error::Error>> {
    exercise_zero_reconciliation_progress(1, 100).await
}

#[cfg(test)]
#[tokio::test]
async fn failed_zero_settlements_cannot_fill_every_batch() -> Result<(), Box<dyn std::error::Error>>
{
    exercise_zero_reconciliation_progress(3, 2).await
}

#[cfg(test)]
#[allow(
    clippy::too_many_lines,
    reason = "native cross-project accounting, permanent PG fault and bounded retry form one scenario"
)]
async fn exercise_zero_reconciliation_progress(
    failed_records: u32,
    batch_size: u32,
) -> Result<(), Box<dyn std::error::Error>> {
    use contracts::resource::FixedDecimal;
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
        .model_tariffs(authority.project, "failing-model", true)
        .await?;
    let healthy_project = contracts::ProjectId::new();
    billing
        .model_tariffs(healthy_project, "healthy-model", true)
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
    let state = GatewayState {
        upstream,
        upstream_key: "provider-key".to_owned(),
        issuer: ScopedTokenIssuer::new(vec![9; 32], Duration::from_hours(24)).ok_or("signer")?,
        pool: pool.clone(),
        resource: billing.client.clone(),
        http: reqwest::Client::new(),
        authorization: authority.authorization.clone(),
    };
    for index in 0..=failed_records {
        let (project, model) = if index == failed_records {
            (healthy_project, "healthy-model")
        } else {
            (authority.project, "failing-model")
        };
        let call_id = Uuid::now_v7();
        let admission = billing
            .client
            .admit_model_cost(
                &call_id.to_string(),
                &ModelCostAdmissionRequest {
                    call_id,
                    project_id: project,
                    model_binding: model.to_owned(),
                },
            )
            .await?;
        let observation = json!({"knownZero":ModelCostSettlementRequest {hold_id:admission.hold.id,call_id,event_id:zero_settlement_event(call_id)?,model_binding:model.to_owned(),measurement:ModelUsageMeasurement::Known {quantities:ModelTokenQuantities::default()}}});
        // Seed only durable no-send facts, with real holds created through the Resource TLS API.
        sqlx::query("INSERT INTO agent.idempotency_ledger(operation,idempotency_key,request_sha256,state,result) VALUES('model_gateway_call',$1,$2,'in_progress',$3)")
            .bind(call_id.to_string()).bind("f".repeat(64)).bind(observation).execute(&pool).await?;
    }
    let fault = format!(
        "CREATE FUNCTION resource.test_project_zero_failure() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF EXISTS(SELECT 1 FROM resource.credit_holds WHERE hold_id=NEW.hold_id AND project_id='{}'::uuid) THEN RAISE EXCEPTION 'isolated persistent project zero-settlement failure'; END IF; RETURN NEW; END; $$; CREATE TRIGGER test_project_zero_failure BEFORE INSERT ON resource.model_cost_observations FOR EACH ROW WHEN (NEW.contract->'measurement'->>'state'='known') EXECUTE FUNCTION resource.test_project_zero_failure();",
        authority.project
    );
    sqlx::raw_sql(sqlx::AssertSqlSafe(&*fault))
        .execute(&pool)
        .await?;
    let mut succeeded = 0;
    let mut failed = 0;
    for _ in 0..=failed_records / batch_size {
        let outcome = state.reconcile_zero_batch(batch_size).await?;
        succeeded += outcome.succeeded;
        failed += outcome.failed;
    }
    assert_eq!(succeeded, 1);
    assert_eq!(failed, u64::from(failed_records));
    let healthy = billing.credits.account(healthy_project).await?;
    assert_eq!(healthy.reserved, FixedDecimal::zero());
    assert_eq!(healthy.spent, FixedDecimal::zero());
    assert!(billing.credits.account(authority.project).await?.reserved > FixedDecimal::zero());
    assert_eq!(
        billing.credits.account(authority.project).await?.spent,
        FixedDecimal::zero()
    );
    let pending: i64 = sqlx::query_scalar("SELECT count(*) FROM agent.idempotency_ledger WHERE operation='model_gateway_call' AND state='in_progress' AND result ? 'knownZero' AND result#>>'{zeroRetry,attempts}'='1' AND result#>>'{zeroRetry,lastError}'='LW_MODEL_ZERO_SETTLEMENT_UNAVAILABLE' AND (result#>>'{zeroRetry,nextAttemptAt}')::timestamptz-(result#>>'{zeroRetry,lastAttemptAt}')::timestamptz BETWEEN interval '10 seconds' AND interval '11 seconds'").fetch_one(&pool).await?;
    assert_eq!(pending, i64::from(failed_records));
    // A different gateway also honors persisted backoff. Advance only the scheduling metadata
    // to exercise the cap without sleeps; the failed project's native PG fault remains active.
    sqlx::query("UPDATE agent.idempotency_ledger SET result=jsonb_set(jsonb_set(result,'{zeroRetry,attempts}','15'::jsonb),'{zeroRetry,nextAttemptAt}',to_jsonb(clock_timestamp()-interval '1 second')) WHERE operation='model_gateway_call' AND state='in_progress' AND result ? 'knownZero'").execute(&pool).await?;
    let restarted = state.clone();
    assert_eq!(
        restarted.reconcile_pending_zero_settlements().await?,
        ZeroSettlementReconciliation {
            succeeded: 0,
            failed: u64::from(failed_records)
        }
    );
    let capped: i64 = sqlx::query_scalar("SELECT count(*) FROM agent.idempotency_ledger WHERE operation='model_gateway_call' AND state='in_progress' AND result ? 'knownZero' AND result#>>'{zeroRetry,attempts}'='16' AND (result#>>'{zeroRetry,nextAttemptAt}')::timestamptz-(result#>>'{zeroRetry,lastAttemptAt}')::timestamptz BETWEEN interval '300 seconds' AND interval '301 seconds'").fetch_one(&pool).await?;
    assert_eq!(capped, i64::from(failed_records));
    assert_eq!(
        state.reconcile_pending_zero_settlements().await?,
        ZeroSettlementReconciliation::default()
    );
    // Once the project fault clears, each original event completes exactly once; retry metadata
    // neither changes knownZero nor prevents the Agent fence from completing.
    sqlx::raw_sql("DROP TRIGGER test_project_zero_failure ON resource.model_cost_observations; DROP FUNCTION resource.test_project_zero_failure();").execute(&pool).await?;
    sqlx::query("UPDATE agent.idempotency_ledger SET result=jsonb_set(result,'{zeroRetry,nextAttemptAt}',to_jsonb(clock_timestamp()-interval '1 second')) WHERE operation='model_gateway_call' AND state='in_progress' AND result ? 'knownZero'").execute(&pool).await?;
    assert_eq!(
        restarted.reconcile_pending_zero_settlements().await?,
        ZeroSettlementReconciliation {
            succeeded: u64::from(failed_records),
            failed: 0
        }
    );
    assert_eq!(
        state.reconcile_pending_zero_settlements().await?,
        ZeroSettlementReconciliation::default()
    );
    let completed: i64 = sqlx::query_scalar("SELECT count(*) FROM agent.idempotency_ledger WHERE operation='model_gateway_call' AND state='completed' AND result ? 'knownZero'").fetch_one(&pool).await?;
    assert_eq!(completed, i64::from(failed_records) + 1);
    let observations: i64 = sqlx::query_scalar("SELECT count(*) FROM resource.model_cost_observations WHERE contract->'measurement'->>'state'='known'").fetch_one(&pool).await?;
    assert_eq!(observations, i64::from(failed_records) + 1);
    assert_eq!(
        billing.credits.account(authority.project).await?.reserved,
        FixedDecimal::zero()
    );
    assert_eq!(
        billing.credits.account(authority.project).await?.spent,
        FixedDecimal::zero()
    );
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    server.abort();
    Ok(())
}

#[cfg(test)]
#[tokio::test]
async fn dlp_refusal_after_admission_releases_real_hold_without_charge()
-> Result<(), Box<dyn std::error::Error>> {
    exercise_zero_settlement(false).await
}

#[cfg(test)]
#[tokio::test]
async fn failed_zero_settlement_is_durable_and_reconciles_idempotently()
-> Result<(), Box<dyn std::error::Error>> {
    exercise_zero_settlement(true).await
}

#[cfg(test)]
#[allow(
    clippy::too_many_lines,
    reason = "native session, admission, refusal and zero settlement are one financial scenario"
)]
async fn exercise_zero_settlement(fail_first: bool) -> Result<(), Box<dyn std::error::Error>> {
    use contracts::resource::{FixedDecimal, credits::CreditHoldState};
    use testcontainers::{ImageExt, runners::AsyncRunner};
    use testcontainers_modules::postgres::Postgres;
    use tower::ServiceExt as _;
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
    let call_key = "confirmed-zero-reconcile";
    let call_id = identity(&[
        authority.project.as_uuid().as_bytes(),
        authority.run_id.as_uuid().as_bytes(),
        &authority.attempt.to_be_bytes(),
        call_key.as_bytes(),
    ]);
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
    let body = Bytes::from(serde_json::to_vec(
        &json!({"model":"fixture-model","messages":[{"role":"user","content":{"-----BEGIN PRIVATE KEY-----":"not-a-secret-value"}}]}),
    )?);
    let request_hash = format!("{:x}", Sha256::digest(&body));
    sqlx::query("INSERT INTO agent.idempotency_ledger(operation,idempotency_key,request_sha256,state) VALUES('model_gateway_call',$1,$2,'in_progress')")
        .bind(call_id.to_string()).bind(request_hash).execute(&pool).await?;
    if fail_first {
        // Fail the actual Resource transaction, not a mock or a replacement backend.
        sqlx::raw_sql("CREATE FUNCTION resource.test_zero_settlement_failure() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'isolated Resource zero-settlement failure'; END $$; CREATE TRIGGER test_zero_settlement_failure BEFORE INSERT ON resource.model_cost_observations FOR EACH ROW WHEN (NEW.contract->'measurement'->>'state'='known') EXECUTE FUNCTION resource.test_zero_settlement_failure();").execute(&pool).await?;
    }
    // Exercise the actual final gateway preflight after a real admission, not a mock classifier.
    let replay_body = body.clone();
    let response = forward(
        state.clone(),
        headers,
        body,
        "fixture-model".to_owned(),
        admission.hold.id,
        call_id,
        claims,
    )
    .await;
    assert_eq!(
        response.status(),
        if fail_first {
            StatusCode::SERVICE_UNAVAILABLE
        } else {
            StatusCode::FORBIDDEN
        }
    );
    let durable: Value = sqlx::query_scalar("SELECT result FROM agent.idempotency_ledger WHERE operation='model_gateway_call' AND idempotency_key=$1").bind(call_id.to_string()).fetch_one(&pool).await?;
    let stable_event = serde_json::to_value(zero_settlement_event(call_id)?)?;
    assert_eq!(durable["knownZero"]["eventId"], stable_event);
    assert_eq!(durable["knownZero"]["measurement"]["state"], "known");
    assert_eq!(
        durable["knownZero"]["measurement"]["quantities"],
        serde_json::to_value(ModelTokenQuantities::default())?
    );
    if fail_first {
        assert!(billing.credits.account(authority.project).await?.reserved > FixedDecimal::zero());
        assert_eq!(
            billing.credits.account(authority.project).await?.spent,
            FixedDecimal::zero()
        );
        let pending: String = sqlx::query_scalar("SELECT state FROM agent.idempotency_ledger WHERE operation='model_gateway_call' AND idempotency_key=$1").bind(call_id.to_string()).fetch_one(&pool).await?;
        assert_eq!(pending, "in_progress");
        sqlx::raw_sql("DROP TRIGGER test_zero_settlement_failure ON resource.model_cost_observations; DROP FUNCTION resource.test_zero_settlement_failure();").execute(&pool).await?;
        sqlx::query(
            "UPDATE access.bff_sessions SET revoked_at=clock_timestamp() WHERE session_id=$1",
        )
        .bind(authority.session)
        .execute(&pool)
        .await?;
        // A new gateway instance replays the persisted observation without a live user session.
        let restarted = state.as_ref().clone();
        assert_eq!(
            restarted.reconcile_pending_zero_settlements().await?,
            ZeroSettlementReconciliation {
                succeeded: 1,
                failed: 0
            }
        );
        assert_eq!(
            restarted.reconcile_pending_zero_settlements().await?,
            ZeroSettlementReconciliation::default()
        );
        sqlx::query("UPDATE access.bff_sessions SET revoked_at=NULL WHERE session_id=$1")
            .bind(authority.session)
            .execute(&pool)
            .await?;
    }
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
    let completed: String = sqlx::query_scalar("SELECT state FROM agent.idempotency_ledger WHERE operation='model_gateway_call' AND idempotency_key=$1").bind(call_id.to_string()).fetch_one(&pool).await?;
    assert_eq!(completed, "completed");
    if fail_first {
        // Simulate losing the Agent completion update after Resource committed the zero event.
        sqlx::query("UPDATE agent.idempotency_ledger SET state='in_progress',completed_at=NULL WHERE operation='model_gateway_call' AND idempotency_key=$1")
            .bind(call_id.to_string()).execute(&pool).await?;
    }
    let duplicate = state
        .as_ref()
        .clone()
        .router()
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("x-api-key", &token)
                .header("x-labweaver-call-id", call_key)
                .body(Body::from(replay_body))?,
        )
        .await?;
    assert_eq!(duplicate.status(), StatusCode::FORBIDDEN);
    let complete_after_retry: String = sqlx::query_scalar("SELECT state FROM agent.idempotency_ledger WHERE operation='model_gateway_call' AND idempotency_key=$1").bind(call_id.to_string()).fetch_one(&pool).await?;
    assert_eq!(complete_after_retry, "completed");
    let observations: i64 = sqlx::query_scalar("SELECT count(*) FROM resource.model_cost_observations WHERE hold_id=$1 AND contract->'measurement'->>'state'='known'").bind(admission.hold.id).fetch_one(&pool).await?;
    assert_eq!(observations, 1);
    assert_eq!(
        billing.credits.account(authority.project).await?.reserved,
        FixedDecimal::zero()
    );
    assert_eq!(
        billing.credits.account(authority.project).await?.spent,
        FixedDecimal::zero()
    );
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    server.abort();
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum GatewayReconciliationError {
    #[error("LW_MODEL_ZERO_OBSERVATION_PERSISTENCE_FAILED")]
    Persistence(#[from] sqlx::Error),
    #[error("LW_MODEL_ZERO_OBSERVATION_INVALID")]
    InvalidObservation,
    #[error("LW_MODEL_ZERO_SETTLEMENT_UNAVAILABLE")]
    Resource(#[from] task_execution::resource::ResourceClientError),
}

fn zero_settlement_event(call_id: Uuid) -> Result<contracts::EventId, GatewayReconciliationError> {
    contracts::EventId::from_str(
        &identity(&[call_id.as_bytes(), b"denied-before-send"]).to_string(),
    )
    .map_err(|_| GatewayReconciliationError::InvalidObservation)
}
