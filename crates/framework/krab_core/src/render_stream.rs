use crate::Render;
use std::collections::HashMap;
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuspenseState {
    Pending,
    Resolved,
    Error,
}

impl SuspenseState {
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

/// Parsed suspense marker emitted by SSR streaming output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuspenseMarker {
    pub boundary_id: String,
    pub state: SuspenseState,
}

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

/// Streaming SSR telemetry snapshot for performance budgets and regressions.
#[derive(Debug, Clone)]
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

#[derive(Debug, Clone)]
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

impl Default for ChunkedStreamWriter {
    fn default() -> Self {
        Self::new(1024, 4096)
    }
}

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
    /// Once the budget is exceeded, subsequent writes are ignored and
    /// `budget_exceeded` is surfaced via [`telemetry_snapshot`](Self::telemetry_snapshot).
    pub fn with_max_total_bytes(mut self, max_total_bytes: usize) -> Self {
        self.max_total_bytes = Some(max_total_bytes.max(1));
        self
    }

    /// Configure/override the maximum per-request render budget in bytes.
    pub fn set_max_total_bytes(&mut self, max_total_bytes: usize) {
        self.max_total_bytes = Some(max_total_bytes.max(1));
    }

    pub fn write(&mut self, input: &str) {
        if self.budget_exceeded || self.stream_cancelled {
            return;
        }

        if let Some(limit) = self.max_total_bytes {
            let projected = self.emitted_bytes + self.pending.len() + input.len();
            if projected > limit {
                self.budget_exceeded = true;
                return;
            }
        }

        if self.first_write_at.is_none() {
            self.first_write_at = Some(Instant::now());
        }

        self.pending.push_str(input);
        self.flush_if_ready();
    }

    pub fn write_suspense_marker(&mut self, boundary_id: &str, state: SuspenseState) {
        self.suspense_marker_count += 1;
        *self
            .boundary_events
            .entry(boundary_id.to_string())
            .or_insert(0) += 1;
        self.write(&format!(
            "<!--krab:suspense:{}:{}-->",
            boundary_id,
            state.as_str()
        ));
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

    pub fn finish(mut self) -> Vec<String> {
        self.flush();
        self.chunks
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

pub fn render_to_chunk_stream(renderable: &impl Render, writer: &mut ChunkedStreamWriter) {
    writer.write(&renderable.render());
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{annotate_hydration_tree, Element, Node};

    #[test]
    fn chunk_writer_splits_and_flushes() {
        let mut writer = ChunkedStreamWriter::new(128, 256);
        writer.write(&"a".repeat(300));

        let chunks = writer.finish();
        assert!(chunks.len() >= 3);
        assert_eq!(chunks.concat(), "a".repeat(300));
    }

    #[test]
    fn suspense_markers_are_hydration_compatible_comments() {
        let mut writer = ChunkedStreamWriter::new(32, 64);
        writer.write_suspense_marker("home-data", SuspenseState::Pending);
        writer.write("<div data-krab-hydration=\"home-data\">fallback</div>");
        writer.write_suspense_marker("home-data", SuspenseState::Resolved);
        let html = writer.finish().concat();

        assert!(html.contains("<!--krab:suspense:home-data:pending-->"));
        assert!(html.contains("data-krab-hydration=\"home-data\""));
        assert!(html.contains("<!--krab:suspense:home-data:resolved-->"));
    }

    #[test]
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
        writer.write("hello");
        assert_eq!(writer.flush_count(), 0);
        writer.write(&"x".repeat(130));
        assert!(writer.flush_count() >= 1);
    }

    #[test]
    fn telemetry_snapshot_tracks_stream_events() {
        let mut writer = ChunkedStreamWriter::new(64, 64);
        writer.write("<!DOCTYPE html>");
        writer.write_suspense_marker("home", SuspenseState::Pending);
        writer.write("<div>hello</div>");
        writer.write_suspense_marker("home", SuspenseState::Resolved);
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
        writer.write("12345");
        writer.write("67890");
        writer.write("EXTRA"); // must be blocked by budget
        writer.flush();
        let telemetry = writer.telemetry_snapshot();
        let html = writer.finish().concat();
        assert_eq!(html, "1234567890");

        assert_eq!(telemetry.budget_limit_bytes, Some(10));
        assert!(telemetry.budget_exceeded);
        assert!(!telemetry.stream_cancelled);
    }

    #[test]
    fn budget_guard_can_be_set_after_initialization() {
        let mut writer = ChunkedStreamWriter::new(128, 256);
        writer.set_max_total_bytes(4);
        writer.write("abcd");
        writer.write("e"); // exceeds budget
        writer.flush();
        let telemetry = writer.telemetry_snapshot();
        let html = writer.finish().concat();
        assert_eq!(html, "abcd");

        assert_eq!(telemetry.budget_limit_bytes, Some(4));
        assert!(telemetry.budget_exceeded);
        assert!(!telemetry.stream_cancelled);
    }

    #[test]
    fn cancellation_stops_future_writes_and_exposes_reason() {
        let mut writer = ChunkedStreamWriter::new(64, 64);
        writer.write("<html>");
        writer.cancel("client disconnected");
        writer.write("<body>should-not-appear</body>");
        writer.flush();

        let telemetry = writer.telemetry_snapshot();
        assert!(writer.is_cancelled());
        assert!(telemetry.stream_cancelled);
        assert_eq!(
            telemetry.cancel_reason.as_deref(),
            Some("client disconnected")
        );

        let html = writer.finish().concat();
        assert_eq!(html, "<html>");
    }
}
