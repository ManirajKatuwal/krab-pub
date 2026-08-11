#[tokio::main]
async fn main() -> anyhow::Result<()> {
    service_users::run_default().await
}
