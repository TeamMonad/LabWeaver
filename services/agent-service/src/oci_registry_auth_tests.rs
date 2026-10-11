//! Strict HTTPS coverage for the external Registry Bearer authentication protocol.

use std::{
    collections::HashMap,
    error::Error,
    io::Write,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use axum::{
    Json, Router,
    body::Bytes,
    extract::{Query, State},
    http::{HeaderMap, Method, StatusCode, Uri, header},
    response::{IntoResponse, Response},
    routing::{any, get},
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use reqwest::Client;
use serde_json::json;
use tokio::sync::Mutex;

use super::{
    OciFileBlob, OciFileImage, OciRegistryError, OciRegistryPublisher, RegistryCredentials,
};

#[path = "../../http_transport.rs"]
#[allow(
    dead_code,
    reason = "registry fixtures only use the shared TLS server construction and serving functions"
)]
mod tls;

const REPOSITORY: &str = "labweaver-system/platform-build";

#[derive(Clone, Copy)]
enum Mode {
    Normal,
    HeadNotFound,
    AccessTokenOnly,
    TokenRejected,
    TokenExpired,
    TokenAmbiguous,
    TokenFuture,
    PushDenied,
    RealmCrossHost,
    RealmCrossPort,
    RealmHttp,
    RealmRedirect,
    LocationCrossPort,
    LocationUserinfo,
    LocationFragment,
    PutDenied,
    StartUnavailable,
    ExpireBeforePut,
    SlowPut,
}

#[derive(Debug)]
struct Event {
    stage: &'static str,
    authorized: bool,
    scope: Option<String>,
    length: usize,
}

struct Registry {
    base: reqwest::Url,
    mode: Mode,
    events: Mutex<Vec<Event>>,
    token_requests: AtomicUsize,
    blobs: Mutex<HashMap<String, Vec<u8>>>,
    manifest: Mutex<Option<(String, Vec<u8>)>>,
}

struct Fixture {
    state: Arc<Registry>,
    client: Client,
    server: tokio::task::JoinHandle<()>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

impl Fixture {
    async fn start(mode: Mode) -> Result<Self, Box<dyn Error>> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let base = reqwest::Url::parse(&format!("https://{}/", listener.local_addr()?))?;
        let certificate = rcgen::generate_simple_self_signed(vec!["127.0.0.1".to_owned()])?;
        let pem = certificate.cert.pem();
        let tls = tls::server_config(
            pem.as_bytes(),
            certificate.signing_key.serialize_pem().as_bytes(),
        )?;
        let client = Client::builder()
            .no_proxy()
            .https_only(true)
            .tls_built_in_root_certs(false)
            .add_root_certificate(reqwest::Certificate::from_pem(pem.as_bytes())?)
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_millis(
                if matches!(mode, Mode::SlowPut) {
                    200
                } else {
                    5_000
                },
            ))
            .build()?;
        let state = Arc::new(Registry {
            base,
            mode,
            events: Mutex::new(Vec::new()),
            token_requests: AtomicUsize::new(0),
            blobs: Mutex::new(HashMap::new()),
            manifest: Mutex::new(None),
        });
        let router = Router::new()
            .route("/token", get(token))
            .route("/v2/{*path}", any(registry))
            .with_state(Arc::clone(&state));
        let server = tokio::spawn(async move {
            let _ = tls::serve_tls(listener, router, tls).await;
        });
        Ok(Self {
            state,
            client,
            server,
        })
    }

    fn publisher(&self) -> Result<OciRegistryPublisher, OciRegistryError> {
        OciRegistryPublisher::new(
            self.state.base.clone(),
            REPOSITORY,
            self.client.clone(),
            RegistryCredentials {
                username: "robot$build".to_owned(),
                password: "fixture-password".to_owned(),
            },
        )
    }
}

fn challenge(state: &Registry) -> String {
    let mut realm = state.base.join("token").expect("fixture realm");
    match state.mode {
        Mode::RealmCrossHost => {
            realm
                .set_host(Some("other.example.invalid"))
                .expect("fixture host");
        }
        Mode::RealmCrossPort => {
            realm
                .set_port(Some(
                    realm.port_or_known_default().expect("fixture port") + 1,
                ))
                .expect("fixture port");
        }
        Mode::RealmHttp => {
            realm.set_scheme("http").expect("fixture scheme");
        }
        _ => {}
    }
    format!(
        "Basic realm=\"registry\", Bearer realm=\"{realm}\",service=\"fixture-registry\",scope=\"repository:{REPOSITORY}:pull\""
    )
}

async fn token(
    State(state): State<Arc<Registry>>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let expected = format!("Basic {}", STANDARD.encode("robot$build:fixture-password"));
    let authorized = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        == Some(expected.as_str());
    let scope = query.get("scope").cloned();
    state.events.lock().await.push(Event {
        stage: "token",
        authorized,
        scope: scope.clone(),
        length: 0,
    });
    if !authorized
        || query.get("service").map(String::as_str) != Some("fixture-registry")
        || !matches!(
            scope.as_deref(),
            Some(
                "repository:labweaver-system/platform-build:pull,push"
                    | "repository:labweaver-system/platform-build:pull"
            )
        )
    {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if matches!(state.mode, Mode::TokenRejected) {
        return StatusCode::FORBIDDEN.into_response();
    }
    if matches!(state.mode, Mode::RealmRedirect) {
        return (
            StatusCode::TEMPORARY_REDIRECT,
            [(header::LOCATION, "https://other.example.invalid/token")],
        )
            .into_response();
    }
    let count = state.token_requests.fetch_add(1, Ordering::SeqCst) + 1;
    let token = format!("fixture-token-{count}");
    if matches!(state.mode, Mode::AccessTokenOnly) {
        return Json(json!({"access_token": token})).into_response();
    }
    if matches!(state.mode, Mode::TokenExpired) {
        return Json(
            json!({"token": token, "expires_in": 60, "issued_at": "2000-01-01T00:00:00Z"}),
        )
        .into_response();
    }
    if matches!(state.mode, Mode::TokenAmbiguous) {
        return Json(json!({"token": token, "access_token": "different-token", "expires_in": 300}))
            .into_response();
    }
    if matches!(state.mode, Mode::TokenFuture) {
        return Json(
            json!({"token": token, "expires_in": 300, "issued_at": "2099-01-01T00:00:00Z"}),
        )
        .into_response();
    }
    Json(json!({"token": token, "expires_in": if matches!(state.mode, Mode::ExpireBeforePut) { 1 } else { 300 }})).into_response()
}

async fn registry(
    State(state): State<Arc<Registry>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    body: Bytes,
) -> Response {
    let path = uri.path();
    let phase = if method == Method::HEAD {
        "head"
    } else if method == Method::POST {
        "start"
    } else if path.contains("/blobs/uploads/") {
        "blob_put"
    } else if method == Method::PUT {
        "manifest_put"
    } else {
        "manifest_get"
    };
    let expected = format!(
        "Bearer fixture-token-{}",
        state.token_requests.load(Ordering::SeqCst)
    );
    let authorized = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        == Some(expected.as_str());
    state.events.lock().await.push(Event {
        stage: phase,
        authorized,
        scope: None,
        length: body.len(),
    });
    if !authorized {
        if method == Method::HEAD && matches!(state.mode, Mode::HeadNotFound) {
            return StatusCode::NOT_FOUND.into_response();
        }
        return (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, challenge(&state))],
        )
            .into_response();
    }
    if method == Method::HEAD {
        return if state
            .blobs
            .lock()
            .await
            .contains_key(path.rsplit('/').next().unwrap_or_default())
        {
            StatusCode::OK
        } else {
            StatusCode::NOT_FOUND
        }
        .into_response();
    }
    if method == Method::POST {
        return upload_start(&state, path).await;
    }
    if phase == "blob_put" {
        if matches!(state.mode, Mode::SlowPut) {
            tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        }
        if matches!(state.mode, Mode::PutDenied) {
            return (
                StatusCode::UNAUTHORIZED,
                [(header::WWW_AUTHENTICATE, challenge(&state))],
            )
                .into_response();
        }
        let digest = format!("sha256:{}", persistence_sqlx::Sha256Digest::of_bytes(&body));
        if query.get("digest") != Some(&digest)
            || query.get("_state").map(String::as_str) != Some("fixture-state")
            || headers
                .get(header::CONTENT_LENGTH)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<usize>().ok())
                != Some(body.len())
        {
            return StatusCode::BAD_REQUEST.into_response();
        }
        state.blobs.lock().await.insert(digest, body.to_vec());
        return StatusCode::CREATED.into_response();
    }
    if phase == "manifest_put" {
        *state.manifest.lock().await = Some((
            path.rsplit('/').next().unwrap_or_default().to_owned(),
            body.to_vec(),
        ));
        return StatusCode::CREATED.into_response();
    }
    manifest_read(&state, path).await
}

async fn upload_start(state: &Registry, path: &str) -> Response {
    if matches!(state.mode, Mode::PushDenied) {
        return StatusCode::FORBIDDEN.into_response();
    }
    if matches!(state.mode, Mode::StartUnavailable) {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    if matches!(state.mode, Mode::ExpireBeforePut) {
        tokio::time::sleep(std::time::Duration::from_millis(1_100)).await;
    }
    let mut location = state
        .base
        .join(&format!("{path}session"))
        .expect("fixture location");
    match state.mode {
        Mode::LocationCrossPort => {
            location
                .set_port(Some(
                    location.port_or_known_default().expect("fixture port") + 1,
                ))
                .expect("fixture port");
        }
        Mode::LocationUserinfo => {
            location.set_username("foreign").expect("fixture userinfo");
        }
        Mode::LocationFragment => location.set_fragment(Some("fragment")),
        _ => {}
    }
    // Exercise Harbor's upload state query: adding the digest must preserve it.
    location.set_query(Some("_state=fixture-state"));
    (
        StatusCode::ACCEPTED,
        [(header::LOCATION, location.to_string())],
    )
        .into_response()
}

async fn manifest_read(state: &Registry, path: &str) -> Response {
    let Some((reference, bytes)) = state.manifest.lock().await.clone() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if path.rsplit('/').next() != Some(reference.as_str()) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let digest = format!(
        "sha256:{}",
        persistence_sqlx::Sha256Digest::of_bytes(&bytes)
    );
    (
        StatusCode::OK,
        [
            (
                header::HeaderName::from_static("docker-content-digest"),
                digest,
            ),
            (
                header::CONTENT_TYPE,
                "application/vnd.oci.image.manifest.v1+json".to_owned(),
            ),
        ],
        bytes,
    )
        .into_response()
}

fn file_image() -> Result<OciFileImage, Box<dyn Error>> {
    let image = super::tests::image();
    let mut blobs = Vec::new();
    for blob in image.blobs {
        let mut file = tempfile::NamedTempFile::new()?;
        file.write_all(&blob.bytes)?;
        blobs.push(OciFileBlob {
            digest: blob.digest,
            media_type: blob.media_type,
            size_bytes: u64::try_from(blob.bytes.len())?,
            path: file.into_temp_path(),
        });
    }
    Ok(OciFileImage {
        manifest_digest: image.manifest_digest,
        manifest_media_type: image.manifest_media_type,
        manifest_bytes: image.manifest_bytes,
        blobs,
    })
}

#[tokio::test]
async fn https_bearer_preserves_file_digest_through_publish_and_manifest_readback()
-> Result<(), Box<dyn Error>> {
    for mode in [Mode::Normal, Mode::HeadNotFound, Mode::AccessTokenOnly] {
        let fixture = Fixture::start(mode).await?;
        let publisher = fixture.publisher()?;
        let image = file_image()?;
        assert_eq!(publisher.publish_file(&image).await?, image.manifest_digest);
        assert_eq!(
            publisher.tag_file("latest", &image).await?,
            image.manifest_digest
        );
        let events = fixture.state.events.lock().await;
        assert_eq!(events.iter().filter(|e| e.stage == "token").count(), 1);
        assert!(
            events
                .iter()
                .filter(|e| e.stage == "token")
                .all(|e| e.authorized
                    && e.scope.as_deref()
                        == Some("repository:labweaver-system/platform-build:pull,push"))
        );
        assert_eq!(
            events
                .iter()
                .filter(|e| e.stage == "blob_put" && e.authorized && e.length > 0)
                .count(),
            2
        );
        assert_eq!(
            events
                .iter()
                .filter(|e| e.stage == "manifest_get" && e.authorized)
                .count(),
            2
        );
        assert!(!format!("{publisher:?}").contains("fixture-token"));
        assert!(!format!("{publisher:?}").contains("fixture-password"));
    }
    Ok(())
}

#[tokio::test]
async fn https_read_resolution_requests_only_exact_repository_pull() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::start(Mode::Normal).await?;
    let image = super::tests::image();
    *fixture.state.manifest.lock().await = Some(("latest".to_owned(), image.manifest_bytes));
    assert_eq!(
        fixture.publisher()?.resolve_tag("latest").await?.digest,
        image.manifest_digest
    );
    let events = fixture.state.events.lock().await;
    assert!(
        events
            .iter()
            .filter(|e| e.stage == "token")
            .all(|e| e.scope.as_deref() == Some("repository:labweaver-system/platform-build:pull"))
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e.stage, "start" | "blob_put" | "manifest_put"))
    );
    Ok(())
}

#[tokio::test]
async fn https_bad_realm_and_redirect_never_receive_registry_credentials()
-> Result<(), Box<dyn Error>> {
    for mode in [
        Mode::RealmCrossHost,
        Mode::RealmCrossPort,
        Mode::RealmHttp,
        Mode::RealmRedirect,
    ] {
        let fixture = Fixture::start(mode).await?;
        assert_eq!(
            fixture
                .publisher()?
                .publish_file(&file_image()?)
                .await
                .err(),
            Some(OciRegistryError::Rejected)
        );
        let events = fixture.state.events.lock().await;
        assert!(
            !events
                .iter()
                .any(|e| matches!(e.stage, "start" | "blob_put" | "manifest_put"))
        );
        assert_eq!(
            events.iter().filter(|e| e.stage == "token").count(),
            usize::from(matches!(mode, Mode::RealmRedirect))
        );
    }
    Ok(())
}

#[tokio::test]
async fn https_denied_or_expired_token_and_subset_grants_fail_closed() -> Result<(), Box<dyn Error>>
{
    for mode in [Mode::TokenRejected, Mode::TokenExpired, Mode::PushDenied] {
        let fixture = Fixture::start(mode).await?;
        assert_eq!(
            fixture
                .publisher()?
                .publish_file(&file_image()?)
                .await
                .err(),
            Some(OciRegistryError::Denied)
        );
        assert!(
            !fixture
                .state
                .events
                .lock()
                .await
                .iter()
                .any(|e| matches!(e.stage, "blob_put" | "manifest_put"))
        );
    }
    Ok(())
}

#[tokio::test]
async fn https_ambiguous_or_future_token_response_is_rejected() -> Result<(), Box<dyn Error>> {
    for mode in [Mode::TokenAmbiguous, Mode::TokenFuture] {
        let fixture = Fixture::start(mode).await?;
        assert_eq!(
            fixture
                .publisher()?
                .publish_file(&file_image()?)
                .await
                .err(),
            Some(OciRegistryError::Rejected)
        );
        assert!(
            !fixture
                .state
                .events
                .lock()
                .await
                .iter()
                .any(|e| matches!(e.stage, "start" | "blob_put" | "manifest_put"))
        );
    }
    Ok(())
}

#[tokio::test]
async fn https_upload_location_cannot_change_origin_or_add_userinfo_or_fragment()
-> Result<(), Box<dyn Error>> {
    for mode in [
        Mode::LocationCrossPort,
        Mode::LocationUserinfo,
        Mode::LocationFragment,
    ] {
        let fixture = Fixture::start(mode).await?;
        assert_eq!(
            fixture
                .publisher()?
                .publish_file(&file_image()?)
                .await
                .err(),
            Some(OciRegistryError::Rejected)
        );
        assert!(
            !fixture
                .state
                .events
                .lock()
                .await
                .iter()
                .any(|e| e.stage == "blob_put")
        );
    }
    Ok(())
}

#[tokio::test]
async fn https_file_stream_is_not_replayed_after_denial_or_failed_start()
-> Result<(), Box<dyn Error>> {
    for mode in [Mode::PutDenied, Mode::StartUnavailable] {
        let fixture = Fixture::start(mode).await?;
        let error = if matches!(mode, Mode::PutDenied) {
            OciRegistryError::Denied
        } else {
            OciRegistryError::Rejected
        };
        assert_eq!(
            fixture
                .publisher()?
                .publish_file(&file_image()?)
                .await
                .err(),
            Some(error)
        );
        let events = fixture.state.events.lock().await;
        assert_eq!(events.iter().filter(|e| e.stage == "start").count(), 1);
        assert_eq!(
            events.iter().filter(|e| e.stage == "blob_put").count(),
            usize::from(matches!(mode, Mode::PutDenied))
        );
        assert_eq!(events.iter().filter(|e| e.stage == "token").count(), 1);
        assert!(!events.iter().any(|e| e.stage == "manifest_put"));
    }
    Ok(())
}

#[tokio::test]
async fn https_expired_cache_refreshes_before_opening_each_file_stream()
-> Result<(), Box<dyn Error>> {
    let fixture = Fixture::start(Mode::ExpireBeforePut).await?;
    let image = file_image()?;
    assert_eq!(
        fixture.publisher()?.publish_file(&image).await?,
        image.manifest_digest
    );
    let events = fixture.state.events.lock().await;
    assert_eq!(events.iter().filter(|e| e.stage == "start").count(), 2);
    assert_eq!(events.iter().filter(|e| e.stage == "blob_put").count(), 2);
    assert!(
        events
            .iter()
            .filter(|e| e.stage == "blob_put")
            .all(|e| e.authorized)
    );
    assert_eq!(events.iter().filter(|e| e.stage == "token").count(), 3);
    Ok(())
}

#[tokio::test]
async fn https_file_put_outlives_metadata_timeout_but_operation_deadline_stops_publication()
-> Result<(), Box<dyn Error>> {
    let fixture = Fixture::start(Mode::SlowPut).await?;
    let image = file_image()?;
    assert_eq!(
        fixture.publisher()?.publish_file(&image).await?,
        image.manifest_digest
    );
    assert_eq!(
        fixture
            .state
            .events
            .lock()
            .await
            .iter()
            .filter(|e| e.stage == "blob_put")
            .count(),
        2
    );

    let cancelled = Fixture::start(Mode::SlowPut).await?;
    let publisher = cancelled.publisher()?;
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(250),
            publisher.publish_file(&image)
        )
        .await
        .is_err()
    );
    tokio::time::sleep(std::time::Duration::from_millis(450)).await;
    let events = cancelled.state.events.lock().await;
    assert_eq!(events.iter().filter(|e| e.stage == "blob_put").count(), 1);
    assert!(
        !events
            .iter()
            .any(|e| matches!(e.stage, "manifest_put" | "manifest_get"))
    );
    assert!(cancelled.state.manifest.lock().await.is_none());
    Ok(())
}
