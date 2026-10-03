# Krab Framework: Architecture Design

## 1. High-Level Overview
Krab follows a "Server-First, Client-Opt-In" architecture. By default, pages are rendered as static HTML on the server. Client-side interactivity is added incrementally via "Islands of Interactivity".

## 2. Core Components

### A. The Server Runtime
- **Foundation**: Axum, on `hyper` (HTTP) and `tokio` (async runtime). The separate `krab_server` crate was removed ([ADR 0005](../adr/0005-krab-server-disposition.md)).
- **Router**: Axum routes. `service_frontend/build.rs` registers each `src/routes/<stem>.rs` module at `/<stem>` (see below).
- **SSR Engine**: Executes Rust components on the server to produce HTML strings — as one string, or progressively with `render_to_stream`, which flushes the shell with `<Suspense>` fallbacks and streams resolved boundaries in ([ADR 0017](../adr/0017-progressive-streaming-ssr.md)).
- **Data Loader**: `async` data fetching happens in the route handler, before rendering (or in a resource's server loader under `render_to_stream`). Island props reach the browser as JSON in the island's `data-props` attribute.

### B. The Client Runtime (Krab Client)
- **Target**: `wasm32-unknown-unknown`.
- **Reactivity System**: Fine-grained reactivity using Signals (similar to SolidJS/Leptos). A signal change re-renders only the affected dynamic region (`Node::Dynamic`) or reactive attribute, and the reconciler patches the DOM for that region (keyed for `<For>`); there is no whole-tree diff.
- **Hydration**: The client runtime only "wakes up" specific interactive components (Islands). Static HTML remains untouched.

### C. The Build System (Krab CLI)
- **Pure Rust Pipeline**: No Node.js or NPM dependencies.
- **Dual Compilation**:
    1.  Compiles the App for the Server (Native binary).
    2.  Compiles the "Islands" for the Client (WASM module).
- **Asset Pipeline**:
    - **Assets**: Fingerprinted (`krab build`, `krab dev --watch`) and served statically. There is no CSS or image processing step.

## 3. Detailed Subsystems

### File-System Routing
In `service_frontend`, file names determine the URL paths (`build.rs`).
```rust
src/
  routes/
    index.rs          // -> /
    about.rs          // -> /about
  api/
    users.rs          // -> /api/users (its `get` / `post` / … fns)
```

Discovery is flat: nested directories and `[param]` segments are not supported.
A dynamic route is an Axum route with a path parameter, declared in code.

### Islands Architecture Implementation
Components are standard Rust functions. To make a component interactive on the client, it must be marked.

```rust
// src/components/counter.rs

use krab_core::signal::create_signal;
use krab_core::IntoNode;
use krab_macros::{island, view};
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
pub struct CounterProps {
    pub initial: i32,
}

#[island] // This macro marks it for WASM compilation
pub fn Counter(props: CounterProps) -> krab_core::Node {
    let (count, set_count) = create_signal(props.initial);
    view! {
        <button on:click={move |_| set_count.update(|n| *n += 1)}>
            "Count: "
            {move || count.get().into_node()}
        </button>
    }
}
```

`#[island]` requires exactly one argument — a `Clone + Serialize + Deserialize`
props struct — and a `krab_core::Node` return type. Multiple arguments and
generic islands are rejected at compile time, because hydration needs a concrete
registration and a serialisable payload per boundary.

- **Server behavior**: Calls `Counter(CounterProps { initial })`, renders an HTML
  string wrapped in `data-island` / `data-krab-boundary` markers.
- **Client behavior**: Loads the application's single WASM bundle (the crate that defines the islands), then attaches event listeners to the existing HTML.

### Data Loading Pattern

**Implemented today.** A route module exports `pub async fn handler()`. The
build script (`services/service_frontend/build.rs`) discovers every
`src/routes/<stem>.rs` and registers `<module>::handler` at `/<stem>` — or `/`
for `index.rs`. Data loading happens inside the handler, which runs only on the
server. Per-route middleware is declared with a `//# middleware: name` comment.

```rust
// src/routes/profile.rs
//# middleware: require_auth

use axum::response::Html;
use krab_core::Render;
use krab_macros::view;

pub async fn handler() -> Html<String> {
    let user = load_user().await;

    Html(
        view! {
            <h1>{user.name}</h1>
        }
        .render(),
    )
}
```

> **Not yet implemented.** A separate `loader` convention — a server-only
> function whose typed return is injected into the page component, with dynamic
> `[id]`-style path segments — is a design goal, not current behaviour. There is
> no `loader` hook in the codebase today, and dynamic segments are handled by
> declaring an Axum path parameter in the handler itself.

## 4. State Management
- **Local State**: Signals (`create_signal`).
- **Shared State**: Context API — `krab_core::signal::provide_context` /
  `use_context`, looked up by type through a tree of owners. Every component
  called through a `view!` tag, every `#[island]`, and every effect and memo
  runs in an owner of its own, so an inner provide shadows an outer one for
  its subtree only, and an effect sees its creator's contexts on every re-run.
  Works the same in SSR and in the browser; owners are thread-local and
  `!Send`, like signals. See [ADR 0014](../adr/0014-context-api-and-owners.md).
- **Server State**: Request-scoped context. An SSR handler opens the request's
  scope with `with_owner(|| { provide_context(session); ... })`; there is no
  thread-wide fallback scope, so one request's values cannot reach the next
  request rendered on the same thread.

## 5. Security
- **CSRF Protection**: Built-in middleware.
- **Secure Headers**: Defaults to sensible HTTP security headers (CSP, HSTS).
- **Type-Safe SQL**: Encourages `sqlx` for compile-time checked queries.
