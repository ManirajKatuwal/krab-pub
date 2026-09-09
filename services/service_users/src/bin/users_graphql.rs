// The runtime is built by hand rather than with `#[tokio::main]` so the split
// target's identity can be published to the environment while the process is
// still single-threaded. `#[tokio::main]` would put that call inside
// `block_on`, where the worker threads already exist and `std::env::set_var`
// races anything else reading the environment.
fn main() -> anyhow::Result<()> {
    let target = service_users::SplitUsersTarget::Graphql;
    service_users::configure_split_target_env(target);

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(service_users::run_split_target(target))
}
