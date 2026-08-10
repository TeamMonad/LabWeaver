//! Structured, JSON-only telemetry and correlation for service processes.

use std::fmt;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use axum::extract::{MatchedPath, Request, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::{Json, Router, middleware};
use opentelemetry::Context;
use opentelemetry::propagation::{Extractor, Injector, TextMapPropagator};
use opentelemetry::trace::{SpanContext, SpanId, TraceContextExt, TraceFlags, TraceId, TraceState};
use opentelemetry_sdk::propagation::TraceContextPropagator;
use thiserror::Error;
use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::FmtContext;
use tracing_subscriber::fmt::format::{FormatEvent, FormatFields, Writer};
use tracing_subscriber::registry::LookupSpan;
use uuid::Uuid;

pub use metrics_exporter_prometheus::PrometheusHandle;

/// Stable schema identifier carried by every boundary event emitted here.
pub const LOG_SCHEMA: &str = "labweaver.log.v1";
/// Portable request correlation header.
pub const REQUEST_ID_HEADER: &str = "x-request-id";
/// W3C distributed trace context header.
pub const TRACEPARENT_HEADER: &str = "traceparent";

tokio::task_local! {
    static ACTIVE_REQUEST_CONTEXT: RequestContext;
}

/// Telemetry initialization failure.
#[derive(Debug, Error)]
pub enum TelemetryError {
    /// The log filter is invalid.
    #[error("LW_CONFIG_LOG_FILTER_INVALID: {0}")]
    InvalidFilter(String),
    /// A global subscriber was already installed.
    #[error("LW_TELEMETRY_ALREADY_INITIALIZED: {0}")]
    AlreadyInitialized(String),
    /// A process-global metrics recorder was already installed or invalid.
    #[error("LW_TELEMETRY_METRICS_INITIALIZATION_FAILED: {0}")]
    Metrics(String),
}

/// Validated request and distributed trace identities.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RequestContext {
    request_id: String,
    trace_id: String,
    traceparent: String,
}

/// Returns the validated correlation context while an instrumented HTTP handler is executing.
#[must_use]
pub fn current_request_context() -> Option<RequestContext> {
    ACTIVE_REQUEST_CONTEXT.try_with(Clone::clone).ok()
}

impl RequestContext {
    /// Generates a fresh request identity and valid W3C trace context for a new boundary.
    #[must_use]
    pub fn generate() -> Self {
        let request_id = Uuid::now_v7().to_string();
        let (context, traceparent) = generated_trace_context();
        Self {
            request_id,
            trace_id: context.span().span_context().trace_id().to_string(),
            traceparent,
        }
    }

    /// Returns the portable request identity.
    #[must_use]
    pub fn request_id(&self) -> &str {
        &self.request_id
    }

    /// Returns the 32-character W3C trace identity.
    #[must_use]
    pub fn trace_id(&self) -> &str {
        &self.trace_id
    }

    /// Returns the validated W3C traceparent value.
    #[must_use]
    pub fn traceparent(&self) -> &str {
        &self.traceparent
    }

    /// Adds the validated identities to an outbound HTTP request.
    ///
    /// # Errors
    ///
    /// Returns a stable correlation-header error if a validated identity cannot be encoded.
    pub fn inject_headers(&self, headers: &mut HeaderMap) -> Result<(), CorrelationHeaderError> {
        insert_header(headers, REQUEST_ID_HEADER, &self.request_id)?;
        let mut source = HeaderMap::new();
        insert_header(&mut source, TRACEPARENT_HEADER, &self.traceparent)?;
        let context = TraceContextPropagator::new().extract(&HeaderExtractor(&source));
        let mut injector = FallibleHeaderInjector {
            headers,
            error: None,
        };
        TraceContextPropagator::new().inject_context(&context, &mut injector);
        injector.error.map_or(Ok(()), Err)
    }
}

/// A locally generated or previously validated correlation value could not be encoded.
#[derive(Debug, Error)]
pub enum CorrelationHeaderError {
    /// A propagated header name was invalid.
    #[error("LW_HTTP_CORRELATION_HEADER_NAME_INVALID")]
    InvalidName,
    /// A propagated header value was invalid.
    #[error("LW_HTTP_CORRELATION_HEADER_VALUE_INVALID")]
    InvalidValue,
}

#[derive(Clone, Copy)]
struct HttpLogConfig {
    service: &'static str,
    component: &'static str,
}

/// Installs a JSON subscriber without logging request bodies or secrets.
///
/// # Errors
///
/// Returns a stable configuration or initialization error when the filter is invalid or another
/// global subscriber has already been installed.
pub fn init(service: &'static str) -> Result<(), TelemetryError> {
    let filter = std::env::var("LABWEAVER_LOG_FILTER").unwrap_or_else(|_| "info".to_owned());
    let filter = EnvFilter::try_new(filter)
        .map_err(|error| TelemetryError::InvalidFilter(error.to_string()))?;
    tracing_subscriber::fmt()
        .event_format(SafeJsonFormatter { service })
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init()
        .map_err(|error| TelemetryError::AlreadyInitialized(format!("{service}: {error}")))
}

#[derive(Clone, Copy)]
struct SafeJsonFormatter {
    service: &'static str,
}

impl<S, N> FormatEvent<S, N> for SafeJsonFormatter
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
    N: for<'writer> FormatFields<'writer> + 'static,
{
    fn format_event(
        &self,
        _context: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> fmt::Result {
        let metadata = event.metadata();
        let mut visitor = SafeEventVisitor::default();
        event.record(&mut visitor);
        let event_name = visitor
            .fields
            .get("event")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("legacy.unclassified")
            .to_owned();
        let default_outcome = match *metadata.level() {
            tracing::Level::ERROR => "failed",
            tracing::Level::WARN => "warning",
            _ => "observed",
        };
        let _ = ACTIVE_REQUEST_CONTEXT.try_with(|context| {
            visitor
                .fields
                .entry("request_id".to_owned())
                .or_insert_with(|| serde_json::Value::from(context.request_id()));
            visitor
                .fields
                .entry("trace_id".to_owned())
                .or_insert_with(|| serde_json::Value::from(context.trace_id()));
        });
        visitor.fields.insert(
            "timestamp_unix_ms".to_owned(),
            serde_json::Value::from(unix_timestamp_millis()),
        );
        visitor.fields.insert(
            "level".to_owned(),
            serde_json::Value::from(metadata.level().as_str()),
        );
        visitor
            .fields
            .insert("schema".to_owned(), serde_json::Value::from(LOG_SCHEMA));
        visitor
            .fields
            .insert("service".to_owned(), serde_json::Value::from(self.service));
        visitor
            .fields
            .entry("event".to_owned())
            .or_insert_with(|| serde_json::Value::from("legacy.unclassified"));
        visitor
            .fields
            .entry("component".to_owned())
            .or_insert_with(|| serde_json::Value::from(metadata.target()));
        visitor
            .fields
            .entry("operation".to_owned())
            .or_insert_with(|| serde_json::Value::from(event_name));
        visitor
            .fields
            .entry("outcome".to_owned())
            .or_insert_with(|| serde_json::Value::from(default_outcome));
        visitor
            .fields
            .entry("duration_ms".to_owned())
            .or_insert_with(|| serde_json::Value::from(0_u64));
        let encoded = serde_json::to_string(&visitor.fields).map_err(|_| fmt::Error)?;
        writeln!(writer, "{encoded}")
    }
}

#[derive(Default)]
struct SafeEventVisitor {
    fields: serde_json::Map<String, serde_json::Value>,
}

impl SafeEventVisitor {
    fn record_value(&mut self, field: &Field, value: serde_json::Value) {
        let name = field.name();
        if safe_log_field(name) {
            self.fields.insert(name.to_owned(), value);
        } else if sensitive_log_field(name) {
            self.fields.insert(
                name.to_owned(),
                serde_json::Value::from("redacted_unclassified"),
            );
        }
    }
}

impl Visit for SafeEventVisitor {
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.record_value(field, serde_json::Value::from(value));
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.record_value(field, serde_json::Value::from(value));
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.record_value(field, serde_json::Value::from(value));
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        let value = if field.name() == "safe_detail" && !valid_safe_detail(value) {
            "redacted_unclassified"
        } else {
            value
        };
        self.record_value(field, serde_json::Value::from(value));
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.record_value(field, serde_json::Value::from(format!("{value:?}")));
    }
}

fn safe_log_field(name: &str) -> bool {
    matches!(
        name,
        "event"
            | "component"
            | "operation"
            | "outcome"
            | "duration_ms"
            | "request_id"
            | "trace_id"
            | "actor_id"
            | "course_id"
            | "project_id"
            | "run_id"
            | "environment_id"
            | "resource_id"
            | "build_request_id"
            | "frozen_submission_id"
            | "submission_id"
            | "release_id"
            | "candidate_id"
            | "approval_id"
            | "artifact_id"
            | "upload_id"
            | "endpoint_grant_id"
            | "lease_id"
            | "session_id"
            | "connection_id"
            | "operation_id"
            | "event_id"
            | "message_id"
            | "revision"
            | "attempt"
            | "delivery_attempt"
            | "provider_step"
            | "binding"
            | "provider"
            | "stream"
            | "consumer"
            | "subject"
            | "diagnostic_code"
            | "error_kind"
            | "failure_stage"
            | "safe_detail"
            | "retryable"
            | "http_method"
            | "http_status"
            | "route"
            | "state"
            | "from_state"
            | "to_state"
            | "status"
            | "kind"
            | "phase"
            | "action"
            | "worker"
            | "executor"
            | "cleanup"
            | "sequence"
            | "count"
            | "items"
    )
}

fn sensitive_log_field(name: &str) -> bool {
    matches!(
        name,
        "error"
            | "diagnostic"
            | "reason"
            | "detail"
            | "path"
            | "url"
            | "endpoint"
            | "object_key"
            | "locator"
            | "token"
            | "secret"
            | "payload"
            | "body"
            | "request"
            | "response"
            | "command"
            | "transcript"
            | "peer_address"
            | "address"
    )
}

fn valid_safe_detail(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-' | b'.')
        })
}

fn unix_timestamp_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
}

/// Applies the common fail-closed HTTP identity and completion logging boundary.
pub fn instrument_http(router: Router, service: &'static str, component: &'static str) -> Router {
    router.layer(middleware::from_fn_with_state(
        HttpLogConfig { service, component },
        request_context_middleware,
    ))
}

async fn request_context_middleware(
    State(config): State<HttpLogConfig>,
    mut request: Request,
    next: Next,
) -> Response {
    let started = Instant::now();
    let method = request.method().clone();
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map_or("unmatched", MatchedPath::as_str)
        .to_owned();
    let context = match parse_request_context(request.headers()) {
        Ok(context) => context,
        Err(failure) => {
            tracing::warn!(
                schema = LOG_SCHEMA,
                event = "http.request.rejected",
                service = config.service,
                component = config.component,
                operation = "http.request",
                outcome = "rejected",
                duration_ms = elapsed_millis(started),
                diagnostic_code = failure.diagnostic_code,
                failure_stage = failure.failure_stage,
                error_kind = failure.error_kind,
                retryable = false,
                http_method = %method,
                route = %route,
            );
            return failure.into_response();
        }
    };
    if context.inject_headers(request.headers_mut()).is_err() {
        return internal_correlation_failure(config, &context, started).into_response();
    }
    request.extensions_mut().insert(context.clone());
    tracing::debug!(
        schema = LOG_SCHEMA,
        event = "http.request.started",
        service = config.service,
        component = config.component,
        operation = "http.request",
        outcome = "started",
        duration_ms = 0_u64,
        request_id = context.request_id(),
        trace_id = context.trace_id(),
        http_method = %method,
        route = %route,
    );
    let mut response = ACTIVE_REQUEST_CONTEXT
        .scope(context.clone(), next.run(request))
        .await;
    let status = response.status();
    let outcome = if status.is_success() || status.is_redirection() {
        "succeeded"
    } else if status.is_client_error() {
        "rejected"
    } else {
        "failed"
    };
    tracing::info!(
        schema = LOG_SCHEMA,
        event = "http.request.completed",
        service = config.service,
        component = config.component,
        operation = "http.request",
        outcome,
        duration_ms = elapsed_millis(started),
        request_id = context.request_id(),
        trace_id = context.trace_id(),
        http_method = %method,
        route = %route,
        http_status = status.as_u16(),
    );
    if insert_header(
        response.headers_mut(),
        REQUEST_ID_HEADER,
        context.request_id(),
    )
    .is_err()
        || insert_header(
            response.headers_mut(),
            TRACEPARENT_HEADER,
            context.traceparent(),
        )
        .is_err()
    {
        return internal_correlation_failure(config, &context, started).into_response();
    }
    response
}

fn internal_correlation_failure(
    config: HttpLogConfig,
    context: &RequestContext,
    started: Instant,
) -> StatusCode {
    tracing::error!(
        schema = LOG_SCHEMA,
        event = "http.correlation_encoding.failed",
        service = config.service,
        component = config.component,
        operation = "http.request_context",
        outcome = "failed",
        duration_ms = elapsed_millis(started),
        request_id = context.request_id(),
        trace_id = context.trace_id(),
        diagnostic_code = "LW_HTTP_CORRELATION_ENCODING_FAILED",
        failure_stage = "http.request_context.encode",
        error_kind = "invalid_internal_correlation_identity",
        retryable = false,
        safe_detail = "redacted_unclassified",
    );
    StatusCode::INTERNAL_SERVER_ERROR
}

struct CorrelationFailure {
    diagnostic_code: &'static str,
    failure_stage: &'static str,
    error_kind: &'static str,
    request_id: String,
}

impl CorrelationFailure {
    fn into_response(self) -> Response {
        let response_request_id = self.request_id.clone();
        let detail = match self.diagnostic_code {
            "LW_HTTP_REQUEST_ID_INVALID" => "x-request-id must be 8-128 portable ASCII characters.",
            _ => "traceparent must contain a valid W3C Trace Context value.",
        };
        let mut response = (
            StatusCode::BAD_REQUEST,
            Json(contracts::ProblemDetails {
                problem_type: "urn:labweaver:problem:invalid-correlation-context".to_owned(),
                title: "Invalid correlation context".to_owned(),
                status: StatusCode::BAD_REQUEST.as_u16(),
                detail: detail.to_owned(),
                instance: String::new(),
                diagnostic_code: contracts::DiagnosticCode::registered(self.diagnostic_code),
                request_id: self.request_id,
                trace_id: None,
                retryable: false,
                violations: Vec::new(),
            }),
        )
            .into_response();
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/problem+json"),
        );
        if insert_header(
            response.headers_mut(),
            REQUEST_ID_HEADER,
            &response_request_id,
        )
        .is_err()
        {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
        response
    }
}

fn parse_request_context(headers: &HeaderMap) -> Result<RequestContext, CorrelationFailure> {
    let generated_request_id = Uuid::now_v7().to_string();
    if headers.get_all(REQUEST_ID_HEADER).iter().count() > 1 {
        return Err(CorrelationFailure {
            diagnostic_code: "LW_HTTP_REQUEST_ID_INVALID",
            failure_stage: "http.request_context.request_id",
            error_kind: "duplicate_request_id",
            request_id: generated_request_id,
        });
    }
    let request_id = match headers.get(REQUEST_ID_HEADER) {
        Some(value) => value.to_str().ok().filter(|value| valid_request_id(value)),
        None => Some(generated_request_id.as_str()),
    }
    .ok_or_else(|| CorrelationFailure {
        diagnostic_code: "LW_HTTP_REQUEST_ID_INVALID",
        failure_stage: "http.request_context.request_id",
        error_kind: "invalid_request_id",
        request_id: generated_request_id.clone(),
    })?
    .to_owned();

    if headers.get_all(TRACEPARENT_HEADER).iter().count() > 1 {
        return Err(CorrelationFailure {
            diagnostic_code: "LW_HTTP_TRACE_CONTEXT_INVALID",
            failure_stage: "http.request_context.traceparent",
            error_kind: "duplicate_trace_context",
            request_id,
        });
    }
    let (context, traceparent) = if let Some(value) = headers.get(TRACEPARENT_HEADER) {
        let value = value.to_str().map_err(|_| CorrelationFailure {
            diagnostic_code: "LW_HTTP_TRACE_CONTEXT_INVALID",
            failure_stage: "http.request_context.traceparent",
            error_kind: "invalid_trace_context",
            request_id: request_id.clone(),
        })?;
        let propagator = TraceContextPropagator::new();
        let context = propagator.extract(&HeaderExtractor(headers));
        let span = context.span();
        if !span.span_context().is_valid() {
            return Err(CorrelationFailure {
                diagnostic_code: "LW_HTTP_TRACE_CONTEXT_INVALID",
                failure_stage: "http.request_context.traceparent",
                error_kind: "invalid_trace_context",
                request_id,
            });
        }
        (context, value.to_owned())
    } else {
        generated_trace_context()
    };
    let span = context.span();
    let trace_id = span.span_context().trace_id().to_string();
    Ok(RequestContext {
        request_id,
        trace_id,
        traceparent,
    })
}

fn generated_trace_context() -> (Context, String) {
    let trace_id = TraceId::from_bytes(*Uuid::now_v7().as_bytes());
    let source = *Uuid::now_v7().as_bytes();
    let mut span_bytes = [0_u8; 8];
    span_bytes.copy_from_slice(&source[8..]);
    let span_context = SpanContext::new(
        trace_id,
        SpanId::from_bytes(span_bytes),
        TraceFlags::SAMPLED,
        false,
        TraceState::default(),
    );
    let traceparent = format!(
        "00-{}-{}-01",
        span_context.trace_id(),
        span_context.span_id()
    );
    let context = Context::new().with_remote_span_context(span_context);
    (context, traceparent)
}

fn valid_request_id(value: &str) -> bool {
    (8..=128).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.:".contains(&byte))
}

fn elapsed_millis(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn insert_header(
    headers: &mut HeaderMap,
    name: &'static str,
    value: &str,
) -> Result<(), CorrelationHeaderError> {
    let value = HeaderValue::from_str(value).map_err(|_| CorrelationHeaderError::InvalidValue)?;
    headers.insert(HeaderName::from_static(name), value);
    Ok(())
}

struct FallibleHeaderInjector<'a> {
    headers: &'a mut HeaderMap,
    error: Option<CorrelationHeaderError>,
}

impl Injector for FallibleHeaderInjector<'_> {
    fn set(&mut self, key: &str, value: String) {
        if self.error.is_some() {
            return;
        }
        let Ok(name) = HeaderName::from_bytes(key.as_bytes()) else {
            self.error = Some(CorrelationHeaderError::InvalidName);
            return;
        };
        let Ok(value) = HeaderValue::from_str(&value) else {
            self.error = Some(CorrelationHeaderError::InvalidValue);
            return;
        };
        self.headers.insert(name, value);
    }
}

struct HeaderExtractor<'a>(&'a HeaderMap);

impl Extractor for HeaderExtractor<'_> {
    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(|value| value.to_str().ok())
    }

    fn keys(&self) -> Vec<&str> {
        self.0.keys().map(HeaderName::as_str).collect()
    }
}

/// Installs the process-wide Prometheus recorder for a service.
///
/// # Errors
///
/// Returns [`TelemetryError`] when another recorder is already installed or
/// the exporter cannot be initialized.
pub fn init_metrics(service: &'static str) -> Result<PrometheusHandle, TelemetryError> {
    metrics_exporter_prometheus::PrometheusBuilder::new()
        .with_recommended_naming(true)
        .add_global_label("service", service)
        .install_recorder()
        .map_err(|error| TelemetryError::Metrics(error.to_string()))
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::sync::{Arc, Mutex};

    use axum::body::{Body, to_bytes};
    use axum::extract::Extension;
    use axum::http::{Request, StatusCode};
    use axum::routing::get;
    use tower::ServiceExt;
    use tracing_subscriber::fmt::MakeWriter;

    use super::{RequestContext, SafeJsonFormatter, instrument_http};

    #[derive(Clone)]
    struct SharedWriter(Arc<Mutex<Vec<u8>>>);

    struct BufferWriter(Arc<Mutex<Vec<u8>>>);

    impl Write for BufferWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0
                .lock()
                .map_err(|_| std::io::Error::other("poisoned"))?
                .extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'writer> MakeWriter<'writer> for SharedWriter {
        type Writer = BufferWriter;

        fn make_writer(&'writer self) -> Self::Writer {
            BufferWriter(Arc::clone(&self.0))
        }
    }

    #[test]
    fn formatter_enforces_schema_and_redacts_protected_values()
    -> Result<(), Box<dyn std::error::Error>> {
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::fmt()
            .event_format(SafeJsonFormatter {
                service: "telemetry-test",
            })
            .with_max_level(tracing::Level::INFO)
            .with_writer(SharedWriter(Arc::clone(&bytes)))
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(
                event = "telemetry.privacy.probe",
                component = "formatter-test",
                operation = "telemetry.format",
                outcome = "succeeded",
                duration_ms = 7_u64,
                trace_id = "01900000000070008000000000000001",
                token = "TOKEN_SENTINEL",
                path = "/PRIVATE/PATH_SENTINEL",
                url = "https://URL_SENTINEL.invalid",
                object_key = "OBJECT_KEY_SENTINEL",
                locator = "LOCATOR_SENTINEL",
                payload = "PAYLOAD_SENTINEL",
                command = "COMMAND_SENTINEL",
                error = "ERROR_SENTINEL",
            );
            tracing::debug!(event = "telemetry.debug.must_not_appear");
        });
        let output = String::from_utf8(bytes.lock().map_err(|_| "poisoned")?.clone())?;
        for sentinel in [
            "TOKEN_SENTINEL",
            "PATH_SENTINEL",
            "URL_SENTINEL",
            "OBJECT_KEY_SENTINEL",
            "LOCATOR_SENTINEL",
            "PAYLOAD_SENTINEL",
            "COMMAND_SENTINEL",
            "ERROR_SENTINEL",
        ] {
            assert!(
                !output.contains(sentinel),
                "protected value leaked: {sentinel}"
            );
        }
        assert!(!output.contains("telemetry.debug.must_not_appear"));
        let event: serde_json::Value = serde_json::from_str(output.trim())?;
        let schema: serde_json::Value = serde_json::from_str(include_str!(
            "../../../schemas/contracts/v1/internal/labweaver-log-v1.schema.json"
        ))?;
        let validator = jsonschema::validator_for(&schema)?;
        if let Err(error) = validator.validate(&event) {
            return Err(error.to_string().into());
        }
        assert_eq!(event["service"], "telemetry-test");
        assert_eq!(event["token"], "redacted_unclassified");
        Ok(())
    }

    #[tokio::test]
    async fn missing_identities_are_generated_and_returned()
    -> Result<(), Box<dyn std::error::Error>> {
        let app = instrument_http(
            axum::Router::new().route(
                "/probe",
                get(|Extension(context): Extension<RequestContext>| async move {
                    context.trace_id().to_owned()
                }),
            ),
            "telemetry-test",
            "http",
        );
        let response = app
            .oneshot(Request::builder().uri("/probe").body(Body::empty())?)
            .await?;
        assert_eq!(response.status(), StatusCode::OK);
        let request_id = response
            .headers()
            .get("x-request-id")
            .and_then(|value| value.to_str().ok())
            .ok_or("missing request id")?;
        assert_eq!(uuid::Uuid::parse_str(request_id)?.get_version_num(), 7);
        assert!(response.headers().contains_key("traceparent"));
        let body = to_bytes(response.into_body(), 128).await?;
        assert_eq!(body.len(), 32);
        Ok(())
    }

    #[tokio::test]
    async fn valid_w3c_context_is_extracted_and_injected() -> Result<(), Box<dyn std::error::Error>>
    {
        const TRACEPARENT: &str = "00-01900000000070008000000000000001-0190000000007001-01";
        let app = instrument_http(
            axum::Router::new().route(
                "/probe",
                get(|Extension(context): Extension<RequestContext>| async move {
                    context.trace_id().to_owned()
                }),
            ),
            "telemetry-test",
            "http",
        );
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/probe")
                    .header("traceparent", TRACEPARENT)
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("traceparent")
                .and_then(|value| value.to_str().ok()),
            Some(TRACEPARENT)
        );
        let body = to_bytes(response.into_body(), 128).await?;
        assert_eq!(&body[..], b"01900000000070008000000000000001");
        Ok(())
    }

    #[tokio::test]
    async fn malformed_traceparent_fails_before_the_handler()
    -> Result<(), Box<dyn std::error::Error>> {
        let app = instrument_http(
            axum::Router::new().route("/probe", get(|| async { StatusCode::NO_CONTENT })),
            "telemetry-test",
            "http",
        );
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/probe")
                    .header("traceparent", "not-a-trace")
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            response
                .headers()
                .get("content-type")
                .and_then(|value| value.to_str().ok()),
            Some("application/problem+json")
        );
        let body = to_bytes(response.into_body(), 4096).await?;
        let problem: serde_json::Value = serde_json::from_slice(&body)?;
        assert_eq!(problem["diagnosticCode"], "LW_HTTP_TRACE_CONTEXT_INVALID");
        Ok(())
    }

    #[tokio::test]
    async fn malformed_request_id_fails_closed() -> Result<(), Box<dyn std::error::Error>> {
        let app = instrument_http(
            axum::Router::new().route("/probe", get(|| async { StatusCode::NO_CONTENT })),
            "telemetry-test",
            "http",
        );
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/probe")
                    .header("x-request-id", "bad id")
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = to_bytes(response.into_body(), 4096).await?;
        let problem: serde_json::Value = serde_json::from_slice(&body)?;
        assert_eq!(problem["diagnosticCode"], "LW_HTTP_REQUEST_ID_INVALID");
        Ok(())
    }

    #[tokio::test]
    async fn duplicate_correlation_headers_fail_closed() -> Result<(), Box<dyn std::error::Error>> {
        for (name, value, expected) in [
            (
                "x-request-id",
                "01900000-0000-7000-8000-000000000001",
                "LW_HTTP_REQUEST_ID_INVALID",
            ),
            (
                "traceparent",
                "00-01900000000070008000000000000001-0190000000007001-01",
                "LW_HTTP_TRACE_CONTEXT_INVALID",
            ),
        ] {
            let app = instrument_http(
                axum::Router::new().route("/probe", get(|| async { StatusCode::NO_CONTENT })),
                "telemetry-test",
                "http",
            );
            let response = app
                .oneshot(
                    Request::builder()
                        .uri("/probe")
                        .header(name, value)
                        .header(name, value)
                        .body(Body::empty())?,
                )
                .await?;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            let body = to_bytes(response.into_body(), 4096).await?;
            let problem: serde_json::Value = serde_json::from_slice(&body)?;
            assert_eq!(problem["diagnosticCode"], expected);
        }
        Ok(())
    }
}
