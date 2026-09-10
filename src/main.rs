#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    agent_pool_gateway::startup::run().await
}
