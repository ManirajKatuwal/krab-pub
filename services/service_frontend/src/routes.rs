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
            get(|| async {
                let html = tokio::task::spawn_blocking(render_about_page)
                    .await
                    .unwrap();
                Html(html)
            }),
        )
        .route(
            "/greet",
            get(|| async {
                let html = tokio::task::spawn_blocking(render_greet_page)
                    .await
                    .unwrap();
                Html(html)
            }),
        )
        .route(
            "/blog/{slug}",
            get(|Path(params): Path<HashMap<String, String>>| async move {
                let slug = params
                    .get("slug")
                    .map(|s| s.as_str())
                    .unwrap_or("unknown")
                    .to_string();
                let html = tokio::task::spawn_blocking(move || render_blog_page(&slug))
                    .await
                    .unwrap();
                Html(html)
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
