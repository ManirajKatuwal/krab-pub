//# middleware: crate::middleware_probe_first::<S>
//# middleware: crate::middleware_probe_second::<S>

use axum::response::Json;
use serde_json::{json, Value};

pub async fn get() -> Json<Value> {
    Json(json!({
        "route": "/api/middleware_probe",
        "status": "ok"
    }))
}
