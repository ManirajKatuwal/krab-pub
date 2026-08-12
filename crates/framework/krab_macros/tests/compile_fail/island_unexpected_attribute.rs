// Should fail: #[island] takes no arguments.
//
// This used to compile: the attribute token stream was bound to `_attr` and
// discarded, so a misspelt or imagined option silently did nothing.
use krab_macros::island;

#[derive(serde::Serialize, serde::Deserialize)]
struct Props {
    value: i32,
}

#[island(lazy)]
fn Widget(props: Props) -> krab_core::Node {
    krab_core::Node::Text(props.value.to_string())
}

fn main() {}
