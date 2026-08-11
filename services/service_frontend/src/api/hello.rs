use axum::response::Json;
use axum::http::StatusCode;
use serde_json::{json, Value};

pub async fn get() -> (StatusCode, Json<Value>) {
    (
        StatusCode::OK,
        Json(json!({"message": "Hello from GET /api/hello"})),
    )
}

pub async fn post(
    axum::Json(body): axum::Json<Value>,
) -> (StatusCode, Json<Value>) {
    (
        StatusCode::CREATED,
        Json(json!({"message": "Hello from POST /api/hello", "received": body})),
    )
}
