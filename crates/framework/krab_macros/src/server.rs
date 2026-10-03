//! `#[server]`: server functions as public RPC endpoints.
//!
//! The user-facing documentation lives on the entry point in `lib.rs`; this
//! module holds the expansion and the signature validation behind it.

use proc_macro::TokenStream;
use quote::quote;
use syn::{
    parse_macro_input, FnArg, GenericArgument, Ident, ItemFn, PathArguments, ReturnType, Type,
    TypePath,
};

pub(crate) fn expand(attr: TokenStream, item: TokenStream) -> TokenStream {
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
            let __args = match serde_json::from_value::<#args_struct_name>(__raw_args) {
                Ok(v) => v,
                Err(e) => {
                    // The request body is not echoed back: a rejected payload
                    // routinely carries the very credential that made it
                    // invalid, and error responses are among the most heavily
                    // logged objects in any stack. The deserializer's message is
                    // not returned verbatim either — serde embeds submitted
                    // values in it — but the schema facts inside it are what
                    // make the error actionable, so those are kept.
                    // `from_deserialization_error` draws that line.
                    return krab_core::server_fn::ServerFnError::from_deserialization_error(
                        stringify!(#fn_name),
                        &e,
                    )
                    .into_response();
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

    // A streaming function's server signature returns an SSE stream of
    // `axum` events, which neither exists nor deserializes on wasm32. Its
    // browser half returns the parsed event stream instead. Before 0.6.0 both
    // shapes called `call_server_fn`, whose `T: DeserializeOwned` bound made
    // every `#[server(stream)]` function a compile error on wasm32.
    let client_impl = if is_stream {
        quote! {
            #[cfg(target_arch = "wasm32")]
            #(#attrs)*
            #vis async fn #fn_name(#fn_inputs) -> ::std::result::Result<
                krab_core::server_fn::ServerEventStream,
                krab_core::server_fn::ServerFnError,
            > {
                #client_args_construction
                krab_core::server_fn::call_server_fn_stream(#url, &__args).await
            }
        }
    } else {
        quote! {
            #[cfg(target_arch = "wasm32")]
            #(#attrs)*
            #vis async fn #fn_name(#fn_inputs) #output {
                #client_args_construction
                krab_core::server_fn::call_server_fn(#url, &__args).await
            }
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
