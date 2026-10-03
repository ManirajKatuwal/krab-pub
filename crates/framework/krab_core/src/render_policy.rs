//! A per-route declaration of how a page is rendered, cached, placed and
//! delivered.
//!
//! A [`RouteRenderPolicy`] is a statement of intent: it installs no caching,
//! routing or streaming by itself. Services and tooling read it, and
//! [`RouteRenderPolicy::validates`] rejects contradictory combinations so a
//! service can refuse to start on one. See `docs/architecture/render_policy.md`.

use std::time::Duration;

/// Where and when a route's HTML is produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderMode {
    /// Deterministic output, the same for every request.
    Static,
    /// Rendered on the server per request (server-side rendering).
    Server,
    /// Rendered in the browser; the server sends only a shell.
    ClientOnly,
}

/// How a route's rendered output is cached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheMode {
    /// Not cached.
    None,
    /// Cached with no time-based revalidation.
    Static,
    /// Incremental static regeneration: cached, and regenerated on the server
    /// once older than `revalidate_after`.
    Isr {
        /// Age after which a cached render is stale and is regenerated.
        revalidate_after: Duration,
    },
    /// Stale-while-revalidate: a stale entry is served while a fresh one is
    /// fetched.
    Swr {
        /// Age after which a cached response counts as stale.
        stale_after: Duration,
    },
}

/// Whether a route may be served from an edge location rather than the
/// origin. Declarative only: nothing in Krab routes traffic to an edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeCapability {
    /// Must be served by the origin. The default.
    OriginOnly,
    /// May run at the edge.
    Eligible,
    /// Should run at the edge where available.
    Preferred,
    /// Must run at the edge.
    Required,
}

/// The render, cache, edge and streaming policy for one route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteRenderPolicy {
    /// The route the policy describes, for example `/blog/:slug`. Not
    /// matched against requests by this type.
    pub route_pattern: String,
    /// How the route is rendered.
    pub render_mode: RenderMode,
    /// How its output is cached.
    pub cache_mode: CacheMode,
    /// Whether it may run at the edge.
    pub edge_capability: EdgeCapability,
    /// Whether the route may send its response as a chunked stream (see
    /// [`crate::render_stream`]); declarative.
    pub streaming: bool,
}

impl RouteRenderPolicy {
    /// A policy for `route_pattern` with [`EdgeCapability::OriginOnly`] and
    /// streaming off.
    pub fn new(
        route_pattern: impl Into<String>,
        render_mode: RenderMode,
        cache_mode: CacheMode,
    ) -> Self {
        Self {
            route_pattern: route_pattern.into(),
            render_mode,
            cache_mode,
            edge_capability: EdgeCapability::OriginOnly,
            streaming: false,
        }
    }

    /// Sets the edge capability.
    pub fn with_edge_capability(mut self, edge_capability: EdgeCapability) -> Self {
        self.edge_capability = edge_capability;
        self
    }

    /// Sets whether the route may stream.
    pub fn with_streaming(mut self, streaming: bool) -> Self {
        self.streaming = streaming;
        self
    }

    /// Checks the combination is coherent. Rejects, with the error code
    /// shown: `Static` + `Isr` (`static_render_cannot_use_runtime_regeneration`),
    /// `Static` + streaming (`static_render_cannot_stream`), `ClientOnly` +
    /// `Isr` or `Swr` (`client_only_render_cannot_use_server_cache_revalidation`),
    /// and `ClientOnly` + streaming (`client_only_render_cannot_stream`).
    /// Everything else is accepted.
    pub fn validates(&self) -> Result<(), &'static str> {
        match (&self.render_mode, &self.cache_mode, self.streaming) {
            (RenderMode::Static, CacheMode::Isr { .. }, _) => {
                Err("static_render_cannot_use_runtime_regeneration")
            }
            (RenderMode::Static, _, true) => Err("static_render_cannot_stream"),
            (RenderMode::ClientOnly, CacheMode::Isr { .. } | CacheMode::Swr { .. }, _) => {
                Err("client_only_render_cannot_use_server_cache_revalidation")
            }
            (RenderMode::ClientOnly, _, true) => Err("client_only_render_cannot_stream"),
            _ => Ok(()),
        }
    }

    /// Whether the cache mode is [`CacheMode::Isr`].
    pub fn is_isr(&self) -> bool {
        matches!(self.cache_mode, CacheMode::Isr { .. })
    }

    /// Whether the cache mode is [`CacheMode::Swr`].
    pub fn is_swr(&self) -> bool {
        matches!(self.cache_mode, CacheMode::Swr { .. })
    }

    /// Whether the cache mode is meant to be backed by a store shared
    /// between replicas: true for [`CacheMode::Swr`] and [`CacheMode::Static`].
    pub fn uses_distributed_cache(&self) -> bool {
        matches!(self.cache_mode, CacheMode::Swr { .. } | CacheMode::Static)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_isr_policy_is_valid() {
        let policy = RouteRenderPolicy::new(
            "/blog/:slug",
            RenderMode::Server,
            CacheMode::Isr {
                revalidate_after: Duration::from_secs(30),
            },
        )
        .with_edge_capability(EdgeCapability::Eligible);

        assert!(policy.validates().is_ok());
        assert!(policy.is_isr());
        assert!(!policy.is_swr());
    }

    #[test]
    fn static_render_rejects_runtime_regeneration() {
        let policy = RouteRenderPolicy::new(
            "/docs",
            RenderMode::Static,
            CacheMode::Isr {
                revalidate_after: Duration::from_secs(60),
            },
        );

        assert_eq!(
            policy.validates(),
            Err("static_render_cannot_use_runtime_regeneration")
        );
    }

    #[test]
    fn client_only_render_rejects_streaming() {
        let policy = RouteRenderPolicy::new("/", RenderMode::ClientOnly, CacheMode::None)
            .with_streaming(true);

        assert_eq!(policy.validates(), Err("client_only_render_cannot_stream"));
    }

    #[test]
    fn static_render_allows_swr_without_streaming() {
        let policy = RouteRenderPolicy::new(
            "/robots.txt",
            RenderMode::Static,
            CacheMode::Swr {
                stale_after: Duration::from_secs(60),
            },
        )
        .with_edge_capability(EdgeCapability::Eligible);

        assert!(policy.validates().is_ok());
        assert!(policy.uses_distributed_cache());
    }
}
