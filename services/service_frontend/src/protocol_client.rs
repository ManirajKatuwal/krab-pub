use anyhow::{anyhow, Context, Result};
use krab_core::protocol::{ProtocolKind, ServiceCapabilities};
use reqwest::Client;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

#[derive(Clone)]
struct CachedCapability {
    caps: ServiceCapabilities,
    fetched_at: Instant,
}

#[derive(Clone)]
pub struct ProtocolAwareClient {
    http: Client,
    service_urls: HashMap<String, String>,
    split_targets: HashMap<String, HashMap<ProtocolKind, String>>,
    use_gateway_external_mode: bool,
    gateway_base_url: Option<String>,
    downstream_bearer_token: Option<String>,
    cache: Arc<RwLock<HashMap<String, CachedCapability>>>,
    cache_ttl: Duration,
}

impl ProtocolAwareClient {
    pub fn from_env(
        http: Client,
        service_urls: HashMap<String, String>,
        cache_ttl: Duration,
    ) -> Result<Self> {
        let split_targets = parse_split_targets_from_env()?;
        let use_gateway_external_mode = bool_env("KRAB_PROTOCOL_EXTERNAL_MODE", false);
        let gateway_base_url = env_trimmed("KRAB_PROTOCOL_GATEWAY_BASE_URL")
            .map(|v| v.trim_end_matches('/').to_string());
        let downstream_bearer_token = env_trimmed("KRAB_FRONTEND_DOWNSTREAM_BEARER_TOKEN");

        Ok(Self {
            http,
            service_urls,
            split_targets,
            use_gateway_external_mode,
            gateway_base_url,
            downstream_bearer_token,
            cache: Arc::new(RwLock::new(HashMap::new())),
            cache_ttl,
        })
    }

    #[allow(dead_code)]
    pub fn new(http: Client, service_urls: HashMap<String, String>, cache_ttl: Duration) -> Self {
        Self {
            http,
            service_urls,
            split_targets: HashMap::new(),
            use_gateway_external_mode: false,
            gateway_base_url: None,
            downstream_bearer_token: None,
            cache: Arc::new(RwLock::new(HashMap::new())),
            cache_ttl,
        }
    }

    pub async fn capabilities(&self, service: &str) -> Result<ServiceCapabilities> {
        {
            let cache = self.cache.read().await;
            if let Some(entry) = cache.get(service) {
                if entry.fetched_at.elapsed() < self.cache_ttl {
                    return Ok(entry.caps.clone());
                }
            }
        }

        let fetched = self.fetch_capabilities(service).await;
        let caps = match fetched {
            Ok(caps) => caps,
            Err(err) => {
                tracing::warn!(
                    service = service,
                    error = %err,
                    "capability_discovery_failed_falling_back_to_static_defaults"
                );
                fallback_capabilities(service)
            }
        };

        {
            let mut cache = self.cache.write().await;
            cache.insert(
                service.to_string(),
                CachedCapability {
                    caps: caps.clone(),
                    fetched_at: Instant::now(),
                },
            );
        }

        Ok(caps)
    }

    pub async fn resolve_protocol(
        &self,
        service: &str,
        operation: &str,
        client_pref: Option<ProtocolKind>,
    ) -> Result<ProtocolKind> {
        let caps = self.capabilities(service).await?;
        let supported = &caps.supported_protocols;
        let op_allowed = operation_allowed_protocols(operation);

        if let Some(pref) = client_pref {
            if supported.contains(&pref) && op_allowed.contains(&pref) {
                return Ok(pref);
            }
        }

        if supported.contains(&caps.default_protocol) && op_allowed.contains(&caps.default_protocol)
        {
            return Ok(caps.default_protocol);
        }

        for protocol in op_allowed {
            if supported.contains(protocol) {
                return Ok(*protocol);
            }
        }

        Err(anyhow!(
            "no protocol available for service='{}' operation='{}'",
            service,
            operation
        ))
    }

    #[allow(dead_code)]
    pub async fn call(
        &self,
        service: &str,
        operation: &str,
        client_pref: Option<ProtocolKind>,
        payload: Option<Value>,
    ) -> Result<Value> {
        let protocol = self
            .resolve_protocol(service, operation, client_pref)
            .await?;
        self.call_for_protocol(service, protocol, operation, payload)
            .await
    }

    pub async fn call_with_fallback(
        &self,
        service: &str,
        operation: &str,
        client_pref: Option<ProtocolKind>,
        payload: Option<Value>,
    ) -> Result<Value> {
        let primary = self
            .resolve_protocol(service, operation, client_pref)
            .await?;
        match self
            .call_for_protocol(service, primary, operation, payload.clone())
            .await
        {
            Ok(value) => Ok(value),
            Err(primary_err) => {
                tracing::warn!(
                    service = service,
                    operation = operation,
                    primary_protocol = %primary.as_str(),
                    error = %primary_err,
                    "protocol_call_failed_trying_fallback"
                );

                let caps = self.capabilities(service).await?;
                let mut candidates = vec![
                    caps.default_protocol,
                    ProtocolKind::Rest,
                    ProtocolKind::Graphql,
                    ProtocolKind::Rpc,
                ];
                candidates.retain(|p| *p != primary);
                candidates.dedup();

                let allowed = operation_allowed_protocols(operation);
                for candidate in candidates {
                    if !caps.supported_protocols.contains(&candidate)
                        || !allowed.contains(&candidate)
                    {
                        continue;
                    }
                    if let Ok(value) = self
                        .call_for_protocol(service, candidate, operation, payload.clone())
                        .await
                    {
                        return Ok(value);
                    }
                }

                Err(primary_err)
            }
        }
    }

    async fn fetch_capabilities(&self, service: &str) -> Result<ServiceCapabilities> {
        let base_url = self
            .service_urls
            .get(service)
            .ok_or_else(|| anyhow!("unknown service: {}", service))?;
        let url = format!("{}/api/v1/capabilities", base_url);
        let response = self
            .http
            .get(&url)
            .timeout(Duration::from_secs(2))
            .send()
            .await
            .with_context(|| format!("capabilities request failed for service='{}'", service))?;

        if !response.status().is_success() {
            anyhow::bail!(
                "capabilities endpoint returned non-success status={} for service='{}'",
                response.status(),
                service
            );
        }

        response
            .json::<ServiceCapabilities>()
            .await
            .with_context(|| format!("invalid capabilities payload for service='{}'", service))
    }

    async fn call_for_protocol(
        &self,
        service: &str,
        protocol: ProtocolKind,
        operation: &str,
        payload: Option<Value>,
    ) -> Result<Value> {
        match protocol {
            ProtocolKind::Rest => self.call_rest(service, operation, payload).await,
            ProtocolKind::Graphql => self.call_graphql(service, operation, payload).await,
            ProtocolKind::Rpc => self.call_rpc(service, operation, payload).await,
        }
    }

    fn resolve_base_url(&self, service: &str, protocol: ProtocolKind) -> Result<String> {
        if self.use_gateway_external_mode {
            if let Some(gateway) = &self.gateway_base_url {
                return Ok(gateway.clone());
            }
        }

        if let Some(by_protocol) = self.split_targets.get(service) {
            if let Some(url) = by_protocol.get(&protocol) {
                return Ok(url.clone());
            }
        }

        self.service_urls
            .get(service)
            .cloned()
            .ok_or_else(|| anyhow!("unknown service: {}", service))
    }

    async fn call_rest(
        &self,
        service: &str,
        operation: &str,
        _payload: Option<Value>,
    ) -> Result<Value> {
        let route = match operation {
            "users.getMe" => "/api/v1/users/me",
            "auth.status" => "/api/v1/auth/status",
            _ => anyhow::bail!("no REST mapping for operation='{}'", operation),
        };

        let base_url = self.resolve_base_url(service, ProtocolKind::Rest)?;
        let url = format!("{}{}", base_url, route);
        let mut request = self.http.get(&url);
        if let Some(token) = &self.downstream_bearer_token {
            request = request.bearer_auth(token);
        }

        let response = request.send().await?;
        if !response.status().is_success() {
            anyhow::bail!(
                "REST call failed status={} route='{}'",
                response.status(),
                route
            );
        }
        Ok(response.json::<Value>().await?)
    }

    async fn call_graphql(
        &self,
        service: &str,
        operation: &str,
        _payload: Option<Value>,
    ) -> Result<Value> {
        let query = match operation {
            "users.getMe" => "{ me { id username } }",
            _ => anyhow::bail!("no GraphQL mapping for operation='{}'", operation),
        };

        let base_url = self.resolve_base_url(service, ProtocolKind::Graphql)?;
        let url = format!("{}/api/v1/graphql", base_url);
        let mut request = self.http.post(&url).json(&json!({ "query": query }));
        if let Some(token) = &self.downstream_bearer_token {
            request = request.bearer_auth(token);
        }

        let response = request.send().await?;
        if !response.status().is_success() {
            anyhow::bail!(
                "GraphQL call failed status={} operation='{}'",
                response.status(),
                operation
            );
        }

        let value: Value = response.json().await?;
        if value.get("errors").is_some() {
            anyhow::bail!(
                "GraphQL response contains errors for operation='{}'",
                operation
            );
        }
        Ok(value)
    }

    async fn call_rpc(
        &self,
        service: &str,
        operation: &str,
        payload: Option<Value>,
    ) -> Result<Value> {
        let base_url = self.resolve_base_url(service, ProtocolKind::Rpc)?;
        let url = format!("{}/api/v1/rpc", base_url);
        let body = json!({
            "method": operation,
            "params": payload.unwrap_or_else(|| json!({})),
            "id": 1
        });

        let mut request = self.http.post(&url).json(&body);
        if let Some(token) = &self.downstream_bearer_token {
            request = request.bearer_auth(token);
        }

        let response = request.send().await?;
        if !response.status().is_success() {
            anyhow::bail!(
                "RPC call failed status={} operation='{}'",
                response.status(),
                operation
            );
        }

        let value: Value = response.json().await?;
        if let Some(err) = value.get("error") {
            anyhow::bail!("RPC error for operation='{}': {}", operation, err);
        }
        Ok(value.get("result").cloned().unwrap_or(Value::Null))
    }
}

fn operation_allowed_protocols(operation: &str) -> &'static [ProtocolKind] {
    match operation {
        "auth.status" => &[ProtocolKind::Rest],
        "users.getMe" => &[ProtocolKind::Graphql, ProtocolKind::Rest, ProtocolKind::Rpc],
        _ => &[ProtocolKind::Rest, ProtocolKind::Graphql, ProtocolKind::Rpc],
    }
}

fn fallback_capabilities(service: &str) -> ServiceCapabilities {
    let mut routes = HashMap::new();
    match service {
        "users" => {
            routes.insert(ProtocolKind::Rest, "/api/v1/users".to_string());
            routes.insert(ProtocolKind::Graphql, "/api/v1/graphql".to_string());
            routes.insert(ProtocolKind::Rpc, "/api/v1/rpc".to_string());
            ServiceCapabilities {
                service: service.to_string(),
                default_protocol: ProtocolKind::Rest,
                supported_protocols: vec![
                    ProtocolKind::Rest,
                    ProtocolKind::Graphql,
                    ProtocolKind::Rpc,
                ],
                protocol_routes: routes,
            }
        }
        _ => {
            routes.insert(ProtocolKind::Rest, format!("/api/v1/{}", service));
            ServiceCapabilities {
                service: service.to_string(),
                default_protocol: ProtocolKind::Rest,
                supported_protocols: vec![ProtocolKind::Rest],
                protocol_routes: routes,
            }
        }
    }
}

fn env_trimmed(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn bool_env(name: &str, default: bool) -> bool {
    match std::env::var(name) {
        Ok(v) if v.eq_ignore_ascii_case("1") || v.eq_ignore_ascii_case("true") => true,
        Ok(v) if v.eq_ignore_ascii_case("0") || v.eq_ignore_ascii_case("false") => false,
        Ok(_) => default,
        Err(_) => default,
    }
}

fn parse_split_targets_from_env() -> Result<HashMap<String, HashMap<ProtocolKind, String>>> {
    let Some(raw) = env_trimmed("KRAB_PROTOCOL_SPLIT_TARGETS_JSON") else {
        return Ok(HashMap::new());
    };

    let parsed: Value = serde_json::from_str(&raw)
        .context("KRAB_PROTOCOL_SPLIT_TARGETS_JSON must be valid JSON object")?;
    let mut out = HashMap::new();

    let top = parsed
        .as_object()
        .ok_or_else(|| anyhow!("KRAB_PROTOCOL_SPLIT_TARGETS_JSON must be a JSON object"))?;

    for (service, protocol_map) in top {
        let Some(protocol_map) = protocol_map.as_object() else {
            continue;
        };
        let mut per_protocol = HashMap::new();
        for (proto_key, url_val) in protocol_map {
            let Some(protocol) = ProtocolKind::parse(proto_key) else {
                continue;
            };
            let Some(url) = url_val.as_str() else {
                continue;
            };
            let normalized = url.trim().trim_end_matches('/').to_string();
            if !normalized.is_empty() {
                per_protocol.insert(protocol, normalized);
            }
        }
        if !per_protocol.is_empty() {
            out.insert(service.to_string(), per_protocol);
        }
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::State;
    use axum::routing::{get, post};
    use axum::{Json, Router};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    #[derive(Default)]
    struct MockState {
        caps_hits: AtomicUsize,
        rest_hits: AtomicUsize,
        graphql_hits: AtomicUsize,
        rpc_hits: AtomicUsize,
        fail_graphql: AtomicBool,
    }

    async fn caps_handler(State(state): State<Arc<MockState>>) -> Json<ServiceCapabilities> {
        state.caps_hits.fetch_add(1, Ordering::Relaxed);
        let mut routes = HashMap::new();
        routes.insert(ProtocolKind::Rest, "/api/v1/users".to_string());
        routes.insert(ProtocolKind::Graphql, "/api/v1/graphql".to_string());
        routes.insert(ProtocolKind::Rpc, "/api/v1/rpc".to_string());

        Json(ServiceCapabilities {
            service: "users".to_string(),
            default_protocol: ProtocolKind::Graphql,
            supported_protocols: vec![ProtocolKind::Rest, ProtocolKind::Graphql, ProtocolKind::Rpc],
            protocol_routes: routes,
        })
    }

    async fn users_me_handler(State(state): State<Arc<MockState>>) -> Json<Value> {
        state.rest_hits.fetch_add(1, Ordering::Relaxed);
        Json(json!({ "id": "u1", "username": "rest-user" }))
    }

    async fn graphql_handler(
        State(state): State<Arc<MockState>>,
    ) -> (axum::http::StatusCode, Json<Value>) {
        state.graphql_hits.fetch_add(1, Ordering::Relaxed);
        if state.fail_graphql.load(Ordering::Relaxed) {
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"errors":[{"message":"graphql failed"}]})),
            );
        }

        (
            axum::http::StatusCode::OK,
            Json(json!({ "data": { "me": { "id": "u1", "username": "graphql-user" } } })),
        )
    }

    async fn rpc_handler(State(state): State<Arc<MockState>>) -> Json<Value> {
        state.rpc_hits.fetch_add(1, Ordering::Relaxed);
        Json(json!({ "result": { "id": "u1", "username": "rpc-user" }, "id": 1 }))
    }

    async fn start_mock_server() -> (String, Arc<MockState>, tokio::task::JoinHandle<()>) {
        let state = Arc::new(MockState::default());
        let app = Router::new()
            .route("/api/v1/capabilities", get(caps_handler))
            .route("/api/v1/users/me", get(users_me_handler))
            .route("/api/v1/graphql", post(graphql_handler))
            .route("/api/v1/rpc", post(rpc_handler))
            .with_state(state.clone());

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        (format!("http://{}", addr), state, handle)
    }

    #[tokio::test]
    async fn test_capability_discovery_and_caching() {
        let (base, state, handle) = start_mock_server().await;
        let client = ProtocolAwareClient::new(
            Client::new(),
            HashMap::from([("users".to_string(), base)]),
            Duration::from_secs(60),
        );

        let _ = client.capabilities("users").await.unwrap();
        let _ = client.capabilities("users").await.unwrap();
        assert_eq!(state.caps_hits.load(Ordering::Relaxed), 1);
        handle.abort();
    }

    #[tokio::test]
    async fn test_call_rest_via_explicit_namespace() {
        let (base, state, handle) = start_mock_server().await;
        let client = ProtocolAwareClient::new(
            Client::new(),
            HashMap::from([("users".to_string(), base)]),
            Duration::from_secs(60),
        );

        let value = client
            .call("users", "users.getMe", Some(ProtocolKind::Rest), None)
            .await
            .unwrap();
        assert_eq!(value.get("id").and_then(Value::as_str), Some("u1"));
        assert_eq!(state.rest_hits.load(Ordering::Relaxed), 1);
        handle.abort();
    }

    #[tokio::test]
    async fn test_call_graphql_via_explicit_namespace() {
        let (base, state, handle) = start_mock_server().await;
        let client = ProtocolAwareClient::new(
            Client::new(),
            HashMap::from([("users".to_string(), base)]),
            Duration::from_secs(60),
        );

        let value = client
            .call("users", "users.getMe", Some(ProtocolKind::Graphql), None)
            .await
            .unwrap();
        assert_eq!(
            value
                .get("data")
                .and_then(|v| v.get("me"))
                .and_then(|v| v.get("id"))
                .and_then(Value::as_str),
            Some("u1")
        );
        assert_eq!(state.graphql_hits.load(Ordering::Relaxed), 1);
        handle.abort();
    }

    #[tokio::test]
    async fn test_call_rpc_via_explicit_namespace() {
        let (base, state, handle) = start_mock_server().await;
        let client = ProtocolAwareClient::new(
            Client::new(),
            HashMap::from([("users".to_string(), base)]),
            Duration::from_secs(60),
        );

        let value = client
            .call(
                "users",
                "users.getMe",
                Some(ProtocolKind::Rpc),
                Some(json!({})),
            )
            .await
            .unwrap();
        assert_eq!(value.get("id").and_then(Value::as_str), Some("u1"));
        assert_eq!(state.rpc_hits.load(Ordering::Relaxed), 1);
        handle.abort();
    }

    #[tokio::test]
    async fn test_fallback_on_primary_failure() {
        let (base, state, handle) = start_mock_server().await;
        state.fail_graphql.store(true, Ordering::Relaxed);

        let client = ProtocolAwareClient::new(
            Client::new(),
            HashMap::from([("users".to_string(), base)]),
            Duration::from_secs(60),
        );

        let value = client
            .call_with_fallback("users", "users.getMe", Some(ProtocolKind::Graphql), None)
            .await
            .unwrap();

        assert_eq!(value.get("id").and_then(Value::as_str), Some("u1"));
        assert!(state.graphql_hits.load(Ordering::Relaxed) >= 1);
        assert_eq!(state.rest_hits.load(Ordering::Relaxed), 1);
        handle.abort();
    }
}
