// Should fail: a component's closing tag must repeat its opening path exactly.
// `</Card>` does not close `<ui::Card>`, even though both could resolve to the
// same function; the macro compares spelling, which is all it can see.
#![allow(non_snake_case)]
use krab_core::Render;
use krab_macros::view;

mod ui {
    use krab_core::Node;

    pub struct CardProps {
        pub children: Node,
    }

    pub fn Card(props: CardProps) -> Node {
        props.children
    }
}

fn main() {
    let _ = view! { <ui::Card>"body"</Card> }.render();
}
