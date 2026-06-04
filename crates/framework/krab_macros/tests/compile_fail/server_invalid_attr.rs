use krab_core::server_fn::ServerFnError;
use krab_macros::server;

#[server(endpoint = "/api/invalid_attr")]
pub async fn invalid_attr(id: String) -> Result<String, ServerFnError> {
    Ok(id)
}

fn main() {}
