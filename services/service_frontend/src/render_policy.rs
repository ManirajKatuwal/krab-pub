use krab_core::render_policy::{CacheMode, EdgeCapability, RenderMode, RouteRenderPolicy};

use crate::frontend_env::{distributed_cache_ttl, isr_revalidate_duration};

fn normalized_route_pattern(path: &str) -> &str {
    if path == "/" {
        "/"
    } else if path.starts_with("/blog/") {
        "/blog/:slug"
    } else {
        path
    }
}

pub(crate) fn route_render_policy(path: &str) -> Option<RouteRenderPolicy> {
    let route_pattern = normalized_route_pattern(path);

    let policy = match route_pattern {
        "/" => RouteRenderPolicy::new(
            "/",
            RenderMode::Server,
            CacheMode::Isr {
                revalidate_after: isr_revalidate_duration(),
            },
        )
        .with_edge_capability(EdgeCapability::Eligible)
        .with_streaming(true),
        "/about" | "/greet" | "/blog/:slug" => RouteRenderPolicy::new(
            route_pattern,
            RenderMode::Server,
            CacheMode::Isr {
                revalidate_after: isr_revalidate_duration(),
            },
        )
        .with_edge_capability(EdgeCapability::Eligible),
        "/robots.txt" | "/sitemap.xml" | "/asset-manifest.json" => RouteRenderPolicy::new(
            route_pattern,
            RenderMode::Static,
            CacheMode::Swr {
                stale_after: distributed_cache_ttl(),
            },
        )
        .with_edge_capability(EdgeCapability::Eligible),
        "/data/dashboard" | "/rpc/version" => RouteRenderPolicy::new(
            route_pattern,
            RenderMode::Server,
            CacheMode::Swr {
                stale_after: distributed_cache_ttl(),
            },
        )
        .with_edge_capability(EdgeCapability::Eligible),
        "/api/status" | "/rpc/now" => {
            RouteRenderPolicy::new(route_pattern, RenderMode::Server, CacheMode::None)
        }
        _ => return None,
    };

    policy.validates().ok().map(|_| policy)
}

pub(crate) fn page_render_policy(path: &str) -> Option<RouteRenderPolicy> {
    let route_pattern = normalized_route_pattern(path);
    if !matches!(route_pattern, "/" | "/about" | "/greet" | "/blog/:slug") {
        return None;
    }

    route_render_policy(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_page_policy_is_streaming_server_isr() {
        let policy = page_render_policy("/").expect("home policy should exist");

        assert_eq!(policy.route_pattern, "/");
        assert_eq!(policy.render_mode, RenderMode::Server);
        assert!(matches!(policy.cache_mode, CacheMode::Isr { .. }));
        assert_eq!(policy.edge_capability, EdgeCapability::Eligible);
        assert!(policy.streaming);
    }

    #[test]
    fn blog_page_policy_normalizes_slug_route() {
        let policy = page_render_policy("/blog/hello-world").expect("blog policy should exist");

        assert_eq!(policy.route_pattern, "/blog/:slug");
        assert!(policy.validates().is_ok());
    }

    #[test]
    fn non_page_route_has_no_page_policy() {
        assert!(page_render_policy("/data/dashboard").is_none());
    }

    #[test]
    fn dashboard_route_policy_is_server_swr() {
        let policy = route_render_policy("/data/dashboard").expect("dashboard policy should exist");

        assert_eq!(policy.route_pattern, "/data/dashboard");
        assert_eq!(policy.render_mode, RenderMode::Server);
        assert!(matches!(policy.cache_mode, CacheMode::Swr { .. }));
        assert!(policy.uses_distributed_cache());
    }

    #[test]
    fn robots_route_policy_is_static_swr() {
        let policy = route_render_policy("/robots.txt").expect("robots policy should exist");

        assert_eq!(policy.route_pattern, "/robots.txt");
        assert_eq!(policy.render_mode, RenderMode::Static);
        assert!(matches!(policy.cache_mode, CacheMode::Swr { .. }));
        assert!(policy.validates().is_ok());
    }

    #[test]
    fn status_route_policy_is_uncached_server_response() {
        let policy = route_render_policy("/api/status").expect("status policy should exist");

        assert_eq!(policy.route_pattern, "/api/status");
        assert_eq!(policy.render_mode, RenderMode::Server);
        assert_eq!(policy.cache_mode, CacheMode::None);
        assert!(!policy.uses_distributed_cache());
        assert!(!policy.is_isr());
    }
}
