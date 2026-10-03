// Should fail: props are struct fields, and a namespaced attribute such as
// `xlink:href` has no field spelling.
#![allow(non_snake_case)]
use krab_core::{Node, Render};
use krab_macros::view;

struct IconProps {
    href: String,
}

fn Icon(props: IconProps) -> Node {
    view! { <svg><use href={props.href}/></svg> }
}

fn main() {
    let _ = view! { <Icon xlink:href="#star"/> }.render();
}
