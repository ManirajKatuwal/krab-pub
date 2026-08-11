// Should fail: #[server] on non-async function.
use krab_macros::server;
use krab_core::server_fn::ServerFnError;

#[server]
pub fn sync_function(id: String) -> Result<String, ServerFnError> {
    Ok(id)
}

fn main() {}
