# Server Function Contract

Krab server functions are async Rust functions annotated with `#[server]`. On the server build, the function body remains available and the macro generates an Axum-compatible HTTP handler. On a WASM client build, the function body becomes a POST request to `/api/rpc/{function_name}`.

Once mounted, every server function is a public HTTP endpoint. Treat it like any other API route: validate inputs, enforce authentication and authorization, and return typed errors that can safely cross the wire.

## Supported Shapes

- Function form: free async functions only. Methods with `self` are rejected.
- Arguments: zero or more named arguments. Each argument must implement `serde::Serialize` and `serde::Deserialize`.
- Return type: non-streaming functions must return `Result<T, ServerFnError>`, where `T` is JSON serializable.
- Endpoint path: `/api/rpc/{function_name}`.
- Streaming form: `#[server(stream)]` returns an SSE stream and uses the same JSON argument decoding path.
- Error body: `ServerFnError` is emitted as a JSON envelope with `error`, `message`, `status_code`, and `code`.

## Validation Failure

```rust
use krab_core::server_fn::{validate_server_fn, ServerFnError};
use krab_macros::server;

#[server]
pub async fn rename_project(project_id: String, name: String) -> Result<String, ServerFnError> {
    validate_server_fn(!project_id.trim().is_empty(), "project_id is required")?;
    validate_server_fn(!name.trim().is_empty(), "name is required")?;
    validate_server_fn(name.len() <= 80, "name must be 80 characters or fewer")?;
    Ok(name)
}
```

## Authenticated Mutation

```rust
use krab_core::server_fn::{require_server_fn_auth, require_server_fn_scope, ServerFnError};
use krab_macros::server;

#[server]
pub async fn delete_project(
    authenticated: bool,
    scopes: Vec<String>,
    project_id: String,
) -> Result<(), ServerFnError> {
    require_server_fn_auth(authenticated, "login required")?;
    require_server_fn_scope(scopes, "projects:delete")?;

    // Perform the mutation after auth checks pass.
    let _ = project_id;
    Ok(())
}
```

## Typed Domain Error

```rust
use krab_core::server_fn::ServerFnError;

enum ProjectError {
    Missing,
    DuplicateName,
}

impl From<ProjectError> for ServerFnError {
    fn from(err: ProjectError) -> Self {
        match err {
            ProjectError::Missing => ServerFnError::not_found("project not found"),
            ProjectError::DuplicateName => ServerFnError::conflict("project name already exists"),
        }
    }
}
```

## Streaming

```rust
use axum::response::sse::Event;
use futures_util::stream;
use krab_macros::server;
use std::convert::Infallible;

#[server(stream)]
pub async fn project_events(project_id: String) -> impl futures_util::Stream<Item = Result<Event, Infallible>> {
    stream::iter([Ok(Event::default().event("project").data(project_id))])
}
```

## Mounting

```rust
use axum::Router;
use krab_core::{collect_server_fns, server_fn::server_fn_router};

static SERVER_FNS: &[krab_core::server_fn::ServerFnRegistration] =
    &collect_server_fns![rename_project, delete_project];

let app = Router::new().merge(server_fn_router(SERVER_FNS));
```

## Calling One From an Island

On a WASM build the call is a `fetch`, so it is `async` — but event handlers are
synchronous. Wrap it in an action rather than spawning by hand:

```rust
use krab_core::action::create_action;

let rename = create_action(|name: String| async move { rename_project(name).await });

view! {
    <button on:click={ move |_| rename.dispatch("new name".to_string()) }>
        "Rename"
    </button>
    <Show when={ move || rename.pending().get() }>
        <span class="pending">"Renaming…"</span>
    </Show>
}
```

Attribute values in `view!` are evaluated once at build time — they are not
reactive, so state like `pending` is reflected through `<Show>` or text
interpolation, not through a `disabled={ … }` closure (which does not compile).

`Action` exposes `pending`, `value`, and `error` as signals. A failed dispatch
keeps the previous `value`, and a superseded dispatch cannot overwrite a newer
one — so two rapid clicks leave the *latest* answer on screen even if the first
request finishes last.

No `#[cfg(target_arch)]` is required: an `#[island]` body compiles for both
targets and `create_action` exists on both. Server-side `dispatch` returns
without touching a signal, so SSR renders the idle state.

`krab_client::spawn` remains available for fire-and-forget calls that need no
observable state.
