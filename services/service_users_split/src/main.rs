use anyhow::Result;
use service_users_split::{build_app, domain::service::InMemoryDomainService};
use std::net::SocketAddr;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let app = build_app(InMemoryDomainService::shared());

    let addr = SocketAddr::from(([127, 0, 0, 1], 3207));
    println!("service_users_split listening on {}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
