//! Read-only Ceph admission guard. No cached success and no Kubernetes writes.
use std::{path::PathBuf, sync::Arc, time::Duration};

use reqwest::{Certificate, Client, Url};
use serde_json::Value;

#[derive(Clone)]
pub enum StorageHealthGate {
    /// Non-Ceph development installations do not claim the HA storage profile.
    NonCeph,
    Invalid,
    Ceph(Arc<CephObserver>),
}

pub struct CephObserver {
    client: Client,
    endpoint: Url,
    token_file: PathBuf,
}

#[derive(Clone, Copy, Debug, thiserror::Error)]
pub enum StorageHealthError {
    #[error("LW_STORAGE_HEALTH_UNAVAILABLE")]
    Unavailable,
    #[error("LW_STORAGE_HEALTH_STALE")]
    Stale,
    #[error("LW_STORAGE_UNSAFE")]
    Unsafe,
}

impl StorageHealthError {
    #[must_use]
    pub const fn diagnostic_code(self) -> &'static str {
        match self {
            Self::Unavailable => "LW_STORAGE_HEALTH_UNAVAILABLE",
            Self::Stale => "LW_STORAGE_HEALTH_STALE",
            Self::Unsafe => "LW_STORAGE_UNSAFE",
        }
    }

    #[must_use]
    pub const fn detail(self) -> &'static str {
        match self {
            Self::Unavailable => {
                "无法读取存储健康，已暂停新准入；请检查存储控制器、网络和只读授权。"
            }
            Self::Stale => "存储健康观测已过期，已暂停新准入；请恢复存储控制器与集群连接。",
            Self::Unsafe => "存储存在容量、仲裁或副本异常，已暂停新准入；请先恢复存储健康。",
        }
    }
}

impl StorageHealthGate {
    #[must_use]
    pub fn from_env() -> Self {
        let endpoint = match std::env::var("LABWEAVER_STORAGE_HEALTH_CEPH_URL") {
            Ok(value) => value,
            Err(std::env::VarError::NotPresent) => return Self::NonCeph,
            Err(_) => return Self::Invalid,
        };
        Self::configured(&endpoint).unwrap_or(Self::Invalid)
    }

    fn configured(endpoint: &str) -> Option<Self> {
        let endpoint = Url::parse(endpoint).ok()?;
        if endpoint.scheme() != "https"
            || endpoint.host_str().is_none()
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
            || !endpoint
                .path()
                .starts_with("/apis/ceph.rook.io/v1/namespaces/")
            || !endpoint.path().contains("/cephclusters/")
        {
            return None;
        }
        let root = PathBuf::from("/var/run/secrets/kubernetes.io/serviceaccount");
        let ca = std::fs::read(root.join("ca.crt")).ok()?;
        let client = Client::builder()
            .no_proxy()
            .https_only(true)
            .tls_built_in_root_certs(false)
            .add_root_certificate(Certificate::from_pem(&ca).ok()?)
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(3))
            .build()
            .ok()?;
        Some(Self::Ceph(Arc::new(CephObserver {
            client,
            endpoint,
            token_file: root.join("token"),
        })))
    }

    pub async fn check(&self) -> Result<(), StorageHealthError> {
        match self {
            Self::NonCeph => Ok(()),
            Self::Invalid => Err(StorageHealthError::Unavailable),
            Self::Ceph(observer) => observer.check().await,
        }
    }
}

impl CephObserver {
    async fn check(&self) -> Result<(), StorageHealthError> {
        let token = tokio::fs::read_to_string(&self.token_file)
            .await
            .map_err(|_| StorageHealthError::Unavailable)?;
        if token.len() > 16384 || token.trim().is_empty() {
            return Err(StorageHealthError::Unavailable);
        }
        let mut response = self
            .client
            .get(self.endpoint.clone())
            .bearer_auth(token.trim())
            .send()
            .await
            .map_err(|_| StorageHealthError::Unavailable)?
            .error_for_status()
            .map_err(|_| StorageHealthError::Unavailable)?;
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| StorageHealthError::Unavailable)?
        {
            if bytes.len() + chunk.len() > 1_048_576 {
                return Err(StorageHealthError::Unavailable);
            }
            bytes.extend_from_slice(&chunk);
        }
        let document =
            serde_json::from_slice(&bytes).map_err(|_| StorageHealthError::Unavailable)?;
        evaluate(&document, time::OffsetDateTime::now_utc())
    }
}

fn evaluate(document: &Value, now: time::OffsetDateTime) -> Result<(), StorageHealthError> {
    let checked = document
        .pointer("/status/ceph/lastChecked")
        .and_then(Value::as_str)
        .and_then(|value| {
            time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339).ok()
        })
        .ok_or(StorageHealthError::Unavailable)?;
    let age = now - checked;
    if age.is_negative() || age > time::Duration::seconds(30) {
        return Err(StorageHealthError::Stale);
    }
    // HEALTH_WARN includes OSD_NEARFULL, degraded/undersized PGs and mon quorum warnings.
    // Never infer safe writes from Ready alone or from a retained last-good health result.
    if document
        .pointer("/status/ceph/health")
        .and_then(Value::as_str)
        != Some("HEALTH_OK")
        || document.pointer("/status/phase").and_then(Value::as_str) != Some("Ready")
        || document
            .pointer("/metadata/deletionTimestamp")
            .is_some_and(|value| !value.is_null())
    {
        return Err(StorageHealthError::Unsafe);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn nearfull_quorum_loss_partition_and_missing_signals_refuse_admission()
    -> Result<(), Box<dyn std::error::Error>> {
        let now = time::OffsetDateTime::parse(
            "2026-10-25T00:00:20Z",
            &time::format_description::well_known::Rfc3339,
        )?;
        let good = json!({"status":{"phase":"Ready","ceph":{"health":"HEALTH_OK","lastChecked":"2026-10-25T00:00:00Z"}}});
        assert!(evaluate(&good, now).is_ok());
        for warning in ["OSD_NEARFULL", "MON_DOWN", "PG_AVAILABILITY"] {
            let mut bad = good.clone();
            bad["status"]["ceph"]["health"] = json!("HEALTH_WARN");
            bad["status"]["ceph"]["details"] = json!({warning:{"severity":"HEALTH_WARN"}});
            assert!(matches!(
                evaluate(&bad, now),
                Err(StorageHealthError::Unsafe)
            ));
        }
        assert!(matches!(
            evaluate(&good, now + time::Duration::seconds(11)),
            Err(StorageHealthError::Stale)
        ));
        assert!(matches!(
            evaluate(&json!({}), now),
            Err(StorageHealthError::Unavailable)
        ));
        Ok(())
    }

    #[tokio::test]
    async fn unreadable_observer_cannot_become_success() {
        assert!(matches!(
            StorageHealthGate::Invalid.check().await,
            Err(StorageHealthError::Unavailable)
        ));
    }

    #[tokio::test]
    async fn actual_kubernetes_reads_do_not_cache_success_and_follow_token_rotation()
    -> Result<(), Box<dyn std::error::Error>> {
        use axum::{
            Json, Router,
            extract::State,
            http::{HeaderMap, StatusCode},
            routing::get,
        };
        use std::sync::Mutex;
        type Signals = Arc<Mutex<(String, Value)>>;
        async fn signal(
            State(state): State<Signals>,
            headers: HeaderMap,
        ) -> Result<Json<Value>, StatusCode> {
            let state = state
                .lock()
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            if headers
                .get("authorization")
                .and_then(|value| value.to_str().ok())
                != Some(state.0.as_str())
            {
                return Err(StatusCode::UNAUTHORIZED);
            }
            Ok(Json(state.1.clone()))
        }
        let now = time::OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)?;
        let signals = Arc::new(Mutex::new((
            "Bearer external-test-token-1".to_owned(),
            json!({"status":{"phase":"Ready","ceph":{"health":"HEALTH_OK","lastChecked":now}}}),
        )));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let router = Router::new()
            .route("/ceph", get(signal))
            .with_state(signals.clone());
        let server = tokio::spawn(async move { axum::serve(listener, router).await });
        let directory = tempfile::tempdir()?;
        let token = directory.path().join("token");
        tokio::fs::write(&token, "external-test-token-1").await?;
        // HTTP is confined to this external Kubernetes boundary fixture. Production builder
        // requires HTTPS, the projected cluster CA and a resource-scoped read-only token.
        let observer = StorageHealthGate::Ceph(Arc::new(CephObserver {
            client: Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(1))
                .build()?,
            endpoint: format!("http://{address}/ceph").parse()?,
            token_file: token.clone(),
        }));
        observer.check().await?;
        signals.lock().map_err(|_| "signal poisoned")?.1["status"]["ceph"]["health"] =
            json!("HEALTH_ERR");
        assert!(matches!(
            observer.check().await,
            Err(StorageHealthError::Unsafe)
        ));
        {
            let mut value = signals.lock().map_err(|_| "signal poisoned")?;
            value.0 = "Bearer external-test-token-2".to_owned();
            value.1["status"]["ceph"]["health"] = json!("HEALTH_OK");
        }
        assert!(matches!(
            observer.check().await,
            Err(StorageHealthError::Unavailable)
        ));
        tokio::fs::write(&token, "external-test-token-2").await?;
        observer.check().await?;
        tokio::fs::remove_file(&token).await?;
        assert!(matches!(
            observer.check().await,
            Err(StorageHealthError::Unavailable)
        ));
        server.abort();
        Ok(())
    }
}
