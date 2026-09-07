use std::sync::Arc;

use axum::extract::Extension;
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use krab_core::http::{ApiError, AuthContext, ErrorCategory};
use serde_json::json;

use crate::domain::models::DomainError;
use crate::domain::service::DomainService;

/// Generic over the router state so the REST routes can be merged into the
/// governed `Router<AppState>` the runtime builds. The handlers take only
/// extensions, so they never touch `S` themselves.
pub fn mount_rest_routes<S>(domain: Arc<dyn DomainService>) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/users/me", get(get_me_handler))
        .layer(Extension(domain))
}

async fn get_me_handler(
    Extension(auth): Extension<AuthContext>,
    Extension(domain): Extension<Arc<dyn DomainService>>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ApiError>)> {
    let tenant_id = auth.tenant_id.as_deref().ok_or((
        StatusCode::BAD_REQUEST,
        Json(ApiError::new(
            ErrorCategory::Validation,
            "BAD_REQUEST",
            "tenant context required",
        )),
    ))?;

    let user = domain
        .get_me(tenant_id)
        .await
        .map_err(domain_error_to_rest)?;

    Ok(Json(json!({ "id": user.id, "username": user.username })))
}

fn domain_error_to_rest(err: DomainError) -> (StatusCode, Json<ApiError>) {
    match err {
        DomainError::TenantRequired => (
            StatusCode::BAD_REQUEST,
            Json(ApiError::new(
                ErrorCategory::Validation,
                "BAD_REQUEST",
                "tenant required",
            )),
        ),
        DomainError::NotFound => (
            StatusCode::NOT_FOUND,
            Json(ApiError::new(
                ErrorCategory::NotFound,
                "NOT_FOUND",
                "user not found",
            )),
        ),
        DomainError::Unauthorized => (
            StatusCode::FORBIDDEN,
            Json(ApiError::new(
                ErrorCategory::Authz,
                "FORBIDDEN",
                "access denied",
            )),
        ),
        DomainError::Internal(msg) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiError::new(ErrorCategory::Internal, "INTERNAL", msg)),
        ),
    }
}
