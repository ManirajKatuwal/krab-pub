//! # Incremental Static Regeneration (ISR)
//!
//! On-demand static page refresh without full rebuild.
//!
//! ## Usage
//!
//! ```rust
//! # tokio_test::block_on(async {
//! use krab_core::isr::{IsrCache, IsrEntry, IsrPolicy};
//! use std::time::Duration;
//!
//! let cache = IsrCache::new();
//! cache
//!     .put("/blog/hello", "<h1>Hello</h1>", IsrPolicy::revalidate(Duration::from_secs(60)))
//!     .await
//!     .unwrap();
//!
//! if let Some(entry) = cache.get("/blog/hello").await.unwrap() {
//!     if entry.is_stale() {
//!         // Trigger background revalidation
//!     }
//!     // Serve cached HTML immediately
//! }
//! # });
//! ```
//!
//! ## Replicas
//!
//! [`IsrCache::new`] is backed by [`MemoryStore`](crate::store::MemoryStore) —
//! per-process, and therefore **only correct for a single replica**. Under more
//! than one, each process keeps its own copy and
//! [`invalidate_prefix`](IsrCache::invalidate_prefix) clears exactly one of
//! them, so a client refreshing a page sees old and new content at random
//! depending on which pod answers.
//!
//! Pass a shared store to fix that:
//!
//! ```rust,no_run
//! # #[cfg(feature = "redis-store")]
//! # tokio_test::block_on(async {
//! use krab_core::isr::IsrCache;
//! use krab_core::store::RedisStore;
//! use std::sync::Arc;
//!
//! let store = RedisStore::from_url("redis://127.0.0.1:6379").unwrap();
//! let cache = IsrCache::with_store(Arc::new(store));
//! # });
//! ```

use crate::store::{DistributedStore, MemoryStore};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

// ── ISR Policy ──────────────────────────────────────────────────────────────

/// Controls how an ISR page is cached and revalidated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum IsrPolicy {
    /// Page is static forever until explicitly invalidated.
    Static,
    /// Page is revalidated after the given duration (stale-while-revalidate).
    Revalidate { max_age: Duration },
    /// Page is revalidated on every request (effectively SSR with caching).
    OnDemand,
}

impl IsrPolicy {
    /// Create a revalidation policy with the given max age.
    pub fn revalidate(max_age: Duration) -> Self {
        Self::Revalidate { max_age }
    }
}

// ── ISR Entry ───────────────────────────────────────────────────────────────

/// A single cached ISR page.
///
/// `generated_at` is a [`SystemTime`], not an `Instant`. An `Instant` is only
/// meaningful inside the process that produced it, so it cannot survive a round
/// trip through a shared store — which is what made a per-process `HashMap` the
/// only option before.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IsrEntry {
    /// The cached HTML content.
    pub html: String,
    /// Wall-clock time this entry was generated.
    pub generated_at: SystemTime,
    /// The caching policy for this entry.
    pub policy: IsrPolicy,
    /// ETag for conditional requests.
    pub etag: String,
}

impl IsrEntry {
    /// Create a new ISR entry.
    pub fn new(html: impl Into<String>, policy: IsrPolicy) -> Self {
        let html = html.into();
        let etag = compute_etag(&html);
        Self {
            html,
            generated_at: SystemTime::now(),
            policy,
            etag,
        }
    }

    /// Returns true if this entry is stale per its policy.
    pub fn is_stale(&self) -> bool {
        match &self.policy {
            IsrPolicy::Static => false,
            IsrPolicy::Revalidate { max_age } => self.age() > *max_age,
            IsrPolicy::OnDemand => true,
        }
    }

    /// Returns true if this entry is still fresh.
    pub fn is_fresh(&self) -> bool {
        !self.is_stale()
    }

    /// Age of this entry.
    ///
    /// Clamps to zero if the entry appears to come from the future, which
    /// happens when replicas disagree about the wall clock. Treating skew as
    /// "brand new" is the safe direction: the page is revalidated later than
    /// ideal rather than being considered infinitely stale and regenerated on
    /// every request across the whole fleet.
    pub fn age(&self) -> Duration {
        self.generated_at.elapsed().unwrap_or(Duration::ZERO)
    }
}

// ── ISR Cache ───────────────────────────────────────────────────────────────

/// Default key namespace. Keeps ISR entries from colliding with other users of
/// the same store (rate limiter counters, sessions).
const DEFAULT_NAMESPACE: &str = "krab:isr";

/// How long a `Revalidate` entry is retained beyond its `max_age`.
///
/// ISR serves stale content while revalidating in the background, so the stored
/// entry has to outlive the point at which it becomes stale — expiring it at
/// exactly `max_age` would turn every revalidation into a cache miss and defeat
/// the pattern. Ten times `max_age`, floored at an hour.
const STALE_RETENTION_FACTOR: u32 = 10;
const MIN_RETENTION: Duration = Duration::from_secs(3600);

/// ISR page cache over a [`DistributedStore`].
///
/// Cloning shares the underlying store.
#[derive(Clone)]
pub struct IsrCache {
    store: Arc<dyn DistributedStore>,
    namespace: String,
}

impl std::fmt::Debug for IsrCache {
    // `dyn DistributedStore` is not `Debug`, and requiring it would force every
    // implementor to derive it.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IsrCache")
            .field("namespace", &self.namespace)
            .finish_non_exhaustive()
    }
}

impl Default for IsrCache {
    fn default() -> Self {
        Self::new()
    }
}

impl IsrCache {
    /// A cache backed by an in-process [`MemoryStore`].
    ///
    /// **Single-replica only.** See the module docs: with more than one process
    /// each keeps its own copy and invalidation reaches only one of them. Use
    /// [`with_store`](Self::with_store) in any deployment that scales out.
    pub fn new() -> Self {
        Self::with_store(Arc::new(MemoryStore::new()))
    }

    /// A cache backed by a store shared across replicas.
    pub fn with_store(store: Arc<dyn DistributedStore>) -> Self {
        Self {
            store,
            namespace: DEFAULT_NAMESPACE.to_string(),
        }
    }

    /// Override the key namespace, for sharing one store between services.
    pub fn with_namespace(mut self, namespace: impl Into<String>) -> Self {
        self.namespace = namespace.into();
        self
    }

    /// Insert or update a cached page.
    pub async fn put(&self, path: &str, html: impl Into<String>, policy: IsrPolicy) -> Result<()> {
        let entry = IsrEntry::new(html, policy);
        let ttl = retention_for(&entry.policy);
        let encoded = serde_json::to_string(&entry).context("failed to encode ISR entry")?;

        self.store
            .set(&self.key(path), &encoded, ttl)
            .await
            .with_context(|| format!("failed to cache ISR entry for {path}"))
    }

    /// Get a cached page entry (may be stale).
    ///
    /// A stored value that cannot be decoded is treated as a miss rather than an
    /// error: it means an older or newer release wrote a different shape, and
    /// regenerating the page is always safe.
    pub async fn get(&self, path: &str) -> Result<Option<IsrEntry>> {
        let Some(raw) = self.store.get(&self.key(path)).await? else {
            return Ok(None);
        };

        match serde_json::from_str::<IsrEntry>(&raw) {
            Ok(entry) => Ok(Some(entry)),
            Err(error) => {
                tracing::warn!(
                    event = "isr_entry_decode_failed",
                    path,
                    %error,
                    "discarding an ISR entry written in an incompatible format"
                );
                Ok(None)
            }
        }
    }

    /// Invalidate a specific path.
    pub async fn invalidate(&self, path: &str) -> Result<bool> {
        self.store.delete(&self.key(path)).await
    }

    /// Invalidate every path matching a prefix. Returns how many were removed.
    pub async fn invalidate_prefix(&self, prefix: &str) -> Result<usize> {
        let keys = self.store.keys_with_prefix(&self.key(prefix)).await?;

        let mut removed = 0;
        for key in keys {
            if self.store.delete(&key).await? {
                removed += 1;
            }
        }
        Ok(removed)
    }

    /// Invalidate every entry in this namespace.
    pub async fn invalidate_all(&self) -> Result<usize> {
        self.invalidate_prefix("").await
    }

    /// Number of cached entries.
    pub async fn len(&self) -> Result<usize> {
        Ok(self.store.keys_with_prefix(&self.key("")).await?.len())
    }

    /// Whether the cache holds nothing.
    pub async fn is_empty(&self) -> Result<bool> {
        Ok(self.len().await? == 0)
    }

    /// Every cached path that is stale and needs revalidation.
    pub async fn stale_paths(&self) -> Result<Vec<String>> {
        let prefix = self.key("");
        let mut stale = Vec::new();

        for key in self.store.keys_with_prefix(&prefix).await? {
            let Some(path) = key.strip_prefix(&prefix) else {
                continue;
            };
            // Re-reads through `get` so a decode failure is handled once.
            if let Some(entry) = self.get(path).await? {
                if entry.is_stale() {
                    stale.push(path.to_string());
                }
            }
        }

        Ok(stale)
    }

    /// Serve a page with stale-while-revalidate semantics.
    ///
    /// Returns `(html, needs_revalidation)`. A cached page is returned even when
    /// stale; the flag says whether background revalidation should be started.
    pub async fn serve(&self, path: &str) -> Result<Option<(String, bool)>> {
        Ok(self
            .get(path)
            .await?
            .map(|entry| (entry.html.clone(), entry.is_stale())))
    }

    fn key(&self, path: &str) -> String {
        format!("{}:{}", self.namespace, path)
    }
}

/// Store TTL for a policy. `Duration::ZERO` means no expiry.
fn retention_for(policy: &IsrPolicy) -> Duration {
    match policy {
        // Static pages live until explicitly invalidated.
        IsrPolicy::Static => Duration::ZERO,
        IsrPolicy::Revalidate { max_age } => max_age
            .saturating_mul(STALE_RETENTION_FACTOR)
            .max(MIN_RETENTION),
        // Always stale, but still worth holding: serving it while regenerating
        // is the whole point of stale-while-revalidate.
        IsrPolicy::OnDemand => MIN_RETENTION,
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────────

fn compute_etag(html: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    html.hash(&mut hasher);
    format!("\"krab-{:016x}\"", hasher.finish())
}

// ── Tests ───────────────────────────────────────────────────────────────────

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_policy_never_stale() {
        let entry = IsrEntry::new("<h1>Home</h1>", IsrPolicy::Static);
        assert!(entry.is_fresh());
        assert!(!entry.is_stale());
    }

    #[test]
    fn on_demand_policy_always_stale() {
        let entry = IsrEntry::new("<h1>Home</h1>", IsrPolicy::OnDemand);
        assert!(entry.is_stale());
        assert!(!entry.is_fresh());
    }

    #[test]
    fn revalidate_policy_staleness() {
        let entry = IsrEntry::new(
            "<h1>Home</h1>",
            IsrPolicy::revalidate(Duration::from_millis(1)),
        );
        std::thread::sleep(Duration::from_millis(5));
        assert!(entry.is_stale());
    }

    #[test]
    fn etag_computed() {
        let entry = IsrEntry::new("<h1>Hello</h1>", IsrPolicy::Static);
        assert!(entry.etag.starts_with("\"krab-"));
        assert!(entry.etag.ends_with("\""));
    }

    /// Clock skew between replicas must not make an entry look infinitely
    /// stale, which would make every pod regenerate on every request.
    #[test]
    fn an_entry_from_the_future_reports_zero_age_rather_than_underflowing() {
        let entry = IsrEntry {
            html: "<h1>Home</h1>".to_string(),
            generated_at: SystemTime::now() + Duration::from_secs(60),
            policy: IsrPolicy::revalidate(Duration::from_secs(1)),
            etag: "\"krab-0\"".to_string(),
        };

        assert_eq!(entry.age(), Duration::ZERO);
        assert!(entry.is_fresh());
    }

    #[tokio::test]
    async fn cache_put_get_invalidate() {
        let cache = IsrCache::new();
        assert!(cache.is_empty().await.unwrap());

        cache
            .put("/", "<h1>Home</h1>", IsrPolicy::Static)
            .await
            .unwrap();
        cache
            .put("/about", "<h1>About</h1>", IsrPolicy::Static)
            .await
            .unwrap();
        assert_eq!(cache.len().await.unwrap(), 2);

        let entry = cache.get("/").await.unwrap().unwrap();
        assert_eq!(entry.html, "<h1>Home</h1>");

        assert!(cache.invalidate("/").await.unwrap());
        assert_eq!(cache.len().await.unwrap(), 1);
        assert!(cache.get("/").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn cache_invalidate_prefix() {
        let cache = IsrCache::new();
        cache.put("/blog/a", "A", IsrPolicy::Static).await.unwrap();
        cache.put("/blog/b", "B", IsrPolicy::Static).await.unwrap();
        cache
            .put("/about", "About", IsrPolicy::Static)
            .await
            .unwrap();

        assert_eq!(cache.invalidate_prefix("/blog").await.unwrap(), 2);
        assert_eq!(cache.len().await.unwrap(), 1);
        assert!(cache.get("/about").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn cache_invalidate_all() {
        let cache = IsrCache::new();
        cache.put("/a", "A", IsrPolicy::Static).await.unwrap();
        cache.put("/b", "B", IsrPolicy::Static).await.unwrap();

        assert_eq!(cache.invalidate_all().await.unwrap(), 2);
        assert!(cache.is_empty().await.unwrap());
    }

    #[tokio::test]
    async fn serve_stale_while_revalidate() {
        let cache = IsrCache::new();
        cache
            .put(
                "/page",
                "<h1>Page</h1>",
                IsrPolicy::revalidate(Duration::from_millis(1)),
            )
            .await
            .unwrap();

        let (html, _needs_reval) = cache.serve("/page").await.unwrap().unwrap();
        assert_eq!(html, "<h1>Page</h1>");

        std::thread::sleep(Duration::from_millis(5));
        let (html2, needs_reval2) = cache.serve("/page").await.unwrap().unwrap();
        assert_eq!(html2, "<h1>Page</h1>", "stale content is still served");
        assert!(needs_reval2, "and flagged for revalidation");
    }

    #[tokio::test]
    async fn stale_paths_collection() {
        let cache = IsrCache::new();
        cache.put("/static", "S", IsrPolicy::Static).await.unwrap();
        cache
            .put("/dynamic", "D", IsrPolicy::OnDemand)
            .await
            .unwrap();

        let stale = cache.stale_paths().await.unwrap();
        assert!(stale.contains(&"/dynamic".to_string()));
        assert!(!stale.contains(&"/static".to_string()));
    }

    /// The whole point of the change: two `IsrCache` handles over one store see
    /// each other's writes and each other's invalidations. With the old
    /// per-process `HashMap` this test could not be written at all.
    #[tokio::test]
    async fn two_caches_sharing_a_store_agree() {
        let store = Arc::new(MemoryStore::new());
        let pod_a = IsrCache::with_store(store.clone());
        let pod_b = IsrCache::with_store(store);

        pod_a.put("/blog/x", "v1", IsrPolicy::Static).await.unwrap();

        // B sees A's write.
        let seen = pod_b.get("/blog/x").await.unwrap().unwrap();
        assert_eq!(seen.html, "v1");

        // B's invalidation reaches A.
        assert_eq!(pod_b.invalidate_prefix("/blog").await.unwrap(), 1);
        assert!(
            pod_a.get("/blog/x").await.unwrap().is_none(),
            "invalidation on one replica must clear the shared entry, not a local copy"
        );
    }

    /// Namespacing is what lets two services share one Redis without colliding.
    #[tokio::test]
    async fn namespaces_isolate_caches_on_a_shared_store() {
        let store = Arc::new(MemoryStore::new());
        let site = IsrCache::with_store(store.clone()).with_namespace("krab:isr:site");
        let docs = IsrCache::with_store(store).with_namespace("krab:isr:docs");

        site.put("/index", "site", IsrPolicy::Static).await.unwrap();
        docs.put("/index", "docs", IsrPolicy::Static).await.unwrap();

        assert_eq!(site.get("/index").await.unwrap().unwrap().html, "site");
        assert_eq!(docs.get("/index").await.unwrap().unwrap().html, "docs");

        site.invalidate_all().await.unwrap();
        assert!(site.is_empty().await.unwrap());
        assert_eq!(
            docs.len().await.unwrap(),
            1,
            "clearing one namespace must not touch another"
        );
    }

    #[tokio::test]
    async fn an_entry_survives_a_round_trip_through_the_store() {
        let cache = IsrCache::new();
        let policy = IsrPolicy::revalidate(Duration::from_secs(30));
        cache
            .put("/round-trip", "<p>hi</p>", policy.clone())
            .await
            .unwrap();

        let entry = cache.get("/round-trip").await.unwrap().unwrap();
        assert_eq!(entry.html, "<p>hi</p>");
        assert_eq!(entry.policy, policy);
        assert!(entry.etag.starts_with("\"krab-"));
        assert!(entry.is_fresh());
    }

    /// A value written by a different release must not take the cache down.
    #[tokio::test]
    async fn an_undecodable_stored_value_is_treated_as_a_miss() {
        let store = Arc::new(MemoryStore::new());
        store
            .set("krab:isr:/broken", "not json", Duration::ZERO)
            .await
            .unwrap();

        let cache = IsrCache::with_store(store);
        assert!(cache.get("/broken").await.unwrap().is_none());
    }

    #[test]
    fn retention_outlives_max_age_so_stale_entries_can_still_be_served() {
        let max_age = Duration::from_secs(600);
        let retention = retention_for(&IsrPolicy::revalidate(max_age));

        assert!(
            retention > max_age,
            "expiring at max_age would make every revalidation a cache miss"
        );
        assert_eq!(retention, max_age * STALE_RETENTION_FACTOR);
    }

    #[test]
    fn static_entries_are_stored_without_expiry() {
        assert_eq!(retention_for(&IsrPolicy::Static), Duration::ZERO);
    }

    #[test]
    fn short_max_age_still_gets_a_usable_retention_floor() {
        let retention = retention_for(&IsrPolicy::revalidate(Duration::from_secs(1)));
        assert_eq!(retention, MIN_RETENTION);
    }
}
