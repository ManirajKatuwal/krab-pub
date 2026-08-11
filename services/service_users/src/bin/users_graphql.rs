#[tokio::main]
async fn main() -> anyhow::Result<()> {
    service_users::run_split_target(service_users::SplitUsersTarget::Graphql).await
}
