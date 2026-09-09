use sqlx::{
    Pool, Postgres,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use std::{str::FromStr, time::Duration};
use thiserror::Error;

/// How long a readiness probe waits before calling the database unreachable.
///
/// A load balancer polls this endpoint, so it must answer faster than the
/// poll interval. A probe that hangs is read as "up" by some balancers and
/// "down" by others, which is worse than a quick, definite "not ready".
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Error)]
pub enum DatabaseError {
    #[error("invalid DATABASE_URL")]
    Url(#[source] sqlx::Error),
    #[error("could not apply migrations")]
    Migrate(#[source] sqlx::migrate::MigrateError),
}

/// Builds the pool without touching the network.
///
/// Connecting lazily keeps process start independent of database start, so
/// `docker compose up` does not depend on container ordering. The first real
/// connection happens in [`migrate`], which is where a bad URL or an
/// unreachable server is reported.
pub fn pool(url: &str, max_connections: u32) -> Result<Pool<Postgres>, DatabaseError> {
    let options = PgConnectOptions::from_str(url).map_err(DatabaseError::Url)?;
    Ok(PgPoolOptions::new()
        .max_connections(max_connections)
        .connect_lazy_with(options))
}

/// Applies every pending migration, and refuses to continue if any fails.
///
/// This runs in the startup path on purpose: a deployment must not need a
/// second manual step, and a gateway serving requests against a half-migrated
/// schema is worse than a gateway that did not start.
pub async fn migrate(pool: &Pool<Postgres>) -> Result<(), DatabaseError> {
    sqlx::migrate!("./migrations")
        .run(pool)
        .await
        .map_err(DatabaseError::Migrate)
}

/// Answers whether the database can serve a query right now.
///
/// Used by readiness only. It never returns an error type: readiness is a
/// yes/no answer, and the reason belongs in the log line the caller writes.
pub async fn is_reachable(pool: &Pool<Postgres>) -> Result<(), sqlx::Error> {
    match tokio::time::timeout(PROBE_TIMEOUT, sqlx::query("SELECT 1").execute(pool)).await {
        Ok(result) => result.map(|_| ()),
        Err(_elapsed) => Err(sqlx::Error::PoolTimedOut),
    }
}
