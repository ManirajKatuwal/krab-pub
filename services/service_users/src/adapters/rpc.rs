use std::sync::Arc;

use axum::extract::Extension;
use axum::routing::post;
use axum::{Json, Router};
use krab_core::http::AuthContext;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::domain::service::UserDomainService;

#[derive(Deserialize)]
struct RpcRequest {
    method: String,
    #[serde(default)]
    params: serde_json::Value,
    id: Option<serde_json::Value>,
}

#[derive(Serialize)]
struct RpcResponse {
    result: Option<serde_json::Value>,
    error: Option<RpcError>,
    id: Option<serde_json::Value>,
}

#[derive(Serialize)]
struct RpcError {
    code: i32,
    message: String,
}

pub fn rpc_router(domain: Arc<dyn UserDomainService>) -> Router {
    Router::new()
        .route("/rpc", post(rpc_dispatch))
        .layer(Extension(domain))
}

async fn rpc_dispatch(
    Extension(auth): Extension<AuthContext>,
    Extension(domain): Extension<Arc<dyn UserDomainService>>,
    Json(req): Json<RpcRequest>,
) -> Json<RpcResponse> {
    let _ = &req.params;
    match req.method.as_str() {
        "users.getMe" => {
            let tenant_id = match auth.tenant_id.as_deref() {
                Some(t) => t,
                None => return rpc_error(-32602, "tenant context required", req.id),
            };

            match domain.get_me(tenant_id).await {
                Ok(user) => Json(RpcResponse {
                    result: Some(json!({ "id": user.id, "username": user.username })),
                    error: None,
                    id: req.id,
                }),
                Err(err) => rpc_error(-32000, &format!("{:?}", err), req.id),
            }
        }
        _ => rpc_error(-32601, "method not found", req.id),
    }
}

fn rpc_error(code: i32, message: &str, id: Option<serde_json::Value>) -> Json<RpcResponse> {
    Json(RpcResponse {
        result: None,
        error: Some(RpcError {
            code,
            message: message.to_string(),
        }),
        id,
    })
}
