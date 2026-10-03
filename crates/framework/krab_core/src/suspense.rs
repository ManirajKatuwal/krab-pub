//! Runtime behind `view!`'s `<Suspense>` tag.
//!
//! `<Suspense fallback={|| view! { <p>"Loading…"</p> }}>children</Suspense>`
//! expands to a [`suspense`] call. The children are built once, inside a scope
//! that provides a [`SuspenseContext`]; every [`Resource`](crate::resource::Resource)
//! created there registers with it. The boundary is a [`Node::Dynamic`] that
//! shows the fallback while any registered source is pending and the children
//! otherwise — so it switches reactively in the browser through the same
//! effect machinery as `<Show>`. See
//! [ADR 0016](https://github.com/ManirajKatuwal/krab-pub/blob/main/docs/adr/0016-suspense-boundaries.md).
//!
//! # Server rendering
//!
//! Synchronous, per ADR 0009: a resource with an initial value is `Ready`, so
//! its boundary renders the children; one without is `Pending`, so the
//! boundary renders the fallback. Either way the output is wrapped in the
//! suspense marker vocabulary of [`crate::render_stream`]:
//!
//! ```text
//! <!--krab:suspense:s7:pending-->…children or fallback…<!--krab:suspense:s7:resolved-->
//! ```
//!
//! The opening marker delimits the boundary; the closing one records that the
//! server has finished with it, so the snapshot counts as finalized for the
//! ISR cache. Under [`render_to_stream`](crate::render_stream) a boundary whose
//! pending resources all declared a server loader closes with
//! `<!--/krab:suspense:s7-->` instead, and its resolved content follows later
//! in the same response.
//!
//! # What counts as pending
//!
//! A source is pending until it has produced its **first** value. A resource
//! that is refetching while holding data, or that failed, does not put its
//! boundary back to the fallback: swapping loaded content out for a spinner on
//! every refresh would destroy the DOM (focus, selection, scroll) the refresh
//! exists to update. Render a refetch indicator from
//! [`Resource::state`](crate::resource::Resource::state) inside the children
//! instead.

use crate::signal::{provide_context, use_context};
use crate::Node;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_SUSPENSE_ID: AtomicU64 = AtomicU64::new(1);

/// A fresh boundary id, `s{n}`, from a process-wide counter: unique within a
/// response, which is what the streaming swap needs. Like island boundary ids
/// it is not stable across renders, so two renders of the same page differ in
/// these markers.
fn next_suspense_id() -> String {
    format!("s{}", NEXT_SUSPENSE_ID.fetch_add(1, Ordering::Relaxed))
}

/// One async source registered with a boundary.
struct Source {
    /// Tracked read: `true` while the source has not produced a first value.
    is_pending: Rc<dyn Fn() -> bool>,
    /// Set by a server loader ([`SourceHandle::mark_streamable`]): a streaming
    /// render can resolve this source itself, so its boundary may be deferred
    /// rather than rendered with the fallback as final output. Nothing streams
    /// in a browser, so nothing reads it there.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    streamable: Rc<Cell<bool>>,
}

struct SuspenseState {
    id: String,
    sources: RefCell<Vec<Source>>,
}

/// The nearest `<Suspense>` boundary, provided to everything built inside its
/// children. Cloning is cheap and shares the boundary.
#[derive(Clone)]
pub struct SuspenseContext {
    state: Rc<SuspenseState>,
}

/// What [`SuspenseContext::register`] returns: lets the registered source
/// tell the boundary it has a server-side loader.
#[derive(Clone)]
pub struct SourceHandle {
    streamable: Rc<Cell<bool>>,
}

impl SourceHandle {
    /// Record that a streaming render can resolve this source on the server.
    /// Called by [`Resource::with_server_loader`](crate::resource::Resource::with_server_loader).
    pub fn mark_streamable(&self) {
        self.streamable.set(true);
    }
}

impl SuspenseContext {
    fn new() -> Self {
        Self {
            state: Rc::new(SuspenseState {
                id: next_suspense_id(),
                sources: RefCell::new(Vec::new()),
            }),
        }
    }

    /// The boundary id, as written into its markers.
    pub fn id(&self) -> &str {
        &self.state.id
    }

    /// Register an async source with this boundary. `is_pending` is read
    /// inside the boundary's reactive closure, so any signal it reads
    /// re-evaluates the boundary when it changes.
    ///
    /// Resources register themselves; call this for a hand-rolled async value
    /// (a signal filled by an action, say) that should hold the boundary on
    /// its fallback until it arrives.
    pub fn register(&self, is_pending: impl Fn() -> bool + 'static) -> SourceHandle {
        let streamable = Rc::new(Cell::new(false));
        self.state.sources.borrow_mut().push(Source {
            is_pending: Rc::new(is_pending),
            streamable: streamable.clone(),
        });
        SourceHandle { streamable }
    }

    /// Whether any registered source is still pending. A tracked read.
    pub fn is_pending(&self) -> bool {
        // Cloned out of the cell before any source runs: a source is user
        // code and must not be able to hit a `RefCell` borrow held here.
        let sources: Vec<Rc<dyn Fn() -> bool>> = self
            .state
            .sources
            .borrow()
            .iter()
            .map(|source| source.is_pending.clone())
            .collect();
        // Every source is read, not just up to the first pending one, so the
        // boundary subscribes to all of them in one evaluation.
        sources
            .iter()
            .fold(false, |pending, is_pending| is_pending() | pending)
    }

    /// Whether every *pending* source has a server loader, so a streaming
    /// render can resolve the boundary itself. Untracked.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    pub(crate) fn pending_sources_are_streamable(&self) -> bool {
        let sources: Vec<(Rc<dyn Fn() -> bool>, bool)> = self
            .state
            .sources
            .borrow()
            .iter()
            .map(|source| (source.is_pending.clone(), source.streamable.get()))
            .collect();
        crate::signal::untrack(|| {
            sources
                .iter()
                .all(|(is_pending, streamable)| *streamable || !is_pending())
        })
    }

    /// The number of registered sources.
    pub fn source_count(&self) -> usize {
        self.state.sources.borrow().len()
    }
}

/// The nearest enclosing `<Suspense>` boundary, if any.
pub fn use_suspense() -> Option<SuspenseContext> {
    use_context::<SuspenseContext>()
}

/// The opening marker's comment text for boundary `id`.
pub(crate) fn open_marker(id: &str) -> String {
    format!("krab:suspense:{id}:pending")
}

/// The closing marker's comment text for a boundary the server has finished.
pub(crate) fn resolved_marker(id: &str) -> String {
    format!("krab:suspense:{id}:resolved")
}

/// The closing delimiter of a boundary whose content will be streamed later.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
pub(crate) fn deferred_end_marker(id: &str) -> String {
    format!("/krab:suspense:{id}")
}

/// A suspense boundary: `children` while every async source registered inside
/// it has produced a value, `fallback` until then.
///
/// `children` runs once, immediately, inside a new owner that provides the
/// boundary's [`SuspenseContext`] — which is how resources created while
/// building it find the boundary. `fallback` runs each time the fallback is
/// shown. The result is a [`Node::Dynamic`], so the boundary switches
/// reactively in the browser. See the module docs for the rendered markers.
///
/// Read resource values inside `move ||` closures in the children, not in a
/// component body: the children are built once, before the data exists, and
/// re-rendered — not rebuilt — when it arrives.
pub fn suspense<F, C>(fallback: F, children: C) -> Node
where
    F: Fn() -> Node + 'static,
    C: FnOnce() -> Node,
{
    let context = SuspenseContext::new();
    // The boundary's scope is kept, not just entered: a streamed boundary's
    // content is rendered after the page render has returned, and it must
    // still see the contexts its scope (and every enclosing one) provides.
    let owner = crate::signal::Owner::new();
    let content = {
        let context = context.clone();
        owner.with(move || {
            provide_context(context);
            children()
        })
    };

    Node::Dynamic(Rc::new(move || {
        let id = context.id();
        if context.is_pending() {
            #[cfg(not(target_arch = "wasm32"))]
            if let Some(deferred) =
                crate::render_stream::defer_boundary(&context, &content, &owner, &fallback)
            {
                return deferred;
            }
            Node::Fragment(vec![
                Node::Comment(open_marker(id)),
                fallback(),
                Node::Comment(resolved_marker(id)),
            ])
        } else {
            Node::Fragment(vec![
                Node::Comment(open_marker(id)),
                content.clone(),
                Node::Comment(resolved_marker(id)),
            ])
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource::{create_resource, create_resource_with_initial};
    use crate::signal::{create_signal, with_owner};
    use crate::{Element, Render};

    fn p(text: &str) -> Node {
        Node::Element(Element {
            tag: "p".to_string(),
            attributes: vec![],
            children: vec![Node::Text(text.to_string())],
            events: vec![],
        })
    }

    #[test]
    fn a_boundary_without_sources_renders_its_children() {
        let html = with_owner(|| suspense(|| p("loading"), || p("done")).render());
        assert!(html.contains("<p>done</p>"), "{html}");
        assert!(!html.contains("loading"));
        assert!(html.starts_with("<!--krab:suspense:s"), "{html}");
        assert!(html.ends_with(":resolved-->"), "{html}");
        assert!(crate::render_stream::is_finalized_ssr_snapshot(&html));
    }

    /// ADR 0009: no initial value means `Pending` on the server, so the
    /// fallback is the rendered output — and the snapshot is still final.
    #[test]
    fn a_pending_resource_renders_the_fallback_on_the_server() {
        let html = with_owner(|| {
            suspense(
                || p("loading"),
                || {
                    let user = create_resource(|| 1u32, |n| async move { Ok::<_, String>(n) });
                    Node::Dynamic(Rc::new(move || {
                        p(&format!("user {:?}", user.value().get()))
                    }))
                },
            )
            .render()
        });
        assert!(html.contains("<p>loading</p>"), "{html}");
        assert!(!html.contains("user"), "{html}");
        assert!(crate::render_stream::is_finalized_ssr_snapshot(&html));
    }

    #[test]
    fn a_resource_with_initial_data_renders_the_children_on_the_server() {
        let html = with_owner(|| {
            suspense(
                || p("loading"),
                || {
                    let user = create_resource_with_initial(
                        Some("ada".to_string()),
                        || 1u32,
                        |_| async move { Ok::<_, String>("x".to_string()) },
                    );
                    Node::Dynamic(Rc::new(move || p(&user.value().get().unwrap_or_default())))
                },
            )
            .render()
        });
        assert!(html.contains("<p>ada</p>"), "{html}");
        assert!(!html.contains("loading"), "{html}");
    }

    #[test]
    fn resources_register_with_the_nearest_boundary_only() {
        let (outer_ctx, inner_ctx) = with_owner(|| {
            let mut outer = None;
            let mut inner = None;
            let _ = suspense(
                || p("outer"),
                || {
                    outer = use_suspense();
                    let _ = create_resource(|| (), |_| async move { Ok::<_, String>(()) });
                    suspense(
                        || p("inner"),
                        || {
                            inner = use_suspense();
                            let _ = create_resource(|| (), |_| async move { Ok::<_, String>(()) });
                            let _ = create_resource(|| (), |_| async move { Ok::<_, String>(()) });
                            p("x")
                        },
                    )
                },
            );
            (outer, inner)
        });
        let (outer, inner) = (outer_ctx.expect("outer"), inner_ctx.expect("inner"));
        assert_ne!(outer.id(), inner.id());
        assert_eq!(outer.source_count(), 1);
        assert_eq!(inner.source_count(), 2);
        assert!(use_suspense().is_none(), "no boundary outside the scope");
    }

    /// The reactive switch, natively: effects run under `cfg(test)`, so a
    /// boundary rendered inside an effect re-renders when its source settles.
    #[test]
    fn the_boundary_switches_when_its_source_settles() {
        let (ready, set_ready) = create_signal(false);
        let rendered = Rc::new(RefCell::new(String::new()));

        with_owner(|| {
            let node = suspense(
                || p("loading"),
                move || {
                    if let Some(ctx) = use_suspense() {
                        ctx.register(move || !ready.get());
                    }
                    p("content")
                },
            );
            let sink = rendered.clone();
            crate::signal::create_effect(move || {
                *sink.borrow_mut() = node.render();
            });
        });

        assert!(rendered.borrow().contains("loading"));
        set_ready.set(true);
        assert!(
            rendered.borrow().contains("content"),
            "{}",
            rendered.borrow()
        );
        assert!(!rendered.borrow().contains("loading"));
    }
}
