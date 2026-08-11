use crate::{Attribute, Element, Node, Render};
use std::panic::{catch_unwind, AssertUnwindSafe};

#[derive(Debug, Clone)]
pub struct BoundaryDiagnostic {
    pub boundary_id: String,
    pub phase: &'static str,
    pub message: String,
}

#[derive(Clone)]
pub struct ErrorBoundary {
    boundary_id: String,
    child: Node,
    fallback: Node,
}

impl ErrorBoundary {
    pub fn new(boundary_id: impl Into<String>, child: Node, fallback: Node) -> Self {
        Self {
            boundary_id: boundary_id.into(),
            child,
            fallback,
        }
    }

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

                (
                    wrapped_fallback.render(),
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
}
