use krab_core::server_fn::ServerFnError;
use krab_macros::server;

struct UserApi;

impl UserApi {
    #[server]
    pub async fn get_user(&self, id: String) -> Result<String, ServerFnError> {
        Ok(id)
    }
}

fn main() {}
