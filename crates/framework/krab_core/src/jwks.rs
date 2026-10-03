//! Remote JSON Web Key Sets for OIDC providers.
//!
//! A provider configured with a `jwks_url` (or the default provider, via
//! `KRAB_OIDC_JWKS_URL`) verifies tokens against the keys its identity
//! provider publishes, instead of a key set copied into the environment. Keys
//! are fetched off the request path and cached:
//!
//! - a background task refreshes every `KRAB_OIDC_JWKS_REFRESH_SECS` (default
//!   300) while the owning [`crate::http_runtime::RuntimeState`] is alive;
//! - a token naming a `kid` the cache does not hold triggers one refetch —
//!   that is how a provider's key rotation is picked up before the next
//!   scheduled refresh — rate-limited to one attempt per
//!   `KRAB_OIDC_JWKS_MIN_REFETCH_SECS` (default 30, floor 1) and single-flight
//!   (requests queued behind a fetch do not fetch again), so a stream of tokens with
//!   made-up `kid`s cannot turn the service into a request amplifier against
//!   the identity provider;
//! - a failed fetch keeps serving the last good key set. Until the first
//!   successful fetch there is nothing to verify with, and the request path
//!   answers 503 rather than 401: the outage is ours, not the caller's.
//!
//! Symmetric (`oct`) keys in a published set are ignored. A JWKS document is
//! public by design, so an HMAC secret in one is not a secret. Encryption
//! keys (`"use": "enc"`) and keys of a type the verifier does not support
//! (for example `ES512`, `ES256K` or an `X25519` curve) are skipped one by
//! one, with a warning naming the `kid`; the rest of the set still loads.

use std::collections::HashMap;
use std::sync::{Mutex, RwLock};
use std::time::{Duration, Instant};

use jsonwebtoken::jwk::{AlgorithmParameters, Jwk};
use jsonwebtoken::DecodingKey;
use tracing::{info, warn};

use crate::http_auth::JwtAlgorithmFamily;

/// A JWKS document with its keys left unparsed, so each key can be parsed —
/// and rejected — on its own.
#[derive(serde::Deserialize)]
struct RawJwkSet {
    keys: Vec<serde_json::Value>,
}

/// Default interval between background refreshes.
const DEFAULT_REFRESH_SECS: u64 = 300;
/// Floor on the background refresh interval.
const MIN_REFRESH_SECS: u64 = 30;
/// Default minimum gap between on-demand refetches for an unknown `kid`.
const DEFAULT_MIN_REFETCH_SECS: u64 = 30;
/// Floor on the on-demand refetch gap. `0` would remove the rate limit and
/// let tokens with made-up `kid`s drive one IdP fetch each.
const MIN_REFETCH_FLOOR_SECS: u64 = 1;
/// Default fetch timeout.
const DEFAULT_TIMEOUT_MS: u64 = 3_000;
/// Largest JWKS document accepted, in bytes. A real key set is a few KiB; the
/// cap keeps a misbehaving or hostile endpoint from exhausting memory.
const MAX_JWKS_BYTES: usize = 1024 * 1024;

/// Tunables for every JWKS source in a process, read from the environment.
#[derive(Debug, Clone, Copy)]
pub(crate) struct JwksSettings {
    pub(crate) refresh_every: Duration,
    pub(crate) min_refetch: Duration,
    pub(crate) timeout: Duration,
}

impl JwksSettings {
    pub(crate) fn from_env() -> Self {
        let secs = |name: &str, default: u64| {
            std::env::var(name)
                .ok()
                .and_then(|v| v.trim().parse::<u64>().ok())
                .unwrap_or(default)
        };
        Self {
            refresh_every: Duration::from_secs(
                secs("KRAB_OIDC_JWKS_REFRESH_SECS", DEFAULT_REFRESH_SECS).max(MIN_REFRESH_SECS),
            ),
            min_refetch: Duration::from_secs(
                secs("KRAB_OIDC_JWKS_MIN_REFETCH_SECS", DEFAULT_MIN_REFETCH_SECS)
                    .max(MIN_REFETCH_FLOOR_SECS),
            ),
            timeout: Duration::from_millis(
                secs("KRAB_OIDC_JWKS_TIMEOUT_MS", DEFAULT_TIMEOUT_MS).max(100),
            ),
        }
    }
}

/// Whether `url` may be fetched in `environment`.
///
/// Keys fetched over plain HTTP can be substituted by anyone on the path, which
/// makes every token they "verify" forgeable. Outside `dev` only `https://` is
/// accepted; in `dev`, `http://` is allowed for a local identity provider.
pub(crate) fn url_allowed(url: &str, environment: &crate::config::Environment) -> bool {
    let url = url.trim();
    url.starts_with("https://")
        || (matches!(environment, crate::config::Environment::Dev) && url.starts_with("http://"))
}

#[derive(Default)]
struct JwksKeys {
    /// Keys by `kid`. A key published without a `kid` is stored under a
    /// synthetic `#<index>` name, so kid-less tokens can still try it.
    keys: HashMap<String, Vec<(JwtAlgorithmFamily, DecodingKey)>>,
    loaded: bool,
}

/// One provider's remote key set.
pub(crate) struct JwksSource {
    url: String,
    client: reqwest::Client,
    settings: JwksSettings,
    keys: RwLock<JwksKeys>,
    last_attempt: Mutex<Option<Instant>>,
    /// When the key set last loaded successfully.
    last_success: Mutex<Option<Instant>>,
    /// Single-flight: concurrent requests that all miss wait for one fetch.
    fetch_lock: tokio::sync::Mutex<()>,
}

impl JwksSource {
    /// A source for `url`. Fails only when the HTTP client cannot be built
    /// (for example, no TLS backend could initialise); the caller must then
    /// treat the provider as unusable rather than fall back to a client
    /// without the settings below.
    ///
    /// Redirects are never followed: the configured URL is the trust anchor,
    /// and a redirect — possibly from `https://` to `http://` — would let
    /// whoever controls the response choose where keys come from.
    pub(crate) fn new(url: String, settings: JwksSettings) -> anyhow::Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(settings.timeout)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| anyhow::anyhow!("JWKS HTTP client could not be built: {error}"))?;
        Ok(Self {
            url,
            client,
            settings,
            keys: RwLock::new(JwksKeys::default()),
            last_attempt: Mutex::new(None),
            last_success: Mutex::new(None),
            fetch_lock: tokio::sync::Mutex::new(()),
        })
    }

    pub(crate) fn url(&self) -> &str {
        &self.url
    }

    pub(crate) fn settings(&self) -> JwksSettings {
        self.settings
    }

    /// Whether a key set has ever been fetched successfully.
    pub(crate) fn loaded(&self) -> bool {
        self.read_keys().loaded
    }

    pub(crate) fn has_kid(&self, kid: &str) -> bool {
        self.read_keys().keys.contains_key(kid)
    }

    /// Every cached `kid`, for tokens that name none.
    pub(crate) fn kids(&self) -> Vec<String> {
        let mut kids: Vec<String> = self.read_keys().keys.keys().cloned().collect();
        kids.sort();
        kids
    }

    pub(crate) fn key(&self, kid: &str, family: JwtAlgorithmFamily) -> Option<DecodingKey> {
        self.read_keys()
            .keys
            .get(kid)?
            .iter()
            .find(|(f, _)| *f == family)
            .map(|(_, key)| key.clone())
    }

    /// Whether an on-demand refetch is allowed now.
    pub(crate) fn may_refetch(&self) -> bool {
        let last = self.last_attempt.lock().unwrap_or_else(|e| e.into_inner());
        last.is_none_or(|at| at.elapsed() >= self.settings.min_refetch)
    }

    /// Whether a request for `kid` needs this key set fetched: it never
    /// loaded, or it lacks `kid` — and the refetch rate limit allows it.
    fn fetch_needed(&self, kid: Option<&str>) -> bool {
        (!self.loaded() || kid.is_some_and(|kid| !self.has_kid(kid))) && self.may_refetch()
    }

    /// Whether the scheduled refresh interval has passed since the last
    /// successful load (and the refetch rate limit allows a fetch).
    fn periodic_refresh_due(&self) -> bool {
        let due = self
            .last_success
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some_and(|at| at.elapsed() >= self.settings.refresh_every);
        due && self.may_refetch()
    }

    /// On the request path: fetch the key set if a token naming `kid` needs
    /// it (see `fetch_needed`), single-flight. `None` when no fetch was made.
    ///
    /// With `periodic` — set when no background refresh could be scheduled —
    /// a set older than the refresh interval is refetched too, so a key the
    /// provider removed stops verifying without a background task.
    ///
    /// The need is checked again after acquiring the fetch lock: requests
    /// that queued behind a fetch find the set loaded (or the attempt just
    /// made inside the rate limit) and do not fetch again. Checking only
    /// before the lock let every queued request fetch in turn.
    pub(crate) async fn refresh_if_needed(
        &self,
        kid: Option<&str>,
        periodic: bool,
    ) -> Option<anyhow::Result<usize>> {
        let needed = || self.fetch_needed(kid) || (periodic && self.periodic_refresh_due());
        if !needed() {
            return None;
        }
        let _single_flight = self.fetch_lock.lock().await;
        if !needed() {
            return None;
        }
        Some(self.fetch_locked().await)
    }

    /// Fetch and replace the key set now. On failure the previous set stays.
    pub(crate) async fn refresh(&self) -> anyhow::Result<usize> {
        let _single_flight = self.fetch_lock.lock().await;
        self.fetch_locked().await
    }

    /// The fetch itself; the caller holds `fetch_lock`.
    ///
    /// The attempt is stamped when it starts (so the rate limit covers a
    /// fetch in flight) and again when it ends, so a fetch slower than the
    /// refetch gap does not leave the requests queued behind it free to
    /// fetch again at once.
    async fn fetch_locked(&self) -> anyhow::Result<usize> {
        self.stamp_attempt();
        let result = self.fetch_and_replace().await;
        self.stamp_attempt();
        result
    }

    fn stamp_attempt(&self) {
        *self.last_attempt.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
    }

    async fn fetch_and_replace(&self) -> anyhow::Result<usize> {
        let mut response = self.client.get(&self.url).send().await?;
        let status = response.status();
        anyhow::ensure!(
            status.is_success(),
            "JWKS at {} answered {status}{}",
            self.url,
            if status.is_redirection() {
                " (redirects are not followed)"
            } else {
                ""
            }
        );
        if let Some(length) = response.content_length() {
            anyhow::ensure!(
                length <= MAX_JWKS_BYTES as u64,
                "JWKS at {} is {length} bytes, over the {MAX_JWKS_BYTES}-byte limit",
                self.url
            );
        }
        // Content-Length may be absent (chunked) or wrong: enforce the cap on
        // what is actually read.
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            anyhow::ensure!(
                body.len() + chunk.len() <= MAX_JWKS_BYTES,
                "JWKS at {} exceeds the {MAX_JWKS_BYTES}-byte limit",
                self.url
            );
            body.extend_from_slice(&chunk);
        }
        let set: RawJwkSet = serde_json::from_slice(&body)?;

        let mut keys: HashMap<String, Vec<(JwtAlgorithmFamily, DecodingKey)>> = HashMap::new();
        for (index, raw) in set.keys.into_iter().enumerate() {
            let Some(jwk) = self.parse_key(index, raw) else {
                continue;
            };
            let jwk = &jwk;
            let family = match &jwk.algorithm {
                AlgorithmParameters::RSA(_) => JwtAlgorithmFamily::Rsa,
                AlgorithmParameters::EllipticCurve(_) => JwtAlgorithmFamily::Ec,
                AlgorithmParameters::OctetKeyPair(_) => JwtAlgorithmFamily::Ed,
                AlgorithmParameters::OctetKey(_) => {
                    warn!(url = %self.url, "jwks_symmetric_key_ignored");
                    continue;
                }
            };
            let key = match DecodingKey::from_jwk(jwk) {
                Ok(key) => key,
                Err(error) => {
                    warn!(url = %self.url, %error, "jwks_key_unusable_skipped");
                    continue;
                }
            };
            let kid = jwk
                .common
                .key_id
                .clone()
                .unwrap_or_else(|| format!("#{index}"));
            keys.entry(kid).or_default().push((family, key));
        }

        anyhow::ensure!(!keys.is_empty(), "JWKS at {} held no usable keys", self.url);
        let count = keys.len();
        *self.keys.write().unwrap_or_else(|e| e.into_inner()) = JwksKeys { keys, loaded: true };
        *self.last_success.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
        info!(url = %self.url, keys = count, "jwks_refreshed");
        Ok(count)
    }

    /// One published key, or `None` (logged) when it is not a signature key
    /// this verifier can use. Parsing each key on its own means one key of
    /// a type `jsonwebtoken` does not model (alg `ES512`, `RSA-OAEP-384`,
    /// `ECDH-ES*`, `ES256K`; crv `X25519`, ...) is skipped instead of failing
    /// the whole set — and with it, every token, for as long as the provider
    /// publishes that key.
    fn parse_key(&self, index: usize, raw: serde_json::Value) -> Option<Jwk> {
        let kid = raw
            .get("kid")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string();
        if raw.get("use").and_then(serde_json::Value::as_str) == Some("enc") {
            info!(url = %self.url, kid = %kid, index, "jwks_encryption_key_skipped");
            return None;
        }
        match serde_json::from_value::<Jwk>(raw) {
            Ok(jwk) => Some(jwk),
            Err(error) => {
                warn!(url = %self.url, kid = %kid, index, %error, "jwks_key_unsupported_skipped");
                None
            }
        }
    }

    fn read_keys(&self) -> std::sync::RwLockReadGuard<'_, JwksKeys> {
        self.keys.read().unwrap_or_else(|e| e.into_inner())
    }
}

/// Parse a key retirement time: RFC 3339 in UTC (`2026-10-01T00:00:00Z`, with
/// optional fractional seconds or a `±HH:MM` offset) or integer Unix seconds.
/// Returns Unix seconds.
pub(crate) fn parse_not_after(raw: &str) -> Option<i64> {
    let raw = raw.trim();
    if let Ok(secs) = raw.parse::<i64>() {
        return Some(secs);
    }

    let (date, rest) = raw.split_once(['T', 't', ' '])?;
    let mut date_parts = date.splitn(3, '-');
    let year: i64 = date_parts.next()?.parse().ok()?;
    let month: u32 = date_parts.next()?.parse().ok()?;
    let day: u32 = date_parts.next()?.parse().ok()?;

    let (time, offset_secs) = if let Some(time) = rest.strip_suffix(['Z', 'z']) {
        (time, 0)
    } else {
        let split = rest.rfind(['+', '-'])?;
        let (time, offset) = rest.split_at(split);
        let sign = if offset.starts_with('-') { -1 } else { 1 };
        let (oh, om) = offset[1..].split_once(':')?;
        let offset: i64 = oh.parse::<i64>().ok()? * 3600 + om.parse::<i64>().ok()? * 60;
        (time, sign * offset)
    };
    let time = time.split('.').next()?;
    let mut time_parts = time.splitn(3, ':');
    let hour: i64 = time_parts.next()?.parse().ok()?;
    let minute: i64 = time_parts.next()?.parse().ok()?;
    let second: i64 = time_parts.next()?.parse().ok()?;

    if !(1..=12).contains(&month)
        || day < 1
        || day > days_in_month(year, month)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }

    Some(
        days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second
            - offset_secs,
    )
}

/// Days in `month` (1-12) of `year`, proleptic Gregorian. An impossible date
/// such as `2026-02-30` must be rejected, not rolled into March.
fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        2 if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's
/// algorithm).
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let yoe = year - era * 400;
    let month = i64::from(month);
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::{parse_not_after, url_allowed, JwksSettings, JwksSource};
    use crate::config::Environment;
    use crate::http_auth::JwtAlgorithmFamily;
    use serde_json::json;
    use std::time::Duration;

    #[test]
    fn not_after_accepts_rfc3339_and_unix_seconds() {
        assert_eq!(parse_not_after("0"), Some(0));
        assert_eq!(parse_not_after("1790000000"), Some(1_790_000_000));
        assert_eq!(parse_not_after("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_not_after("2026-10-01T00:00:00Z"), Some(1_790_812_800));
        assert_eq!(
            parse_not_after("2026-10-01T02:00:00+02:00"),
            Some(1_790_812_800)
        );
        assert_eq!(
            parse_not_after("2026-09-30T19:00:00.250-05:00"),
            Some(1_790_812_800)
        );
        assert_eq!(parse_not_after("2024-02-29T12:00:00Z"), Some(1_709_208_000));
        for bad in [
            "",
            "soon",
            "2026-13-01T00:00:00Z",
            "2026-10-01",
            "2026-10-01T25:00:00Z",
        ] {
            assert_eq!(parse_not_after(bad), None, "{bad}");
        }
    }

    /// Impossible calendar dates are rejected rather than rolled over into
    /// the next month (`2026-02-30` used to mean 2 March).
    #[test]
    fn not_after_rejects_impossible_dates() {
        for bad in [
            "2026-02-29T00:00:00Z",
            "2026-02-30T00:00:00Z",
            "2026-04-31T00:00:00Z",
            "2026-06-31T00:00:00Z",
            "2026-09-31T00:00:00Z",
            "2026-11-31T00:00:00Z",
            "1900-02-29T00:00:00Z",
            "2026-01-00T00:00:00Z",
        ] {
            assert_eq!(parse_not_after(bad), None, "{bad}");
        }
        for good in [
            "2024-02-29T00:00:00Z",
            "2000-02-29T00:00:00Z",
            "2026-01-31T00:00:00Z",
            "2026-12-31T23:59:59Z",
        ] {
            assert!(parse_not_after(good).is_some(), "{good}");
        }
    }

    /// Base64url `x` of a throwaway Ed25519 public key (the auth suite's
    /// `ED_X_1`).
    const ED_X: &str = "tQCGXC6DpH3eQ7mQpTmUwz_UrjPnQ-X2ztczWt5Uyis";

    fn settings() -> JwksSettings {
        JwksSettings {
            refresh_every: Duration::from_secs(300),
            min_refetch: Duration::from_secs(30),
            timeout: Duration::from_secs(2),
        }
    }

    /// Serve `app` on an ephemeral local port; returns its base URL.
    async fn serve(app: axum::Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        format!("http://{addr}")
    }

    async fn serve_json(document: serde_json::Value) -> String {
        let app = axum::Router::new().route(
            "/jwks",
            axum::routing::get(move || {
                let document = document.clone();
                async move { axum::Json(document) }
            }),
        );
        format!("{}/jwks", serve(app).await)
    }

    /// One key `jsonwebtoken` cannot model used to fail the whole set, and
    /// with it every token (a permanent 503 before the first load). Such keys
    /// — and encryption keys — are now skipped one by one.
    #[tokio::test]
    async fn unsupported_keys_do_not_break_the_set() {
        let url = serve_json(json!({"keys": [
            {"kty": "EC", "crv": "P-521", "alg": "ES512", "kid": "es512",
             "x": "AQ", "y": "AQ", "use": "sig"},
            {"kty": "EC", "crv": "secp256k1", "alg": "ES256K", "kid": "k1",
             "x": "AQ", "y": "AQ"},
            {"kty": "OKP", "crv": "X25519", "kid": "x25519", "x": ED_X, "use": "enc"},
            {"kty": "RSA", "alg": "RSA-OAEP-384", "kid": "oaep", "n": "AQ", "e": "AQAB",
             "use": "enc"},
            {"kty": "OKP", "crv": "Ed25519", "kid": "enc-ed", "x": ED_X, "use": "enc"},
            {"kty": "OKP", "crv": "Ed25519", "kid": "good", "x": ED_X, "use": "sig",
             "alg": "EdDSA"}
        ]}))
        .await;

        let source = JwksSource::new(url, settings()).unwrap();
        assert_eq!(source.refresh().await.unwrap(), 1);
        assert!(source.has_kid("good"));
        assert!(
            !source.has_kid("enc-ed"),
            "an encryption key must not verify signatures"
        );
        assert!(source.key("good", JwtAlgorithmFamily::Ed).is_some());
    }

    /// A redirect is not followed: the configured URL is the trust anchor,
    /// and a redirect could move the fetch to plain http or another host.
    #[tokio::test]
    async fn redirects_are_not_followed() {
        let target_hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let hits = target_hits.clone();
        let app = axum::Router::new()
            .route(
                "/jwks",
                axum::routing::get(|| async { axum::response::Redirect::temporary("/real") }),
            )
            .route(
                "/real",
                axum::routing::get(move || {
                    let hits = hits.clone();
                    async move {
                        hits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        axum::Json(json!({"keys": [
                            {"kty": "OKP", "crv": "Ed25519", "kid": "k", "x": ED_X}
                        ]}))
                    }
                }),
            );
        let url = format!("{}/jwks", serve(app).await);

        let source = JwksSource::new(url, settings()).unwrap();
        let error = source.refresh().await.unwrap_err().to_string();
        assert!(error.contains("redirects are not followed"), "{error}");
        assert!(!source.loaded());
        assert_eq!(target_hits.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    /// The body is capped, whether or not the server declares its length.
    #[tokio::test]
    async fn oversized_documents_are_rejected() {
        let padding = "x".repeat(super::MAX_JWKS_BYTES);
        let document = json!({
            "keys": [{"kty": "OKP", "crv": "Ed25519", "kid": "k", "x": ED_X}],
            "padding": padding,
        })
        .to_string();
        let declared = document.clone();
        let app = axum::Router::new().route(
            "/declared",
            axum::routing::get(move || {
                let body = declared.clone();
                async move { body }
            }),
        );
        let declared_url = format!("{}/declared", serve(app).await);

        // No Content-Length: the body runs to connection close, so only the
        // cap on bytes actually read can stop it.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let undeclared_url = format!("http://{}/jwks", listener.local_addr().unwrap());
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            if let Ok((mut socket, _)) = listener.accept().await {
                let mut request = [0u8; 4096];
                let _ = socket.read(&mut request).await;
                let head = "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\nconnection: close\r\n\r\n";
                let _ = socket.write_all(head.as_bytes()).await;
                let _ = socket.write_all(document.as_bytes()).await;
                let _ = socket.shutdown().await;
            }
        });

        for url in [declared_url, undeclared_url] {
            let source = JwksSource::new(url.clone(), settings()).unwrap();
            let error = source.refresh().await.unwrap_err().to_string();
            assert!(error.contains("limit"), "{url}: {error}");
            assert!(!source.loaded(), "{url}");
        }
    }

    /// `KRAB_OIDC_JWKS_MIN_REFETCH_SECS=0` would remove the refetch rate
    /// limit; it is floored at one second.
    #[test]
    #[serial_test::serial]
    fn min_refetch_has_a_floor() {
        std::env::set_var("KRAB_OIDC_JWKS_MIN_REFETCH_SECS", "0");
        let floored = JwksSettings::from_env().min_refetch;
        std::env::set_var("KRAB_OIDC_JWKS_MIN_REFETCH_SECS", "45");
        let explicit = JwksSettings::from_env().min_refetch;
        std::env::remove_var("KRAB_OIDC_JWKS_MIN_REFETCH_SECS");
        assert_eq!(floored, Duration::from_secs(1));
        assert_eq!(explicit, Duration::from_secs(45));
    }

    #[test]
    fn plain_http_is_dev_only() {
        assert!(url_allowed(
            "https://idp.example.com/jwks",
            &Environment::Prod
        ));
        assert!(!url_allowed(
            "http://idp.example.com/jwks",
            &Environment::Prod
        ));
        assert!(!url_allowed(
            "http://idp.example.com/jwks",
            &Environment::Staging
        ));
        assert!(url_allowed("http://127.0.0.1:8080/jwks", &Environment::Dev));
        assert!(!url_allowed("ftp://x", &Environment::Dev));
    }
}
