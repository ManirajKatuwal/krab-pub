use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CircuitState {
    Closed,
    Open,
    HalfOpen,
}

#[derive(Debug, Clone)]
pub struct CircuitBreaker {
    failure_threshold: u32,
    open_cooldown: Duration,
    half_open_max_probes: u32,
    state: CircuitState,
    consecutive_failures: u32,
    opened_at: Option<Instant>,
    half_open_probes: u32,
}

impl CircuitBreaker {
    pub fn new(failure_threshold: u32, open_cooldown: Duration, half_open_max_probes: u32) -> Self {
        Self {
            failure_threshold,
            open_cooldown,
            half_open_max_probes,
            state: CircuitState::Closed,
            consecutive_failures: 0,
            opened_at: None,
            half_open_probes: 0,
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
                    if opened_at.elapsed() >= self.open_cooldown {
                        self.state = CircuitState::HalfOpen;
                        self.half_open_probes = 0;
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
        self.consecutive_failures = 0;
        self.half_open_probes = 0;
        self.opened_at = None;
        self.state = CircuitState::Closed;
    }

    pub fn record_failure(&mut self) {
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);

        match self.state {
            CircuitState::Closed => {
                if self.consecutive_failures >= self.failure_threshold {
                    self.state = CircuitState::Open;
                    self.opened_at = Some(Instant::now());
                }
            }
            CircuitState::HalfOpen => {
                self.state = CircuitState::Open;
                self.opened_at = Some(Instant::now());
            }
            CircuitState::Open => {}
        }
    }
}
