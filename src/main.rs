mod config;
mod error;
mod gateway;
mod routes;
mod startup;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    startup::run().await
}
