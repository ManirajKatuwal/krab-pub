// Should fail: #[server] functions cannot be generic.
//
// The expansion rebuilds the function from `sig` piece by piece and never
// re-emits `sig.generics`, so this used to expand into a body referencing an
// undeclared `T` and report "cannot find type `T` in this scope" against
// generated code.
//
// The macro replaces the function with its error, so the return type's import
// goes unused; that warning is not what this case asserts.
#![allow(unused_imports)]

use krab_core::server_fn::ServerFnError;
use krab_macros::server;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct Wrapper {
    pub id: String,
}

#[server]
pub async fn store<T: Serialize + Send>(value: T) -> Result<Wrapper, ServerFnError> {
    let _ = value;
    Ok(Wrapper {
        id: "1".to_string(),
    })
}

fn main() {}
