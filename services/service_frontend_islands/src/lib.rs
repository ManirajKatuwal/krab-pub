//! Islands for the Krab reference frontend.
//!
//! `service_frontend` renders these on the server — without the `web` feature,
//! `#[island]` emits the `data-island` / `data-props` wrapper — and this crate's
//! WASM bundle, built *with* `web`, registers their hydrating halves so the
//! `hydrate()` export it re-exposes from `krab_client` can find them by name.
//!
//! They were `krab_client`'s bundled demo islands until 0.6.0. Shipping them
//! from the framework registered `Counter`, `Toggle` and `Likes` in every
//! consumer's hydration registry, where a user island of the same name
//! collided with them; an application's islands belong in the application.
#![allow(non_snake_case)]

use krab_core::signal::*;
use krab_core::{IntoNode, Node};
use krab_macros::{island, view};
use serde::{Deserialize, Serialize};

/// Props for [`Counter`].
#[derive(Serialize, Deserialize, Clone)]
pub struct CounterProps {
    /// The starting count.
    pub initial: i32,
}

/// A button that counts its clicks.
#[island]
pub fn Counter(props: CounterProps) -> Node {
    // The setters are only called from `on:` handlers, which the SSR half of
    // `#[island]` drops, hence the underscore names.
    let (count, _set_count) = create_signal(props.initial);
    // The dynamic count is wrapped in a <span> so the hydration algorithm can
    // find a real DOM element to anchor the reactive update against. Without
    // it, adjacent text nodes are merged by the browser and the Dynamic node
    // has no corresponding DOM node to replace on signal change.
    view! {
        <button on:click={ move |_| _set_count.update(|c| *c += 1) }>
            "Count: " <span>{ move || count.get().into_node() }</span>
        </button>
    }
}

/// Props for [`Toggle`].
#[derive(Serialize, Deserialize, Clone)]
pub struct ToggleProps {
    /// Whether the toggle starts on.
    pub initial: bool,
}

/// A button that flips an ON/OFF label.
#[island]
pub fn Toggle(props: ToggleProps) -> Node {
    let (on, _set_on) = create_signal(props.initial);
    view! {
        <div>
            <button on:click={ move |_| _set_on.update(|v| *v = !*v) }>
                "Toggle"
            </button>
            <span>{ move || if on.get() { " ON" } else { " OFF" }.into_node() }</span>
        </div>
    }
}

/// Props for [`Likes`].
#[derive(Serialize, Deserialize, Clone)]
pub struct LikesProps {
    /// The starting like count.
    pub initial: i32,
}

/// A like button with a running total.
#[island]
pub fn Likes(props: LikesProps) -> Node {
    let (likes, _set_likes) = create_signal(props.initial);
    view! {
        <div>
            <button on:click={ move |_| _set_likes.update(|v| *v += 1) }>
                "Like"
            </button>
            <span>{ move || likes.get().into_node() }</span>
        </div>
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use krab_core::Render;

    /// Without `web`, each island renders the SSR wrapper the bundle hydrates.
    #[test]
    fn islands_render_hydration_wrappers_on_the_server() {
        let html = Counter(CounterProps { initial: 10 }).render();
        assert!(html.contains("data-island=\"Counter\""), "{html}");
        assert!(html.contains("Count: "), "{html}");
        assert!(html.contains("10"), "{html}");

        let html = Toggle(ToggleProps { initial: false }).render();
        assert!(html.contains("data-island=\"Toggle\""), "{html}");
        assert!(html.contains("OFF"), "{html}");

        let html = Likes(LikesProps { initial: 3 }).render();
        assert!(html.contains("data-island=\"Likes\""), "{html}");
        assert!(html.contains("Like"), "{html}");
    }
}
