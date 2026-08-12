use axum::extract::{Request, State};
use axum::http::Method;
use axum::middleware::Next;
use axum::response::Response;
use http_body_util::BodyExt;

use crate::app_state::{AppState, CachedHttpPayload};
use krab_core::http::HasRuntimeState;

// Use IsrPolicy from core explicitly inside `cache_middleware` if needed, or re-export it.
use axum::body::Body;
use krab_core::isr::{IsrEntry, IsrPolicy, IsrServeOutcome};
use krab_core::render_policy::CacheMode;
use krab_core::render_stream::{SuspenseMarker, SuspenseState};
use std::collections::HashMap;
use std::time::Duration;

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

/// Query parameters allowed to participate in cache keys, from
/// `FRONTEND_ISR_QUERY_ALLOWLIST` (comma-separated). Default: empty, meaning
/// cache keys are path-only.
pub fn isr_query_allowlist() -> Vec<String> {
    std::env::var("FRONTEND_ISR_QUERY_ALLOWLIST")
        .ok()
        .map(|raw| {
            raw.split(',')
                .map(|name| name.trim().to_string())
                .filter(|name| !name.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// Cache key for a request: the path plus only allowlisted query parameters,
/// sorted for determinism.
///
/// Keying on the raw request URI let every distinct query string mint its own
/// cache entry — unbounded key cardinality from attacker-chosen URLs
/// (`/page?x=1`, `/page?x=2`, …), each one a cold render plus a stored copy of
/// the page. Parameters outside the allowlist are dropped from the key, so
/// they can no longer multiply entries; renderers ignore them anyway (ISR
/// pages render from the path). Invalidation and revalidation both operate on
/// this same normalized key, and ETags derive from the cached HTML, so both
/// stay consistent with it.
pub fn normalized_cache_key(uri: &axum::http::Uri) -> String {
    let path = uri.path();
    let Some(query) = uri.query() else {
        return path.to_string();
    };

    let allowlist = isr_query_allowlist();
    if allowlist.is_empty() {
        return path.to_string();
    }

    let mut kept: Vec<(&str, &str)> = query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| pair.split_once('=').unwrap_or((pair, "")))
        .filter(|(name, _)| allowlist.iter().any(|allowed| allowed == name))
        .collect();
    if kept.is_empty() {
        return path.to_string();
    }
    kept.sort_unstable();

    let joined = kept
        .iter()
        .map(|(name, value)| {
            if value.is_empty() {
                (*name).to_string()
            } else {
                format!("{name}={value}")
            }
        })
        .collect::<Vec<_>>()
        .join("&");
    format!("{path}?{joined}")
}

/// How long a request that lost the cold-miss render lease polls for the
/// winner's entry before rendering anyway (fail open).
const ISR_COLD_WAIT_ATTEMPTS: u32 = 10;
const ISR_COLD_WAIT_INTERVAL: Duration = Duration::from_millis(100);

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

/// Build the response for an ISR cache hit and, when the entry is stale,
/// start background revalidation.
fn serve_isr_hit(state: &AppState, cache_key: &str, path: &str, entry: IsrEntry) -> Response {
    let state_header = if entry.is_stale() { "stale" } else { "fresh" };

    let etag = entry.etag.clone();
    let mut res = Response::new(Body::from(entry.html.into_bytes()));
    res.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("text/html; charset=utf-8"),
    );
    if let Ok(etag) = etag.parse() {
        res.headers_mut().insert(axum::http::header::ETAG, etag);
    }

    if state_header == "stale" {
        let cache_key_bg = cache_key.to_string();
        let path_bg = path.to_string();
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

    res
}

pub async fn cache_middleware(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let path = req.uri().path().to_string();
    // Path plus allowlisted query params only — raw URIs gave every distinct
    // query string its own entry (unbounded cardinality).
    let cache_key = normalized_cache_key(req.uri());
    let method = req.method().clone();

    let authority = cache_authority(&method, &path);
    let isr_eligible = authority == CacheAuthority::Isr;
    let distributed_eligible = authority == CacheAuthority::Distributed;
    let distributed_key = distributed_eligible.then(|| distributed_cache_key(&cache_key));

    // True when this request won the cold-miss render lease and must release
    // it once the render outcome is settled.
    let mut isr_lease_held = false;

    if isr_eligible {
        // A cache read that fails is a degraded cache, not a failed request:
        // fall through and render the page rather than 500.
        match state.isr_cache.serve_or_lease(&cache_key).await {
            Ok(IsrServeOutcome::Hit(entry)) => {
                return serve_isr_hit(&state, &cache_key, &path, entry);
            }
            Ok(IsrServeOutcome::MissAcquired) => {
                isr_lease_held = true;
            }
            Ok(IsrServeOutcome::MissLocked) => {
                // Another request (possibly on another replica) is rendering
                // this page. Poll briefly for its entry; if it never lands,
                // render anyway — the lease must not make requests fail.
                for _ in 0..ISR_COLD_WAIT_ATTEMPTS {
                    tokio::time::sleep(ISR_COLD_WAIT_INTERVAL).await;
                    match state.isr_cache.get(&cache_key).await {
                        Ok(Some(entry)) => {
                            return serve_isr_hit(&state, &cache_key, &path, entry);
                        }
                        Ok(None) => {}
                        Err(_) => break,
                    }
                }
                tracing::debug!(
                    event = "isr_cold_miss_wait_timeout",
                    cache_key = %cache_key,
                    http.route = %path,
                    "lease holder did not populate in time; rendering anyway"
                );
            }
            Err(error) => {
                tracing::warn!(
                    event = "isr_cache_read_failed",
                    http.route = %path,
                    %error,
                    "serving uncached after an ISR read failure"
                );
            }
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
        if isr_lease_held {
            let _ = state.isr_cache.release_lease(&cache_key).await;
        }
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
            if isr_lease_held {
                let _ = state.isr_cache.release_lease(&cache_key).await;
            }
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

    // Whatever happened above — stored, skipped, or errored — the render is
    // settled, so let go of the cold-miss lease rather than making the next
    // request wait out its TTL. The lease expires on its own if this is missed.
    if isr_lease_held {
        let _ = state.isr_cache.release_lease(&cache_key).await;
    }

    res
}

// Serial: these tests mutate FRONTEND_ISR_QUERY_ALLOWLIST, shared process env.
#[cfg(test)]
#[serial_test::serial]
mod cache_key_tests {
    use super::*;
    use axum::http::Uri;

    #[test]
    fn default_cache_key_is_path_only() {
        std::env::remove_var("FRONTEND_ISR_QUERY_ALLOWLIST");

        let with_query: Uri = "/page?a=1&b=2".parse().unwrap();
        assert_eq!(normalized_cache_key(&with_query), "/page");

        let bare: Uri = "/page".parse().unwrap();
        assert_eq!(normalized_cache_key(&bare), "/page");
    }

    #[test]
    fn allowlisted_params_are_kept_sorted_and_the_rest_dropped() {
        std::env::set_var("FRONTEND_ISR_QUERY_ALLOWLIST", "page, lang");

        let uri: Uri = "/blog?utm_source=x&page=2&lang=en".parse().unwrap();
        assert_eq!(normalized_cache_key(&uri), "/blog?lang=en&page=2");

        // Parameter order in the URL must not change the key.
        let reordered: Uri = "/blog?lang=en&utm_source=y&page=2".parse().unwrap();
        assert_eq!(normalized_cache_key(&reordered), "/blog?lang=en&page=2");

        std::env::remove_var("FRONTEND_ISR_QUERY_ALLOWLIST");
    }

    #[test]
    fn query_with_no_allowlisted_params_falls_back_to_path_only() {
        std::env::set_var("FRONTEND_ISR_QUERY_ALLOWLIST", "page");

        let uri: Uri = "/blog?utm_source=x&utm_medium=y".parse().unwrap();
        assert_eq!(normalized_cache_key(&uri), "/blog");

        std::env::remove_var("FRONTEND_ISR_QUERY_ALLOWLIST");
    }

    #[test]
    fn allowlist_env_parsing_trims_and_skips_empty_segments() {
        std::env::set_var("FRONTEND_ISR_QUERY_ALLOWLIST", " page ,, lang ,");
        assert_eq!(isr_query_allowlist(), vec!["page", "lang"]);

        std::env::remove_var("FRONTEND_ISR_QUERY_ALLOWLIST");
        assert!(isr_query_allowlist().is_empty());
    }
}
