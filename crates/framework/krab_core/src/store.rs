use std::collections::HashMap;
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
    async fn get(&self, key: &str) -> Result<Option<String>>;
    async fn set(&self, key: &str, value: &str, ttl: Duration) -> Result<()>;
    async fn incr(&self, key: &str, delta: u64) -> Result<u64>;
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
}

#[derive(Clone, Default)]
pub struct MemoryStore {
    inner: Arc<RwLock<HashMap<String, MemoryEntry>>>,
    writes_since_sweep: Arc<std::sync::atomic::AtomicUsize>,
}

#[derive(Clone)]
struct MemoryEntry {
    value: String,
    expires_at: Option<Instant>,
}

/// Writes between full sweeps of expired entries. Amortizes the O(n) scan so
/// steady-state writes stay O(1).
const SWEEP_EVERY_N_WRITES: usize = 256;

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Reap every expired entry once per [`SWEEP_EVERY_N_WRITES`] writes.
    ///
    /// Point-`get` only reaps the key it touches, and epoch-suffixed keys
    /// (`rate:ip:<ip>:<epoch>`) are never touched again once their window
    /// passes — without this sweep they accumulate for the life of the
    /// process, one per client IP per window.
    fn sweep_if_due(&self, entries: &mut HashMap<String, MemoryEntry>) {
        use std::sync::atomic::Ordering;

        if self.writes_since_sweep.fetch_add(1, Ordering::Relaxed) + 1 < SWEEP_EVERY_N_WRITES {
            return;
        }
        self.writes_since_sweep.store(0, Ordering::Relaxed);
        let now = Instant::now();
        entries.retain(|_, entry| !entry.expires_at.map(|ts| now >= ts).unwrap_or(false));
    }
}

#[async_trait]
impl DistributedStore for MemoryStore {
    async fn get(&self, key: &str) -> Result<Option<String>> {
        {
            let guard = self.inner.read().await;
            if let Some(entry) = guard.get(key) {
                if entry
                    .expires_at
                    .map(|ts| Instant::now() >= ts)
                    .unwrap_or(false)
                {
                    drop(guard);
                    let mut guard = self.inner.write().await;
                    guard.remove(key);
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
        guard.insert(
            key.to_string(),
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

        let current = match guard.get(key) {
            Some(entry) if entry.expires_at.map(|ts| now >= ts).unwrap_or(false) => {
                guard.remove(key);
                0
            }
            Some(entry) => match entry.value.parse::<u64>() {
                Ok(value) => value,
                Err(_) => {
                    // A non-numeric value here is a key collision with a
                    // non-counter entry; resetting silently would corrupt
                    // whichever caller loses.
                    tracing::warn!(key, "memory_store_incr_reset_non_numeric_value");
                    0
                }
            },
            None => 0,
        };

        let next = current.saturating_add(delta);
        let ttl = guard.get(key).and_then(|entry| entry.expires_at);
        guard.insert(
            key.to_string(),
            MemoryEntry {
                value: next.to_string(),
                expires_at: ttl,
            },
        );
        self.sweep_if_due(&mut guard);
        Ok(next)
    }

    async fn expire(&self, key: &str, ttl: Duration) -> Result<()> {
        let mut guard = self.inner.write().await;
        if let Some(entry) = guard.get_mut(key) {
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
        Ok(guard.remove(key).is_some())
    }

    async fn keys_with_prefix(&self, prefix: &str) -> Result<Vec<String>> {
        let now = Instant::now();
        let guard = self.inner.read().await;

        // Expired-but-not-yet-reaped entries are skipped: `get` treats them as
        // absent, so listing them would report keys that cannot be read.
        Ok(guard
            .iter()
            .filter(|(key, entry)| {
                key.starts_with(prefix) && !entry.expires_at.map(|ts| now >= ts).unwrap_or(false)
            })
            .map(|(key, _)| key.clone())
            .collect())
    }
}

#[cfg(feature = "redis-store")]
#[derive(Clone)]
pub struct RedisStore {
    client: redis::Client,
}

#[cfg(feature = "redis-store")]
impl RedisStore {
    pub fn new(client: redis::Client) -> Self {
        Self { client }
    }

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

        let remaining = store.inner.read().await.len();
        assert_eq!(
            remaining, 1,
            "expired epoch keys must be swept; {remaining} entries remain"
        );
    }
}
