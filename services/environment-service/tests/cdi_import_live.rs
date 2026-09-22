//! Opt-in live CDI import of a runtime-registered VM base disk.
//!
//! The test only runs when `LW_LIVE_CDI=1` and the bound connection variables are present. It
//! drives the production `KubernetesCdiImportClient` against a cluster that runs CDI, so the
//! on-demand import path proves itself end to end: an importer `DataVolume` is created and reaches
//! `Succeeded`, the base `DataSource` is published with the recorded identity, a second use reuses
//! the published base, and a drifted identity or an over-capacity recording fails closed.
//!
//! ```text
//! LW_LIVE_CDI=1 \
//! LW_LIVE_CDI_API_SERVER=https://127.0.0.1:4433 \
//! LW_LIVE_CDI_TOKEN_FILE=/tmp/opencode/cdi-token \
//! LW_LIVE_CDI_CA_FILE=/tmp/opencode/cdi-ca.pem \
//! LW_LIVE_CDI_NAMESPACE=labweaver-cdi-live \
//! LW_LIVE_CDI_REGISTRY_URL=docker://172.18.0.1:55478/test-containerdisk@sha256:... \
//! LW_LIVE_CDI_CAPACITY_BYTES=67108864 \
//! LW_LIVE_CDI_STORAGE_CLASS=standard \
//! cargo test -p environment-service --test cdi_import_live -- --nocapture
//! ```
//!
//! The namespace must exist and the identity must be allowed to create, read and delete
//! `datavolumes` and `datasources` there:
//!
//! ```text
//! kubectl create namespace <namespace>
//! kubectl -n <namespace> create serviceaccount cdi-live-verifier
//! kubectl -n <namespace> create role cdi-live-verifier \
//!   --verb=create,get,list,delete,patch,update \
//!   --resource=datavolumes.cdi.kubevirt.io,datasources.cdi.kubevirt.io
//! kubectl -n <namespace> create rolebinding cdi-live-verifier \
//!   --role=cdi-live-verifier --serviceaccount=<namespace>:cdi-live-verifier
//! kubectl -n <namespace> create token cdi-live-verifier --duration=4h > token
//! ```

#![allow(
    clippy::too_many_lines,
    reason = "one live acceptance flow keeps import, reuse, drift and capacity readbacks auditable together"
)]

use std::env;
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use environment_service::{
    CdiImportError, KubeVirtBaseDiskIdentity, KubeVirtBaseDiskImport, KubernetesCdiImportClient,
    ensure_base_disk,
};
use reqwest::{Certificate, Client, Method, Url};
use serde_json::{Value, json};

const SOURCE_REGISTRY_ANNOTATION: &str = "labweaver.io/source-registry";
const CAPACITY_ANNOTATION: &str = "labweaver.io/base-disk-capacity-bytes";
const CDI_PREFIX: &str = "apis/cdi.kubevirt.io/v1beta1";
const POLL_INTERVAL: Duration = Duration::from_secs(2);

struct LiveEnvironment {
    api_server: Url,
    token: String,
    namespace: String,
    import: KubeVirtBaseDiskImport,
}

fn live_environment() -> Option<LiveEnvironment> {
    if env::var("LW_LIVE_CDI").ok().as_deref() != Some("1") {
        return None;
    }
    let namespace = env::var("LW_LIVE_CDI_NAMESPACE").ok()?;
    let registry_url = env::var("LW_LIVE_CDI_REGISTRY_URL").ok()?;
    let capacity_bytes = env::var("LW_LIVE_CDI_CAPACITY_BYTES").ok()?.parse().ok()?;
    let storage_class_name = env::var("LW_LIVE_CDI_STORAGE_CLASS").ok()?;
    let name = format!(
        "lw-cdi-live-{}",
        registry_url
            .rsplit('@')
            .next()
            .unwrap_or("base")
            .chars()
            .filter(char::is_ascii_hexdigit)
            .take(12)
            .collect::<String>()
    );
    Some(LiveEnvironment {
        api_server: Url::parse(&env::var("LW_LIVE_CDI_API_SERVER").ok()?).ok()?,
        token: fs::read_to_string(env::var("LW_LIVE_CDI_TOKEN_FILE").ok()?)
            .ok()?
            .trim()
            .to_owned(),
        namespace: namespace.clone(),
        import: KubeVirtBaseDiskImport {
            data_source_namespace: namespace,
            data_source_name: name,
            source_registry_digest: registry_url,
            disk_sha256: String::new(),
            identity: KubeVirtBaseDiskIdentity::RuntimeRegistryDigest,
            storage_class_name,
            capacity_bytes,
        },
    })
}

/// Minimal REST surface for the readbacks and mutations the live case needs.
struct LiveRest {
    client: Client,
    api_server: Url,
    token: String,
}

impl LiveRest {
    fn new(
        environment: &LiveEnvironment,
        ca_file: &str,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let ca = Certificate::from_pem(&fs::read(PathBuf::from(ca_file))?)?;
        let client = Client::builder()
            .https_only(true)
            .tls_built_in_root_certs(false)
            .add_root_certificate(ca)
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(10))
            .build()?;
        Ok(Self {
            client,
            api_server: environment.api_server.clone(),
            token: environment.token.clone(),
        })
    }

    fn url(&self, path: &str) -> Result<Url, Box<dyn std::error::Error>> {
        Ok(self.api_server.join(path)?)
    }

    async fn get(&self, path: &str) -> Result<Value, Box<dyn std::error::Error>> {
        let response = self
            .client
            .get(self.url(path)?)
            .bearer_auth(&self.token)
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(
                format!("live readback of {path} failed with {}", response.status()).into(),
            );
        }
        Ok(response.json().await?)
    }

    async fn patch_annotation(
        &self,
        path: &str,
        annotation: &str,
        value: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let escaped = annotation.replace('/', "~1");
        let document = json!([
            {"op": "replace", "path": format!("/metadata/annotations/{escaped}"), "value": value}
        ]);
        let response = self
            .client
            .request(Method::PATCH, self.url(path)?)
            .bearer_auth(&self.token)
            .header("content-type", "application/json-patch+json")
            .body(serde_json::to_vec(&document)?)
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(format!("live annotation patch failed with {}", response.status()).into());
        }
        Ok(())
    }

    async fn delete(&self, path: &str) -> Result<(), Box<dyn std::error::Error>> {
        let response = self
            .client
            .delete(self.url(path)?)
            .bearer_auth(&self.token)
            .send()
            .await?;
        if !response.status().is_success() && response.status() != reqwest::StatusCode::NOT_FOUND {
            return Err(format!("live delete of {path} failed with {}", response.status()).into());
        }
        Ok(())
    }
}

#[tokio::test]
async fn live_cdi_import_publishes_reuses_and_verifies_one_base_disk()
-> Result<(), Box<dyn std::error::Error>> {
    let Some(environment) = live_environment() else {
        return Ok(());
    };
    let ca_file = env::var("LW_LIVE_CDI_CA_FILE")?;
    let rest = LiveRest::new(&environment, &ca_file)?;
    let client = KubernetesCdiImportClient::new(
        rest.client.clone(),
        environment.api_server.clone(),
        environment.token.clone(),
        POLL_INTERVAL,
        Duration::from_mins(3),
    );
    let namespace = environment.namespace.as_str();
    let data_source_path = format!(
        "{CDI_PREFIX}/namespaces/{namespace}/datasources/{}",
        environment.import.data_source_name
    );
    let data_volume_path = format!(
        "{CDI_PREFIX}/namespaces/{namespace}/datavolumes/{}",
        environment.import.data_volume_name()
    );
    let _ = rest.delete(&data_source_path).await;
    let _ = rest.delete(&data_volume_path).await;

    let published = ensure_base_disk(&client, &environment.import).await?;
    assert_eq!(
        published.data_source_name,
        environment.import.data_source_name
    );
    let data_source = rest.get(&data_source_path).await?;
    let annotations = data_source
        .pointer("/metadata/annotations")
        .and_then(Value::as_object)
        .ok_or("the published DataSource carries no annotations")?;
    assert_eq!(
        annotations
            .get(SOURCE_REGISTRY_ANNOTATION)
            .and_then(Value::as_str),
        Some(environment.import.source_registry_digest.as_str()),
        "the published DataSource must record the reviewed registry digest"
    );
    assert_eq!(
        annotations.get(CAPACITY_ANNOTATION).and_then(Value::as_str),
        Some(environment.import.capacity_bytes.to_string().as_str()),
        "the published DataSource must record the reviewed capacity"
    );
    let data_volume = rest.get(&data_volume_path).await?;
    let uid = data_volume
        .pointer("/metadata/uid")
        .and_then(Value::as_str)
        .ok_or("the imported DataVolume has no UID")?
        .to_owned();
    println!(
        "live CDI import readback: phase={:?} dataVolume={} dataSource={} capacityBytes={}",
        data_volume.pointer("/status/phase").and_then(Value::as_str),
        environment.import.data_volume_name(),
        environment.import.data_source_name,
        environment.import.capacity_bytes
    );

    // A second use resolves the published base instead of importing again.
    let reused = ensure_base_disk(&client, &environment.import).await?;
    assert_eq!(reused.identity, published.identity);
    let after = rest.get(&data_volume_path).await?;
    assert_eq!(
        after.pointer("/metadata/uid").and_then(Value::as_str),
        Some(uid.as_str()),
        "a second use must reuse the imported DataVolume"
    );
    println!("live CDI reuse readback: the published base DataSource was resolved again");

    // A drifted registry digest fails closed instead of silently reusing the base.
    rest.patch_annotation(
        &data_source_path,
        SOURCE_REGISTRY_ANNOTATION,
        "docker://drifted.invalid/base@sha256:0000000000000000000000000000000000000000000000000000000000000000",
    )
    .await?;
    assert_eq!(
        ensure_base_disk(&client, &environment.import)
            .await
            .err()
            .map(|error| error.to_string()),
        Some(CdiImportError::IdentityMismatch.to_string()),
        "a drifted recorded identity must fail closed"
    );
    // An over-capacity recording fails closed as well.
    rest.patch_annotation(
        &data_source_path,
        SOURCE_REGISTRY_ANNOTATION,
        environment.import.source_registry_digest.as_str(),
    )
    .await?;
    rest.patch_annotation(
        &data_source_path,
        CAPACITY_ANNOTATION,
        (environment.import.capacity_bytes + 1).to_string().as_str(),
    )
    .await?;
    assert_eq!(
        ensure_base_disk(&client, &environment.import)
            .await
            .err()
            .map(|error| error.to_string()),
        Some(CdiImportError::CapacityExceeded.to_string()),
        "an over-capacity recording must fail closed"
    );
    println!(
        "live CDI identity readback: drifted digest and over-capacity recording both fail closed"
    );

    rest.delete(&data_source_path).await?;
    rest.delete(&data_volume_path).await?;
    Ok(())
}
