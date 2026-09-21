//! Shared `PostgreSQL` and OCI registry fixtures for the Agent integration binaries.
#![allow(
    dead_code,
    reason = "each integration binary links one subset of the shared fixtures"
)]

use std::collections::{BTreeMap, HashMap};
use std::error::Error;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use artifact_store::{ImmutableObjectStore, ObjectStoreError, PresignedUpload, VerifiedObject};
use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::{Method, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use contracts::{ArtifactRef, UtcTimestamp};
use persistence_sqlx::{Domain, MigrationCatalog, Sha256Digest};
use sqlx::PgPool;

/// Creates the Agent schema and applies every migration currently listed in the checked-in
/// catalog. Integration tests must exercise the same schema as the service instead of selecting
/// a historical subset of migrations by hand.
pub async fn apply_agent_migrations(pool: &PgPool) -> Result<(), Box<dyn Error>> {
    sqlx::query("CREATE SCHEMA agent").execute(pool).await?;
    let mut connection = pool.acquire().await?;
    sqlx::query("SET search_path = agent, pg_catalog")
        .execute(&mut *connection)
        .await?;
    let migration_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../migrations");
    let catalog = MigrationCatalog::load(&migration_root.join("catalog.yaml"))?;
    let agent_migrations = catalog
        .domains
        .iter()
        .find(|domain| domain.name == Domain::Agent)
        .ok_or_else(|| std::io::Error::other("migration catalog has no agent domain"))?;
    for migration in &agent_migrations.migrations {
        let sql = MigrationCatalog::read_verified_sql(&migration_root, migration)?;
        sqlx::raw_sql(&sql).execute(&mut *connection).await?;
    }
    Ok(())
}

#[derive(Default)]
struct FakeRegistryState {
    blobs: BTreeMap<String, Vec<u8>>,
    manifests: BTreeMap<String, Vec<u8>>,
    uploads: u32,
    manifest_reads: u32,
}

/// Immutable object store fixture that serves one staged archive and refuses every other call.
///
/// `read_verified` answers with the staged bytes and the caller's expected reference, so an
/// import test exercises the real verify-then-publish path without an S3 endpoint.
#[derive(Clone, Debug)]
pub struct FakeObjects {
    bytes: Vec<u8>,
}

impl FakeObjects {
    /// Stages `bytes` as the exact object every verified read returns.
    #[must_use]
    pub fn new(bytes: Vec<u8>) -> Self {
        Self { bytes }
    }
}

#[async_trait::async_trait]
impl ImmutableObjectStore for FakeObjects {
    fn binding(&self) -> &'static str {
        "test-object-store"
    }

    async fn presign_upload(
        &self,
        _: &str,
        _: u64,
        _: &str,
        _: UtcTimestamp,
    ) -> Result<PresignedUpload, ObjectStoreError> {
        Err(ObjectStoreError::ObjectUnavailable)
    }

    async fn read_verified(
        &self,
        _: &str,
        expected: &ArtifactRef,
    ) -> Result<VerifiedObject, ObjectStoreError> {
        Ok(VerifiedObject {
            reference: expected.clone(),
            bytes: self.bytes.clone(),
        })
    }

    async fn freeze_current(
        &self,
        _: &str,
        _: u64,
        _: &str,
    ) -> Result<VerifiedObject, ObjectStoreError> {
        Err(ObjectStoreError::ObjectUnavailable)
    }

    async fn delete_orphan(&self, _: &str, _: &str) -> Result<(), ObjectStoreError> {
        Err(ObjectStoreError::DeleteFailed)
    }
}

/// In-memory OCI distribution fixture for the platform image resolver and publisher.
///
/// The fixture recomputes every digest from the bytes it actually holds, so a manifest readback
/// can never be satisfied by fixture bookkeeping: a push that uploaded different content than the
/// caller verified is rejected by the same `docker-content-digest` rule a real registry applies.
/// Every accepted blob is retained so a test can assert the digest-preserving push really sent
/// the verified bytes.
#[derive(Clone, Default)]
pub struct FakeRegistry {
    state: Arc<Mutex<FakeRegistryState>>,
}

impl FakeRegistry {
    /// Serves the registry on an ephemeral loopback port and returns the `http://host:port/` base.
    ///
    /// # Errors
    ///
    /// Returns the loopback bind failure.
    pub async fn spawn() -> Result<(Self, String), std::io::Error> {
        let registry = Self::default();
        let router = axum::Router::new()
            .route("/v2/{*path}", axum::routing::any(handle))
            .with_state(registry.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        Ok((registry, format!("http://{address}/")))
    }

    /// Returns the `host:port` authority a reviewed reference must name for this fixture.
    #[must_use]
    pub fn authority(base: &str) -> String {
        base.trim_start_matches("http://")
            .trim_end_matches('/')
            .to_owned()
    }

    /// Publishes `bytes` as the manifest the registry serves under `reference`.
    pub fn set_manifest(&self, reference: &str, bytes: Vec<u8>) {
        self.lock().manifests.insert(reference.to_owned(), bytes);
    }

    /// Returns every blob digest the registry accepted, in stable order.
    #[must_use]
    pub fn blob_digests(&self) -> Vec<String> {
        self.lock().blobs.keys().cloned().collect()
    }

    /// Returns the exact bytes the registry holds for one blob digest.
    #[must_use]
    pub fn blob(&self, digest: &str) -> Option<Vec<u8>> {
        self.lock().blobs.get(digest).cloned()
    }

    /// Returns the digest `reference` currently resolves to, or `None` when it is not published.
    #[must_use]
    pub fn manifest_digest(&self, reference: &str) -> Option<String> {
        self.lock()
            .manifests
            .get(reference)
            .map(|bytes| digest_of(bytes))
    }

    /// Returns how many blob upload sessions the registry accepted.
    #[must_use]
    pub fn uploads(&self) -> u32 {
        self.lock().uploads
    }

    /// Returns how many manifest readbacks the registry served.
    #[must_use]
    pub fn manifest_reads(&self) -> u32 {
        self.lock().manifest_reads
    }

    fn lock(&self) -> MutexGuard<'_, FakeRegistryState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

fn digest_of(bytes: &[u8]) -> String {
    format!("sha256:{}", Sha256Digest::of_bytes(bytes))
}

async fn handle(
    State(registry): State<FakeRegistry>,
    method: Method,
    uri: Uri,
    Query(params): Query<HashMap<String, String>>,
    body: Bytes,
) -> Response {
    let path = uri.path().to_owned();
    let reference = path.rsplit('/').next().unwrap_or_default().to_owned();
    if method == Method::HEAD && path.contains("/blobs/") && !path.contains("/blobs/uploads/") {
        return if registry.lock().blobs.contains_key(&reference) {
            StatusCode::OK
        } else {
            StatusCode::NOT_FOUND
        }
        .into_response();
    }
    if method == Method::POST && path.ends_with("/blobs/uploads/") {
        let mut state = registry.lock();
        state.uploads += 1;
        let location = format!("{path}session-{}", state.uploads);
        return (StatusCode::ACCEPTED, [(header::LOCATION, location)]).into_response();
    }
    if method == Method::PUT && path.contains("/blobs/uploads/") {
        let Some(declared) = params.get("digest") else {
            return StatusCode::BAD_REQUEST.into_response();
        };
        if body.is_empty() {
            return StatusCode::BAD_REQUEST.into_response();
        }
        let digest = digest_of(&body);
        if digest != *declared {
            return StatusCode::BAD_REQUEST.into_response();
        }
        registry.lock().blobs.insert(digest, body.to_vec());
        return StatusCode::CREATED.into_response();
    }
    if method == Method::PUT && path.contains("/manifests/") {
        registry.lock().manifests.insert(reference, body.to_vec());
        return StatusCode::CREATED.into_response();
    }
    if method == Method::GET && path.contains("/manifests/") {
        let mut state = registry.lock();
        state.manifest_reads += 1;
        let Some(bytes) = state.manifests.get(&reference).cloned() else {
            return StatusCode::NOT_FOUND.into_response();
        };
        let digest = digest_of(&bytes);
        return (
            [
                (
                    header::CONTENT_TYPE,
                    "application/vnd.oci.image.manifest.v1+json".to_owned(),
                ),
                (
                    header::HeaderName::from_static("docker-content-digest"),
                    digest,
                ),
            ],
            bytes,
        )
            .into_response();
    }
    StatusCode::NOT_FOUND.into_response()
}
