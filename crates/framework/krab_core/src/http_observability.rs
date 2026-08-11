use std::sync::atomic::Ordering;
use std::time::Instant;

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderValue, Request};
use axum::middleware::Next;
use axum::response::Response;
use tracing::{debug, info};

use crate::http_protocol::{
    operation_label, protocol_label, protocol_metric_index, resolved_protocol,
    response_metric_slot, response_status_class_index, selection_source_label,
};
use crate::http_runtime::HasRuntimeState;

pub(crate) fn request_id_value_from_headers(
    headers: &axum::http::HeaderMap,
) -> (HeaderValue, &'static str) {
    match headers.get("x-request-id").cloned() {
        Some(existing) => (existing, "inbound"),
        None => {
            let generated = uuid::Uuid::new_v4().to_string();
            let value = HeaderValue::from_str(&generated)
                .unwrap_or_else(|_| HeaderValue::from_static("request-id-invalid"));
            (value, "uuid_v4")
        }
    }
}

pub async fn request_id_middleware<S>(
    _state: State<S>,
    mut req: Request<Body>,
    next: Next,
) -> Response
where
    S: Clone + Send + Sync + 'static + HasRuntimeState,
{
    let (request_id_value, strategy) = request_id_value_from_headers(req.headers());
    req.headers_mut()
        .insert("x-request-id", request_id_value.clone());

    let mut response = next.run(req).await;

    response
        .headers_mut()
        .insert("x-request-id", request_id_value.clone());

    debug!(
        request_id = %request_id_value.to_str().unwrap_or("non-utf8"),
        strategy = %strategy,
        "request_id_attached"
    );

    response
}

pub async fn metrics_middleware<S>(
    State(state): State<S>,
    req: Request<Body>,
    next: Next,
) -> Response
where
    S: Clone + Send + Sync + 'static + HasRuntimeState,
{
    let runtime = state.runtime_state();
    let protocol_index = protocol_metric_index(resolved_protocol(&req));

    runtime.request_count.fetch_add(1, Ordering::Relaxed);
    runtime.protocol_request_totals[protocol_index].fetch_add(1, Ordering::Relaxed);
    runtime.inflight_requests.fetch_add(1, Ordering::Relaxed);

    // Decrement in a drop guard, not after the await: when the client
    // disconnects mid-request hyper drops this future, and a plain
    // `fetch_sub` after `next.run` would never execute — the gauge (exported
    // to Prometheus) would drift upward permanently.
    struct InflightGuard(std::sync::Arc<std::sync::atomic::AtomicU64>);
    impl Drop for InflightGuard {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::Relaxed);
        }
    }
    let _inflight = InflightGuard(runtime.inflight_requests.clone());

    let response = next.run(req).await;

    let code = response.status().as_u16();
    if let Some(class_index) = response_status_class_index(code) {
        match class_index {
            0 => {
                runtime.response_2xx_total.fetch_add(1, Ordering::Relaxed);
            }
            1 => {
                runtime.response_4xx_total.fetch_add(1, Ordering::Relaxed);
            }
            2 => {
                runtime.response_5xx_total.fetch_add(1, Ordering::Relaxed);
            }
            _ => {}
        }

        let slot = response_metric_slot(class_index, protocol_index);
        runtime.response_class_protocol_totals[slot].fetch_add(1, Ordering::Relaxed);
    }

    response
}

pub async fn tracing_middleware<S>(
    State(state): State<S>,
    req: Request<Body>,
    next: Next,
) -> Response
where
    S: Clone + Send + Sync + 'static + HasRuntimeState,
{
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let protocol = protocol_label(&req);
    let operation = operation_label(&method, &path);
    let selection_source = selection_source_label(&req, &path);
    let request_id = req
        .headers()
        .get("x-request-id")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("unknown")
        .to_string();
    if request_id == "unknown" {
        debug!("tracing_missing_request_id_in_inbound_headers");
    }

    let start = Instant::now();
    let resp = next.run(req).await;
    let status = resp.status();
    let elapsed = start.elapsed();
    let elapsed_ms = elapsed.as_millis();

    let buckets = &state.runtime_state().latency_buckets;
    if elapsed_ms <= 10 {
        buckets[0].fetch_add(1, Ordering::Relaxed);
    } else if elapsed_ms <= 50 {
        buckets[1].fetch_add(1, Ordering::Relaxed);
    } else if elapsed_ms <= 100 {
        buckets[2].fetch_add(1, Ordering::Relaxed);
    } else if elapsed_ms <= 200 {
        buckets[3].fetch_add(1, Ordering::Relaxed);
    } else if elapsed_ms <= 500 {
        buckets[4].fetch_add(1, Ordering::Relaxed);
    } else if elapsed_ms <= 1000 {
        buckets[5].fetch_add(1, Ordering::Relaxed);
    } else if elapsed_ms <= 2000 {
        buckets[6].fetch_add(1, Ordering::Relaxed);
    } else {
        buckets[7].fetch_add(1, Ordering::Relaxed);
    }

    info!(
        event = "http_request_complete",
        http.method = %method,
        http.route = %path,
        http.status_code = %status.as_u16(),
        http.request_id = %request_id,
        krab.protocol = %protocol,
        krab.operation = %operation,
        krab.selection_source = %selection_source,
        duration_ms = elapsed_ms,
        "request_complete"
    );
    resp
}

/// Headers that must be forwarded on every outbound service-to-service call
/// to maintain request-id and trace correlation across the topology.
///
/// # Usage
///
/// ```rust
/// use krab_core::http::PropagationHeaders;
///
/// // Inside an Axum handler, extract the inbound headers:
/// let mut headers = axum::http::HeaderMap::new();
/// headers.insert("x-request-id", "req-123".parse().unwrap());
/// let prop = PropagationHeaders::from_request_headers(&headers);
///
/// assert_eq!(prop.request_id.as_deref(), Some("req-123"));
///
/// // Then inject them into every outbound request. `as_header_pairs` suits
/// // builder-style clients:
/// let client = reqwest::Client::new();
/// let mut builder = client.get("http://service_users:3002/api/v1/graphql");
/// for (name, value) in prop.as_header_pairs() {
///     builder = builder.header(name, value);
/// }
/// # let _ = builder;
///
/// // `inject_into_headers` suits anything holding a `HeaderMap` directly:
/// let mut outbound = axum::http::HeaderMap::new();
/// prop.inject_into_headers(&mut outbound);
/// assert_eq!(outbound.get("x-request-id").unwrap(), "req-123");
/// ```
#[derive(Debug, Clone, Default)]
pub struct PropagationHeaders {
    pub request_id: Option<String>,
    pub trace_id: Option<String>,
}

impl PropagationHeaders {
    pub const REQUEST_ID_HEADER: &'static str = "x-request-id";
    pub const TRACE_ID_HEADER: &'static str = "x-trace-id";

    pub fn from_request_headers(headers: &axum::http::HeaderMap) -> Self {
        Self {
            request_id: headers
                .get(Self::REQUEST_ID_HEADER)
                .and_then(|v| v.to_str().ok())
                .map(ToString::to_string),
            trace_id: headers
                .get(Self::TRACE_ID_HEADER)
                .and_then(|v| v.to_str().ok())
                .map(ToString::to_string),
        }
    }

    pub fn as_header_pairs(&self) -> Vec<(&'static str, &str)> {
        let mut pairs = Vec::new();
        if let Some(id) = &self.request_id {
            pairs.push((Self::REQUEST_ID_HEADER, id.as_str()));
        }
        if let Some(tid) = &self.trace_id {
            pairs.push((Self::TRACE_ID_HEADER, tid.as_str()));
        }
        pairs
    }

    pub fn inject_into_headers(&self, headers: &mut axum::http::HeaderMap) {
        if let Some(id) = &self.request_id {
            if let Ok(v) = axum::http::HeaderValue::from_str(id) {
                headers.insert(
                    axum::http::HeaderName::from_static(Self::REQUEST_ID_HEADER),
                    v,
                );
            }
        }
        if let Some(tid) = &self.trace_id {
            if let Ok(v) = axum::http::HeaderValue::from_str(tid) {
                headers.insert(
                    axum::http::HeaderName::from_static(Self::TRACE_ID_HEADER),
                    v,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn propagation_headers_round_trip() {
        let mut map = axum::http::HeaderMap::new();
        map.insert(
            axum::http::HeaderName::from_static("x-request-id"),
            axum::http::HeaderValue::from_static("req-abc"),
        );
        map.insert(
            axum::http::HeaderName::from_static("x-trace-id"),
            axum::http::HeaderValue::from_static("trace-xyz"),
        );

        let prop = PropagationHeaders::from_request_headers(&map);
        assert_eq!(prop.request_id.as_deref(), Some("req-abc"));
        assert_eq!(prop.trace_id.as_deref(), Some("trace-xyz"));

        let mut out = axum::http::HeaderMap::new();
        prop.inject_into_headers(&mut out);
        assert_eq!(
            out.get("x-request-id").and_then(|v| v.to_str().ok()),
            Some("req-abc")
        );
        assert_eq!(
            out.get("x-trace-id").and_then(|v| v.to_str().ok()),
            Some("trace-xyz")
        );
    }

    #[test]
    fn propagation_headers_missing_fields_produce_empty_pairs() {
        let map = axum::http::HeaderMap::new();
        let prop = PropagationHeaders::from_request_headers(&map);
        assert!(prop.request_id.is_none());
        assert!(prop.trace_id.is_none());
        assert!(prop.as_header_pairs().is_empty());
    }

    #[test]
    fn request_id_generation_preserves_inbound_header_when_present() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            axum::http::HeaderName::from_static("x-request-id"),
            axum::http::HeaderValue::from_static("req-123"),
        );

        let (value, strategy) = request_id_value_from_headers(&headers);
        assert_eq!(strategy, "inbound");
        assert_eq!(value.to_str().ok(), Some("req-123"));
    }

    #[test]
    fn request_id_generation_creates_uuid_when_absent() {
        let headers = axum::http::HeaderMap::new();
        let (value, strategy) = request_id_value_from_headers(&headers);
        assert_eq!(strategy, "uuid_v4");
        assert!(value.to_str().ok().is_some());
    }
}
