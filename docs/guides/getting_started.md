# Getting Started

Install Krab, scaffold a project, and build a page that server-renders, hydrates
an island, and calls a server function.

Every command here was run against this version of the framework. Where a step
has a sharp edge, it is called out rather than left for you to hit.

**Prerequisites:** Rust stable 1.75+ ([rustup.rs](https://rustup.rs/)).

---

## 1. Install the CLI

```sh
cargo install krab_cli
krab --version
```

The package is `krab_cli`; the installed binary is **`krab`**. The crates.io
name `krab` was registered in 2023 by an unrelated crate, so the package could
not take it.

> **Not published yet.** Until the first crates.io release, install from a
> checkout:
>
> ```sh
> git clone https://github.com/krab-framework/krab.git
> cargo install --path krab/crates/tooling/krab_cli
> ```

---

## 2. Scaffold a project

```sh
krab new hello-krab
cd hello-krab
cp .env.example .env
cargo run
```

Four templates are available via `--template`: `default` (shown here), `saas`,
`edge-ssr`, and `event-stream`.

You get a service on `http://127.0.0.1:3000` with `/`, `/health`, and `/ready`,
plus a `krab.toml`, a `Dockerfile`, a Kubernetes manifest, and a CI workflow.

> **Working against a local checkout?** Pass `--path-deps`:
>
> ```sh
> krab new hello-krab --path-deps /path/to/krab
> ```
>
> This writes path dependencies instead of crates.io versions. It is what the
> [`generated-project`](../../.github/workflows/generated-project.yaml) CI gate
> uses to build scaffolded output before a release exists.

---

## 3. Your first page with `view!`

`view!` is Krab's HTML macro. It returns a `krab_core::Node`, which `.render()`
turns into a string.

Add to `src/main.rs`:

```rust
use axum::response::Html;
use krab_core::Render;
use krab_macros::view;

async fn page() -> Html<String> {
    let node = view! {
        <html lang="en">
            <body data-app="hello-krab">
                <h1>"Hello from Krab"</h1>
                <p class="intro" aria-label="intro">"Server-rendered with view!."</p>
            </body>
        </html>
    };
    Html(format!("<!doctype html>{}", node.render()))
}
```

and register it:

```rust
let app = Router::new()
    .route("/", get(index))
    .route("/page", get(page));
```

`cargo run`, then open `http://127.0.0.1:3000/page`.

### Two things `view!` will not let you do

**Text must be quoted.** `<h1>Hello</h1>` does not compile; `<h1>"Hello"</h1>`
does. Bare words are parsed as Rust, not text.

**Capitalised tags are a compile error.**

```rust
view! { <MyComponent/> }   // error
```

`view!` has no component composition — a capitalised tag would be emitted as a
literal `<MyComponent>` element that no browser renders, so it is rejected with
a diagnostic instead. Compose by calling the function and interpolating:

```rust
view! { <div>{my_component(props)}</div> }
```

See [ADR 0006](../adr/0006-view-component-composition.md).

---

## 4. Your first island

An **island** is a component that server-renders and then hydrates in the
browser. Mark it with `#[island]`. It takes exactly one argument — a props
struct that is `Clone + Serialize + Deserialize`.

```rust
use krab_core::signal::*;
use krab_core::{IntoNode, Node};
use krab_macros::island;
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
pub struct CounterProps {
    pub initial: i32,
}

#[island]
#[allow(non_snake_case)]
pub fn Counter(props: CounterProps) -> Node {
    let (count, _set_count) = create_signal(props.initial);
    view! {
        <button on:click={ move |_| _set_count.update(|c| *c += 1) }>
            "Count: " <span>{ move || count.get().into_node() }</span>
        </button>
    }
}
```

Interpolate it into a page like any other node: `view! { <div>{Counter(CounterProps { initial: 0 })}</div> }`.

Server-side, that renders a wrapper carrying the hydration markers the browser
runtime looks for — `data-island`, `data-props`, `data-krab-boundary`,
`data-krab-boundary-id`, `data-krab-boundary-state`.

### Wiring the browser half

The `default` template is server-only, so this part needs three additions.

1. Make the crate a library as well as a binary, and add the browser
   dependencies:

   ```toml
   [lib]
   crate-type = ["cdylib", "rlib"]

   [target.'cfg(target_arch = "wasm32")'.dependencies]
   krab_core   = { version = "0.2", features = ["web"] }
   krab_client = { version = "0.2", features = ["web"] }
   inventory   = "0.3"
   wasm-bindgen = "0.2"

   [features]
   web = []
   ```

2. Build the bundle:

   ```sh
   wasm-pack build --target web -- --features web
   ```

3. Load it from your page: `<script type="module" src="/pkg/hello_krab.js"></script>`,
   and serve `pkg/` as static files.

> **`--features web` is required, and only valid for `wasm32`.** `#[island]`
> selects its browser half on that feature alone, but that half needs
> `krab_client`, which is a wasm32-only dependency. Enabling `web` for a native
> build will not compile.

Signals (`create_signal`, `.get()`, `.update()`) are `!Send` — they use `Rc`
internally. Do not hold a `Node` across an `.await`; build and consume it inside
synchronous sections. See
[signal_safety.md](../architecture/signal_safety.md).

---

## 5. Your first server function

`#[server]` turns an async function into an HTTP endpoint at
`/api/rpc/<fn_name>`. It must be `async` and return
`Result<T, ServerFnError>`.

```rust
use krab_core::server_fn::ServerFnError;
use krab_macros::server;

#[derive(serde::Serialize, serde::Deserialize)]
pub struct TaskSummary {
    pub title: String,
}

#[server]
pub async fn add_task(title: String) -> Result<TaskSummary, ServerFnError> {
    let trimmed = title.trim();
    if trimmed.is_empty() {
        return Err(ServerFnError::validation("title must not be empty".into()));
    }
    Ok(TaskSummary { title: trimmed.to_string() })
}
```

The macro generates `add_task_handler`. Mount it, matching the URL the client
half calls:

```rust
use axum::routing::post;

let app = Router::new().route("/api/rpc/add_task", post(add_task_handler));
```

On `wasm32` the same `add_task(...)` call becomes a `fetch` to that URL, so an
island calls it as a plain async fn. Event handlers are synchronous, so bridge
with `spawn_local`:

```rust
on:click={
    move |_| {
        #[cfg(target_arch = "wasm32")]
        wasm_bindgen_futures::spawn_local(async move {
            let _ = add_task("from the island".to_string()).await;
        });
    }
}
```

> **Server functions are public HTTP endpoints.** They are not privileged
> because they look like function calls. Validate input and check authorisation
> inside the function — `krab_core::server_fn` provides
> `require_server_fn_auth` and `require_server_fn_scope`. See
> [ADR 0003](../adr/0003-server-functions-public-endpoints.md) and
> [server_functions.md](../reference/server_functions.md).

---

## 6. See it all working

Everything above is assembled and tested in the vendored reference application:

```sh
cargo run  -p reference_app_islands_rpc --bin islands_rpc_server   # http://127.0.0.1:3100
cargo test -p reference_app_islands_rpc
```

[`examples/reference_apps/islands_rpc/src/lib.rs`](../../examples/reference_apps/islands_rpc/src/lib.rs)
is one file containing the page, two islands with different props, and the
server function one of them calls. It is a workspace member, so CI builds it,
tests it, and builds its WASM bundle on every change.

---

## Where to go next

| You want to | Read |
|---|---|
| Understand SSR, islands, and hydration | [architecture/hydration.md](../architecture/hydration.md) |
| Control per-route rendering | [architecture/render_policy.md](../architecture/render_policy.md) |
| Add a database | [reference/database.md](../reference/database.md) |
| Configure the runtime | [reference/environment.md](../reference/environment.md) |
| Set up auth and secrets | [reference/security.md](../reference/security.md) |
| Run more than one service | [architecture/service_composition.md](../architecture/service_composition.md) |
| Deploy | [reference/deployment.md](../reference/deployment.md) |
| Everything | [docs/README.md](../README.md) |

## Feature flags

`krab_core` ships **no default features**. Enable what you use:

| Feature | Gives you |
|---|---|
| `rest` | Axum HTTP layer, JWT auth, middleware, `server_fn` handlers |
| `graphql` | `async-graphql` integration |
| `auth` | Argon2id password hashing, `CredentialStore` |
| `db-postgres` | Postgres + full migration governance |
| `db-sqlite` | SQLite driver |
| `redis-store` | Redis-backed distributed store |
| `web` | Browser bindings, for the wasm32 half |
| `grpc-semantics` | gRPC status-code and timeout vocabulary for a gateway — **not a transport** |

Deprecated aliases, removable no earlier than `0.3.0`: `db` → `db-postgres`,
`grpc` → `grpc-semantics`.

A feature-set mismatch is the most common first build failure: `cargo test -p
krab_core` with no features compiles a much smaller surface than CI runs.
