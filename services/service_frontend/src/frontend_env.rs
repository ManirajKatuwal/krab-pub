use std::time::Duration;

use krab_core::service_contract::{ServiceTopology, TopologyRuntime};
use serde::Serialize;

const DEFAULT_DISTRIBUTED_CACHE_TTL_SECS: u64 = 60;
const MAX_DISTRIBUTED_CACHE_TTL_SECS: u64 = 3600;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum HydrationMode {
    Wasm,
    MinimalJs,
    SsrOnly,
}

impl HydrationMode {
    pub(crate) fn from_env() -> Self {
        match std::env::var("KRAB_HYDRATION_MODE") {
            Ok(raw) => match raw.trim().to_ascii_lowercase().as_str() {
                "wasm" => Self::Wasm,
                "minimal_js" | "minimal-js" => Self::MinimalJs,
                "ssr_only" | "ssr-only" => Self::SsrOnly,
                other => {
                    tracing::warn!(
                        event = "hydration_mode_invalid_fallback",
                        code = "KRAB-HYDRATE-001",
                        mode = %other,
                        "invalid KRAB_HYDRATION_MODE value, falling back to wasm"
                    );
                    Self::Wasm
                }
            },
            Err(_) => Self::Wasm,
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Wasm => "wasm",
            Self::MinimalJs => "minimal_js",
            Self::SsrOnly => "ssr_only",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct RouteHydrationBudget {
    pub(crate) route: String,
    pub(crate) max_js_kb: usize,
    pub(crate) max_wasm_kb: usize,
    pub(crate) max_startup_ms: u64,
    pub(crate) island_count_hint: usize,
    pub(crate) mode: &'static str,
}

pub(crate) fn usize_env(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(default)
}

pub(crate) fn u64_env(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(default)
}

pub(crate) fn bool_env(name: &str, default: bool) -> bool {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_ascii_lowercase())
        .and_then(|v| match v.as_str() {
            "1" | "true" | "yes" | "on" => Some(true),
            "0" | "false" | "no" | "off" => Some(false),
            _ => None,
        })
        .unwrap_or(default)
}

pub(crate) fn hydration_budget_for_route(path: &str, mode: HydrationMode) -> RouteHydrationBudget {
    let route = if path == "/" {
        "/"
    } else if path.starts_with("/blog/") {
        "/blog/:slug"
    } else {
        path
    };

    match route {
        "/" => RouteHydrationBudget {
            route: "/".to_string(),
            max_js_kb: usize_env("KRAB_HYDRATION_BUDGET_HOME_JS_KB", 160),
            max_wasm_kb: usize_env("KRAB_HYDRATION_BUDGET_HOME_WASM_KB", 512),
            max_startup_ms: u64_env("KRAB_HYDRATION_BUDGET_HOME_STARTUP_MS", 1500),
            island_count_hint: 3,
            mode: mode.as_str(),
        },
        _ => RouteHydrationBudget {
            route: route.to_string(),
            max_js_kb: 0,
            max_wasm_kb: 0,
            max_startup_ms: 0,
            island_count_hint: 0,
            mode: mode.as_str(),
        },
    }
}

pub(crate) fn hydration_preload_links_html(budget: &RouteHydrationBudget) -> String {
    if budget.island_count_hint == 0 || budget.mode != "wasm" {
        return String::new();
    }

    [
        "<link rel=\"modulepreload\" href=\"/pkg/krab_client.js\" fetchpriority=\"high\" />",
        "<link rel=\"preload\" as=\"fetch\" type=\"application/wasm\" href=\"/pkg/krab_client_bg.wasm\" crossorigin=\"anonymous\" fetchpriority=\"high\" />",
    ]
    .join("")
}

pub(crate) fn isr_revalidate_duration() -> Duration {
    let secs = std::env::var("KRAB_ISR_REVALIDATE_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(30)
        .max(1);
    Duration::from_secs(secs)
}

pub(crate) fn distributed_cache_ttl() -> Duration {
    let secs = std::env::var("KRAB_DISTRIBUTED_CACHE_TTL_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DEFAULT_DISTRIBUTED_CACHE_TTL_SECS)
        .clamp(1, MAX_DISTRIBUTED_CACHE_TTL_SECS);
    Duration::from_secs(secs)
}

pub(crate) fn stream_budget_bytes() -> usize {
    std::env::var("KRAB_SSR_STREAM_BUDGET_BYTES")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(2 * 1024 * 1024)
        .max(1024)
}

pub(crate) fn normalize_public_base_url() -> String {
    std::env::var("KRAB_PUBLIC_BASE_URL")
        .ok()
        .map(|v| v.trim().trim_end_matches('/').to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "http://localhost:3000".to_string())
}

pub(crate) fn normalize_service_base_url(name: &str, default_url: &str) -> String {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().trim_end_matches('/').to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default_url.to_string())
}

#[allow(dead_code)]
pub(crate) fn env_trimmed(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

pub(crate) fn resolve_service_base_url(
    topology: &TopologyRuntime,
    domain: &str,
    env_name: &str,
    default_url: &str,
) -> String {
    if topology.mode == ServiceTopology::Distributed {
        if let Some(endpoint) = topology.endpoint_for(domain) {
            let base_url = endpoint.base_url.trim().trim_end_matches('/');
            if !base_url.is_empty() {
                return base_url.to_string();
            }
        }
    }

    normalize_service_base_url(env_name, default_url)
}
