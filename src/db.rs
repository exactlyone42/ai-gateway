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
const STATEMENT_TIMEOUT: &str = "2s";
const LOCK_TIMEOUT: &str = "1s";

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
    let options = gateway_options(PgConnectOptions::from_str(url).map_err(DatabaseError::Url)?);
    Ok(PgPoolOptions::new()
        .max_connections(max_connections)
        .acquire_timeout(PROBE_TIMEOUT)
        .connect_lazy_with(options))
}

pub(crate) fn gateway_options(options: PgConnectOptions) -> PgConnectOptions {
    // Identity resolution holds a consistency gate while querying. Server-side
    // bounds ensure a locked row cannot turn that gate into a gateway-wide hang.
    options.options([
        ("statement_timeout", STATEMENT_TIMEOUT),
        ("lock_timeout", LOCK_TIMEOUT),
    ])
}

/// Applies every pending migration, and refuses to continue if any fails.
///
/// This runs in the startup path on purpose: a deployment must not need a
/// second manual step, and a gateway serving requests against a half-migrated
/// schema is worse than a gateway that did not start.
pub async fn migrate(pool: &Pool<Postgres>) -> Result<(), DatabaseError> {
    let mut migrations = sqlx::migrate!("./migrations");
    // A rolled-back binary sees migrations applied by the newer release. The
    // schema changes are expand-only, so unknown newer versions are safe to
    // leave in place and must not prevent the old binary from starting.
    migrations.set_ignore_missing(true);
    let mut connection = pool
        .acquire()
        .await
        .map_err(|error| DatabaseError::Migrate(sqlx::migrate::MigrateError::Execute(error)))?;
    let original_timeouts = sqlx::query_as::<_, (String, String)>(
        "SELECT current_setting('statement_timeout'), current_setting('lock_timeout')",
    )
    .fetch_one(&mut *connection)
    .await
    .map_err(|error| DatabaseError::Migrate(sqlx::migrate::MigrateError::Execute(error)))?;
    sqlx::query(
        "SELECT set_config('statement_timeout', $1, false), set_config('lock_timeout', $2, false)",
    )
    .bind("30s")
    .bind("5s")
    .execute(&mut *connection)
    .await
    .map_err(|error| DatabaseError::Migrate(sqlx::migrate::MigrateError::Execute(error)))?;
    let migration_result = migrations.run(&mut *connection).await;
    let restore_result = sqlx::query(
        "SELECT set_config('statement_timeout', $1, false), set_config('lock_timeout', $2, false)",
    )
    .bind(&original_timeouts.0)
    .bind(&original_timeouts.1)
    .execute(&mut *connection)
    .await;
    migration_result.map_err(DatabaseError::Migrate)?;
    restore_result
        .map(|_| ())
        .map_err(|error| DatabaseError::Migrate(sqlx::migrate::MigrateError::Execute(error)))
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

#[cfg(test)]
mod tests {
    use sqlx::PgPool;

    #[sqlx::test]
    async fn startup_migration_path_accepts_the_timeout_configuration(pool: PgPool) {
        let before = sqlx::query_as::<_, (String, String)>(
            "SELECT current_setting('statement_timeout'), current_setting('lock_timeout')",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        super::migrate(&pool).await.unwrap();
        let after = sqlx::query_as::<_, (String, String)>(
            "SELECT current_setting('statement_timeout'), current_setting('lock_timeout')",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(after, before);
    }
}
