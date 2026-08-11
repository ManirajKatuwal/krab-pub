# Krab Framework: Architecture Design

## 1. High-Level Overview
Krab follows a "Server-First, Client-Opt-In" architecture. By default, pages are rendered as static HTML on the server. Client-side interactivity is added incrementally via "Islands of Interactivity".

## 2. Core Components

### A. The Server Runtime (Krab Server)
- **Foundation**: Built on top of `hyper` (HTTP) and `tokio` (Async Runtime).
- **Router**: A trie-based router that maps URLs to file-system paths.
- **SSR Engine**: Executes Rust components on the server to produce HTML strings.
- **Data Loader**: Handles `async` data fetching on the server before rendering. Data is serialized (e.g., via `serde_json` or `rkyv`) and embedded in the HTML for hydration.

### B. The Client Runtime (Krab Client)
- **Target**: `wasm32-unknown-unknown`.
- **Reactivity System**: Fine-grained reactivity using Signals (similar to SolidJS/Leptos). No Virtual DOM diffing; updates are direct DOM manipulations.
- **Hydration**: The client runtime only "wakes up" specific interactive components (Islands). Static HTML remains untouched.

### C. The Build System (Krab CLI)
- **Pure Rust Pipeline**: No Node.js or NPM dependencies.
- **Dual Compilation**:
    1.  Compiles the App for the Server (Native binary).
    2.  Compiles the "Islands" for the Client (WASM module).
- **Asset Pipeline**:
    - **CSS**: Processed by `lightningcss` (Rust-based) for minification, prefixing, and syntax lowering.
    - **Images**: Optimized via the `image` crate (WebP conversion).
    - **Assets**: Fingerprinted and served statically.

## 3. Detailed Subsystems

### File-System Routing
Directory structure determines the URL paths.
```rust
src/
  routes/
    index.rs          // -> /
    about.rs          // -> /about
    blog/
      index.rs        // -> /blog
      [slug].rs       // -> /blog/:slug (Dynamic Route)
    api/
      users.rs        // -> /api/users (API Endpoint)
```

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
- **Client behavior**: Downloads `counter.wasm` (or a chunk), attaches event listeners to the existing HTML.

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
- **Global Client State**: Context API (dependency injection for deep trees).
- **Server State**: Request-scoped context (for Headers, User Session).

## 5. Security
- **CSRF Protection**: Built-in middleware.
- **Secure Headers**: Defaults to sensible HTTP security headers (CSP, HSTS).
- **Type-Safe SQL**: Encourages `sqlx` for compile-time checked queries.
