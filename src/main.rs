mod auth;
mod config;
mod error;
mod routes;
mod startup;
mod state;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    startup::run().await
}
