//! Per-service HTTP runtime state and the health, readiness and metrics
//! handlers that report on it.
//!
//! A service embeds one [`RuntimeState`] in its axum state and exposes it via
//! [`HasRuntimeState`]; the middleware installed by
//! [`crate::http::apply_common_http_layers`] reads its configuration from it
//! and records its counters into it.

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

/// Configuration snapshot, shared store and live counters for one service's
/// HTTP stack.
///
/// Built from the environment by [`RuntimeState::try_new`] (or the lenient
/// [`RuntimeState::new`]). Configuration fields are read once at construction:
/// changing the environment afterwards has no effect on an existing state.
/// Cloning is cheap and clones share the counters and store (they are
/// `Arc`s); the configuration fields are copied.
///
/// The counters are what [`metrics`] and [`metrics_prometheus`] report. They
/// are written by the middleware, with relaxed atomics, and are not meant to
/// be written by application code.
#[derive(Clone)]
pub struct RuntimeState {
    /// Requests seen by the metrics middleware, counted when they start.
    pub request_count: Arc<AtomicU64>,
    /// Requests currently in flight. Decremented when the request future is
    /// dropped, so client disconnects do not leak.
    pub inflight_requests: Arc<AtomicU64>,
    /// Requests rejected by the authentication middleware, across all
    /// reasons (see `auth_failure_reasons` for the breakdown).
    pub auth_failures_total: Arc<AtomicU64>,
    /// Completed responses with a 2xx status.
    pub response_2xx_total: Arc<AtomicU64>,
    /// Completed responses with a 4xx status.
    pub response_4xx_total: Arc<AtomicU64>,
    /// Completed responses with a 5xx status.
    pub response_5xx_total: Arc<AtomicU64>,
    /// When this state was constructed; the basis of `uptime_seconds`.
    pub started_at: Instant,
    /// Key-value store for the per-IP rate-limit and auth-failure counters
    /// and the token-revocation list (`auth:revoked:{jti}`): a Redis store
    /// when `KRAB_REDIS_URL` is set (feature `redis-store`), otherwise an
    /// in-process [`MemoryStore`] that replicas do not share.
    pub store: Arc<dyn DistributedStore>,
    /// Request-duration histogram, non-cumulative: completed requests taking
    /// ≤10 ms, ≤50 ms, ≤100 ms, ≤200 ms, ≤500 ms, ≤1 s, ≤2 s and longer, one
    /// bucket each. The Prometheus exposition makes them cumulative.
    pub latency_buckets: Arc<[AtomicU64; 8]>,
    /// Sum of all observed request durations, in microseconds — the
    /// histogram's `_sum` series. Added in 0.6.0.
    pub latency_sum_micros: Arc<AtomicU64>,
    /// Last readiness verdict, exported as `krab_readiness_status`. Starts
    /// `true`; only [`readiness_with_dependencies`] updates it, so it is stale
    /// until that endpoint is polled.
    pub readiness_status: Arc<std::sync::atomic::AtomicBool>,
    /// Requests allowed per client IP per rate-limit window
    /// ([`HttpConfig::rate_limit_capacity`](crate::config::HttpConfig::rate_limit_capacity)).
    pub rate_limit_capacity: f64,
    /// Sets the rate-limit window with the capacity
    /// ([`HttpConfig::rate_limit_refill_per_sec`](crate::config::HttpConfig::rate_limit_refill_per_sec)).
    pub rate_limit_refill_per_sec: f64,
    /// Origins allowed by CORS (`KRAB_CORS_ORIGINS`).
    pub cors_origins: Vec<String>,
    /// Whether CORS allows any origin when `cors_origins` is empty; true only
    /// in `dev`.
    pub cors_allow_any_origin: bool,
    /// Whether client-IP extraction trusts `x-forwarded-for` / `x-real-ip`
    /// (`KRAB_TRUST_PROXY_HEADERS`, default false).
    pub trust_proxy_headers: bool,
    /// Whether a rate-limit store error lets the request through (true) or
    /// rejects it (false). `KRAB_RATE_LIMIT_FAIL_OPEN`; defaults to true only
    /// in `dev`.
    pub rate_limit_fail_open: bool,
    /// Length of the fixed per-IP auth-failure window, in seconds
    /// (`KRAB_AUTH_FAILURE_WINDOW_SECS`, default 60).
    pub auth_fail_window_secs: u64,
    /// Auth failures one IP may accumulate in a window before further failures
    /// are answered `429` (`KRAB_AUTH_FAILURE_THRESHOLD`, default 100).
    pub auth_fail_threshold: u64,
    /// `KRAB_AUTH_MODE`: `jwt` or `oidc` verify bearer JWTs; any other value
    /// uses the static bearer token (dev only).
    pub auth_mode: String,
    /// Scope required on `/internal` and `/api/internal` paths
    /// (`KRAB_SERVICE_AUTH_SCOPE`, default `service:internal`).
    pub service_auth_scope: String,
    /// Operator-declared unauthenticated path patterns:
    /// `KRAB_AUTH_PUBLIC_PATHS` (comma-separated). Always honoured, whatever
    /// `KRAB_AUTH_OPEN_PATHS` says. A trailing `*` is a prefix match;
    /// anything else must match exactly.
    ///
    /// Before 0.6.0 this also held the paths added with
    /// [`RuntimeState::with_public_paths`]; those now live in
    /// [`RuntimeState::code_public_paths`].
    pub public_paths: Vec<String>,
    /// Code-declared unauthenticated path patterns, added with
    /// [`RuntimeState::with_public_paths`]. Honoured only while
    /// `KRAB_AUTH_OPEN_PATHS` is unset (see
    /// [`RuntimeState::auth_open_paths_explicit`]). Added in 0.6.0.
    pub code_public_paths: Vec<String>,
    /// Whether `KRAB_AUTH_OPEN_PATHS` was set when this state was built. An
    /// explicit open-path list is the complete unauthenticated surface
    /// (together with `KRAB_AUTH_PUBLIC_PATHS` and `KRAB_METRICS_PUBLIC`), so
    /// [`RuntimeState::code_public_paths`] are then ignored. Added in 0.6.0.
    pub auth_open_paths_explicit: bool,
    /// Completed responses by resolved protocol, indexed REST, GraphQL, RPC,
    /// unknown.
    pub protocol_request_totals: Arc<[AtomicU64; 4]>,
    /// Completed responses by status class and protocol: slot
    /// `class * 4 + protocol`, with classes 2xx, 4xx, 5xx and protocols
    /// ordered as in `protocol_request_totals`.
    pub response_class_protocol_totals: Arc<[AtomicU64; 12]>,
    /// Protocol configuration set with [`RuntimeState::with_protocol_config`];
    /// `None` means [`ProtocolConfig::default`](crate::protocol::ProtocolConfig::default)
    /// (REST only). Not read from the environment here.
    pub protocol_config: Option<crate::protocol::ProtocolConfig>,
    /// Parsed JWT providers with pre-built decoding keys, constructed once at
    /// state construction instead of per request. Per-instance on purpose:
    /// tests (and services) that mutate the environment build a fresh
    /// `RuntimeState` and get a fresh cache.
    pub jwt_verifier_cache: Arc<crate::http_auth::JwtVerifierCache>,
    /// Claim policy (required scopes/roles, admin, tenant, route policies,
    /// required claims), parsed once here rather than from the environment on
    /// every request. Added in 0.6.0.
    pub auth_policy: Arc<crate::http_auth::AuthPolicy>,
    /// Resolved unauthenticated path patterns (`KRAB_AUTH_OPEN_PATHS`, the
    /// defaults, `KRAB_METRICS_PUBLIC`). Added in 0.6.0.
    pub auth_open_paths: Vec<String>,
    /// Authentication failures by [`crate::http_auth::AuthFailureReason`], in
    /// `AuthFailureReason::ALL` order. Added in 0.6.0.
    pub auth_failure_reasons: Arc<[AtomicU64; crate::http_auth::AuthFailureReason::ALL.len()]>,
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
    /// store was requested via `KRAB_REDIS_URL` (or its `_FILE` /
    /// `_VAULT_REF` form) but could not be initialized;
    /// how to react (warn-and-fallback vs fail) is the caller's policy.
    ///
    /// The URL is a secret (it can carry a password), so it is read through
    /// [`crate::config::read_env_or_file`]: `KRAB_REDIS_URL`, then
    /// `KRAB_REDIS_URL_FILE`, then `KRAB_REDIS_URL_VAULT_REF`. The startup
    /// policy rejects the inline form outside dev, so reading only the inline
    /// variable here made the sanctioned `_FILE` form pass policy and then
    /// silently yield a per-process store. A source that is set but cannot be
    /// read is an `Err`, exactly like a broken URL.
    fn build_store() -> anyhow::Result<Arc<dyn DistributedStore>> {
        let redis_url = crate::config::read_env_or_file("KRAB_REDIS_URL")
            .map_err(|err| err.context("KRAB_REDIS_URL could not be read"))?
            .map(|url| url.trim().to_string())
            .filter(|url| !url.is_empty());

        #[cfg(feature = "redis-store")]
        {
            if let Some(redis_url) = redis_url {
                let redis = RedisStore::from_url(&redis_url)?;
                return Ok(Arc::new(redis));
            }
        }

        // Compiled WITHOUT `redis-store`, but the operator set `KRAB_REDIS_URL`:
        // the in-process store cannot honor that intent. Surface it as an init
        // error so `try_new` fails closed outside dev exactly as it does for a
        // broken URL — a silent per-replica downgrade is the more dangerous
        // outcome (revocation and rate limits stop being shared).
        #[cfg(not(feature = "redis-store"))]
        {
            if redis_url.is_some() {
                anyhow::bail!(
                    "KRAB_REDIS_URL is set but this binary was compiled without the \
                     `redis-store` feature and cannot use a shared Redis store"
                );
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
            latency_sum_micros: Arc::new(AtomicU64::new(0)),
            readiness_status: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            rate_limit_capacity: http_cfg.rate_limit_capacity as f64,
            rate_limit_refill_per_sec: http_cfg.rate_limit_refill_per_sec as f64,
            cors_origins: http_cfg.cors_origins,
            cors_allow_any_origin: http_cfg.cors_allow_any_origin,
            trust_proxy_headers: http_cfg.trust_proxy_headers,
            rate_limit_fail_open: http_cfg.rate_limit_fail_open,
            auth_fail_window_secs: http_cfg.auth_fail_window_secs,
            auth_fail_threshold: http_cfg.auth_fail_threshold,
            auth_mode: http_cfg.auth_mode,
            service_auth_scope: http_cfg.service_auth_scope,
            public_paths: std::env::var("KRAB_AUTH_PUBLIC_PATHS")
                .ok()
                .map(|v| crate::http::parse_csv_set(&v))
                .unwrap_or_default(),
            code_public_paths: Vec::new(),
            auth_open_paths_explicit: std::env::var_os("KRAB_AUTH_OPEN_PATHS").is_some(),
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
            jwt_verifier_cache: {
                let cache = Arc::new(crate::http_auth::JwtVerifierCache::from_env());
                crate::http_auth::JwtVerifierCache::spawn_background_refresh(&cache);
                cache
            },
            auth_policy: Arc::new(crate::http_auth::AuthPolicy::from_env()),
            auth_open_paths: crate::http_auth::auth_open_path_patterns(),
            auth_failure_reasons: Arc::new(std::array::from_fn(|_| AtomicU64::new(0))),
        }
    }

    /// Declare this service's default unauthenticated path patterns in code.
    /// A trailing `*` is a prefix match.
    ///
    /// The way for a service to declare its own public routes, rather than
    /// relying on the framework's default open-path list — which still holds
    /// app-specific entries in 0.6.x and loses them in 0.7.0.
    ///
    /// These are **defaults the operator can override**, stored in
    /// [`RuntimeState::code_public_paths`], separately from the
    /// operator's `KRAB_AUTH_PUBLIC_PATHS`:
    ///
    /// - `KRAB_AUTH_OPEN_PATHS` unset: the code-declared paths are open, in
    ///   addition to the default open list and `KRAB_AUTH_PUBLIC_PATHS`.
    /// - `KRAB_AUTH_OPEN_PATHS` set (even to an empty value): it is the
    ///   complete open list, and the code-declared paths are **ignored** — an
    ///   operator who closed a route must not have it reopened by a service
    ///   upgrade. `KRAB_AUTH_PUBLIC_PATHS` and `KRAB_METRICS_PUBLIC` still
    ///   apply on top. A warning names the ignored paths.
    pub fn with_public_paths<I, P>(mut self, paths: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: Into<String>,
    {
        for path in paths {
            let path = path.into();
            if !self.code_public_paths.contains(&path) {
                self.code_public_paths.push(path);
            }
        }
        if self.auth_open_paths_explicit && !self.code_public_paths.is_empty() {
            tracing::warn!(
                ignored_paths = ?self.code_public_paths,
                "auth_code_public_paths_ignored: KRAB_AUTH_OPEN_PATHS is set and is the \
                 complete open-path list; add any of these routes to it (or to \
                 KRAB_AUTH_PUBLIC_PATHS) to keep them unauthenticated"
            );
        }
        self
    }

    /// Whether `path` is unauthenticated because a service or operator
    /// declared it public: [`RuntimeState::public_paths`] always, and
    /// [`RuntimeState::code_public_paths`] unless `KRAB_AUTH_OPEN_PATHS` was
    /// set. Does not consult the open-path list.
    pub fn is_public_path(&self, path: &str) -> bool {
        crate::http_auth::path_matches_patterns(path, &self.public_paths)
            || (!self.auth_open_paths_explicit
                && crate::http_auth::path_matches_patterns(path, &self.code_public_paths))
    }

    /// Sets the protocol configuration the protocol-resolution middleware
    /// enforces. Without it the service serves REST only.
    pub fn with_protocol_config(
        mut self,
        protocol_config: crate::protocol::ProtocolConfig,
    ) -> Self {
        self.protocol_config = Some(protocol_config);
        self
    }

    /// The effective protocol configuration: the one set with
    /// [`RuntimeState::with_protocol_config`], or the default. Returns a clone.
    pub fn protocol_config(&self) -> crate::protocol::ProtocolConfig {
        self.protocol_config.clone().unwrap_or_default()
    }
}

/// Implemented by an axum state type that carries a [`RuntimeState`]; every
/// Krab middleware and the metrics handlers are generic over it.
pub trait HasRuntimeState {
    /// The state's embedded [`RuntimeState`].
    fn runtime_state(&self) -> &RuntimeState;
}

/// Implemented by an axum state type that can report on its dependencies,
/// for [`readiness_with_dependencies`].
pub trait HasReadinessDependencies {
    /// Checks each dependency now and reports its status. Called on every
    /// readiness request, so it should be quick and bounded.
    fn readiness_dependencies(&self) -> Vec<DependencyStatus>;
}

/// JSON body of the [`metrics`] handler: a point-in-time read of the
/// [`RuntimeState`] counters.
#[derive(Serialize)]
pub struct MetricsPayload {
    /// Requests started since boot.
    pub requests_total: u64,
    /// Requests in flight at the time of the read.
    pub inflight_requests: u64,
    /// Requests rejected by authentication since boot.
    pub auth_failures_total: u64,
    /// 2xx responses since boot.
    pub response_2xx_total: u64,
    /// 4xx responses since boot.
    pub response_4xx_total: u64,
    /// 5xx responses since boot.
    pub response_5xx_total: u64,
    /// Seconds since the [`RuntimeState`] was constructed.
    pub uptime_seconds: u64,
    /// Completed responses served as REST.
    pub protocol_rest_total: u64,
    /// Completed responses served as GraphQL.
    pub protocol_graphql_total: u64,
    /// Completed responses served as RPC.
    pub protocol_rpc_total: u64,
    /// Completed responses with no resolved protocol (for example, rejected
    /// before protocol resolution ran).
    pub protocol_unknown_total: u64,
}

/// The health of one dependency, as reported by
/// [`HasReadinessDependencies::readiness_dependencies`].
#[derive(Serialize, Clone)]
pub struct DependencyStatus {
    /// Dependency name, for example `database`.
    pub name: &'static str,
    /// Whether the dependency is usable right now.
    pub ready: bool,
    /// Whether the service cannot serve without it. A critical dependency
    /// that is not ready makes readiness `503 not_ready`; a non-critical one
    /// only makes it `degraded`.
    pub critical: bool,
    /// How long the check took, in milliseconds, if measured.
    pub latency_ms: Option<u64>,
    /// Free-form diagnostic, for example the error from a failed check.
    pub detail: Option<String>,
}

/// JSON body of [`readiness_with_dependencies`].
#[derive(Serialize)]
pub struct ReadinessPayload {
    /// `ready`, `degraded` (a non-critical dependency is down) or
    /// `not_ready` (a critical one is).
    pub status: &'static str,
    /// Seconds since the [`RuntimeState`] was constructed.
    pub uptime_seconds: u64,
    /// Every dependency's status, as reported.
    pub dependencies: Vec<DependencyStatus>,
}

/// JSON body of [`health`] and [`readiness`]: `{"status": "..."}`.
#[derive(Serialize)]
pub struct StatusPayload {
    /// `ok` from [`health`], `ready` from [`readiness`].
    pub status: &'static str,
}

/// Liveness handler: always `200 {"status":"ok"}`.
pub async fn health() -> Json<StatusPayload> {
    Json(StatusPayload { status: "ok" })
}

/// Readiness handler with no dependency checks: always
/// `200 {"status":"ready"}`. Use [`readiness_with_dependencies`] when the
/// service has dependencies worth checking.
pub async fn readiness() -> Json<StatusPayload> {
    Json(StatusPayload { status: "ready" })
}

/// Readiness handler that checks the state's dependencies.
///
/// Answers `503` with status `not_ready` when any critical dependency is not
/// ready; otherwise `200` with `degraded` (a non-critical dependency is down)
/// or `ready`. Also records the verdict in
/// [`RuntimeState::readiness_status`], which `krab_readiness_status` exports.
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

/// JSON metrics handler: the state's counters as a [`MetricsPayload`].
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

/// Prometheus text-exposition handler (`text/plain; version=0.0.4`).
///
/// Emits request, in-flight, auth-failure (total and by reason), response
/// class, per-protocol, uptime and readiness series, and the
/// `krab_http_request_duration_seconds` histogram with `_sum` and `_count`.
/// The same buckets are also written as `krab_request_duration_seconds`, a
/// deprecated alias removed in 0.7.0.
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
    // `+Inf` is the cumulative count of every *completed* observation, so it
    // includes the >2s bucket and equals `_count`. It used to be
    // `requests_total`, which counts requests when they start — in-flight ones
    // included — and never read the >2s bucket at all.
    let b_inf = b6 + runtime.latency_buckets[7].load(Ordering::Relaxed);
    let latency_sum_seconds =
        runtime.latency_sum_micros.load(Ordering::Relaxed) as f64 / 1_000_000.0;

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
        "# HELP krab_auth_failures_by_reason_total Authentication failures by reason\n# TYPE krab_auth_failures_by_reason_total counter"
    );
    for reason in crate::http_auth::AuthFailureReason::ALL {
        let _ = writeln!(
            body,
            "krab_auth_failures_by_reason_total{{reason=\"{}\"}} {}",
            reason.as_str(),
            runtime.auth_failure_reasons[reason.slot()].load(Ordering::Relaxed)
        );
    }
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
    // The histogram is `krab_http_request_duration_seconds`, the name the
    // shipped alert rules, Grafana dashboard and on-call playbook query. Until
    // 0.6.0 it was emitted only as `krab_request_duration_seconds` with no
    // `_sum`/`_count`, so every latency alert evaluated to nothing. The old
    // name is still written, buckets only, for one minor version (deprecated
    // in 0.6.0, removed in 0.7.0).
    let buckets = [
        ("0.01", b0),
        ("0.05", b1),
        ("0.1", b2),
        ("0.2", b3),
        ("0.5", b4),
        ("1", b5),
        ("2", b6),
        ("+Inf", b_inf),
    ];
    for name in [
        "krab_http_request_duration_seconds",
        "krab_request_duration_seconds",
    ] {
        let help = if name == "krab_request_duration_seconds" {
            "Deprecated alias of krab_http_request_duration_seconds; removed in 0.7.0"
        } else {
            "HTTP request duration histogram"
        };
        let _ = writeln!(body, "# HELP {name} {help}\n# TYPE {name} histogram");
        for (le, count) in buckets {
            let _ = writeln!(body, "{name}_bucket{{le=\"{le}\"}} {count}");
        }
        if name == "krab_http_request_duration_seconds" {
            let _ = writeln!(body, "{name}_sum {latency_sum_seconds}");
            let _ = writeln!(body, "{name}_count {b_inf}");
        }
    }

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
mod prometheus_tests {
    use super::{metrics_prometheus_impl, RuntimeState};
    use std::sync::atomic::Ordering;

    async fn body_of(state: &RuntimeState) -> String {
        let response = metrics_prometheus_impl(state);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("metrics body");
        String::from_utf8(bytes.to_vec()).expect("utf-8 metrics body")
    }

    fn value_of(body: &str, series: &str) -> String {
        body.lines()
            .find_map(|line| line.strip_prefix(series).map(|v| v.trim().to_string()))
            .unwrap_or_else(|| panic!("series {series} missing from:\n{body}"))
    }

    /// `+Inf` and `_count` count completed requests, including the >2s bucket,
    /// and are independent of `requests_total` (which also counts in-flight
    /// requests).
    #[tokio::test]
    async fn histogram_inf_bucket_matches_count_and_includes_slowest_bucket() {
        let state = RuntimeState::default();
        state.latency_buckets[0].store(3, Ordering::Relaxed);
        state.latency_buckets[4].store(2, Ordering::Relaxed);
        state.latency_buckets[7].store(1, Ordering::Relaxed);
        state.latency_sum_micros.store(3_500_000, Ordering::Relaxed);
        // Two requests still in flight: counted as started, not observed.
        state.request_count.store(8, Ordering::Relaxed);

        let body = body_of(&state).await;
        let name = "krab_http_request_duration_seconds";
        assert_eq!(
            value_of(&body, &format!("{name}_bucket{{le=\"0.01\"}}")),
            "3"
        );
        assert_eq!(value_of(&body, &format!("{name}_bucket{{le=\"2\"}}")), "5");
        assert_eq!(
            value_of(&body, &format!("{name}_bucket{{le=\"+Inf\"}}")),
            "6"
        );
        assert_eq!(value_of(&body, &format!("{name}_count")), "6");
        assert_eq!(value_of(&body, &format!("{name}_sum")), "3.5");

        // The deprecated alias carries the same buckets.
        assert_eq!(
            value_of(&body, "krab_request_duration_seconds_bucket{le=\"+Inf\"}"),
            "6"
        );
    }

    /// Every metric the shipped alert rules and dashboard query must exist in
    /// the exposition. This is the check that would have caught the latency
    /// alerts querying a histogram nothing emitted.
    #[tokio::test]
    async fn monitoring_assets_only_query_emitted_metrics() {
        let body = body_of(&RuntimeState::default()).await;
        let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../monitoring");
        for file in ["alert_rules.yml", "grafana_dashboard.json"] {
            let text = std::fs::read_to_string(format!("{root}/{file}"))
                .unwrap_or_else(|e| panic!("read monitoring/{file}: {e}"));
            for metric in referenced_series(&text) {
                assert!(
                    body.lines().any(|l| l.starts_with(&metric)),
                    "monitoring/{file} queries `{metric}`, which the metrics endpoint never emits"
                );
            }
        }
    }

    /// Every `krab_*` identifier in `text` used as a PromQL series: followed
    /// by a selector, a range, a closing paren or quote, or a comparison or
    /// arithmetic operator. That excludes rule-group names (`krab_latency` at
    /// end of line) and the `job=~"krab_.*"` matcher.
    fn referenced_series(text: &str) -> std::collections::BTreeSet<String> {
        let mut found = std::collections::BTreeSet::new();
        let mut rest = text;
        while let Some(start) = rest.find("krab_") {
            let tail = &rest[start..];
            let end = tail
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .unwrap_or(tail.len());
            let after = &tail[end..];
            let after_trimmed = after.trim_start_matches(' ');
            let is_series = after.starts_with(['{', '[', ')', '"', '\\'])
                || ["==", "!=", ">", "<", "-", "+", "/", "*"]
                    .iter()
                    .any(|op| after_trimmed.starts_with(op));
            if is_series {
                found.insert(tail[..end].to_string());
            }
            rest = after;
        }
        found
    }
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

    fn clear_redis_url_sources() {
        for name in [
            "KRAB_REDIS_URL",
            "KRAB_REDIS_URL_FILE",
            "KRAB_REDIS_URL_VAULT_REF",
        ] {
            std::env::remove_var(name);
        }
    }

    /// `KRAB_REDIS_URL_FILE` is the form the prod secrets policy requires, so
    /// the store must honour it. The file holds a URL the store rejects: if
    /// the file were ignored (the old behaviour), `try_new` would quietly
    /// succeed on a `MemoryStore` in prod.
    #[test]
    #[serial_test::serial]
    fn try_new_reads_the_redis_url_from_the_file_form() {
        clear_redis_url_sources();
        let path =
            std::env::temp_dir().join(format!("krab-redis-url-file-{}.txt", std::process::id()));
        std::fs::write(&path, "definitely-not-a-redis-url\n").expect("write url file");
        std::env::set_var("KRAB_REDIS_URL_FILE", &path);
        std::env::set_var("KRAB_ENVIRONMENT", "prod");

        let bad = RuntimeState::try_new();

        #[cfg(feature = "redis-store")]
        let good = {
            std::fs::write(&path, "redis://127.0.0.1:6379\n").expect("write url file");
            RuntimeState::try_new()
        };

        clear_redis_url_sources();
        std::env::remove_var("KRAB_ENVIRONMENT");
        let _ = std::fs::remove_file(&path);

        assert!(
            bad.is_err(),
            "the URL in KRAB_REDIS_URL_FILE must reach the store"
        );
        #[cfg(feature = "redis-store")]
        assert!(good.is_ok(), "a valid URL from the file builds the store");
    }

    /// A Redis URL source that is configured but unreadable fails closed
    /// outside dev, exactly like a broken URL, and falls back in dev.
    #[test]
    #[serial_test::serial]
    fn try_new_fails_closed_outside_dev_when_the_redis_url_cannot_be_read() {
        clear_redis_url_sources();
        let missing = std::env::temp_dir().join("krab-redis-url-file-that-does-not-exist.txt");
        let _ = std::fs::remove_file(&missing);
        std::env::set_var("KRAB_REDIS_URL_FILE", &missing);

        let mut outcomes = Vec::new();
        for environment in ["prod", "staging", "some-unknown-env", "dev"] {
            std::env::set_var("KRAB_ENVIRONMENT", environment);
            outcomes.push((environment, RuntimeState::try_new().is_ok()));
        }

        clear_redis_url_sources();
        std::env::remove_var("KRAB_ENVIRONMENT");

        for (environment, ok) in outcomes {
            assert_eq!(
                ok,
                environment == "dev",
                "unexpected try_new outcome in '{environment}' with an unreadable KRAB_REDIS_URL_FILE"
            );
        }
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
