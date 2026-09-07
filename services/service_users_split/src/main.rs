#[tokio::main]
async fn main() -> anyhow::Result<()> {
    service_users_split::run_default().await
}
