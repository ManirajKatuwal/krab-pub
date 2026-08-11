//! # Layout System
//!
//! Provides composable layout wrappers for nested route rendering.
//!
//! ## Usage
//!
//! ```rust
//! use krab_core::head::HeadContext;
//! use krab_core::layout::{Layout, LayoutTree, Outlet};
//!
//! // Define a root layout
//! let root = Layout::new("root", |outlet: &Outlet, head: &HeadContext| {
//!     format!(r#"<!DOCTYPE html>
//!     <html><head>{}</head>
//!     <body><nav>Krab App</nav><main>{}</main></body>
//!     </html>"#, head.render_tags(), outlet.content)
//! });
//!
//! // Define a nested blog layout
//! let blog = Layout::new("blog", |outlet: &Outlet, _head: &HeadContext| {
//!     format!(r#"<div class="blog-layout"><aside>Blog Nav</aside>{}</div>"#, outlet.content)
//! });
//!
//! // Build a layout tree
//! let tree = LayoutTree::new(root)
//!     .nest("/blog", blog);
//!
//! let html = tree.render("/blog/hello", "<h1>Hello</h1>".to_string(), &HeadContext::new());
//! assert!(html.contains("blog-layout"));
//! assert!(html.contains("<h1>Hello</h1>"));
//! ```

use crate::head::HeadContext;

// ── Outlet ──────────────────────────────────────────────────────────────────

/// Represents the child content slot rendered inside a layout.
#[derive(Debug, Clone)]
pub struct Outlet {
    /// The rendered HTML content of the child route or nested layout.
    pub content: String,
}

impl Outlet {
    /// Create a new outlet with the given content.
    pub fn new(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
        }
    }

    /// Returns an empty outlet (for loading states or error fallbacks).
    pub fn empty() -> Self {
        Self {
            content: String::new(),
        }
    }
}

// ── Layout ──────────────────────────────────────────────────────────────────

/// A layout wrapper that composes around child route content.
///
/// Layouts receive an `Outlet` (child content) and a `HeadContext` (metadata),
/// and return the fully rendered HTML string for their region.
type LayoutRenderFn = dyn Fn(&Outlet, &HeadContext) -> String + Send + Sync;

pub struct Layout {
    /// Unique name for this layout (used in diagnostics and debugging).
    pub name: String,
    /// The render function: (outlet, head) -> rendered HTML string.
    render_fn: Box<LayoutRenderFn>,
}

impl Layout {
    /// Create a new layout with a name and render function.
    ///
    /// The render function receives the child content (`Outlet`) and the
    /// accumulated head metadata (`HeadContext`), and should return the
    /// complete HTML string for this layout region.
    pub fn new<F>(name: impl Into<String>, render_fn: F) -> Self
    where
        F: Fn(&Outlet, &HeadContext) -> String + Send + Sync + 'static,
    {
        Self {
            name: name.into(),
            render_fn: Box::new(render_fn),
        }
    }

    /// Render this layout with the given outlet content and head context.
    pub fn render(&self, outlet: &Outlet, head: &HeadContext) -> String {
        (self.render_fn)(outlet, head)
    }
}

impl std::fmt::Debug for Layout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Layout").field("name", &self.name).finish()
    }
}

// ── Layout Route ────────────────────────────────────────────────────────────

/// A route entry within a layout tree, binding a path prefix to a layout.
#[derive(Debug)]
pub struct LayoutRoute {
    /// Path prefix this layout applies to (e.g., "/blog").
    pub path_prefix: String,
    /// The layout to apply.
    pub layout: Layout,
    /// Child layout routes (nested layouts).
    pub children: Vec<LayoutRoute>,
}

impl LayoutRoute {
    /// Create a new layout route.
    pub fn new(path_prefix: impl Into<String>, layout: Layout) -> Self {
        Self {
            path_prefix: path_prefix.into(),
            layout,
            children: Vec::new(),
        }
    }

    /// Add a nested child layout route.
    pub fn child(mut self, route: LayoutRoute) -> Self {
        self.children.push(route);
        self
    }
}

// ── Layout Tree ─────────────────────────────────────────────────────────────

/// A tree of layouts that maps path prefixes to layout wrappers.
///
/// The tree is resolved from root to leaf: the root layout wraps
/// the first matching child layout, which wraps the next, and so on.
/// The innermost layout wraps the actual page content.
#[derive(Debug)]
pub struct LayoutTree {
    /// The root layout (applied to all routes).
    root: LayoutRoute,
}

impl LayoutTree {
    /// Create a layout tree with a root layout.
    pub fn new(root_layout: Layout) -> Self {
        Self {
            root: LayoutRoute::new("/", root_layout),
        }
    }

    /// Add a nested layout for a path prefix.
    pub fn nest(mut self, path_prefix: impl Into<String>, layout: Layout) -> Self {
        self.root
            .children
            .push(LayoutRoute::new(path_prefix, layout));
        self
    }

    /// Add a deeply nested layout route.
    pub fn nest_route(mut self, route: LayoutRoute) -> Self {
        self.root.children.push(route);
        self
    }

    /// Resolve the layout chain for a given request path.
    ///
    /// Returns the ordered list of layouts from root to innermost,
    /// matching by path prefix.
    pub fn resolve(&self, path: &str) -> Vec<&Layout> {
        let mut chain = vec![&self.root.layout];
        Self::resolve_recursive(&self.root.children, path, &mut chain);
        chain
    }

    fn resolve_recursive<'a>(children: &'a [LayoutRoute], path: &str, chain: &mut Vec<&'a Layout>) {
        for child in children {
            if path.starts_with(&child.path_prefix) {
                chain.push(&child.layout);
                Self::resolve_recursive(&child.children, path, chain);
                return; // First match wins at each level
            }
        }
    }

    /// Render a page through the full layout chain.
    ///
    /// The `page_content` is the innermost content (the actual page HTML).
    /// The `head` context is passed to every layout in the chain.
    ///
    /// Layouts are applied from innermost to outermost:
    /// root_layout(child_layout(page_content))
    pub fn render(&self, path: &str, page_content: String, head: &HeadContext) -> String {
        let chain = self.resolve(path);

        // Apply layouts from innermost to outermost
        let mut content = page_content;
        for layout in chain.iter().rev() {
            let outlet = Outlet::new(content);
            content = layout.render(&outlet, head);
        }

        content
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_root_layout() -> Layout {
        Layout::new("root", |outlet, head| {
            format!(
                "<!DOCTYPE html><html><head>{}</head><body><nav>Nav</nav><main>{}</main></body></html>",
                head.render_tags(),
                outlet.content
            )
        })
    }

    fn make_blog_layout() -> Layout {
        Layout::new("blog", |outlet, _head| {
            format!(
                "<div class=\"blog\"><aside>Blog Sidebar</aside><article>{}</article></div>",
                outlet.content
            )
        })
    }

    fn make_admin_layout() -> Layout {
        Layout::new("admin", |outlet, _head| {
            format!(
                "<div class=\"admin\"><nav>Admin Nav</nav><section>{}</section></div>",
                outlet.content
            )
        })
    }

    #[test]
    fn layout_tree_root_only() {
        let tree = LayoutTree::new(make_root_layout());
        let head = HeadContext::new().title("Home");

        let result = tree.render("/", "<h1>Welcome</h1>".to_string(), &head);
        assert!(result.contains("<nav>Nav</nav>"));
        assert!(result.contains("<h1>Welcome</h1>"));
        assert!(result.contains("<title>Home</title>"));
    }

    #[test]
    fn layout_tree_nested_blog() {
        let tree = LayoutTree::new(make_root_layout()).nest("/blog", make_blog_layout());
        let head = HeadContext::new().title("Blog Post");

        let result = tree.render("/blog/my-post", "<p>Post content</p>".to_string(), &head);
        assert!(result.contains("<nav>Nav</nav>"));
        assert!(result.contains("Blog Sidebar"));
        assert!(result.contains("<p>Post content</p>"));
    }

    #[test]
    fn layout_chain_resolution() {
        let tree = LayoutTree::new(make_root_layout())
            .nest("/blog", make_blog_layout())
            .nest("/admin", make_admin_layout());

        let blog_chain = tree.resolve("/blog/post-1");
        assert_eq!(blog_chain.len(), 2);
        assert_eq!(blog_chain[0].name, "root");
        assert_eq!(blog_chain[1].name, "blog");

        let admin_chain = tree.resolve("/admin/users");
        assert_eq!(admin_chain.len(), 2);
        assert_eq!(admin_chain[0].name, "root");
        assert_eq!(admin_chain[1].name, "admin");

        let root_chain = tree.resolve("/about");
        assert_eq!(root_chain.len(), 1);
        assert_eq!(root_chain[0].name, "root");
    }

    #[test]
    fn outlet_empty() {
        let outlet = Outlet::empty();
        assert!(outlet.content.is_empty());
    }

    #[test]
    fn deeply_nested_layouts() {
        let tree = LayoutTree::new(make_root_layout()).nest_route(
            LayoutRoute::new("/admin", make_admin_layout()).child(LayoutRoute::new(
                "/admin/settings",
                Layout::new("settings", |outlet, _| {
                    format!("<div class=\"settings\">{}</div>", outlet.content)
                }),
            )),
        );

        let chain = tree.resolve("/admin/settings/profile");
        assert_eq!(chain.len(), 3);
        assert_eq!(chain[0].name, "root");
        assert_eq!(chain[1].name, "admin");
        assert_eq!(chain[2].name, "settings");
    }

    #[test]
    fn preserves_parent_shell_across_child_transitions() {
        let tree = LayoutTree::new(make_root_layout())
            .nest("/blog", make_blog_layout())
            .nest("/admin", make_admin_layout());
        let head = HeadContext::new().title("Transitions");

        let blog_html = tree.render("/blog/post-1", "<p>Blog</p>".to_string(), &head);
        let admin_html = tree.render("/admin/users", "<p>Admin</p>".to_string(), &head);

        assert!(blog_html.contains("<nav>Nav</nav>"));
        assert!(admin_html.contains("<nav>Nav</nav>"));
        assert_eq!(blog_html.matches("<nav>Nav</nav>").count(), 1);
        assert_eq!(admin_html.matches("<nav>Nav</nav>").count(), 1);
    }

    #[test]
    fn applies_layouts_from_leaf_to_root_order() {
        let root = Layout::new("root", |outlet, _| format!("[ROOT:{}]", outlet.content));
        let child = Layout::new("child", |outlet, _| format!("[CHILD:{}]", outlet.content));

        let tree = LayoutTree::new(root).nest("/child", child);
        let html = tree.render("/child/page", "PAGE".to_string(), &HeadContext::new());

        assert_eq!(html, "[ROOT:[CHILD:PAGE]]");
    }
}
