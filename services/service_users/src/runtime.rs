use anyhow::Result;
use async_graphql::{EmptyMutation, EmptySubscription, Schema};
use async_graphql_axum::{GraphQLRequest, GraphQLResponse};
use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use krab_core::http::AuthContext;
use krab_core::http::{
    apply_common_http_layers, health, metrics, metrics_prometheus, readiness_with_dependencies,
    DependencyStatus, HasReadinessDependencies, HasRuntimeState, RuntimeState,
};
use krab_core::protocol::{ProtocolConfig, ProtocolKind, ServiceCapabilities};
use serde::Serialize;
use std::sync::Arc;

use crate::adapters;
use crate::db::bootstrap::UsersDbPool;
use crate::domain::service::UserDomainService;

pub(crate) type UsersSchema =
    Schema<adapters::graphql::UserQuery, EmptyMutation, EmptySubscription>;

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) schema: UsersSchema,
    pub(crate) pool: UsersDbPool,
    pub(crate) runtime: RuntimeState,
    pub(crate) domain: Arc<dyn UserDomainService>,
    pub(crate) protocol_config: ProtocolConfig,
    pub(crate) capabilities: ServiceCapabilities,
}

#[derive(Serialize)]
struct StatusPayload {
    status: &'static str,
}

async fn root() -> &'static str {
    "Users Service Online"
}

impl HasReadinessDependencies for AppState {
    fn readiness_dependencies(&self) -> Vec<DependencyStatus> {
        let db_ready = self.pool.try_acquire_available();
        vec![DependencyStatus {
            name: self.pool.dependency_name(),
            ready: db_ready,
            critical: true,
            latency_ms: None,
            detail: Some(if db_ready {
                "connection-pool-available".to_string()
            } else {
                "connection-pool-unavailable".to_string()
            }),
        }]
    }
}

async fn graphql_handler(
    State(state): State<AppState>,
    Extension(auth_ctx): Extension<AuthContext>,
    req: GraphQLRequest,
) -> GraphQLResponse {
    let request = req.into_inner().data(auth_ctx);
    let response = state.schema.execute(request).await;
    GraphQLResponse::from(response)
}

async fn capabilities_handler(State(state): State<AppState>) -> Json<ServiceCapabilities> {
    Json(state.capabilities.clone())
}

fn has_admin_entitlement(auth: &AuthContext) -> bool {
    let admin_scope =
        std::env::var("KRAB_AUTH_ADMIN_SCOPE").unwrap_or_else(|_| "admin".to_string());
    let admin_role = std::env::var("KRAB_AUTH_ADMIN_ROLE").unwrap_or_else(|_| "admin".to_string());
    auth.scopes.iter().any(|s| s == &admin_scope) || auth.roles.iter().any(|r| r == &admin_role)
}

async fn admin_audit_handler(
    Extension(auth_ctx): Extension<AuthContext>,
) -> (StatusCode, Json<StatusPayload>) {
    if !has_admin_entitlement(&auth_ctx) {
        return (
            StatusCode::FORBIDDEN,
            Json(StatusPayload {
                status: "forbidden",
            }),
        );
    }

    (StatusCode::OK, Json(StatusPayload { status: "admin_ok" }))
}

async fn admin_rbac_middleware(req: Request, next: Next) -> Result<Response, StatusCode> {
    let authorized = req
        .extensions()
        .get::<AuthContext>()
        .map(has_admin_entitlement)
        .unwrap_or(false);

    if !authorized {
        return Err(StatusCode::FORBIDDEN);
    }

    Ok(next.run(req).await)
}

impl HasRuntimeState for AppState {
    fn runtime_state(&self) -> &RuntimeState {
        &self.runtime
    }
}

pub(crate) fn build_app(state: AppState) -> Router {
    let domain = state.domain.clone();
    let proto_cfg = state.protocol_config.clone();

    let admin_api = Router::new()
        .route("/audit", get(admin_audit_handler))
        .route_layer(middleware::from_fn(admin_rbac_middleware));

    let mut api = Router::new();

    if proto_cfg.enabled_protocols.contains(&ProtocolKind::Graphql) {
        api = api.route("/graphql", post(graphql_handler));
    }
    if proto_cfg.enabled_protocols.contains(&ProtocolKind::Rest) {
        api = api.merge(adapters::rest::rest_router(domain.clone()).with_state(()));
    }
    if proto_cfg.enabled_protocols.contains(&ProtocolKind::Rpc) {
        api = api.merge(adapters::rpc::rpc_router(domain.clone()).with_state(()));
    }

    api = api
        .nest("/admin", admin_api)
        .route("/capabilities", get(capabilities_handler));

    let app = Router::new()
        .route("/", get(root))
        .route("/health", get(health))
        .route("/ready", get(readiness_with_dependencies::<AppState>))
        .route("/metrics", get(metrics::<AppState>))
        .route("/metrics/prometheus", get(metrics_prometheus::<AppState>))
        .nest("/api/v1", api);

    apply_common_http_layers(app, state.clone()).with_state(state)
}
