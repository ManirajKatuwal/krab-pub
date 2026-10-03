use krab_core::service_contract::{
    DomainError, DomainErrorKind, NewUserRequest, ServiceEndpoint, ServiceTopology,
    TopologyRuntime, UserRecord, UsersServiceContract,
};
use reqwest::Client;
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsersAdapterKind {
    LocalInProcess,
    RemoteRest,
}

impl UsersAdapterKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LocalInProcess => "local_in_process",
            Self::RemoteRest => "remote_rest",
        }
    }
}

pub struct UsersAdapterBundle {
    pub kind: UsersAdapterKind,
    pub adapter: Arc<dyn UsersServiceContract>,
}

/// The in-process adapter, for tests.
#[cfg(test)]
pub(crate) fn local_users_adapter() -> Arc<dyn UsersServiceContract> {
    Arc::new(LocalUsersAdapter)
}

/// HTTP status for a users-contract failure.
fn status_for(kind: DomainErrorKind) -> axum::http::StatusCode {
    use axum::http::StatusCode;
    match kind {
        DomainErrorKind::Validation => StatusCode::BAD_REQUEST,
        DomainErrorKind::Unauthorized => StatusCode::UNAUTHORIZED,
        DomainErrorKind::Forbidden => StatusCode::FORBIDDEN,
        DomainErrorKind::NotFound => StatusCode::NOT_FOUND,
        DomainErrorKind::Conflict => StatusCode::CONFLICT,
        DomainErrorKind::Timeout => StatusCode::GATEWAY_TIMEOUT,
        DomainErrorKind::UpstreamUnavailable => StatusCode::BAD_GATEWAY,
        DomainErrorKind::Internal => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

fn domain_error_response(err: DomainError) -> axum::response::Response {
    use axum::response::IntoResponse;
    (
        status_for(err.kind),
        axum::Json(serde_json::json!({ "code": err.code, "message": err.message })),
    )
        .into_response()
}

/// `GET /api/users/{id}` through whichever adapter the topology selected —
/// in-process in a single topology, the users service over REST in a
/// distributed one. The adapter was built at startup but only its `kind` was
/// ever read, so the topology selection it encodes had no observable effect.
///
/// The lookup is made on behalf of the caller: their `Authorization` header
/// is what the users service sees, never the frontend's own service token —
/// otherwise every caller would be answered with what the service account
/// may read (`/api/users/me` returned the service account's record).
pub(crate) async fn get_user_handler(
    axum::extract::State(state): axum::extract::State<crate::app_state::AppState>,
    headers: axum::http::HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let authorization = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok());
    match state.users.get_user_on_behalf_of(&id, authorization).await {
        Ok(user) => axum::Json(user).into_response(),
        Err(err) => domain_error_response(err),
    }
}

/// `POST /api/users` through the selected adapter.
pub(crate) async fn create_user_handler(
    axum::extract::State(state): axum::extract::State<crate::app_state::AppState>,
    axum::Json(request): axum::Json<NewUserRequest>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    match state.users.create_user(request).await {
        Ok(user) => (axum::http::StatusCode::CREATED, axum::Json(user)).into_response(),
        Err(err) => domain_error_response(err),
    }
}

pub fn build_users_adapter(
    topology: &TopologyRuntime,
    users_base_url: String,
    downstream_bearer_token: Option<String>,
) -> UsersAdapterBundle {
    if topology.mode == ServiceTopology::Distributed {
        let endpoint = topology
            .endpoint_for("users")
            .cloned()
            .unwrap_or(ServiceEndpoint {
                base_url: users_base_url,
                ..ServiceEndpoint::default()
            });

        let adapter = RemoteUsersRestAdapter::new(endpoint, downstream_bearer_token);
        return UsersAdapterBundle {
            kind: UsersAdapterKind::RemoteRest,
            adapter: Arc::new(adapter),
        };
    }

    UsersAdapterBundle {
        kind: UsersAdapterKind::LocalInProcess,
        adapter: Arc::new(LocalUsersAdapter),
    }
}

struct LocalUsersAdapter;

#[async_trait::async_trait]
impl UsersServiceContract for LocalUsersAdapter {
    async fn get_user(&self, id: &str) -> Result<UserRecord, DomainError> {
        let user_id = id.trim();
        if user_id.is_empty() {
            return Err(DomainError::new(
                DomainErrorKind::Validation,
                "users.id_required",
                "user id is required",
            ));
        }

        Ok(UserRecord {
            id: user_id.to_string(),
            email: format!("{}@local.krab", user_id),
            display_name: format!("local_{}", user_id),
        })
    }

    async fn create_user(&self, request: NewUserRequest) -> Result<UserRecord, DomainError> {
        let email = request.email.trim();
        let display_name = request.display_name.trim();
        if email.is_empty() || display_name.is_empty() {
            return Err(DomainError::new(
                DomainErrorKind::Validation,
                "users.invalid_payload",
                "email and display_name are required",
            ));
        }

        Ok(UserRecord {
            id: format!("local-{}", display_name.to_ascii_lowercase()),
            email: email.to_string(),
            display_name: display_name.to_string(),
        })
    }
}

struct RemoteUsersRestAdapter {
    http_client: Client,
    base_url: String,
    downstream_bearer_token: Option<String>,
    max_retries: u8,
}

impl RemoteUsersRestAdapter {
    fn new(endpoint: ServiceEndpoint, downstream_bearer_token: Option<String>) -> Self {
        let timeout = Duration::from_millis(endpoint.timeout_ms.max(100));
        let http_client = Client::builder()
            .timeout(timeout)
            .build()
            .unwrap_or_else(|_| Client::new());

        Self {
            http_client,
            base_url: endpoint.base_url.trim().trim_end_matches('/').to_string(),
            downstream_bearer_token,
            max_retries: endpoint.max_retries,
        }
    }

    /// The client-facing error for a failed request. Deliberately generic:
    /// reqwest's error text names the internal users URL, which is logged
    /// here rather than returned to the caller.
    fn request_failed_error(err: &reqwest::Error) -> DomainError {
        tracing::warn!(
            event = "users_remote_request_failed",
            error = %err,
            timeout = err.is_timeout(),
            connect = err.is_connect(),
        );
        DomainError::new(
            DomainErrorKind::UpstreamUnavailable,
            "users.remote.request_failed",
            "users service request failed",
        )
    }

    fn retry_backoff(attempt: usize) -> Duration {
        let base_ms = 50_u64.saturating_mul((attempt as u64) + 1);
        Duration::from_millis(base_ms.min(250))
    }

    fn should_retry_status(status: reqwest::StatusCode) -> bool {
        status == reqwest::StatusCode::REQUEST_TIMEOUT
            || status == reqwest::StatusCode::TOO_MANY_REQUESTS
            || status.is_server_error()
    }

    fn should_retry_error(err: &reqwest::Error) -> bool {
        err.is_timeout() || err.is_connect() || err.is_request()
    }

    async fn send_get_with_retry(
        &self,
        url: &str,
        credential: Credential<'_>,
    ) -> Result<reqwest::Response, DomainError> {
        let max_attempts = usize::from(self.max_retries) + 1;

        for attempt in 0..max_attempts {
            let mut request = self.http_client.get(url);
            match credential {
                Credential::ServiceAccount => {
                    if let Some(token) = &self.downstream_bearer_token {
                        request = request.bearer_auth(token);
                    }
                }
                // On behalf of a caller: their credential or none, never the
                // service token.
                Credential::Caller(Some(authorization)) => {
                    request = request.header(reqwest::header::AUTHORIZATION, authorization);
                }
                Credential::Caller(None) => {}
            }

            match request.send().await {
                Ok(response)
                    if attempt + 1 < max_attempts
                        && Self::should_retry_status(response.status()) =>
                {
                    tokio::time::sleep(Self::retry_backoff(attempt)).await;
                }
                Ok(response) => return Ok(response),
                Err(err) if attempt + 1 < max_attempts && Self::should_retry_error(&err) => {
                    tokio::time::sleep(Self::retry_backoff(attempt)).await;
                }
                Err(err) => return Err(Self::request_failed_error(&err)),
            }
        }

        Err(DomainError::new(
            DomainErrorKind::UpstreamUnavailable,
            "users.remote.request_failed",
            "users REST request exhausted retry budget",
        ))
    }
}

/// Whose identity a remote users call carries.
#[derive(Clone, Copy)]
enum Credential<'a> {
    /// The frontend's own service token (`KRAB_FRONTEND_DOWNSTREAM_BEARER_TOKEN`),
    /// for calls not made on behalf of a user.
    ServiceAccount,
    /// The end caller's `Authorization` header value, forwarded as-is.
    Caller(Option<&'a str>),
}

impl RemoteUsersRestAdapter {
    async fn fetch_user(
        &self,
        id: &str,
        credential: Credential<'_>,
    ) -> Result<UserRecord, DomainError> {
        if id != "me" {
            return Err(DomainError::new(
                DomainErrorKind::Validation,
                "users.remote.unsupported_lookup",
                "remote users adapter currently supports id='me' only",
            ));
        }

        let url = format!("{}/api/v1/users/me", self.base_url);
        let response = self.send_get_with_retry(&url, credential).await?;

        match response.status() {
            reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN => {
                return Err(DomainError::new(
                    DomainErrorKind::Unauthorized,
                    "users.remote.unauthorized",
                    "downstream users service rejected authorization",
                ));
            }
            reqwest::StatusCode::NOT_FOUND => {
                return Err(DomainError::new(
                    DomainErrorKind::NotFound,
                    "users.remote.not_found",
                    "user not found",
                ));
            }
            status if !status.is_success() => {
                return Err(DomainError::new(
                    DomainErrorKind::UpstreamUnavailable,
                    "users.remote.bad_status",
                    format!("downstream users service returned status {}", status),
                ));
            }
            _ => {}
        }

        let payload: Value = response.json().await.map_err(|err| {
            tracing::warn!(event = "users_remote_payload_invalid", error = %err);
            DomainError::new(
                DomainErrorKind::Internal,
                "users.remote.invalid_payload",
                "users service returned an unreadable payload",
            )
        })?;

        let resolved_id = payload
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| "unknown".to_string());

        let display_name = payload
            .get("username")
            .and_then(Value::as_str)
            .or_else(|| payload.get("display_name").and_then(Value::as_str))
            .unwrap_or("user")
            .to_string();

        let email = payload
            .get("email")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();

        Ok(UserRecord {
            id: resolved_id,
            email,
            display_name,
        })
    }
}

#[async_trait::async_trait]
impl UsersServiceContract for RemoteUsersRestAdapter {
    async fn get_user(&self, id: &str) -> Result<UserRecord, DomainError> {
        self.fetch_user(id, Credential::ServiceAccount).await
    }

    async fn get_user_on_behalf_of(
        &self,
        id: &str,
        authorization: Option<&str>,
    ) -> Result<UserRecord, DomainError> {
        self.fetch_user(id, Credential::Caller(authorization)).await
    }

    async fn create_user(&self, _request: NewUserRequest) -> Result<UserRecord, DomainError> {
        Err(DomainError::new(
            DomainErrorKind::Forbidden,
            "users.remote.read_only",
            "remote users adapter is currently read-only",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::State;
    use axum::http::StatusCode;
    use axum::routing::get;
    use axum::{Json, Router};
    use serde_json::json;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn build_users_adapter_selects_local_for_monolith() {
        let topology = TopologyRuntime::default();
        let bundle = build_users_adapter(&topology, "http://127.0.0.1:3002".to_string(), None);
        assert_eq!(bundle.kind, UsersAdapterKind::LocalInProcess);
    }

    #[test]
    fn build_users_adapter_selects_remote_for_distributed() {
        let topology = TopologyRuntime {
            mode: ServiceTopology::Distributed,
            endpoints: HashMap::from([(
                "users".to_string(),
                ServiceEndpoint {
                    base_url: "http://127.0.0.1:3002".to_string(),
                    timeout_ms: 900,
                    max_retries: 1,
                },
            )]),
        };

        let bundle = build_users_adapter(&topology, "http://127.0.0.1:3002".to_string(), None);
        assert_eq!(bundle.kind, UsersAdapterKind::RemoteRest);
    }

    #[tokio::test]
    async fn remote_rest_adapter_fetches_user_me() {
        async fn users_me() -> Json<serde_json::Value> {
            Json(json!({ "id": "u-1", "username": "alice" }))
        }

        let app = Router::new().route("/api/v1/users/me", get(users_me));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let topology = TopologyRuntime {
            mode: ServiceTopology::Distributed,
            endpoints: HashMap::from([(
                "users".to_string(),
                ServiceEndpoint {
                    base_url: format!("http://{}", addr),
                    timeout_ms: 900,
                    max_retries: 0,
                },
            )]),
        };

        let bundle = build_users_adapter(&topology, format!("http://{}", addr), None);
        let user = bundle.adapter.get_user("me").await.unwrap();
        assert_eq!(user.id, "u-1");
        assert_eq!(user.display_name, "alice");

        handle.abort();
    }

    #[tokio::test]
    async fn remote_rest_adapter_maps_unauthorized_status() {
        async fn users_me_unauthorized() -> (StatusCode, Json<serde_json::Value>) {
            (
                StatusCode::UNAUTHORIZED,
                Json(json!({ "error": "unauthorized" })),
            )
        }

        let app = Router::new().route("/api/v1/users/me", get(users_me_unauthorized));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let topology = TopologyRuntime {
            mode: ServiceTopology::Distributed,
            endpoints: HashMap::from([(
                "users".to_string(),
                ServiceEndpoint {
                    base_url: format!("http://{}", addr),
                    timeout_ms: 900,
                    max_retries: 0,
                },
            )]),
        };

        let bundle = build_users_adapter(&topology, format!("http://{}", addr), None);
        let err = bundle.adapter.get_user("me").await.unwrap_err();
        assert_eq!(err.kind, DomainErrorKind::Unauthorized);
        assert_eq!(err.code, "users.remote.unauthorized");

        handle.abort();
    }

    #[tokio::test]
    async fn remote_rest_adapter_maps_invalid_payload_as_internal() {
        async fn users_me_invalid_payload() -> (StatusCode, &'static str) {
            (StatusCode::OK, "this-is-not-json")
        }

        let app = Router::new().route("/api/v1/users/me", get(users_me_invalid_payload));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let topology = TopologyRuntime {
            mode: ServiceTopology::Distributed,
            endpoints: HashMap::from([(
                "users".to_string(),
                ServiceEndpoint {
                    base_url: format!("http://{}", addr),
                    timeout_ms: 900,
                    max_retries: 0,
                },
            )]),
        };

        let bundle = build_users_adapter(&topology, format!("http://{}", addr), None);
        let err = bundle.adapter.get_user("me").await.unwrap_err();
        assert_eq!(err.kind, DomainErrorKind::Internal);
        assert_eq!(err.code, "users.remote.invalid_payload");

        handle.abort();
    }

    #[tokio::test]
    async fn remote_rest_adapter_maps_unreachable_upstream_as_unavailable() {
        let topology = TopologyRuntime {
            mode: ServiceTopology::Distributed,
            endpoints: HashMap::from([(
                "users".to_string(),
                ServiceEndpoint {
                    base_url: "http://127.0.0.1:1".to_string(),
                    timeout_ms: 200,
                    max_retries: 0,
                },
            )]),
        };

        let bundle = build_users_adapter(&topology, "http://127.0.0.1:1".to_string(), None);
        let err = bundle.adapter.get_user("me").await.unwrap_err();
        assert_eq!(err.kind, DomainErrorKind::UpstreamUnavailable);
        assert_eq!(err.code, "users.remote.request_failed");
    }

    #[tokio::test]
    async fn remote_rest_adapter_retries_retryable_statuses() {
        async fn flaky_users_me(
            State(attempts): State<Arc<AtomicUsize>>,
        ) -> (StatusCode, Json<serde_json::Value>) {
            let call = attempts.fetch_add(1, Ordering::Relaxed);
            if call == 0 {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(json!({ "error": "retry" })),
                );
            }

            (
                StatusCode::OK,
                Json(json!({ "id": "u-1", "username": "alice" })),
            )
        }

        let attempts = Arc::new(AtomicUsize::new(0));
        let app = Router::new()
            .route("/api/v1/users/me", get(flaky_users_me))
            .with_state(attempts.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let topology = TopologyRuntime {
            mode: ServiceTopology::Distributed,
            endpoints: HashMap::from([(
                "users".to_string(),
                ServiceEndpoint {
                    base_url: format!("http://{}", addr),
                    timeout_ms: 900,
                    max_retries: 1,
                },
            )]),
        };

        let bundle = build_users_adapter(&topology, format!("http://{}", addr), None);
        let user = bundle.adapter.get_user("me").await.unwrap();
        assert_eq!(user.id, "u-1");
        assert_eq!(attempts.load(Ordering::Relaxed), 2);

        handle.abort();
    }
}
