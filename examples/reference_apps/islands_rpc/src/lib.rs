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
// Page
// ---------------------------------------------------------------------------

/// Build the full page.
///
/// Every element here comes from `view!`, including the `<meta>` and `data-*`
/// attributes. The reference `service_frontend` hand-writes its island markup as
/// HTML string literals because `view!` could not express hyphenated attribute
/// names; this page is the demonstration that it now can.
pub fn page() -> Node {
    let counter = TaskCounter(TaskCounterProps {
        label: "Tasks added".to_string(),
        initial: 0,
    });

    let filter = TaskFilter(TaskFilterProps {
        show_completed: false,
        options: vec!["all".to_string(), "open".to_string(), "done".to_string()],
    });

    view! {
        <html lang="en">
            <head>
                <meta charset="utf-8"/>
                <meta name="viewport" content="width=device-width, initial-scale=1"/>
                <title>"Krab — islands and RPC"</title>
            </head>
            <body data-app="islands-rpc">
                <main class="page" data-testid="page">
                    <h1>"Islands and server functions"</h1>
                    <p class="intro">
                        "This page is server-rendered. The two components below hydrate in "
                        "the browser, and the counter calls a server function over HTTP."
                    </p>
                    <section class="islands" aria-label="interactive components">
                        {counter}
                        {filter}
                    </section>
                </main>
                <script type="module" src="/pkg/reference_app_islands_rpc.js"></script>
            </body>
        </html>
    }
}

/// Render [`page`] to an HTML document string.
pub fn render_page() -> String {
    use krab_core::Render;
    format!("<!doctype html>{}", page().render())
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

    /// The point of Phase 4: `view!` emitting hyphenated attribute names. If
    /// this regresses, the page silently loses its test hooks and ARIA labels.
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
