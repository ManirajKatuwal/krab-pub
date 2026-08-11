use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopedStyleArtifact {
    pub component: String,
    pub scope_id: String,
    pub class_name: String,
    pub css: String,
}

#[derive(Debug, Default, Clone)]
pub struct ScopedStyleBundle {
    artifacts: BTreeMap<String, ScopedStyleArtifact>,
}

impl ScopedStyleBundle {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, artifact: ScopedStyleArtifact) {
        self.artifacts.insert(artifact.component.clone(), artifact);
    }

    pub fn extract_production_css(&self) -> String {
        self.artifacts
            .values()
            .map(|a| a.css.clone())
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn len(&self) -> usize {
        self.artifacts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.artifacts.is_empty()
    }
}

pub fn deterministic_scope_id(component: &str) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    component.hash(&mut hasher);
    format!("k{:08x}", (hasher.finish() & 0xffff_ffff) as u32)
}

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
