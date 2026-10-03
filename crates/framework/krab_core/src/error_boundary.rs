//! Render-time error boundary.
//!
//! # This works on the server only
//!
//! Recovery here depends on unwinding, so it is real for SSR and for any native
//! build. This module is **not** feature-gated, so it also compiles into the
//! `wasm32` browser bundle — where it is inert. `wasm32-unknown-unknown` is
//! `panic = "abort"`, so [`catch_unwind`] never returns `Err`: a panicking
//! child traps the module instead of rendering the fallback. Do not rely on a
//! boundary to contain a panic in the browser.
//!
//! `krab_client`'s `tests/panic_boundary_browser.rs` demonstrates the browser
//! behaviour, including that a panicking island leaves every island after it in
//! document order unhydrated.

use crate::{Attribute, Element, Node, Render};
use std::panic::{catch_unwind, AssertUnwindSafe};

/// What went wrong inside an [`ErrorBoundary`] that fell back.
#[derive(Debug, Clone)]
pub struct BoundaryDiagnostic {
    /// The boundary's id, as passed to [`ErrorBoundary::new`].
    pub boundary_id: String,
    /// Where the panic happened; currently always `"ssr"`.
    pub phase: &'static str,
    /// The panic message, or `component panicked` for a non-string payload.
    pub message: String,
}

/// Renders a child node, and if rendering it panics, renders a fallback
/// instead of letting the panic escape. Server-side only in effect — see the
/// module docs.
///
/// The fallback is wrapped in
/// `<div data-krab-boundary="{id}" data-krab-boundary-state="error">`. If the
/// fallback panics too, that wrapper is rendered empty. Rendering it via
/// [`Render`] discards the diagnostic; use
/// [`render_with_diagnostics`](Self::render_with_diagnostics) to keep it.
#[derive(Clone)]
pub struct ErrorBoundary {
    boundary_id: String,
    child: Node,
    fallback: Node,
}

impl ErrorBoundary {
    /// A boundary named `boundary_id` around `child`, rendering `fallback` if
    /// `child` panics.
    pub fn new(boundary_id: impl Into<String>, child: Node, fallback: Node) -> Self {
        Self {
            boundary_id: boundary_id.into(),
            child,
            fallback,
        }
    }

    /// Renders the child, or the wrapped fallback if it panics. The
    /// diagnostic is `Some` exactly when the fallback was used. The panic is
    /// still reported by the process panic hook as usual.
    pub fn render_with_diagnostics(&self) -> (String, Option<BoundaryDiagnostic>) {
        let result = catch_unwind(AssertUnwindSafe(|| self.child.render()));
        match result {
            Ok(html) => (html, None),
            Err(payload) => {
                let message = panic_message(payload);
                let wrapped_fallback = Node::Element(Element {
                    tag: "div".to_string(),
                    attributes: vec![
                        Attribute::new("data-krab-boundary".to_string(), self.boundary_id.clone()),
                        Attribute::new("data-krab-boundary-state".to_string(), "error".to_string()),
                    ],
                    children: vec![self.fallback.clone()],
                    events: vec![],
                });

                // The fallback is user-supplied markup too, and a boundary
                // whose fallback also panics must still produce HTML — the
                // whole point of the boundary is that rendering continues.
                // Degrade to a minimal error div that keeps the
                // `data-krab-boundary-state="error"` attribute contract the
                // client and tests key on.
                let html = match catch_unwind(AssertUnwindSafe(|| wrapped_fallback.render())) {
                    Ok(html) => html,
                    Err(fallback_payload) => {
                        let fallback_message = panic_message(fallback_payload);
                        tracing::error!(
                            boundary_id = %self.boundary_id,
                            error = %fallback_message,
                            "error_boundary_fallback_panicked"
                        );
                        format!(
                            "<div data-krab-boundary=\"{}\" data-krab-boundary-state=\"error\"></div>",
                            crate::escape_html_attr(&self.boundary_id)
                        )
                    }
                };

                (
                    html,
                    Some(BoundaryDiagnostic {
                        boundary_id: self.boundary_id.clone(),
                        phase: "ssr",
                        message,
                    }),
                )
            }
        }
    }
}

impl Render for ErrorBoundary {
    fn render(&self) -> String {
        self.render_with_diagnostics().0
    }
}

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(msg) = payload.downcast_ref::<&str>() {
        (*msg).to_string()
    } else if let Some(msg) = payload.downcast_ref::<String>() {
        msg.clone()
    } else {
        "component panicked".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boundary_returns_child_html_when_no_error() {
        let boundary = ErrorBoundary::new(
            "home",
            Node::Text("ok".to_string()),
            Node::Text("fallback".to_string()),
        );

        let (html, diag) = boundary.render_with_diagnostics();
        assert_eq!(html, "ok");
        assert!(diag.is_none());
    }

    #[test]
    fn boundary_captures_panic_and_renders_fallback() {
        let boundary = ErrorBoundary::new(
            "home",
            Node::Dynamic(std::rc::Rc::new(|| panic!("boom"))),
            Node::Text("fallback".to_string()),
        );

        let (html, diag) = boundary.render_with_diagnostics();
        assert!(html.contains("data-krab-boundary=\"home\""));
        assert!(html.contains("fallback"));
        let diag = diag.expect("diagnostic should exist");
        assert_eq!(diag.phase, "ssr");
        assert!(diag.message.contains("boom"));
    }

    /// A fallback that itself panics must not escape the boundary: the render
    /// degrades to a minimal error div that still carries the
    /// `data-krab-boundary-state="error"` attribute contract.
    #[test]
    fn a_panicking_fallback_degrades_to_minimal_error_markup() {
        let boundary = ErrorBoundary::new(
            "home",
            Node::Dynamic(std::rc::Rc::new(|| panic!("child boom"))),
            Node::Dynamic(std::rc::Rc::new(|| panic!("fallback boom"))),
        );

        let (html, diag) = boundary.render_with_diagnostics();
        assert!(
            html.contains("data-krab-boundary-state=\"error\""),
            "the degraded markup must keep the boundary-state contract, got: {html}"
        );
        assert!(html.contains("data-krab-boundary=\"home\""));

        // The diagnostic reports the original child failure, which is the
        // error the operator needs first.
        let diag = diag.expect("diagnostic should exist");
        assert_eq!(diag.phase, "ssr");
        assert!(diag.message.contains("child boom"));
    }
}
