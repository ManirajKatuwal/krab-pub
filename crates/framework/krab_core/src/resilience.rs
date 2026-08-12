//! Circuit breaker primitives for outbound resilience.
//!
//! Two trip policies are supported:
//!
//! - **Consecutive failures** (the historical default): the circuit opens
//!   after N failures in a row. [`CircuitBreaker::new`] builds this mode.
//! - **Rolling failure rate**: per-second success/failure counts are kept in
//!   a ring buffer; the circuit opens when the failure rate over the window
//!   reaches a threshold, given a minimum number of samples. Built via
//!   [`CircuitBreakerConfig`] and [`CircuitBreaker::from_config`].
//!
//! [`CircuitBreaker`] is single-owner (`&mut self`); wrap it in
//! [`SharedCircuitBreaker`] for `&self` access shared across tasks/threads.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CircuitState {
    Closed,
    Open,
    HalfOpen,
}

/// When the circuit should trip from `Closed` to `Open`.
#[derive(Debug, Clone, PartialEq)]
pub enum TripPolicy {
    /// Open after this many consecutive failures.
    ConsecutiveFailures {
        /// Number of consecutive failures that opens the circuit.
        failure_threshold: u32,
    },
    /// Open when the failure rate over a rolling window reaches a threshold.
    FailureRate {
        /// Failure ratio in `0.0..=1.0` that opens the circuit (`>=` trips).
        failure_rate_threshold: f64,
        /// Rolling window length in seconds (per-second ring buffer).
        window_secs: u32,
        /// Minimum samples (successes + failures) in the window before the
        /// rate is evaluated at all.
        min_samples: u32,
    },
}

/// Pure-code configuration for a [`CircuitBreaker`]. No environment variables
/// are read.
#[derive(Debug, Clone, PartialEq)]
pub struct CircuitBreakerConfig {
    /// How long the circuit stays `Open` before probing (`HalfOpen`).
    pub open_cooldown: Duration,
    /// Maximum in-flight probes allowed while `HalfOpen`.
    pub half_open_max_probes: u32,
    /// Trip policy: consecutive failures or rolling failure rate.
    pub trip_policy: TripPolicy,
}

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        Self {
            open_cooldown: Duration::from_secs(30),
            half_open_max_probes: 1,
            trip_policy: TripPolicy::ConsecutiveFailures {
                failure_threshold: 5,
            },
        }
    }
}

/// Time source. Production uses the system clock; tests swap in a manually
/// advanced clock so cooldowns and rolling windows need no real sleeping.
#[derive(Debug, Clone)]
enum Clock {
    System,
    #[cfg(test)]
    Manual(Arc<Mutex<Instant>>),
}

impl Clock {
    fn now(&self) -> Instant {
        match self {
            Clock::System => Instant::now(),
            #[cfg(test)]
            Clock::Manual(instant) => *instant.lock().unwrap_or_else(|e| e.into_inner()),
        }
    }
}

/// One second's worth of samples in the rolling window.
#[derive(Debug, Clone, Copy, Default)]
struct RateBucket {
    second: u64,
    successes: u32,
    failures: u32,
}

/// Ring buffer of per-second success/failure counts.
#[derive(Debug, Clone)]
struct RateWindow {
    /// Reference point for computing the current second. Set lazily on the
    /// first sample so an injected clock is honoured.
    epoch: Option<Instant>,
    buckets: Vec<RateBucket>,
    window_secs: u64,
}

impl RateWindow {
    fn new(window_secs: u32) -> Self {
        let window_secs = window_secs.max(1) as u64;
        Self {
            epoch: None,
            buckets: vec![RateBucket::default(); window_secs as usize],
            window_secs,
        }
    }

    fn current_second(&mut self, now: Instant) -> u64 {
        let epoch = *self.epoch.get_or_insert(now);
        now.saturating_duration_since(epoch).as_secs()
    }

    fn record(&mut self, now: Instant, success: bool) {
        let second = self.current_second(now);
        let index = (second % self.window_secs) as usize;
        let bucket = &mut self.buckets[index];
        if bucket.second != second {
            *bucket = RateBucket {
                second,
                successes: 0,
                failures: 0,
            };
        }
        if success {
            bucket.successes = bucket.successes.saturating_add(1);
        } else {
            bucket.failures = bucket.failures.saturating_add(1);
        }
    }

    /// Sum of (successes, failures) over the buckets still inside the window.
    fn totals(&mut self, now: Instant) -> (u64, u64) {
        let current = self.current_second(now);
        let oldest_valid = current.saturating_sub(self.window_secs - 1);
        let mut successes = 0u64;
        let mut failures = 0u64;
        for bucket in &self.buckets {
            if bucket.second >= oldest_valid && bucket.second <= current {
                successes += u64::from(bucket.successes);
                failures += u64::from(bucket.failures);
            }
        }
        (successes, failures)
    }

    fn reset(&mut self) {
        for bucket in &mut self.buckets {
            *bucket = RateBucket::default();
        }
        self.epoch = None;
    }
}

#[derive(Debug, Clone)]
pub struct CircuitBreaker {
    open_cooldown: Duration,
    half_open_max_probes: u32,
    trip_policy: TripPolicy,
    state: CircuitState,
    consecutive_failures: u32,
    opened_at: Option<Instant>,
    half_open_probes: u32,
    window: Option<RateWindow>,
    clock: Clock,
}

impl CircuitBreaker {
    /// Consecutive-failure breaker (historical constructor, unchanged
    /// behaviour).
    pub fn new(failure_threshold: u32, open_cooldown: Duration, half_open_max_probes: u32) -> Self {
        Self::from_config(CircuitBreakerConfig {
            open_cooldown,
            half_open_max_probes,
            trip_policy: TripPolicy::ConsecutiveFailures { failure_threshold },
        })
    }

    /// Build a breaker from a [`CircuitBreakerConfig`].
    pub fn from_config(config: CircuitBreakerConfig) -> Self {
        let window = match config.trip_policy {
            TripPolicy::FailureRate { window_secs, .. } => Some(RateWindow::new(window_secs)),
            TripPolicy::ConsecutiveFailures { .. } => None,
        };
        Self {
            open_cooldown: config.open_cooldown,
            half_open_max_probes: config.half_open_max_probes,
            trip_policy: config.trip_policy,
            state: CircuitState::Closed,
            consecutive_failures: 0,
            opened_at: None,
            half_open_probes: 0,
            window,
            clock: Clock::System,
        }
    }

    pub fn state(&self) -> CircuitState {
        self.state
    }

    pub fn allow_request(&mut self) -> bool {
        match self.state {
            CircuitState::Closed => true,
            CircuitState::Open => {
                if let Some(opened_at) = self.opened_at {
                    if self.clock.now().saturating_duration_since(opened_at) >= self.open_cooldown {
                        self.state = CircuitState::HalfOpen;
                        self.half_open_probes = 1;
                        return true;
                    }
                }
                false
            }
            CircuitState::HalfOpen => {
                if self.half_open_probes < self.half_open_max_probes {
                    self.half_open_probes += 1;
                    true
                } else {
                    false
                }
            }
        }
    }

    pub fn record_success(&mut self) {
        match self.state {
            CircuitState::Closed => {
                self.consecutive_failures = 0;
                if let Some(window) = self.window.as_mut() {
                    window.record(self.clock.now(), true);
                }
            }
            // Half-open probe success (or a legacy success while open)
            // closes the circuit and resets all counters.
            CircuitState::HalfOpen | CircuitState::Open => self.close_and_reset(),
        }
    }

    pub fn record_failure(&mut self) {
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);

        match self.state {
            CircuitState::Closed => {
                let now = self.clock.now();
                if let Some(window) = self.window.as_mut() {
                    window.record(now, false);
                }
                if self.should_trip(now) {
                    self.trip_open(now);
                }
            }
            CircuitState::HalfOpen => {
                let now = self.clock.now();
                self.trip_open(now);
            }
            CircuitState::Open => {}
        }
    }

    fn should_trip(&mut self, now: Instant) -> bool {
        match self.trip_policy {
            TripPolicy::ConsecutiveFailures { failure_threshold } => {
                self.consecutive_failures >= failure_threshold
            }
            TripPolicy::FailureRate {
                failure_rate_threshold,
                min_samples,
                ..
            } => {
                let Some(window) = self.window.as_mut() else {
                    return false;
                };
                let (successes, failures) = window.totals(now);
                let total = successes + failures;
                if total < u64::from(min_samples) || total == 0 {
                    return false;
                }
                (failures as f64 / total as f64) >= failure_rate_threshold
            }
        }
    }

    fn trip_open(&mut self, now: Instant) {
        self.state = CircuitState::Open;
        self.opened_at = Some(now);
        self.half_open_probes = 0;
        tracing::warn!(
            consecutive_failures = self.consecutive_failures,
            cooldown_ms = self.open_cooldown.as_millis() as u64,
            "circuit_breaker_opened"
        );
    }

    fn close_and_reset(&mut self) {
        if self.state != CircuitState::Closed {
            tracing::info!("circuit_breaker_closed");
        }
        self.state = CircuitState::Closed;
        self.consecutive_failures = 0;
        self.half_open_probes = 0;
        self.opened_at = None;
        if let Some(window) = self.window.as_mut() {
            window.reset();
        }
    }

    #[cfg(test)]
    fn with_manual_clock(mut self) -> (Self, Arc<Mutex<Instant>>) {
        let handle = Arc::new(Mutex::new(Instant::now()));
        self.clock = Clock::Manual(Arc::clone(&handle));
        (self, handle)
    }
}

// ── SharedCircuitBreaker ────────────────────────────────────────────────────

/// Thread-safe circuit breaker for sharing across tasks: `&self` methods over
/// an `Arc<Mutex<CircuitBreaker>>`. Cloning shares the same breaker.
#[derive(Debug, Clone)]
pub struct SharedCircuitBreaker {
    inner: Arc<Mutex<CircuitBreaker>>,
}

impl SharedCircuitBreaker {
    /// Wrap an existing breaker for shared use.
    pub fn new(breaker: CircuitBreaker) -> Self {
        Self {
            inner: Arc::new(Mutex::new(breaker)),
        }
    }

    /// Build a shared breaker from a [`CircuitBreakerConfig`].
    pub fn from_config(config: CircuitBreakerConfig) -> Self {
        Self::new(CircuitBreaker::from_config(config))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, CircuitBreaker> {
        // A poisoned lock only means another task panicked mid-transition;
        // the breaker state itself is always valid, so recover it.
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn state(&self) -> CircuitState {
        self.lock().state()
    }

    pub fn allow_request(&self) -> bool {
        self.lock().allow_request()
    }

    pub fn record_success(&self) {
        self.lock().record_success();
    }

    pub fn record_failure(&self) {
        self.lock().record_failure();
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn advance(clock: &Arc<Mutex<Instant>>, by: Duration) {
        let mut now = clock.lock().unwrap();
        *now += by;
    }

    fn consecutive_breaker(threshold: u32) -> (CircuitBreaker, Arc<Mutex<Instant>>) {
        CircuitBreaker::new(threshold, Duration::from_secs(2), 1).with_manual_clock()
    }

    #[test]
    fn closed_circuit_allows_requests() {
        let (mut breaker, _clock) = consecutive_breaker(3);
        assert_eq!(breaker.state(), CircuitState::Closed);
        assert!(breaker.allow_request());
    }

    #[test]
    fn opens_at_consecutive_failure_threshold() {
        let (mut breaker, _clock) = consecutive_breaker(3);
        breaker.record_failure();
        breaker.record_failure();
        assert_eq!(breaker.state(), CircuitState::Closed);

        breaker.record_failure();
        assert_eq!(breaker.state(), CircuitState::Open);
        assert!(!breaker.allow_request());
    }

    #[test]
    fn success_resets_consecutive_failure_count() {
        let (mut breaker, _clock) = consecutive_breaker(3);
        breaker.record_failure();
        breaker.record_failure();
        breaker.record_success();
        breaker.record_failure();
        breaker.record_failure();
        assert_eq!(breaker.state(), CircuitState::Closed);
    }

    #[test]
    fn open_blocks_until_cooldown_elapses() {
        let (mut breaker, clock) = consecutive_breaker(1);
        breaker.record_failure();
        assert_eq!(breaker.state(), CircuitState::Open);
        assert!(!breaker.allow_request());

        advance(&clock, Duration::from_millis(1999));
        assert!(!breaker.allow_request());

        advance(&clock, Duration::from_millis(1));
        assert!(breaker.allow_request());
        assert_eq!(breaker.state(), CircuitState::HalfOpen);
    }

    #[test]
    fn half_open_allows_limited_probes() {
        let (mut breaker, clock) =
            CircuitBreaker::new(1, Duration::from_secs(2), 2).with_manual_clock();
        breaker.record_failure();
        advance(&clock, Duration::from_secs(2));

        // Cooldown transition consumes the first probe slot.
        assert!(breaker.allow_request());
        assert!(breaker.allow_request());
        assert!(!breaker.allow_request());
        assert_eq!(breaker.state(), CircuitState::HalfOpen);
    }

    #[test]
    fn half_open_failure_reopens_and_restarts_cooldown() {
        let (mut breaker, clock) = consecutive_breaker(1);
        breaker.record_failure();
        advance(&clock, Duration::from_secs(2));
        assert!(breaker.allow_request());
        assert_eq!(breaker.state(), CircuitState::HalfOpen);

        breaker.record_failure();
        assert_eq!(breaker.state(), CircuitState::Open);
        assert!(!breaker.allow_request());

        // The cooldown restarts from the re-open.
        advance(&clock, Duration::from_millis(1999));
        assert!(!breaker.allow_request());
        advance(&clock, Duration::from_millis(1));
        assert!(breaker.allow_request());
    }

    #[test]
    fn half_open_success_closes_and_resets() {
        let (mut breaker, clock) = consecutive_breaker(2);
        breaker.record_failure();
        breaker.record_failure();
        advance(&clock, Duration::from_secs(2));
        assert!(breaker.allow_request());
        assert_eq!(breaker.state(), CircuitState::HalfOpen);

        breaker.record_success();
        assert_eq!(breaker.state(), CircuitState::Closed);
        assert!(breaker.allow_request());

        // Counters were reset: it takes the full threshold to open again.
        breaker.record_failure();
        assert_eq!(breaker.state(), CircuitState::Closed);
        breaker.record_failure();
        assert_eq!(breaker.state(), CircuitState::Open);
    }

    fn rate_breaker(
        rate: f64,
        window_secs: u32,
        min_samples: u32,
    ) -> (CircuitBreaker, Arc<Mutex<Instant>>) {
        CircuitBreaker::from_config(CircuitBreakerConfig {
            open_cooldown: Duration::from_secs(2),
            half_open_max_probes: 1,
            trip_policy: TripPolicy::FailureRate {
                failure_rate_threshold: rate,
                window_secs,
                min_samples,
            },
        })
        .with_manual_clock()
    }

    #[test]
    fn failure_rate_does_not_trip_below_min_samples() {
        let (mut breaker, _clock) = rate_breaker(0.5, 10, 5);
        // 100% failures but only 4 samples: stays closed.
        for _ in 0..4 {
            breaker.record_failure();
        }
        assert_eq!(breaker.state(), CircuitState::Closed);
    }

    #[test]
    fn failure_rate_trips_at_threshold_with_min_samples() {
        let (mut breaker, _clock) = rate_breaker(0.5, 10, 4);
        breaker.record_success();
        breaker.record_success();
        breaker.record_failure();
        assert_eq!(breaker.state(), CircuitState::Closed);

        // 2 failures / 4 samples = 0.5 >= threshold: trips.
        breaker.record_failure();
        assert_eq!(breaker.state(), CircuitState::Open);
        assert!(!breaker.allow_request());
    }

    #[test]
    fn failure_rate_stays_closed_below_threshold() {
        let (mut breaker, _clock) = rate_breaker(0.5, 10, 4);
        for _ in 0..9 {
            breaker.record_success();
        }
        for _ in 0..4 {
            breaker.record_failure();
        }
        // 4 failures / 13 samples ≈ 0.31 < 0.5: stays closed.
        assert_eq!(breaker.state(), CircuitState::Closed);
    }

    #[test]
    fn failure_rate_window_expires_old_failures() {
        let (mut breaker, clock) = rate_breaker(0.5, 3, 4);
        breaker.record_failure();
        breaker.record_failure();
        breaker.record_failure();
        assert_eq!(breaker.state(), CircuitState::Closed); // below min_samples

        // Move past the window so the old failures fall out.
        advance(&clock, Duration::from_secs(5));
        breaker.record_success();
        breaker.record_success();
        breaker.record_success();
        // 1 failure / 4 samples = 0.25 < 0.5: must not trip.
        breaker.record_failure();
        assert_eq!(breaker.state(), CircuitState::Closed);
    }

    #[test]
    fn failure_rate_half_open_success_resets_window() {
        let (mut breaker, clock) = rate_breaker(0.5, 10, 2);
        breaker.record_failure();
        breaker.record_failure();
        assert_eq!(breaker.state(), CircuitState::Open);

        advance(&clock, Duration::from_secs(2));
        assert!(breaker.allow_request());
        breaker.record_success();
        assert_eq!(breaker.state(), CircuitState::Closed);

        // The window was reset: a single new failure is below min_samples.
        breaker.record_failure();
        assert_eq!(breaker.state(), CircuitState::Closed);
    }

    #[test]
    fn shared_breaker_exposes_state_through_shared_handles() {
        let (breaker, clock) =
            CircuitBreaker::new(2, Duration::from_secs(2), 1).with_manual_clock();
        let shared = SharedCircuitBreaker::new(breaker);
        let other_handle = shared.clone();

        assert!(shared.allow_request());
        shared.record_failure();
        other_handle.record_failure();

        // Both handles observe the same breaker.
        assert_eq!(shared.state(), CircuitState::Open);
        assert_eq!(other_handle.state(), CircuitState::Open);
        assert!(!shared.allow_request());

        advance(&clock, Duration::from_secs(2));
        assert!(other_handle.allow_request());
        other_handle.record_success();
        assert_eq!(shared.state(), CircuitState::Closed);
    }

    #[test]
    fn shared_breaker_concurrent_smoke() {
        let shared = SharedCircuitBreaker::from_config(CircuitBreakerConfig {
            open_cooldown: Duration::from_millis(5),
            half_open_max_probes: 2,
            trip_policy: TripPolicy::FailureRate {
                failure_rate_threshold: 0.5,
                window_secs: 2,
                min_samples: 10,
            },
        });

        let mut handles = Vec::new();
        for worker in 0..8 {
            let breaker = shared.clone();
            handles.push(std::thread::spawn(move || {
                for i in 0..500 {
                    if breaker.allow_request() {
                        if (worker + i) % 3 == 0 {
                            breaker.record_failure();
                        } else {
                            breaker.record_success();
                        }
                    }
                    let _ = breaker.state();
                }
            }));
        }
        for handle in handles {
            handle.join().expect("worker thread must not panic");
        }

        // The breaker is still in a coherent, usable state.
        let state = shared.state();
        assert!(matches!(
            state,
            CircuitState::Closed | CircuitState::Open | CircuitState::HalfOpen
        ));
    }
}
