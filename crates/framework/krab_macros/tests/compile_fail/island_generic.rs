// Should fail: #[island] with generic type parameters.
use krab_macros::island;

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct GenericProps<T> {
    value: T,
}

#[island]
fn GenericIsland<T>(props: GenericProps<T>) -> krab_core::Node {
    krab_core::Node::Text(format!("generic"))
}

fn main() {}
