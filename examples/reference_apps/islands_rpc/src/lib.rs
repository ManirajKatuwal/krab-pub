//! Krab reference application: SSR + islands + server functions.
//!
//! This crate exists to prove the framework's headline pitch end to end. Before
//! it, `#[island]` and `#[server]` had zero usages outside the framework's own
//! tests and documentation, so nothing demonstrated that the three features work
//! *together*.
//!
//! What it shows:
//!
//! - [`page`] builds an entire HTML page with [`view!`] — including the `data-*`
//!   attributes the hydration protocol needs, which `view!` could not express
//!   until hyphenated names were supported.
//! - [`TaskCounter`] and [`TaskFilter`] are two `#[island]` components with
//!   distinct props, server-rendered with hydration markers and hydrated in the
//!   browser.
//! - [`add_task`] is a `#[server]` function mounted at `/api/rpc/add_task`,
//!   called from `TaskCounter`'s click handler.
//! - `krab_boot` hydrates and then starts the **client router**, and
//!   [`page_for`] renders two routes sharing one outlet, so moving between them
//!   is an outlet swap rather than a document load — the islands and their
//!   signal state survive it.
//!
//! The same source builds both halves: `cargo build` produces the server,
//! `wasm-pack build --features web` produces the browser bundle.

#![allow(non_snake_case)]

use krab_core::action::create_action;
use krab_core::server_fn::ServerFnError;
use krab_core::signal::*;
use krab_core::{IntoNode, Node};
use krab_macros::{island, server, view};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Server function
// ---------------------------------------------------------------------------

/// What [`add_task`] returns on success.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskSummary {
    /// The accepted, trimmed title.
    pub title: String,
    /// Number of tasks after the addition.
    pub total: u32,
}

/// Add a task.
///
/// Mounted at `POST /api/rpc/add_task` on the server; on wasm32 the same call
/// becomes a `fetch` to that URL, so the island calls it as a plain async fn.
///
/// Deliberately has a rejection path — an empty title — so the integration test
/// can assert that a malformed request is refused rather than silently accepted.
#[server]
pub async fn add_task(title: String) -> Result<TaskSummary, ServerFnError> {
    let trimmed = title.trim();

    if trimmed.is_empty() {
        return Err(ServerFnError::validation(
            "task title must not be empty".to_string(),
        ));
    }

    if trimmed.chars().count() > MAX_TITLE_CHARS {
        return Err(ServerFnError::validation(format!(
            "task title must be at most {MAX_TITLE_CHARS} characters"
        )));
    }

    // A real application would persist here. The example keeps no state so the
    // test asserts the contract rather than a database.
    Ok(TaskSummary {
        title: trimmed.to_string(),
        total: 1,
    })
}

/// Longest accepted task title.
pub const MAX_TITLE_CHARS: usize = 80;

// ---------------------------------------------------------------------------
// Islands
// ---------------------------------------------------------------------------

/// Props for [`TaskCounter`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskCounterProps {
    /// Label rendered beside the count.
    pub label: String,
    /// Count to start from, server-rendered so the first paint is correct.
    pub initial: u32,
}

/// A counter that also calls a `#[server]` function from its click handler.
///
/// This is the island that proves the RPC half of the pitch: the click both
/// updates local signal state and dispatches `add_task` over HTTP.
#[island]
pub fn TaskCounter(props: TaskCounterProps) -> Node {
    // The setter is only read inside the `on:click` closure, which `view!`
    // compiles under `feature = "web"` alone — hence the underscore, matching
    // the convention in `krab_client::components`.
    let (count, _set_count) = create_signal(props.initial);
    let label = props.label.clone();

    // `Action` wraps the `#[server]` call with pending / value / error signals,
    // so the handler stays a one-liner and the markup can react to the request
    // without any of that state being hand-rolled.
    //
    // Note the absence of a `#[cfg]`: an island body compiles for *both* targets,
    // and `Action` lives in `krab_core`, so it exists on both. Underscored for
    // the same reason as `_set_count` — it is read only inside the `on:click`
    // closure, which `view!` compiles under `feature = "web"`.
    let _add = create_action(|title: String| async move { add_task(title).await });

    view! {
        <div class="island task-counter" data-testid="task-counter">
            <span class="island-label" aria-label="counter label">{ label }</span>
            <button
                class="island-button"
                data-action="add-task"
                aria-label="add a task"
                on:click={
                    move |_| {
                        _set_count.update(|c| *c += 1);
                        // Fire-and-observe: `add` carries the pending state and
                        // any error, so the handler stays a one-liner.
                        _add.dispatch("task from island".to_string());
                    }
                }
            >
                "Add task"
            </button>
            <span class="island-count" data-testid="task-count">
                // `IntoNode` covers `i32`, not every integer width, so an
                // unsigned count renders through its string form.
                { move || count.get().to_string().into_node() }
            </span>
        </div>
    }
}

/// Props for [`TaskFilter`] — a different shape from [`TaskCounterProps`], so
/// the example covers two islands with genuinely distinct props rather than the
/// same struct twice.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskFilterProps {
    /// Whether completed tasks are shown.
    pub show_completed: bool,
    /// Filter names offered in the UI.
    pub options: Vec<String>,
}

/// A toggle over the task filter.
#[island]
pub fn TaskFilter(props: TaskFilterProps) -> Node {
    let (show_completed, _set_show_completed) = create_signal(props.show_completed);
    let summary = props.options.join(", ");

    view! {
        <div class="island task-filter" data-testid="task-filter">
            <span class="island-label" aria-label="available filters">{ summary }</span>
            <button
                class="island-button"
                data-action="toggle-completed"
                aria-label="toggle completed tasks"
                on:click={ move |_| _set_show_completed.update(|v| *v = !*v) }
            >
                "Toggle completed"
            </button>
            <span class="island-state" data-testid="filter-state">
                { move || if show_completed.get() { "showing" } else { "hidden" }.into_node() }
            </span>
        </div>
    }
}

// ---------------------------------------------------------------------------
// Client entry point
// ---------------------------------------------------------------------------

/// Boot the browser half: hydrate the islands, then start the client router.
///
/// Exported to JavaScript, and called by the module script [`page_for`] emits.
/// It is deliberately **not** `#[wasm_bindgen(start)]` — `krab_client` already
/// owns the single start function a bundle may have, and a second one is a
/// wasm-bindgen error rather than a runtime surprise.
///
/// Ordering matters: hydration must claim the server-rendered islands before the
/// router can ever swap them out, and the router must be started after the
/// outlet exists in the document.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn krab_boot() {
    krab_client::hydrate();
    krab_client::router::start();
}

/// The module script that calls [`krab_boot`].
///
/// Emitted as a text child of a `<script>` element, so it must survive
/// `krab_core`'s text escaping unchanged: no `<`, `>`, or `&`. That rules out
/// arrow functions and `&&`, which is why it reads the way it does.
const BOOT_SCRIPT: &str = "import init, { krab_boot } from '/pkg/reference_app_islands_rpc.js';\n\
     init().then(function () { krab_boot(); });";

// ---------------------------------------------------------------------------
// Page
// ---------------------------------------------------------------------------

/// The routes this example serves, as (path, nav label) pairs.
///
/// Two of them, because one route cannot demonstrate a router. Both render
/// through [`page_for`], so both carry the outlet — a destination without one
/// makes the router fall back to a full page load.
pub const ROUTES: [(&str, &str); 2] = [("/", "Home"), ("/about", "About")];

/// Build the full page for `route`.
///
/// Every element here comes from `view!`, including the `<meta>` and `data-*`
/// attributes. The reference `service_frontend` hand-writes its island markup as
/// HTML string literals because `view!` could not express hyphenated attribute
/// names; this page is the demonstration that it now can.
///
/// `<main>` carries `data-krab-router-outlet`: its contents are what the router
/// swaps, and the `<nav>` above it is what survives the swap. `tabindex="-1"` is
/// declared rather than left to the router, so the attribute is in the
/// server-rendered markup instead of appearing after the first navigation.
pub fn page_for(route: &str) -> Node {
    let title = match route {
        "/about" => "About — Krab islands and RPC",
        _ => "Krab — islands and RPC",
    };

    view! {
        <html lang="en">
            <head>
                <meta charset="utf-8"/>
                <meta name="viewport" content="width=device-width, initial-scale=1"/>
                <title>{ title.to_string() }</title>
            </head>
            <body data-app="islands-rpc">
                <nav class="site-nav" aria-label="primary">
                    <a href="/" data-testid="nav-home">"Home"</a>
                    <a href="/about" data-testid="nav-about">"About"</a>
                </nav>
                <main
                    class="page"
                    data-testid="page"
                    data-krab-router-outlet=""
                    tabindex="-1"
                >
                    { route_content(route) }
                </main>
                <script type="module">{ BOOT_SCRIPT.to_string() }</script>
            </body>
        </html>
    }
}

/// The swappable part of the document: everything inside the outlet.
fn route_content(route: &str) -> Node {
    if route == "/about" {
        return view! {
            <section class="route about" data-testid="route-about">
                <h1>"About this example"</h1>
                <p class="intro">
                    "Getting here did not reload the document. The client router "
                    "fetched this page, took the contents of the outlet, and swapped "
                    "them in — the nav above and the WASM module stayed exactly as "
                    "they were."
                </p>
            </section>
        };
    }

    let counter = TaskCounter(TaskCounterProps {
        label: "Tasks added".to_string(),
        initial: 0,
    });

    let filter = TaskFilter(TaskFilterProps {
        show_completed: false,
        options: vec!["all".to_string(), "open".to_string(), "done".to_string()],
    });

    view! {
        <section class="route home" data-testid="route-home">
            <h1>"Islands and server functions"</h1>
            <p class="intro">
                "This page is server-rendered. The two components below hydrate in "
                "the browser, and the counter calls a server function over HTTP."
            </p>
            <section class="islands" aria-label="interactive components">
                {counter}
                {filter}
            </section>
        </section>
    }
}

/// Build the home page.
///
/// Retained as the zero-argument entry point the original example published;
/// [`page_for`] is the general form.
pub fn page() -> Node {
    page_for("/")
}

/// Render [`page_for`] to an HTML document string.
pub fn render_page_for(route: &str) -> String {
    use krab_core::Render;
    format!("<!doctype html>{}", page_for(route).render())
}

/// Render the home page to an HTML document string.
pub fn render_page() -> String {
    render_page_for("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn html() -> String {
        render_page()
    }

    #[test]
    fn page_renders_both_island_wrappers_with_hydration_markers() {
        let html = html();

        for island in ["TaskCounter", "TaskFilter"] {
            assert!(
                html.contains(&format!(r#"data-island="{island}""#)),
                "missing data-island for {island}"
            );
            assert!(
                html.contains(&format!(r#"data-krab-boundary="{island}""#)),
                "missing data-krab-boundary for {island}"
            );
        }

        // The full marker set the client runtime queries on.
        assert!(html.contains("data-krab-boundary-id="));
        assert!(html.contains(r#"data-krab-boundary-state="ssr""#));
        assert!(html.contains("data-props="));
    }

    #[test]
    fn island_props_are_serialized_into_the_markup() {
        let html = html();

        // Props are JSON in an HTML attribute, so quotes arrive escaped.
        assert!(
            html.contains("Tasks&quot;") || html.contains("Tasks added"),
            "counter props should be present in the rendered page"
        );
        assert!(
            html.contains("show_completed"),
            "filter props should be present in the rendered page"
        );
    }

    /// Covers `view!` emitting hyphenated attribute names. If this regresses,
    /// the page silently loses its test hooks and ARIA labels.
    #[test]
    fn view_macro_emits_hyphenated_and_aria_attributes() {
        let html = html();

        assert!(html.contains(r#"data-testid="page""#));
        assert!(html.contains(r#"data-testid="task-counter""#));
        assert!(html.contains(r#"data-action="add-task""#));
        assert!(html.contains(r#"aria-label="toggle completed tasks""#));
        assert!(html.contains(r#"data-app="islands-rpc""#));
    }

    #[test]
    fn page_is_a_complete_document() {
        let html = html();

        assert!(html.starts_with("<!doctype html>"));
        assert!(html.contains(r#"<html lang="en">"#));
        assert!(html.contains("<title>Krab — islands and RPC</title>"));
        assert!(html.contains("/pkg/reference_app_islands_rpc.js"));
    }

    #[tokio::test]
    async fn add_task_accepts_a_valid_title_and_trims_it() {
        let result = add_task("  write docs  ".to_string()).await.unwrap();

        assert_eq!(
            result,
            TaskSummary {
                title: "write docs".to_string(),
                total: 1,
            }
        );
    }

    #[tokio::test]
    async fn add_task_rejects_an_empty_title() {
        assert!(add_task("   ".to_string()).await.is_err());
    }

    #[tokio::test]
    async fn add_task_rejects_an_overlong_title() {
        let long = "x".repeat(MAX_TITLE_CHARS + 1);
        assert!(add_task(long).await.is_err());
    }
}
