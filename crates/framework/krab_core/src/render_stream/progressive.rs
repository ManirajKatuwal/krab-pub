//! Progressive (out-of-order) streaming SSR, built on `<Suspense>` — ADR 0017.
//!
//! The render itself stays synchronous and single-threaded, as ADR 0009
//! requires: [`crate::Node`] is `!Send`, so every `Node` — the page, each
//! boundary's children, each re-render — is built and rendered on one
//! dedicated blocking thread. What changes is *when* bytes leave:
//!
//! 1. The page closure runs on the render thread and the result is rendered.
//!    A `<Suspense>` boundary whose pending resources all declared a server
//!    loader ([`Resource::with_server_loader`](crate::resource::Resource::with_server_loader))
//!    is **deferred**: it renders its fallback between
//!    `<!--krab:suspense:{id}:pending-->` and `<!--/krab:suspense:{id}-->`.
//! 2. The shell — everything up to the closing `</body>` — is flushed at once,
//!    with a `<script src>` for the swap runtime ([`STREAM_SWAP_SCRIPT`]).
//! 3. The loaders' futures (which must be `Send`) run on the Tokio runtime;
//!    their results come back to the render thread over a channel and are
//!    applied to the resources there.
//! 4. Each boundary that is no longer pending is rendered again on the render
//!    thread — now showing its children — and flushed as
//!    `<template data-krab-suspense="{id}">…<!--krab:suspense:{id}:resolved--></template>`
//!    followed by a `data-krab-suspense-ready` sentinel. The swap runtime
//!    replaces the fallback with the template's content and dispatches
//!    `krab:suspense-resolved`, on which `krab_client` hydrates the islands
//!    the new content contains.
//! 5. When every deferred boundary has resolved — or the timeout passes, in
//!    which case the leftovers get a `<!--krab:suspense:{id}:error-->` marker
//!    and keep their fallback — the held-back `</body></html>` closes the
//!    document.
//!
//! The concatenated body always balances its markers, so
//! [`is_finalized_ssr_snapshot`](super::is_finalized_ssr_snapshot) holds for a
//! complete stream and fails for a truncated one; a cache that buffers the
//! whole body and checks it before storing can never store a half-streamed
//! page.

use crate::suspense::{deferred_end_marker, open_marker, SuspenseContext};
use crate::Node;
use std::any::Any;
use std::cell::{Cell, RefCell};
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
#[cfg(feature = "rest")]
use std::time::{Duration, Instant};

/// Where `StreamOptions::default()` expects the swap runtime to be served.
/// The application serves [`STREAM_SWAP_SCRIPT`] at this path.
pub const STREAM_SWAP_SCRIPT_PATH: &str = "/_krab/stream.js";

/// The swap runtime a streamed page loads, as JavaScript source.
///
/// Serve it as `application/javascript` at `StreamOptions::swap_script_src`
/// (by default [`STREAM_SWAP_SCRIPT_PATH`]). It is an **external** script on
/// purpose: Krab's security headers send `script-src 'self'`, which blocks
/// inline scripts, so an inline swap call per boundary — the common design —
/// would never run. Instead the runtime watches the document with a
/// `MutationObserver` and swaps a boundary when its `data-krab-suspense-ready`
/// sentinel arrives; the sentinel follows the `<template>`, so the template is
/// complete by then even if the network split it across packets.
///
/// Before removing the fallback it dispatches `krab:suspense-resolving` on
/// `document` with `detail: { id, nodes }` (the outgoing fallback nodes, still
/// connected), and after the swap `krab:suspense-resolved` with the incoming
/// ones. `krab_client::hydrate` listens for both: it unmounts islands in the
/// fallback, which would otherwise keep their event closures and effects alive
/// for the life of the page, and hydrates the islands that arrived.
pub const STREAM_SWAP_SCRIPT: &str = r#"(function () {
  if (window.__krabSuspense) { return; }
  window.__krabSuspense = true;
  function swap(id) {
    var ready = document.querySelector('[data-krab-suspense-ready="' + id + '"]');
    if (ready) { ready.remove(); }
    var template = document.querySelector('template[data-krab-suspense="' + id + '"]');
    if (!template) { return; }
    var open = 'krab:suspense:' + id + ':pending';
    var close = '/krab:suspense:' + id;
    var walker = document.createTreeWalker(document, NodeFilter.SHOW_COMMENT);
    var start = null, end = null, node;
    while ((node = walker.nextNode())) {
      if (start === null) {
        if (node.data === open) { start = node; }
      } else if (node.data === close) {
        end = node;
        break;
      }
    }
    if (start === null || end === null) { template.remove(); return; }
    var parent = end.parentNode;
    var outgoing = [];
    for (var n = start.nextSibling; n !== null && n !== end; n = n.nextSibling) {
      outgoing.push(n);
    }
    document.dispatchEvent(new CustomEvent('krab:suspense-resolving', { detail: { id: id, nodes: outgoing } }));
    while (start.nextSibling !== null && start.nextSibling !== end) {
      parent.removeChild(start.nextSibling);
    }
    var nodes = Array.prototype.slice.call(template.content.childNodes);
    parent.replaceChild(template.content, end);
    template.remove();
    document.dispatchEvent(new CustomEvent('krab:suspense-resolved', { detail: { id: id, nodes: nodes } }));
  }
  function scan(root) {
    if (!root.querySelectorAll) { return; }
    var found = root.querySelectorAll('[data-krab-suspense-ready]');
    for (var i = 0; i < found.length; i++) {
      swap(found[i].getAttribute('data-krab-suspense-ready'));
    }
  }
  new MutationObserver(function (records) {
    for (var i = 0; i < records.length; i++) {
      var added = records[i].addedNodes;
      for (var j = 0; j < added.length; j++) {
        var node = added[j];
        if (node.nodeType !== 1) { continue; }
        if (node.hasAttribute('data-krab-suspense-ready')) {
          swap(node.getAttribute('data-krab-suspense-ready'));
        } else {
          scan(node);
        }
      }
    }
  }).observe(document, { childList: true, subtree: true });
  scan(document);
  document.addEventListener('DOMContentLoaded', function () { scan(document); });
})();
"#;

type LoaderFuture = Pin<Box<dyn Future<Output = Box<dyn Any + Send>> + Send>>;
type LoaderApply = Box<dyn FnOnce(Box<dyn Any + Send>)>;

/// A boundary rendered with its fallback, whose content is still to come.
///
/// Without `rest` nothing can install a stream, so nothing reads these back.
#[cfg_attr(not(feature = "rest"), allow(dead_code))]
struct DeferredBoundary {
    context: SuspenseContext,
    content: Node,
    /// The boundary's scope, kept alive so the late render sees the contexts
    /// it had during the page render.
    owner: crate::signal::Owner,
    emitted: bool,
}

/// Per-render state, reachable from the render thread only.
#[cfg_attr(not(feature = "rest"), allow(dead_code))]
struct StreamScope {
    next_loader_id: Cell<u64>,
    /// Registered since the render loop last looked; drained and spawned.
    new_loaders: RefCell<Vec<(u64, LoaderFuture)>>,
    /// How to apply each outstanding loader's result, by loader id.
    applies: RefCell<std::collections::HashMap<u64, LoaderApply>>,
    deferred: RefCell<Vec<DeferredBoundary>>,
}

impl StreamScope {
    #[cfg(feature = "rest")]
    fn new() -> Self {
        Self {
            next_loader_id: Cell::new(0),
            new_loaders: RefCell::new(Vec::new()),
            applies: RefCell::new(std::collections::HashMap::new()),
            deferred: RefCell::new(Vec::new()),
        }
    }
}

thread_local! {
    /// The streaming render running on this thread, if any. Only ever set on
    /// the dedicated render thread, for the duration of one response.
    static ACTIVE_STREAM: RefCell<Option<Rc<StreamScope>>> = const { RefCell::new(None) };
}

#[cfg(feature = "rest")]
/// Installs a scope as this thread's active stream and removes it on drop —
/// including on unwind, so a panicking render cannot leave a later render on
/// the same (pooled) thread believing it is streaming.
struct ActiveStreamGuard;

#[cfg(feature = "rest")]
impl ActiveStreamGuard {
    fn install(scope: Rc<StreamScope>) -> Self {
        ACTIVE_STREAM.with(|active| *active.borrow_mut() = Some(scope));
        Self
    }
}

#[cfg(feature = "rest")]
impl Drop for ActiveStreamGuard {
    fn drop(&mut self) {
        ACTIVE_STREAM.with(|active| *active.borrow_mut() = None);
    }
}

fn active_scope() -> Option<Rc<StreamScope>> {
    ACTIVE_STREAM.with(|active| active.borrow().clone())
}

/// Whether a streaming render is running on this thread.
pub(crate) fn stream_active() -> bool {
    ACTIVE_STREAM.with(|active| active.borrow().is_some())
}

/// Queue `future` to run once the shell is flushed, and `apply` to receive its
/// output on the render thread. `false` when no streaming render is active.
pub(crate) fn register_server_loader<T, Fut, A>(future: Fut, apply: A) -> bool
where
    T: Send + 'static,
    Fut: Future<Output = T> + Send + 'static,
    A: FnOnce(T) + 'static,
{
    let Some(scope) = active_scope() else {
        return false;
    };
    let id = scope.next_loader_id.get();
    scope.next_loader_id.set(id + 1);

    let boxed: LoaderFuture = Box::pin(async move {
        let output: Box<dyn Any + Send> = Box::new(future.await);
        output
    });
    let apply: LoaderApply = Box::new(move |output: Box<dyn Any + Send>| {
        // The loader and its apply are registered together with the same `T`,
        // so the downcast cannot fail; a mismatch would be a bug here, and is
        // dropped rather than panicking a response.
        if let Ok(output) = output.downcast::<T>() {
            apply(*output);
        }
    });
    scope.new_loaders.borrow_mut().push((id, boxed));
    scope.applies.borrow_mut().insert(id, apply);
    true
}

/// Called by a `<Suspense>` boundary rendering its fallback. Under a streaming
/// render, when every pending source can be resolved by a server loader, the
/// boundary is deferred: this returns the fallback delimited for a later swap.
/// Otherwise `None`, and the boundary renders its fallback as final output.
pub(crate) fn defer_boundary(
    context: &SuspenseContext,
    content: &Node,
    owner: &crate::signal::Owner,
    fallback: &dyn Fn() -> Node,
) -> Option<Node> {
    let scope = active_scope()?;
    if !context.pending_sources_are_streamable() {
        return None;
    }
    {
        let mut deferred = scope.deferred.borrow_mut();
        if !deferred
            .iter()
            .any(|boundary| boundary.context.id() == context.id())
        {
            deferred.push(DeferredBoundary {
                context: context.clone(),
                content: content.clone(),
                owner: owner.clone(),
                emitted: false,
            });
        }
    }
    let id = context.id();
    Some(Node::Fragment(vec![
        Node::Comment(open_marker(id)),
        fallback(),
        Node::Comment(deferred_end_marker(id)),
    ]))
}

#[cfg(feature = "rest")]
/// Options for [`render_to_stream_with`].
#[derive(Debug, Clone)]
pub struct StreamOptions {
    /// The deadline for the whole stream, measured from the moment the render
    /// starts — not from the shell flush — so the time spent rendering the
    /// shell comes out of it. Server loaders still pending at the deadline
    /// are aborted; their boundaries keep the fallback and are closed with an
    /// `error` marker. A slow shell render therefore leaves the loaders less
    /// time; it never extends the deadline. Default 10 seconds.
    pub timeout: Duration,
    /// The URL the shell loads the swap runtime from. Default
    /// [`STREAM_SWAP_SCRIPT_PATH`]; the application must serve
    /// [`STREAM_SWAP_SCRIPT`] there.
    pub swap_script_src: String,
    /// Prefix the document with `<!DOCTYPE html>`. Default `true`.
    pub doctype: bool,
}

#[cfg(feature = "rest")]
impl Default for StreamOptions {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(10),
            swap_script_src: STREAM_SWAP_SCRIPT_PATH.to_string(),
            doctype: true,
        }
    }
}

#[cfg(feature = "rest")]
/// A streamed HTML response body: the shell first, then each resolved
/// `<Suspense>` boundary as it completes. Implements `Stream`, so
/// `axum::body::Body::from_stream(render_to_stream(...))` serves it.
///
/// Dropping it (the client went away) stops the render thread at its next
/// write and aborts the loaders still running.
pub struct RenderStream {
    receiver: tokio::sync::mpsc::Receiver<axum::body::Bytes>,
}

#[cfg(feature = "rest")]
impl std::fmt::Debug for RenderStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RenderStream").finish_non_exhaustive()
    }
}

#[cfg(feature = "rest")]
impl futures_core::Stream for RenderStream {
    type Item = Result<axum::body::Bytes, std::convert::Infallible>;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.receiver.poll_recv(cx).map(|chunk| chunk.map(Ok))
    }
}

#[cfg(feature = "rest")]
/// [`render_to_stream_with`] with [`StreamOptions::default`].
pub fn render_to_stream<F>(render: F) -> RenderStream
where
    F: FnOnce() -> Node + Send + 'static,
{
    render_to_stream_with(StreamOptions::default(), render)
}

#[cfg(feature = "rest")]
/// Render the page `render` builds as a progressively streamed response.
///
/// `render` must return the whole document (`<html>…</html>`). It runs on a
/// dedicated blocking thread — `Node` is `!Send`, so the tree is built,
/// rendered and re-rendered there and nowhere else — inside a fresh owner, so
/// it can `provide_context` for the request. See the module docs for the
/// protocol, and ADR 0017 for what is and is not supported.
///
/// Must be called from within a Tokio runtime: the server loaders run on it.
/// Called outside one, the page still streams, but no loader runs and every
/// boundary renders its fallback as final output (logged as
/// `ssr_stream_without_runtime`).
pub fn render_to_stream_with<F>(options: StreamOptions, render: F) -> RenderStream
where
    F: FnOnce() -> Node + Send + 'static,
{
    let (sender, receiver) = tokio::sync::mpsc::channel(16);
    let runtime = tokio::runtime::Handle::try_current().ok();

    match runtime.clone() {
        Some(handle) => {
            handle.spawn_blocking(move || run_stream(render, options, sender, runtime));
        }
        None => {
            tracing::warn!(
                event = "ssr_stream_without_runtime",
                "render_to_stream called outside a Tokio runtime; server loaders will not run"
            );
            let spawned = std::thread::Builder::new()
                .name("krab-ssr-stream".to_string())
                .spawn(move || run_stream(render, options, sender, None));
            if let Err(error) = spawned {
                tracing::error!(
                    event = "ssr_stream_thread_spawn_failed",
                    %error,
                    "could not start the render thread; the response body will be empty"
                );
            }
        }
    }

    RenderStream { receiver }
}

#[cfg(feature = "rest")]
/// Escape a value for a double-quoted attribute.
fn escape_attr(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(feature = "rest")]
/// The render thread's whole job. Every `Node` lives and dies in here.
fn run_stream<F>(
    render: F,
    options: StreamOptions,
    sender: tokio::sync::mpsc::Sender<axum::body::Bytes>,
    runtime: Option<tokio::runtime::Handle>,
) where
    F: FnOnce() -> Node,
{
    let started = Instant::now();
    let scope = Rc::new(StreamScope::new());
    // Only a thread with a runtime to run loaders on streams at all: without
    // one, nothing could ever resolve a deferred boundary.
    let _guard = runtime
        .is_some()
        .then(|| ActiveStreamGuard::install(scope.clone()));

    let send =
        |chunk: String| -> bool { sender.blocking_send(axum::body::Bytes::from(chunk)).is_ok() };

    // A panic in user render code must not take the thread's scope with it
    // (the guard handles that) nor leave the client waiting on a stream that
    // will never end: the sender drops with this frame either way.
    let rendered = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        use crate::Render;
        crate::signal::with_owner(|| render().render())
    }));
    let html = match rendered {
        Ok(html) => html,
        Err(_) => {
            tracing::error!(
                event = "ssr_stream_render_panicked",
                "the page render panicked before the shell was flushed"
            );
            return;
        }
    };

    let mut document = String::with_capacity(html.len() + 64);
    if options.doctype {
        document.push_str("<!DOCTYPE html>");
    }
    document.push_str(&html);
    // Everything from the last `</body>` on is held back until the streamed
    // boundaries are in, so they land inside the body rather than after the
    // end of the document.
    let tail = match document.to_ascii_lowercase().rfind("</body>") {
        Some(at) => document.split_off(at),
        None => String::new(),
    };

    let has_deferred = !scope.deferred.borrow().is_empty();
    if has_deferred {
        document.push_str(&format!(
            "<script src=\"{}\"></script>",
            escape_attr(&options.swap_script_src)
        ));
    }
    if !send(document) {
        return;
    }

    // `None` reports a loader that panicked. The render loop holds a sender of
    // its own (it clones one per loader, including loaders registered while
    // rendering resolved content), so the channel never disconnects and a
    // loader that dies without reporting would hold the stream open until the
    // deadline. Each loader therefore runs as its own task, and a wrapper
    // reports its outcome either way.
    let (result_sender, results) = std::sync::mpsc::channel::<(u64, Option<Box<dyn Any + Send>>)>();
    let mut running: Vec<tokio::task::AbortHandle> = Vec::new();
    let deadline = started + options.timeout;
    let mut resolved = 0usize;
    let mut client_gone = false;

    loop {
        // Start whatever loaders registered since the last pass — the page
        // render's, then any registered while rendering resolved content.
        let fresh: Vec<(u64, LoaderFuture)> = scope.new_loaders.borrow_mut().drain(..).collect();
        if let Some(runtime) = &runtime {
            for (id, future) in fresh {
                let result_sender = result_sender.clone();
                let loader = runtime.spawn(future);
                running.push(loader.abort_handle());
                running.push(
                    runtime
                        .spawn(async move {
                            let output = loader.await.ok();
                            let _ = result_sender.send((id, output));
                        })
                        .abort_handle(),
                );
            }
        }

        // Emit every deferred boundary that is no longer pending, in document
        // order. Rendering one can defer boundaries nested in it; the loop
        // comes back round for those before it waits.
        let mut chunk = String::new();
        let mut index = 0;
        loop {
            let next = {
                let deferred = scope.deferred.borrow();
                deferred.get(index).map(|boundary| {
                    (
                        boundary.emitted,
                        boundary.context.clone(),
                        boundary.content.clone(),
                        boundary.owner.clone(),
                    )
                })
            };
            let Some((emitted, context, content, owner)) = next else {
                break;
            };
            if !emitted && !crate::signal::untrack(|| context.is_pending()) {
                // Inside the boundary's own scope: the page render that
                // opened it has long returned, and without this a context
                // provided for the request (locale, user, tenant) would be
                // missing from the streamed content.
                let body = owner.with(|| {
                    use crate::Render;
                    content.render()
                });
                let id = context.id();
                chunk.push_str(&format!(
                    "<template data-krab-suspense=\"{id}\">{body}<!--{}--></template><span data-krab-suspense-ready=\"{id}\" hidden></span>",
                    crate::suspense::resolved_marker(id)
                ));
                scope.deferred.borrow_mut()[index].emitted = true;
                resolved += 1;
                tracing::debug!(
                    event = "ssr_stream_boundary_resolved",
                    krab.boundary_id = id,
                    duration_ms = started.elapsed().as_millis() as u64,
                );
            }
            index += 1;
        }
        if !chunk.is_empty() {
            if !send(chunk) {
                client_gone = true;
                break;
            }
            continue;
        }

        let all_emitted = scope
            .deferred
            .borrow()
            .iter()
            .all(|boundary| boundary.emitted);
        if all_emitted || scope.applies.borrow().is_empty() {
            break;
        }

        let now = Instant::now();
        if now >= deadline {
            break;
        }
        match results.recv_timeout(deadline - now) {
            Ok((id, Some(output))) => {
                let apply = scope.applies.borrow_mut().remove(&id);
                if let Some(apply) = apply {
                    apply(output);
                }
            }
            Ok((id, None)) => {
                // The loader panicked. Its boundary keeps its fallback and is
                // closed with an error marker below; dropping its apply lets
                // the loop finish as soon as nothing else is outstanding,
                // instead of waiting out the timeout.
                scope.applies.borrow_mut().remove(&id);
                tracing::warn!(
                    event = "ssr_stream_loader_panicked",
                    loader_id = id,
                    "a server loader panicked; its boundary keeps its fallback"
                );
            }
            Err(_) => break,
        }
    }

    for task in &running {
        task.abort();
    }
    if client_gone {
        tracing::debug!(
            event = "ssr_stream_client_disconnected",
            duration_ms = started.elapsed().as_millis() as u64,
        );
        return;
    }

    // Close out whatever never resolved: the fallback stays on screen, and the
    // `error` marker balances the boundary so the snapshot reads as finalized.
    let mut closing = String::new();
    let mut timed_out = 0usize;
    for boundary in scope.deferred.borrow().iter().filter(|b| !b.emitted) {
        closing.push_str(&format!(
            "<!--krab:suspense:{}:error-->",
            boundary.context.id()
        ));
        timed_out += 1;
    }
    if timed_out > 0 {
        tracing::warn!(
            event = "ssr_stream_boundaries_unresolved",
            count = timed_out,
            timeout_ms = options.timeout.as_millis() as u64,
            "streamed boundaries did not resolve before the stream closed; their fallbacks stay"
        );
    }
    closing.push_str(&tail);
    let _ = send(closing);

    tracing::debug!(
        event = "ssr_stream_completed",
        boundaries_resolved = resolved,
        boundaries_unresolved = timed_out,
        duration_ms = started.elapsed().as_millis() as u64,
    );
}

#[cfg(all(test, feature = "rest"))]
mod tests {
    use super::*;
    use crate::resource::create_resource;
    use crate::suspense::suspense;
    use crate::Element;
    use std::future::poll_fn;

    fn el(tag: &str, children: Vec<Node>) -> Node {
        Node::Element(Element {
            tag: tag.to_string(),
            attributes: vec![],
            children,
            events: vec![],
        })
    }

    fn text(value: &str) -> Node {
        Node::Text(value.to_string())
    }

    /// A page with one boundary around a resource. With `gate`, the resource
    /// has a server loader that resolves to "ada" once the gate opens.
    fn page(gate: Option<tokio::sync::oneshot::Receiver<()>>) -> Node {
        let boundary = suspense(
            || el("p", vec![text("Loading")]),
            move || {
                let user = create_resource(
                    || (),
                    |_| async move { Ok::<String, String>(String::new()) },
                );
                let user = match gate {
                    Some(gate) => user.with_server_loader(move || async move {
                        let _ = gate.await;
                        Ok::<_, String>("ada".to_string())
                    }),
                    None => user,
                };
                Node::Dynamic(Rc::new(move || {
                    el(
                        "p",
                        vec![text(&format!(
                            "user {}",
                            user.value().get().unwrap_or_default()
                        ))],
                    )
                }))
            },
        );
        el(
            "html",
            vec![
                el("head", vec![el("title", vec![text("t")])]),
                el("body", vec![el("main", vec![boundary])]),
            ],
        )
    }

    async fn next_chunk(stream: &mut RenderStream) -> Option<String> {
        use futures_core::Stream;
        poll_fn(|cx| Pin::new(&mut *stream).poll_next(cx))
            .await
            .map(|chunk| match chunk {
                Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
                Err(never) => match never {},
            })
    }

    async fn rest_of(stream: &mut RenderStream) -> String {
        let mut out = String::new();
        while let Some(chunk) = next_chunk(stream).await {
            out.push_str(&chunk);
        }
        out
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_shell_flushes_the_fallback_before_the_data_and_the_content_follows() {
        let (open_gate, gate) = tokio::sync::oneshot::channel();
        let mut stream = render_to_stream(move || page(Some(gate)));

        let shell = next_chunk(&mut stream).await.expect("shell chunk");
        assert!(shell.starts_with("<!DOCTYPE html><html>"), "{shell}");
        assert!(shell.contains("<p>Loading</p>"), "{shell}");
        assert!(shell.contains(":pending-->"), "{shell}");
        assert!(shell.contains("<!--/krab:suspense:s"), "{shell}");
        assert!(
            shell.contains("<script src=\"/_krab/stream.js\"></script>"),
            "{shell}"
        );
        assert!(!shell.contains("user ada"), "{shell}");
        assert!(
            !shell.contains("</body>"),
            "the body close is held back: {shell}"
        );
        assert!(!is_finalized_snapshot(&shell));

        let _ = open_gate.send(());
        let rest = rest_of(&mut stream).await;
        assert!(rest.contains("<template data-krab-suspense=\"s"), "{rest}");
        assert!(rest.contains("<p>user ada</p>"), "{rest}");
        assert!(rest.contains(":resolved--></template>"), "{rest}");
        assert!(rest.contains("data-krab-suspense-ready=\"s"), "{rest}");
        assert!(rest.ends_with("</body></html>"), "{rest}");

        let whole = format!("{shell}{rest}");
        assert!(is_finalized_snapshot(&whole), "{whole}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_boundary_without_a_server_loader_is_rendered_final_in_the_shell() {
        let mut stream = render_to_stream(|| page(None));
        let whole = rest_of(&mut stream).await;

        assert!(whole.contains("<p>Loading</p>"), "{whole}");
        assert!(!whole.contains("<template"), "{whole}");
        assert!(
            !whole.contains("stream.js"),
            "no swap runtime without a deferral"
        );
        assert!(whole.contains(":resolved-->"), "{whole}");
        assert!(is_finalized_snapshot(&whole));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_loader_that_misses_the_timeout_keeps_its_fallback_and_closes_the_boundary() {
        let (open_gate, gate) = tokio::sync::oneshot::channel::<()>();
        let options = StreamOptions {
            timeout: Duration::from_millis(50),
            ..StreamOptions::default()
        };
        let mut stream = render_to_stream_with(options, move || page(Some(gate)));
        let whole = rest_of(&mut stream).await;
        drop(open_gate);

        assert!(whole.contains("<p>Loading</p>"), "{whole}");
        assert!(!whole.contains("user ada"), "{whole}");
        assert!(whole.contains(":error-->"), "{whole}");
        assert!(whole.ends_with("</body></html>"), "{whole}");
        assert!(is_finalized_snapshot(&whole), "{whole}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_render_thread_is_left_without_an_active_stream() {
        // Two streams back to back on the blocking pool: the second must not
        // see the first's scope, and a plain render afterwards is not
        // streaming.
        let _ = rest_of(&mut render_to_stream(|| page(None))).await;
        let _ = rest_of(&mut render_to_stream(|| page(None))).await;
        let active = tokio::task::spawn_blocking(stream_active)
            .await
            .expect("join");
        assert!(!active);
    }

    /// A context provided for the request is visible to a boundary's content
    /// when that content is rendered later, in the streamed template.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn streamed_content_sees_the_request_contexts() {
        #[derive(Clone)]
        struct Locale(&'static str);

        let (open_gate, gate) = tokio::sync::oneshot::channel();
        let mut stream = render_to_stream(move || {
            crate::signal::provide_context(Locale("fr"));
            let boundary = suspense(
                || el("p", vec![text("Loading")]),
                move || {
                    let user = create_resource(
                        || (),
                        |_| async move { Ok::<String, String>(String::new()) },
                    )
                    .with_server_loader(move || async move {
                        let _ = gate.await;
                        Ok::<_, String>("ada".to_string())
                    });
                    Node::Dynamic(Rc::new(move || {
                        let locale = crate::signal::use_context::<Locale>()
                            .map(|l| l.0)
                            .unwrap_or("none");
                        el(
                            "p",
                            vec![text(&format!(
                                "{locale}:{}",
                                user.value().get().unwrap_or_default()
                            ))],
                        )
                    }))
                },
            );
            el("html", vec![el("body", vec![boundary])])
        });

        let _shell = next_chunk(&mut stream).await.expect("shell");
        let _ = open_gate.send(());
        let rest = rest_of(&mut stream).await;
        assert!(rest.contains("<p>fr:ada</p>"), "{rest}");
    }

    /// A panicking loader ends the stream promptly, not at the timeout.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_panicking_loader_does_not_hold_the_stream_open() {
        let options = StreamOptions {
            timeout: Duration::from_secs(30),
            ..StreamOptions::default()
        };
        let started = Instant::now();
        let mut stream = render_to_stream_with(options, || {
            let boundary = suspense(
                || el("p", vec![text("Loading")]),
                || {
                    let user = create_resource(
                        || (),
                        |_| async move { Ok::<String, String>(String::new()) },
                    )
                    .with_server_loader(|| async move {
                        if true {
                            panic!("loader failed");
                        }
                        Ok::<String, String>(String::new())
                    });
                    Node::Dynamic(Rc::new(move || {
                        text(&user.value().get().unwrap_or_default())
                    }))
                },
            );
            el("html", vec![el("body", vec![boundary])])
        });
        let whole = rest_of(&mut stream).await;

        assert!(
            started.elapsed() < Duration::from_secs(10),
            "stream held open for {:?}",
            started.elapsed()
        );
        assert!(whole.contains("<p>Loading</p>"), "{whole}");
        assert!(whole.contains(":error-->"), "{whole}");
        assert!(whole.ends_with("</body></html>"), "{whole}");
    }

    fn is_finalized_snapshot(html: &str) -> bool {
        super::super::is_finalized_ssr_snapshot(html)
    }
}
