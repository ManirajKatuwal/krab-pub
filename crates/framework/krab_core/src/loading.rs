//! # Loading States
//!
//! Convention-based loading UI during route transitions and async data fetches.
//!
//! ## Usage
//!
//! ```rust
//! use krab_core::loading::{LoadingState, LoadingFallback, RouteTransition};
//!
//! // Define a loading fallback for a route
//! let fallback = LoadingFallback::new()
//!     .skeleton("<div class='skeleton'>Loading...</div>")
//!     .timeout(std::time::Duration::from_secs(5))
//!     .error_fallback("<div class='error'>Failed to load</div>");
//!
//! // Check transition state
//! let transition = RouteTransition::new("/blog/post-1");
//! assert!(transition.is_idle());
//! ```

use std::time::Duration;

// ── Loading State ───────────────────────────────────────────────────────────

/// The current state of a route or data fetch operation.
#[derive(Debug, Clone, PartialEq)]
pub enum LoadingState {
    /// No operation in progress.
    Idle,
    /// Content is loading; optional progress percentage (0.0–1.0).
    Loading { progress: Option<f64> },
    /// Content loaded successfully.
    Loaded,
    /// Loading failed.
    Error { message: String },
    /// Loading was cancelled (e.g., user navigated away).
    Cancelled,
}

impl LoadingState {
    /// Returns true if the state is `Idle`.
    pub fn is_idle(&self) -> bool {
        matches!(self, LoadingState::Idle)
    }

    /// Returns true if the state is `Loading`.
    pub fn is_loading(&self) -> bool {
        matches!(self, LoadingState::Loading { .. })
    }

    /// Returns true if the state is `Loaded`.
    pub fn is_loaded(&self) -> bool {
        matches!(self, LoadingState::Loaded)
    }

    /// Returns true if the state is `Error`.
    pub fn is_error(&self) -> bool {
        matches!(self, LoadingState::Error { .. })
    }

    /// Returns true if the state is `Cancelled`.
    pub fn is_cancelled(&self) -> bool {
        matches!(self, LoadingState::Cancelled)
    }
}

// ── Loading Fallback ────────────────────────────────────────────────────────

/// Defines the fallback UI to show while content is loading.
///
/// Each route or layout can provide its own fallback configuration.
#[derive(Debug, Clone)]
pub struct LoadingFallback {
    /// HTML skeleton to render during loading state.
    skeleton_html: String,
    /// Maximum time to show loading before transitioning to error.
    timeout: Duration,
    /// HTML to show when loading fails or times out.
    error_html: String,
    /// Minimum time to show loading (prevents flash of loading state).
    min_display_ms: u64,
}

impl Default for LoadingFallback {
    fn default() -> Self {
        Self {
            skeleton_html: default_skeleton_html().to_string(),
            timeout: Duration::from_secs(10),
            error_html: default_error_html().to_string(),
            min_display_ms: 200,
        }
    }
}

impl LoadingFallback {
    /// Create a new loading fallback with default skeleton and timeout.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the skeleton HTML to display during loading.
    pub fn skeleton(mut self, html: impl Into<String>) -> Self {
        self.skeleton_html = html.into();
        self
    }

    /// Set the loading timeout duration.
    pub fn timeout(mut self, duration: Duration) -> Self {
        self.timeout = duration;
        self
    }

    /// Set the error fallback HTML.
    pub fn error_fallback(mut self, html: impl Into<String>) -> Self {
        self.error_html = html.into();
        self
    }

    /// Set the minimum display time (prevents flash of loading state).
    pub fn min_display_ms(mut self, ms: u64) -> Self {
        self.min_display_ms = ms;
        self
    }

    /// Get the skeleton HTML.
    pub fn get_skeleton(&self) -> &str {
        &self.skeleton_html
    }

    /// Get the error fallback HTML.
    pub fn get_error_html(&self) -> &str {
        &self.error_html
    }

    /// Get the timeout duration.
    pub fn get_timeout(&self) -> Duration {
        self.timeout
    }

    /// Get the minimum display time in milliseconds.
    pub fn get_min_display_ms(&self) -> u64 {
        self.min_display_ms
    }

    /// Render the appropriate HTML based on current loading state.
    pub fn render(&self, state: &LoadingState) -> String {
        match state {
            LoadingState::Idle | LoadingState::Loaded => String::new(),
            LoadingState::Loading { progress } => {
                if let Some(pct) = progress {
                    format!(
                        "<div data-loading=\"true\" data-progress=\"{:.0}\">{}</div>",
                        pct * 100.0,
                        self.skeleton_html
                    )
                } else {
                    format!("<div data-loading=\"true\">{}</div>", self.skeleton_html)
                }
            }
            LoadingState::Error { message } => {
                format!(
                    "<div data-loading-error=\"true\" data-error-message=\"{}\">{}</div>",
                    html_escape(message),
                    self.error_html
                )
            }
            LoadingState::Cancelled => String::new(),
        }
    }
}

// ── Route Transition ────────────────────────────────────────────────────────

/// Tracks the transition state of a route navigation.
///
/// Used by the router to propagate loading/loaded/error states
/// to layouts and components.
#[derive(Debug, Clone)]
pub struct RouteTransition {
    /// The target path being navigated to.
    pub target_path: String,
    /// Current loading state.
    pub state: LoadingState,
    /// The fallback configuration for this transition.
    pub fallback: LoadingFallback,
    /// Whether this transition has been cancelled.
    cancelled: bool,
}

impl RouteTransition {
    /// Create a new idle transition for a path.
    pub fn new(path: impl Into<String>) -> Self {
        Self {
            target_path: path.into(),
            state: LoadingState::Idle,
            fallback: LoadingFallback::new(),
            cancelled: false,
        }
    }

    /// Create a new transition with a custom fallback.
    pub fn with_fallback(path: impl Into<String>, fallback: LoadingFallback) -> Self {
        Self {
            target_path: path.into(),
            state: LoadingState::Idle,
            fallback,
            cancelled: false,
        }
    }

    /// Start the loading state.
    pub fn start_loading(&mut self) {
        if !self.cancelled {
            self.state = LoadingState::Loading { progress: None };
        }
    }

    /// Update loading progress (0.0–1.0).
    pub fn set_progress(&mut self, progress: f64) {
        if !self.cancelled {
            self.state = LoadingState::Loading {
                progress: Some(progress.clamp(0.0, 1.0)),
            };
        }
    }

    /// Mark the transition as loaded.
    pub fn finish(&mut self) {
        if !self.cancelled {
            self.state = LoadingState::Loaded;
        }
    }

    /// Mark the transition as failed.
    pub fn fail(&mut self, message: impl Into<String>) {
        if !self.cancelled {
            self.state = LoadingState::Error {
                message: message.into(),
            };
        }
    }

    /// Cancel the transition (e.g., user navigated away).
    ///
    /// Once cancelled, further state changes are ignored.
    pub fn cancel(&mut self) {
        self.cancelled = true;
        self.state = LoadingState::Cancelled;
    }

    /// Returns true if the transition is idle (not started).
    pub fn is_idle(&self) -> bool {
        self.state.is_idle()
    }

    /// Returns true if the transition is loading.
    pub fn is_loading(&self) -> bool {
        self.state.is_loading()
    }

    /// Returns true if the transition was cancelled.
    pub fn is_cancelled(&self) -> bool {
        self.cancelled
    }

    /// Render the current loading UI.
    pub fn render(&self) -> String {
        self.fallback.render(&self.state)
    }
}

// ── Default HTML ────────────────────────────────────────────────────────────

fn default_skeleton_html() -> &'static str {
    r#"<div class="krab-loading" role="status" aria-label="Loading">
  <div class="krab-loading-spinner"></div>
  <style>
    .krab-loading { display:flex; justify-content:center; align-items:center; padding:2rem; }
    .krab-loading-spinner {
      width:2rem; height:2rem;
      border:3px solid rgba(0,0,0,0.1);
      border-top-color:#3b82f6;
      border-radius:50%;
      animation:krab-spin 0.8s ease-in-out infinite;
    }
    @keyframes krab-spin { to { transform:rotate(360deg); } }
  </style>
</div>"#
}

fn default_error_html() -> &'static str {
    r#"<div class="krab-error" role="alert">
  <p>Something went wrong. Please try again.</p>
  <style>
    .krab-error { padding:2rem; text-align:center; color:#dc2626; }
  </style>
</div>"#
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loading_state_transitions() {
        let state = LoadingState::Idle;
        assert!(state.is_idle());
        assert!(!state.is_loading());

        let state = LoadingState::Loading { progress: None };
        assert!(state.is_loading());
        assert!(!state.is_idle());

        let state = LoadingState::Loaded;
        assert!(state.is_loaded());

        let state = LoadingState::Error {
            message: "fail".to_string(),
        };
        assert!(state.is_error());

        let state = LoadingState::Cancelled;
        assert!(state.is_cancelled());
    }

    #[test]
    fn route_transition_lifecycle() {
        let mut transition = RouteTransition::new("/blog/post-1");
        assert!(transition.is_idle());

        transition.start_loading();
        assert!(transition.is_loading());
        assert!(!transition.render().is_empty());

        transition.set_progress(0.5);
        let html = transition.render();
        assert!(html.contains("data-progress=\"50\""));

        transition.finish();
        assert!(transition.state.is_loaded());
        assert!(transition.render().is_empty()); // Loaded = no fallback
    }

    #[test]
    fn route_transition_cancellation() {
        let mut transition = RouteTransition::new("/about");
        transition.start_loading();
        assert!(transition.is_loading());

        transition.cancel();
        assert!(transition.is_cancelled());

        // Further state changes ignored after cancellation
        transition.finish();
        assert!(transition.is_cancelled());
        assert!(!transition.state.is_loaded());
    }

    #[test]
    fn route_transition_failure() {
        let mut transition = RouteTransition::new("/error-page");
        transition.start_loading();
        transition.fail("Network error");

        assert!(transition.state.is_error());
        let html = transition.render();
        assert!(html.contains("data-loading-error"));
        assert!(html.contains("Network error"));
    }

    #[test]
    fn loading_fallback_custom_skeleton() {
        let fallback =
            LoadingFallback::new().skeleton("<div class='custom-loading'>Please wait...</div>");

        let state = LoadingState::Loading { progress: None };
        let html = fallback.render(&state);
        assert!(html.contains("custom-loading"));
        assert!(html.contains("Please wait..."));
    }

    #[test]
    fn loading_fallback_idle_and_loaded_render_nothing() {
        let fallback = LoadingFallback::new();
        assert!(fallback.render(&LoadingState::Idle).is_empty());
        assert!(fallback.render(&LoadingState::Loaded).is_empty());
        assert!(fallback.render(&LoadingState::Cancelled).is_empty());
    }

    #[test]
    fn loading_fallback_error_render() {
        let fallback = LoadingFallback::new().error_fallback("<p class='retry'>Click to retry</p>");

        let state = LoadingState::Error {
            message: "timeout".to_string(),
        };
        let html = fallback.render(&state);
        assert!(html.contains("Click to retry"));
        assert!(html.contains("data-error-message=\"timeout\""));
    }

    #[test]
    fn loading_fallback_timeout_config() {
        let fallback = LoadingFallback::new()
            .timeout(Duration::from_secs(30))
            .min_display_ms(500);

        assert_eq!(fallback.get_timeout(), Duration::from_secs(30));
        assert_eq!(fallback.get_min_display_ms(), 500);
    }
}
