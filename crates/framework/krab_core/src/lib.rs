use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

pub mod action;
pub mod config;
pub mod control_flow;
#[cfg(feature = "auth")]
pub mod credentials;
pub mod error_boundary;
#[cfg(feature = "graphql")]
pub mod graphql;
#[cfg(feature = "grpc-semantics")]
pub mod grpc_semantics;
pub mod resource;

/// Deprecated alias for [`grpc_semantics`].
///
/// The name promised a transport this crate does not have. Kept for one minor
/// version per the breaking-change policy in `RELEASE_POLICY.md`; deprecated in
/// `0.2.0`, so removable no earlier than `0.3.0`. See ADR 0007.
#[cfg(feature = "grpc-semantics")]
#[deprecated(
    since = "0.2.0",
    note = "renamed to `grpc_semantics`: this module provides gRPC status-code and \
            timeout-header semantics for a gateway, not a gRPC transport. \
            Update the feature name too: `grpc` -> `grpc-semantics`."
)]
pub use grpc_semantics as grpc;
pub mod head;
#[cfg(feature = "rest")]
pub mod http_error;
pub mod i18n;
pub mod image;
// Server-side: ISR caches rendered pages, and now does so through
// `store::DistributedStore`, which needs `tokio`. Neither is meaningful in a
// browser bundle.
#[cfg(not(target_arch = "wasm32"))]
pub mod isr;
pub mod layout;
pub mod loading;
pub mod protocol;
pub mod render_policy;
pub mod render_stream;
pub mod resilience;
pub mod service_contract;
pub mod signal;
pub mod style_scope;

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

pub const HYDRATION_NODE_ID_ATTR: &str = "data-krab-node-id";
const ISLAND_NAME_ATTR: &str = "data-island";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HydrationNodeMarker {
    pub boundary_id: String,
    pub path: String,
}

impl HydrationNodeMarker {
    pub fn new(boundary_id: impl Into<String>, path: impl Into<String>) -> Self {
        Self {
            boundary_id: boundary_id.into(),
            path: path.into(),
        }
    }

    pub fn root(boundary_id: &str) -> Self {
        Self::new(boundary_id, "0")
    }

    pub fn child(&self, child_index: usize) -> Self {
        Self::new(
            &self.boundary_id,
            child_hydration_path(&self.path, child_index),
        )
    }

    pub fn parse(input: &str) -> Option<Self> {
        let (boundary_id, path) = input.split_once('/')?;
        let boundary_id = boundary_id.trim();
        let path = path.trim();
        if boundary_id.is_empty() || path.is_empty() {
            return None;
        }
        Some(Self::new(boundary_id, path))
    }

    pub fn as_attr_value(&self) -> String {
        format!("{}/{}", self.boundary_id, self.path)
    }
}

pub fn next_hydration_boundary_id(scope: &str) -> String {
    let id = NEXT_HYDRATION_BOUNDARY_ID.fetch_add(1, Ordering::Relaxed);
    format!("{scope}:{id}")
}

pub fn annotate_hydration_tree(node: Node, boundary_id: &str) -> Node {
    annotate_hydration_tree_with_marker(node, &HydrationNodeMarker::root(boundary_id))
}

fn annotate_hydration_tree_with_marker(node: Node, marker: &HydrationNodeMarker) -> Node {
    match node {
        Node::Element(mut element) => {
            if !element
                .attributes
                .iter()
                .any(|attr| attr.name == HYDRATION_NODE_ID_ATTR)
            {
                element.attributes.push(Attribute::new(
                    HYDRATION_NODE_ID_ATTR.to_string(),
                    marker.as_attr_value(),
                ));
            }

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

fn child_hydration_path(parent_path: &str, child_index: usize) -> String {
    if parent_path.is_empty() {
        child_index.to_string()
    } else {
        format!("{parent_path}.{child_index}")
    }
}

pub trait Render {
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

#[derive(Clone)]
pub enum Node {
    Element(Element),
    Text(String),
    Fragment(Vec<Node>),
    Dynamic(Rc<dyn Fn() -> Node>),
}

// Remove Debug derive from Node because Fn is not Debug
impl std::fmt::Debug for Node {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Node::Element(e) => f.debug_tuple("Element").field(e).finish(),
            Node::Text(t) => f.debug_tuple("Text").field(t).finish(),
            Node::Fragment(nodes) => f.debug_tuple("Fragment").field(nodes).finish(),
            Node::Dynamic(_) => f.debug_tuple("Dynamic").finish(),
        }
    }
}

impl Render for Node {
    fn render(&self) -> String {
        match self {
            Node::Element(el) => el.render(),
            Node::Text(text) => escape_html_text(text),
            Node::Fragment(nodes) => nodes.iter().map(|n| n.render()).collect(),
            Node::Dynamic(f) => f().render(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Element {
    pub tag: String,
    pub attributes: Vec<Attribute>,
    pub children: Vec<Node>,
    pub events: Vec<EventListener>,
}

#[derive(Clone)]
pub struct EventListener {
    pub name: String,
    #[cfg(feature = "web")]
    pub callback: Rc<dyn Fn(web_sys::Event)>,
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
            .map(|a| format!(" {}=\"{}\"", a.name, escape_html_attr(&a.value)))
            .collect::<String>();
        let children = self.children.iter().map(|c| c.render()).collect::<String>();

        // Note: events are not rendered to HTML string

        if self.children.is_empty() {
            match self.tag.as_str() {
                "area" | "base" | "br" | "col" | "embed" | "hr" | "img" | "input" | "link"
                | "meta" | "param" | "source" | "track" | "wbr" => {
                    format!("<{}{}/>", self.tag, attrs)
                }
                _ => format!("<{}{}></{}>", self.tag, attrs, self.tag),
            }
        } else {
            format!("<{}{}>{}</{}>", self.tag, attrs, children, self.tag)
        }
    }
}

#[derive(Debug, Clone)]
pub struct Attribute {
    pub name: String,
    pub value: String,
}

impl Attribute {
    pub fn new(name: String, value: String) -> Self {
        Self { name, value }
    }
}

pub trait IntoNode {
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
