fn main() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let result = runtime.block_on(agent_pool_gateway::startup::run());
    runtime.shutdown_timeout(std::time::Duration::from_secs(1));
    result
}
