//! `#[island]`: components that hydrate in the browser.
//!
//! The user-facing documentation lives on the entry point in `lib.rs`; this
//! module holds the expansion into its server and browser halves.

use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, Ident, ItemFn};

pub(crate) fn expand(attr: TokenStream, item: TokenStream) -> TokenStream {
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
            //
            // The body runs in a context scope of its own (ADR 0014), on the
            // server as in the browser below, so what it provides reaches the
            // components it renders and nothing else.
            let children = krab_core::annotate_hydration_tree(
                krab_core::signal::with_owner(move || #inner_fn_name(#props_arg_name)),
                &boundary_id,
            );

            // Wrap in div
            krab_core::Node::Element(krab_core::Element {
                tag: "div".to_string(),
                attributes: vec![
                    krab_core::Attribute::new("data-island".to_string(), stringify!(#original_fn_name).to_string()),
                    krab_core::Attribute::new("data-props".to_string(), props_json),
                    krab_core::Attribute::new("data-krab-boundary".to_string(), stringify!(#original_fn_name).to_string()),
                    krab_core::Attribute::new("data-krab-boundary-id".to_string(), boundary_id),
                    krab_core::Attribute::new("data-krab-boundary-state".to_string(), boundary_state.to_string()),
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
            krab_core::signal::with_owner(move || #inner_fn_name(#props_arg_name))
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
                // Hydration calls this outside any scope, so the island opens
                // its own: without one, `provide_context` in the body would be
                // a no-op in the browser while working on the server.
                Ok(props) => krab_core::signal::with_owner(move || #inner_fn_name(props)),
                Err(err) => {
                    krab_client::log_hydration_diagnostic(
                        "island_decode",
                        stringify!(#original_fn_name),
                        &format!("prop decode failed: {}", err),
                    );
                    krab_core::Node::Element(krab_core::Element {
                        tag: "div".to_string(),
                        attributes: vec![
                            krab_core::Attribute::new(
                                "data-krab-boundary".to_string(),
                                stringify!(#original_fn_name).to_string(),
                            ),
                            krab_core::Attribute::new(
                                "data-krab-boundary-state".to_string(),
                                "decode-error".to_string(),
                            ),
                            krab_core::Attribute::new(
                                "role".to_string(),
                                "alert".to_string(),
                            ),
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
