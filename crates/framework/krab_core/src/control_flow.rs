//! Runtime behind `view!`'s built-in control-flow tags.
//!
//! `<Show>` and `<For>` expand into calls to [`show`] and [`for_each`]; see
//! [ADR 0008](https://github.com/krab-framework/krab/blob/main/docs/adr/0008-view-control-flow-tags.md).
//!
//! Both return [`Node::Dynamic`], so they re-evaluate through the same effect
//! machinery as any other reactive interpolation — there is no separate update
//! path to keep in step.

use crate::Node;
use std::rc::Rc;

/// Render `children` when `when` is true, `fallback` otherwise.
///
/// Both branches are closures, so only the taken one is evaluated.
pub fn show<W, C, F>(when: W, children: C, fallback: F) -> Node
where
    W: Fn() -> bool + 'static,
    C: Fn() -> Node + 'static,
    F: Fn() -> Node + 'static,
{
    Node::Dynamic(Rc::new(
        move || {
            if when() {
                children()
            } else {
                fallback()
            }
        },
    ))
}

/// Render one node per item, keyed.
///
/// The key is stamped onto each rendered node as
/// [`HYDRATION_NODE_ID_ATTR`](crate::HYDRATION_NODE_ID_ATTR), which is what the
/// client reconciler matches on. That is the point of `<For>` over a hand-written
/// `map`: without a key, reconciliation falls back to matching by position, and
/// inserting a row at the top shifts every row's identity — losing focus,
/// selection, and scroll on all of them.
///
/// Keys must be unique within the list. Duplicates make two rows compete for the
/// same identity, and the second is rebuilt rather than moved.
pub fn for_each<T, I, K, KF, VF>(each: I, key: KF, view: VF) -> Node
where
    I: Fn() -> Vec<T> + 'static,
    KF: Fn(&T) -> K + 'static,
    K: std::fmt::Display,
    VF: Fn(T) -> Node + 'static,
{
    Node::Dynamic(Rc::new(move || {
        Node::Fragment(
            each()
                .into_iter()
                .map(|item| {
                    let key = key(&item).to_string();
                    with_key(view(item), &key)
                })
                .collect(),
        )
    }))
}

/// Stamp `key` onto `node` as the reconciliation marker.
///
/// A node that already carries the marker keeps it: this and
/// `annotate_hydration_tree` share [`crate::stamp_hydration_marker_if_absent`],
/// so a key set here survives hydration annotation instead of being
/// overwritten by a positional path.
///
/// A `Fragment` is keyed by keying its children — it has no node of its own to
/// carry the attribute, and the reconciler flattens fragments before matching.
/// The children's paths come from [`crate::child_hydration_path`], the same
/// scheme hydration annotation uses, so the two cannot drift.
fn with_key(node: Node, key: &str) -> Node {
    match node {
        Node::Element(mut element) => {
            crate::stamp_hydration_marker_if_absent(&mut element, || key.to_string());
            Node::Element(element)
        }
        Node::Fragment(children) => Node::Fragment(
            children
                .into_iter()
                .enumerate()
                .map(|(index, child)| with_key(child, &crate::child_hydration_path(key, index)))
                .collect(),
        ),
        // Text and Dynamic carry no attributes. A keyed list of bare text nodes
        // reconciles positionally, which is the pre-existing behaviour.
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Attribute, Element, Render};

    fn div(text: &str) -> Node {
        Node::Element(Element {
            tag: "div".to_string(),
            attributes: vec![],
            children: vec![Node::Text(text.to_string())],
            events: vec![],
        })
    }

    fn render(node: &Node) -> String {
        node.render()
    }

    #[test]
    fn show_renders_the_children_branch_when_true() {
        let node = show(|| true, || div("yes"), || div("no"));
        assert!(render(&node).contains("yes"));
        assert!(!render(&node).contains("no"));
    }

    #[test]
    fn show_renders_the_fallback_when_false() {
        let node = show(|| false, || div("yes"), || div("no"));
        assert!(render(&node).contains("no"));
        assert!(!render(&node).contains("yes"));
    }

    #[test]
    fn for_each_stamps_the_key_on_every_row() {
        let node = for_each(
            || vec![7u32, 8, 9],
            |item: &u32| *item,
            |item: u32| div(&format!("row {item}")),
        );

        let html = render(&node);
        for key in [7, 8, 9] {
            assert!(
                html.contains(&format!(r#"data-krab-node-id="{key}""#)),
                "key {key} not stamped; got: {html}"
            );
        }
    }

    #[test]
    fn for_each_over_an_empty_list_renders_nothing() {
        let node = for_each(Vec::<u32>::new, |item: &u32| *item, |_| div("x"));
        assert_eq!(render(&node), "");
    }

    /// The key is the user's, not a positional index — that is what lets a row
    /// keep its identity when the list is reordered.
    #[test]
    fn keys_come_from_the_key_function_not_the_position() {
        let node = for_each(
            || vec!["b", "a"],
            |item: &&str| item.to_string(),
            |item: &str| div(item),
        );

        let html = render(&node);
        assert!(html.contains(r#"data-krab-node-id="b""#));
        assert!(html.contains(r#"data-krab-node-id="a""#));
        // Position 0 holds "b", so a positional scheme would have stamped 0/1.
        assert!(!html.contains(r#"data-krab-node-id="0""#));
    }

    #[test]
    fn an_existing_marker_is_not_overwritten() {
        let pre_keyed = Node::Element(Element {
            tag: "div".to_string(),
            attributes: vec![Attribute::new(
                crate::HYDRATION_NODE_ID_ATTR.to_string(),
                "explicit".to_string(),
            )],
            children: vec![],
            events: vec![],
        });

        let node = with_key(pre_keyed, "generated");
        let html = render(&node);
        assert!(html.contains("explicit"));
        assert!(!html.contains("generated"));
    }

    #[test]
    fn a_fragment_row_keys_its_children() {
        let node = with_key(Node::Fragment(vec![div("a"), div("b")]), "k");
        let html = render(&node);
        assert!(html.contains(r#"data-krab-node-id="k.0""#));
        assert!(html.contains(r#"data-krab-node-id="k.1""#));
    }
}
