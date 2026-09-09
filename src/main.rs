mod auth;
mod config;
mod db;
mod error;
mod logging;
mod metrics;
mod middleware;
mod routes;
mod services;
mod startup;
mod state;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    startup::run().await
}
