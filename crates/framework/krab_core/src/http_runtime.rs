use std::fmt::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use axum::extract::State;
use axum::http::header::CONTENT_TYPE;
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;

#[cfg(feature = "redis-store")]
use crate::store::RedisStore;
use crate::store::{DistributedStore, MemoryStore};

#[derive(Clone)]
pub struct RuntimeState {
    pub request_count: Arc<AtomicU64>,
    pub inflight_requests: Arc<AtomicU64>,
    pub auth_failures_total: Arc<AtomicU64>,
    pub response_2xx_total: Arc<AtomicU64>,
    pub response_4xx_total: Arc<AtomicU64>,
    pub response_5xx_total: Arc<AtomicU64>,
    pub started_at: Instant,
    pub store: Arc<dyn DistributedStore>,
    pub latency_buckets: Arc<[AtomicU64; 8]>,
    pub readiness_status: Arc<std::sync::atomic::AtomicBool>,
    pub rate_limit_capacity: f64,
    pub rate_limit_refill_per_sec: f64,
    pub cors_origins: Vec<String>,
    pub cors_allow_any_origin: bool,
    pub trust_proxy_headers: bool,
    pub rate_limit_fail_open: bool,
    pub auth_mode: String,
    pub service_auth_scope: String,
    pub public_paths: Vec<String>,
    pub protocol_request_totals: Arc<[AtomicU64; 4]>,
    pub response_class_protocol_totals: Arc<[AtomicU64; 12]>,
    pub protocol_config: Option<crate::protocol::ProtocolConfig>,
    /// Parsed JWT providers with pre-built decoding keys, constructed once at
    /// state construction instead of per request. Per-instance on purpose:
    /// tests (and services) that mutate the environment build a fresh
    /// `RuntimeState` and get a fresh cache.
    pub jwt_verifier_cache: Arc<crate::http_auth::JwtVerifierCache>,
}

impl Default for RuntimeState {
    fn default() -> Self {
        Self::new()
    }
}

impl RuntimeState {
    /// Lenient constructor: a configured-but-broken `KRAB_REDIS_URL` warns and
    /// falls back to an in-process [`MemoryStore`] in every environment. Boot
    /// paths should prefer [`RuntimeState::try_new`], which fails closed
    /// outside dev.
    pub fn new() -> Self {
        let store: Arc<dyn DistributedStore> = match Self::build_store() {
            Ok(store) => store,
            Err(err) => {
                tracing::warn!(error = %err, "failed_to_initialize_redis_store_falling_back_to_memory");
                Arc::new(MemoryStore::new())
            }
        };
        Self::from_store(store)
    }

    /// Fallible constructor for service boot paths. When `KRAB_REDIS_URL` is
    /// set and the Redis store cannot be initialized, this returns an error in
    /// `staging`, `prod`, and unknown environments (unknown fails closed,
    /// mirroring the config posture); in `dev` it warns and falls back to an
    /// in-process [`MemoryStore`].
    pub fn try_new() -> anyhow::Result<Self> {
        let store: Arc<dyn DistributedStore> = match Self::build_store() {
            Ok(store) => store,
            Err(err) => {
                let environment = crate::config::Environment::from_env();
                match environment {
                    crate::config::Environment::Dev => {
                        tracing::warn!(
                            error = %err,
                            environment = %environment.as_str(),
                            "failed_to_initialize_redis_store_falling_back_to_memory"
                        );
                        Arc::new(MemoryStore::new())
                    }
                    _ => {
                        return Err(err.context(format!(
                            "KRAB_REDIS_URL is set but the redis store failed to initialize; \
                             refusing to fall back to an in-process store in '{}'",
                            environment.as_str()
                        )));
                    }
                }
            }
        };
        Ok(Self::from_store(store))
    }

    /// Build the distributed store from the environment. `Err` means a Redis
    /// store was requested via `KRAB_REDIS_URL` but could not be initialized;
    /// how to react (warn-and-fallback vs fail) is the caller's policy.
    fn build_store() -> anyhow::Result<Arc<dyn DistributedStore>> {
        #[cfg(feature = "redis-store")]
        {
            if let Ok(redis_url) = std::env::var("KRAB_REDIS_URL") {
                if !redis_url.trim().is_empty() {
                    let redis = RedisStore::from_url(redis_url.trim())?;
                    return Ok(Arc::new(redis));
                }
            }
        }

        // Compiled WITHOUT `redis-store`, but the operator set `KRAB_REDIS_URL`:
        // the in-process store cannot honor that intent. Surface it as an init
        // error so `try_new` fails closed outside dev exactly as it does for a
        // broken URL — a silent per-replica downgrade is the more dangerous
        // outcome (revocation and rate limits stop being shared).
        #[cfg(not(feature = "redis-store"))]
        {
            if let Ok(redis_url) = std::env::var("KRAB_REDIS_URL") {
                if !redis_url.trim().is_empty() {
                    anyhow::bail!(
                        "KRAB_REDIS_URL is set but this binary was compiled without the \
                         `redis-store` feature and cannot use a shared Redis store"
                    );
                }
            }
        }

        Ok(Arc::new(MemoryStore::new()))
    }

    fn from_store(store: Arc<dyn DistributedStore>) -> Self {
        let http_cfg = crate::config::HttpConfig::from_env();

        Self {
            request_count: Arc::new(AtomicU64::new(0)),
            inflight_requests: Arc::new(AtomicU64::new(0)),
            auth_failures_total: Arc::new(AtomicU64::new(0)),
            response_2xx_total: Arc::new(AtomicU64::new(0)),
            response_4xx_total: Arc::new(AtomicU64::new(0)),
            response_5xx_total: Arc::new(AtomicU64::new(0)),
            started_at: Instant::now(),
            store,
            latency_buckets: Arc::new([
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
            ]),
            readiness_status: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            rate_limit_capacity: http_cfg.rate_limit_capacity as f64,
            rate_limit_refill_per_sec: http_cfg.rate_limit_refill_per_sec as f64,
            cors_origins: http_cfg.cors_origins,
            cors_allow_any_origin: http_cfg.cors_allow_any_origin,
            trust_proxy_headers: http_cfg.trust_proxy_headers,
            rate_limit_fail_open: http_cfg.rate_limit_fail_open,
            auth_mode: http_cfg.auth_mode,
            service_auth_scope: http_cfg.service_auth_scope,
            public_paths: std::env::var("KRAB_AUTH_PUBLIC_PATHS")
                .ok()
                .map(|v| crate::http::parse_csv_set(&v))
                .unwrap_or_default(),
            protocol_request_totals: Arc::new([
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
            ]),
            response_class_protocol_totals: Arc::new([
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
            ]),
            protocol_config: None,
            jwt_verifier_cache: Arc::new(crate::http_auth::JwtVerifierCache::from_env()),
        }
    }

    pub fn with_protocol_config(
        mut self,
        protocol_config: crate::protocol::ProtocolConfig,
    ) -> Self {
        self.protocol_config = Some(protocol_config);
        self
    }

    pub fn protocol_config(&self) -> crate::protocol::ProtocolConfig {
        self.protocol_config.clone().unwrap_or_default()
    }
}

pub trait HasRuntimeState {
    fn runtime_state(&self) -> &RuntimeState;
}

pub trait HasReadinessDependencies {
    fn readiness_dependencies(&self) -> Vec<DependencyStatus>;
}

#[derive(Serialize)]
pub struct MetricsPayload {
    pub requests_total: u64,
    pub inflight_requests: u64,
    pub auth_failures_total: u64,
    pub response_2xx_total: u64,
    pub response_4xx_total: u64,
    pub response_5xx_total: u64,
    pub uptime_seconds: u64,
    pub protocol_rest_total: u64,
    pub protocol_graphql_total: u64,
    pub protocol_rpc_total: u64,
    pub protocol_unknown_total: u64,
}

#[derive(Serialize, Clone)]
pub struct DependencyStatus {
    pub name: &'static str,
    pub ready: bool,
    pub critical: bool,
    pub latency_ms: Option<u64>,
    pub detail: Option<String>,
}

#[derive(Serialize)]
pub struct ReadinessPayload {
    pub status: &'static str,
    pub uptime_seconds: u64,
    pub dependencies: Vec<DependencyStatus>,
}

#[derive(Serialize)]
pub struct StatusPayload {
    pub status: &'static str,
}

pub async fn health() -> Json<StatusPayload> {
    Json(StatusPayload { status: "ok" })
}

pub async fn readiness() -> Json<StatusPayload> {
    Json(StatusPayload { status: "ready" })
}

pub async fn readiness_with_dependencies<S>(
    State(state): State<S>,
) -> (StatusCode, Json<ReadinessPayload>)
where
    S: HasReadinessDependencies + HasRuntimeState,
{
    let dependencies = state.readiness_dependencies();
    let has_critical_failure = dependencies.iter().any(|d| d.critical && !d.ready);
    let has_non_critical_failure = dependencies.iter().any(|d| !d.critical && !d.ready);
    let status = if has_critical_failure {
        "not_ready"
    } else if has_non_critical_failure {
        "degraded"
    } else {
        "ready"
    };
    let code = if has_critical_failure {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::OK
    };

    if has_critical_failure {
        state
            .runtime_state()
            .readiness_status
            .store(false, Ordering::Relaxed);
    } else {
        state
            .runtime_state()
            .readiness_status
            .store(true, Ordering::Relaxed);
    }

    let uptime_seconds = state.runtime_state().started_at.elapsed().as_secs();

    (
        code,
        Json(ReadinessPayload {
            status,
            uptime_seconds,
            dependencies,
        }),
    )
}

pub async fn metrics<S>(State(state): State<S>) -> Json<MetricsPayload>
where
    S: HasRuntimeState,
{
    let runtime = state.runtime_state();
    let protocol_totals = &runtime.protocol_request_totals;
    Json(MetricsPayload {
        requests_total: runtime.request_count.load(Ordering::Relaxed),
        inflight_requests: runtime.inflight_requests.load(Ordering::Relaxed),
        auth_failures_total: runtime.auth_failures_total.load(Ordering::Relaxed),
        response_2xx_total: runtime.response_2xx_total.load(Ordering::Relaxed),
        response_4xx_total: runtime.response_4xx_total.load(Ordering::Relaxed),
        response_5xx_total: runtime.response_5xx_total.load(Ordering::Relaxed),
        uptime_seconds: runtime.started_at.elapsed().as_secs(),
        protocol_rest_total: protocol_totals[0].load(Ordering::Relaxed),
        protocol_graphql_total: protocol_totals[1].load(Ordering::Relaxed),
        protocol_rpc_total: protocol_totals[2].load(Ordering::Relaxed),
        protocol_unknown_total: protocol_totals[3].load(Ordering::Relaxed),
    })
}

pub async fn metrics_prometheus<S>(State(state): State<S>) -> Response
where
    S: HasRuntimeState,
{
    metrics_prometheus_impl(state.runtime_state())
}

pub(crate) fn metrics_prometheus_impl(runtime: &RuntimeState) -> Response {
    let requests_total = runtime.request_count.load(Ordering::Relaxed);
    let inflight_requests = runtime.inflight_requests.load(Ordering::Relaxed);
    let auth_failures_total = runtime.auth_failures_total.load(Ordering::Relaxed);
    let response_2xx_total = runtime.response_2xx_total.load(Ordering::Relaxed);
    let response_4xx_total = runtime.response_4xx_total.load(Ordering::Relaxed);
    let response_5xx_total = runtime.response_5xx_total.load(Ordering::Relaxed);
    let uptime_seconds = runtime.started_at.elapsed().as_secs();
    let protocol_rest_total = runtime.protocol_request_totals[0].load(Ordering::Relaxed);
    let protocol_graphql_total = runtime.protocol_request_totals[1].load(Ordering::Relaxed);
    let protocol_rpc_total = runtime.protocol_request_totals[2].load(Ordering::Relaxed);
    let protocol_unknown_total = runtime.protocol_request_totals[3].load(Ordering::Relaxed);

    let readiness_status = match runtime.readiness_status.load(Ordering::Relaxed) {
        true => 1,
        false => 0,
    };

    let b0 = runtime.latency_buckets[0].load(Ordering::Relaxed);
    let b1 = b0 + runtime.latency_buckets[1].load(Ordering::Relaxed);
    let b2 = b1 + runtime.latency_buckets[2].load(Ordering::Relaxed);
    let b3 = b2 + runtime.latency_buckets[3].load(Ordering::Relaxed);
    let b4 = b3 + runtime.latency_buckets[4].load(Ordering::Relaxed);
    let b5 = b4 + runtime.latency_buckets[5].load(Ordering::Relaxed);
    let b6 = b5 + runtime.latency_buckets[6].load(Ordering::Relaxed);
    let b_inf = requests_total;

    let protocols = ["rest", "graphql", "rpc", "unknown"];
    let mut body = String::new();
    let _ = writeln!(
        body,
        "# HELP krab_requests_total Total HTTP requests handled\n# TYPE krab_requests_total counter\nkrab_requests_total {}",
        requests_total
    );
    let _ = writeln!(
        body,
        "# HELP krab_http_requests_by_protocol_total Total HTTP requests by resolved protocol\n# TYPE krab_http_requests_by_protocol_total counter\nkrab_http_requests_by_protocol_total{{protocol=\"rest\"}} {}\nkrab_http_requests_by_protocol_total{{protocol=\"graphql\"}} {}\nkrab_http_requests_by_protocol_total{{protocol=\"rpc\"}} {}\nkrab_http_requests_by_protocol_total{{protocol=\"unknown\"}} {}",
        protocol_rest_total, protocol_graphql_total, protocol_rpc_total, protocol_unknown_total
    );
    let _ = writeln!(
        body,
        "# HELP krab_inflight_requests Current inflight HTTP requests\n# TYPE krab_inflight_requests gauge\nkrab_inflight_requests {}",
        inflight_requests
    );
    let _ = writeln!(
        body,
        "# HELP krab_auth_failures_total Authentication failures\n# TYPE krab_auth_failures_total counter\nkrab_auth_failures_total {}",
        auth_failures_total
    );
    let _ = writeln!(
        body,
        "# HELP krab_response_2xx_total 2xx responses\n# TYPE krab_response_2xx_total counter\nkrab_response_2xx_total {}",
        response_2xx_total
    );
    let _ = writeln!(
        body,
        "# HELP krab_response_4xx_total 4xx responses\n# TYPE krab_response_4xx_total counter\nkrab_response_4xx_total {}",
        response_4xx_total
    );
    let _ = writeln!(
        body,
        "# HELP krab_response_5xx_total 5xx responses\n# TYPE krab_response_5xx_total counter\nkrab_response_5xx_total {}",
        response_5xx_total
    );
    let _ = writeln!(
        body,
        "# HELP krab_http_responses_total Total HTTP responses by class\n# TYPE krab_http_responses_total counter\nkrab_http_responses_total{{class=\"2xx\"}} {}\nkrab_http_responses_total{{class=\"4xx\"}} {}\nkrab_http_responses_total{{class=\"5xx\"}} {}",
        response_2xx_total, response_4xx_total, response_5xx_total
    );
    let _ = writeln!(
        body,
        "# HELP krab_uptime_seconds Service uptime seconds\n# TYPE krab_uptime_seconds gauge\nkrab_uptime_seconds {}",
        uptime_seconds
    );
    let _ = writeln!(
        body,
        "# HELP krab_readiness_status Readiness status (1=ready,0=not_ready)\n# TYPE krab_readiness_status gauge\nkrab_readiness_status {}",
        readiness_status
    );
    let _ = writeln!(
        body,
        "# HELP krab_request_duration_seconds Request duration histogram\n# TYPE krab_request_duration_seconds histogram\nkrab_request_duration_seconds_bucket{{le=\"0.01\"}} {}\nkrab_request_duration_seconds_bucket{{le=\"0.05\"}} {}\nkrab_request_duration_seconds_bucket{{le=\"0.1\"}} {}\nkrab_request_duration_seconds_bucket{{le=\"0.2\"}} {}\nkrab_request_duration_seconds_bucket{{le=\"0.5\"}} {}\nkrab_request_duration_seconds_bucket{{le=\"1\"}} {}\nkrab_request_duration_seconds_bucket{{le=\"2\"}} {}\nkrab_request_duration_seconds_bucket{{le=\"+Inf\"}} {}",
        b0, b1, b2, b3, b4, b5, b6, b_inf
    );

    let _ = writeln!(
        body,
        "# HELP krab_http_responses_by_protocol_total Total HTTP responses by resolved protocol and status class\n# TYPE krab_http_responses_by_protocol_total counter"
    );
    let _ = writeln!(
        body,
        "# HELP krab_http_responses_by_protocol_and_class_total Total HTTP responses by resolved protocol and status class\n# TYPE krab_http_responses_by_protocol_and_class_total counter"
    );

    for (protocol_idx, protocol) in protocols.iter().enumerate() {
        let success = runtime.response_class_protocol_totals[protocol_idx].load(Ordering::Relaxed);
        let client_error =
            runtime.response_class_protocol_totals[4 + protocol_idx].load(Ordering::Relaxed);
        let server_error =
            runtime.response_class_protocol_totals[8 + protocol_idx].load(Ordering::Relaxed);
        let _ = writeln!(
            body,
            "krab_http_responses_by_protocol_total{{protocol=\"{}\",class=\"2xx\"}} {}\nkrab_http_responses_by_protocol_total{{protocol=\"{}\",class=\"4xx\"}} {}\nkrab_http_responses_by_protocol_total{{protocol=\"{}\",class=\"5xx\"}} {}\nkrab_http_responses_by_protocol_and_class_total{{protocol=\"{}\",class=\"2xx\"}} {}\nkrab_http_responses_by_protocol_and_class_total{{protocol=\"{}\",class=\"4xx\"}} {}\nkrab_http_responses_by_protocol_and_class_total{{protocol=\"{}\",class=\"5xx\"}} {}",
            protocol,
            success,
            protocol,
            client_error,
            protocol,
            server_error,
            protocol, success, protocol, client_error, protocol, server_error
        );
    }

    let mut response = body.into_response();
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/plain; version=0.0.4; charset=utf-8"),
    );
    response
}

#[cfg(test)]
mod runtime_state_tests {
    #[allow(unused_imports)]
    use super::RuntimeState;

    /// A broken `KRAB_REDIS_URL` must abort startup outside dev: silently
    /// downgrading a shared store to an in-process one breaks rate limiting
    /// and revocation across replicas. Unknown environments fail closed like
    /// prod, mirroring the config posture.
    #[cfg(feature = "redis-store")]
    #[test]
    #[serial_test::serial]
    fn try_new_fails_closed_outside_dev_with_bad_redis_url() {
        std::env::set_var("KRAB_REDIS_URL", "definitely-not-a-redis-url");

        for environment in ["prod", "staging", "some-unknown-env"] {
            std::env::set_var("KRAB_ENVIRONMENT", environment);
            let result = RuntimeState::try_new();
            assert!(
                result.is_err(),
                "try_new must fail in '{environment}' when the redis store cannot initialize"
            );
        }

        std::env::remove_var("KRAB_REDIS_URL");
        std::env::remove_var("KRAB_ENVIRONMENT");
    }

    /// In dev the same misconfiguration warns and falls back to the
    /// in-process store, keeping the local loop unbroken.
    #[cfg(feature = "redis-store")]
    #[test]
    #[serial_test::serial]
    fn try_new_warns_and_falls_back_in_dev_with_bad_redis_url() {
        std::env::set_var("KRAB_ENVIRONMENT", "dev");
        std::env::set_var("KRAB_REDIS_URL", "definitely-not-a-redis-url");

        let result = RuntimeState::try_new();

        std::env::remove_var("KRAB_REDIS_URL");
        std::env::remove_var("KRAB_ENVIRONMENT");

        assert!(result.is_ok(), "dev must fall back to the memory store");
    }

    /// Without `KRAB_REDIS_URL`, `try_new` succeeds in every environment on
    /// the in-process store.
    #[test]
    #[serial_test::serial]
    fn try_new_succeeds_without_redis_url() {
        std::env::remove_var("KRAB_REDIS_URL");
        std::env::set_var("KRAB_ENVIRONMENT", "prod");

        let result = RuntimeState::try_new();

        std::env::remove_var("KRAB_ENVIRONMENT");

        assert!(result.is_ok());
    }

    /// A binary compiled WITHOUT `redis-store` but told to use Redis is
    /// misconfigured: `try_new` must fail closed outside dev rather than
    /// silently serve on an in-process store. This build (no default features)
    /// exercises exactly that configuration.
    #[cfg(not(feature = "redis-store"))]
    #[test]
    #[serial_test::serial]
    fn try_new_fails_closed_when_redis_requested_but_feature_absent() {
        std::env::set_var("KRAB_REDIS_URL", "redis://127.0.0.1:6379");

        for environment in ["prod", "staging", "some-unknown-env"] {
            std::env::set_var("KRAB_ENVIRONMENT", environment);
            assert!(
                RuntimeState::try_new().is_err(),
                "try_new must fail in '{environment}' when redis is requested but not compiled in"
            );
        }

        std::env::set_var("KRAB_ENVIRONMENT", "dev");
        assert!(
            RuntimeState::try_new().is_ok(),
            "dev must fall back to the memory store"
        );

        std::env::remove_var("KRAB_REDIS_URL");
        std::env::remove_var("KRAB_ENVIRONMENT");
    }
}
