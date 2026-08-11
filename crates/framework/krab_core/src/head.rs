//! # Head Management
//!
//! Per-route `<head>` metadata with deterministic merge semantics for
//! nested layout composition.
//!
//! ## Usage
//!
//! ```rust
//! use krab_core::head::HeadContext;
//!
//! let head = HeadContext::new()
//!     .title("Blog Post Title")
//!     .description("A great post about Rust")
//!     .canonical("/blog/my-post")
//!     .og_type("article")
//!     .meta("author", "Jane Doe")
//!     .link_stylesheet("/css/blog.css")
//!     .script_module("/js/blog.js");
//!
//! // In SSR, render the head tags
//! let tags = head.render_tags();
//! assert!(tags.contains("<title>Blog Post Title</title>"));
//! ```

use std::collections::HashMap;

// ── Head Tag Types ──────────────────────────────────────────────────────────

/// A single `<meta>` tag.
#[derive(Debug, Clone, PartialEq)]
pub struct MetaTag {
    /// The meta attribute key (e.g., "name", "property", "http-equiv").
    pub attr_type: MetaAttrType,
    /// The attribute key value (e.g., "description", "og:title").
    pub key: String,
    /// The content value.
    pub content: String,
}

/// The type of meta attribute.
#[derive(Debug, Clone, PartialEq)]
pub enum MetaAttrType {
    /// `<meta name="..." content="...">`
    Name,
    /// `<meta property="..." content="...">`
    Property,
    /// `<meta http-equiv="..." content="...">`
    HttpEquiv,
}

impl MetaTag {
    /// Render this meta tag to an HTML string.
    pub fn render(&self) -> String {
        let attr = match self.attr_type {
            MetaAttrType::Name => "name",
            MetaAttrType::Property => "property",
            MetaAttrType::HttpEquiv => "http-equiv",
        };
        format!(
            "<meta {}=\"{}\" content=\"{}\"/>",
            attr,
            html_escape(&self.key),
            html_escape(&self.content)
        )
    }
}

/// A `<link>` tag.
#[derive(Debug, Clone, PartialEq)]
pub struct LinkTag {
    pub rel: String,
    pub href: String,
    pub extra_attrs: HashMap<String, String>,
}

impl LinkTag {
    /// Render this link tag to an HTML string.
    pub fn render(&self) -> String {
        let mut attrs = format!(
            "rel=\"{}\" href=\"{}\"",
            html_escape(&self.rel),
            html_escape(&self.href)
        );
        for (key, value) in &self.extra_attrs {
            attrs.push_str(&format!(" {}=\"{}\"", html_escape(key), html_escape(value)));
        }
        format!("<link {}/>", attrs)
    }
}

/// A `<script>` tag.
#[derive(Debug, Clone, PartialEq)]
pub struct ScriptTag {
    pub src: Option<String>,
    pub inline_content: Option<String>,
    pub is_module: bool,
    pub is_async: bool,
    pub is_defer: bool,
    pub extra_attrs: HashMap<String, String>,
}

impl ScriptTag {
    /// Render this script tag to an HTML string.
    pub fn render(&self) -> String {
        let mut attrs = String::new();
        if let Some(ref src) = self.src {
            attrs.push_str(&format!(" src=\"{}\"", html_escape(src)));
        }
        if self.is_module {
            attrs.push_str(" type=\"module\"");
        }
        if self.is_async {
            attrs.push_str(" async");
        }
        if self.is_defer {
            attrs.push_str(" defer");
        }
        for (key, value) in &self.extra_attrs {
            attrs.push_str(&format!(" {}=\"{}\"", html_escape(key), html_escape(value)));
        }
        if let Some(ref content) = self.inline_content {
            format!("<script{}>{}</script>", attrs, content)
        } else {
            format!("<script{}></script>", attrs)
        }
    }
}

// ── Head Context ────────────────────────────────────────────────────────────

/// Accumulated `<head>` metadata for a route, supporting merge semantics
/// across nested layouts.
///
/// ## Merge Rules
///
/// When a child route's `HeadContext` is merged into a parent layout's context:
///
/// - **`title`**: Child wins (overwrites parent).
/// - **`description`**: Child wins (overwrites parent).
/// - **`canonical`**: Child wins (overwrites parent).
/// - **`og_type`**: Child wins (overwrites parent).
/// - **`meta` tags**: Child tags with the same key overwrite parent; unique keys are appended.
/// - **`link` tags**: Appended (no deduplication).
/// - **`script` tags**: Appended (no deduplication).
/// - **`json_ld`**: Child wins (overwrites parent).
#[derive(Debug, Clone, Default)]
pub struct HeadContext {
    title_value: Option<String>,
    description_value: Option<String>,
    canonical_value: Option<String>,
    og_type_value: Option<String>,
    og_image_value: Option<String>,
    robots_value: Option<String>,
    charset: String,
    viewport: String,
    meta_tags: Vec<MetaTag>,
    link_tags: Vec<LinkTag>,
    script_tags: Vec<ScriptTag>,
    json_ld: Option<String>,
}

impl HeadContext {
    /// Create a new head context with sensible defaults.
    pub fn new() -> Self {
        Self {
            charset: "utf-8".to_string(),
            viewport: "width=device-width, initial-scale=1.0".to_string(),
            ..Default::default()
        }
    }

    // ── Builder Methods ─────────────────────────────────────────────────

    /// Set the page title.
    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.title_value = Some(title.into());
        self
    }

    /// Set the meta description.
    pub fn description(mut self, desc: impl Into<String>) -> Self {
        self.description_value = Some(desc.into());
        self
    }

    /// Set the canonical URL.
    pub fn canonical(mut self, url: impl Into<String>) -> Self {
        self.canonical_value = Some(url.into());
        self
    }

    /// Set the OpenGraph type (e.g., "website", "article").
    pub fn og_type(mut self, og: impl Into<String>) -> Self {
        self.og_type_value = Some(og.into());
        self
    }

    /// Set the OpenGraph image URL.
    pub fn og_image(mut self, url: impl Into<String>) -> Self {
        self.og_image_value = Some(url.into());
        self
    }

    /// Set the robots directive.
    pub fn robots(mut self, directive: impl Into<String>) -> Self {
        self.robots_value = Some(directive.into());
        self
    }

    /// Add a `<meta name="..." content="...">` tag.
    pub fn meta(mut self, name: impl Into<String>, content: impl Into<String>) -> Self {
        self.meta_tags.push(MetaTag {
            attr_type: MetaAttrType::Name,
            key: name.into(),
            content: content.into(),
        });
        self
    }

    /// Add a `<meta property="..." content="...">` tag (OpenGraph).
    pub fn meta_property(
        mut self,
        property: impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
        self.meta_tags.push(MetaTag {
            attr_type: MetaAttrType::Property,
            key: property.into(),
            content: content.into(),
        });
        self
    }

    /// Add a stylesheet `<link>`.
    pub fn link_stylesheet(mut self, href: impl Into<String>) -> Self {
        self.link_tags.push(LinkTag {
            rel: "stylesheet".to_string(),
            href: href.into(),
            extra_attrs: HashMap::new(),
        });
        self
    }

    /// Add a preload `<link>`.
    pub fn link_preload(mut self, href: impl Into<String>, as_type: impl Into<String>) -> Self {
        let mut extra = HashMap::new();
        extra.insert("as".to_string(), as_type.into());
        self.link_tags.push(LinkTag {
            rel: "preload".to_string(),
            href: href.into(),
            extra_attrs: extra,
        });
        self
    }

    /// Add a generic `<link>` tag.
    pub fn link(mut self, rel: impl Into<String>, href: impl Into<String>) -> Self {
        self.link_tags.push(LinkTag {
            rel: rel.into(),
            href: href.into(),
            extra_attrs: HashMap::new(),
        });
        self
    }

    /// Add an external `<script>` tag.
    pub fn script(mut self, src: impl Into<String>) -> Self {
        self.script_tags.push(ScriptTag {
            src: Some(src.into()),
            inline_content: None,
            is_module: false,
            is_async: false,
            is_defer: true,
            extra_attrs: HashMap::new(),
        });
        self
    }

    /// Add a module `<script type="module">` tag.
    pub fn script_module(mut self, src: impl Into<String>) -> Self {
        self.script_tags.push(ScriptTag {
            src: Some(src.into()),
            inline_content: None,
            is_module: true,
            is_async: false,
            is_defer: false,
            extra_attrs: HashMap::new(),
        });
        self
    }

    /// Add an inline `<script>` tag.
    pub fn script_inline(mut self, content: impl Into<String>) -> Self {
        self.script_tags.push(ScriptTag {
            src: None,
            inline_content: Some(content.into()),
            is_module: false,
            is_async: false,
            is_defer: false,
            extra_attrs: HashMap::new(),
        });
        self
    }

    /// Set structured data (JSON-LD).
    pub fn json_ld(mut self, json: impl Into<String>) -> Self {
        self.json_ld = Some(json.into());
        self
    }

    // ── Merge ───────────────────────────────────────────────────────────

    /// Merge a child context into this parent context.
    ///
    /// Child scalar values (title, description, canonical, og_type) overwrite parent.
    /// Child meta tags with matching keys overwrite parent; unique keys are appended.
    /// Child link and script tags are appended.
    pub fn merge(mut self, child: &HeadContext) -> Self {
        // Scalars: child wins
        if child.title_value.is_some() {
            self.title_value = child.title_value.clone();
        }
        if child.description_value.is_some() {
            self.description_value = child.description_value.clone();
        }
        if child.canonical_value.is_some() {
            self.canonical_value = child.canonical_value.clone();
        }
        if child.og_type_value.is_some() {
            self.og_type_value = child.og_type_value.clone();
        }
        if child.og_image_value.is_some() {
            self.og_image_value = child.og_image_value.clone();
        }
        if child.robots_value.is_some() {
            self.robots_value = child.robots_value.clone();
        }
        if child.json_ld.is_some() {
            self.json_ld = child.json_ld.clone();
        }

        // Meta tags: child keys overwrite, unique keys append
        for child_tag in &child.meta_tags {
            if let Some(pos) = self.meta_tags.iter().position(|t| t.key == child_tag.key) {
                self.meta_tags[pos] = child_tag.clone();
            } else {
                self.meta_tags.push(child_tag.clone());
            }
        }

        // Links and scripts: append
        self.link_tags.extend(child.link_tags.iter().cloned());
        self.script_tags.extend(child.script_tags.iter().cloned());

        self
    }

    // ── Rendering ───────────────────────────────────────────────────────

    /// Render all accumulated head tags into an HTML string.
    ///
    /// The output is deterministic and suitable for SSR injection.
    pub fn render_tags(&self) -> String {
        let mut parts: Vec<String> = Vec::new();

        // Charset
        parts.push(format!(
            "<meta charset=\"{}\"/>",
            html_escape(&self.charset)
        ));

        // Viewport
        if !self.viewport.is_empty() {
            parts.push(format!(
                "<meta name=\"viewport\" content=\"{}\"/>",
                html_escape(&self.viewport)
            ));
        }

        // Title
        if let Some(ref title) = self.title_value {
            parts.push(format!("<title>{}</title>", html_escape(title)));
        }

        // Description
        if let Some(ref desc) = self.description_value {
            parts.push(format!(
                "<meta name=\"description\" content=\"{}\"/>",
                html_escape(desc)
            ));
        }

        // Canonical
        if let Some(ref canonical) = self.canonical_value {
            parts.push(format!(
                "<link rel=\"canonical\" href=\"{}\"/>",
                html_escape(canonical)
            ));
        }

        // Robots
        if let Some(ref robots) = self.robots_value {
            parts.push(format!(
                "<meta name=\"robots\" content=\"{}\"/>",
                html_escape(robots)
            ));
        }

        // OpenGraph
        if let Some(ref title) = self.title_value {
            parts.push(format!(
                "<meta property=\"og:title\" content=\"{}\"/>",
                html_escape(title)
            ));
        }
        if let Some(ref desc) = self.description_value {
            parts.push(format!(
                "<meta property=\"og:description\" content=\"{}\"/>",
                html_escape(desc)
            ));
        }
        if let Some(ref og_type) = self.og_type_value {
            parts.push(format!(
                "<meta property=\"og:type\" content=\"{}\"/>",
                html_escape(og_type)
            ));
        }
        if let Some(ref og_image) = self.og_image_value {
            parts.push(format!(
                "<meta property=\"og:image\" content=\"{}\"/>",
                html_escape(og_image)
            ));
        }
        if let Some(ref canonical) = self.canonical_value {
            parts.push(format!(
                "<meta property=\"og:url\" content=\"{}\"/>",
                html_escape(canonical)
            ));
        }

        // Custom meta tags
        for tag in &self.meta_tags {
            parts.push(tag.render());
        }

        // Link tags
        for tag in &self.link_tags {
            parts.push(tag.render());
        }

        // Script tags
        for tag in &self.script_tags {
            parts.push(tag.render());
        }

        // JSON-LD
        if let Some(ref json_ld) = self.json_ld {
            parts.push(format!(
                "<script type=\"application/ld+json\">{}</script>",
                json_ld
            ));
        }

        parts.join("\n")
    }

    /// Get the current title value, if set.
    pub fn get_title(&self) -> Option<&str> {
        self.title_value.as_deref()
    }

    /// Get the current description value, if set.
    pub fn get_description(&self) -> Option<&str> {
        self.description_value.as_deref()
    }

    /// Get the current canonical URL, if set.
    pub fn get_canonical(&self) -> Option<&str> {
        self.canonical_value.as_deref()
    }
}

// ── HTML Escaping ───────────────────────────────────────────────────────────

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_head_rendering() {
        let head = HeadContext::new()
            .title("Test Page")
            .description("A test page description");

        let tags = head.render_tags();
        assert!(tags.contains("<title>Test Page</title>"));
        assert!(tags.contains("name=\"description\" content=\"A test page description\""));
        assert!(tags.contains("charset=\"utf-8\""));
        assert!(tags.contains("name=\"viewport\""));
    }

    #[test]
    fn canonical_and_og_tags() {
        let head = HeadContext::new()
            .title("Blog Post")
            .canonical("https://example.com/blog/post-1")
            .og_type("article")
            .og_image("https://example.com/img.jpg");

        let tags = head.render_tags();
        assert!(tags.contains("rel=\"canonical\" href=\"https://example.com/blog/post-1\""));
        assert!(tags.contains("og:type\" content=\"article\""));
        assert!(tags.contains("og:image\" content=\"https://example.com/img.jpg\""));
        assert!(tags.contains("og:url\" content=\"https://example.com/blog/post-1\""));
    }

    #[test]
    fn custom_meta_tags() {
        let head = HeadContext::new()
            .meta("author", "Jane Doe")
            .meta("theme-color", "#000000");

        let tags = head.render_tags();
        assert!(tags.contains("name=\"author\" content=\"Jane Doe\""));
        assert!(tags.contains("name=\"theme-color\" content=\"#000000\""));
    }

    #[test]
    fn link_and_script_tags() {
        let head = HeadContext::new()
            .link_stylesheet("/css/main.css")
            .script_module("/js/app.js")
            .script_inline("console.log('hello')");

        let tags = head.render_tags();
        assert!(tags.contains("rel=\"stylesheet\" href=\"/css/main.css\""));
        assert!(tags.contains("src=\"/js/app.js\" type=\"module\""));
        assert!(tags.contains("console.log('hello')"));
    }

    #[test]
    fn merge_child_overwrites_scalars() {
        let parent = HeadContext::new()
            .title("Parent Title")
            .description("Parent desc")
            .canonical("/parent");

        let child = HeadContext::new()
            .title("Child Title")
            .description("Child desc");

        let merged = parent.merge(&child);
        assert_eq!(merged.get_title(), Some("Child Title"));
        assert_eq!(merged.get_description(), Some("Child desc"));
        // Canonical stays from parent since child didn't set it
        assert_eq!(merged.get_canonical(), Some("/parent"));
    }

    #[test]
    fn merge_meta_tags_child_key_overwrites() {
        let parent = HeadContext::new()
            .meta("author", "Parent Author")
            .meta("theme-color", "#fff");

        let child = HeadContext::new()
            .meta("author", "Child Author")
            .meta("keywords", "rust,web");

        let merged = parent.merge(&child);
        let tags = merged.render_tags();
        assert!(tags.contains("name=\"author\" content=\"Child Author\""));
        assert!(!tags.contains("Parent Author"));
        assert!(tags.contains("name=\"theme-color\" content=\"#fff\""));
        assert!(tags.contains("name=\"keywords\" content=\"rust,web\""));
    }

    #[test]
    fn merge_appends_links_and_scripts() {
        let parent = HeadContext::new().link_stylesheet("/css/parent.css");

        let child = HeadContext::new()
            .link_stylesheet("/css/child.css")
            .script("/js/child.js");

        let merged = parent.merge(&child);
        let tags = merged.render_tags();
        assert!(tags.contains("/css/parent.css"));
        assert!(tags.contains("/css/child.css"));
        assert!(tags.contains("/js/child.js"));
    }

    #[test]
    fn json_ld_rendering() {
        let head =
            HeadContext::new().json_ld(r#"{"@context":"https://schema.org","@type":"Article"}"#);

        let tags = head.render_tags();
        assert!(tags.contains("application/ld+json"));
        assert!(tags.contains("@context"));
    }

    #[test]
    fn html_escaping_in_head() {
        let head = HeadContext::new().title("Page with \"quotes\" & <tags>");

        let tags = head.render_tags();
        assert!(tags.contains("&amp;"));
        assert!(tags.contains("&lt;tags&gt;"));
    }

    #[test]
    fn robots_directive() {
        let head = HeadContext::new().robots("noindex, nofollow");

        let tags = head.render_tags();
        assert!(tags.contains("name=\"robots\" content=\"noindex, nofollow\""));
    }

    #[test]
    fn preload_link() {
        let head = HeadContext::new().link_preload("/fonts/inter.woff2", "font");

        let tags = head.render_tags();
        assert!(tags.contains("rel=\"preload\""));
        assert!(tags.contains("href=\"/fonts/inter.woff2\""));
        assert!(tags.contains("as=\"font\""));
    }
}
