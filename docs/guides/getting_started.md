# Getting Started

Install Krab, scaffold a project, and build a page that server-renders, hydrates
an island, and calls a server function.

Every command here was run against this version of the framework. Where a step
has a sharp edge, it is called out rather than left for you to hit.

**Prerequisites:** Rust stable 1.89+ ([rustup.rs](https://rustup.rs/)).

---

## 1. Install the CLI

```sh
cargo install krab_cli
krab --version
```

The package is `krab_cli`; the installed binary is **`krab`**. The crates.io
name `krab` was registered in 2023 by an unrelated crate, so the package could
not take it.

> Prefer building against a checkout (for example, to test an unreleased
> framework change)? Clone and install the CLI from the path instead:
>
> ```sh
> git clone https://github.com/ManirajKatuwal/krab-pub.git
> cargo install --path krab-pub/crates/tooling/krab_cli
> ```

---

## 2. Scaffold a project

```sh
krab new hello-krab
cd hello-krab
cp .env.example .env
cargo run
```

Five templates are available via `--template`: `default` (shown here), `saas`,
`edge-ssr`, `event-stream`, and `fullstack`.

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

**Text must be quoted.** `<h1>Hello</h1>` does not compile; `<h1>"Hello"</h1>`
does. Bare words are parsed as Rust, not text.

### Composing components

A component is a function taking one props struct and returning a `Node`. A
capitalised tag calls it, building `{Name}Props` from the attributes:

```rust
use krab_core::Node;
use krab_macros::view;

pub struct CardProps {
    pub title: String,
    pub children: Node,
}

#[allow(non_snake_case)] // component functions are capitalised, like islands
pub fn Card(props: CardProps) -> Node {
    view! { <section class="card"><h2>{props.title}</h2>{props.children}</section> }
}

let page = view! {
    <Card title="Inbox">
        <p>"You have mail"</p>
    </Card>
};
// renders: <section class="card"><h2>Inbox</h2><p>You have mail</p></section>
```

The rules, in full:

- **Lowercase tags are HTML, capitalised tags are components.** A path works
  too — `<ui::Card/>` calls `ui::Card` with a `ui::CardProps` — and the closing
  tag repeats it: `</ui::Card>`. An unqualified `<Card/>` needs both `Card`
  and `CardProps` in scope. `Show`, `For` and `Suspense` are reserved
  ([control flow](../adr/0008-view-control-flow-tags.md),
  [`<Suspense>`](../adr/0016-suspense-boundaries.md)).
- **Attributes are fields.** `aria-label="x"` sets `aria_label`; `type="x"`
  sets `r#type`. A misspelt attribute is rustc's "no field named ..." error,
  pointing at the attribute.
- **String literals convert, expressions do not.** `title="Inbox"` becomes
  `Into::into("Inbox")`, so it fills a `String`. `title={name}` is passed as
  written — `name` must already be the field's type.
- **Content between the tags is `children`**, a single `Node`. Omit the field
  from the props struct if the component takes none.
- **Every field is required**, unless you end the attributes with `..`:
  `<Button label="Save" ../>` fills the rest from `ButtonProps::default()`.
  `Node` implements `Default` (an empty fragment), so a props struct with
  `children` can `#[derive(Default)]`.
- **No `on:` on components** — a component has no element of its own to listen
  on. Pass the handler as a prop and attach it inside.

An `#[island]` has exactly this signature, so islands are used the same way —
`<Counter initial={0}/>` — and still render their hydration wrapper on the
server. Calling the function directly, `{Card(CardProps { ... })}`, remains
valid. See [ADR 0013](../adr/0013-view-component-tags.md).

### Sharing values with context

A value every component in a subtree needs — the session, the locale — does not
have to be threaded through props:

```rust
use krab_core::signal::{provide_context, use_context, with_owner};

#[derive(Clone)]
pub struct Locale(pub &'static str);

pub struct GreetingProps {}

#[allow(non_snake_case)]
pub fn Greeting(_props: GreetingProps) -> Node {
    let locale = use_context::<Locale>().map(|l| l.0).unwrap_or("en");
    view! { <p>{if locale == "fr" { "Bonjour" } else { "Hello" }}</p> }
}

async fn page() -> Html<String> {
    Html(with_owner(|| {
        provide_context(Locale("fr"));
        view! { <main><Greeting/></main> }.render()
    }))
}
```

`with_owner` opens a scope; `provide_context` stores a value in it by type;
`use_context` finds the nearest one above. Every component tag and island runs
in a scope of its own, so a component's `provide_context` reaches what it
renders and not its siblings. Outside any scope `provide_context` does nothing
(and logs `context_provided_without_owner`) — on a server, that is what stops
one request's values reaching the next.

Read contexts in the component body and move the value into closures; a
`Node::Dynamic` closure or event handler runs later, outside the scope. Effects
are the exception — they keep their scope across re-runs. Content passed
between a component's tags is built *before* the component runs, so it does not
see what that component provides. See
[ADR 0014](../adr/0014-context-api-and-owners.md).

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

Use it in a page like any other component: `view! { <div><Counter initial={0}/></div> }`.

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
   krab_core   = { version = "0.6", features = ["web"] }
   krab_client = { version = "0.6", features = ["web"] }
   inventory   = "0.3"
   wasm-bindgen = "0.2"

   [features]
   web = []
   ```

2. Build the bundle:

   ```sh
   wasm-pack build --target web -- --features web
   ```

3. Load it from your page. Importing the glue file does not hydrate anything —
   call `init()` and then `hydrate()` from a small module of your own:

   ```js
   // /app.js, loaded with <script type="module" src="/app.js"></script>
   import init, { hydrate } from '/pkg/hello_krab.js';
   await init();
   hydrate();
   ```

   Serve `pkg/` as static files, including its `snippets/` subdirectory (the
   per-island panic isolation ships there). Keep the bootstrap in a file rather
   than an inline `<script>`: Krab's security headers send
   `script-src 'self' 'wasm-unsafe-eval'`, which blocks inline scripts.

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
island calls it as a plain async fn. Event handlers are synchronous, so wrap the
call in an **action** — `create_action` turns an async operation into three
signals you can render directly:

```rust
use krab_core::action::create_action;

let add = create_action(|title: String| async move { add_task(title).await });

view! {
    <button on:click={ move |_| add.dispatch("from the island".to_string()) }>
        "Add task"
    </button>
    <Show when={ move || add.pending().get() }>
        <span class="pending">"Saving…"</span>
    </Show>
    <Show when={ move || add.error().get().is_some() }>
        <p class="error">{ move || add.error().get().unwrap_or_default() }</p>
    </Show>
}
```

**Attributes can be reactive too.** An attribute whose value is a closure
literal is re-evaluated whenever the signals it reads change, so the button can
disable itself while the request is in flight:

```rust
view! {
    <button
        disabled={ move || add.pending().get() }
        class={ move || if add.pending().get() { "btn busy" } else { "btn" } }
        on:click={ move |_| add.dispatch("from the island".to_string()) }
    >
        "Add task"
    </button>
}
```

The closure may return a string, a number, a `bool` — `true` renders the
attribute present and empty, `false` removes it — or an `Option`, where `None`
removes it. The server renders the current value; in the browser an effect
patches the attribute (and, for `value`, `checked` and `selected`, the form
control's live property). Only a closure *literal* is reactive: any other
expression is stringified once, when the element is built, and a closure held
in a variable must be written `{ move || f() }`. See
[ADR 0015](../adr/0015-reactive-attributes.md).

`pending` is true from `dispatch` until the request settles, `value` holds the
last success, and `error` holds the last failure. Two guarantees are worth
knowing because hand-rolled versions usually get them wrong:

- **A failed retry keeps the previous value.** Replacing rendered data with
  nothing because a refresh failed is worse than showing the last good value
  beside the error.
- **Only the newest dispatch can write.** Click twice and have the first request
  finish second, and the stale response is discarded rather than overwriting the
  newer one.

No `#[cfg(target_arch)]` is needed: an island body compiles for both targets and
`create_action` exists on both. On the server `dispatch` is inert — it returns
without touching a signal, so the SSR markup renders the idle state the browser
hydrates against.

`krab_client::spawn` is still there as the raw primitive for fire-and-forget work
that needs none of this state.

For the **read** side — data a component loads rather than writes — use
`create_resource`. It tracks a source and refetches when it changes:

```rust
use krab_core::resource::create_resource_with_initial;

// `props.user` was fetched by the async route handler and arrived through
// island props, so the server renders Ready and the client does NOT refetch
// on mount. Pass `None` (or use `create_resource`) to fetch on hydration.
let user = create_resource_with_initial(
    props.user,
    move || user_id.get(),
    |id| async move { fetch_user(id).await },
);

view! {
    <Show when={ move || user.state().get().is_pending() }>
        <p class="loading">"Loading…"</p>
    </Show>
    <p>{ move || user.value().get().map(|u| u.name).unwrap_or_default() }</p>
}
```

`state()` reports `Pending` / `Ready` / `Error`, and `value()` keeps the last
good data even through a failed refetch — a spinner beside stale data, never a
blank page. On the server a resource never polls its future: with an initial
value it renders `Ready`, without one `Pending`. See
[ADR 0009](../adr/0009-resource-ssr-semantics.md) for why data needed at first
paint belongs in the route handler, not a blocking render.

The alternative is to not block at all: wrap the slow part in
`<Suspense fallback={…}>`, give its resource a server loader with
`.with_server_loader(...)`, and render the page with
`krab_core::render_stream::render_to_stream`. The shell flushes with the
fallback and the resolved content streams in when the loader finishes, swapped
into place by the external `/_krab/stream.js` runtime. This works at page level
only, not inside an island. See [ADR 0016](../adr/0016-suspense-boundaries.md),
[ADR 0017](../adr/0017-progressive-streaming-ssr.md), and the `/streaming` page
of the reference frontend.

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

The deprecated aliases `db` and `grpc` (and the module alias `krab_core::grpc`)
were removed in 0.6.0; use `db-postgres`, `grpc-semantics` and
`krab_core::grpc_semantics`.

A feature-set mismatch is the most common first build failure: `cargo test -p
krab_core` with no features compiles a much smaller surface than CI runs.
