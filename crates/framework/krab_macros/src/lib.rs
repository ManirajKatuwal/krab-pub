use proc_macro::TokenStream;
use proc_macro2::Span;
use quote::{quote, ToTokens};
use syn::{
    ext::IdentExt,
    parse::{Parse, ParseStream},
    parse_macro_input, token, Expr, FnArg, GenericArgument, Ident, ItemFn, LitInt, LitStr,
    PathArguments, Result, ReturnType, Token, Type, TypePath,
};

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
    let input_fn = parse_macro_input!(item as ItemFn);
    let attr_str = attr.to_string();
    let is_stream = attr_str.contains("stream");

    if let Err(err) = validate_server_attr(&attr_str, &input_fn) {
        return err.to_compile_error().into();
    }

    // Validate: must be async
    if input_fn.sig.asyncness.is_none() {
        return syn::Error::new_spanned(
            input_fn.sig.fn_token,
            "#[server] functions must be async. Add `async` before `fn`:\n  #[server]\n  pub async fn my_function(...) -> Result<T, ServerFnError> { ... }",
        )
        .to_compile_error()
        .into();
    }

    // Validate: must have a return type
    if matches!(input_fn.sig.output, syn::ReturnType::Default) {
        return syn::Error::new_spanned(
            input_fn.sig.fn_token,
            "#[server] functions must return Result<T, ServerFnError>.\n  Expected: async fn my_func() -> Result<MyType, ServerFnError> { ... }",
        )
        .to_compile_error()
        .into();
    }

    let fn_name = &input_fn.sig.ident;
    let fn_name_str = fn_name.to_string();
    let vis = &input_fn.vis;
    let output = &input_fn.sig.output;
    let block = &input_fn.block;
    let attrs = &input_fn.attrs;

    // Extract function arguments (skip self)
    let args: Vec<_> = input_fn
        .sig
        .inputs
        .iter()
        .filter_map(|arg| {
            if let syn::FnArg::Typed(pat_type) = arg {
                Some(pat_type)
            } else {
                None
            }
        })
        .collect();

    let arg_pats: Vec<_> = args.iter().map(|a| &a.pat).collect();
    let arg_types: Vec<_> = args.iter().map(|a| &a.ty).collect();
    let fn_inputs = &input_fn.sig.inputs;

    // Generate args struct name: get_user -> GetUserArgs
    let args_struct_name = Ident::new(
        &format!("__{}Args", to_pascal_case(&fn_name_str)),
        fn_name.span(),
    );

    // Handler function name: get_user -> get_user_handler
    let handler_fn_name = Ident::new(&format!("{}_handler", fn_name_str), fn_name.span());

    // Internal handler for dispatch: __get_user_handler
    let dispatch_handler_name = Ident::new(&format!("__{}_handler", fn_name_str), fn_name.span());

    let url = format!("/api/rpc/{}", fn_name_str);

    // Generate args struct (shared between server and client)
    let args_struct = if args.is_empty() {
        quote! {
            #[derive(serde::Serialize, serde::Deserialize)]
            #[allow(non_camel_case_types)]
            struct #args_struct_name {}
        }
    } else {
        quote! {
            #[derive(serde::Serialize, serde::Deserialize)]
            #[allow(non_camel_case_types)]
            struct #args_struct_name {
                #(#arg_pats: #arg_types),*
            }
        }
    };

    // Construct the call to the original function with destructured args
    let call_args: Vec<_> = arg_pats
        .iter()
        .map(|pat| {
            quote! { __args.#pat }
        })
        .collect();

    let call_expr = if call_args.is_empty() {
        quote! { #fn_name().await }
    } else {
        quote! { #fn_name(#(#call_args),*).await }
    };

    // How the handler turns the inner call into a response. The only thing that
    // actually differs between the two protocol shapes; argument decoding and
    // the dispatch shim are shared below.
    let respond = if is_stream {
        quote! {
            let stream = #call_expr;
            axum::response::sse::Sse::new(stream).into_response()
        }
    } else {
        quote! {
            match #call_expr {
                Ok(result) => {
                    match serde_json::to_value(result) {
                        Ok(json) => (axum::http::StatusCode::OK, axum::Json(json)).into_response(),
                        Err(e) => krab_core::server_fn::ServerFnError::new(e.to_string()).into_response(),
                    }
                }
                Err(err) => err.into_response(),
            }
        }
    };

    let server_impl = quote! {
        #[cfg(not(target_arch = "wasm32"))]
        #(#attrs)*
        #vis async fn #fn_name(#fn_inputs) #output
            #block

        #[cfg(not(target_arch = "wasm32"))]
        #vis async fn #handler_fn_name(
            axum::Json(__raw_args): axum::Json<serde_json::Value>,
        ) -> axum::response::Response {
            use axum::response::IntoResponse;
            let __args = match serde_json::from_value::<#args_struct_name>(__raw_args.clone()) {
                Ok(v) => v,
                Err(e) => {
                    let msg = format!("Validation failed for '{}': {}. Payload: {}", stringify!(#fn_name), e, __raw_args);
                    return krab_core::server_fn::ServerFnError::validation(msg).into_response();
                }
            };
            #respond
        }

        // The dispatch shim exists to give `collect_server_fns!` one uniform
        // signature to call. It delegates to the handler rather than repeating
        // it: argument decoding and response mapping used to be written out
        // once per (handler, dispatch) × (stream, non-stream) — four copies of
        // the same logic, where a fix to one silently missed the others.
        #[cfg(not(target_arch = "wasm32"))]
        #[doc(hidden)]
        #vis fn #dispatch_handler_name(
            args: serde_json::Value,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = axum::response::Response> + Send>> {
            Box::pin(#handler_fn_name(axum::Json(args)))
        }
    };

    // Client-side (WASM): replace body with fetch call
    let client_args_construction = if args.is_empty() {
        quote! { let __args = #args_struct_name {}; }
    } else {
        quote! {
            let __args = #args_struct_name {
                #(#arg_pats),*
            };
        }
    };

    let client_impl = quote! {
        #[cfg(target_arch = "wasm32")]
        #(#attrs)*
        #vis async fn #fn_name(#fn_inputs) #output {
            #client_args_construction
            krab_core::server_fn::call_server_fn(#url, &__args).await
        }
    };

    // Marker type carrying this function's registration metadata, consumed by
    // `krab_core::collect_server_fns!`. Declared as `struct name {}` so it
    // occupies only the type namespace and does not collide with the function
    // of the same name in the value namespace.
    let registration_impl = quote! {
        #[cfg(not(target_arch = "wasm32"))]
        #[doc(hidden)]
        #[allow(non_camel_case_types)]
        #vis struct #fn_name {}

        #[cfg(not(target_arch = "wasm32"))]
        impl krab_core::server_fn::ServerFn for #fn_name {
            const NAME: &'static str = #fn_name_str;
            const URL: &'static str = #url;
            fn dispatch(
                args: serde_json::Value,
            ) -> krab_core::server_fn::BoxFuture<axum::response::Response> {
                #dispatch_handler_name(args)
            }
        }
    };

    let output = quote! {
        #args_struct
        #server_impl
        #registration_impl
        #client_impl
    };

    TokenStream::from(output)
}

fn validate_server_attr(attr_str: &str, input_fn: &ItemFn) -> syn::Result<()> {
    let is_stream = attr_str.contains("stream");
    let trimmed = attr_str.trim();
    if !trimmed.is_empty() && trimmed != "stream" {
        return Err(syn::Error::new_spanned(
            &input_fn.sig.ident,
            "#[server] only supports no arguments or `stream` as an option",
        ));
    }

    // The expansion re-emits the function from its pieces — `#vis async fn
    // #fn_name(#fn_inputs) #output #block` — which silently drops
    // `sig.generics`. A generic server function therefore expanded into a body
    // referencing undeclared type parameters, and the user saw "cannot find
    // type `T` in this scope" pointing into generated code. Reject it here
    // instead, the way `#[island]` already does.
    if !input_fn.sig.generics.params.is_empty() {
        return Err(syn::Error::new_spanned(
            &input_fn.sig.generics,
            "#[server] functions cannot be generic.\n  \
             The generated handler deserializes one concrete argument struct and registers a single\n  \
             `ServerFn` implementation, so every argument type must be known at expansion time.\n  \
             Use concrete types, or an enum covering the cases you need.",
        ));
    }

    if let Some(where_clause) = &input_fn.sig.generics.where_clause {
        return Err(syn::Error::new_spanned(
            where_clause,
            "#[server] functions cannot carry a `where` clause; it is not reproduced in the generated handler.",
        ));
    }

    for arg in &input_fn.sig.inputs {
        if matches!(arg, FnArg::Receiver(_)) {
            return Err(syn::Error::new_spanned(
                arg,
                "#[server] methods with `self` are not supported. Use a free async function instead",
            ));
        }
    }

    if is_result_server_fn(&input_fn.sig.output) || is_stream {
        return Ok(());
    }

    Err(syn::Error::new_spanned(
        &input_fn.sig.output,
        "#[server] functions must return `Result<T, ServerFnError>` unless declared as `#[server(stream)]`",
    ))
}

fn is_result_server_fn(output: &ReturnType) -> bool {
    let ReturnType::Type(_, ty) = output else {
        return false;
    };

    let Type::Path(TypePath { path, .. }) = ty.as_ref() else {
        return false;
    };

    let Some(last) = path.segments.last() else {
        return false;
    };

    if last.ident != "Result" {
        return false;
    }

    let PathArguments::AngleBracketed(args) = &last.arguments else {
        return false;
    };

    let generic_args: Vec<&GenericArgument> = args.args.iter().collect();
    if generic_args.len() != 2 {
        return false;
    }

    matches!(generic_args[1], GenericArgument::Type(Type::Path(error_path)) if path_ends_with_server_fn_error(&error_path.path))
}

fn path_ends_with_server_fn_error(path: &syn::Path) -> bool {
    path.segments
        .last()
        .map(|segment| segment.ident == "ServerFnError")
        .unwrap_or(false)
}

/// Convert snake_case to PascalCase.
fn to_pascal_case(s: &str) -> String {
    s.split('_')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect()
}

/// `MyComponent` -> `my_component`, for suggesting the function to call in the
/// diagnostic emitted when `view!` sees a capitalised tag.
fn to_snake_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for (index, ch) in s.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if index != 0 {
                out.push('_');
            }
            out.extend(ch.to_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
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
    // Previously ignored outright, so `#[island(lazy)]` — or a typo'd option a
    // user expected to mean something — compiled and did nothing at all.
    if !attr.is_empty() {
        return syn::Error::new_spanned(
            proc_macro2::TokenStream::from(attr),
            "#[island] takes no arguments.\n  \
             Write `#[island]` on its own; hydration behaviour is not configurable per island.",
        )
        .to_compile_error()
        .into();
    }

    let mut input_fn = parse_macro_input!(item as ItemFn);
    let fn_name = &input_fn.sig.ident;
    let vis = &input_fn.vis;
    let inputs = &input_fn.sig.inputs;

    // Check for generics
    if !input_fn.sig.generics.params.is_empty() {
        return syn::Error::new_spanned(
            &input_fn.sig.generics,
            "Island components cannot be generic because they require concrete function registration for client-side hydration",
        )
        .to_compile_error()
        .into();
    }

    // Check for props argument
    let props_type = if let Some(syn::FnArg::Typed(pat_type)) = inputs.first() {
        &pat_type.ty
    } else {
        return syn::Error::new_spanned(
            inputs,
            "#[island] component must have exactly one props argument.\n  \
             Example:\n    #[island]\n    fn MyIsland(props: MyProps) -> krab_core::Node { ... }",
        )
        .to_compile_error()
        .into();
    };

    // Validate: must have exactly one argument
    if inputs.len() > 1 {
        return syn::Error::new_spanned(
            inputs,
            "#[island] component must have exactly one argument (props struct).\n  \
             Multiple arguments are not supported. Bundle them into a single props struct:\n    \
             #[derive(Clone, Serialize, Deserialize)]\n    \
             struct MyProps { field1: String, field2: i32 }\n    \
             #[island]\n    \
             fn MyIsland(props: MyProps) -> krab_core::Node { ... }",
        )
        .to_compile_error()
        .into();
    }

    let props_arg_name = if let Some(syn::FnArg::Typed(pat_type)) = inputs.first() {
        if let syn::Pat::Ident(pat_ident) = &*pat_type.pat {
            &pat_ident.ident
        } else {
            return syn::Error::new_spanned(
                inputs,
                "#[island] component argument must be a simple identifier, not a pattern.\n  \
                 Use: fn MyIsland(props: MyProps) instead of destructuring",
            )
            .to_compile_error()
            .into();
        }
    } else {
        return syn::Error::new_spanned(inputs, "#[island] component must have one argument")
            .to_compile_error()
            .into();
    };

    // Rename original function to inner
    let inner_fn_name = Ident::new(&format!("{}_impl", fn_name), fn_name.span());
    let original_fn_name = fn_name.clone();
    input_fn.sig.ident = inner_fn_name.clone();

    // Server implementation
    let server_impl = quote! {
        #[cfg(not(feature = "web"))]
        #vis fn #original_fn_name(#inputs) -> krab_core::Node {
            // A props value that will not serialize cannot hydrate. This used
            // to be `unwrap_or_default()`, which emitted `data-props=""` — the
            // browser then reported a *client* decode failure for a problem
            // that happened on the server, and the two were indistinguishable
            // in the boundary state. They are now distinct.
            //
            // The serde message is deliberately kept out of the markup: it is
            // derived from application data, and this string is served to every
            // visitor.
            let (props_json, boundary_state) = match serde_json::to_string(&#props_arg_name) {
                Ok(json) => (json, "ssr"),
                Err(_) => (String::new(), "props-encode-error"),
            };
            let boundary_id = krab_core::next_hydration_boundary_id(stringify!(#original_fn_name));
            // `props` is moved, not cloned: `to_string` above only borrowed it,
            // and nothing reads it afterwards. Cloning here put a `Clone` bound
            // on every island's props type for no reason.
            let children =
                krab_core::annotate_hydration_tree(#inner_fn_name(#props_arg_name), &boundary_id);

            // Wrap in div
            krab_core::Node::Element(krab_core::Element {
                tag: "div".to_string(),
                attributes: vec![
                    krab_core::Attribute { name: "data-island".to_string(), value: stringify!(#original_fn_name).to_string() },
                    krab_core::Attribute { name: "data-props".to_string(), value: props_json },
                    krab_core::Attribute { name: "data-krab-boundary".to_string(), value: stringify!(#original_fn_name).to_string() },
                    krab_core::Attribute { name: "data-krab-boundary-id".to_string(), value: boundary_id },
                    krab_core::Attribute { name: "data-krab-boundary-state".to_string(), value: boundary_state.to_string() },
                ],
                children: vec![children],
                events: vec![],
            })
        }
    };

    // Client implementation
    let client_impl = quote! {
        #[cfg(feature = "web")]
        #vis fn #original_fn_name(#inputs) -> krab_core::Node {
            #inner_fn_name(#props_arg_name)
        }
    };

    // Hydration handler
    let hydrate_fn_name = Ident::new(
        &format!("hydrate_{}", original_fn_name),
        original_fn_name.span(),
    );

    let hydration_handler = quote! {
        #[cfg(feature = "web")]
        #[allow(non_snake_case)]
        pub fn #hydrate_fn_name(props_json: String) -> krab_core::Node {
            match serde_json::from_str::<#props_type>(&props_json) {
                Ok(props) => #inner_fn_name(props),
                Err(err) => {
                    krab_client::log_hydration_diagnostic(
                        "island_decode",
                        stringify!(#original_fn_name),
                        &format!("prop decode failed: {}", err),
                    );
                    krab_core::Node::Element(krab_core::Element {
                        tag: "div".to_string(),
                        attributes: vec![
                            krab_core::Attribute {
                                name: "data-krab-boundary".to_string(),
                                value: stringify!(#original_fn_name).to_string(),
                            },
                            krab_core::Attribute {
                                name: "data-krab-boundary-state".to_string(),
                                value: "decode-error".to_string(),
                            },
                            krab_core::Attribute {
                                name: "role".to_string(),
                                value: "alert".to_string(),
                            },
                        ],
                        children: vec![krab_core::Node::Text(
                            "Hydration fallback rendered due to invalid props.".to_string(),
                        )],
                        events: vec![],
                    })
                }
            }
        }

        #[cfg(feature = "web")]
        inventory::submit! {
            krab_client::IslandDefinition {
                name: stringify!(#original_fn_name),
                factory: |props_json| #hydrate_fn_name(props_json),
            }
        }
    };

    let output = quote! {
        #[allow(non_snake_case)]
        #input_fn

        #server_impl

        #client_impl

        #hydration_handler
    };

    TokenStream::from(output)
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
///   converted with `to_string()`.
/// - **Event listeners** as `on:click={closure}`. These compile only under the
///   calling crate's `web` feature.
/// - **Text** as a string literal. A bare literal of any other kind is not
///   accepted — write `{42.to_string()}`, not `42`.
/// - **Expressions** in braces, converted through `krab_core::IntoNode`.
/// - **Fragments**, `<>...</>`, for a list of siblings with no wrapper element.
/// - **Control flow**: `<Show when={...} fallback={...}>` and
///   `<For each={...} key={...} view={...}/>`, which expand to
///   `krab_core::control_flow` calls rather than to markup. `key` on `<For>` is
///   mandatory; see [ADR 0008](https://github.com/ManirajKatuwal/krab-pub/blob/main/docs/adr/0008-view-control-flow.md).
///
/// ## What it does not
///
/// There is no component composition: a capitalised tag other than `Show` or
/// `For` is rejected rather than emitted as literal markup a browser would
/// ignore. Call the function and interpolate its node instead. See
/// [ADR 0006](https://github.com/ManirajKatuwal/krab-pub/blob/main/docs/adr/0006-view-component-composition.md).
#[proc_macro]
pub fn view(input: TokenStream) -> TokenStream {
    let node = parse_macro_input!(input as Node);
    TokenStream::from(quote! {
        #node
    })
}

enum Node {
    Element(Element),
    Text(LitStr),
    Expression(Expr),
    Fragment(Vec<Node>),
    /// `<Show>` / `<For>` — expands to a `krab_core::control_flow` call rather
    /// than to markup. See ADR 0008.
    ///
    /// Boxed: `ControlFlow` holds several `Expr`s and would otherwise make
    /// every `Node` — including a bare text node — as large as the biggest
    /// control-flow form. Trees are mostly ordinary markup, so the indirection
    /// is paid only where it is used.
    ControlFlow(Box<ControlFlow>),
}

enum ControlFlow {
    Show {
        when: Expr,
        fallback: Option<Expr>,
        children: Vec<Node>,
    },
    For {
        each: Expr,
        key: Expr,
        view: Expr,
    },
}

/// Tags `view!` expands into control flow instead of markup.
///
/// A closed set, deliberately. ADR 0006 rejects capitalised tags because they
/// would otherwise be emitted as literal markup that no browser renders, and
/// that rule stays safe only while the macro knows every capitalised name it
/// accepts. See ADR 0008.
const CONTROL_FLOW_TAGS: &[&str] = &["Show", "For"];

struct Element {
    name: String,
    attributes: Vec<Attribute>,
    events: Vec<EventListener>,
    children: Vec<Node>,
}

struct Attribute {
    name: String,
    value: Expr,
}

struct EventListener {
    name: String,
    value: Expr,
}

/// Parse an HTML tag or attribute name into its source text.
///
/// Grammar: `Ident (('-' | ':') (Ident | LitInt))*`
///
/// Tag and attribute names were parsed as a bare [`syn::Ident`], which cannot
/// represent most real HTML. Two separate consequences:
///
/// - A Rust identifier contains no `-` or `:`, so `data-testid`, `aria-label`,
///   `xlink:href`, and every custom element (`<my-widget>`) were unparseable.
///   The framework's own `#[island]` macro builds `data-island` and
///   `data-krab-boundary-id` by constructing `krab_core::Attribute` values
///   directly, because `view!` could not express them.
/// - Rust keywords are not `Ident`s to `syn`'s default parser, so `type`,
///   `for`, `as`, and `loop` were rejected — meaning no `<input type="text">`
///   and no `<label for="name">`. [`IdentExt::parse_any`] accepts them.
///
/// Returns the reassembled name and the span of its first segment, which is
/// what diagnostics point at.
fn parse_html_name(input: ParseStream) -> Result<(String, Span)> {
    let first = Ident::parse_any(input)?;
    let span = first.span();
    let mut name = strip_raw(&first);

    loop {
        // `::` is a path separator, never part of an HTML name. Leaving it to
        // the caller keeps `{some::path}` expressions parsing as before.
        let separator = if input.peek(Token![-]) {
            input.parse::<Token![-]>()?;
            '-'
        } else if input.peek(Token![:]) && !input.peek(Token![::]) {
            input.parse::<Token![:]>()?;
            ':'
        } else {
            break;
        };
        name.push(separator);

        if input.peek(Ident::peek_any) {
            name.push_str(&strip_raw(&Ident::parse_any(input)?));
        } else if input.peek(LitInt) {
            let segment: LitInt = input.parse()?;
            name.push_str(&segment.to_string());
        } else {
            return Err(input.error(format!(
                "expected a name segment after '{separator}' in '{name}'.\n  \
                 Names are made of segments joined by '-' or ':':\n    \
                 <div data-testid=\"x\">\n    \
                 <use xlink:href=\"#icon\"/>"
            )));
        }
    }

    Ok((name, span))
}

/// `r#type` reaches the parser with its raw prefix intact; HTML wants `type`.
fn strip_raw(ident: &Ident) -> String {
    let text = ident.to_string();
    text.strip_prefix("r#").unwrap_or(&text).to_string()
}

/// Parse child nodes up to the matching `</...>`.
///
/// The `is_empty` check is what makes an unclosed tag diagnosable. Without it
/// the loop hands an exhausted stream to [`Node::parse`], whose first act is to
/// reject an empty stream with "view! macro body is empty" — so
/// `view! { <div>"hi" }` reported that its body was empty, pointing at the
/// whole macro, rather than naming the tag that was never closed.
///
/// The loop condition is also the De Morgan dual of what it replaced
/// (`!peek(<) || !peek2(/)`), which is why the two copies of this loop each
/// carried a `break` on the negation of their own condition — unreachable in
/// both.
fn parse_children(
    input: ParseStream,
    open_tag: &str,
    close_tag: &str,
    open_span: Span,
) -> Result<Vec<Node>> {
    let mut children = Vec::new();
    while !(input.peek(Token![<]) && input.peek2(Token![/])) {
        if input.is_empty() {
            return Err(syn::Error::new(
                open_span,
                format!(
                    "unclosed `{open_tag}`: reached the end of the `view!` body without a matching `{close_tag}`.\n  \
                     Every element needs a closing tag, or `/>` if it has no children:\n    \
                     view! {{ {open_tag}\"text\"{close_tag} }}\n    \
                     view! {{ <img src=\"a.png\"/> }}"
                ),
            ));
        }
        children.push(input.parse()?);
    }
    Ok(children)
}

impl Element {
    /// Reinterpret a parsed `<Show>` / `<For>` element as control flow.
    ///
    /// Diagnostics matter more than usual here: these tags look like markup, so
    /// a missing attribute has to say which one and why, not just fail to
    /// compile somewhere inside the expansion.
    fn into_control_flow(self) -> Result<ControlFlow> {
        let span = Span::call_site();

        if !self.events.is_empty() {
            return Err(syn::Error::new(
                span,
                format!(
                    "<{}> is control flow, not an element, so it has no event listeners.
                       Put the handler on an element inside it.",
                    self.name
                ),
            ));
        }

        let take = |attributes: &mut Vec<Attribute>, wanted: &str| -> Option<Expr> {
            attributes
                .iter()
                .position(|attr| attr.name == wanted)
                .map(|index| attributes.remove(index).value)
        };

        let mut attributes = self.attributes;

        match self.name.as_str() {
            "Show" => {
                let when = take(&mut attributes, "when").ok_or_else(|| {
                    syn::Error::new(
                        span,
                        "<Show> requires `when`, a closure returning bool.\n  \
                         Example:\n  \
                           <Show when={move || logged_in.get()}>...</Show>",
                    )
                })?;
                let fallback = take(&mut attributes, "fallback");
                reject_unknown(&attributes, "Show", &["when", "fallback"], span)?;

                Ok(ControlFlow::Show {
                    when,
                    fallback,
                    children: self.children,
                })
            }
            "For" => {
                if !self.children.is_empty() {
                    return Err(syn::Error::new(
                        span,
                        "<For> renders each row through `view`, so it takes no children.\n  \
                         Move the markup into the `view` closure and close the tag with `/>`.",
                    ));
                }

                let each = take(&mut attributes, "each").ok_or_else(|| {
                    syn::Error::new(
                        span,
                        "<For> requires `each`, a closure returning the items.\n  \
                         Example: each={move || todos.get()}",
                    )
                })?;

                // Deliberately not optional. Falling back to positional keys
                // would silently reintroduce the behaviour <For> exists to
                // prevent: inserting a row shifts every row's identity, losing
                // focus and selection on all of them. See ADR 0008.
                let key = take(&mut attributes, "key").ok_or_else(|| {
                    syn::Error::new(
                        span,
                        "<For> requires `key`, a closure returning a stable, unique id per item.\n  \
                         Without it rows match by position, so inserting one row renumbers every\n  \
                         row after it and they lose focus and DOM state.\n  \
                         Example: key={|todo: &Todo| todo.id}",
                    )
                })?;

                let view = take(&mut attributes, "view").ok_or_else(|| {
                    syn::Error::new(
                        span,
                        "<For> requires `view`, a closure rendering one item.\n  \
                         Example: view={|todo: Todo| view! { <li>{todo.title}</li> }}",
                    )
                })?;

                reject_unknown(&attributes, "For", &["each", "key", "view"], span)?;

                Ok(ControlFlow::For { each, key, view })
            }
            other => Err(syn::Error::new(
                span,
                format!("unknown control-flow tag <{other}>"),
            )),
        }
    }
}

/// Reject attributes a control-flow tag does not understand.
///
/// Silently ignoring them would make a typo (`fallbck`) look like a working
/// fallback that never renders.
fn reject_unknown(attributes: &[Attribute], tag: &str, known: &[&str], span: Span) -> Result<()> {
    if let Some(unexpected) = attributes.first() {
        return Err(syn::Error::new(
            span,
            format!(
                "<{tag}> has no attribute `{}`. It accepts: {}.",
                unexpected.name,
                known.join(", ")
            ),
        ));
    }
    Ok(())
}

impl Parse for Node {
    fn parse(input: ParseStream) -> Result<Self> {
        if input.is_empty() {
            return Err(input.error(
                "view! macro body is empty. Provide at least one element, text, or expression:\n  \
                 view! { <div>\"Hello\"</div> }\n  \
                 view! { \"Static text\" }\n  \
                 view! { {my_variable} }",
            ));
        }

        if input.peek(Token![<]) {
            if input.peek2(Token![>]) {
                // Fragment <>...</>
                let open_span = input.span();
                input.parse::<Token![<]>()?;
                input.parse::<Token![>]>()?;
                let children = parse_children(input, "<>", "</>", open_span)?;
                input.parse::<Token![<]>()?;
                input.parse::<Token![/]>()?;
                input.parse::<Token![>]>()?;
                Ok(Node::Fragment(children))
            } else {
                let element: Element = input.parse()?;
                if CONTROL_FLOW_TAGS.contains(&element.name.as_str()) {
                    // Parsed as an ordinary element first, then reinterpreted:
                    // attribute and child parsing is identical, and only the
                    // meaning differs.
                    return Ok(Node::ControlFlow(Box::new(element.into_control_flow()?)));
                }
                Ok(Node::Element(element))
            }
        } else if input.peek(token::Brace) {
            let content;
            syn::braced!(content in input);
            let expr: Expr = content.parse()?;
            Ok(Node::Expression(expr))
        } else {
            let text: LitStr = input.parse()?;
            Ok(Node::Text(text))
        }
    }
}

impl Parse for Element {
    fn parse(input: ParseStream) -> Result<Self> {
        input.parse::<Token![<]>()?;
        let (name, name_span) = parse_html_name(input)?;

        // No HTML or SVG element name begins with an uppercase letter, so a
        // capitalised tag is always an attempt at component composition —
        // `<MyComponent/>`. `view!` does not support that: it would emit the
        // literal markup `<MyComponent>`, which no browser renders and no test
        // catches. Failing loudly beats silently producing broken HTML.
        //
        // Whether to support component composition is open; see
        // docs/adr/0006-view-component-composition.md.
        if name.starts_with(|c: char| c.is_ascii_uppercase())
            && !CONTROL_FLOW_TAGS.contains(&name.as_str())
        {
            return Err(syn::Error::new(
                name_span,
                format!(
                    "`view!` has no component composition, so <{name}> would be emitted as a \
                     literal HTML tag named '{name}'.\n  \
                     Call the function and interpolate its node instead:\n    \
                     view! {{ <div>{{{}(props)}}</div> }}\n  \
                     For an interactive component, annotate it with #[island].\n  \
                     The only capitalised tags `view!` knows are: {}.",
                    to_snake_case(&name),
                    CONTROL_FLOW_TAGS.join(", ")
                ),
            ));
        }

        let mut attributes = Vec::new();
        let mut events = Vec::new();
        loop {
            if input.peek(Token![>]) || input.peek(Token![/]) {
                break;
            }

            let (attr_name_str, attr_span) = parse_html_name(input)?;

            // `on:click` now parses as a single name, because ':' is a legal
            // separator. Event handlers are therefore recognised by prefix
            // after the fact, rather than by special-casing a bare `on`
            // followed by ':' during parsing — the old approach becomes
            // ambiguous once ':' can appear inside a name at all.
            if let Some(event_name) = attr_name_str.strip_prefix("on:") {
                if event_name.is_empty() {
                    return Err(syn::Error::new(
                        attr_span,
                        "event handler needs a name after 'on:', e.g. on:click={handler}",
                    ));
                }
                input.parse::<Token![=]>()?;

                let value: Expr = if input.peek(token::Brace) {
                    let content;
                    syn::braced!(content in input);
                    content.parse()?
                } else {
                    return Err(input.error(format!(
                        "Expected expression block for event handler '{attr_name_str}'.\n  \
                         Example: on:click={{move |_| count.set(count.get() + 1)}}"
                    )));
                };

                events.push(EventListener {
                    name: event_name.to_string(),
                    value,
                });
                continue;
            }

            input.parse::<Token![=]>()?;

            let value: Expr = if input.peek(LitStr) {
                let lit: LitStr = input.parse()?;
                syn::parse_quote!(#lit.to_string())
            } else if input.peek(token::Brace) {
                let content;
                syn::braced!(content in input);
                content.parse()?
            } else {
                return Err(input.error(format!(
                    "Expected string literal or {{expression}} for attribute '{}' value.\n  \
                             Examples:\n    \
                             <div class=\"my-class\">  (string literal)\n    \
                             <div class={{my_var}}>  (expression block)",
                    attr_name_str
                )));
            };

            attributes.push(Attribute {
                name: attr_name_str,
                value,
            });
        }

        if input.peek(Token![/]) {
            input.parse::<Token![/]>()?;
            input.parse::<Token![>]>()?;
            return Ok(Element {
                name,
                attributes,
                events,
                children: Vec::new(),
            });
        }

        input.parse::<Token![>]>()?;

        let children = parse_children(
            input,
            &format!("<{name}>"),
            &format!("</{name}>"),
            name_span,
        )?;

        input.parse::<Token![<]>()?;
        input.parse::<Token![/]>()?;
        // Parsed with the same grammar as the opening tag so the comparison is
        // on full names — `</my-widget>` must match `<my-widget>`, and the
        // diagnostic must print the hyphenated name rather than its first
        // segment.
        let (closing_name, closing_span) = parse_html_name(input)?;

        if closing_name != name {
            return Err(syn::Error::new(
                closing_span,
                format!("Mismatched closing tag: expected </{name}>, found </{closing_name}>"),
            ));
        }

        input.parse::<Token![>]>()?;

        Ok(Element {
            name,
            attributes,
            events,
            children,
        })
    }
}

impl ToTokens for Node {
    fn to_tokens(&self, tokens: &mut proc_macro2::TokenStream) {
        match self {
            // `as_ref()` rather than a box pattern, which is still unstable.
            Node::ControlFlow(control) => match control.as_ref() {
                ControlFlow::Show {
                    when,
                    fallback,
                    children,
                } => {
                    // Children are wrapped in a fragment so `<Show>` can hold
                    // more than one node without the user adding a container
                    // element.
                    let shown = quote! {
                        krab_core::Node::Fragment(vec![#(#children),*])
                    };
                    let fallback = match fallback {
                        Some(expr) => quote! { #expr },
                        // Rendering nothing is the sane default for a
                        // conditional, and an empty fragment produces no markup.
                        None => quote! { || krab_core::Node::Fragment(Vec::new()) },
                    };

                    tokens.extend(quote! {
                        krab_core::control_flow::show(#when, move || #shown, #fallback)
                    });
                }
                ControlFlow::For { each, key, view } => {
                    tokens.extend(quote! {
                        krab_core::control_flow::for_each(#each, #key, #view)
                    });
                }
            },
            Node::Element(el) => {
                let name = &el.name;
                let attrs = &el.attributes;
                let events = &el.events;
                let children = &el.children;
                tokens.extend(quote! {
                    krab_core::Node::Element(krab_core::Element {
                        tag: #name.to_string(),
                        attributes: vec![#(#attrs),*],
                        children: vec![#(#children),*],
                        events: vec![#(#events),*],
                    })
                });
            }
            Node::Text(text) => {
                tokens.extend(quote! {
                    krab_core::Node::Text(#text.to_string())
                });
            }
            Node::Expression(expr) => {
                // Expressions should evaluate to something that can be converted to a Node.
                // We use the `IntoNode` trait for this.
                tokens.extend(quote! {
                     krab_core::IntoNode::into_node(#expr)
                });
            }
            Node::Fragment(children) => {
                tokens.extend(quote! {
                    krab_core::Node::Fragment(vec![#(#children),*])
                });
            }
        }
    }
}

impl ToTokens for Attribute {
    fn to_tokens(&self, tokens: &mut proc_macro2::TokenStream) {
        let name = &self.name;
        let value = &self.value;
        tokens.extend(quote! {
            krab_core::Attribute {
                name: #name.to_string(),
                value: (#value).to_string(),
            }
        });
    }
}

impl ToTokens for EventListener {
    fn to_tokens(&self, tokens: &mut proc_macro2::TokenStream) {
        let name = &self.name;
        let value = &self.value;
        tokens.extend(quote! {
            #[cfg(feature = "web")]
            krab_core::EventListener {
                name: #name.to_string(),
                callback: std::rc::Rc::new(#value),
            }
        });
    }
}
