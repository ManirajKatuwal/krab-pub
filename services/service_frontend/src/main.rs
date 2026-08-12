use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::Html;
use axum::{Json, Router};
use krab_client::components::{Counter, CounterProps, Likes, LikesProps, Toggle, ToggleProps};
use krab_core::config::KrabConfig;
use krab_core::error_boundary::ErrorBoundary;
use krab_core::http::{apply_common_http_layers, HasRuntimeState, RuntimeState};
use krab_core::i18n::{detect_locale_from_header, I18n, Locale, TranslationBundle};
use krab_core::isr::{IsrCache, IsrPolicy};
use krab_core::render_stream::{ChunkedStreamWriter, SuspenseState};
use krab_core::service::{serve_with_graceful_shutdown, ServiceConfig};
use krab_core::service_contract::TopologyRuntime;
use krab_core::telemetry::init_tracing;
use krab_core::Render;
use krab_macros::view;
use reqwest::Client;
use serde::Deserialize;
use serde_json::json;
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::Duration;
use tracing::warn;

mod app_state;
mod cache;
mod frontend_env;
mod protocol_client;
mod render_policy;
mod rendering;
mod routes;
mod users_contract;
mod ws;
use crate::app_state::AppState;
use crate::cache::{cache_middleware, is_finalized_ssr_snapshot};
#[cfg(test)]
use crate::frontend_env::normalize_service_base_url;
use crate::frontend_env::{
    bool_env, env_trimmed, hydration_budget_for_route, hydration_preload_links_html,
    isr_revalidate_duration, normalize_public_base_url, resolve_service_base_url,
    stream_budget_bytes, u64_env, HydrationMode,
};
use crate::protocol_client::ProtocolAwareClient;
use crate::render_policy::page_render_policy;
use crate::rendering::{canonical_url, render_about_page, render_blog_page, render_greet_page};
use crate::routes::register_frontend_routes;

const SERVER_FUNCTION_VERSION: &str = "2026-02-27.1";

#[cfg(test)]
fn distributed_cache_key(uri: &str) -> String {
    crate::cache::distributed_cache_key(uri)
}

#[cfg(test)]
fn distributed_cache_ttl() -> Duration {
    crate::frontend_env::distributed_cache_ttl()
}

fn i18n_bundle() -> TranslationBundle {
    let mut bundle = TranslationBundle::new();
    bundle.add_locale(
        Locale::new("en", "English"),
        vec![
            ("home_title", "Krab Framework"),
            ("hello", "Hello from Krab!"),
            ("rendered", "This is rendered server-side."),
        ],
    );
    bundle.add_locale(
        Locale::new("ne", "नेपाली"),
        vec![
            ("home_title", "क्र्याब फ्रेमवर्क"),
            ("hello", "क्र्याबबाट नमस्ते!"),
            ("rendered", "यो सर्भर-साइडबाट रेन्डर गरिएको हो।"),
        ],
    );
    bundle
}

fn i18n_for(locale: &str) -> I18n {
    I18n::new(i18n_bundle(), "en").with_locale(locale)
}

fn resolve_locale(headers: &HeaderMap) -> String {
    let supported = i18n_bundle().supported_locales().to_vec();
    let from_header = headers
        .get(axum::http::header::ACCEPT_LANGUAGE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| detect_locale_from_header(v, &supported));
    from_header.unwrap_or_else(|| "en".to_string())
}

fn render_isr_path(path: &str) -> Option<String> {
    let policy = page_render_policy(path)?;
    if !policy.is_isr() {
        return None;
    }

    if path == "/" {
        return Some(render_home_page());
    }
    if path == "/about" {
        return Some(render_about_page());
    }
    if path == "/greet" {
        return Some(render_greet_page());
    }
    if let Some(slug) = path.strip_prefix("/blog/") {
        return Some(render_blog_page(slug));
    }
    None
}

async fn trigger_isr_revalidation(state: AppState, cache_key: String, path: String) {
    {
        let mut in_progress = state.isr_revalidating.lock().await;
        if !in_progress.insert(cache_key.clone()) {
            return;
        }
    }

    if let Some(html) = render_isr_path(&path) {
        if is_finalized_ssr_snapshot(&html) {
            // Background revalidation: nothing is waiting on this, so a store
            // failure is logged and the stale entry stays until the next attempt.
            if let Err(error) = state
                .isr_cache
                .put(
                    &cache_key,
                    html,
                    IsrPolicy::revalidate(isr_revalidate_duration()),
                )
                .await
            {
                tracing::warn!(
                    event = "isr_revalidation_write_failed",
                    cache_key = %cache_key,
                    http.route = %path,
                    %error,
                    "stale entry retained; will retry on the next request"
                );
            }
        } else {
            tracing::warn!(
                event = "isr_revalidation_snapshot_skipped_non_finalized",
                cache_key = %cache_key,
                path = %path,
                "skipping ISR revalidation write because snapshot is not finalized"
            );
        }
    }

    let mut in_progress = state.isr_revalidating.lock().await;
    in_progress.remove(&cache_key);
}

fn render_home_page() -> String {
    render_home_page_localized("en")
}

fn render_home_page_localized(locale: &str) -> String {
    let i18n = i18n_for(locale);
    let page_title = i18n.t("home_title");
    let hello = i18n.t("hello");
    let rendered = i18n.t("rendered");
    let hydration_mode = HydrationMode::from_env();
    let hydration_budget = hydration_budget_for_route("/", hydration_mode);
    let hydration_preloads = hydration_preload_links_html(&hydration_budget);
    let ttfb_budget_ms = u64_env("KRAB_HYDRATION_BUDGET_HOME_TTFB_MS", 800);
    let minimal_js_audit = bool_env("KRAB_MINIMAL_JS_AUDIT", true);

    let counter = Counter(CounterProps { initial: 10 });
    let toggle = Toggle(ToggleProps { initial: false });
    let likes = Likes(LikesProps { initial: 3 });
    let critical_islands_json =
        serde_json::to_string(&vec!["Counter"]).unwrap_or_else(|_| "[\"Counter\"]".to_string());
    let deferred_islands_json = serde_json::to_string(&vec!["Toggle", "Likes"])
        .unwrap_or_else(|_| "[\"Toggle\",\"Likes\"]".to_string());

    tracing::info!(
        event = "hydration_mode_selected",
        code = "KRAB-HYDRATE-010",
        route = "/",
        mode = hydration_mode.as_str(),
        minimal_js_audit = minimal_js_audit,
        startup_budget_ms = hydration_budget.max_startup_ms,
        ttfb_budget_ms = ttfb_budget_ms,
        "hydration mode selected for homepage"
    );

    if hydration_mode == HydrationMode::SsrOnly {
        tracing::warn!(
            event = "hydration_ssr_only_mode",
            code = "KRAB-HYDRATE-300",
            route = "/",
            "SSR-only hydration mode enabled"
        );
    }

    let runtime_script_base = r#"
                    function hydrationDiag(code, level, detail, extra = {}) {
                        const payload = {
                            code,
                            mode: KRAB_HYDRATION_MODE,
                            detail,
                            ...extra,
                        };
                        if (level === 'error') {
                            console.error('[krab-hydration]', payload);
                        } else if (level === 'warn') {
                            console.warn('[krab-hydration]', payload);
                        } else {
                            console.log('[krab-hydration]', payload);
                        }
                    }

                    async function loadHydratorModule() {
                        const mod = await import('/pkg/krab_client.js');
                        return {
                            init: mod.default,
                            hydrate: mod.hydrate,
                        };
                    }

                    const SERVER_FUNCTION_VERSION = '2026-02-27.1';

                    function asObject(value) {
                        return !!value && typeof value === 'object' && !Array.isArray(value);
                    }

                    function setText(id, text) {
                        const el = document.getElementById(id);
                        if (el) {
                            el.textContent = text;
                        }
                    }

                    function markDegraded(reason) {
                        const el = document.getElementById('frontend-degraded');
                        if (el) {
                            el.textContent = '⚠ partial functionality mode: ' + reason;
                        }
                    }

                    function classifyIslandsForDeferredHydration() {
                        const critical = new Set(KRAB_CRITICAL_ISLANDS);
                        const deferred = new Set(KRAB_DEFERRED_ISLANDS);
                        document.querySelectorAll('[data-island]').forEach((el) => {
                            const name = el.getAttribute('data-island') || '';
                            const priority = critical.has(name)
                                ? 'critical'
                                : (deferred.has(name) ? 'deferred' : 'critical');

                            el.setAttribute('data-krab-priority', priority);

                            if (priority === 'deferred') {
                                el.setAttribute('data-island-deferred', name);
                                el.removeAttribute('data-island');
                            }
                        });
                    }

                    function freezeCriticalIslandsAfterHydration() {
                        document.querySelectorAll('[data-island][data-krab-priority="critical"]').forEach((el) => {
                            const name = el.getAttribute('data-island');
                            if (name) {
                                el.setAttribute('data-island-hydrated', name);
                                el.removeAttribute('data-island');
                            }
                        });
                    }

                    function activateDeferredIslands() {
                        document.querySelectorAll('[data-island-deferred]').forEach((el) => {
                            const name = el.getAttribute('data-island-deferred');
                            if (name) {
                                el.setAttribute('data-island', name);
                                el.removeAttribute('data-island-deferred');
                            }
                        });
                    }

                    function scheduleDeferredHydration(hydrator) {
                        const runDeferred = () => {
                            try {
                                activateDeferredIslands();
                                hydrator.hydrate();
                                document.querySelectorAll('[data-island][data-krab-priority="deferred"]').forEach((el) => {
                                    const name = el.getAttribute('data-island');
                                    if (name) {
                                        el.setAttribute('data-island-hydrated', name);
                                        el.removeAttribute('data-island');
                                    }
                                });
                            } catch (err) {
                                hydrationDiag(
                                    'KRAB-HYDRATE-510',
                                    'warn',
                                    'deferred hydration failed; continuing in partial mode',
                                    { error: String(err) }
                                );
                                markDegraded('deferred island hydration failed');
                            }
                        };

                        if ('requestIdleCallback' in window) {
                            window.requestIdleCallback(runDeferred, { timeout: 1200 });
                        } else {
                            setTimeout(runDeferred, 250);
                        }
                    }

                    function wireMinimalJsCounter(root) {
                        const button = root.querySelector('button');
                        const valueNode = root.querySelector('span');
                        if (!button || !valueNode) {
                            return false;
                        }
                        button.addEventListener('click', () => {
                            const parsed = Number.parseInt((valueNode.textContent || '').trim(), 10);
                            const next = Number.isFinite(parsed) ? parsed + 1 : 1;
                            valueNode.textContent = String(next);
                        });
                        return true;
                    }

                    function wireMinimalJsToggle(root) {
                        const button = root.querySelector('button');
                        const valueNode = root.querySelector('span');
                        if (!button || !valueNode) {
                            return false;
                        }
                        button.addEventListener('click', () => {
                            const on = (valueNode.textContent || '').includes('ON');
                            valueNode.textContent = on ? ' OFF' : ' ON';
                        });
                        return true;
                    }

                    function wireMinimalJsLikes(root) {
                        const button = root.querySelector('button');
                        const valueNode = root.querySelector('span');
                        if (!button || !valueNode) {
                            return false;
                        }
                        button.addEventListener('click', () => {
                            const parsed = Number.parseInt((valueNode.textContent || '').trim(), 10);
                            const next = Number.isFinite(parsed) ? parsed + 1 : 1;
                            valueNode.textContent = String(next);
                        });
                        return true;
                    }

                    function enableMinimalJsFallback() {
                        let wired = 0;
                        document.querySelectorAll('[data-island]').forEach((root) => {
                            const name = root.getAttribute('data-island') || '';
                            let ok = false;
                            if (name === 'Counter') {
                                ok = wireMinimalJsCounter(root);
                            } else if (name === 'Toggle') {
                                ok = wireMinimalJsToggle(root);
                            } else if (name === 'Likes') {
                                ok = wireMinimalJsLikes(root);
                            }

                            if (ok) {
                                wired += 1;
                                root.setAttribute('data-krab-boundary-state', 'minimal_js');
                            }
                        });

                        hydrationDiag('KRAB-HYDRATE-200', 'warn', 'minimal-js escape hatch activated', {
                            wiredIslands: wired,
                            auditEnabled: KRAB_MINIMAL_JS_AUDIT,
                        });

                        if (!KRAB_MINIMAL_JS_AUDIT) {
                            hydrationDiag(
                                'KRAB-HYDRATE-220',
                                'warn',
                                'minimal-js audit flag disabled; this mode is not policy-compliant'
                            );
                        }
                    }

                    function validateStatus(payload) {
                        return asObject(payload)
                            && payload.service === 'frontend'
                            && (payload.status === 'ok' || payload.status === 'degraded');
                    }

                    function validateRpcNow(payload) {
                        return asObject(payload)
                            && Number.isFinite(payload.epoch_millis)
                            && typeof payload.server_function_version === 'string';
                    }

                    function validateRpcVersion(payload) {
                        return asObject(payload)
                            && typeof payload.server_function_version === 'string'
                            && typeof payload.policy === 'string';
                    }

                    function validateDashboard(payload) {
                        return asObject(payload)
                            && Number.isFinite(payload.users_online)
                            && Number.isFinite(payload.active_sessions)
                            && payload.feature === 'islands';
                    }

                    async function fetchJsonWithRetry(url, options = {}) {
                        const timeoutMs = options.timeoutMs ?? 1200;
                        const retries = options.retries ?? 2;
                        const baseBackoffMs = options.baseBackoffMs ?? 150;
                        const validator = options.validator;
                        let lastError = null;

                        for (let attempt = 0; attempt <= retries; attempt++) {
                            const controller = new AbortController();
                            const timeoutId = setTimeout(() => controller.abort(), timeoutMs);
                            try {
                                const response = await fetch(url, {
                                    signal: controller.signal,
                                    cache: 'no-store',
                                });
                                if (!response.ok) {
                                    throw new Error('HTTP ' + response.status);
                                }
                                const json = await response.json();
                                if (validator && !validator(json)) {
                                    throw new Error('schema mismatch');
                                }
                                clearTimeout(timeoutId);
                                return { ok: true, data: json };
                            } catch (err) {
                                clearTimeout(timeoutId);
                                lastError = err;
                                if (attempt < retries) {
                                    const backoff = baseBackoffMs * (attempt + 1);
                                    await new Promise(resolve => setTimeout(resolve, backoff));
                                }
                            }
                        }

                        return { ok: false, error: String(lastError) };
                    }

                    async function verifyManifestIntegrity() {
                        const manifest = await fetchJsonWithRetry('/asset-manifest.json', {
                            timeoutMs: 700,
                            retries: 0,
                            validator: payload => asObject(payload) && asObject(payload.assets),
                        });

                        if (!manifest.ok) {
                            console.warn('manifest check skipped:', manifest.error);
                            return;
                        }

                        const clientEntry = manifest.data.assets['krab_client.js'];
                        const valid = asObject(clientEntry)
                            && typeof clientEntry.path === 'string'
                            && typeof clientEntry.integrity === 'string'
                            && clientEntry.integrity.startsWith('sha256-')
                            && clientEntry.immutable === true;

                        if (!valid) {
                            markDegraded('asset manifest integrity validation failed');
                        }
                    }

                    function checkRouteBudgets(hydrationMs) {
                        const nav = performance.getEntriesByType('navigation')[0];
                        if (nav && nav.responseStart > ROUTE_BUDGETS.ttfbMs) {
                            hydrationDiag('KRAB-HYDRATE-410', 'warn', 'TTFB budget exceeded', {
                                ttfbMs: nav.responseStart,
                                budgetMs: ROUTE_BUDGETS.ttfbMs,
                            });
                            console.warn('TTFB budget exceeded', {
                                ttfbMs: nav.responseStart,
                                budgetMs: ROUTE_BUDGETS.ttfbMs,
                            });
                        }

                        if (hydrationMs > ROUTE_BUDGETS.hydrationMs) {
                            hydrationDiag('KRAB-HYDRATE-411', 'warn', 'Hydration budget exceeded', {
                                hydrationMs,
                                budgetMs: ROUTE_BUDGETS.hydrationMs,
                            });
                            console.warn('Hydration budget exceeded', {
                                hydrationMs,
                                budgetMs: ROUTE_BUDGETS.hydrationMs,
                            });
                        }
                    }

                    async function loadData() {
                        const [status, rpc, rpcVersion, dashboard] = await Promise.all([
                            fetchJsonWithRetry('/api/status', { validator: validateStatus }),
                            fetchJsonWithRetry('/rpc/now', { validator: validateRpcNow }),
                            fetchJsonWithRetry('/rpc/version', { validator: validateRpcVersion }),
                            fetchJsonWithRetry('/data/dashboard', { validator: validateDashboard }),
                        ]);

                        setText('status', status.ok ? JSON.stringify(status.data) : 'status unavailable');
                        setText('rpc', rpc.ok ? JSON.stringify(rpc.data) : 'rpc unavailable');
                        setText('version', rpcVersion.ok ? JSON.stringify(rpcVersion.data) : 'version unavailable');
                        setText('dashboard', dashboard.ok ? JSON.stringify(dashboard.data) : 'dashboard unavailable');

                        if (!status.ok || !rpc.ok || !rpcVersion.ok || !dashboard.ok) {
                            markDegraded('one or more upstream APIs are unavailable');
                        }
                    }

                    async function run() {
                        const hydrationStart = performance.now();
                        if (KRAB_HYDRATION_MODE === 'ssr_only') {
                            markDegraded('SSR-only mode active');
                            hydrationDiag('KRAB-HYDRATE-300', 'warn', 'SSR-only mode skips client hydration');
                        } else if (KRAB_HYDRATION_MODE === 'minimal_js') {
                            enableMinimalJsFallback();
                        } else {
                            try {
                                classifyIslandsForDeferredHydration();
                                const hydrator = await loadHydratorModule();
                                await hydrator.init();
                                hydrator.hydrate();
                                freezeCriticalIslandsAfterHydration();
                                scheduleDeferredHydration(hydrator);
                            } catch (err) {
                                markDegraded('hydration mismatch recovered via SSR fallback');
                                hydrationDiag('KRAB-HYDRATE-500', 'error', 'WASM hydration bootstrap failed', {
                                    error: String(err),
                                });
                                console.error('hydration failed:', err);
                            }
                        }
                        const hydrationMs = performance.now() - hydrationStart;

                        checkRouteBudgets(hydrationMs);
                        await loadData();
                        await verifyManifestIntegrity();

                        if (SERVER_FUNCTION_VERSION !== '2026-02-27.1') {
                            markDegraded('server function version mismatch');
                        }

                        // HMR
                        if (location.hostname === 'localhost' || location.hostname === '127.0.0.1') {
                            const evtSource = new EventSource('/api/hmr');
                            evtSource.onmessage = (e) => {
                                console.log('HMR signal received:', e.data);
                                // Full page reload is the current HMR strategy. Module-level hot
                                // swapping would require DOM diffing that the runtime does not yet do.
                                window.location.reload();
                            };
                        }
                    }

                    run();"#;
    let runtime_script = format!(
        "const KRAB_HYDRATION_MODE = \"{}\";\nconst KRAB_MINIMAL_JS_AUDIT = {};\nconst KRAB_CRITICAL_ISLANDS = {};\nconst KRAB_DEFERRED_ISLANDS = {};\nconst ROUTE_BUDGETS = {{ ttfbMs: {}, hydrationMs: {} }};\n{}",
        hydration_mode.as_str(),
        if minimal_js_audit { "true" } else { "false" },
        critical_islands_json,
        deferred_islands_json,
        ttfb_budget_ms,
        hydration_budget.max_startup_ms,
        runtime_script_base
    );

    let base_url = normalize_public_base_url();
    let canonical = canonical_url(&base_url, "/");
    let structured_data = serde_json::to_string(&json!({
        "@context": "https://schema.org",
        "@type": "WebPage",
        "name": "Krab Framework",
        "url": canonical,
        "description": "Krab full-stack Rust framework home page"
    }))
    .unwrap_or_else(|_| "{}".to_string());

    let content = view! {
        <html>
            <head>
                <title>{page_title}</title>
                <meta charset="utf-8" />
                <meta name="viewport" content="width=device-width, initial-scale=1" />
                <meta name="description" content="Krab full-stack Rust framework home page" />
                <meta name="robots" content="index,follow" />
                <link rel="canonical" href={canonical.clone()} />
                <meta property="og:title" content="Krab Framework" />
                <meta property="og:description" content="Krab full-stack Rust framework home page" />
                <meta property="og:type" content="website" />
                <meta property="og:url" content={canonical.clone()} />
                <meta property="og:site_name" content="Krab" />
                <meta name="twitter:card" content="summary_large_image" />
                <meta name="twitter:title" content="Krab Framework" />
                <meta name="twitter:description" content="Krab full-stack Rust framework home page" />
                <style>
                    r#"
                    :root {
                        --bg-color: #0f172a;
                        --text-color: #e2e8f0;
                        --primary: #38bdf8;
                        --secondary: #94a3b8;
                        --card-bg: #1e293b;
                        --border: #334155;
                    }
                    body {
                        font-family: system-ui, -apple-system, sans-serif;
                        line-height: 1.5;
                        color: var(--text-color);
                        background: var(--bg-color);
                        margin: 0;
                        padding: 0;
                    }
                    .container {
                        max_width: 800px;
                        margin: 0 auto;
                        padding: 2rem;
                    }
                    header {
                        text-align: center;
                        padding: 4rem 0;
                    }
                    h1 {
                        font-size: 3.5rem;
                        font-weight: 800;
                        margin: 0 0 1rem;
                        background: linear-gradient(to right, var(--primary), #a855f7);
                        -webkit-background-clip: text;
                        -webkit-text-fill-color: transparent;
                    }
                    .tagline {
                        font-size: 1.25rem;
                        color: var(--secondary);
                        max_width: 600px;
                        margin: 0 auto 2rem;
                    }
                    .links {
                        display: flex;
                        gap: 1rem;
                        justify-content: center;
                        margin-bottom: 4rem;
                    }
                    .btn {
                        display: inline-block;
                        padding: 0.75rem 1.5rem;
                        border-radius: 9999px;
                        font-weight: 600;
                        text-decoration: none;
                        transition: transform 0.2s;
                    }
                    .btn-primary {
                        background: var(--primary);
                        color: #0f172a;
                    }
                    .btn-secondary {
                        background: var(--card-bg);
                        color: var(--text-color);
                        border: 1px solid var(--border);
                    }
                    .btn:hover {
                        transform: translateY(-2px);
                    }
                    .grid {
                        display: grid;
                        grid-template-columns: repeat(auto-fit, minmax(250px, 1fr));
                        gap: 1.5rem;
                        margin-bottom: 4rem;
                    }
                    .card {
                        background: var(--card-bg);
                        border: 1px solid var(--border);
                        border-radius: 0.75rem;
                        padding: 1.5rem;
                    }
                    .card h3 {
                        margin-top: 0;
                        color: var(--primary);
                    }
                    .interactive-demo {
                        background: var(--card-bg);
                        border: 1px solid var(--border);
                        border-radius: 1rem;
                        padding: 2rem;
                        margin-top: 2rem;
                    }
                    .demo-row {
                        display: flex;
                        align-items: center;
                        gap: 1rem;
                        margin-bottom: 1rem;
                        padding-bottom: 1rem;
                        border-bottom: 1px solid var(--border);
                    }
                    .demo-row:last-child {
                        border-bottom: none;
                        margin-bottom: 0;
                        padding-bottom: 0;
                    }
                    .status-grid {
                        display: grid;
                        grid-template-columns: repeat(2, 1fr);
                        gap: 1rem;
                        font-family: monospace;
                        font-size: 0.875rem;
                    }
                    .status-item {
                        background: #0002;
                        padding: 0.5rem;
                        border-radius: 0.25rem;
                    }
                    .status-label {
                        color: var(--secondary);
                        display: block;
                        font-size: 0.75rem;
                        margin-bottom: 0.25rem;
                    }
                    "#
                </style>
                <script r#type="application/ld+json">{structured_data}</script>
                <script r#type="module">
                    {runtime_script}
                </script>
            </head>
            <body>
                <div class="container">
                    <header>
                        <h1>{hello}</h1>
                        <p class="tagline">
                            "The full-stack Rust framework designed for performance, type safety, and developer joy."
                        </p>
                        <div class="links">
                            <a href="https://github.com/ManirajKatuwal/krab-pub" class="btn btn-primary">"Get Started"</a>
                            <a href="/docs" class="btn btn-secondary">"Documentation"</a>
                            <a href="https://github.com/ManirajKatuwal/krab-pub" class="btn btn-secondary">"GitHub"</a>
                        </div>
                    </header>

                    <div class="grid">
                        <div class="card">
                            <h3>"Server-Side Rendering"</h3>
                            <p>"Blazing fast initial loads with Rust-powered HTML generation. SEO-friendly by default."</p>
                        </div>
                        <div class="card">
                            <h3>"Islands Architecture"</h3>
                            <p>"Ship zero JS by default. Hydrate only the interactive bits for optimal performance."</p>
                        </div>
                        <div class="card">
                            <h3>"Type-Safe Everywhere"</h3>
                            <p>"Share types between backend and frontend. Catch errors at compile time, not runtime."</p>
                        </div>
                    </div>

                    <div class="interactive-demo">
                        <h2>"Interactive Islands"</h2>
                        <p class="mb-4 text-secondary">"These components are hydrated on the client. Try them out!"</p>

                        <div class="demo-row">
                            <span>"Counter:"</span>
                            {counter}
                        </div>
                        <div class="demo-row">
                            <span>"Toggle:"</span>
                            <div>{toggle}</div>
                        </div>
                        <div class="demo-row">
                            <span>"Likes:"</span>
                            <div>{likes}</div>
                        </div>
                    </div>

                    <div class="interactive-demo" style="margin-top: 2rem;">
                        <h2>"Real-Time Data"</h2>
                        <p id="frontend-degraded" style="color: #f87171;"></p>
                        <div class="status-grid">
                            <div class="status-item">
                                <span class="status-label">"SYSTEM STATUS"</span>
                                <span id="status">"connecting..."</span>
                            </div>
                            <div class="status-item">
                                <span class="status-label">"RPC CONNECTION"</span>
                                <span id="rpc">"connecting..."</span>
                            </div>
                            <div class="status-item">
                                <span class="status-label">"SERVER VERSION"</span>
                                <span id="version">"checking..."</span>
                            </div>
                            <div class="status-item">
                                <span class="status-label">"DASHBOARD METRICS"</span>
                                <span id="dashboard">"loading..."</span>
                            </div>
                        </div>
                        <p style="margin-top: 1rem; font-size: 0.875rem; color: var(--secondary);">
                            {rendered}
                        </p>
                    </div>

                    <footer style="text-align: center; margin-top: 4rem; color: var(--secondary); font-size: 0.875rem;">
                        <p>"Built with Rust & Krab Framework"</p>
                    </footer>
                </div>
            </body>
        </html>
    };

    let guarded = ErrorBoundary::new(
        "home-page",
        content,
        view! { <html><body><h1>"Krab fallback"</h1></body></html> },
    );
    let mut writer =
        ChunkedStreamWriter::new(1024, 2048).with_max_total_bytes(stream_budget_bytes());
    let _ = writer.write("<!DOCTYPE html>");
    let _ = writer.write_suspense_marker("home", SuspenseState::Pending);
    let _ = writer.write("<div data-krab-hydration=\"home\">");
    let mut rendered_html = guarded.render();
    if !hydration_preloads.is_empty() {
        rendered_html = rendered_html.replacen(
            "</head>",
            &format!("{}{}", hydration_preloads, "</head>"),
            1,
        );
    }
    let _ = writer.write(&rendered_html);
    let _ = writer.write("</div>");
    let _ = writer.write_suspense_marker("home", SuspenseState::Resolved);
    writer.flush();
    let stream_telemetry = writer.telemetry_snapshot();
    tracing::debug!(
        event = "ssr_stream_telemetry",
        route = "/",
        ttfb_ms = ?stream_telemetry.ttfb_ms,
        first_visible_chunk_ms = ?stream_telemetry.first_visible_chunk_ms,
        full_stream_complete_ms = ?stream_telemetry.full_stream_complete_ms,
        emitted_bytes = stream_telemetry.emitted_bytes,
        flush_count = stream_telemetry.flush_count,
        suspense_markers = stream_telemetry.suspense_marker_count,
        budget_limit_bytes = ?stream_telemetry.budget_limit_bytes,
        budget_exceeded = stream_telemetry.budget_exceeded,
        stream_cancelled = stream_telemetry.stream_cancelled,
        cancel_reason = ?stream_telemetry.cancel_reason,
    );
    let finished = writer.finish();
    if !finished.is_complete() {
        tracing::warn!(
            route = "/",
            budget_exceeded = finished.budget_exceeded,
            cancelled = finished.cancelled,
            "ssr_stream_truncated"
        );
    }
    finished.concat()
}

async fn home_handler(headers: HeaderMap) -> Html<String> {
    let locale = resolve_locale(&headers);
    let html = tokio::task::spawn_blocking(move || render_home_page_localized(&locale))
        .await
        .unwrap();
    Html(html)
}

async fn localized_home_handler(Path(params): Path<HashMap<String, String>>) -> Html<String> {
    let locale = params
        .get("locale")
        .map(|s| s.to_string())
        .unwrap_or_else(|| "en".to_string());
    let html = tokio::task::spawn_blocking(move || render_home_page_localized(&locale))
        .await
        .unwrap();
    Html(html)
}

async fn robots_txt_handler() -> ([(&'static str, &'static str); 1], String) {
    let base_url = normalize_public_base_url();
    (
        [("content-type", "text/plain; charset=utf-8")],
        format!(
            "User-agent: *\nAllow: /\nSitemap: {}/sitemap.xml\n",
            base_url
        ),
    )
}

async fn sitemap_xml_handler() -> ([(&'static str, &'static str); 1], String) {
    let base_url = normalize_public_base_url();
    let routes = ["/", "/about", "/greet"];
    let urls = routes
        .iter()
        .map(|route| format!("<url><loc>{}{}</loc></url>", base_url, route))
        .collect::<Vec<String>>()
        .join("");

    (
        [("content-type", "application/xml; charset=utf-8")],
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?><urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">{}</urlset>",
            urls
        ),
    )
}

async fn api_status_handler(State(state): State<AppState>) -> Json<serde_json::Value> {
    let auth_ok = state
        .http_client
        .get(format!("{}/api/v1/auth/status", state.auth_base_url))
        .send()
        .await
        .map(|resp| resp.status().is_success())
        .unwrap_or(false);

    let users_ok = state
        .http_client
        .get(format!("{}/ready", state.users_base_url))
        .send()
        .await
        .map(|resp| resp.status().is_success())
        .unwrap_or(false);

    Json(json!({
        "service": "frontend",
        "status": if auth_ok && users_ok { "ok" } else { "degraded" },
        "dependencies": {
            "auth": auth_ok,
            "users": users_ok
        }
    }))
}

async fn health_handler() -> Json<serde_json::Value> {
    Json(json!({
        "service": "frontend",
        "status": "ok"
    }))
}

async fn ready_handler(State(state): State<AppState>) -> Json<serde_json::Value> {
    let uptime = state.runtime_state().started_at.elapsed().as_secs();
    Json(json!({
        "status": "ready",
        "uptime_seconds": uptime,
        "dependencies": []
    }))
}

async fn dashboard_handler(State(state): State<AppState>) -> Json<serde_json::Value> {
    let users_ready = state
        .http_client
        .get(format!("{}/ready", state.users_base_url))
        .send()
        .await
        .map(|resp| resp.status().is_success())
        .unwrap_or(false);

    let mut auth_key_count = 0_u64;
    if let Ok(payload) = state
        .protocol_client
        .call_with_fallback("auth", "auth.status", None, None)
        .await
    {
        auth_key_count = payload
            .get("key_count")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
    }

    Json(json!({
        "users_online": if users_ready { 1 } else { 0 },
        "active_sessions": auth_key_count,
        "feature": "islands",
        "sources": {
            "users_ready": users_ready,
            "auth_key_count": auth_key_count
        }
    }))
}

fn asset_manifest_json() -> String {
    format!(
        "{{\"assets\":{{\"krab_client.js\":{{\"path\":\"/pkg/krab_client.js?h=6f2c1a\",\"integrity\":\"sha256-demo-manifest-checksum\",\"immutable\":true}}}},\"server_function_version\":\"{}\"}}",
        SERVER_FUNCTION_VERSION
    )
}

fn rpc_version_json() -> String {
    format!(
        "{{\"server_function_version\":\"{}\",\"policy\":\"date-revision (YYYY-MM-DD.N), additive-first changes\"}}",
        SERVER_FUNCTION_VERSION
    )
}

fn rpc_now_json() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!(
        "{{\"epoch_millis\":{},\"server_function_version\":\"{}\"}}",
        now, SERVER_FUNCTION_VERSION
    )
}

#[derive(Debug, Deserialize)]
struct ContactSubmission {
    name: String,
    email: String,
    message: String,
}

fn redact_name_for_log(name: &str) -> String {
    format!("len:{}", name.chars().count())
}

fn redact_email_for_log(email: &str) -> String {
    let trimmed = email.trim();
    let Some((local, domain)) = trimmed.split_once('@') else {
        return "invalid-email".to_string();
    };

    let mut hasher = DefaultHasher::new();
    local.hash(&mut hasher);
    let local_hash = hasher.finish();
    format!("hash:{local_hash:016x}@{}", domain.to_ascii_lowercase())
}

async fn submit_contact_handler(
    Json(payload): Json<ContactSubmission>,
) -> (axum::http::StatusCode, Json<serde_json::Value>) {
    let name = payload.name.trim();
    let email = payload.email.trim();
    let message = payload.message.trim();

    if name.is_empty() || email.is_empty() || message.is_empty() || !email.contains('@') {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(json!({
                "status": "invalid",
                "message": "name, email, and message are required"
            })),
        );
    }

    tracing::info!(
        event = "contact_submission_received",
        name_redacted = %redact_name_for_log(name),
        email_redacted = %redact_email_for_log(email),
        message_len = message.len()
    );

    (
        axum::http::StatusCode::ACCEPTED,
        Json(json!({
            "status": "accepted",
            "queued": true,
            "contact": {
                "name": name,
                "email": email,
            }
        })),
    )
}

async fn hmr_handler(
    State(state): State<AppState>,
) -> axum::response::Sse<
    impl futures_util::Stream<Item = Result<axum::response::sse::Event, std::convert::Infallible>>,
> {
    use futures_util::StreamExt;
    use tokio_stream::wrappers::WatchStream;

    let stream = WatchStream::new(state.hmr_rx)
        .map(|sig| Ok(axum::response::sse::Event::default().data(sig.to_string())));

    axum::response::Sse::new(stream)
}

fn build_router(state: AppState) -> Router {
    let app: Router<AppState> = register_frontend_routes(Router::new());

    // Merge file-system routes generated by build.rs
    // Note: If generated routes have conflicts, Axum will panic at startup.
    let app = service_frontend::register_routes(app, state.clone());

    // Apply cache middleware to the router before wrapping with common layers.
    // With Axum's layer composition, this yields: Request -> Common -> Cache -> Handler.

    let app = app.layer(axum::middleware::from_fn_with_state(
        state.clone(),
        cache_middleware,
    ));

    apply_common_http_layers(app, state.clone()).with_state(state)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    init_tracing("service_frontend");
    let cfg = KrabConfig::from_env_checked("frontend", 3000)?;
    let secrets_report = cfg.validate_all()?;
    if !secrets_report.is_clean() {
        warn!(
            issue_count = secrets_report.issues.len(),
            "startup_secrets_policy_warnings_detected"
        );
    }
    let service_config = ServiceConfig {
        name: cfg.service_name.clone(),
        host: cfg.host.clone(),
        port: cfg.port,
        protocol: None,
    };
    let (hmr_tx, hmr_rx) = tokio::sync::watch::channel(0);

    tokio::spawn(async move {
        let mut last_sig = 0;
        let p = std::path::PathBuf::from("dist/.hmr_signal");
        loop {
            tokio::time::sleep(Duration::from_millis(100)).await;
            if let Ok(content) = std::fs::read_to_string(&p) {
                if let Ok(sig) = content.trim().parse::<u64>() {
                    if sig != last_sig {
                        last_sig = sig;
                        let _ = hmr_tx.send(sig);
                    }
                }
            }
        }
    });

    let topology_runtime = TopologyRuntime::from_env_checked()?;
    let auth_base_url = resolve_service_base_url(
        &topology_runtime,
        "auth",
        "KRAB_AUTH_BASE_URL",
        "http://127.0.0.1:3001",
    );
    let users_base_url = resolve_service_base_url(
        &topology_runtime,
        "users",
        "KRAB_USERS_BASE_URL",
        "http://127.0.0.1:3002",
    );

    tracing::info!(
        event = "topology_runtime_resolved",
        mode = ?topology_runtime.mode,
        auth_base_url = %auth_base_url,
        users_base_url = %users_base_url,
        endpoint_count = topology_runtime.endpoints.len(),
    );

    let users_contract_bundle = users_contract::build_users_adapter(
        &topology_runtime,
        users_base_url.clone(),
        env_trimmed("KRAB_FRONTEND_DOWNSTREAM_BEARER_TOKEN"),
    );
    tracing::info!(
        event = "users_contract_adapter_selected",
        adapter = users_contract_bundle.kind.as_str(),
    );

    // ISR shares the runtime's store rather than keeping its own map, so with
    // `KRAB_REDIS_URL` set every replica reads and invalidates the same
    // entries. Without it this is a `MemoryStore` and behaves as before —
    // correct for one process, not for several.
    let runtime = RuntimeState::try_new()?;
    let isr_cache = IsrCache::with_store(runtime.store.clone());

    let state = AppState {
        runtime,
        http_client: Client::builder().timeout(Duration::from_secs(2)).build()?,
        auth_base_url: auth_base_url.clone(),
        users_base_url: users_base_url.clone(),
        protocol_client: {
            let service_urls = HashMap::from([
                ("auth".to_string(), auth_base_url),
                ("users".to_string(), users_base_url),
            ]);
            Arc::new(ProtocolAwareClient::from_env(
                Client::builder().timeout(Duration::from_secs(2)).build()?,
                service_urls,
                Duration::from_secs(60),
            )?)
        },
        isr_cache,
        isr_revalidating: Arc::new(tokio::sync::Mutex::new(HashSet::new())),
        hmr_rx,
    };
    let app = build_router(state);

    serve_with_graceful_shutdown(app, &service_config).await?;
    Ok(())
}

#[cfg(test)]
#[serial_test::serial]
#[allow(dead_code, unused_imports)]
mod tests {
    use super::{
        api_status_handler, asset_manifest_json, dashboard_handler, health_handler,
        is_finalized_ssr_snapshot, normalize_public_base_url, normalize_service_base_url,
        ready_handler, redact_email_for_log, redact_name_for_log, render_about_page,
        render_blog_page, render_home_page, resolve_service_base_url, robots_txt_handler,
        rpc_now_json, rpc_version_json, sitemap_xml_handler, stream_budget_bytes, AppState,
        RuntimeState, SERVER_FUNCTION_VERSION,
    };
    use crate::app_state::CachedHttpPayload;
    use crate::cache::{cache_authority, CacheAuthority};
    use crate::protocol_client::ProtocolAwareClient;
    use axum::extract::State;
    use axum::http::Method;
    use axum::Json;
    use krab_core::http::HasRuntimeState;
    use krab_core::isr::IsrCache;
    use krab_core::service_contract::{ServiceEndpoint, ServiceTopology, TopologyRuntime};
    use reqwest::Client;
    use std::collections::{HashMap, HashSet};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    const P95_SSR_RENDER_MS_THRESHOLD: u128 = 6;
    const P99_SSR_RENDER_MS_THRESHOLD: u128 = 12;

    fn percentile_millis(samples_ms: &mut [u128], percentile: f64) -> u128 {
        assert!(!samples_ms.is_empty(), "samples must not be empty");
        let bounded = percentile.clamp(0.0, 1.0);
        samples_ms.sort_unstable();
        let index = ((samples_ms.len() - 1) as f64 * bounded).round() as usize;
        samples_ms[index]
    }

    fn test_protocol_client() -> Arc<ProtocolAwareClient> {
        Arc::new(ProtocolAwareClient::new(
            Client::new(),
            HashMap::from([
                ("auth".to_string(), "http://127.0.0.1:1".to_string()),
                ("users".to_string(), "http://127.0.0.1:1".to_string()),
            ]),
            Duration::from_secs(60),
        ))
    }

    fn test_state_with_protocol_client(timeout: Duration) -> AppState {
        AppState {
            runtime: RuntimeState::new(),
            http_client: Client::builder().timeout(timeout).build().unwrap(),
            auth_base_url: "http://127.0.0.1:1".to_string(),
            users_base_url: "http://127.0.0.1:1".to_string(),
            protocol_client: test_protocol_client(),
            isr_cache: IsrCache::new(),
            isr_revalidating: Arc::new(tokio::sync::Mutex::new(HashSet::new())),
            hmr_rx: tokio::sync::watch::channel(0).1,
        }
    }

    #[test]
    fn ssr_home_includes_hydration_and_data_loading_contracts() {
        let html = render_home_page();
        assert!(html.contains("loadHydratorModule"));
        assert!(html.contains("import('/pkg/krab_client.js')"));
        assert!(html.contains("/api/status"));
        assert!(html.contains("/rpc/now"));
        assert!(html.contains("/rpc/version"));
        assert!(html.contains("/data/dashboard"));
        assert!(html.contains("/asset-manifest.json"));
        assert!(html.contains("id=\"status\""));
        assert!(html.contains("id=\"rpc\""));
        assert!(html.contains("id=\"version\""));
        assert!(html.contains("id=\"dashboard\""));
        assert!(html.contains("id=\"frontend-degraded\""));
        assert!(html.contains("fetchJsonWithRetry"));
        assert!(html.contains("ROUTE_BUDGETS"));
        assert!(html.contains("schema mismatch"));
        // In non-web SSR mode, island wrappers expose `data-island` + `data-props`.
        // In `--all-features` builds, `krab_client` may compile with `feature = "web"`
        // and render direct component markup without wrapper attributes.
        // Assert stable interactive component payload in either mode.
        assert!(html.contains("Count:"));
        assert!(html.contains("Toggle"));
        assert!(html.contains("Like"));
    }

    #[test]
    fn ssr_home_includes_robust_seo_metadata() {
        std::env::set_var("KRAB_PUBLIC_BASE_URL", "https://krab.example.com");
        let html = render_home_page();
        assert!(html.contains("<meta name=\"description\""));
        assert!(html.contains("<meta name=\"robots\" content=\"index,follow\""));
        assert!(html.contains("<link rel=\"canonical\" href=\"https://krab.example.com/\""));
        assert!(html.contains("<meta property=\"og:title\""));
        assert!(html.contains("<meta name=\"twitter:card\""));
        assert!(html.contains("application/ld+json"));
    }

    #[test]
    fn blog_page_uses_route_specific_canonical_and_article_type() {
        std::env::set_var("KRAB_PUBLIC_BASE_URL", "https://krab.example.com");
        let html = render_blog_page("integration-check");
        assert!(html.contains(
            "<link rel=\"canonical\" href=\"https://krab.example.com/blog/integration-check\""
        ));
        assert!(html.contains("<meta property=\"og:type\" content=\"article\""));
        assert!(html.contains("Blog Post: integration-check | Krab Framework"));
    }

    #[test]
    fn about_page_uses_route_specific_canonical() {
        std::env::set_var("KRAB_PUBLIC_BASE_URL", "https://krab.example.com");
        let html = render_about_page();
        assert!(html.contains("<link rel=\"canonical\" href=\"https://krab.example.com/about\""));
    }

    #[tokio::test]
    async fn robots_and_sitemap_routes_publish_crawler_contracts() {
        std::env::set_var("KRAB_PUBLIC_BASE_URL", "https://krab.example.com");

        let (_robots_headers, robots_body) = robots_txt_handler().await;
        assert!(robots_body.contains("User-agent: *"));
        assert!(robots_body.contains("Sitemap: https://krab.example.com/sitemap.xml"));

        let (_sitemap_headers, sitemap_body) = sitemap_xml_handler().await;
        assert!(sitemap_body.contains("<urlset"));
        assert!(sitemap_body.contains("<loc>https://krab.example.com/</loc>"));
        assert!(sitemap_body.contains("<loc>https://krab.example.com/about</loc>"));
        assert!(sitemap_body.contains("<loc>https://krab.example.com/greet</loc>"));
    }

    #[test]
    fn seo_public_base_url_defaults_for_local_development() {
        std::env::remove_var("KRAB_PUBLIC_BASE_URL");
        assert_eq!(normalize_public_base_url(), "http://localhost:3000");
    }

    #[tokio::test]
    async fn api_contract_status_payload_is_stable_json() {
        let state = test_state_with_protocol_client(std::time::Duration::from_millis(10));
        let Json(json) = api_status_handler(State(state)).await;
        assert_eq!(
            json.get("service").and_then(|v| v.as_str()),
            Some("frontend")
        );
        assert!(matches!(
            json.get("status").and_then(|v| v.as_str()),
            Some("ok") | Some("degraded")
        ));
    }

    #[tokio::test]
    async fn operational_health_payload_is_stable_json() {
        let Json(json) = health_handler().await;
        assert_eq!(
            json.get("service").and_then(|v| v.as_str()),
            Some("frontend")
        );
        assert_eq!(json.get("status").and_then(|v| v.as_str()), Some("ok"));
    }

    #[tokio::test]
    async fn operational_ready_payload_matches_contract_shape() {
        let state = test_state_with_protocol_client(std::time::Duration::from_secs(1));
        let Json(json) = ready_handler(State(state)).await;
        assert_eq!(json.get("status").and_then(|v| v.as_str()), Some("ready"));
        assert!(json
            .get("uptime_seconds")
            .and_then(|v| v.as_u64())
            .is_some());
        assert!(json
            .get("dependencies")
            .and_then(|v| v.as_array())
            .is_some());
    }

    #[tokio::test]
    async fn test_ops_probes_are_always_rest() {
        use axum::routing::get;
        use axum::Router;

        async fn auth_status() -> Json<serde_json::Value> {
            Json(serde_json::json!({"status": "ok", "key_count": 2}))
        }

        async fn users_ready() -> Json<serde_json::Value> {
            Json(serde_json::json!({"status": "ready"}))
        }

        let app = Router::new()
            .route("/api/v1/auth/status", get(auth_status))
            .route("/ready", get(users_ready));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let state = AppState {
            runtime: RuntimeState::new(),
            http_client: Client::builder()
                .timeout(std::time::Duration::from_secs(1))
                .build()
                .unwrap(),
            auth_base_url: format!("http://{}", addr),
            users_base_url: format!("http://{}", addr),
            protocol_client: Arc::new(ProtocolAwareClient::new(
                Client::new(),
                HashMap::from([
                    ("auth".to_string(), "http://127.0.0.1:1".to_string()),
                    ("users".to_string(), "http://127.0.0.1:1".to_string()),
                ]),
                Duration::from_secs(60),
            )),
            isr_cache: IsrCache::new(),
            isr_revalidating: Arc::new(tokio::sync::Mutex::new(HashSet::new())),
            hmr_rx: tokio::sync::watch::channel(0).1,
        };

        let Json(api_status) = api_status_handler(State(state.clone())).await;
        assert_eq!(
            api_status.get("status").and_then(|v| v.as_str()),
            Some("ok")
        );

        let Json(ready_status) = ready_handler(State(state)).await;
        assert_eq!(
            ready_status.get("status").and_then(|v| v.as_str()),
            Some("ready")
        );

        handle.abort();
    }

    #[tokio::test]
    async fn api_contract_dashboard_payload_contains_expected_fields() {
        let state = test_state_with_protocol_client(std::time::Duration::from_millis(10));
        let Json(json) = dashboard_handler(State(state)).await;
        assert!(json.get("users_online").and_then(|v| v.as_u64()).is_some());
        assert!(json
            .get("active_sessions")
            .and_then(|v| v.as_u64())
            .is_some());
        assert_eq!(
            json.get("feature").and_then(|v| v.as_str()),
            Some("islands")
        );
    }

    #[test]
    fn service_base_url_normalization_uses_default_when_env_missing() {
        std::env::remove_var("KRAB_USERS_BASE_URL");
        let base = normalize_service_base_url("KRAB_USERS_BASE_URL", "http://127.0.0.1:3002");
        assert_eq!(base, "http://127.0.0.1:3002");
    }

    #[test]
    fn stream_budget_defaults_to_two_mb_and_honors_floor() {
        std::env::remove_var("KRAB_SSR_STREAM_BUDGET_BYTES");
        assert_eq!(stream_budget_bytes(), 2 * 1024 * 1024);

        std::env::set_var("KRAB_SSR_STREAM_BUDGET_BYTES", "256");
        assert_eq!(stream_budget_bytes(), 1024);

        std::env::set_var("KRAB_SSR_STREAM_BUDGET_BYTES", "4096");
        assert_eq!(stream_budget_bytes(), 4096);

        std::env::remove_var("KRAB_SSR_STREAM_BUDGET_BYTES");
    }

    #[test]
    fn isr_snapshot_finalization_accepts_balanced_markers() {
        let html = [
            "<html>",
            "<!--krab:suspense:home:pending-->",
            "<div>home shell</div>",
            "<!--krab:suspense:home:resolved-->",
            "</html>",
        ]
        .join("");
        assert!(is_finalized_ssr_snapshot(&html));
    }

    #[test]
    fn isr_snapshot_finalization_rejects_unresolved_markers() {
        let html = [
            "<html>",
            "<!--krab:suspense:home:pending-->",
            "<div>still pending...</div>",
            "</html>",
        ]
        .join("");
        assert!(!is_finalized_ssr_snapshot(&html));
    }

    #[test]
    fn topology_resolver_prefers_distributed_endpoint_over_env_default() {
        std::env::set_var("KRAB_USERS_BASE_URL", "http://127.0.0.1:3999");

        let topology = TopologyRuntime {
            mode: ServiceTopology::Distributed,
            endpoints: HashMap::from([(
                "users".to_string(),
                ServiceEndpoint {
                    base_url: "http://127.0.0.1:3002".to_string(),
                    timeout_ms: 1200,
                    max_retries: 1,
                },
            )]),
        };

        let resolved = resolve_service_base_url(
            &topology,
            "users",
            "KRAB_USERS_BASE_URL",
            "http://127.0.0.1:3000",
        );
        assert_eq!(resolved, "http://127.0.0.1:3002");

        std::env::remove_var("KRAB_USERS_BASE_URL");
    }

    #[test]
    fn topology_resolver_uses_env_in_monolith_mode() {
        std::env::set_var("KRAB_AUTH_BASE_URL", "http://127.0.0.1:4101");

        let topology = TopologyRuntime {
            mode: ServiceTopology::Monolith,
            endpoints: HashMap::from([(
                "auth".to_string(),
                ServiceEndpoint {
                    base_url: "http://127.0.0.1:3001".to_string(),
                    timeout_ms: 1200,
                    max_retries: 1,
                },
            )]),
        };

        let resolved = resolve_service_base_url(
            &topology,
            "auth",
            "KRAB_AUTH_BASE_URL",
            "http://127.0.0.1:3001",
        );
        assert_eq!(resolved, "http://127.0.0.1:4101");

        std::env::remove_var("KRAB_AUTH_BASE_URL");
    }

    #[test]
    fn browser_journey_contract_scripts_reference_api_matrix() {
        let html = render_home_page();
        assert!(html.contains("fetchJsonWithRetry('/api/status'"));
        assert!(html.contains("fetchJsonWithRetry('/rpc/now'"));
        assert!(html.contains("fetchJsonWithRetry('/rpc/version'"));
        assert!(html.contains("fetchJsonWithRetry('/data/dashboard'"));
        assert!(html.contains("Promise.all"));
    }

    #[test]
    fn rpc_now_contract_returns_epoch_millis_number_and_version() {
        let raw = rpc_now_json();
        let json: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert!(json.get("epoch_millis").and_then(|v| v.as_u64()).is_some());
        assert_eq!(
            json.get("server_function_version").and_then(|v| v.as_str()),
            Some(SERVER_FUNCTION_VERSION)
        );
    }

    #[test]
    fn rpc_version_contract_exposes_policy_and_version() {
        let raw = rpc_version_json();
        let json: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(
            json.get("server_function_version").and_then(|v| v.as_str()),
            Some(SERVER_FUNCTION_VERSION)
        );
        assert!(json.get("policy").and_then(|v| v.as_str()).is_some());
    }

    #[test]
    fn asset_manifest_contract_enforces_integrity_shape() {
        let raw = asset_manifest_json();
        let json: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let entry = &json["assets"]["krab_client.js"];
        assert!(entry["path"].as_str().is_some());
        assert!(entry["integrity"]
            .as_str()
            .map(|v| v.starts_with("sha256-"))
            .unwrap_or(false));
        assert_eq!(entry["immutable"].as_bool(), Some(true));
    }

    #[test]
    fn e2e_ssr_to_hydration_journey_contract() {
        let html = render_home_page();
        assert!(html.contains("import('/pkg/krab_client.js')"));
        assert!(html.contains("await hydrator.init();"));
        assert!(html.contains("hydrator.hydrate();"));
        assert!(html.contains("hydration mismatch recovered via SSR fallback"));
        assert!(html.contains("checkRouteBudgets"));
        assert!(html.contains("KRAB-HYDRATE-500"));
    }

    #[test]
    fn smart_preload_hints_only_apply_to_island_bearing_routes() {
        std::env::set_var("KRAB_HYDRATION_MODE", "wasm");

        let home = render_home_page();
        assert!(home.contains("modulepreload"));
        assert!(home.contains("krab_client_bg.wasm"));

        let about = render_about_page();
        assert!(!about.contains("modulepreload"));

        std::env::remove_var("KRAB_HYDRATION_MODE");
    }

    #[test]
    fn minimal_js_escape_hatch_is_explicit_and_auditable() {
        std::env::set_var("KRAB_HYDRATION_MODE", "minimal_js");
        std::env::set_var("KRAB_MINIMAL_JS_AUDIT", "true");

        let html = render_home_page();
        assert!(html.contains("enableMinimalJsFallback"));
        assert!(html.contains("KRAB_MINIMAL_JS_AUDIT"));
        assert!(html.contains("KRAB-HYDRATE-200"));
        assert!(html.contains("KRAB-HYDRATE-220"));

        std::env::remove_var("KRAB_HYDRATION_MODE");
        std::env::remove_var("KRAB_MINIMAL_JS_AUDIT");
    }

    #[test]
    fn ssr_only_mode_surfaces_stable_hydration_diagnostic_code() {
        std::env::set_var("KRAB_HYDRATION_MODE", "ssr_only");

        let html = render_home_page();
        assert!(html.contains("KRAB-HYDRATE-300"));
        assert!(html.contains("SSR-only mode skips client hydration"));

        std::env::remove_var("KRAB_HYDRATION_MODE");
    }

    #[cfg(feature = "nft")]
    #[test]
    fn non_functional_load_profile_ssr_render_stability() {
        let mut render_samples_ms = Vec::with_capacity(1_000);
        for _ in 0..1_000 {
            let start = Instant::now();
            let html = render_home_page();
            assert!(html.contains("Hello from Krab!"));
            render_samples_ms.push(start.elapsed().as_millis());
        }

        let mut p95_samples = render_samples_ms.clone();
        let mut p99_samples = render_samples_ms;
        let p95 = percentile_millis(&mut p95_samples, 0.95);
        let p99 = percentile_millis(&mut p99_samples, 0.99);

        assert!(
            p95 <= P95_SSR_RENDER_MS_THRESHOLD,
            "p95 SSR render latency {}ms exceeded threshold {}ms",
            p95,
            P95_SSR_RENDER_MS_THRESHOLD
        );
        assert!(
            p99 <= P99_SSR_RENDER_MS_THRESHOLD,
            "p99 SSR render latency {}ms exceeded threshold {}ms",
            p99,
            P99_SSR_RENDER_MS_THRESHOLD
        );
    }

    #[cfg(feature = "nft")]
    #[test]
    fn non_functional_spike_profile_ssr_render_stability() {
        let mut render_samples_ms = Vec::with_capacity(3_000);
        for _ in 0..3_000 {
            let start = Instant::now();
            let html = render_home_page();
            assert!(html.contains("id=\"status\""));
            render_samples_ms.push(start.elapsed().as_millis());
        }

        let mut p95_samples = render_samples_ms.clone();
        let mut p99_samples = render_samples_ms;
        let p95 = percentile_millis(&mut p95_samples, 0.95);
        let p99 = percentile_millis(&mut p99_samples, 0.99);

        assert!(
            p95 <= P95_SSR_RENDER_MS_THRESHOLD,
            "spike profile p95 SSR render latency {}ms exceeded threshold {}ms",
            p95,
            P95_SSR_RENDER_MS_THRESHOLD
        );
        assert!(
            p99 <= P99_SSR_RENDER_MS_THRESHOLD,
            "spike profile p99 SSR render latency {}ms exceeded threshold {}ms",
            p99,
            P99_SSR_RENDER_MS_THRESHOLD
        );
    }

    #[cfg(feature = "nft")]
    #[test]
    fn non_functional_soak_profile_ssr_render_stability() {
        let mut render_samples_ms = Vec::with_capacity(10_000);
        for _ in 0..10_000 {
            let start = Instant::now();
            let html = render_home_page();
            // Stable across both macro expansion modes (with/without island wrappers).
            assert!(html.contains("Count:"));
            render_samples_ms.push(start.elapsed().as_millis());
        }

        let mut p95_samples = render_samples_ms.clone();
        let mut p99_samples = render_samples_ms;
        let p95 = percentile_millis(&mut p95_samples, 0.95);
        let p99 = percentile_millis(&mut p99_samples, 0.99);

        assert!(
            p95 <= P95_SSR_RENDER_MS_THRESHOLD,
            "soak profile p95 SSR render latency {}ms exceeded threshold {}ms",
            p95,
            P95_SSR_RENDER_MS_THRESHOLD
        );
        assert!(
            p99 <= P99_SSR_RENDER_MS_THRESHOLD,
            "soak profile p99 SSR render latency {}ms exceeded threshold {}ms",
            p99,
            P99_SSR_RENDER_MS_THRESHOLD
        );
    }

    #[cfg(feature = "nft")]
    #[test]
    fn non_functional_mixed_fast_slow_client_streaming_profile() {
        let total_samples = std::env::var("KRAB_SSR_MIXED_PROFILE_SAMPLES")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(1200)
            .max(100);
        let slow_every = std::env::var("KRAB_SSR_MIXED_PROFILE_SLOW_EVERY")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(5)
            .max(2);
        let slow_penalty_ms = std::env::var("KRAB_SSR_MIXED_PROFILE_SLOW_PENALTY_MS")
            .ok()
            .and_then(|v| v.parse::<u128>().ok())
            .unwrap_or(5);

        let mut fast_samples_ms = Vec::new();
        let mut slow_samples_ms = Vec::new();
        let mut combined_samples_ms = Vec::with_capacity(total_samples);

        for idx in 0..total_samples {
            let started = Instant::now();
            let html = render_home_page();
            assert!(html.contains("<!--krab:suspense:home:pending-->"));
            assert!(html.contains("<!--krab:suspense:home:resolved-->"));

            let mut sample_ms = started.elapsed().as_millis();
            if idx % slow_every == 0 {
                std::thread::sleep(Duration::from_millis(slow_penalty_ms as u64));
                sample_ms += slow_penalty_ms;
                slow_samples_ms.push(sample_ms);
            } else {
                fast_samples_ms.push(sample_ms);
            }
            combined_samples_ms.push(sample_ms);
        }

        assert!(!fast_samples_ms.is_empty(), "fast cohort must not be empty");
        assert!(!slow_samples_ms.is_empty(), "slow cohort must not be empty");

        let mut combined_p95_samples = combined_samples_ms.clone();
        let mut combined_p99_samples = combined_samples_ms.clone();
        let mut fast_p95_samples = fast_samples_ms.clone();
        let mut slow_p95_samples = slow_samples_ms.clone();

        let combined_p95 = percentile_millis(&mut combined_p95_samples, 0.95);
        let combined_p99 = percentile_millis(&mut combined_p99_samples, 0.99);
        let fast_p95 = percentile_millis(&mut fast_p95_samples, 0.95);
        let slow_p95 = percentile_millis(&mut slow_p95_samples, 0.95);

        let slo_mixed_p95_max = std::env::var("KRAB_SSR_STREAM_SLO_P95_MS")
            .ok()
            .and_then(|v| v.parse::<u128>().ok())
            .unwrap_or(200);
        let slo_mixed_p99_max = std::env::var("KRAB_SSR_STREAM_SLO_P99_MS")
            .ok()
            .and_then(|v| v.parse::<u128>().ok())
            .unwrap_or(400);
        let slo_fast_p95_max = std::env::var("KRAB_SSR_STREAM_FAST_P95_MS")
            .ok()
            .and_then(|v| v.parse::<u128>().ok())
            .unwrap_or(200);
        let slo_slow_p95_max = std::env::var("KRAB_SSR_STREAM_SLOW_P95_MS")
            .ok()
            .and_then(|v| v.parse::<u128>().ok())
            .unwrap_or(400);

        assert!(
            combined_p95 <= slo_mixed_p95_max,
            "mixed profile p95 {}ms exceeded SLO {}ms",
            combined_p95,
            slo_mixed_p95_max
        );
        assert!(
            combined_p99 <= slo_mixed_p99_max,
            "mixed profile p99 {}ms exceeded SLO {}ms",
            combined_p99,
            slo_mixed_p99_max
        );
        assert!(
            fast_p95 <= slo_fast_p95_max,
            "mixed fast cohort p95 {}ms exceeded SLO {}ms",
            fast_p95,
            slo_fast_p95_max
        );
        assert!(
            slow_p95 <= slo_slow_p95_max,
            "mixed slow cohort p95 {}ms exceeded SLO {}ms",
            slow_p95,
            slo_slow_p95_max
        );
    }

    #[tokio::test]
    async fn cache_tier_contract_api_paths_are_cached() {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;

        let state = AppState {
            runtime: RuntimeState::new(),
            http_client: Client::builder()
                .timeout(std::time::Duration::from_secs(1))
                .build()
                .unwrap(),
            auth_base_url: "http://127.0.0.1:1".to_string(),
            users_base_url: "http://127.0.0.1:1".to_string(),
            protocol_client: test_protocol_client(),
            isr_cache: IsrCache::new(),
            isr_revalidating: Arc::new(tokio::sync::Mutex::new(HashSet::new())),
            hmr_rx: tokio::sync::watch::channel(0).1,
        };

        let app = super::build_router(state);

        // First request - MISS
        let req1 = Request::builder()
            .uri("/data/dashboard")
            .body(Body::empty())
            .unwrap();
        let response1 = app.clone().oneshot(req1).await.unwrap();

        assert_eq!(response1.status(), axum::http::StatusCode::OK);
        assert_eq!(
            response1
                .headers()
                .get("x-cache")
                .and_then(|v| v.to_str().ok()),
            Some("MISS")
        );

        // Second request - HIT
        let req2 = Request::builder()
            .uri("/data/dashboard")
            .body(Body::empty())
            .unwrap();
        let response2 = app.oneshot(req2).await.unwrap();

        assert_eq!(response2.status(), axum::http::StatusCode::OK);
        assert_eq!(
            response2
                .headers()
                .get("x-cache")
                .and_then(|v| v.to_str().ok()),
            Some("HIT")
        );
    }

    #[tokio::test]
    async fn isr_cache_serves_fresh_then_stale() {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;

        std::env::set_var("KRAB_ISR_REVALIDATE_SECS", "1");

        let state = AppState {
            runtime: RuntimeState::new(),
            http_client: Client::builder()
                .timeout(std::time::Duration::from_secs(1))
                .build()
                .unwrap(),
            auth_base_url: "http://127.0.0.1:1".to_string(),
            users_base_url: "http://127.0.0.1:1".to_string(),
            protocol_client: test_protocol_client(),
            isr_cache: IsrCache::new(),
            isr_revalidating: Arc::new(tokio::sync::Mutex::new(HashSet::new())),
            hmr_rx: tokio::sync::watch::channel(0).1,
        };

        let app = super::build_router(state);

        let first = Request::builder().uri("/").body(Body::empty()).unwrap();
        let response1 = app.clone().oneshot(first).await.unwrap();
        assert_eq!(response1.status(), axum::http::StatusCode::OK);
        assert_eq!(
            response1
                .headers()
                .get("x-isr-state")
                .and_then(|v| v.to_str().ok()),
            Some("fresh")
        );

        tokio::time::sleep(std::time::Duration::from_millis(1200)).await;

        let second = Request::builder().uri("/").body(Body::empty()).unwrap();
        let response2 = app.clone().oneshot(second).await.unwrap();
        assert_eq!(response2.status(), axum::http::StatusCode::OK);
        assert_eq!(
            response2
                .headers()
                .get("x-isr-state")
                .and_then(|v| v.to_str().ok()),
            Some("stale")
        );
        assert_eq!(
            response2
                .headers()
                .get("x-cache")
                .and_then(|v| v.to_str().ok()),
            Some("STALE")
        );

        std::env::remove_var("KRAB_ISR_REVALIDATE_SECS");
    }

    #[tokio::test]
    async fn isr_stale_request_triggers_background_regeneration() {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;

        std::env::set_var("KRAB_ISR_REVALIDATE_SECS", "1");

        let state = AppState {
            runtime: RuntimeState::new(),
            http_client: Client::builder()
                .timeout(std::time::Duration::from_secs(1))
                .build()
                .unwrap(),
            auth_base_url: "http://127.0.0.1:1".to_string(),
            users_base_url: "http://127.0.0.1:1".to_string(),
            protocol_client: test_protocol_client(),
            isr_cache: IsrCache::new(),
            isr_revalidating: Arc::new(tokio::sync::Mutex::new(HashSet::new())),
            hmr_rx: tokio::sync::watch::channel(0).1,
        };

        let app = super::build_router(state);

        let first = Request::builder().uri("/").body(Body::empty()).unwrap();
        let _ = app.clone().oneshot(first).await.unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(1200)).await;

        let stale_req = Request::builder().uri("/").body(Body::empty()).unwrap();
        let stale_response = app.clone().oneshot(stale_req).await.unwrap();
        assert_eq!(
            stale_response
                .headers()
                .get("x-isr-state")
                .and_then(|v| v.to_str().ok()),
            Some("stale")
        );

        let mut became_fresh = false;
        for _ in 0..10 {
            tokio::time::sleep(std::time::Duration::from_millis(60)).await;
            let req = Request::builder().uri("/").body(Body::empty()).unwrap();
            let response = app.clone().oneshot(req).await.unwrap();
            let state = response
                .headers()
                .get("x-isr-state")
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .to_string();
            if state == "fresh" {
                became_fresh = true;
                break;
            }
        }

        assert!(
            became_fresh,
            "expected background ISR regeneration to refresh stale cache"
        );
        std::env::remove_var("KRAB_ISR_REVALIDATE_SECS");
    }

    #[tokio::test]
    async fn cache_authority_prefers_isr_for_page_routes_over_distributed_cache() {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;

        let state = AppState {
            runtime: RuntimeState::new(),
            http_client: Client::builder()
                .timeout(std::time::Duration::from_secs(1))
                .build()
                .unwrap(),
            auth_base_url: "http://127.0.0.1:1".to_string(),
            users_base_url: "http://127.0.0.1:1".to_string(),
            protocol_client: test_protocol_client(),
            isr_cache: IsrCache::new(),
            isr_revalidating: Arc::new(tokio::sync::Mutex::new(HashSet::new())),
            hmr_rx: tokio::sync::watch::channel(0).1,
        };

        let stale_payload = CachedHttpPayload {
            body: "<html><body>distributed</body></html>".to_string(),
            content_type: "text/html; charset=utf-8".to_string(),
        };
        let serialized = serde_json::to_string(&stale_payload).unwrap();
        let _ = state
            .runtime_state()
            .store
            .set("/", &serialized, Duration::from_secs(60))
            .await;

        let app = super::build_router(state);
        let req = Request::builder().uri("/").body(Body::empty()).unwrap();
        let response = app.clone().oneshot(req).await.unwrap();

        assert_eq!(response.status(), axum::http::StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("x-cache")
                .and_then(|v| v.to_str().ok()),
            Some("MISS")
        );
        assert_eq!(
            response
                .headers()
                .get("x-isr-state")
                .and_then(|v| v.to_str().ok()),
            Some("fresh")
        );
    }

    #[test]
    fn cache_authority_uses_route_policy_for_distributed_and_uncached_routes() {
        assert_eq!(
            cache_authority(&Method::GET, "/data/dashboard"),
            CacheAuthority::Distributed
        );
        assert_eq!(
            cache_authority(&Method::GET, "/robots.txt"),
            CacheAuthority::Distributed
        );
        assert_eq!(
            cache_authority(&Method::GET, "/api/status"),
            CacheAuthority::None
        );
        assert_eq!(cache_authority(&Method::POST, "/"), CacheAuthority::None);
    }

    #[test]
    fn distributed_cache_key_uses_namespace() {
        std::env::set_var("KRAB_CACHE_NAMESPACE", "tenant-a");
        assert_eq!(
            super::distributed_cache_key("/data/dashboard?page=1"),
            "cache:tenant-a:/data/dashboard?page=1"
        );
        std::env::remove_var("KRAB_CACHE_NAMESPACE");
        assert_eq!(
            super::distributed_cache_key("/data/dashboard?page=1"),
            "cache:default:/data/dashboard?page=1"
        );
    }

    #[test]
    fn distributed_cache_ttl_obeys_bounds() {
        std::env::remove_var("KRAB_DISTRIBUTED_CACHE_TTL_SECS");
        assert_eq!(super::distributed_cache_ttl().as_secs(), 60);

        std::env::set_var("KRAB_DISTRIBUTED_CACHE_TTL_SECS", "7200");
        assert_eq!(super::distributed_cache_ttl().as_secs(), 3600);

        std::env::set_var("KRAB_DISTRIBUTED_CACHE_TTL_SECS", "0");
        assert_eq!(super::distributed_cache_ttl().as_secs(), 1);

        std::env::set_var("KRAB_DISTRIBUTED_CACHE_TTL_SECS", "15");
        assert_eq!(super::distributed_cache_ttl().as_secs(), 15);
        std::env::remove_var("KRAB_DISTRIBUTED_CACHE_TTL_SECS");
    }

    #[tokio::test]
    async fn distributed_cache_skips_oversized_bodies() {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;

        std::env::set_var("KRAB_CACHE_MAX_BODY_BYTES", "128");
        std::env::set_var("KRAB_CACHE_NAMESPACE", "oversize-test");

        let state = AppState {
            runtime: RuntimeState::new(),
            http_client: Client::builder()
                .timeout(std::time::Duration::from_secs(1))
                .build()
                .unwrap(),
            auth_base_url: "http://127.0.0.1:1".to_string(),
            users_base_url: "http://127.0.0.1:1".to_string(),
            protocol_client: test_protocol_client(),
            isr_cache: IsrCache::new(),
            isr_revalidating: Arc::new(tokio::sync::Mutex::new(HashSet::new())),
            hmr_rx: tokio::sync::watch::channel(0).1,
        };

        let app = super::build_router(state.clone());
        let response = app
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), axum::http::StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("x-isr-state")
                .and_then(|v| v.to_str().ok()),
            Some("skip-oversize")
        );

        let cached = state
            .runtime_state()
            .store
            .get(&super::distributed_cache_key("/"))
            .await
            .unwrap();
        assert!(
            cached.is_none(),
            "oversized response should not be persisted in distributed cache"
        );

        assert!(
            state.isr_cache.get("/").await.unwrap().is_none(),
            "oversized response should not be persisted in ISR cache"
        );

        std::env::remove_var("KRAB_CACHE_MAX_BODY_BYTES");
        std::env::remove_var("KRAB_CACHE_NAMESPACE");
    }

    /// Unlisted query params must not multiply cache entries: `/?a=1` and
    /// `/?b=2` share one entry and serve identical HTML.
    #[tokio::test]
    async fn isr_cache_key_ignores_unlisted_query_params() {
        use axum::body::Body;
        use axum::http::Request;
        use http_body_util::BodyExt;
        use tower::ServiceExt;

        std::env::remove_var("FRONTEND_ISR_QUERY_ALLOWLIST");

        let state = test_state_with_protocol_client(std::time::Duration::from_secs(1));
        let cache = state.isr_cache.clone();
        let app = super::build_router(state);

        let first = app
            .clone()
            .oneshot(Request::builder().uri("/?a=1").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(
            first.headers().get("x-cache").and_then(|v| v.to_str().ok()),
            Some("MISS")
        );
        let first_html = first.into_body().collect().await.unwrap().to_bytes();

        let second = app
            .clone()
            .oneshot(Request::builder().uri("/?b=2").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(
            second
                .headers()
                .get("x-cache")
                .and_then(|v| v.to_str().ok()),
            Some("HIT"),
            "a different unlisted query string must hit the same entry"
        );
        let second_html = second.into_body().collect().await.unwrap().to_bytes();

        assert_eq!(
            first_html, second_html,
            "both query variants must serve identical HTML"
        );
        assert_eq!(
            cache.len().await.unwrap(),
            1,
            "one cache entry per path, not per query string"
        );
    }

    #[tokio::test]
    async fn isr_cache_key_distinguishes_allowlisted_query_params() {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;

        std::env::set_var("FRONTEND_ISR_QUERY_ALLOWLIST", "page");

        let state = test_state_with_protocol_client(std::time::Duration::from_secs(1));
        let cache = state.isr_cache.clone();
        let app = super::build_router(state);

        let x_cache = |uri: &str| {
            let app = app.clone();
            let uri = uri.to_string();
            async move {
                let response = app
                    .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
                    .await
                    .unwrap();
                response
                    .headers()
                    .get("x-cache")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or_default()
                    .to_string()
            }
        };

        assert_eq!(x_cache("/?page=1").await, "MISS");
        assert_eq!(
            x_cache("/?page=1").await,
            "HIT",
            "the same allowlisted value must hit its entry"
        );
        assert_eq!(
            x_cache("/?page=2").await,
            "MISS",
            "a different allowlisted value must get its own entry"
        );
        assert_eq!(cache.len().await.unwrap(), 2);

        std::env::remove_var("FRONTEND_ISR_QUERY_ALLOWLIST");
    }

    #[tokio::test]
    async fn contact_routes_are_public_and_submission_contract_is_stable() {
        use axum::body::Body;
        use axum::http::{Method, Request};
        use tower::ServiceExt;

        let state = AppState {
            runtime: RuntimeState::new(),
            http_client: Client::builder()
                .timeout(std::time::Duration::from_secs(1))
                .build()
                .unwrap(),
            auth_base_url: "http://127.0.0.1:1".to_string(),
            users_base_url: "http://127.0.0.1:1".to_string(),
            protocol_client: test_protocol_client(),
            isr_cache: IsrCache::new(),
            isr_revalidating: Arc::new(tokio::sync::Mutex::new(HashSet::new())),
            hmr_rx: tokio::sync::watch::channel(0).1,
        };
        let app = super::build_router(state);

        let get_contact = Request::builder()
            .method(Method::GET)
            .uri("/contact")
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(get_contact).await.unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);

        let submit_contact = Request::builder()
            .method(Method::POST)
            .uri("/api/contact")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"name":"Alex","email":"alex@example.com","message":"Hello team"}"#,
            ))
            .unwrap();
        let response = app.oneshot(submit_contact).await.unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::ACCEPTED);
    }

    #[test]
    fn security_log_redaction_masks_email_local_part() {
        let redacted = redact_email_for_log("alex@example.com");
        assert!(redacted.contains("@example.com"));
        assert!(!redacted.contains("alex"));
        assert!(redacted.starts_with("hash:"));
    }

    #[test]
    fn security_log_redaction_masks_name_content() {
        let redacted = redact_name_for_log("Alex Example");
        assert_eq!(redacted, "len:12");
        assert!(!redacted.contains("Alex"));
        assert!(!redacted.contains("Example"));
    }

    #[tokio::test]
    async fn blog_slug_route_renders_successfully() {
        use axum::body::Body;
        use axum::http::{Method, Request};
        use tower::ServiceExt;

        let state = AppState {
            runtime: RuntimeState::new(),
            http_client: Client::builder()
                .timeout(std::time::Duration::from_secs(1))
                .build()
                .unwrap(),
            auth_base_url: "http://127.0.0.1:1".to_string(),
            users_base_url: "http://127.0.0.1:1".to_string(),
            protocol_client: test_protocol_client(),
            isr_cache: IsrCache::new(),
            isr_revalidating: Arc::new(tokio::sync::Mutex::new(HashSet::new())),
            hmr_rx: tokio::sync::watch::channel(0).1,
        };
        let app = super::build_router(state);

        let request = Request::builder()
            .method(Method::GET)
            .uri("/blog/integration-check")
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);
    }

    #[tokio::test]
    async fn i18n_home_uses_accept_language_locale() {
        use axum::body::Body;
        use axum::http::{Method, Request};
        use http_body_util::BodyExt;
        use tower::ServiceExt;

        let state = AppState {
            runtime: RuntimeState::new(),
            http_client: Client::builder()
                .timeout(std::time::Duration::from_secs(1))
                .build()
                .unwrap(),
            auth_base_url: "http://127.0.0.1:1".to_string(),
            users_base_url: "http://127.0.0.1:1".to_string(),
            protocol_client: test_protocol_client(),
            isr_cache: IsrCache::new(),
            isr_revalidating: Arc::new(tokio::sync::Mutex::new(HashSet::new())),
            hmr_rx: tokio::sync::watch::channel(0).1,
        };
        let app = super::build_router(state);

        let request = Request::builder()
            .method(Method::GET)
            .uri("/")
            .header("accept-language", "ne-NP,ne;q=0.9,en;q=0.5")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let body_bytes = response.into_body().collect().await.unwrap().to_bytes();
        let html = String::from_utf8(body_bytes.to_vec()).unwrap();
        assert!(html.contains("क्र्याबबाट नमस्ते!"));
    }

    #[tokio::test]
    async fn websocket_ergonomic_publish_endpoint_is_available() {
        use axum::body::Body;
        use axum::http::{Method, Request};
        use http_body_util::BodyExt;
        use tower::ServiceExt;

        std::env::set_var("KRAB_AUTH_PUBLIC_PATHS", "/api/ws/*");

        let state = AppState {
            runtime: RuntimeState::new(),
            http_client: Client::builder()
                .timeout(std::time::Duration::from_secs(1))
                .build()
                .unwrap(),
            auth_base_url: "http://127.0.0.1:1".to_string(),
            users_base_url: "http://127.0.0.1:1".to_string(),
            protocol_client: test_protocol_client(),
            isr_cache: IsrCache::new(),
            isr_revalidating: Arc::new(tokio::sync::Mutex::new(HashSet::new())),
            hmr_rx: tokio::sync::watch::channel(0).1,
        };
        let app = super::build_router(state);

        let request = Request::builder()
            .method(Method::POST)
            .uri("/api/ws/publish")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"message":"hello from publish"}"#))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let body_bytes = response.into_body().collect().await.unwrap().to_bytes();
        let payload = String::from_utf8(body_bytes.to_vec()).unwrap();
        assert!(payload.contains("published"));

        std::env::remove_var("KRAB_AUTH_PUBLIC_PATHS");
    }
}
