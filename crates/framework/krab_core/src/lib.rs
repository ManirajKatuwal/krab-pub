//! Shared runtime for the Krab full-stack web framework.
//!
//! The crate root holds the view tree that `view!` builds and SSR renders —
//! [`Node`], [`Element`], [`Attribute`], [`EventListener`], the [`Render`] and
//! [`IntoNode`] traits — and the hydration markers that let the browser
//! runtime (`krab_client`) match server-rendered HTML back to it. The modules
//! cover the rest of a service: configuration and secret sourcing
//! ([`config`]), the HTTP stack, database and migration governance, the
//! shared store, telemetry, resilience, signals, render policy, ISR, i18n,
//! WebSockets and server functions.
//!
//! # Features
//!
//! There are **no default features**; much of the crate is behind one:
//!
//! | Feature | Enables |
//! |---|---|
//! | `rest` | The axum HTTP stack: `http` and its auth, errors, headers, runtime and security modules; server functions |
//! | `graphql` | `graphql`, on `async-graphql` |
//! | `grpc-semantics` | `grpc_semantics`: gRPC status codes and `grpc-timeout` parsing for a gateway — not a gRPC transport |
//! | `auth` | `credentials`: Argon2id password hashing |
//! | `db-postgres` | `db` with the Postgres runtime and migration governance |
//! | `db-sqlite` | `db` driver selection with the SQLite driver |
//! | `redis-store` | The Redis-backed shared store |
//! | `web` | Browser bindings for the `wasm32` island runtime |
//!
//! [`Node`] is `!Send` (its `Dynamic` variant holds an `Rc`): build and render
//! it inside synchronous code, and do not hold one across an `.await`.

#![warn(missing_docs)]

use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

pub mod action;
pub mod config;
pub mod control_flow;
#[cfg(feature = "auth")]
pub mod credentials;
// Compiled unconditionally: the CSRF wire-contract names are shared between
// the `rest` server half and the `web` wasm client half, which never build
// together, so the constants cannot live behind either feature gate.
pub mod csrf;
pub mod error_boundary;
#[cfg(feature = "graphql")]
pub mod graphql;
#[cfg(feature = "grpc-semantics")]
pub mod grpc_semantics;
pub mod head;
#[cfg(feature = "rest")]
pub mod http_error;
pub mod i18n;
/// `<picture>` markup with AVIF/WebP `<source>`s.
///
/// **Deprecated in 0.6.0, removed in 0.7.0.** Nothing in Krab generates the
/// image variants it points at, and by default it emits AVIF and WebP sources,
/// so a page using it as documented references files that do not exist — and a
/// browser that picks a 404ing `<source>` shows a broken image rather than
/// falling back to the `<img>`. Write the `<picture>` element with `view!`,
/// pointing at variants your asset pipeline actually produces.
#[deprecated(
    since = "0.6.0",
    note = "emits <source> elements for image variants nothing generates; write the <picture> with view! instead. Removed in 0.7.0"
)]
pub mod image;
pub mod resource;
// Server-side: ISR caches rendered pages, and now does so through
// `store::DistributedStore`, which needs `tokio`. Neither is meaningful in a
// browser bundle.
#[cfg(not(target_arch = "wasm32"))]
pub mod isr;
pub mod layout;
pub mod protocol;
pub mod render_policy;
// Compiled on every target, but only half of it: the suspense-marker parsing
// (`SuspenseState`, `is_finalized_ssr_snapshot`) is plain
// string handling and was reachable from `wasm32` in 0.4.0. The streaming
// side is gated *inside* the module — `ChunkedStreamWriter` times its flushes
// with `std::time::Instant`, which on `wasm32-unknown-unknown` compiles and
// then panics at runtime, and the progressive renderer (`render_to_stream`,
// ADR 0017, which supersedes ADR 0009's "streaming is deferred" clause) is
// server-only, not on `wasm32` (its response stream also needs feature
// `rest`): its client half is the plain-JS swap
// runtime served at `/_krab/stream.js`, not WASM. Gating the whole `pub mod`
// here, as 0.5.0 first did, would have taken the parser off the browser with
// the writer and broken code that worked.
pub mod render_stream;
pub mod resilience;
pub mod service_contract;
pub mod signal;
/// Component-scoped CSS class rewriting.
///
/// **Deprecated in 0.6.0, removed in 0.7.0** (each item carries the
/// attribute). It is not connected to `view!`, so nothing applies the class
/// it generates, and it derives scope ids with `DefaultHasher`, whose output
/// is not stable across Rust releases — a stylesheet built by one toolchain
/// would not match markup rendered by another.
pub mod style_scope;
// `<Suspense>` boundaries: the runtime behind the `view!` tag (ADR 0016).
pub mod suspense;

#[cfg(not(target_arch = "wasm32"))]
pub mod ws;

// Available to both halves of a server function, not just the server half.
//
// This was `#[cfg(feature = "rest")]`, which gated out the entire module for a
// browser build — including `call_server_fn`, which is what the `#[server]`
// macro's wasm32 stub calls. The module's own internals already branch on
// `not(feature = "rest")` and `target_arch = "wasm32"`, so it was written to
// compile without `rest`; the module declaration made that code unreachable and
// the client half of `#[server]` could never build.
#[cfg(any(feature = "rest", feature = "web"))]
pub mod server_fn;

#[cfg(not(target_arch = "wasm32"))]
pub mod telemetry;

fn escape_html_attr(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn escape_html_text(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// True for the elements whose content the HTML tokenizer reads as raw text:
/// `<script>` and `<style>`. Their children are not entity-decoded by a
/// browser, so they must not be entity-encoded on the way out either — see
/// [`escape_raw_text`]. The match is ASCII case-insensitive, as the
/// tokenizer's is.
pub(crate) fn is_raw_text_element(tag: &str) -> bool {
    tag.eq_ignore_ascii_case("script") || tag.eq_ignore_ascii_case("style")
}

/// Make `content` safe to emit verbatim inside a raw-text element `tag`
/// (`script` or `style`).
///
/// Entity escaping is wrong here — the browser does not decode `&gt;` inside a
/// `<script>`, so `=>` would arrive as `=&gt;` — but emitting the text as-is is
/// unsafe, because the tokenizer ends the element at the first `</script`
/// (or `</style`), in any letter case, wherever it appears: inside a string, a
/// comment, a regular expression. So only the sequences the tokenizer acts on
/// are rewritten, in forms that mean the same thing to the script or style
/// engine where they can legitimately appear:
///
/// - `</tag` becomes `<\/tag`. Inside a JavaScript string, template literal or
///   regex, and inside a JSON string, `\/` is `/`; inside CSS it only occurs in
///   strings and comments, where the backslash is harmless.
/// - In a `script`, `<!--` becomes `\u003C!--`. `<!--` followed by `<script`
///   puts the tokenizer into the "double escaped" state, in which the real
///   `</script>` no longer closes the element and the rest of the page is
///   swallowed as script. `\u003C` is `<` inside a JavaScript or JSON string,
///   which is the only place the sequence legitimately appears.
///
/// Both matches are ASCII case-insensitive and preserve the original casing of
/// the tag name.
pub(crate) fn escape_raw_text(tag: &str, content: &str) -> String {
    let tag_lower = tag.to_ascii_lowercase();
    let close = format!("</{tag_lower}");
    let is_script = tag_lower == "script";
    // ASCII lowercasing is byte-for-byte, so indices into `lower` are valid
    // indices into `content`.
    let lower = content.to_ascii_lowercase();
    let mut out = String::with_capacity(content.len());
    let mut cursor = 0;
    loop {
        let next_close = lower[cursor..].find(&close).map(|rel| (cursor + rel, true));
        let next_comment = if is_script {
            lower[cursor..]
                .find("<!--")
                .map(|rel| (cursor + rel, false))
        } else {
            None
        };
        let next = match (next_close, next_comment) {
            (Some(a), Some(b)) => Some(if a.0 <= b.0 { a } else { b }),
            (a, b) => a.or(b),
        };
        let Some((at, is_close)) = next else {
            break;
        };
        out.push_str(&content[cursor..at]);
        if is_close {
            out.push_str("<\\/");
            // Preserve the original casing of the tag name.
            out.push_str(&content[at + 2..at + close.len()]);
            cursor = at + close.len();
        } else {
            out.push_str("\\u003C!--");
            cursor = at + 4;
        }
    }
    out.push_str(&content[cursor..]);
    out
}

/// The text content a raw-text element's children render to, before
/// [`escape_raw_text`]. Text is taken verbatim; a nested element renders as its
/// markup, which is what a browser would then read as script or style text.
fn raw_text_content(node: &Node, out: &mut String) {
    match node {
        Node::Text(text) => out.push_str(text),
        // A comment inside raw text would be text, not a comment; it carries
        // no content, so it contributes nothing.
        Node::Comment(_) => {}
        Node::Element(element) => out.push_str(&element.render()),
        Node::Fragment(nodes) => nodes.iter().for_each(|n| raw_text_content(n, out)),
        Node::Dynamic(f) => raw_text_content(&f(), out),
    }
}

/// Make `text` safe as the body of an HTML comment.
///
/// A comment ends at the first `-->` (and, in error recovery, `--!>`), so
/// every `--` is split with a space, and a leading `>` or `->` and a trailing
/// `-` — which would fuse with the delimiters — are padded. The framework's
/// own markers never contain these; the rewrite exists for comments built
/// from runtime data.
pub(crate) fn sanitize_comment(text: &str) -> String {
    let mut out = text.to_string();
    while out.contains("--") {
        out = out.replace("--", "- -");
    }
    if out.starts_with('>') || out.starts_with("->") {
        out.insert(0, ' ');
    }
    if out.ends_with('-') || out.ends_with("<!") {
        out.push(' ');
    }
    out
}

/// True when `name` can be emitted as a raw HTML attribute name.
///
/// Attribute *values* are escaped on the way out, but a name is interpolated
/// into the tag itself, where escaping is not enough: [`escape_html_attr`]
/// leaves spaces and `=` untouched, so a name such as `x onload=alert(1)`
/// would still introduce an event handler. Names are therefore validated
/// against the HTML name grammar and dropped when they fail it — the same
/// disposition a browser gives a name it cannot parse.
///
/// `view!` builds names from literal tokens, so this only bites callers who
/// construct [`Attribute`] directly with a runtime-supplied name.
pub(crate) fn is_valid_attr_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' || c == ':' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':' | '.'))
}

#[cfg(not(target_arch = "wasm32"))]
pub mod service;

// Driver selection is available under either driver; the Postgres runtime
// inside is gated on `db-postgres`.
#[cfg(any(feature = "db-postgres", feature = "db-sqlite"))]
pub mod db;

// `UserRepository` is the port applications implement per driver, so it is
// not Postgres-specific.
#[cfg(any(feature = "db-postgres", feature = "db-sqlite"))]
pub mod repository;

#[cfg(feature = "rest")]
pub mod http;
#[cfg(feature = "rest")]
pub mod http_auth;
#[cfg(feature = "rest")]
pub mod http_headers;
#[cfg(feature = "rest")]
mod http_observability;
#[cfg(feature = "rest")]
mod http_protocol;
#[cfg(feature = "rest")]
pub mod http_runtime;
#[cfg(feature = "rest")]
pub mod http_security;
/// Remote JSON Web Key Sets for OIDC providers.
#[cfg(feature = "rest")]
pub(crate) mod jwks;

#[cfg(feature = "rest")]
pub mod static_assets;
// Gated on the target, not on `rest`. A shared key-value store has nothing to
// do with having an HTTP surface, and `isr` — which is not feature-gated —
// depends on it. It needs `tokio`, which is a non-wasm dependency, hence the
// target gate rather than no gate at all.
#[cfg(not(target_arch = "wasm32"))]
pub mod store;

#[cfg(all(feature = "rest", test))]
mod auth_tests;

#[cfg(all(feature = "db-postgres", test))]
mod db_tests;

#[cfg(all(feature = "rest", test))]
mod api_tests;

#[cfg(all(feature = "rest", test))]
mod server_fn_tests;

#[cfg(all(feature = "rest", test))]
mod protocol_tests;

static NEXT_HYDRATION_BOUNDARY_ID: AtomicU64 = AtomicU64::new(1);

/// Attribute that carries an element's [`HydrationNodeMarker`] in rendered
/// HTML: `data-krab-node-id="{boundary_id}/{path}"`.
pub const HYDRATION_NODE_ID_ATTR: &str = "data-krab-node-id";
const ISLAND_NAME_ATTR: &str = "data-island";

/// Identifies one element inside a hydration boundary, so the browser can
/// match server-rendered markup to the view tree it rebuilds.
///
/// Written into the HTML as [`HYDRATION_NODE_ID_ATTR`] with the value
/// `{boundary_id}/{path}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HydrationNodeMarker {
    /// The island instance this element belongs to, as produced by
    /// [`next_hydration_boundary_id`].
    pub boundary_id: String,
    /// The element's position below the boundary root: child indices joined
    /// with `.`, the root being `0` (so `0.2` is the root's third child).
    pub path: String,
}

impl HydrationNodeMarker {
    /// A marker from its two parts, taken as given.
    pub fn new(boundary_id: impl Into<String>, path: impl Into<String>) -> Self {
        Self {
            boundary_id: boundary_id.into(),
            path: path.into(),
        }
    }

    /// The marker of a boundary's root element (path `0`).
    pub fn root(boundary_id: &str) -> Self {
        Self::new(boundary_id, "0")
    }

    /// The marker of this element's child at `child_index` (zero-based).
    pub fn child(&self, child_index: usize) -> Self {
        Self::new(
            &self.boundary_id,
            child_hydration_path(&self.path, child_index),
        )
    }

    /// Parses a `{boundary_id}/{path}` attribute value, splitting at the
    /// first `/` and trimming both parts. `None` without a `/` or when either
    /// part is empty. The path's shape is not validated.
    pub fn parse(input: &str) -> Option<Self> {
        let (boundary_id, path) = input.split_once('/')?;
        let boundary_id = boundary_id.trim();
        let path = path.trim();
        if boundary_id.is_empty() || path.is_empty() {
            return None;
        }
        Some(Self::new(boundary_id, path))
    }

    /// The attribute value, `{boundary_id}/{path}`; the inverse of
    /// [`HydrationNodeMarker::parse`].
    pub fn as_attr_value(&self) -> String {
        format!("{}/{}", self.boundary_id, self.path)
    }
}

/// A fresh boundary id, `{scope}:{n}`, with `n` from a process-wide counter.
/// The `#[island]` macro calls it once per rendered island, with the island's
/// name as `scope`, so ids are unique within a process but not across
/// processes or restarts.
pub fn next_hydration_boundary_id(scope: &str) -> String {
    let id = NEXT_HYDRATION_BOUNDARY_ID.fetch_add(1, Ordering::Relaxed);
    format!("{scope}:{id}")
}

/// Stamps a [`HydrationNodeMarker`] on every element of `node` for the
/// boundary `boundary_id`, starting from the root marker.
///
/// An element that already carries [`HYDRATION_NODE_ID_ATTR`] keeps its
/// value. The walk does not descend into nested islands (elements with a
/// `data-island` attribute), which own their own boundary, and wraps
/// `Dynamic` nodes so their output is annotated when rendered. The server
/// (`#[island]`) and the browser runtime both apply it, which is what makes
/// the markers agree.
pub fn annotate_hydration_tree(node: Node, boundary_id: &str) -> Node {
    annotate_hydration_tree_with_marker(node, &HydrationNodeMarker::root(boundary_id))
}

fn annotate_hydration_tree_with_marker(node: Node, marker: &HydrationNodeMarker) -> Node {
    match node {
        Node::Element(mut element) => {
            stamp_hydration_marker_if_absent(&mut element, || marker.as_attr_value());

            if !element
                .attributes
                .iter()
                .any(|attr| attr.name == ISLAND_NAME_ATTR)
            {
                element.children = annotate_hydration_children(element.children, marker);
            }

            Node::Element(element)
        }
        Node::Fragment(children) => Node::Fragment(annotate_hydration_children(children, marker)),
        Node::Dynamic(factory) => {
            let marker = marker.clone();
            Node::Dynamic(Rc::new(move || {
                annotate_hydration_tree_with_marker(factory(), &marker)
            }))
        }
        Node::Text(text) => Node::Text(text),
        Node::Comment(text) => Node::Comment(text),
    }
}

fn annotate_hydration_children(
    children: Vec<Node>,
    parent_marker: &HydrationNodeMarker,
) -> Vec<Node> {
    children
        .into_iter()
        .enumerate()
        .map(|(index, child)| {
            annotate_hydration_tree_with_marker(child, &parent_marker.child(index))
        })
        .collect()
}

/// The `{parent}.{index}` path scheme shared by hydration annotation and
/// `<For>`'s fragment-row keying (`control_flow::with_key`). One definition, so
/// the two walkers cannot drift apart.
pub(crate) fn child_hydration_path(parent_path: &str, child_index: usize) -> String {
    if parent_path.is_empty() {
        child_index.to_string()
    } else {
        format!("{parent_path}.{child_index}")
    }
}

/// Stamp `value()` onto `element` as [`HYDRATION_NODE_ID_ATTR`] unless the
/// marker is already present — an existing marker always wins. Both
/// `annotate_hydration_tree` and `<For>`'s key stamping go through here, which
/// is what lets a key set by `with_key` survive hydration annotation instead of
/// being overwritten by a positional path (and vice versa).
///
/// `value` is a closure so callers that build the attribute value on demand pay
/// for it only when the stamp is actually applied.
pub(crate) fn stamp_hydration_marker_if_absent(
    element: &mut Element,
    value: impl FnOnce() -> String,
) {
    if !element
        .attributes
        .iter()
        .any(|attr| attr.name == HYDRATION_NODE_ID_ATTR)
    {
        element
            .attributes
            .push(Attribute::new(HYDRATION_NODE_ID_ATTR.to_string(), value()));
    }
}

/// Conversion to an HTML string.
///
/// For [`Node`] and [`Element`] this is SSR: text is escaped, attribute
/// values are escaped, and event listeners are not rendered. The exception is
/// the text inside `<script>` and `<style>`, which a browser reads as raw text:
/// it is emitted verbatim, with only the sequences that would end the element
/// early (`</script`, `</style`, and `<!--` in a script) neutralised. The `String`,
/// `&str` and `&T: Display` implementations return the text **unescaped** —
/// it is treated as markup.
pub trait Render {
    /// The HTML for `self`.
    fn render(&self) -> String;
}

impl Render for String {
    fn render(&self) -> String {
        self.clone()
    }
}

impl Render for &str {
    fn render(&self) -> String {
        self.to_string()
    }
}

impl<T: std::fmt::Display> Render for &T {
    fn render(&self) -> String {
        self.to_string()
    }
}

/// A node in the view tree that `view!` builds.
///
/// `!Send`: the `Dynamic` variant holds an `Rc` (see the crate docs).
#[derive(Clone)]
pub enum Node {
    /// An HTML element.
    Element(Element),
    /// Text content; HTML-escaped when rendered, except inside `<script>` and
    /// `<style>`, whose raw text is emitted verbatim with breakout protection.
    Text(String),
    /// A sequence of sibling nodes with no wrapper element.
    Fragment(Vec<Node>),
    /// A node computed on each render by calling the closure — how reactive
    /// content is expressed.
    Dynamic(Rc<dyn Fn() -> Node>),
    /// An HTML comment, `<!--text-->`. Rendered with any sequence that would
    /// end the comment early (`--`) neutralised.
    ///
    /// Added in 0.6.0 for `<Suspense>`'s boundary markers
    /// (`<!--krab:suspense:{id}:pending-->`, ADR 0016), which have to be part
    /// of the tree so the browser's hydration walk finds them where the server
    /// put them. A `match` on `Node` that listed every variant needs an arm
    /// for it.
    Comment(String),
}

// Remove Debug derive from Node because Fn is not Debug
impl std::fmt::Debug for Node {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Node::Element(e) => f.debug_tuple("Element").field(e).finish(),
            Node::Text(t) => f.debug_tuple("Text").field(t).finish(),
            Node::Comment(t) => f.debug_tuple("Comment").field(t).finish(),
            Node::Fragment(nodes) => f.debug_tuple("Fragment").field(nodes).finish(),
            Node::Dynamic(_) => f.debug_tuple("Dynamic").finish(),
        }
    }
}

/// An empty fragment, which renders nothing.
///
/// Exists so a component's props struct can `#[derive(Default)]` while holding
/// `children: Node`, which is what `view!`'s `..` (fill omitted props from
/// `Default`) needs. See ADR 0013.
impl Default for Node {
    fn default() -> Self {
        Node::Fragment(Vec::new())
    }
}

impl Render for Node {
    fn render(&self) -> String {
        match self {
            Node::Element(el) => el.render(),
            Node::Text(text) => escape_html_text(text),
            Node::Comment(text) => format!("<!--{}-->", sanitize_comment(text)),
            Node::Fragment(nodes) => nodes.iter().map(|n| n.render()).collect(),
            Node::Dynamic(f) => f().render(),
        }
    }
}

/// An HTML element in the view tree.
#[derive(Debug, Clone)]
pub struct Element {
    /// The tag name, for example `div`. Void elements (`br`, `img`, `input`,
    /// ...) render without a closing tag.
    pub tag: String,
    /// Attributes, rendered in order. An attribute whose name is not a valid
    /// HTML attribute name is dropped at render time.
    pub attributes: Vec<Attribute>,
    /// Child nodes, rendered in order.
    pub children: Vec<Node>,
    /// Event listeners, attached during browser hydration. Never rendered to
    /// HTML.
    pub events: Vec<EventListener>,
}

/// An event handler attached to an [`Element`] (`on:click={...}` in
/// `view!`). Only meaningful in the browser.
#[derive(Clone)]
pub struct EventListener {
    /// The DOM event name without the `on:` prefix, for example `click`.
    pub name: String,
    /// The handler, called with the DOM event.
    #[cfg(feature = "web")]
    pub callback: Rc<dyn Fn(web_sys::Event)>,
    /// Placeholder without the `web` feature: server-side there is nothing
    /// to call.
    #[cfg(not(feature = "web"))]
    pub callback: (),
}

impl std::fmt::Debug for EventListener {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventListener")
            .field("name", &self.name)
            .finish()
    }
}

impl Render for Element {
    fn render(&self) -> String {
        let attrs = self
            .attributes
            .iter()
            .filter(|a| is_valid_attr_name(&a.name))
            // A dynamic attribute renders its value as of now, and is omitted
            // when its source returns `None` — the same shape the browser's
            // effect maintains afterwards, so hydration starts in agreement.
            .filter_map(|a| {
                a.current_value()
                    .map(|value| format!(" {}=\"{}\"", a.name, escape_html_attr(&value)))
            })
            .collect::<String>();
        // `<script>` and `<style>` are raw-text elements: a browser does not
        // decode entities inside them, so HTML-escaping their text turned
        // every `=>` into `=&gt;` and broke the script. They get breakout
        // protection instead of escaping (see `escape_raw_text`).
        let children = if is_raw_text_element(&self.tag) {
            let mut raw = String::new();
            self.children
                .iter()
                .for_each(|c| raw_text_content(c, &mut raw));
            escape_raw_text(&self.tag, &raw)
        } else {
            self.children.iter().map(|c| c.render()).collect::<String>()
        };

        // Note: events are not rendered to HTML string

        match self.tag.as_str() {
            // A void element has no closing tag, children or not. Giving one
            // children is a caller error, and `<br>x</br>` is malformed markup;
            // a browser parses that source as `<br>` followed by the text, so
            // that is what is emitted here rather than silently dropping the
            // children on the floor.
            "area" | "base" | "br" | "col" | "embed" | "hr" | "img" | "input" | "link" | "meta"
            | "param" | "source" | "track" | "wbr" => {
                format!("<{}{}/>{}", self.tag, attrs, children)
            }
            _ if self.children.is_empty() => format!("<{}{}></{}>", self.tag, attrs, self.tag),
            _ => format!("<{}{}>{}</{}>", self.tag, attrs, children, self.tag),
        }
    }
}

/// The source of a reactive attribute: called on the server at render time and
/// in the browser inside an effect, each time the signals it reads change.
/// `None` means "attribute absent". See [`Attribute::dynamic`].
pub type DynamicAttributeValue = Rc<dyn Fn() -> Option<String>>;

/// A `name="value"` attribute of an [`Element`].
///
/// Either **static** — `value` is the value, `dynamic` is `None` — or
/// **dynamic**: `dynamic` holds a closure that is re-evaluated whenever the
/// signals it reads change, and `value` is unused (empty). `view!` builds a
/// dynamic attribute from a closure literal, `disabled={move || busy.get()}`;
/// see [`Attribute::dynamic`] and ADR 0015.
///
/// Construct with [`Attribute::new`] or [`Attribute::dynamic`]. A struct
/// literal must now name `dynamic` too — the field was added in 0.6.0.
#[derive(Clone)]
pub struct Attribute {
    /// The attribute name. Must be a valid HTML attribute name or the
    /// attribute is dropped at render time — relevant only when it is built
    /// from runtime data rather than by `view!`.
    pub name: String,
    /// The value, unescaped; it is escaped when rendered. Empty, and ignored,
    /// when `dynamic` is set: read [`Attribute::current_value`] instead.
    pub value: String,
    /// The reactive source of a dynamic attribute, or `None` for a static one.
    ///
    /// Holding an `Rc` keeps [`Node`] `!Send`, which it already was.
    pub dynamic: Option<DynamicAttributeValue>,
}

impl std::fmt::Debug for Attribute {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut debug = f.debug_struct("Attribute");
        debug.field("name", &self.name);
        match &self.dynamic {
            // Not evaluated: formatting must not read signals, which would
            // subscribe whatever effect happens to be running.
            Some(_) => debug.field("value", &"<dynamic>"),
            None => debug.field("value", &self.value),
        };
        debug.finish()
    }
}

impl Attribute {
    /// A static attribute from its name and value.
    pub fn new(name: String, value: String) -> Self {
        Self {
            name,
            value,
            dynamic: None,
        }
    }

    /// A reactive attribute whose value is computed by `source`.
    ///
    /// On the server `source` is called once, when the element renders; a
    /// `None` result (for example `false` from a boolean source) omits the
    /// attribute. In the browser (`krab_client` with `web`) it runs inside an
    /// effect bound to the element, which sets or removes the attribute each
    /// time a signal it reads changes. Hydration evaluates it once against the
    /// server-rendered element and writes only when the DOM disagrees.
    ///
    /// `source` may return anything implementing [`IntoAttributeValue`]:
    /// strings and numbers set the value, `bool` makes a boolean attribute
    /// (`true` → present and empty, `false` → absent), and `Option<T>` is
    /// absent on `None`.
    ///
    /// `view!` calls this for an attribute whose value is a closure literal:
    /// `<button disabled={move || busy.get()}>`.
    pub fn dynamic<F, V>(name: impl Into<String>, source: F) -> Self
    where
        F: Fn() -> V + 'static,
        V: IntoAttributeValue,
    {
        Self {
            name: name.into(),
            value: String::new(),
            dynamic: Some(Rc::new(move || source().into_attribute_value())),
        }
    }

    /// Whether this attribute is reactive.
    pub fn is_dynamic(&self) -> bool {
        self.dynamic.is_some()
    }

    /// The value the attribute has right now: `value` for a static attribute,
    /// the source's current result for a dynamic one (`None` = absent).
    ///
    /// Calling this inside an effect subscribes the effect to whatever the
    /// source reads.
    pub fn current_value(&self) -> Option<String> {
        match &self.dynamic {
            Some(source) => source(),
            None => Some(self.value.clone()),
        }
    }
}

/// Conversion of a reactive attribute source's result into an attribute value;
/// `None` means the attribute is absent. See [`Attribute::dynamic`].
pub trait IntoAttributeValue {
    /// The attribute value, or `None` to omit the attribute.
    fn into_attribute_value(self) -> Option<String>;
}

impl IntoAttributeValue for String {
    fn into_attribute_value(self) -> Option<String> {
        Some(self)
    }
}

impl IntoAttributeValue for &str {
    fn into_attribute_value(self) -> Option<String> {
        Some(self.to_string())
    }
}

impl IntoAttributeValue for &String {
    fn into_attribute_value(self) -> Option<String> {
        Some(self.clone())
    }
}

impl IntoAttributeValue for std::borrow::Cow<'_, str> {
    fn into_attribute_value(self) -> Option<String> {
        Some(self.into_owned())
    }
}

/// A boolean attribute: `true` renders it present with an empty value (HTML
/// reads presence, not the value — `disabled="false"` is still disabled),
/// `false` omits it.
impl IntoAttributeValue for bool {
    fn into_attribute_value(self) -> Option<String> {
        self.then(String::new)
    }
}

impl<T: IntoAttributeValue> IntoAttributeValue for Option<T> {
    fn into_attribute_value(self) -> Option<String> {
        self.and_then(IntoAttributeValue::into_attribute_value)
    }
}

macro_rules! display_attribute_value {
    ($($t:ty),* $(,)?) => {
        $(
            impl IntoAttributeValue for $t {
                fn into_attribute_value(self) -> Option<String> {
                    Some(self.to_string())
                }
            }
        )*
    };
}

display_attribute_value!(
    char, i8, i16, i32, i64, i128, isize, u8, u16, u32, u64, u128, usize, f32, f64
);

/// Conversion into a [`Node`]; how expressions interpolated with `{...}` in
/// `view!` become part of the tree.
///
/// Strings and `i32`s become [`Node::Text`] (escaped when rendered), and a
/// `Fn() -> Node` closure becomes [`Node::Dynamic`].
pub trait IntoNode {
    /// Converts `self` into a node.
    fn into_node(self) -> Node;
}

impl IntoNode for Node {
    fn into_node(self) -> Node {
        self
    }
}

impl IntoNode for String {
    fn into_node(self) -> Node {
        Node::Text(self)
    }
}

impl IntoNode for &str {
    fn into_node(self) -> Node {
        Node::Text(self.to_string())
    }
}

impl IntoNode for i32 {
    fn into_node(self) -> Node {
        Node::Text(self.to_string())
    }
}

impl IntoNode for &i32 {
    fn into_node(self) -> Node {
        Node::Text(self.to_string())
    }
}

// Generic implementation for Closures?
// impl<F> IntoNode for F where F: Fn() -> Node + 'static { ... }
// This might conflict or requires boxing.
// Since we use Rc<dyn Fn() -> Node>, we can impl it.
impl<F> IntoNode for F
where
    F: Fn() -> Node + 'static,
{
    fn into_node(self) -> Node {
        Node::Dynamic(Rc::new(self))
    }
}

// Also support closures returning things that can be nodes?
// e.g. Fn() -> String.
// Rust doesn't support specialization well, so F: Fn() -> Node is safer.
// If the user returns String from closure, they might need to wrap it.

#[cfg(test)]
mod tests {
    use super::*;

    fn attr_value<'a>(node: &'a Node, name: &str) -> Option<&'a str> {
        let Node::Element(element) = node else {
            return None;
        };

        element
            .attributes
            .iter()
            .find(|attr| attr.name == name)
            .map(|attr| attr.value.as_str())
    }

    #[test]
    fn annotate_hydration_tree_assigns_stable_element_paths() {
        let node = Node::Element(Element {
            tag: "div".to_string(),
            attributes: vec![],
            children: vec![Node::Element(Element {
                tag: "span".to_string(),
                attributes: vec![],
                children: vec![Node::Text("hello".to_string())],
                events: vec![],
            })],
            events: vec![],
        });

        let annotated = annotate_hydration_tree(node, "boundary:1");

        let Node::Element(root) = annotated else {
            panic!("expected root element");
        };
        assert_eq!(
            root.attributes
                .iter()
                .find(|attr| attr.name == HYDRATION_NODE_ID_ATTR)
                .map(|attr| attr.value.as_str()),
            Some("boundary:1/0")
        );
        assert_eq!(
            root.children
                .first()
                .and_then(|child| attr_value(child, HYDRATION_NODE_ID_ATTR)),
            Some("boundary:1/0.0")
        );
    }

    #[test]
    fn hydration_node_marker_round_trips_and_builds_child_paths() {
        let marker = HydrationNodeMarker::root("profile:7");
        assert_eq!(marker.as_attr_value(), "profile:7/0");

        let child = marker.child(2);
        assert_eq!(child.as_attr_value(), "profile:7/0.2");

        let parsed = HydrationNodeMarker::parse("profile:7/0.2").expect("marker should parse");
        assert_eq!(parsed, child);
    }

    #[test]
    fn annotate_hydration_tree_does_not_descend_into_nested_island_wrappers() {
        let node = Node::Element(Element {
            tag: "section".to_string(),
            attributes: vec![],
            children: vec![Node::Element(Element {
                tag: "div".to_string(),
                attributes: vec![Attribute::new(
                    ISLAND_NAME_ATTR.to_string(),
                    "NestedIsland".to_string(),
                )],
                children: vec![Node::Element(Element {
                    tag: "span".to_string(),
                    attributes: vec![Attribute::new(
                        HYDRATION_NODE_ID_ATTR.to_string(),
                        "nested-boundary/0".to_string(),
                    )],
                    children: vec![],
                    events: vec![],
                })],
                events: vec![],
            })],
            events: vec![],
        });

        let annotated = annotate_hydration_tree(node, "outer:1");
        let Node::Element(root) = annotated else {
            panic!("expected root element");
        };
        let Some(Node::Element(nested_wrapper)) = root.children.first() else {
            panic!("expected nested island wrapper");
        };

        assert_eq!(
            nested_wrapper
                .attributes
                .iter()
                .find(|attr| attr.name == HYDRATION_NODE_ID_ATTR)
                .map(|attr| attr.value.as_str()),
            Some("outer:1/0.0")
        );
        assert_eq!(
            nested_wrapper
                .children
                .first()
                .and_then(|child| attr_value(child, HYDRATION_NODE_ID_ATTR)),
            Some("nested-boundary/0")
        );
    }
}

#[cfg(test)]
mod render_safety_tests {
    use super::*;

    fn el(tag: &str, attrs: Vec<Attribute>, children: Vec<Node>) -> Element {
        Element {
            tag: tag.to_string(),
            attributes: attrs,
            children,
            events: Vec::new(),
        }
    }

    fn attr(name: &str, value: &str) -> Attribute {
        Attribute::new(name.to_string(), value.to_string())
    }

    #[test]
    fn dynamic_attribute_renders_its_current_value() {
        let (label, set_label) = crate::signal::create_signal("first".to_string());
        let node = el(
            "div",
            vec![Attribute::dynamic("title", move || label.get())],
            vec![],
        );

        assert_eq!(node.render(), "<div title=\"first\"></div>");
        set_label.set("se\"cond".to_string());
        // Evaluated at render time, and escaped like any other value.
        assert_eq!(node.render(), "<div title=\"se&quot;cond\"></div>");
    }

    #[test]
    fn dynamic_attribute_returning_none_is_omitted() {
        let (busy, set_busy) = crate::signal::create_signal(false);
        let node = el(
            "button",
            vec![
                Attribute::dynamic("disabled", move || busy.get()),
                attr("class", "b"),
            ],
            vec![],
        );

        assert_eq!(node.render(), "<button class=\"b\"></button>");
        set_busy.set(true);
        assert_eq!(node.render(), "<button disabled=\"\" class=\"b\"></button>");
    }

    #[test]
    fn attribute_values_convert_by_type() {
        assert_eq!(true.into_attribute_value(), Some(String::new()));
        assert_eq!(false.into_attribute_value(), None);
        assert_eq!(Some(3u8).into_attribute_value(), Some("3".to_string()));
        assert_eq!(None::<&str>.into_attribute_value(), None);
        assert_eq!("x".into_attribute_value(), Some("x".to_string()));
    }

    #[test]
    fn static_and_dynamic_attributes_report_their_kind() {
        let fixed = attr("id", "a");
        let live = Attribute::dynamic("id", || "b");
        assert!(!fixed.is_dynamic());
        assert!(live.is_dynamic());
        assert_eq!(fixed.current_value().as_deref(), Some("a"));
        assert_eq!(live.current_value().as_deref(), Some("b"));
        assert!(format!("{live:?}").contains("<dynamic>"));
    }

    #[test]
    fn attribute_name_that_would_open_a_second_attribute_is_dropped() {
        // Escaping the name would not help: `escape_html_attr` leaves the space
        // and the `=` intact, so this would still render an event handler.
        let node = el(
            "div",
            vec![attr("x onload=alert(1)", "v"), attr("class", "ok")],
            vec![],
        );

        let html = node.render();

        assert!(
            !html.contains("onload"),
            "injected handler survived: {html}"
        );
        assert_eq!(html, "<div class=\"ok\"></div>");
    }

    #[test]
    fn ordinary_attribute_names_still_render() {
        let node = el(
            "div",
            vec![
                attr("data-krab-boundary-id", "counter/0"),
                attr("aria-label", "hi"),
                attr("xml:lang", "en"),
                attr("_private", "1"),
            ],
            vec![],
        );

        let html = node.render();

        assert!(html.contains("data-krab-boundary-id=\"counter/0\""));
        assert!(html.contains("aria-label=\"hi\""));
        assert!(html.contains("xml:lang=\"en\""));
        assert!(html.contains("_private=\"1\""));
    }

    #[test]
    fn attribute_values_are_still_escaped() {
        let node = el("div", vec![attr("title", "\"><script>x</script>")], vec![]);

        let html = node.render();

        assert!(!html.contains("<script>"), "unescaped value: {html}");
        assert!(html.contains("&quot;&gt;&lt;script&gt;"));
    }

    #[test]
    fn void_element_with_children_does_not_emit_a_closing_tag() {
        let node = el(
            "br",
            vec![],
            vec![Node::Text("after the break".to_string())],
        );

        let html = node.render();

        // `<br>x</br>` is malformed; a browser parses that source as `<br>`
        // followed by the text, so that is what is rendered.
        assert_eq!(html, "<br/>after the break");
        assert!(!html.contains("</br>"));
    }

    #[test]
    fn script_text_is_emitted_raw_not_entity_escaped() {
        let node = el(
            "script",
            vec![attr("type", "module")],
            vec![Node::Text(
                "const f = (a) => a && b < c; x > y;".to_string(),
            )],
        );

        assert_eq!(
            node.render(),
            "<script type=\"module\">const f = (a) => a && b < c; x > y;</script>"
        );
    }

    #[test]
    fn style_text_is_emitted_raw_not_entity_escaped() {
        let node = el(
            "style",
            vec![],
            vec![Node::Text(".a > .b { content: \"&\"; }".to_string())],
        );

        assert_eq!(node.render(), "<style>.a > .b { content: \"&\"; }</style>");
    }

    #[test]
    fn script_text_cannot_close_its_own_element() {
        let node = el(
            "script",
            vec![],
            vec![Node::Text(
                "let s = \"</ScRiPt><img src=x onerror=alert(1)>\";".to_string(),
            )],
        );

        let html = node.render();

        // Exactly one closing tag — the element's own.
        assert_eq!(html.to_ascii_lowercase().matches("</script").count(), 1);
        assert!(html.ends_with("</script>"));
        assert!(html.contains(r"<\/ScRiPt>"), "casing not preserved: {html}");
    }

    #[test]
    fn script_text_cannot_enter_the_double_escaped_state() {
        // `<!--<script>` would make the tokenizer skip the real `</script>`.
        let node = el(
            "script",
            vec![],
            vec![Node::Text("var s = \"<!--<script>\";".to_string())],
        );

        let html = node.render();

        assert!(!html.contains("<!--"), "{html}");
        assert!(html.contains("\\u003C!--"), "{html}");
    }

    #[test]
    fn style_text_cannot_close_its_own_element() {
        let node = el(
            "style",
            vec![],
            vec![Node::Text(
                "a{} </STYLE><script>alert(1)</script>".to_string(),
            )],
        );

        let html = node.render();

        assert_eq!(html.to_ascii_lowercase().matches("</style").count(), 1);
        assert!(html.contains(r"<\/STYLE>"), "{html}");
        // Only `</style` ends a style element; other markup is inert text.
        assert!(html.contains("<script>alert(1)</script>"));
    }

    #[test]
    fn raw_text_elements_match_their_tag_case_insensitively() {
        let node = el("SCRIPT", vec![], vec![Node::Text("a => b".to_string())]);
        assert_eq!(node.render(), "<SCRIPT>a => b</SCRIPT>");
    }

    #[test]
    fn text_outside_raw_text_elements_is_still_escaped() {
        let node = el("p", vec![], vec![Node::Text("a => b".to_string())]);
        assert_eq!(node.render(), "<p>a =&gt; b</p>");
    }

    #[test]
    fn void_and_normal_elements_without_children_are_unchanged() {
        assert_eq!(el("br", vec![], vec![]).render(), "<br/>");
        assert_eq!(el("div", vec![], vec![]).render(), "<div></div>");
    }
}
