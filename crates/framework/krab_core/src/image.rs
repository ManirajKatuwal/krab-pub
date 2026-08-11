use crate::{Attribute, Element, Node};

pub struct ImageProps {
    pub src: String,
    pub alt: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub class: Option<String>,
    pub loading: Option<String>,
    pub generate_avif: bool,
    pub generate_webp: bool,
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
