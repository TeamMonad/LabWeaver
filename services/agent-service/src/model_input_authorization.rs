//! Concrete Control HTTP recheck and existing Agent cancellation facts.
use crate::claude_code::{ClaudeCodeRuntimeError, ModelInputAuthorization};
use crate::run_store::PostgresAgentRunStore;
use async_trait::async_trait;
use contracts::http::{ModelInputAuthorizationResolution, ModelInputAuthorizationResolveRequest};
use futures_util::StreamExt;
use sqlx::Row as _;
use std::collections::BTreeSet;
use std::io::Read;
use std::sync::Arc;

#[derive(Debug, thiserror::Error)]
#[error("invalid model input Control connection configuration")]
pub struct ModelInputAuthorizationConfigurationError;

#[derive(Clone)]
pub struct ModelInputAuthorizationClient {
    http: reqwest::Client,
    endpoint: reqwest::Url,
    audience: String,
    token: Arc<auth::ServiceTokenClient>,
}
impl ModelInputAuthorizationClient {
    /// Missing wiring disables only new model dispatches; partial/invalid wiring fails startup.
    pub fn from_env(
        token: Arc<auth::ServiceTokenClient>,
    ) -> Result<Option<Self>, ModelInputAuthorizationConfigurationError> {
        Self::from_env_inner(token).map_err(|()| ModelInputAuthorizationConfigurationError)
    }

    /// Constructs the same bounded HTTPS connection without mutating process environment.
    pub fn from_configuration(
        base_uri: String,
        ca_file: &std::path::Path,
        audience: String,
        timeout_milliseconds: u64,
        token: Arc<auth::ServiceTokenClient>,
    ) -> Result<Self, ModelInputAuthorizationConfigurationError> {
        let values = [
            Some(base_uri),
            Some(ca_file.to_string_lossy().into_owned()),
            Some(audience),
            Some(timeout_milliseconds.to_string()),
        ];
        let (http, endpoint, audience) = Self::read_configuration(values)
            .map_err(|()| ModelInputAuthorizationConfigurationError)?
            .ok_or(ModelInputAuthorizationConfigurationError)?;
        Ok(Self {
            http,
            endpoint,
            audience,
            token,
        })
    }

    fn from_env_inner(token: Arc<auth::ServiceTokenClient>) -> Result<Option<Self>, ()> {
        let names = [
            "LABWEAVER_CONTROL_SERVICE_BASE_URI",
            "LABWEAVER_CONTROL_SERVICE_CA_PATH",
            "LABWEAVER_CONTROL_SERVICE_AUDIENCE",
            "LABWEAVER_CONTROL_SERVICE_TIMEOUT_MILLISECONDS",
        ];
        let values = names.map(|name| std::env::var(name).ok());
        Ok(
            Self::read_configuration(values)?.map(|(http, endpoint, audience)| Self {
                http,
                endpoint,
                audience,
                token,
            }),
        )
    }

    fn read_configuration(
        values: [Option<String>; 4],
    ) -> Result<Option<(reqwest::Client, reqwest::Url, String)>, ()> {
        if values.iter().all(Option::is_none) {
            return Ok(None);
        }
        let [Some(base), Some(ca), Some(audience), Some(timeout)] = values else {
            return Err(());
        };
        if audience.trim().is_empty() {
            return Err(());
        }
        let base = reqwest::Url::parse(&base).map_err(|_| ())?;
        if base.scheme() != "https"
            || base.host_str().is_none()
            || !matches!(base.path(), "" | "/")
            || !base.username().is_empty()
            || base.password().is_some()
            || base.query().is_some()
            || base.fragment().is_some()
        {
            return Err(());
        }
        let endpoint = base
            .join("/internal/v1/model-input-authorizations/resolve")
            .map_err(|_| ())?;
        let timeout: u64 = timeout.parse().map_err(|_| ())?;
        if timeout == 0 || timeout > 5_000 {
            return Err(());
        }
        let path = std::path::Path::new(&ca);
        let metadata = std::fs::metadata(path).map_err(|_| ())?;
        if !path.is_absolute()
            || !metadata.is_file()
            || metadata.len() == 0
            || metadata.len() > 65_536
        {
            return Err(());
        }
        let mut ca_bytes = Vec::new();
        std::fs::File::open(path)
            .map_err(|_| ())?
            .take(65_537)
            .read_to_end(&mut ca_bytes)
            .map_err(|_| ())?;
        if ca_bytes.len() > 65_536 {
            return Err(());
        }
        let certificate = reqwest::Certificate::from_pem(&ca_bytes).map_err(|_| ())?;
        let http = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .https_only(true)
            .tls_built_in_root_certs(false)
            .add_root_certificate(certificate)
            .timeout(std::time::Duration::from_millis(timeout))
            .build()
            .map_err(|_| ())?;
        Ok(Some((http, endpoint, audience)))
    }
    pub(crate) async fn resolve(
        &self,
        request: &ModelInputAuthorizationResolveRequest,
    ) -> Result<(), ClaudeCodeRuntimeError> {
        let mut headers = reqwest::header::HeaderMap::new();
        self.token
            .bearer_auth_for(
                &mut headers,
                &self.audience,
                &BTreeSet::from(["control.agent_run.read".to_owned()]),
            )
            .await
            .map_err(|_| ClaudeCodeRuntimeError::InputAuthorizationDenied)?;
        let response = self
            .http
            .post(self.endpoint.clone())
            .headers(headers)
            .json(request)
            .send()
            .await
            .map_err(|_| ClaudeCodeRuntimeError::InputAuthorizationDenied)?;
        if response.status() != reqwest::StatusCode::OK
            || response.content_length().is_some_and(|size| size > 65_536)
        {
            return Err(ClaudeCodeRuntimeError::InputAuthorizationDenied);
        }
        let mut stream = response.bytes_stream();
        let mut bytes = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| ClaudeCodeRuntimeError::InputAuthorizationDenied)?;
            if bytes.len().saturating_add(chunk.len()) > 65_536 {
                return Err(ClaudeCodeRuntimeError::InputAuthorizationDenied);
            }
            bytes.extend_from_slice(&chunk);
        }
        let resolution: ModelInputAuthorizationResolution = serde_json::from_slice(&bytes)
            .map_err(|_| ClaudeCodeRuntimeError::InputAuthorizationDenied)?;
        if resolution.binding != request.binding {
            return Err(ClaudeCodeRuntimeError::InputAuthorizationDenied);
        }
        Ok(())
    }
}

pub struct DispatchInputAuthorization {
    pub client: ModelInputAuthorizationClient,
    pub store: PostgresAgentRunStore,
    pub run_id: contracts::AgentRunId,
    pub request: ModelInputAuthorizationResolveRequest,
}
#[async_trait]
impl ModelInputAuthorization for DispatchInputAuthorization {
    async fn check(&self) -> Result<(), ClaudeCodeRuntimeError> {
        if self
            .store
            .cancellation_requested(self.run_id)
            .await
            .map_err(|_| ClaudeCodeRuntimeError::InputAuthorizationDenied)?
        {
            return Err(ClaudeCodeRuntimeError::Cancelled);
        }
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            self.client.resolve(&self.request),
        )
        .await
        .map_err(|_| ClaudeCodeRuntimeError::InputAuthorizationDenied)??;
        if self
            .store
            .cancellation_requested(self.run_id)
            .await
            .map_err(|_| ClaudeCodeRuntimeError::InputAuthorizationDenied)?
        {
            return Err(ClaudeCodeRuntimeError::Cancelled);
        }
        Ok(())
    }
}

/// Concrete Control check plus the existing Agent durable review cancellation/lease fence.
pub struct ReviewControlAuthorization {
    pub client: ModelInputAuthorizationClient,
    pub store: crate::llm_review::LlmReviewStore,
}
#[async_trait]
impl crate::llm_review::ReviewInputAuthorization for ReviewControlAuthorization {
    async fn check(
        &self,
        lease: &crate::llm_review::LlmReviewLease,
    ) -> Result<(), ClaudeCodeRuntimeError> {
        self.store
            .check_invocation(lease)
            .await
            .map_err(|_| ClaudeCodeRuntimeError::InputAuthorizationDenied)?;
        self.client
            .resolve(&ModelInputAuthorizationResolveRequest {
                binding: lease.request.input_authorization.clone(),
                selected_paths: lease
                    .request
                    .files
                    .iter()
                    .map(|file| file.path.clone())
                    .collect(),
            })
            .await?;
        self.store
            .check_invocation(lease)
            .await
            .map_err(|_| ClaudeCodeRuntimeError::InputAuthorizationDenied)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::ModelInputAuthorizationClient;
    #[test]
    fn connection_configuration_is_explicit_bounded_and_projection_compatible()
    -> Result<(), Box<dyn std::error::Error>> {
        assert!(
            ModelInputAuthorizationClient::read_configuration([None, None, None, None])
                .map_err(|()| "configuration")?
                .is_none()
        );
        assert!(
            ModelInputAuthorizationClient::read_configuration([
                Some("https://control.invalid".to_owned()),
                None,
                None,
                None
            ])
            .is_err()
        );
        let fixture = tempfile::tempdir()?;
        let ca = fixture.path().join("ca.pem");
        let certificate = rcgen::generate_simple_self_signed(vec!["control.invalid".to_owned()])?;
        std::fs::write(&ca, certificate.cert.pem())?;
        let config = |base: &str, path: &std::path::Path, timeout: &str| {
            [
                Some(base.to_owned()),
                Some(path.display().to_string()),
                Some("control-service".to_owned()),
                Some(timeout.to_owned()),
            ]
        };
        assert!(
            ModelInputAuthorizationClient::read_configuration(config(
                "https://control.invalid/",
                &ca,
                "5000"
            ))
            .is_ok()
        );
        for base in [
            "http://control.invalid/",
            "https://control.invalid/path",
            "https://control.invalid/?query=1",
            "https://control.invalid/#fragment",
            "https://user@control.invalid/",
        ] {
            assert!(
                ModelInputAuthorizationClient::read_configuration(config(base, &ca, "5000"))
                    .is_err()
            );
        }
        for timeout in ["0", "5001", "invalid"] {
            assert!(
                ModelInputAuthorizationClient::read_configuration(config(
                    "https://control.invalid/",
                    &ca,
                    timeout
                ))
                .is_err()
            );
        }
        assert!(
            ModelInputAuthorizationClient::read_configuration(config(
                "https://control.invalid/",
                std::path::Path::new("relative.pem"),
                "5000"
            ))
            .is_err()
        );
        #[cfg(unix)]
        {
            let projected = fixture.path().join("projected-ca.pem");
            std::os::unix::fs::symlink(&ca, &projected)?;
            assert!(
                ModelInputAuthorizationClient::read_configuration(config(
                    "https://control.invalid/",
                    &projected,
                    "5000"
                ))
                .is_ok()
            );
        }
        std::fs::write(&ca, vec![b'a'; 65_537])?;
        assert!(
            ModelInputAuthorizationClient::read_configuration(config(
                "https://control.invalid/",
                &ca,
                "5000"
            ))
            .is_err()
        );
        Ok(())
    }
}

/// Current-session authorization and deterministic DLP for the single metered model gateway.
#[derive(Clone)]
pub struct GatewayInputAuthorization {
    pub client: Option<ModelInputAuthorizationClient>,
    pub classifier: Arc<dyn crate::claude_code::EgressClassifier>,
}

impl GatewayInputAuthorization {
    pub(crate) async fn check(
        &self,
        pool: &sqlx::PgPool,
        claims: &crate::model_gateway::TokenClaims,
    ) -> Result<String, &'static str> {
        const DENIED: &str = "LW_MODEL_INPUT_AUTHORIZATION_DENIED";
        let client = self
            .client
            .as_ref()
            .ok_or("LW_MODEL_INPUT_AUTHORIZATION_UNAVAILABLE")?;
        let (binding, selected_paths, policy) = match &claims.scope {
            crate::model_gateway::ModelGatewayScope::Authoring { track, .. } => {
                let track = match track {
                    contracts::authoring::AgentTrackKind::Environment => "environment",
                    contracts::authoring::AgentTrackKind::Evaluation => "evaluation",
                    contracts::authoring::AgentTrackKind::WorkConfiguration => "work_configuration",
                };
                let row = sqlx::query("SELECT d.request,d.policy FROM agent.agent_run_dispatches d JOIN agent.agent_runs r USING(run_id) JOIN agent.agent_track_work_items t USING(run_id) WHERE r.run_id=$1 AND r.project_id=$2 AND t.track=$3 AND t.attempt_number=$4 AND t.attempt_started_at+((d.policy->'budget'->>'timeoutMilliseconds')::bigint*interval '1 millisecond')>clock_timestamp()")
                    .bind(claims.agent_run_id).bind(claims.project_id.as_uuid()).bind(track).bind(i64::from(claims.attempt)).fetch_optional(pool).await.map_err(|_| DENIED)?.ok_or(DENIED)?;
                let dispatch: crate::run_store::PersistedAgentDispatchRequest =
                    serde_json::from_value(row.try_get("request").map_err(|_| DENIED)?)
                        .map_err(|_| DENIED)?;
                let selected = match dispatch.request {
                    contracts::http::InternalAgentRunRequest::Authoring(request) => {
                        request.selected_paths
                    }
                    contracts::http::InternalAgentRunRequest::WorkConfiguration(request) => {
                        request.selected_paths
                    }
                }
                .ok_or(DENIED)?;
                (
                    dispatch.input_authorization,
                    selected,
                    row.try_get::<serde_json::Value, _>("policy")
                        .map_err(|_| DENIED)?,
                )
            }
            crate::model_gateway::ModelGatewayScope::Review { .. } => {
                let row = sqlx::query("SELECT request_json->'inputAuthorization' AS binding,request_json->'policy' AS policy,jsonb_path_query_array(request_json,'$.files[*].path') AS paths FROM agent.llm_review_runs WHERE task_run_id=$1 AND project_id=$2 AND attempt=$3 AND (request_json->>'deadlineAt')::timestamptz>clock_timestamp()")
                    .bind(claims.agent_run_id).bind(claims.project_id.as_uuid()).bind(i64::from(claims.attempt)).fetch_optional(pool).await.map_err(|_| DENIED)?.ok_or(DENIED)?;
                let binding: contracts::http::ModelInputAuthorizationBinding =
                    serde_json::from_value(row.try_get("binding").map_err(|_| DENIED)?)
                        .map_err(|_| DENIED)?;
                if !matches!(&binding.source, contracts::http::ModelInputAuthorizationSource::FrozenSubmission(source) if source.task_run_id.as_uuid()==claims.agent_run_id)
                {
                    return Err(DENIED);
                }
                let paths = serde_json::from_value(row.try_get("paths").map_err(|_| DENIED)?)
                    .map_err(|_| DENIED)?;
                (
                    binding,
                    paths,
                    row.try_get::<serde_json::Value, _>("policy")
                        .map_err(|_| DENIED)?,
                )
            }
        };
        let policy: contracts::authoring::ProjectLlmEgressPolicy =
            serde_json::from_value(policy).map_err(|_| DENIED)?;
        if binding.project_id != claims.project_id
            || policy.project_id != claims.project_id
            || contracts::http::model_input_policy_sha256(&policy).map_err(|_| DENIED)?
                != binding.policy_sha256
        {
            return Err(DENIED);
        }
        client
            .resolve(&ModelInputAuthorizationResolveRequest {
                binding,
                selected_paths,
            })
            .await
            .map_err(|_| DENIED)?;
        Ok(policy.binding.model)
    }

    pub(crate) async fn classify(
        &self,
        value: &serde_json::Value,
        capability: &str,
        credential: &str,
    ) -> Result<(), &'static str> {
        let mut pending = vec![value];
        while let Some(value) = pending.pop() {
            match value {
                serde_json::Value::String(text) => {
                    self.classify_text(text, capability, credential).await?;
                }
                serde_json::Value::Array(values) => pending.extend(values),
                serde_json::Value::Object(values) => {
                    for (key, value) in values {
                        self.classify_text(key, capability, credential).await?;
                        pending.push(value);
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    async fn classify_text(
        &self,
        text: &str,
        capability: &str,
        credential: &str,
    ) -> Result<(), &'static str> {
        if !self
            .classifier
            .classify("model/tool-result", text.as_bytes())
            .await
            .map_err(|_| "LW_MODEL_INPUT_AUTHORIZATION_DENIED")?
            .is_empty()
            || text.contains(capability)
            || text.contains(credential)
        {
            return Err("LW_MODEL_INPUT_AUTHORIZATION_DENIED");
        }
        Ok(())
    }
}
