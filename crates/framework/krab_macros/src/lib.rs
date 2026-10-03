//! Procedural macros for Krab: `view!`, `#[island]`, and `#[server]`.
//!
//! This file holds only the `#[proc_macro*]` entry points and their
//! documentation. A proc-macro crate can export nothing but these functions,
//! so the expansions live in private modules, one per macro, where each can
//! grow without the others scrolling past.

use proc_macro::TokenStream;

mod island;
mod server;
mod view;

// ── Server Function Macro ───────────────────────────────────────────────────

/// Marks an async function as a server function.
///
/// On the **server**, the function body is preserved and an Axum handler
/// function (`{fn_name}_handler`) is generated alongside it. Once mounted,
/// a server function is a public HTTP POST endpoint and must perform its own
/// validation and authorization checks.
///
/// On the **client** (WASM), the function body is replaced with a `fetch`
/// call to `/api/rpc/{fn_name}`, transparently calling the server.
///
/// ## Requirements
///
/// - The function must be `async`.
/// - The return type must be `Result<T, ServerFnError>` where `T: Serialize + Deserialize`.
/// - All arguments must implement `Serialize + Deserialize`.
/// - The expansion references `axum`, `serde`, `serde_json`, and `krab_core` by
///   path, so the calling crate must have all four as direct dependencies.
/// - `krab_core` must be built with the `rest` feature. On non-wasm targets the
///   expansion implements `krab_core::server_fn::ServerFn`, and that trait is
///   gated behind `rest`; without it the impl fails to resolve. A proc macro
///   cannot observe the calling crate's feature flags, so this cannot be
///   detected at expansion time — it surfaces as a missing-trait error.
///
/// Alongside the function, the macro generates an Axum handler
/// (`{name}_handler`), a dispatch shim (`__{name}_handler`), and a hidden
/// marker type named `{name}` implementing `krab_core::server_fn::ServerFn`.
/// The marker is what `krab_core::collect_server_fns!` resolves; it is
/// declared as `struct {name} {}` so it occupies only the type namespace and
/// does not collide with the function itself.
///
/// (Not an intra-doc link: `krab_core` is a dev-dependency of this crate — a
/// proc-macro crate cannot take it as a normal one — so rustdoc cannot resolve
/// a path into it.)
///
/// ## Example
///
/// ```rust
/// use krab_core::server_fn::ServerFnError;
/// use krab_macros::server;
/// use serde::{Deserialize, Serialize};
///
/// #[derive(Serialize, Deserialize)]
/// pub struct User {
///     pub id: String,
///     pub name: String,
/// }
///
/// #[server]
/// pub async fn get_user(id: String) -> Result<User, ServerFnError> {
///     find_user(&id).await
///         .map_err(|e| ServerFnError::new(e.to_string()))
/// }
///
/// // On the server, wire it into your router:
/// // .route("/api/rpc/get_user", post(get_user_handler))
///
/// # async fn find_user(id: &str) -> Result<User, std::io::Error> {
/// #     Ok(User { id: id.to_string(), name: "Ada".to_string() })
/// # }
/// ```
#[proc_macro_attribute]
pub fn server(attr: TokenStream, item: TokenStream) -> TokenStream {
    server::expand(attr, item)
}

/// Marks a component function as a hydration island.
///
/// An island is the unit of interactivity in a Krab page: the server renders it
/// to HTML like any other component, and the browser re-runs just that subtree
/// against the markup already on the page.
///
/// The macro expands to two halves selected by **the calling crate's** `web`
/// feature — not `krab_client`'s:
///
/// - without `web`: the SSR half, wrapping the rendered tree in a `<div>`
///   carrying `data-island`, the serialized `data-props`, and the
///   `data-krab-boundary*` markers the client walks.
/// - with `web`: the browser half, which calls the component directly and
///   registers a hydration factory through `inventory` so `hydrate()` can find
///   it by name.
///
/// A crate that never enables `web` therefore gets a server-only island, and
/// its WASM bundle registers no hydrators. See
/// `docs/guides/troubleshooting.md` for the symptoms that produces.
///
/// ## Requirements
///
/// - Exactly one argument, a props struct bound to a plain identifier.
/// - The props type must implement `Serialize` (server half) and
///   `Deserialize` (browser half).
/// - The function must return `krab_core::Node`.
/// - No generic parameters: hydration resolves components by name at runtime,
///   which needs one concrete registration per island.
/// - The expansion references `krab_core`, `krab_client`, `serde_json`, and
///   `inventory` by path, so the calling crate needs all four as direct
///   dependencies.
///
/// ## Example
///
/// ```ignore
/// use krab_macros::island;
/// use serde::{Deserialize, Serialize};
///
/// #[derive(Serialize, Deserialize)]
/// pub struct CounterProps {
///     pub start: i32,
/// }
///
/// #[island]
/// fn Counter(props: CounterProps) -> krab_core::Node {
///     view! { <button>{props.start.to_string()}</button> }
/// }
/// ```
#[proc_macro_attribute]
pub fn island(attr: TokenStream, item: TokenStream) -> TokenStream {
    island::expand(attr, item)
}

/// Builds a `krab_core::Node` tree from HTML-like syntax.
///
/// ```ignore
/// view! {
///     <div class="card" data-testid="greeting">
///         <label r#for="name">"Name"</label>
///         <input type="text" value={current.get()}/>
///         <button on:click={move |_| count.set(count.get() + 1)}>
///             {count.get().to_string()}
///         </button>
///     </div>
/// }
/// ```
///
/// ## What the syntax accepts
///
/// - **Elements**, with children or self-closed (`<img src="a.png"/>`).
///   Hyphenated and namespaced names work (`<my-widget>`, `xlink:href`), as do
///   Rust keywords used as HTML names (`type`, `for`).
/// - **Attributes** as a string literal or a `{expression}`. Values are
///   converted with `to_string()` once, when the tree is built.
/// - **Reactive attributes**: when the braced value is a *closure literal*,
///   `disabled={move || busy.get()}`, the attribute is dynamic
///   (`krab_core::Attribute::dynamic`). The server renders its value at render
///   time; in the browser an effect re-evaluates it whenever the signals it
///   reads change and patches the DOM. The closure may return a string, a
///   number, a `bool` (`true` → present and empty, `false` → removed), or an
///   `Option` (`None` → removed). `value`, `checked` and `selected` are also
///   written to the element's DOM property, since the attribute alone does not
///   change what a form control shows once the user has touched it. The test
///   is syntactic: a closure stored in a variable is not recognised — write
///   `{move || f()}`. See [ADR 0015](https://github.com/ManirajKatuwal/krab-pub/blob/main/docs/adr/0015-reactive-attributes.md).
/// - **Event listeners** as `on:click={closure}`. These compile only under the
///   calling crate's `web` feature.
/// - **Text** as a string literal. A bare literal of any other kind is not
///   accepted — write `{42.to_string()}`, not `42`.
/// - **Expressions** in braces, converted through `krab_core::IntoNode`.
/// - **Fragments**, `<>...</>`, for a list of siblings with no wrapper element.
/// - **Control flow**: `<Show when={...} fallback={...}>` and
///   `<For each={...} key={...} view={...}/>`, which expand to
///   `krab_core::control_flow` calls rather than to markup. `key` on `<For>` is
///   mandatory; see [ADR 0008](https://github.com/ManirajKatuwal/krab-pub/blob/main/docs/adr/0008-view-control-flow-tags.md).
/// - **Suspense**: `<Suspense fallback={|| view! { ... }}>children</Suspense>`
///   expands to `krab_core::suspense::suspense`. Resources created while its
///   children are built register with it, and it shows `fallback` until every
///   one has a first value. `fallback` is required. On the server it renders
///   synchronously (children with initial data, fallback without) inside
///   `<!--krab:suspense:{id}:…-->` markers; under
///   `krab_core::render_stream::render_to_stream` its content can stream in
///   later. See [ADR 0016](https://github.com/ManirajKatuwal/krab-pub/blob/main/docs/adr/0016-suspense-boundaries.md).
/// - **Components**: any other tag whose name (or last path segment) starts
///   with an uppercase letter. See below.
///
/// ## Components
///
/// A capitalised tag calls the function of that name with a props struct named
/// after it — the same `fn Name(props: NameProps) -> krab_core::Node` shape
/// `#[island]` requires, so an island works as a tag too:
///
/// ```ignore
/// pub struct CardProps {
///     pub title: String,
///     pub count: i32,
///     pub children: krab_core::Node,
/// }
///
/// #[allow(non_snake_case)]
/// pub fn Card(props: CardProps) -> krab_core::Node {
///     view! { <section><h2>{props.title}</h2>{props.children}</section> }
/// }
///
/// view! {
///     <Card title="Inbox" count={3}>
///         <p>"You have mail"</p>
///     </Card>
/// }
/// // expands to (the call itself runs inside `krab_core::signal::with_owner`,
/// // giving the component its own context scope)
/// Card(CardProps {
///     title: Into::into("Inbox"),
///     count: 3,
///     children: view! { <p>"You have mail"</p> },
/// })
/// ```
///
/// - The tag may be a path, `<ui::Card/>`; the props type is then
///   `ui::CardProps`. The closing tag must repeat the path exactly.
/// - Attributes are struct fields. `aria-label` sets `aria_label`, `type` sets
///   `r#type`.
/// - A **string literal** value is converted with `Into::into`, so it fills a
///   `String` field. A **`{expression}`** is passed through unchanged, so
///   coercions such as `Box<dyn Fn()>` and integer inference behave exactly as
///   in a hand-written struct literal.
/// - Content between the tags becomes the `children: krab_core::Node` field
///   (several children arrive as one fragment). A self-closed tag sets no
///   `children`.
/// - Every field must be written out, unless the tag contains a bare `..`
///   (`<Button label="Go" ../>`), which fills the rest from the props type's
///   `Default`.
/// - `on:` listeners and other namespaced attributes are rejected on a
///   component: it has no DOM node of its own. Pass a handler as a prop.
/// - `Show`, `For` and `Suspense` are reserved; a component with one of those
///   names must be written with a path.
///
/// See [ADR 0013](https://github.com/ManirajKatuwal/krab-pub/blob/main/docs/adr/0013-view-component-tags.md).
#[proc_macro]
pub fn view(input: TokenStream) -> TokenStream {
    view::expand(input)
}
