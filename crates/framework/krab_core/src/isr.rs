//! # Incremental Static Regeneration (ISR)
//!
//! On-demand static page refresh without full rebuild.
//!
//! ## Usage
//!
//! ```rust
//! use krab_core::isr::{IsrCache, IsrEntry, IsrPolicy};
//! use std::time::Duration;
//!
//! let cache = IsrCache::new();
//! cache.put("/blog/hello", "<h1>Hello</h1>", IsrPolicy::revalidate(Duration::from_secs(60)));
//!
//! if let Some(entry) = cache.get("/blog/hello") {
//!     if entry.is_stale() {
//!         // Trigger background revalidation
//!     }
//!     // Serve cached HTML immediately
//! }
//! ```

use std::collections::HashMap;
use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::{Duration, Instant};
use tracing::warn;

// ── ISR Policy ──────────────────────────────────────────────────────────────

/// Controls how an ISR page is cached and revalidated.
#[derive(Debug, Clone)]
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
#[derive(Debug, Clone)]
pub struct IsrEntry {
    /// The cached HTML content.
    pub html: String,
    /// When this entry was generated.
    pub generated_at: Instant,
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
            generated_at: Instant::now(),
            policy,
            etag,
        }
    }

    /// Returns true if this entry is stale per its policy.
    pub fn is_stale(&self) -> bool {
        match &self.policy {
            IsrPolicy::Static => false,
            IsrPolicy::Revalidate { max_age } => self.generated_at.elapsed() > *max_age,
            IsrPolicy::OnDemand => true,
        }
    }

    /// Returns true if this entry is still fresh.
    pub fn is_fresh(&self) -> bool {
        !self.is_stale()
    }

    /// Age of this entry.
    pub fn age(&self) -> Duration {
        self.generated_at.elapsed()
    }
}

// ── ISR Cache ───────────────────────────────────────────────────────────────

/// Thread-safe ISR page cache.
#[derive(Debug, Clone)]
pub struct IsrCache {
    entries: Arc<RwLock<HashMap<String, IsrEntry>>>,
}

impl Default for IsrCache {
    fn default() -> Self {
        Self::new()
    }
}

impl IsrCache {
    /// Create a new empty ISR cache.
    pub fn new() -> Self {
        Self {
            entries: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Insert or update a cached page.
    pub fn put(&self, path: &str, html: impl Into<String>, policy: IsrPolicy) {
        let entry = IsrEntry::new(html, policy);
        self.write_entries().insert(path.to_string(), entry);
    }

    /// Get a cached page entry (may be stale).
    pub fn get(&self, path: &str) -> Option<IsrEntry> {
        self.read_entries().get(path).cloned()
    }

    /// Invalidate a specific path.
    pub fn invalidate(&self, path: &str) -> bool {
        self.write_entries().remove(path).is_some()
    }

    /// Invalidate all paths matching a prefix.
    pub fn invalidate_prefix(&self, prefix: &str) -> usize {
        let mut cache = self.write_entries();
        let keys: Vec<String> = cache
            .keys()
            .filter(|k| k.starts_with(prefix))
            .cloned()
            .collect();
        let count = keys.len();
        for key in keys {
            cache.remove(&key);
        }
        count
    }

    /// Invalidate all entries.
    pub fn invalidate_all(&self) -> usize {
        let mut cache = self.write_entries();
        let count = cache.len();
        cache.clear();
        count
    }

    /// Get the number of cached entries.
    pub fn len(&self) -> usize {
        self.read_entries().len()
    }

    /// Returns true if the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Get all stale paths that need revalidation.
    pub fn stale_paths(&self) -> Vec<String> {
        self.read_entries()
            .iter()
            .filter(|(_, entry)| entry.is_stale())
            .map(|(path, _)| path.clone())
            .collect()
    }

    /// Serve a page with stale-while-revalidate semantics.
    ///
    /// Returns `(html, needs_revalidation)`.
    /// If the page is cached (even stale), it returns the cached HTML immediately.
    /// The boolean indicates whether background revalidation should be triggered.
    pub fn serve(&self, path: &str) -> Option<(String, bool)> {
        self.get(path)
            .map(|entry| (entry.html.clone(), entry.is_stale()))
    }

    fn write_entries(&self) -> RwLockWriteGuard<'_, HashMap<String, IsrEntry>> {
        match self.entries.write() {
            Ok(guard) => guard,
            Err(poisoned) => {
                warn!("isr_cache_write_lock_poisoned_recovering");
                poisoned.into_inner()
            }
        }
    }

    fn read_entries(&self) -> RwLockReadGuard<'_, HashMap<String, IsrEntry>> {
        match self.entries.read() {
            Ok(guard) => guard,
            Err(poisoned) => {
                warn!("isr_cache_read_lock_poisoned_recovering");
                poisoned.into_inner()
            }
        }
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
    fn cache_put_get_invalidate() {
        let cache = IsrCache::new();
        assert!(cache.is_empty());

        cache.put("/", "<h1>Home</h1>", IsrPolicy::Static);
        cache.put("/about", "<h1>About</h1>", IsrPolicy::Static);
        assert_eq!(cache.len(), 2);

        let entry = cache.get("/").unwrap();
        assert_eq!(entry.html, "<h1>Home</h1>");

        assert!(cache.invalidate("/"));
        assert_eq!(cache.len(), 1);
        assert!(cache.get("/").is_none());
    }

    #[test]
    fn cache_invalidate_prefix() {
        let cache = IsrCache::new();
        cache.put("/blog/a", "A", IsrPolicy::Static);
        cache.put("/blog/b", "B", IsrPolicy::Static);
        cache.put("/about", "About", IsrPolicy::Static);

        let removed = cache.invalidate_prefix("/blog");
        assert_eq!(removed, 2);
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn serve_stale_while_revalidate() {
        let cache = IsrCache::new();
        cache.put(
            "/page",
            "<h1>Page</h1>",
            IsrPolicy::revalidate(Duration::from_millis(1)),
        );

        // Immediately fresh
        let (html, _needs_reval) = cache.serve("/page").unwrap();
        assert_eq!(html, "<h1>Page</h1>");
        // May or may not be stale yet—depends on timing

        std::thread::sleep(Duration::from_millis(5));
        let (html2, needs_reval2) = cache.serve("/page").unwrap();
        assert_eq!(html2, "<h1>Page</h1>"); // Still served
        assert!(needs_reval2); // But needs revalidation
    }

    #[test]
    fn etag_computed() {
        let entry = IsrEntry::new("<h1>Hello</h1>", IsrPolicy::Static);
        assert!(entry.etag.starts_with("\"krab-"));
        assert!(entry.etag.ends_with("\""));
    }

    #[test]
    fn stale_paths_collection() {
        let cache = IsrCache::new();
        cache.put("/static", "S", IsrPolicy::Static);
        cache.put("/dynamic", "D", IsrPolicy::OnDemand);

        let stale = cache.stale_paths();
        assert!(stale.contains(&"/dynamic".to_string()));
        assert!(!stale.contains(&"/static".to_string()));
    }
}
