use async_graphql_axum::{GraphQLRequest, GraphQLResponse};
use axum::extract::Extension;
use axum::{routing::get, routing::post, Json, Router};
use krab_core::http::AuthContext;
use serde_json::json;
use std::sync::Arc;

pub mod adapters;
pub mod domain;

use crate::domain::service::DomainService;

async fn health() -> Json<serde_json::Value> {
    Json(json!({"status": "ok", "service": "service_users_split"}))
}

async fn ready() -> Json<serde_json::Value> {
    Json(json!({"status": "ready", "service": "service_users_split"}))
}

async fn graphql_handler(
    Extension(auth_ctx): Extension<AuthContext>,
    Extension(schema): Extension<adapters::graphql::UsersSchema>,
    req: GraphQLRequest,
) -> GraphQLResponse {
    GraphQLResponse::from(schema.execute(req.into_inner().data(auth_ctx)).await)
}

fn build_api_router(domain: Arc<dyn DomainService>) -> Router {
    let schema = adapters::graphql::build_schema(domain.clone());

    Router::new()
        .route("/graphql", post(graphql_handler))
        .merge(adapters::rest::mount_rest_routes(domain))
        .layer(Extension(schema))
}

pub fn build_app(domain: Arc<dyn DomainService>) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/ready", get(ready))
        .nest("/api/v1", build_api_router(domain))
}
