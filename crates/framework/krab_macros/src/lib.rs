use proc_macro::TokenStream;
use quote::{quote, ToTokens};
use syn::{
    parse::{Parse, ParseStream},
    parse_macro_input, token, Expr, FnArg, GenericArgument, Ident, ItemFn, LitStr, PathArguments,
    Result, ReturnType, Token, Type, TypePath,
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

    let server_impl = if is_stream {
        quote! {
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
                let stream = #call_expr;
                axum::response::sse::Sse::new(stream).into_response()
            }

            #[cfg(not(target_arch = "wasm32"))]
            #[doc(hidden)]
            #vis fn #dispatch_handler_name(
                args: serde_json::Value,
            ) -> std::pin::Pin<Box<dyn std::future::Future<Output = axum::response::Response> + Send>> {
                use axum::response::IntoResponse;
                Box::pin(async move {
                    let __args = match serde_json::from_value::<#args_struct_name>(args.clone()) {
                        Ok(v) => v,
                        Err(e) => {
                            let msg = format!("Validation failed for '{}': {}. Payload: {}", stringify!(#fn_name), e, args);
                            return krab_core::server_fn::ServerFnError::validation(msg).into_response();
                        }
                    };
                    let stream = #call_expr;
                    axum::response::sse::Sse::new(stream).into_response()
                })
            }
        }
    } else {
        quote! {
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

            #[cfg(not(target_arch = "wasm32"))]
            #[doc(hidden)]
            #vis fn #dispatch_handler_name(
                args: serde_json::Value,
            ) -> std::pin::Pin<Box<dyn std::future::Future<Output = axum::response::Response> + Send>> {
                use axum::response::IntoResponse;
                Box::pin(async move {
                    let __args = match serde_json::from_value::<#args_struct_name>(args.clone()) {
                        Ok(v) => v,
                        Err(e) => {
                            let msg = format!("Validation failed for '{}': {}. Payload: {}", stringify!(#fn_name), e, args);
                            return krab_core::server_fn::ServerFnError::validation(msg).into_response();
                        }
                    };
                    match #call_expr {
                        Ok(result) => {
                            match serde_json::to_value(result) {
                                Ok(json) => (axum::http::StatusCode::OK, axum::Json(json)).into_response(),
                                Err(e) => krab_core::server_fn::ServerFnError::new(e.to_string()).into_response(),
                            }
                        }
                        Err(err) => err.into_response(),
                    }
                })
            }
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

#[proc_macro_attribute]
pub fn island(_attr: TokenStream, item: TokenStream) -> TokenStream {
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
            // Serialize props
            let props_json = serde_json::to_string(&#props_arg_name).unwrap_or_default();
            let boundary_id = krab_core::next_hydration_boundary_id(stringify!(#original_fn_name));
            let children =
                krab_core::annotate_hydration_tree(#inner_fn_name(#props_arg_name.clone()), &boundary_id);

            // Wrap in div
            krab_core::Node::Element(krab_core::Element {
                tag: "div".to_string(),
                attributes: vec![
                    krab_core::Attribute { name: "data-island".to_string(), value: stringify!(#original_fn_name).to_string() },
                    krab_core::Attribute { name: "data-props".to_string(), value: props_json },
                    krab_core::Attribute { name: "data-krab-boundary".to_string(), value: stringify!(#original_fn_name).to_string() },
                    krab_core::Attribute { name: "data-krab-boundary-id".to_string(), value: boundary_id },
                    krab_core::Attribute { name: "data-krab-boundary-state".to_string(), value: "ssr".to_string() },
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
}

struct Element {
    name: Ident,
    attributes: Vec<Attribute>,
    events: Vec<EventListener>,
    children: Vec<Node>,
}

struct Attribute {
    name: Ident,
    value: Expr,
}

struct EventListener {
    name: Ident,
    value: Expr,
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
                input.parse::<Token![<]>()?;
                input.parse::<Token![>]>()?;
                let mut children = Vec::new();
                while !input.peek(Token![<]) || !input.peek2(Token![/]) {
                    if input.peek(Token![<]) && input.peek2(Token![/]) {
                        break;
                    }
                    children.push(input.parse()?);
                }
                input.parse::<Token![<]>()?;
                input.parse::<Token![/]>()?;
                input.parse::<Token![>]>()?;
                Ok(Node::Fragment(children))
            } else {
                let element: Element = input.parse()?;
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
        let name: Ident = input.parse()?;

        let mut attributes = Vec::new();
        let mut events = Vec::new();
        loop {
            if input.peek(Token![>]) || input.peek(Token![/]) {
                break;
            }

            let attr_name: Ident = input.parse()?;
            let attr_name_str = attr_name.to_string();
            if attr_name_str == "on" && input.peek(Token![:]) {
                input.parse::<Token![:]>()?;
                let event_name: Ident = input.parse()?;
                input.parse::<Token![=]>()?;

                let value: Expr = if input.peek(token::Brace) {
                    let content;
                    syn::braced!(content in input);
                    content.parse()?
                } else {
                    return Err(input.error("Expected expression block for event handler"));
                };

                events.push(EventListener {
                    name: event_name,
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
                name: attr_name,
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

        let mut children = Vec::new();
        while !input.peek(Token![<]) || !input.peek2(Token![/]) {
            if input.peek(Token![<]) && input.peek2(Token![/]) {
                break;
            }
            children.push(input.parse()?);
        }

        input.parse::<Token![<]>()?;
        input.parse::<Token![/]>()?;
        let closing_name: Ident = input.parse()?;

        if closing_name != name {
            return Err(syn::Error::new(
                closing_name.span(),
                format!(
                    "Mismatched closing tag: expected </{}>, found </{}>",
                    name, closing_name
                ),
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
            Node::Element(el) => {
                let name = &el.name.to_string();
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
        let name = &self.name.to_string();
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
        let name = &self.name.to_string();
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
