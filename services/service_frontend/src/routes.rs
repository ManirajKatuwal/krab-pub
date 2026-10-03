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
        // Telemetry, the same handlers every other service mounts. Not on the
        // public-path list: anonymous only with `KRAB_METRICS_PUBLIC=true`.
        // Before these existed, `GET /metrics` fell through to `/{locale}`.
        .route("/metrics", get(krab_core::http::metrics::<AppState>))
        .route(
            "/metrics/prometheus",
            get(krab_core::http::metrics_prometheus::<AppState>),
        )
        // Authenticated: not on the public-path list.
        .route(
            "/api/users/{id}",
            get(crate::users_contract::get_user_handler),
        )
        .route(
            "/api/users",
            axum::routing::post(crate::users_contract::create_user_handler),
        )
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
        // Progressive streaming SSR demo (ADR 0017) and its swap runtime.
        .route("/streaming", get(crate::streaming::streaming_handler))
        .route(
            "/_krab/stream.js",
            get(crate::streaming::stream_script_handler),
        )
        // The home page's hydration runtime, external so the CSP's
        // `script-src 'self'` allows it.
        .route(
            "/_krab/home.js",
            get(|| async {
                (
                    [(
                        axum::http::header::CONTENT_TYPE,
                        "application/javascript; charset=utf-8",
                    )],
                    crate::home_runtime_js(),
                )
            }),
        )
        // The contact form's submit handler, external for the same reason.
        .route(
            "/_krab/contact.js",
            get(|| async {
                (
                    [(
                        axum::http::header::CONTENT_TYPE,
                        "application/javascript; charset=utf-8",
                    )],
                    CONTACT_SCRIPT,
                )
            }),
        )
}

/// The `/contact` page's submit handler, served as `/_krab/contact.js`.
///
/// It used to be an inline `<script>` wired up with `onsubmit=`, both of
/// which Krab's CSP (`script-src 'self' 'wasm-unsafe-eval'`) blocks. Fields
/// are read by id: `form.name` is the form's own `name` attribute, not the
/// input called `name`.
pub(crate) const CONTACT_SCRIPT: &str = r#"const form = document.getElementById('contact-form');
const result = document.getElementById('contact-result');

async function submitContact(event) {
    event.preventDefault();
    const payload = {
        name: document.getElementById('name').value,
        email: document.getElementById('email').value,
        message: document.getElementById('message').value,
    };

    try {
        const response = await fetch('/api/contact', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify(payload),
        });
        const data = await response.json().catch(() => ({}));
        if (response.ok) {
            result.textContent = 'Message queued successfully.';
            result.dataset.state = 'success';
        } else {
            result.textContent = data.message || 'Submission failed.';
            result.dataset.state = 'error';
        }
    } catch (err) {
        result.textContent = 'Submission failed due to network error.';
        result.dataset.state = 'error';
        console.error('contact submission failed', err);
    }
}

if (form && result) {
    form.addEventListener('submit', submitContact);
}
"#;

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
