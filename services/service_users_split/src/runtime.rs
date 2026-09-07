//! Runtime state and router assembly.
//!
//! This mirrors `services/service_users/src/runtime.rs`: an [`AppState`] that
//! implements [`HasRuntimeState`], and a `build_app` that finishes with
//! [`apply_common_http_layers`]. That last call is not decoration — it is the
//! only thing in the workspace that inserts the `AuthContext` extension the
//! adapters extract. Without it every `/api/v1/*` route answered 500 with a
//! missing-extension rejection while `/health` and `/ready` stayed green, so
//! the orchestrator reported a healthy service whose entire API was dead.

use anyhow::Result;
use async_graphql_axum::{GraphQLRequest, GraphQLResponse};
use axum::extract::State;
use axum::routing::{get, post};
use axum::{Extension, Router};
use krab_core::http::{
    apply_common_http_layers, health, metrics, metrics_prometheus, readiness, AuthContext,
    HasRuntimeState, RuntimeState,
};
use krab_core::protocol::{ProtocolConfig, ProtocolKind};
use std::sync::Arc;

use crate::adapters;
use crate::domain::service::DomainService;

#[derive(Clone)]
pub struct AppState {
    pub runtime: RuntimeState,
    pub schema: adapters::graphql::UsersSchema,
    pub domain: Arc<dyn DomainService>,
    pub protocol_config: ProtocolConfig,
}

impl AppState {
    /// Build the state a boot path should use.
    ///
    /// [`RuntimeState::try_new`] rather than `RuntimeState::new`: the lenient
    /// constructor downgrades an unreachable `KRAB_REDIS_URL` to a per-process
    /// `MemoryStore` in every environment, which silently turns distributed
    /// auth-failure and revocation state back into per-replica state.
    pub fn try_new(
        domain: Arc<dyn DomainService>,
        protocol_config: ProtocolConfig,
    ) -> Result<Self> {
        let schema = adapters::graphql::build_schema(domain.clone());
        let runtime = RuntimeState::try_new()?.with_protocol_config(protocol_config.clone());

        Ok(Self {
            runtime,
            schema,
            domain,
            protocol_config,
        })
    }
}

impl HasRuntimeState for AppState {
    fn runtime_state(&self) -> &RuntimeState {
        &self.runtime
    }
}

async fn graphql_handler(
    State(state): State<AppState>,
    Extension(auth_ctx): Extension<AuthContext>,
    req: GraphQLRequest,
) -> GraphQLResponse {
    GraphQLResponse::from(state.schema.execute(req.into_inner().data(auth_ctx)).await)
}

/// Assemble the router, protocol adapters included, and wrap it in the common
/// governance layers.
pub fn build_app(state: AppState) -> Router {
    let enabled = &state.protocol_config.enabled_protocols;

    let mut api: Router<AppState> = Router::new();
    if enabled.contains(&ProtocolKind::Graphql) {
        api = api.route("/graphql", post(graphql_handler));
    }
    if enabled.contains(&ProtocolKind::Rest) {
        api = api.merge(adapters::rest::mount_rest_routes::<AppState>(
            state.domain.clone(),
        ));
    }

    let app = Router::new()
        .route("/health", get(health))
        .route("/ready", get(readiness))
        .route("/metrics", get(metrics::<AppState>))
        .route("/metrics/prometheus", get(metrics_prometheus::<AppState>))
        .nest("/api/v1", api);

    apply_common_http_layers(app, state.clone()).with_state(state)
}
