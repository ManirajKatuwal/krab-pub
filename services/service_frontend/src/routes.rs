use axum::extract::Path;
use axum::response::Html;
use axum::routing::{get, post};
use axum::Router;
use std::collections::HashMap;

use crate::app_state::AppState;
use crate::rendering::{render_about_page, render_blog_page, render_greet_page};
use crate::ws::{ws_chat_handler, ws_publish_handler};
use crate::{
    api_status_handler, asset_manifest_json, dashboard_handler, health_handler, hmr_handler,
    home_handler, localized_home_handler, ready_handler, robots_txt_handler, rpc_now_json,
    rpc_version_json, sitemap_xml_handler, submit_contact_handler,
};

pub(crate) fn register_frontend_routes(app: Router<AppState>) -> Router<AppState> {
    app
        // Critical probes defined FIRST to ensure they are available
        .route("/health", get(health_handler))
        .route("/ready", get(ready_handler))
        .route("/api/status", get(api_status_handler))
        // Application routes
        .route("/", get(home_handler))
        .route("/{locale}", get(localized_home_handler))
        .route(
            "/about",
            get(|| async { render_blocking("about", render_about_page).await }),
        )
        .route(
            "/greet",
            get(|| async { render_blocking("greet", render_greet_page).await }),
        )
        .route(
            "/blog/{slug}",
            get(|Path(params): Path<HashMap<String, String>>| async move {
                let slug = params
                    .get("slug")
                    .map(|s| s.as_str())
                    .unwrap_or("unknown")
                    .to_string();
                render_blocking("blog", move || render_blog_page(&slug)).await
            }),
        )
        .route("/robots.txt", get(robots_txt_handler))
        .route("/sitemap.xml", get(sitemap_xml_handler))
        .route("/data/dashboard", get(dashboard_handler))
        .route(
            "/asset-manifest.json",
            get(|| async { asset_manifest_json() }),
        )
        .route("/rpc/now", get(|| async { rpc_now_json() }))
        .route("/rpc/version", get(|| async { rpc_version_json() }))
        .route("/api/ws/chat", get(ws_chat_handler))
        .route("/api/ws/publish", post(ws_publish_handler))
        .route("/api/contact", post(submit_contact_handler))
        .route("/api/hmr", get(hmr_handler))
}

/// Run a render on the blocking pool and map a failed join to a 500.
///
/// `spawn_blocking(..).await.unwrap()` was the shape here: a panic inside the
/// render function became a panic in the handler, which tore down the
/// connection with no response. The home handlers in `main.rs` were fixed to
/// map that case to `500 render_task_failed`; these three were the same defect
/// at a second site and are now the same fix.
async fn render_blocking<F>(
    route: &'static str,
    render: F,
) -> Result<Html<String>, (axum::http::StatusCode, &'static str)>
where
    F: FnOnce() -> String + Send + 'static,
{
    match tokio::task::spawn_blocking(render).await {
        Ok(html) => Ok(Html(html)),
        Err(err) => {
            tracing::error!(%err, route, "render_task_failed");
            Err((
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "render_task_failed",
            ))
        }
    }
}
