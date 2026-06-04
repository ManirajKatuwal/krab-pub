use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderMode {
    Static,
    Server,
    ClientOnly,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheMode {
    None,
    Static,
    Isr { revalidate_after: Duration },
    Swr { stale_after: Duration },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeCapability {
    OriginOnly,
    Eligible,
    Preferred,
    Required,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteRenderPolicy {
    pub route_pattern: String,
    pub render_mode: RenderMode,
    pub cache_mode: CacheMode,
    pub edge_capability: EdgeCapability,
    pub streaming: bool,
}

impl RouteRenderPolicy {
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

    pub fn with_edge_capability(mut self, edge_capability: EdgeCapability) -> Self {
        self.edge_capability = edge_capability;
        self
    }

    pub fn with_streaming(mut self, streaming: bool) -> Self {
        self.streaming = streaming;
        self
    }

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

    pub fn is_isr(&self) -> bool {
        matches!(self.cache_mode, CacheMode::Isr { .. })
    }

    pub fn is_swr(&self) -> bool {
        matches!(self.cache_mode, CacheMode::Swr { .. })
    }

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
