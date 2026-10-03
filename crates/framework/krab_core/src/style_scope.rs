// Every public item here is deprecated; they and the tests keep using each
// other until the module is removed in 0.7.0.
#![allow(deprecated)]

use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};

#[deprecated(
    since = "0.6.0",
    note = "not wired into view!, and its scope ids are not stable across toolchains. Removed in 0.7.0"
)]
#[derive(Debug, Clone, PartialEq, Eq)]
/// One component's stylesheet with `:scope` rewritten to its scope class.
pub struct ScopedStyleArtifact {
    /// The component name the scope was derived from.
    pub component: String,
    /// `k` plus 8 hex digits from [`deterministic_scope_id`].
    pub scope_id: String,
    /// The CSS class to put on the component's root: `krab-{scope_id}`.
    pub class_name: String,
    /// The stylesheet with every `:scope` replaced by `.{class_name}`.
    pub css: String,
}

#[deprecated(
    since = "0.6.0",
    note = "not wired into view!, and its scope ids are not stable across toolchains. Removed in 0.7.0"
)]
/// A set of [`ScopedStyleArtifact`]s, at most one per component, kept in
/// component-name order.
#[derive(Debug, Default, Clone)]
pub struct ScopedStyleBundle {
    artifacts: BTreeMap<String, ScopedStyleArtifact>,
}

impl ScopedStyleBundle {
    /// An empty bundle.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds `artifact`, replacing any artifact for the same component.
    pub fn insert(&mut self, artifact: ScopedStyleArtifact) {
        self.artifacts.insert(artifact.component.clone(), artifact);
    }

    /// Every artifact's CSS joined with newlines, in component-name order.
    pub fn extract_production_css(&self) -> String {
        self.artifacts
            .values()
            .map(|a| a.css.clone())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Number of components in the bundle.
    pub fn len(&self) -> usize {
        self.artifacts.len()
    }

    /// Whether the bundle has no components.
    pub fn is_empty(&self) -> bool {
        self.artifacts.is_empty()
    }
}

#[deprecated(
    since = "0.6.0",
    note = "not wired into view!, and its scope ids are not stable across toolchains. Removed in 0.7.0"
)]
/// A scope id for `component`: `k` plus 8 hex digits of its hash. Stable
/// within one build only — it uses `DefaultHasher`, whose output may change
/// between Rust releases.
pub fn deterministic_scope_id(component: &str) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    component.hash(&mut hasher);
    format!("k{:08x}", (hasher.finish() & 0xffff_ffff) as u32)
}

#[deprecated(
    since = "0.6.0",
    note = "not wired into view!, and its scope ids are not stable across toolchains. Removed in 0.7.0"
)]
/// Scopes `css` to `component` by replacing every `:scope` with the
/// component's class selector. A plain text replacement; the CSS is not
/// parsed.
pub fn compile_scoped_style(component: &str, css: &str) -> ScopedStyleArtifact {
    let scope_id = deterministic_scope_id(component);
    let class_name = format!("krab-{}", scope_id);
    let rewritten = css.replace(":scope", &format!(".{}", class_name));

    ScopedStyleArtifact {
        component: component.to_string(),
        scope_id,
        class_name,
        css: rewritten,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_scope_id_is_stable() {
        let a = deterministic_scope_id("Counter");
        let b = deterministic_scope_id("Counter");
        let c = deterministic_scope_id("Likes");
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn scoped_compilation_rewrites_scope_selector() {
        let artifact = compile_scoped_style("Counter", ":scope { color: red; }");
        assert!(artifact.class_name.starts_with("krab-k"));
        assert!(artifact.css.contains(&format!(".{}", artifact.class_name)));
    }

    #[test]
    fn production_css_extraction_is_deterministic() {
        let mut bundle = ScopedStyleBundle::new();
        bundle.insert(compile_scoped_style("A", ":scope { color: red; }"));
        bundle.insert(compile_scoped_style("B", ":scope { color: blue; }"));
        let css = bundle.extract_production_css();
        assert!(css.contains("color: red"));
        assert!(css.contains("color: blue"));
        assert_eq!(bundle.len(), 2);
    }
}
