//! A small key-value store abstraction for state that must be shared
//! between a service's replicas: rate-limit and auth-failure counters, token
//! revocations, ISR cache entries and render leases.
//!
//! [`MemoryStore`] is in-process — correct for one replica, per-replica
//! otherwise. `RedisStore` (feature `redis-store`) is the shared
//! implementation; the HTTP runtime state picks it when `KRAB_REDIS_URL` is
//! set.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[cfg(feature = "redis-store")]
use anyhow::Context;
use anyhow::Result;
use async_trait::async_trait;
use tokio::sync::RwLock;

/// A key-value store shared by every replica of a service.
///
/// `Duration::ZERO` as a `ttl` means **no expiry**, in every implementation.
#[async_trait]
pub trait DistributedStore: Send + Sync {
    /// The value at `key`, or `None` if it is absent or expired.
    async fn get(&self, key: &str) -> Result<Option<String>>;
    /// Stores `value` at `key`, replacing any existing value and expiry.
    async fn set(&self, key: &str, value: &str, ttl: Duration) -> Result<()>;
    /// Adds `delta` to the integer at `key` (an absent key counts as 0) and
    /// returns the new value. An existing expiry is kept; a new key gets
    /// none. Errors if the stored value is not an integer.
    async fn incr(&self, key: &str, delta: u64) -> Result<u64>;
    /// Sets the expiry of an existing key to `ttl` from now, or removes its
    /// expiry for `Duration::ZERO`. A missing key is not an error.
    async fn expire(&self, key: &str, ttl: Duration) -> Result<()>;

    /// Remove a key. `Ok(true)` if it existed.
    ///
    /// Added for [`IsrCache`](crate::isr::IsrCache), which cannot express
    /// invalidation without it — the reason ISR previously kept its own
    /// process-local `HashMap` instead of using this trait.
    async fn delete(&self, key: &str) -> Result<bool>;

    /// Every live key starting with `prefix`.
    ///
    /// Required for prefix invalidation. Implementations must not block the
    /// server while scanning: the Redis implementation uses `SCAN`, never
    /// `KEYS`, which is O(n) over the whole keyspace and single-threaded.
    async fn keys_with_prefix(&self, prefix: &str) -> Result<Vec<String>>;

    /// Write `value` only if no live entry exists for `key`. `Ok(true)` if
    /// this call created the entry, `Ok(false)` if a live entry already held
    /// the key.
    ///
    /// The check-and-write is atomic within one store (Redis `SET NX PX`; a
    /// single write-lock section for the in-memory store), which is what makes
    /// it usable as a distributed lease: exactly one replica of a fleet wins a
    /// cold-cache render and the rest observe `false`.
    ///
    /// Note for external implementors: this method was added in 0.2.0 as a
    /// required method — implementations of this trait outside the workspace
    /// must add it.
    async fn set_if_absent(&self, key: &str, value: &str, ttl: Duration) -> Result<bool>;

    /// Increment `key` by `delta`, giving it `ttl` when this call creates it.
    /// Returns the new value.
    ///
    /// For fixed-window counters (rate limiting). Implementations should make
    /// the increment and the expiry one atomic step: done as two calls, a
    /// crash — or a store error on the second — between them leaves a counter
    /// with no expiry, which then limits that client forever. The default
    /// implementation is that two-call sequence, kept so external
    /// implementations of this trait keep compiling; both built-in stores
    /// override it. Added in 0.6.0.
    async fn incr_with_ttl(&self, key: &str, delta: u64, ttl: Duration) -> Result<u64> {
        let value = self.incr(key, delta).await?;
        if value == delta {
            self.expire(key, ttl).await?;
        }
        Ok(value)
    }
}

/// The in-process [`DistributedStore`]: a map behind an async lock.
///
/// Not shared between processes, so with more than one replica each holds
/// its own counters and cache. Clones share the same map. Capped at
/// `KRAB_MEMORY_STORE_MAX_ENTRIES` entries (default 100 000, `0` for no cap)
/// when built with [`MemoryStore::new`]; expired entries are swept
/// periodically.
#[derive(Clone)]
pub struct MemoryStore {
    inner: Arc<RwLock<MemoryInner>>,
    writes_since_sweep: Arc<std::sync::atomic::AtomicUsize>,
    evictions: Arc<std::sync::atomic::AtomicU64>,
    max_entries: usize,
}

#[derive(Default)]
struct MemoryInner {
    entries: HashMap<String, MemoryEntry>,
    /// Coarse insertion order used for capacity eviction. Keys deleted or
    /// swept from `entries` may linger here as ghosts; they are skipped on
    /// eviction and compacted during the periodic sweep, so the queue stays
    /// proportional to the live entry count plus one sweep window of churn.
    order: VecDeque<String>,
}

#[derive(Clone)]
struct MemoryEntry {
    value: String,
    expires_at: Option<Instant>,
}

impl MemoryEntry {
    fn is_expired(&self, now: Instant) -> bool {
        self.expires_at.map(|ts| now >= ts).unwrap_or(false)
    }
}

/// Writes between full sweeps of expired entries. Amortizes the O(n) scan so
/// steady-state writes stay O(1).
const SWEEP_EVERY_N_WRITES: usize = 256;

/// Capacity default when `KRAB_MEMORY_STORE_MAX_ENTRIES` is unset or invalid.
const DEFAULT_MAX_ENTRIES: usize = 100_000;

/// Emit the capacity-eviction warning on the first eviction and then once per
/// this many, instead of per entry — at capacity every insert evicts, and a
/// per-entry warn would be log spam exactly when the process is under load.
const EVICTION_WARN_EVERY: u64 = 1000;

/// Capacity cap for stores built via `new`/`default`, read from
/// `KRAB_MEMORY_STORE_MAX_ENTRIES` exactly once per process. `0` disables the
/// cap.
fn default_max_entries() -> usize {
    static CAP: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *CAP.get_or_init(|| {
        std::env::var("KRAB_MEMORY_STORE_MAX_ENTRIES")
            .ok()
            .and_then(|raw| raw.trim().parse::<usize>().ok())
            .unwrap_or(DEFAULT_MAX_ENTRIES)
    })
}

impl Default for MemoryStore {
    fn default() -> Self {
        Self::with_max_entries(default_max_entries())
    }
}

impl MemoryStore {
    /// An empty store capped at `KRAB_MEMORY_STORE_MAX_ENTRIES` (read once
    /// per process; default 100 000, `0` for no cap). Same as `default()`.
    pub fn new() -> Self {
        Self::default()
    }

    /// A store holding at most `max_entries` live entries (`0` = unlimited).
    ///
    /// On an insert at capacity, expired entries at the front of the insertion
    /// order are reclaimed first; if none are found there, the
    /// oldest-inserted live entry is evicted. Without a cap, `Static` ISR
    /// entries (no TTL) accumulate for the life of the process.
    pub fn with_max_entries(max_entries: usize) -> Self {
        Self {
            inner: Arc::new(RwLock::new(MemoryInner::default())),
            writes_since_sweep: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            evictions: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            max_entries,
        }
    }

    /// Reap every expired entry once per [`SWEEP_EVERY_N_WRITES`] writes.
    ///
    /// Point-`get` only reaps the key it touches, and epoch-suffixed keys
    /// (`rate:ip:<ip>:<epoch>`) are never touched again once their window
    /// passes — without this sweep they accumulate for the life of the
    /// process, one per client IP per window.
    fn sweep_if_due(&self, inner: &mut MemoryInner) {
        use std::sync::atomic::Ordering;

        if self.writes_since_sweep.fetch_add(1, Ordering::Relaxed) + 1 < SWEEP_EVERY_N_WRITES {
            return;
        }
        self.writes_since_sweep.store(0, Ordering::Relaxed);
        let now = Instant::now();
        let MemoryInner { entries, order } = inner;
        entries.retain(|_, entry| !entry.is_expired(now));

        // Compact ghosts (keys removed from `entries`) and duplicates out of
        // the insertion-order queue so it cannot outgrow the live entry set.
        let mut seen = std::collections::HashSet::with_capacity(entries.len());
        order.retain(|key| entries.contains_key(key) && seen.insert(key.clone()));
    }

    /// Insert honoring the capacity cap. Must be called with the write lock
    /// held (`inner` is the locked state).
    fn insert_entry(&self, inner: &mut MemoryInner, key: &str, entry: MemoryEntry) {
        if !inner.entries.contains_key(key) {
            self.evict_for_capacity(inner);
            inner.order.push_back(key.to_string());
        }
        inner.entries.insert(key.to_string(), entry);
    }

    /// Make room for one new entry when at capacity: pop the insertion-order
    /// queue from the front, dropping ghosts, reclaiming expired entries
    /// first, and finally evicting the oldest-inserted live entry. Each queue
    /// slot is popped at most once, so the cost is O(1) amortized. Expired
    /// entries deeper in the queue are reclaimed by the periodic sweep.
    fn evict_for_capacity(&self, inner: &mut MemoryInner) {
        if self.max_entries == 0 {
            return;
        }
        let now = Instant::now();
        while inner.entries.len() >= self.max_entries {
            let Some(candidate) = inner.order.pop_front() else {
                // Every live entry has a queue slot, so an empty queue means
                // the map is empty too; nothing left to evict.
                return;
            };
            let Some(entry) = inner.entries.get(&candidate) else {
                continue; // ghost of a deleted/swept key
            };
            let reason = if entry.is_expired(now) {
                "expired"
            } else {
                "oldest"
            };
            inner.entries.remove(&candidate);
            self.record_eviction(reason);
        }
    }

    fn record_eviction(&self, reason: &'static str) {
        use std::sync::atomic::Ordering;

        let total = self.evictions.fetch_add(1, Ordering::Relaxed) + 1;
        if total == 1 || total.is_multiple_of(EVICTION_WARN_EVERY) {
            tracing::warn!(
                event = "memory_store_evicted",
                evicted_total = total,
                max_entries = self.max_entries,
                reason,
                "memory store at capacity; evicting entries"
            );
        }
    }
}

#[async_trait]
impl DistributedStore for MemoryStore {
    async fn get(&self, key: &str) -> Result<Option<String>> {
        {
            let guard = self.inner.read().await;
            if let Some(entry) = guard.entries.get(key) {
                if entry.is_expired(Instant::now()) {
                    drop(guard);
                    let mut guard = self.inner.write().await;
                    guard.entries.remove(key);
                    return Ok(None);
                }
                return Ok(Some(entry.value.clone()));
            }
        }
        Ok(None)
    }

    async fn set(&self, key: &str, value: &str, ttl: Duration) -> Result<()> {
        let expires_at = if ttl.is_zero() {
            None
        } else {
            Some(Instant::now() + ttl)
        };

        let mut guard = self.inner.write().await;
        self.insert_entry(
            &mut guard,
            key,
            MemoryEntry {
                value: value.to_string(),
                expires_at,
            },
        );
        self.sweep_if_due(&mut guard);
        Ok(())
    }

    async fn incr(&self, key: &str, delta: u64) -> Result<u64> {
        let mut guard = self.inner.write().await;
        let now = Instant::now();

        let current = match guard.entries.get(key) {
            Some(entry) if entry.is_expired(now) => {
                guard.entries.remove(key);
                0
            }
            Some(entry) => match entry.value.parse::<u64>() {
                Ok(value) => value,
                Err(_) => {
                    // A non-numeric value is a key collision with a
                    // non-counter entry. Redis answers `INCR` on such a key
                    // with an error and leaves it alone; this store used to
                    // overwrite it with `delta`, corrupting whichever caller
                    // owned the key. Matching Redis keeps the two stores
                    // interchangeable, and callers already treat a store
                    // error per their fail-open/closed policy.
                    tracing::warn!(key, "memory_store_incr_non_numeric_value");
                    anyhow::bail!("value at `{key}` is not an integer");
                }
            },
            None => 0,
        };

        let next = current.saturating_add(delta);
        let ttl = guard.entries.get(key).and_then(|entry| entry.expires_at);
        self.insert_entry(
            &mut guard,
            key,
            MemoryEntry {
                value: next.to_string(),
                expires_at: ttl,
            },
        );
        self.sweep_if_due(&mut guard);
        Ok(next)
    }

    async fn incr_with_ttl(&self, key: &str, delta: u64, ttl: Duration) -> Result<u64> {
        // One write-lock section: the increment and the expiry cannot be
        // separated by another task, and a new counter never exists without
        // its expiry.
        let mut guard = self.inner.write().await;
        let now = Instant::now();

        let existing = match guard.entries.get(key) {
            Some(entry) if !entry.is_expired(now) => Some(entry),
            _ => None,
        };
        let (current, expires_at) = match existing {
            Some(entry) => match entry.value.parse::<u64>() {
                Ok(value) => (value, entry.expires_at),
                Err(_) => {
                    tracing::warn!(key, "memory_store_incr_non_numeric_value");
                    anyhow::bail!("value at `{key}` is not an integer");
                }
            },
            None => (0, (!ttl.is_zero()).then(|| now + ttl)),
        };

        let next = current.saturating_add(delta);
        self.insert_entry(
            &mut guard,
            key,
            MemoryEntry {
                value: next.to_string(),
                expires_at,
            },
        );
        self.sweep_if_due(&mut guard);
        Ok(next)
    }

    async fn expire(&self, key: &str, ttl: Duration) -> Result<()> {
        let mut guard = self.inner.write().await;
        if let Some(entry) = guard.entries.get_mut(key) {
            entry.expires_at = if ttl.is_zero() {
                None
            } else {
                Some(Instant::now() + ttl)
            };
        }
        Ok(())
    }

    async fn delete(&self, key: &str) -> Result<bool> {
        let mut guard = self.inner.write().await;
        Ok(guard.entries.remove(key).is_some())
    }

    async fn keys_with_prefix(&self, prefix: &str) -> Result<Vec<String>> {
        let now = Instant::now();
        let guard = self.inner.read().await;

        // Expired-but-not-yet-reaped entries are skipped: `get` treats them as
        // absent, so listing them would report keys that cannot be read.
        Ok(guard
            .entries
            .iter()
            .filter(|(key, entry)| key.starts_with(prefix) && !entry.is_expired(now))
            .map(|(key, _)| key.clone())
            .collect())
    }

    async fn set_if_absent(&self, key: &str, value: &str, ttl: Duration) -> Result<bool> {
        let mut guard = self.inner.write().await;
        let now = Instant::now();

        // Atomic under the write lock: the liveness check and the insert
        // cannot interleave with another writer.
        if let Some(existing) = guard.entries.get(key) {
            if !existing.is_expired(now) {
                return Ok(false);
            }
        }

        let expires_at = if ttl.is_zero() { None } else { Some(now + ttl) };
        self.insert_entry(
            &mut guard,
            key,
            MemoryEntry {
                value: value.to_string(),
                expires_at,
            },
        );
        self.sweep_if_due(&mut guard);
        Ok(true)
    }
}

/// The shared [`DistributedStore`], backed by Redis (feature `redis-store`).
///
/// Expiries are applied in whole seconds, rounded down with a minimum of one
/// second. Each operation takes a fresh multiplexed connection from the
/// client; a connection failure is an `Err`, which callers handle per their
/// fail-open or fail-closed policy.
#[cfg(feature = "redis-store")]
#[derive(Clone)]
pub struct RedisStore {
    client: redis::Client,
}

#[cfg(feature = "redis-store")]
impl RedisStore {
    /// A store using an existing Redis client.
    pub fn new(client: redis::Client) -> Self {
        Self { client }
    }

    /// A store for the Redis server at `url` (for example
    /// `redis://127.0.0.1:6379`). Only parses the URL — errors for a malformed
    /// one — and does not connect; an unreachable server surfaces on the
    /// first operation.
    pub fn from_url(url: &str) -> Result<Self> {
        let client = redis::Client::open(url).context("invalid redis url")?;
        Ok(Self::new(client))
    }

    async fn conn(&self) -> Result<redis::aio::MultiplexedConnection> {
        self.client
            .get_multiplexed_tokio_connection()
            .await
            .context("failed to connect to redis")
    }
}

#[cfg(feature = "redis-store")]
#[async_trait]
impl DistributedStore for RedisStore {
    async fn get(&self, key: &str) -> Result<Option<String>> {
        use redis::AsyncCommands;

        let mut conn = self.conn().await?;
        conn.get(key).await.context("redis GET failed")
    }

    async fn set(&self, key: &str, value: &str, ttl: Duration) -> Result<()> {
        use redis::AsyncCommands;

        let mut conn = self.conn().await?;

        // `Duration::ZERO` means no expiry. This previously went through
        // `set_ex` with `.max(1)`, so a caller asking for a permanent entry got
        // one that vanished after a second — which is what an ISR `Static`
        // policy would have asked for.
        if ttl.is_zero() {
            return conn.set(key, value).await.context("redis SET failed");
        }

        conn.set_ex(key, value, ttl.as_secs().max(1))
            .await
            .context("redis SETEX failed")
    }

    async fn incr(&self, key: &str, delta: u64) -> Result<u64> {
        use redis::AsyncCommands;

        let mut conn = self.conn().await?;
        let value: u64 = conn.incr(key, delta).await.context("redis INCR failed")?;
        Ok(value)
    }

    async fn incr_with_ttl(&self, key: &str, delta: u64, ttl: Duration) -> Result<u64> {
        // A script runs atomically on the server, so no other client — and no
        // crash of this one — can observe the counter without its expiry.
        const SCRIPT: &str = "local v = redis.call('INCRBY', KEYS[1], ARGV[1]) \
             if v == tonumber(ARGV[1]) and tonumber(ARGV[2]) > 0 then \
               redis.call('EXPIRE', KEYS[1], ARGV[2]) \
             end \
             return v";

        let mut conn = self.conn().await?;
        let value: u64 = redis::Script::new(SCRIPT)
            .key(key)
            .arg(delta)
            .arg(if ttl.is_zero() {
                0
            } else {
                ttl.as_secs().max(1)
            })
            .invoke_async(&mut conn)
            .await
            .context("redis INCRBY+EXPIRE script failed")?;
        Ok(value)
    }

    async fn expire(&self, key: &str, ttl: Duration) -> Result<()> {
        use redis::AsyncCommands;

        let mut conn = self.conn().await?;

        // Consistent with `set`: zero means "no expiry", which in Redis is
        // PERSIST rather than an EXPIRE of 0 (that would delete the key).
        if ttl.is_zero() {
            let _: bool = conn.persist(key).await.context("redis PERSIST failed")?;
            return Ok(());
        }

        let _: bool = conn
            .expire(key, ttl.as_secs().max(1) as i64)
            .await
            .context("redis EXPIRE failed")?;
        Ok(())
    }

    async fn delete(&self, key: &str) -> Result<bool> {
        use redis::AsyncCommands;

        let mut conn = self.conn().await?;
        let removed: u64 = conn.del(key).await.context("redis DEL failed")?;
        Ok(removed > 0)
    }

    async fn keys_with_prefix(&self, prefix: &str) -> Result<Vec<String>> {
        let mut conn = self.conn().await?;

        // SCAN, not KEYS. `KEYS prefix*` is O(keyspace) and blocks the single
        // Redis thread for its whole duration, so on a large keyspace it stalls
        // every other client — including the ones serving requests.
        let pattern = format!("{}*", escape_scan_glob(prefix));
        let mut cursor: u64 = 0;
        let mut keys = Vec::new();

        loop {
            let (next, batch): (u64, Vec<String>) = redis::cmd("SCAN")
                .arg(cursor)
                .arg("MATCH")
                .arg(&pattern)
                .arg("COUNT")
                .arg(SCAN_BATCH)
                .query_async(&mut conn)
                .await
                .context("redis SCAN failed")?;

            keys.extend(batch);
            cursor = next;
            if cursor == 0 {
                break;
            }
        }

        // SCAN can return the same key more than once across iterations.
        keys.sort_unstable();
        keys.dedup();
        Ok(keys)
    }

    async fn set_if_absent(&self, key: &str, value: &str, ttl: Duration) -> Result<bool> {
        let mut conn = self.conn().await?;

        // SET NX PX in one command — the atomicity is the point; a separate
        // EXISTS check would reintroduce the race this method exists to close.
        let mut cmd = redis::cmd("SET");
        cmd.arg(key).arg(value).arg("NX");
        if !ttl.is_zero() {
            cmd.arg("PX").arg(ttl.as_millis().max(1) as u64);
        }

        // Redis replies `OK` when the write happened and nil when NX blocked it.
        let reply: Option<String> = cmd
            .query_async(&mut conn)
            .await
            .context("redis SET NX failed")?;
        Ok(reply.is_some())
    }
}

/// Keys per `SCAN` round trip. A hint, not a limit.
#[cfg(feature = "redis-store")]
const SCAN_BATCH: usize = 512;

/// Escape the glob metacharacters `SCAN MATCH` would otherwise interpret.
///
/// An ISR path is user-facing and can legitimately contain `*`, `?`, `[`, or
/// `]`. Without escaping, invalidating the prefix `/blog/[draft]` would match
/// paths that merely share one of those characters — silently over-invalidating.
#[cfg(feature = "redis-store")]
fn escape_scan_glob(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        if matches!(ch, '*' | '?' | '[' | ']' | '\\') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod memory_store_tests {
    use super::*;

    /// The counter and its expiry are created together, a later increment does
    /// not extend the window, and the counter restarts once it expires.
    #[tokio::test]
    async fn incr_with_ttl_sets_expiry_only_on_creation() {
        let store = MemoryStore::new();
        let ttl = Duration::from_millis(80);

        assert_eq!(store.incr_with_ttl("w", 1, ttl).await.unwrap(), 1);
        let first_expiry = store.inner.read().await.entries["w"].expires_at;
        assert!(first_expiry.is_some(), "a new counter always has an expiry");

        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(store.incr_with_ttl("w", 1, ttl).await.unwrap(), 2);
        assert_eq!(
            store.inner.read().await.entries["w"].expires_at,
            first_expiry,
            "a fixed window does not slide"
        );

        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(store.incr_with_ttl("w", 1, ttl).await.unwrap(), 1);
    }

    /// Like Redis, incrementing a non-numeric value is an error that leaves
    /// the value alone — it used to overwrite it.
    #[tokio::test]
    async fn incr_on_a_non_numeric_value_errors_and_preserves_it() {
        let store = MemoryStore::new();
        store
            .set("k", "not-a-number", Duration::ZERO)
            .await
            .unwrap();

        assert!(store.incr("k", 1).await.is_err());
        assert!(store
            .incr_with_ttl("k", 1, Duration::from_secs(5))
            .await
            .is_err());
        assert_eq!(
            store.get("k").await.unwrap().as_deref(),
            Some("not-a-number")
        );
    }

    /// The trait's provided method still works for a store that only
    /// implements the required ones.
    #[tokio::test]
    async fn default_incr_with_ttl_sets_expiry_on_first_increment() {
        struct Minimal(MemoryStore);
        #[async_trait]
        impl DistributedStore for Minimal {
            async fn get(&self, key: &str) -> Result<Option<String>> {
                self.0.get(key).await
            }
            async fn set(&self, key: &str, value: &str, ttl: Duration) -> Result<()> {
                self.0.set(key, value, ttl).await
            }
            async fn incr(&self, key: &str, delta: u64) -> Result<u64> {
                self.0.incr(key, delta).await
            }
            async fn expire(&self, key: &str, ttl: Duration) -> Result<()> {
                self.0.expire(key, ttl).await
            }
            async fn delete(&self, key: &str) -> Result<bool> {
                self.0.delete(key).await
            }
            async fn keys_with_prefix(&self, prefix: &str) -> Result<Vec<String>> {
                self.0.keys_with_prefix(prefix).await
            }
            async fn set_if_absent(&self, key: &str, value: &str, ttl: Duration) -> Result<bool> {
                self.0.set_if_absent(key, value, ttl).await
            }
        }

        let store = Minimal(MemoryStore::new());
        assert_eq!(
            store
                .incr_with_ttl("d", 1, Duration::from_secs(60))
                .await
                .unwrap(),
            1
        );
        assert!(store.0.inner.read().await.entries["d"].expires_at.is_some());
    }

    /// Epoch-suffixed keys (rate limiting) are written once and never touched
    /// again; the periodic write sweep is the only thing that reclaims them.
    #[tokio::test]
    async fn write_sweep_reaps_expired_untouched_keys() {
        let store = MemoryStore::new();
        for i in 0..SWEEP_EVERY_N_WRITES {
            store
                .set(
                    &format!("rate:ip:10.0.0.1:{i}"),
                    "1",
                    Duration::from_millis(1),
                )
                .await
                .unwrap();
        }
        tokio::time::sleep(Duration::from_millis(20)).await;

        // Enough writes to guarantee a sweep fires after the entries expired.
        for _ in 0..SWEEP_EVERY_N_WRITES {
            store
                .set("rate:ip:10.0.0.2:current", "1", Duration::from_secs(60))
                .await
                .unwrap();
        }

        let remaining = store.inner.read().await.entries.len();
        assert_eq!(
            remaining, 1,
            "expired epoch keys must be swept; {remaining} entries remain"
        );
    }

    #[tokio::test]
    async fn set_if_absent_lets_only_the_first_writer_win() {
        let store = MemoryStore::new();

        assert!(store
            .set_if_absent("lease:/page", "a", Duration::from_secs(10))
            .await
            .unwrap());
        assert!(!store
            .set_if_absent("lease:/page", "b", Duration::from_secs(10))
            .await
            .unwrap());

        // The losing write must not have replaced the value.
        assert_eq!(
            store.get("lease:/page").await.unwrap().as_deref(),
            Some("a")
        );
    }

    #[tokio::test]
    async fn set_if_absent_succeeds_once_the_previous_entry_expired() {
        let store = MemoryStore::new();
        assert!(store
            .set_if_absent("lease:/page", "a", Duration::from_millis(5))
            .await
            .unwrap());
        tokio::time::sleep(Duration::from_millis(20)).await;

        assert!(
            store
                .set_if_absent("lease:/page", "b", Duration::from_secs(10))
                .await
                .unwrap(),
            "an expired lease must be reacquirable"
        );
        assert_eq!(
            store.get("lease:/page").await.unwrap().as_deref(),
            Some("b")
        );
    }

    /// Static ISR entries carry no TTL and were previously never reclaimed —
    /// the cap is what bounds the store's memory.
    #[tokio::test]
    async fn capacity_cap_holds_len_and_keeps_the_newest_entry() {
        let cap = 8;
        let store = MemoryStore::with_max_entries(cap);

        for i in 0..=cap {
            store
                .set(&format!("static:/page-{i}"), "html", Duration::ZERO)
                .await
                .unwrap();
        }

        let len = store.inner.read().await.entries.len();
        assert!(len <= cap, "cap {cap} exceeded: {len} entries");
        assert_eq!(
            store
                .get(&format!("static:/page-{cap}"))
                .await
                .unwrap()
                .as_deref(),
            Some("html"),
            "the newest entry must survive eviction"
        );
        assert!(
            store.get("static:/page-0").await.unwrap().is_none(),
            "the oldest-inserted entry is the one evicted"
        );
    }

    #[tokio::test]
    async fn capacity_eviction_reclaims_expired_entries_before_live_ones() {
        let store = MemoryStore::with_max_entries(2);
        store
            .set("expired-old", "x", Duration::from_millis(1))
            .await
            .unwrap();
        store.set("live-old", "y", Duration::ZERO).await.unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;

        store.set("newcomer", "z", Duration::ZERO).await.unwrap();

        assert!(
            store.get("live-old").await.unwrap().is_some(),
            "a live entry must not be evicted while an expired one is reclaimable"
        );
        assert!(store.get("newcomer").await.unwrap().is_some());
        assert!(store.get("expired-old").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn zero_cap_means_unlimited() {
        let store = MemoryStore::with_max_entries(0);
        for i in 0..64 {
            store
                .set(&format!("k{i}"), "v", Duration::ZERO)
                .await
                .unwrap();
        }
        assert_eq!(store.inner.read().await.entries.len(), 64);
    }

    /// Overwriting an existing key must not evict anything: the live entry
    /// count does not grow.
    #[tokio::test]
    async fn overwriting_at_capacity_does_not_evict() {
        let store = MemoryStore::with_max_entries(2);
        store.set("a", "1", Duration::ZERO).await.unwrap();
        store.set("b", "1", Duration::ZERO).await.unwrap();

        store.set("a", "2", Duration::ZERO).await.unwrap();

        assert_eq!(store.get("a").await.unwrap().as_deref(), Some("2"));
        assert_eq!(store.get("b").await.unwrap().as_deref(), Some("1"));
    }
}

/// `RedisStore` against a real server.
///
/// Needs `KRAB_TEST_REDIS_URL` (e.g. `redis://127.0.0.1:6379/15`). Without a
/// reachable server each test prints that it executed NOTHING and passes —
/// unless `KRAB_REQUIRE_REDIS_TESTS=1`, in which case it fails, so a CI job
/// that is meant to exercise Redis cannot go green by skipping. Every key is
/// prefixed with the process id and test name, so concurrent runs against a
/// shared server do not collide, and each test deletes what it wrote.
#[cfg(all(test, feature = "redis-store"))]
mod redis_store_tests {
    use super::*;

    async fn store_or_skip(test: &str) -> Option<(RedisStore, String)> {
        let skip = |reason: String| {
            if std::env::var("KRAB_REQUIRE_REDIS_TESTS").as_deref() == Ok("1") {
                panic!("{test}: KRAB_REQUIRE_REDIS_TESTS=1 but {reason}");
            }
            eprintln!("SKIPPED {test}: {reason}; this test executed NOTHING");
            None
        };
        let url = match std::env::var("KRAB_TEST_REDIS_URL") {
            Ok(url) if !url.trim().is_empty() => url,
            _ => return skip("KRAB_TEST_REDIS_URL is unset".to_string()),
        };
        let store = match RedisStore::from_url(&url) {
            Ok(store) => store,
            Err(err) => return skip(format!("invalid KRAB_TEST_REDIS_URL: {err:#}")),
        };
        if let Err(err) = store.get("krab-test:probe").await {
            return skip(format!("redis unreachable: {err:#}"));
        }
        Some((store, format!("krab-test:{}:{test}:", std::process::id())))
    }

    async fn ttl_secs(store: &RedisStore, key: &str) -> i64 {
        let mut conn = store.conn().await.expect("connection");
        redis::cmd("TTL")
            .arg(key)
            .query_async::<i64>(&mut conn)
            .await
            .expect("TTL")
    }

    async fn cleanup(store: &RedisStore, prefix: &str) {
        for key in store.keys_with_prefix(prefix).await.unwrap_or_default() {
            let _ = store.delete(&key).await;
        }
    }

    #[tokio::test]
    async fn set_get_delete_and_zero_ttl_is_permanent() {
        let Some((store, p)) = store_or_skip("set_get_delete").await else {
            return;
        };
        let (a, b) = (format!("{p}a"), format!("{p}b"));

        store.set(&a, "one", Duration::from_secs(60)).await.unwrap();
        store.set(&b, "two", Duration::ZERO).await.unwrap();
        assert_eq!(store.get(&a).await.unwrap().as_deref(), Some("one"));
        assert!((1..=60).contains(&ttl_secs(&store, &a).await));
        assert_eq!(ttl_secs(&store, &b).await, -1, "ZERO means no expiry");

        assert!(store.delete(&a).await.unwrap());
        assert!(!store.delete(&a).await.unwrap());
        assert_eq!(store.get(&a).await.unwrap(), None);
        cleanup(&store, &p).await;
    }

    #[tokio::test]
    async fn incr_with_ttl_sets_expiry_once_and_atomically() {
        let Some((store, p)) = store_or_skip("incr_with_ttl").await else {
            return;
        };
        let key = format!("{p}counter");

        assert_eq!(
            store
                .incr_with_ttl(&key, 1, Duration::from_secs(120))
                .await
                .unwrap(),
            1
        );
        let first = ttl_secs(&store, &key).await;
        assert!(
            (1..=120).contains(&first),
            "new counter has an expiry: {first}"
        );

        assert_eq!(
            store
                .incr_with_ttl(&key, 2, Duration::from_secs(9999))
                .await
                .unwrap(),
            3
        );
        assert!(
            ttl_secs(&store, &key).await <= first,
            "a later increment must not extend the window"
        );

        // `incr` on a non-numeric value is an error, as with MemoryStore.
        let text = format!("{p}text");
        store
            .set(&text, "abc", Duration::from_secs(60))
            .await
            .unwrap();
        assert!(store.incr(&text, 1).await.is_err());
        assert!(store
            .incr_with_ttl(&text, 1, Duration::from_secs(60))
            .await
            .is_err());
        assert_eq!(store.get(&text).await.unwrap().as_deref(), Some("abc"));
        cleanup(&store, &p).await;
    }

    #[tokio::test]
    async fn expire_zero_persists_rather_than_deleting() {
        let Some((store, p)) = store_or_skip("expire_zero").await else {
            return;
        };
        let key = format!("{p}k");
        store.set(&key, "v", Duration::from_secs(60)).await.unwrap();
        store.expire(&key, Duration::ZERO).await.unwrap();
        assert_eq!(ttl_secs(&store, &key).await, -1);
        assert_eq!(store.get(&key).await.unwrap().as_deref(), Some("v"));
        cleanup(&store, &p).await;
    }

    #[tokio::test]
    async fn set_if_absent_is_a_lease() {
        let Some((store, p)) = store_or_skip("set_if_absent").await else {
            return;
        };
        let key = format!("{p}lease");
        assert!(store
            .set_if_absent(&key, "a", Duration::from_secs(60))
            .await
            .unwrap());
        assert!(!store
            .set_if_absent(&key, "b", Duration::from_secs(60))
            .await
            .unwrap());
        assert_eq!(store.get(&key).await.unwrap().as_deref(), Some("a"));
        cleanup(&store, &p).await;
    }

    /// Glob metacharacters in a prefix are literal: `a*` must not match `ab`.
    #[tokio::test]
    async fn keys_with_prefix_treats_glob_characters_literally() {
        let Some((store, p)) = store_or_skip("keys_with_prefix").await else {
            return;
        };
        for suffix in ["x*[1]:one", "x*[1]:two", "xy:three"] {
            store
                .set(&format!("{p}{suffix}"), "v", Duration::from_secs(60))
                .await
                .unwrap();
        }
        let mut found = store.keys_with_prefix(&format!("{p}x*[1]:")).await.unwrap();
        found.sort();
        assert_eq!(
            found,
            vec![format!("{p}x*[1]:one"), format!("{p}x*[1]:two")]
        );
        cleanup(&store, &p).await;
    }
}

#[cfg(all(test, feature = "redis-store"))]
mod scan_glob_tests {
    use super::escape_scan_glob;

    #[test]
    fn every_glob_metacharacter_is_escaped() {
        assert_eq!(escape_scan_glob("plain:key"), "plain:key");
        assert_eq!(escape_scan_glob(r"a*b?c[d]e\f"), r"a\*b\?c\[d\]e\\f");
    }
}
