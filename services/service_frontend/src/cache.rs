use axum::extract::{Request, State};
use axum::http::Method;
use axum::middleware::Next;
use axum::response::Response;
use http_body_util::BodyExt;

use crate::app_state::{AppState, CachedHttpPayload};
use krab_core::http::HasRuntimeState;

// Use IsrPolicy from core explicitly inside `cache_middleware` if needed, or re-export it.
use axum::body::Body;
use krab_core::isr::IsrPolicy;
use krab_core::render_policy::CacheMode;
use krab_core::render_stream::{SuspenseMarker, SuspenseState};
use std::collections::HashMap;

use crate::frontend_env::{distributed_cache_ttl, isr_revalidate_duration};
use crate::render_policy::route_render_policy;
use crate::trigger_isr_revalidation;

const DEFAULT_CACHE_MAX_BODY_BYTES: usize = 10 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CacheAuthority {
    Isr,
    Distributed,
    None,
}

pub fn cache_authority(method: &Method, path: &str) -> CacheAuthority {
    if *method != Method::GET {
        return CacheAuthority::None;
    }

    if let Some(policy) = route_render_policy(path) {
        return match policy.cache_mode {
            CacheMode::Isr { .. } => CacheAuthority::Isr,
            CacheMode::Swr { .. } | CacheMode::Static => CacheAuthority::Distributed,
            CacheMode::None => CacheAuthority::None,
        };
    }

    CacheAuthority::None
}

pub fn cache_max_body_bytes() -> usize {
    std::env::var("KRAB_CACHE_MAX_BODY_BYTES")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(DEFAULT_CACHE_MAX_BODY_BYTES)
        .max(1024)
}

pub fn distributed_cache_namespace() -> String {
    std::env::var("KRAB_CACHE_NAMESPACE")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "default".to_string())
}

pub fn distributed_cache_key(uri: &str) -> String {
    format!("cache:{}:{}", distributed_cache_namespace(), uri)
}

pub fn is_finalized_ssr_snapshot(html: &str) -> bool {
    let mut boundary_state: HashMap<String, (usize, usize, usize)> = HashMap::new();

    for segment in html.split("<!--").skip(1) {
        let Some(comment_end) = segment.find("-->") else {
            continue;
        };
        let marker_raw = format!("<!--{}-->", &segment[..comment_end]);
        let Some(marker) = SuspenseMarker::parse(&marker_raw) else {
            continue;
        };

        let counts = boundary_state
            .entry(marker.boundary_id)
            .or_insert((0usize, 0usize, 0usize));
        match marker.state {
            SuspenseState::Pending => counts.0 += 1,
            SuspenseState::Resolved => counts.1 += 1,
            SuspenseState::Error => counts.2 += 1,
        }
    }

    if boundary_state.is_empty() {
        return true;
    }

    boundary_state
        .values()
        .all(|(pending, resolved, error)| *pending > 0 && *pending == (*resolved + *error))
}

pub async fn cache_middleware(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let path = req.uri().path().to_string();
    let cache_key = req.uri().to_string(); // Include query params in cache key
    let method = req.method().clone();

    let authority = cache_authority(&method, &path);
    let isr_eligible = authority == CacheAuthority::Isr;
    let distributed_eligible = authority == CacheAuthority::Distributed;
    let distributed_key = distributed_eligible.then(|| distributed_cache_key(&cache_key));

    if isr_eligible {
        // A cache read that fails is a degraded cache, not a failed request:
        // fall through and render the page rather than 500.
        let cached = match state.isr_cache.get(&cache_key).await {
            Ok(entry) => entry,
            Err(error) => {
                tracing::warn!(
                    event = "isr_cache_read_failed",
                    http.route = %path,
                    %error,
                    "serving uncached after an ISR read failure"
                );
                None
            }
        };

        if let Some(entry) = cached {
            let state_header = if entry.is_stale() { "stale" } else { "fresh" };

            let mut res = Response::new(Body::from(entry.html.into_bytes()));
            res.headers_mut().insert(
                axum::http::header::CONTENT_TYPE,
                axum::http::HeaderValue::from_static("text/html; charset=utf-8"),
            );
            if let Ok(etag) = entry.etag.parse() {
                res.headers_mut().insert(axum::http::header::ETAG, etag);
            }

            if state_header == "stale" {
                let cache_key_bg = cache_key.clone();
                let path_bg = path.clone();
                let state_bg = state.clone();
                tokio::spawn(async move {
                    trigger_isr_revalidation(state_bg, cache_key_bg, path_bg).await;
                });
                res.headers_mut().insert(
                    axum::http::header::HeaderName::from_static("x-cache"),
                    axum::http::HeaderValue::from_static("STALE"),
                );
            } else {
                res.headers_mut().insert(
                    axum::http::header::HeaderName::from_static("x-cache"),
                    axum::http::HeaderValue::from_static("HIT"),
                );
            }

            res.headers_mut().insert(
                axum::http::header::HeaderName::from_static("x-isr-state"),
                axum::http::HeaderValue::from_static(state_header),
            );

            return res;
        }
    }

    if authority == CacheAuthority::None {
        return next.run(req).await;
    }

    if distributed_eligible {
        // Check distributed cache
        if let Some(distributed_key) = distributed_key.as_deref() {
            if let Ok(Some(raw)) = state.runtime_state().store.get(distributed_key).await {
                if let Ok(entry) = serde_json::from_str::<CachedHttpPayload>(&raw) {
                    tracing::debug!(path = %cache_key, cache_key = %distributed_key, "cache_hit");
                    let mut res = Response::new(Body::from(entry.body.into_bytes()));
                    res.headers_mut().insert(
                        axum::http::header::CONTENT_TYPE,
                        entry
                            .content_type
                            .parse()
                            .unwrap_or(axum::http::HeaderValue::from_static("text/html")),
                    );
                    res.headers_mut().insert(
                        axum::http::header::HeaderName::from_static("x-cache"),
                        axum::http::HeaderValue::from_static("HIT"),
                    );
                    res.headers_mut().insert(
                        axum::http::header::HeaderName::from_static("x-cache-ttl-secs"),
                        axum::http::HeaderValue::from_str(
                            &distributed_cache_ttl().as_secs().to_string(),
                        )
                        .unwrap_or(axum::http::HeaderValue::from_static("60")),
                    );
                    return res;
                }
            }
        }
    }

    tracing::debug!(path = %path, "cache_miss");
    let res = next.run(req).await;

    // Only cache successful responses
    if !res.status().is_success() {
        return res;
    }

    // Extract body and cache it
    let (parts, body) = res.into_parts();

    // We need to buffer the body to store it.
    // Limit size to avoid memory issues (e.g. 10MB)
    let bytes = match body.collect().await {
        Ok(collected) => collected.to_bytes(),
        Err(err) => {
            tracing::error!("failed to read response body for caching: {}", err);
            return Response::from_parts(parts, Body::empty());
        }
    };

    let content_type = parts
        .headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("text/html")
        .to_string();

    let max_cache_body_bytes = cache_max_body_bytes();
    let cacheable_body = bytes.len() <= max_cache_body_bytes;
    if !cacheable_body {
        tracing::warn!(
            event = "cache_body_too_large_skip_store",
            path = %path,
            cache_key = %cache_key,
            body_bytes = bytes.len(),
            max_cache_body_bytes,
            "skipping cache store for oversized response body"
        );
    }

    let html_for_isr = String::from_utf8(bytes.to_vec()).ok();

    let distributed_payload = if distributed_eligible && cacheable_body {
        distributed_key.as_deref().zip(html_for_isr.clone())
    } else {
        None
    };

    if let Some((distributed_key, body)) = distributed_payload {
        let payload = CachedHttpPayload {
            body,
            content_type: content_type.clone(),
        };
        if let Ok(serialized) = serde_json::to_string(&payload) {
            let _ = state
                .runtime_state()
                .store
                .set(distributed_key, &serialized, distributed_cache_ttl())
                .await;
        }
    }

    let mut res = Response::from_parts(parts, Body::from(bytes));
    res.headers_mut().insert(
        axum::http::header::HeaderName::from_static("x-cache"),
        axum::http::HeaderValue::from_static("MISS"),
    );
    if distributed_eligible {
        res.headers_mut().insert(
            axum::http::header::HeaderName::from_static("x-cache-ttl-secs"),
            axum::http::HeaderValue::from_str(&distributed_cache_ttl().as_secs().to_string())
                .unwrap_or(axum::http::HeaderValue::from_static("60")),
        );
        if !cacheable_body {
            res.headers_mut().insert(
                axum::http::header::HeaderName::from_static("x-cache-store"),
                axum::http::HeaderValue::from_static("SKIP_OVERSIZE"),
            );
        }
    }

    if isr_eligible {
        if !cacheable_body {
            res.headers_mut().insert(
                axum::http::header::HeaderName::from_static("x-isr-state"),
                axum::http::HeaderValue::from_static("skip-oversize"),
            );
        } else if let Some(html) = html_for_isr {
            if is_finalized_ssr_snapshot(&html) {
                // A failed write means the next request re-renders — worse for
                // latency, correct for content. Never fail the response over it.
                let stored = state
                    .isr_cache
                    .put(
                        &cache_key,
                        html,
                        IsrPolicy::revalidate(isr_revalidate_duration()),
                    )
                    .await;

                let isr_state = match stored {
                    Ok(()) => "fresh",
                    Err(error) => {
                        tracing::warn!(
                            event = "isr_cache_write_failed",
                            cache_key = %cache_key,
                            http.route = %path,
                            %error,
                            "response served but not cached"
                        );
                        "store-error"
                    }
                };

                res.headers_mut().insert(
                    axum::http::header::HeaderName::from_static("x-isr-state"),
                    axum::http::HeaderValue::from_static(isr_state),
                );
            } else {
                tracing::warn!(
                    event = "isr_cache_snapshot_skipped_non_finalized",
                    cache_key = %cache_key,
                    path = %path,
                    "skipping ISR cache write because snapshot is not finalized"
                );
                res.headers_mut().insert(
                    axum::http::header::HeaderName::from_static("x-isr-state"),
                    axum::http::HeaderValue::from_static("skip-non-final"),
                );
            }
        }
    }

    res
}
