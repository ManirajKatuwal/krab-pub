//! Chunked streaming SSR: byte-budgeted output, suspense-boundary markers,
//! and flush timing.
//!
//! Two halves, gated differently:
//!
//! - The **marker vocabulary** — [`SuspenseState`], [`SuspenseMarker`] and
//!   [`is_finalized_ssr_snapshot`] — is pure string parsing and compiles on
//!   every target. It was reachable from `wasm32` in `0.4.0`, when this module
//!   was exported unconditionally, and it stays reachable: a browser-side crate
//!   that parses `<!--krab:suspense:*-->` markers keeps compiling.
//! - The **streaming writer** — everything from [`StreamTelemetry`] down — is
//!   `#[cfg(not(target_arch = "wasm32"))]`. `ChunkedStreamWriter` times its
//!   flushes with `std::time::Instant`, and `Instant::now()` compiles for
//!   `wasm32-unknown-unknown` but panics when called, so an ungated writer
//!   shipped a live panic into the island bundle. It is also dead payload there:
//!   ADR 0009 records that streaming has no client half.
//!
//! The gate used to sit on the `pub mod` in `lib.rs`, which took the parsing
//! half off `wasm32` along with the writer and turned a dead-code removal into
//! a breaking change for anyone parsing markers in the browser. Anything added
//! below the writer's gate may assume a server clock; anything above it may not.

#[cfg(not(target_arch = "wasm32"))]
use crate::Render;
use std::collections::HashMap;
#[cfg(not(target_arch = "wasm32"))]
use std::time::Instant;

/// Lifecycle state of a streaming SSR suspense boundary.
///
/// Part of the server-side streaming protocol. The only client-side consumer
/// of these markers does not exist yet — ADR 0009 records that streaming has
/// no client half — so nothing in the browser reacts to a `Pending` boundary
/// being later resolved. It is public only because [`ChunkedStreamWriter::write_suspense_marker`]
/// takes it; do not treat it as a documented hook for client-side swap-in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuspenseState {
    Pending,
    Resolved,
    Error,
}

impl SuspenseState {
    // Only the writer serialises states; on `wasm32` there is no writer.
    #[cfg(not(target_arch = "wasm32"))]
    fn as_str(self) -> &'static str {
        match self {
            SuspenseState::Pending => "pending",
            SuspenseState::Resolved => "resolved",
            SuspenseState::Error => "error",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "pending" => Some(Self::Pending),
            "resolved" => Some(Self::Resolved),
            "error" => Some(Self::Error),
            _ => None,
        }
    }
}

/// A parsed `<!--krab:suspense:{id}:{state}-->` marker.
///
/// **Deprecated in 0.5.0, removed in 0.6.0.** The markers this parses are part
/// of a server-only streaming protocol: ADR 0009 records that streaming has no
/// client half, so nothing in the browser consumes them and there is no stable
/// meaning for a downstream crate to build on. The one real use — deciding
/// whether a rendered snapshot has every boundary resolved and is therefore
/// safe to cache — is now [`is_finalized_ssr_snapshot`].
///
/// Kept public through 0.5.x because it shipped in the 0.4.0 public API. See
/// `docs/reference/api.md`.
#[deprecated(
    since = "0.5.0",
    note = "use `is_finalized_ssr_snapshot` to test a rendered snapshot; this parses a \
            server-only streaming marker with no browser counterpart (ADR 0009) and is \
            removed in 0.6.0"
)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuspenseMarker {
    pub boundary_id: String,
    pub state: SuspenseState,
}

#[allow(deprecated)]
impl SuspenseMarker {
    /// Parse from either full marker comment (`<!--krab:suspense:...-->`) or raw body (`krab:suspense:...`).
    pub fn parse(input: &str) -> Option<Self> {
        let trimmed = input.trim();
        let body = trimmed
            .strip_prefix("<!--")
            .and_then(|v| v.strip_suffix("-->"))
            .unwrap_or(trimmed);

        let mut parts = body.split(':');
        let prefix = parts.next()?;
        let kind = parts.next()?;
        let boundary_id = parts.next()?.trim();
        let state_raw = parts.next()?.trim();

        if prefix != "krab" || kind != "suspense" || boundary_id.is_empty() {
            return None;
        }

        let state = SuspenseState::parse(state_raw)?;
        Some(Self {
            boundary_id: boundary_id.to_string(),
            state,
        })
    }
}

/// Whether an SSR snapshot has every suspense boundary resolved, so it is safe
/// to cache without serving a half-rendered page.
///
/// A streamed render may flush a `Pending` boundary before its `Resolved` (or
/// `Error`) marker; if that partial HTML were cached it would always be served
/// and the page would be stuck showing fallbacks. This scans the rendered
/// output and reports whether each boundary's `pending` count is exactly
/// matched by its `resolved + error` count. A snapshot with no suspense markers
/// counts as finalized — nothing is waiting on a boundary.
// `SuspenseMarker` is deprecated for downstream callers but is still the parser
// this helper is built on, so the use is allowed here rather than duplicated.
#[allow(deprecated)]
pub fn is_finalized_ssr_snapshot(html: &str) -> bool {
    let mut boundary_state: HashMap<String, (usize, usize, usize)> = HashMap::new();

    for segment in html.split("<!--").skip(1) {
        let Some(comment_end) = segment.find("-->") else {
            continue;
        };
        let marker_raw = format!("<!--{}-->", &segment[..comment_end]);
        let Some(marker) = SuspenseMarker::parse(&marker_raw) else {
            continue;
        };

        let counts = boundary_state
            .entry(marker.boundary_id)
            .or_insert((0usize, 0usize, 0usize));
        match marker.state {
            SuspenseState::Pending => counts.0 += 1,
            SuspenseState::Resolved => counts.1 += 1,
            SuspenseState::Error => counts.2 += 1,
        }
    }

    if boundary_state.is_empty() {
        return true;
    }

    boundary_state
        .values()
        .all(|(pending, resolved, error)| *pending > 0 && *pending == (*resolved + *error))
}

/// Streaming SSR telemetry snapshot for performance budgets and regressions.
#[derive(Debug, Clone)]
#[cfg(not(target_arch = "wasm32"))]
pub struct StreamTelemetry {
    pub ttfb_ms: Option<u128>,
    pub first_visible_chunk_ms: Option<u128>,
    pub full_stream_complete_ms: Option<u128>,
    pub emitted_bytes: usize,
    pub flush_count: usize,
    pub suspense_marker_count: usize,
    /// Number of boundary transitions by `boundary_id`.
    pub boundary_events: HashMap<String, usize>,
    pub budget_limit_bytes: Option<usize>,
    pub budget_exceeded: bool,
    pub stream_cancelled: bool,
    pub cancel_reason: Option<String>,
}

/// Result of consuming a [`ChunkedStreamWriter`] via [`finish`](ChunkedStreamWriter::finish).
///
/// Carries the emitted chunks together with the terminal stream flags so
/// callers can tell a complete render apart from one that was truncated by
/// the byte budget or cancelled mid-flight.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg(not(target_arch = "wasm32"))]
pub struct FinishedStream {
    /// Emitted chunks, in order.
    pub chunks: Vec<String>,
    /// True if at least one write was dropped because the byte budget was hit.
    pub budget_exceeded: bool,
    /// True if the stream was cancelled before completion.
    pub cancelled: bool,
}

#[cfg(not(target_arch = "wasm32"))]
impl FinishedStream {
    /// Emitted chunks, in order.
    pub fn chunks(&self) -> &[String] {
        &self.chunks
    }

    /// Consume, returning the emitted chunks.
    pub fn into_chunks(self) -> Vec<String> {
        self.chunks
    }

    /// Concatenate all chunks into a single string.
    pub fn concat(&self) -> String {
        self.chunks.concat()
    }

    /// True if at least one write was dropped because the byte budget was hit.
    pub fn budget_exceeded(&self) -> bool {
        self.budget_exceeded
    }

    /// True if the stream was cancelled before completion.
    pub fn cancelled(&self) -> bool {
        self.cancelled
    }

    /// True if the stream finished without truncation or cancellation.
    pub fn is_complete(&self) -> bool {
        !self.budget_exceeded && !self.cancelled
    }
}

#[derive(Debug, Clone)]
#[cfg(not(target_arch = "wasm32"))]
pub struct ChunkedStreamWriter {
    chunk_size: usize,
    flush_threshold: usize,
    pending: String,
    chunks: Vec<String>,
    flush_count: usize,
    created_at: Instant,
    first_write_at: Option<Instant>,
    first_flush_at: Option<Instant>,
    emitted_bytes: usize,
    suspense_marker_count: usize,
    boundary_events: HashMap<String, usize>,
    max_total_bytes: Option<usize>,
    budget_exceeded: bool,
    stream_cancelled: bool,
    cancel_reason: Option<String>,
}

#[cfg(not(target_arch = "wasm32"))]
impl Default for ChunkedStreamWriter {
    fn default() -> Self {
        Self::new(1024, 4096)
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl ChunkedStreamWriter {
    pub fn new(chunk_size: usize, flush_threshold: usize) -> Self {
        Self {
            chunk_size: chunk_size.max(128),
            flush_threshold: flush_threshold.max(chunk_size.max(128)),
            pending: String::new(),
            chunks: Vec::new(),
            flush_count: 0,
            created_at: Instant::now(),
            first_write_at: None,
            first_flush_at: None,
            emitted_bytes: 0,
            suspense_marker_count: 0,
            boundary_events: HashMap::new(),
            max_total_bytes: None,
            budget_exceeded: false,
            stream_cancelled: false,
            cancel_reason: None,
        }
    }

    /// Set a maximum per-request render budget in bytes.
    ///
    /// Once the budget is exceeded, subsequent writes are rejected —
    /// [`write`](Self::write) returns `false` — and `budget_exceeded` is
    /// surfaced via [`telemetry_snapshot`](Self::telemetry_snapshot) and
    /// [`finish`](Self::finish).
    pub fn with_max_total_bytes(mut self, max_total_bytes: usize) -> Self {
        self.max_total_bytes = Some(max_total_bytes.max(1));
        self
    }

    /// Configure/override the maximum per-request render budget in bytes.
    pub fn set_max_total_bytes(&mut self, max_total_bytes: usize) {
        self.max_total_bytes = Some(max_total_bytes.max(1));
    }

    /// Write raw output into the stream.
    ///
    /// Returns `true` if the input was accepted, `false` if it was dropped
    /// because the stream is cancelled or the byte budget would be exceeded.
    /// The first budget-driven drop emits a `render_budget_exceeded` warning.
    #[must_use = "a false return means the output was dropped (budget exceeded or stream cancelled)"]
    pub fn write(&mut self, input: &str) -> bool {
        if self.budget_exceeded || self.stream_cancelled {
            return false;
        }

        if let Some(limit) = self.max_total_bytes {
            let projected = self.emitted_bytes + self.pending.len() + input.len();
            if projected > limit {
                self.budget_exceeded = true;
                tracing::warn!(
                    emitted_bytes = self.emitted_bytes,
                    pending_bytes = self.pending.len(),
                    dropped_bytes = input.len(),
                    limit,
                    "render_budget_exceeded"
                );
                return false;
            }
        }

        if self.first_write_at.is_none() {
            self.first_write_at = Some(Instant::now());
        }

        self.pending.push_str(input);
        self.flush_if_ready();
        true
    }

    /// Write a suspense boundary marker.
    ///
    /// Returns `true` if the marker was accepted. Marker and boundary
    /// counters are only incremented for markers that actually reached the
    /// stream — a marker dropped by the budget or a cancellation is not
    /// counted in telemetry.
    #[must_use = "a false return means the marker was dropped (budget exceeded or stream cancelled)"]
    pub fn write_suspense_marker(&mut self, boundary_id: &str, state: SuspenseState) -> bool {
        let accepted = self.write(&format!(
            "<!--krab:suspense:{}:{}-->",
            boundary_id,
            state.as_str()
        ));
        if accepted {
            self.suspense_marker_count += 1;
            *self
                .boundary_events
                .entry(boundary_id.to_string())
                .or_insert(0) += 1;
        }
        accepted
    }

    pub fn flush(&mut self) {
        if self.pending.is_empty() || self.stream_cancelled {
            return;
        }

        while self.pending.len() > self.chunk_size {
            let split_at = nearest_char_boundary(&self.pending, self.chunk_size);
            let chunk = self.pending[..split_at].to_string();
            self.emitted_bytes += chunk.len();
            self.chunks.push(chunk);
            self.pending = self.pending[split_at..].to_string();
        }

        if !self.pending.is_empty() {
            let chunk = std::mem::take(&mut self.pending);
            self.emitted_bytes += chunk.len();
            self.chunks.push(chunk);
        }

        if self.first_flush_at.is_none() {
            self.first_flush_at = Some(Instant::now());
        }

        self.flush_count += 1;
    }

    /// Flush any pending output and consume the writer.
    ///
    /// The returned [`FinishedStream`] carries the emitted chunks plus the
    /// terminal `budget_exceeded` / `cancelled` flags, so truncation is
    /// visible to the caller instead of silently producing a shorter stream.
    pub fn finish(mut self) -> FinishedStream {
        self.flush();
        FinishedStream {
            budget_exceeded: self.budget_exceeded,
            cancelled: self.stream_cancelled,
            chunks: self.chunks,
        }
    }

    pub fn flush_count(&self) -> usize {
        self.flush_count
    }

    pub fn telemetry_snapshot(&self) -> StreamTelemetry {
        let now = Instant::now();
        StreamTelemetry {
            ttfb_ms: self
                .first_flush_at
                .map(|t| t.duration_since(self.created_at).as_millis()),
            first_visible_chunk_ms: self
                .first_flush_at
                .map(|t| t.duration_since(self.created_at).as_millis()),
            full_stream_complete_ms: if self.flush_count > 0 {
                Some(now.duration_since(self.created_at).as_millis())
            } else {
                None
            },
            emitted_bytes: self.emitted_bytes,
            flush_count: self.flush_count,
            suspense_marker_count: self.suspense_marker_count,
            boundary_events: self.boundary_events.clone(),
            budget_limit_bytes: self.max_total_bytes,
            budget_exceeded: self.budget_exceeded,
            stream_cancelled: self.stream_cancelled,
            cancel_reason: self.cancel_reason.clone(),
        }
    }

    /// Cancels stream emission for current request.
    /// Further writes/flushes are ignored.
    pub fn cancel(&mut self, reason: impl Into<String>) {
        if !self.pending.is_empty() {
            self.flush();
        }
        self.stream_cancelled = true;
        self.cancel_reason = Some(reason.into());
    }

    pub fn is_cancelled(&self) -> bool {
        self.stream_cancelled
    }

    fn flush_if_ready(&mut self) {
        if self.pending.len() >= self.flush_threshold {
            self.flush();
        }
    }
}

/// Render into the chunk stream. Returns `true` if the rendered output was
/// accepted, `false` if it was dropped (budget exceeded or stream cancelled).
#[cfg(not(target_arch = "wasm32"))]
pub fn render_to_chunk_stream(renderable: &impl Render, writer: &mut ChunkedStreamWriter) -> bool {
    writer.write(&renderable.render())
}

#[cfg(not(target_arch = "wasm32"))]
fn nearest_char_boundary(s: &str, target: usize) -> usize {
    if target >= s.len() {
        return s.len();
    }
    let mut i = target;
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

// The writer tests need the server clock; the marker tests could run anywhere
// but `krab_core` has no `wasm32` test runner, so one gate covers both.
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use crate::{annotate_hydration_tree, Element, Node};

    #[test]
    fn chunk_writer_splits_and_flushes() {
        let mut writer = ChunkedStreamWriter::new(128, 256);
        assert!(writer.write(&"a".repeat(300)));

        let finished = writer.finish();
        assert!(finished.chunks.len() >= 3);
        assert_eq!(finished.concat(), "a".repeat(300));
        assert!(finished.is_complete());
        assert!(!finished.budget_exceeded());
        assert!(!finished.cancelled());
    }

    #[test]
    fn finalized_snapshot_with_all_boundaries_resolved() {
        let html = concat!(
            "<html><body>",
            "<!--krab:suspense:home:pending-->",
            "<div data-krab-hydration=\"home\">fallback</div>",
            "<!--krab:suspense:home:resolved-->",
            "</body></html>"
        );
        assert!(is_finalized_ssr_snapshot(html));
    }

    #[test]
    fn unfinalized_snapshot_without_resolution() {
        let html = concat!(
            "<html><body>",
            "<!--krab:suspense:home:pending-->",
            "<div>fallback</div>",
            "</body></html>"
        );
        assert!(!is_finalized_ssr_snapshot(html));
    }

    #[test]
    fn unfinalized_snapshot_with_error_boundary_matches_resolution() {
        // An Error boundary is terminal, so pending == resolved + error still
        // holds and the page may be cached.
        let html = concat!(
            "<!--krab:suspense:home:pending-->",
            "<!--krab:suspense:home:error-->"
        );
        assert!(is_finalized_ssr_snapshot(html));
    }

    #[test]
    fn snapshot_with_no_markers_is_finalized() {
        assert!(is_finalized_ssr_snapshot("<html><body>plain</body></html>"));
        assert!(is_finalized_ssr_snapshot(""));
    }

    #[test]
    fn suspense_markers_are_hydration_compatible_comments() {
        let mut writer = ChunkedStreamWriter::new(32, 64);
        let _ = writer.write_suspense_marker("home-data", SuspenseState::Pending);
        let _ = writer.write("<div data-krab-hydration=\"home-data\">fallback</div>");
        let _ = writer.write_suspense_marker("home-data", SuspenseState::Resolved);
        let html = writer.finish().concat();

        assert!(html.contains("<!--krab:suspense:home-data:pending-->"));
        assert!(html.contains("data-krab-hydration=\"home-data\""));
        assert!(html.contains("<!--krab:suspense:home-data:resolved-->"));
    }

    #[test]
    #[allow(deprecated)]
    fn suspense_marker_parser_handles_comment_and_raw_body() {
        let parsed_comment = SuspenseMarker::parse("<!--krab:suspense:home:pending-->")
            .expect("comment marker should parse");
        assert_eq!(parsed_comment.boundary_id, "home");
        assert_eq!(parsed_comment.state, SuspenseState::Pending);

        let parsed_raw = SuspenseMarker::parse("krab:suspense:profile:resolved")
            .expect("raw marker should parse");
        assert_eq!(parsed_raw.boundary_id, "profile");
        assert_eq!(parsed_raw.state, SuspenseState::Resolved);
    }

    #[test]
    fn hydration_node_markers_survive_chunk_stream_rendering() {
        let renderable = annotate_hydration_tree(
            Node::Element(Element {
                tag: "section".to_string(),
                attributes: vec![],
                children: vec![Node::Element(Element {
                    tag: "div".to_string(),
                    attributes: vec![],
                    children: vec![Node::Text("streamed".to_string())],
                    events: vec![],
                })],
                events: vec![],
            }),
            "stream-home:1",
        );
        let mut writer = ChunkedStreamWriter::new(16, 16);

        render_to_chunk_stream(&renderable, &mut writer);

        let html = writer.finish().concat();
        assert!(html.contains("data-krab-node-id=\"stream-home:1/0\""));
        assert!(html.contains("data-krab-node-id=\"stream-home:1/0.0\""));
        assert!(html.contains(">streamed</div>"));
    }

    #[test]
    fn backpressure_flushes_when_threshold_reached() {
        let mut writer = ChunkedStreamWriter::new(128, 128);
        let _ = writer.write("hello");
        assert_eq!(writer.flush_count(), 0);
        let _ = writer.write(&"x".repeat(130));
        assert!(writer.flush_count() >= 1);
    }

    #[test]
    fn telemetry_snapshot_tracks_stream_events() {
        let mut writer = ChunkedStreamWriter::new(64, 64);
        let _ = writer.write("<!DOCTYPE html>");
        let _ = writer.write_suspense_marker("home", SuspenseState::Pending);
        let _ = writer.write("<div>hello</div>");
        let _ = writer.write_suspense_marker("home", SuspenseState::Resolved);
        writer.flush();

        let telemetry = writer.telemetry_snapshot();
        assert!(telemetry.flush_count >= 1);
        assert_eq!(telemetry.suspense_marker_count, 2);
        assert_eq!(telemetry.boundary_events.get("home"), Some(&2));
        assert!(telemetry.ttfb_ms.is_some());
        assert_eq!(telemetry.budget_limit_bytes, None);
        assert!(!telemetry.budget_exceeded);
    }

    #[test]
    fn budget_guard_blocks_writes_once_exceeded() {
        let mut writer = ChunkedStreamWriter::new(128, 256).with_max_total_bytes(10);
        assert!(writer.write("12345"));
        assert!(writer.write("67890"));
        assert!(!writer.write("EXTRA")); // must be blocked by budget
        writer.flush();
        let telemetry = writer.telemetry_snapshot();
        let finished = writer.finish();
        assert_eq!(finished.concat(), "1234567890");
        assert!(finished.budget_exceeded);
        assert!(!finished.cancelled);
        assert!(!finished.is_complete());

        assert_eq!(telemetry.budget_limit_bytes, Some(10));
        assert!(telemetry.budget_exceeded);
        assert!(!telemetry.stream_cancelled);
    }

    #[test]
    fn suspense_marker_dropped_by_budget_is_not_counted() {
        // Budget fits the pending marker exactly; nothing more.
        let pending_marker = "<!--krab:suspense:home:pending-->";
        let mut writer =
            ChunkedStreamWriter::new(128, 256).with_max_total_bytes(pending_marker.len());

        assert!(writer.write_suspense_marker("home", SuspenseState::Pending));
        assert!(!writer.write_suspense_marker("home", SuspenseState::Resolved));

        let telemetry = writer.telemetry_snapshot();
        assert_eq!(telemetry.suspense_marker_count, 1);
        assert_eq!(telemetry.boundary_events.get("home"), Some(&1));

        let finished = writer.finish();
        assert!(finished.budget_exceeded);
        assert_eq!(finished.concat(), pending_marker);
    }

    #[test]
    fn suspense_marker_after_cancel_is_not_counted() {
        let mut writer = ChunkedStreamWriter::new(64, 64);
        assert!(writer.write_suspense_marker("home", SuspenseState::Pending));
        writer.cancel("client disconnected");
        assert!(!writer.write_suspense_marker("home", SuspenseState::Resolved));

        let telemetry = writer.telemetry_snapshot();
        assert_eq!(telemetry.suspense_marker_count, 1);
        assert_eq!(telemetry.boundary_events.get("home"), Some(&1));
    }

    #[test]
    fn budget_guard_can_be_set_after_initialization() {
        let mut writer = ChunkedStreamWriter::new(128, 256);
        writer.set_max_total_bytes(4);
        assert!(writer.write("abcd"));
        assert!(!writer.write("e")); // exceeds budget
        writer.flush();
        let telemetry = writer.telemetry_snapshot();
        let finished = writer.finish();
        assert!(finished.budget_exceeded);
        assert_eq!(finished.concat(), "abcd");

        assert_eq!(telemetry.budget_limit_bytes, Some(4));
        assert!(telemetry.budget_exceeded);
        assert!(!telemetry.stream_cancelled);
    }

    #[test]
    fn cancellation_stops_future_writes_and_exposes_reason() {
        let mut writer = ChunkedStreamWriter::new(64, 64);
        assert!(writer.write("<html>"));
        writer.cancel("client disconnected");
        assert!(!writer.write("<body>should-not-appear</body>"));
        writer.flush();

        let telemetry = writer.telemetry_snapshot();
        assert!(writer.is_cancelled());
        assert!(telemetry.stream_cancelled);
        assert_eq!(
            telemetry.cancel_reason.as_deref(),
            Some("client disconnected")
        );

        let finished = writer.finish();
        assert!(finished.cancelled);
        assert!(!finished.budget_exceeded);
        assert!(!finished.is_complete());
        assert_eq!(finished.concat(), "<html>");
    }
}
