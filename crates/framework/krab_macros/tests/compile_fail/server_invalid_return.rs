use krab_macros::server;

#[server]
pub async fn invalid_return(id: String) -> String {
    id
}

fn main() {}
