//! Attempt-scoped Claude transport. Sandboxes never receive the upstream provider credential.
//! Every tool hook and every HTTP model request rechecks the original user binding and live lease.

use std::{collections::BTreeMap, sync::Arc};

use axum::{
    Router,
    body::{Body, Bytes},
    extract::{DefaultBodyLimit, Path, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::post,
};
use contracts::{
    TaskRunId,
    authoring::ProjectLlmEgressPolicy,
    http::{
        InternalAgentRunRequest, ModelInputAuthorizationBinding,
        ModelInputAuthorizationResolveRequest,
    },
};
use persistence_sqlx::Sha256Digest;
use serde::Deserialize;
use sqlx::Row;
use subtle::ConstantTimeEq;
use uuid::Uuid;

use crate::{
    claude_code::EgressClassifier, model_input_authorization::ModelInputAuthorizationClient,
    run_store::PostgresAgentRunStore,
};

#[derive(Clone)]
pub struct ModelInputProxy {
    store: PostgresAgentRunStore,
    authority: Option<ModelInputAuthorizationClient>,
    classifier: Arc<dyn EgressClassifier>,
    http: reqwest::Client,
    upstream: reqwest::Url,
    credential: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Dispatch {
    request: InternalAgentRunRequest,
    input_authorization: ModelInputAuthorizationBinding,
}

#[derive(Debug)]
struct Denied;
impl IntoResponse for Denied {
    fn into_response(self) -> Response {
        (StatusCode::FORBIDDEN, [(header::CACHE_CONTROL, "no-store")], axum::Json(serde_json::json!({"type":"urn:labweaver:problem:model-input-authorization-denied","title":"模型输入授权已失效","status":403,"diagnosticCode":"LW_MODEL_INPUT_AUTHORIZATION_DENIED","detail":"原会话、对象授权、任务期限或执行租约不再允许新的工具动作及模型发送。"}))).into_response()
    }
}

#[derive(Debug)]
enum ProxyError {
    Authorization(Denied),
    UpstreamUnavailable,
}
impl From<Denied> for ProxyError {
    fn from(value: Denied) -> Self {
        Self::Authorization(value)
    }
}
impl IntoResponse for ProxyError {
    fn into_response(self) -> Response {
        match self {
            Self::Authorization(denied) => denied.into_response(),
            Self::UpstreamUnavailable => (StatusCode::BAD_GATEWAY, [(header::CACHE_CONTROL, "no-store")], axum::Json(serde_json::json!({"type":"urn:labweaver:problem:model-provider-unavailable","title":"模型服务连接失败","status":502,"diagnosticCode":"LW_AGENT_RUNTIME_UNAVAILABLE","detail":"上游模型服务未提供可用响应。"}))).into_response(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("LW_MODEL_INPUT_PROXY_CONFIGURATION_INVALID")]
pub struct ModelInputProxyConfigurationError;

impl ModelInputProxy {
    pub fn new(
        store: PostgresAgentRunStore,
        authority: Option<ModelInputAuthorizationClient>,
        classifier: Arc<dyn EgressClassifier>,
        environment: &BTreeMap<String, String>,
        http: reqwest::Client,
    ) -> Result<Self, ModelInputProxyConfigurationError> {
        let upstream = environment
            .get("ANTHROPIC_BASE_URL")
            .and_then(|value| reqwest::Url::parse(value).ok())
            .filter(|url| {
                url.scheme() == "https"
                    && url.host_str().is_some()
                    && url.username().is_empty()
                    && url.password().is_none()
                    && url.query().is_none()
                    && url.fragment().is_none()
            })
            .ok_or(ModelInputProxyConfigurationError)?;
        let credential = environment
            .get("ANTHROPIC_AUTH_TOKEN")
            .filter(|value| !value.is_empty())
            .cloned()
            .ok_or(ModelInputProxyConfigurationError)?;
        Ok(Self {
            store,
            authority,
            classifier,
            http,
            upstream,
            credential,
        })
    }

    pub fn router(self) -> Router {
        Router::new()
            .route(
                "/internal/v1/model-input-attempts/{task_id}/authorize",
                post(authorize_tool),
            )
            .route(
                "/internal/v1/model-input-attempts/{task_id}/v1/messages",
                post(model_request),
            )
            .route(
                "/internal/v1/model-input-attempts/{task_id}/v1/messages/count_tokens",
                post(count_tokens),
            )
            .layer(DefaultBodyLimit::max(16 * 1024 * 1024))
            .with_state(Arc::new(self))
    }

    async fn authorize(
        &self,
        task_id: TaskRunId,
        headers: &HeaderMap,
    ) -> Result<ProjectLlmEgressPolicy, Denied> {
        let token = capability_token(headers)?;
        let row = sqlx::query("SELECT work.lease_token,dispatch.request,dispatch.policy,run.run_id,sandbox.binding FROM agent.authoring_sandbox_attempts sandbox JOIN agent.agent_track_work_items work ON work.run_id=sandbox.run_id AND work.track=sandbox.track AND work.attempt_number=sandbox.attempt_number JOIN agent.agent_runs run ON run.run_id=work.run_id JOIN agent.agent_run_dispatches dispatch ON dispatch.run_id=run.run_id WHERE sandbox.task_run_id=$1 AND sandbox.binding IS NOT NULL AND sandbox.state IN ('creating','submitted') AND work.state='running' AND work.lease_expires_at>clock_timestamp() AND run.cancellation_requested_at IS NULL AND work.attempt_started_at+((dispatch.policy->'budget'->>'timeoutMilliseconds')::bigint*interval '1 millisecond')>clock_timestamp()")
            .bind(task_id.as_uuid()).fetch_optional(self.store.pool()).await.map_err(|_| Denied)?.ok_or(Denied)?;
        let lease: Uuid = row.try_get("lease_token").map_err(|_| Denied)?;
        let expected = attempt_credential(task_id.as_uuid(), lease);
        if !bool::from(expected.as_bytes().ct_eq(token.as_bytes())) {
            return Err(Denied);
        }
        let dispatch: Dispatch =
            serde_json::from_value(row.try_get("request").map_err(|_| Denied)?)
                .map_err(|_| Denied)?;
        let sandbox: contracts::execution::TaskExecutionBinding =
            serde_json::from_value(row.try_get("binding").map_err(|_| Denied)?)
                .map_err(|_| Denied)?;
        sandbox.validate().map_err(|_| Denied)?;
        if sandbox.task_run_id != task_id
            || sandbox.project_id != dispatch.input_authorization.project_id
        {
            return Err(Denied);
        }
        let policy: ProjectLlmEgressPolicy =
            serde_json::from_value(row.try_get("policy").map_err(|_| Denied)?)
                .map_err(|_| Denied)?;
        let selected_paths = match dispatch.request {
            InternalAgentRunRequest::Authoring(request) => request.selected_paths,
            InternalAgentRunRequest::WorkConfiguration(request) => request.selected_paths,
        }
        .ok_or(Denied)?;
        self.authority
            .as_ref()
            .ok_or(Denied)?
            .resolve(&ModelInputAuthorizationResolveRequest {
                binding: dispatch.input_authorization,
                selected_paths,
            })
            .await
            .map_err(|_| Denied)?;
        // Recheck the exact lease after the remote authorization read, without holding a DB lock
        // across the RPC. Cancel/lease rollover during that call cannot release another send.
        let run_id: Uuid = row.try_get("run_id").map_err(|_| Denied)?;
        let current: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM agent.authoring_sandbox_attempts sandbox JOIN agent.agent_track_work_items work ON work.run_id=sandbox.run_id AND work.track=sandbox.track AND work.attempt_number=sandbox.attempt_number JOIN agent.agent_runs run ON run.run_id=work.run_id JOIN agent.agent_run_dispatches dispatch ON dispatch.run_id=run.run_id WHERE sandbox.task_run_id=$1 AND run.run_id=$2 AND work.lease_token=$3 AND work.lease_expires_at>clock_timestamp() AND work.state='running' AND run.cancellation_requested_at IS NULL AND sandbox.state IN ('creating','submitted') AND work.attempt_started_at+((dispatch.policy->'budget'->>'timeoutMilliseconds')::bigint*interval '1 millisecond')>clock_timestamp())")
            .bind(task_id.as_uuid()).bind(run_id).bind(lease).fetch_one(self.store.pool()).await.map_err(|_| Denied)?;
        if !current {
            return Err(Denied);
        }
        Ok(policy)
    }

    async fn forward(
        &self,
        task_id: TaskRunId,
        headers: HeaderMap,
        body: Bytes,
        operation: &str,
    ) -> Result<Response, ProxyError> {
        let policy = self.authorize(task_id, &headers).await?;
        let document: serde_json::Value =
            contracts::parse_strict_json(&body).map_err(|_| Denied)?;
        if document.get("model").and_then(serde_json::Value::as_str)
            != Some(policy.binding.model.as_str())
            || self.credential.is_empty()
        {
            return Err(Denied.into());
        }
        // Classify the actual messages/tool results, not only the initial materials. Serialized
        // JSON alone would hide newlines/escapes from the existing secret classifier.
        self.classify_strings(&document, capability_token(&headers)?)
            .await?;
        let mut url = self.upstream.clone();
        url.set_path(&format!(
            "{}/v1/{operation}",
            self.upstream.path().trim_end_matches('/')
        ));
        let mut request = self
            .http
            .post(url)
            .header(header::CONTENT_TYPE, "application/json")
            .bearer_auth(&self.credential)
            .body(body);
        for name in ["anthropic-version", "anthropic-beta"] {
            if let Some(value) = headers.get(name) {
                request = request.header(name, value);
            }
        }
        // This is the last authorization point before any user/tool content leaves the platform.
        self.authorize(task_id, &headers).await?;
        let upstream = request
            .send()
            .await
            .map_err(|_| ProxyError::UpstreamUnavailable)?;
        let status = upstream.status();
        if status.is_redirection() {
            return Err(ProxyError::UpstreamUnavailable);
        }
        let content_type = upstream.headers().get(header::CONTENT_TYPE).cloned();
        let mut response = Response::new(Body::from_stream(upstream.bytes_stream()));
        *response.status_mut() = status;
        response.headers_mut().insert(
            header::CACHE_CONTROL,
            header::HeaderValue::from_static("no-store"),
        );
        if let Some(value) = content_type {
            response.headers_mut().insert(header::CONTENT_TYPE, value);
        }
        Ok(response)
    }

    async fn classify_strings(
        &self,
        value: &serde_json::Value,
        capability: &str,
    ) -> Result<(), Denied> {
        let mut pending = vec![value];
        while let Some(value) = pending.pop() {
            match value {
                serde_json::Value::String(text) => {
                    if !self
                        .classifier
                        .classify("model/tool-result", text.as_bytes())
                        .await
                        .map_err(|_| Denied)?
                        .is_empty()
                        || text.contains(&self.credential)
                        || text.contains(capability)
                    {
                        return Err(Denied);
                    }
                }
                serde_json::Value::Array(values) => pending.extend(values),
                serde_json::Value::Object(values) => {
                    if values
                        .keys()
                        .any(|key| key.contains(capability) || key.contains(&self.credential))
                    {
                        return Err(Denied);
                    }
                    pending.extend(values.values());
                }
                _ => {}
            }
        }
        Ok(())
    }
}

/// Domain-separated, task-local bearer capability; never reveals the parent lease token.
#[must_use]
pub fn attempt_credential(task_id: Uuid, lease: Uuid) -> String {
    Sha256Digest::of_bytes(format!("labweaver-model-input-attempt/v1:{lease}:{task_id}").as_bytes())
        .to_string()
}

async fn authorize_tool(
    State(proxy): State<Arc<ModelInputProxy>>,
    Path(task_id): Path<TaskRunId>,
    headers: HeaderMap,
) -> Result<StatusCode, Denied> {
    proxy.authorize(task_id, &headers).await?;
    Ok(StatusCode::NO_CONTENT)
}
async fn model_request(
    State(proxy): State<Arc<ModelInputProxy>>,
    Path(task_id): Path<TaskRunId>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ProxyError> {
    proxy.forward(task_id, headers, body, "messages").await
}
async fn count_tokens(
    State(proxy): State<Arc<ModelInputProxy>>,
    Path(task_id): Path<TaskRunId>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ProxyError> {
    proxy
        .forward(task_id, headers, body, "messages/count_tokens")
        .await
}

fn capability_token(headers: &HeaderMap) -> Result<&str, Denied> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .or_else(|| {
            headers
                .get("x-api-key")
                .and_then(|value| value.to_str().ok())
        })
        .filter(|value| value.len() == 64)
        .ok_or(Denied)
}
