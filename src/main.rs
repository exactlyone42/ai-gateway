mod auth;
mod config;
mod error;
mod logging;
mod middleware;
mod routes;
mod services;
mod startup;
mod state;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    startup::run().await
}
