// The module is deprecated (see its declaration in lib.rs); its own items and
// tests keep using each other until it is removed in 0.7.0.
#![allow(deprecated)]

use crate::{Attribute, Element, Node};

/// Input to [`optimized_image`]. Deprecated with the module.
pub struct ImageProps {
    /// The fallback `<img>` source. Its path minus the extension is also the
    /// base the `<source>` variants are derived from.
    pub src: String,
    /// The `alt` text.
    pub alt: String,
    /// The `width` attribute, if any.
    pub width: Option<u32>,
    /// The `height` attribute, if any.
    pub height: Option<u32>,
    /// The `class` attribute, if any.
    pub class: Option<String>,
    /// The `loading` attribute (`lazy` or `eager`), if any.
    pub loading: Option<String>,
    /// Emit an `image/avif` `<source>` (default true). Nothing generates the
    /// file it points at.
    pub generate_avif: bool,
    /// Emit an `image/webp` `<source>` (default true). Nothing generates the
    /// file it points at.
    pub generate_webp: bool,
    /// Widths for the `<source>` `srcset`s, as `{base}-{w}w.{ext} {w}w`
    /// entries; empty means a single `{base}.{ext}`.
    pub srcset_widths: Vec<u32>,
}

impl Default for ImageProps {
    fn default() -> Self {
        Self {
            src: String::new(),
            alt: String::new(),
            width: None,
            height: None,
            class: None,
            loading: None,
            generate_avif: true,
            generate_webp: true,
            srcset_widths: vec![],
        }
    }
}

/// Builds a `<picture>` with the requested AVIF/WebP `<source>`s and an
/// `<img>` fallback. Deprecated with the module: the variant files it
/// references are not produced by anything in Krab.
pub fn optimized_image(props: ImageProps) -> Node {
    let mut picture_children = Vec::new();

    let base_src = if let Some(idx) = props.src.rfind('.') {
        &props.src[..idx]
    } else {
        &props.src
    };

    let generate_srcset = |ext: &str| -> String {
        if props.srcset_widths.is_empty() {
            return format!("{}.{}", base_src, ext);
        }
        props
            .srcset_widths
            .iter()
            .map(|w| format!("{}-{w}w.{ext} {w}w", base_src))
            .collect::<Vec<_>>()
            .join(", ")
    };

    if props.generate_avif {
        picture_children.push(Node::Element(Element {
            tag: "source".to_string(),
            attributes: vec![
                Attribute::new("type".to_string(), "image/avif".to_string()),
                Attribute::new("srcset".to_string(), generate_srcset("avif")),
            ],
            children: vec![],
            events: vec![],
        }));
    }

    if props.generate_webp {
        picture_children.push(Node::Element(Element {
            tag: "source".to_string(),
            attributes: vec![
                Attribute::new("type".to_string(), "image/webp".to_string()),
                Attribute::new("srcset".to_string(), generate_srcset("webp")),
            ],
            children: vec![],
            events: vec![],
        }));
    }

    let mut img_attrs = vec![
        Attribute::new("src".to_string(), props.src.clone()),
        Attribute::new("alt".to_string(), props.alt.clone()),
    ];

    if let Some(w) = props.width {
        img_attrs.push(Attribute::new("width".to_string(), w.to_string()));
    }
    if let Some(h) = props.height {
        img_attrs.push(Attribute::new("height".to_string(), h.to_string()));
    }
    if let Some(c) = props.class {
        img_attrs.push(Attribute::new("class".to_string(), c));
    }
    if let Some(l) = props.loading {
        img_attrs.push(Attribute::new("loading".to_string(), l));
    }

    picture_children.push(Node::Element(Element {
        tag: "img".to_string(),
        attributes: img_attrs,
        children: vec![],
        events: vec![],
    }));

    Node::Element(Element {
        tag: "picture".to_string(),
        attributes: vec![],
        children: picture_children,
        events: vec![],
    })
}
