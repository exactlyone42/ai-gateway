use std::{io, str::FromStr, time::Duration};

use sqlx::{
    Connection, PgConnection,
    postgres::{PgConnectOptions, PgSslMode},
};
use tokio::runtime::Handle;

use crate::capture_erasure::ErasurePosition;

const TIMEOUT: Duration = Duration::from_secs(10);

pub(crate) struct RemoteWitness {
    options: PgConnectOptions,
    runtime: Handle,
}

fn unavailable() -> io::Error {
    io::Error::other("remote erasure witness unavailable")
}

impl RemoteWitness {
    pub(crate) fn new(url: &str, database_url: &str) -> io::Result<Self> {
        url.strip_prefix("postgres://")
            .or_else(|| url.strip_prefix("postgresql://"))
            .and_then(|rest| rest.split(['/', '?', '#']).next())
            .and_then(|value| value.rsplit('@').next())
            .filter(|value| !value.is_empty() && !value.starts_with(':'))
            .ok_or_else(unavailable)?;
        let options = PgConnectOptions::from_str(url).map_err(|_| unavailable())?;
        let database = PgConnectOptions::from_str(database_url).map_err(|_| unavailable())?;
        if !matches!(options.get_ssl_mode(), PgSslMode::VerifyFull)
            || options.get_socket().is_some()
            || options.get_host().starts_with('/')
            || options.get_host().eq_ignore_ascii_case(database.get_host())
        {
            return Err(unavailable());
        }
        let runtime = Handle::try_current().map_err(|_| unavailable())?;
        Ok(Self { options, runtime })
    }

    pub(crate) fn read(&self) -> io::Result<ErasurePosition> {
        self.runtime.block_on(async {
            tokio::time::timeout(TIMEOUT, async {
                let mut connection = PgConnection::connect_with(&self.options).await?;
                if !sqlx::query_file_scalar!("queries/erasure_witness_caller_safe.sql")
                    .fetch_one(&mut connection)
                    .await?
                {
                    return Err(sqlx::Error::Protocol("unsafe witness privileges".into()));
                }
                let row = sqlx::query_file!("queries/erasure_witness_get.sql")
                    .fetch_optional(&mut connection)
                    .await?
                    .ok_or(sqlx::Error::RowNotFound)?;
                ErasurePosition::from_database(row.install, row.sequence, row.tip)
                    .map_err(sqlx::Error::Io)
            })
            .await
            .map_err(|_| unavailable())?
            .map_err(|_| unavailable())
        })
    }

    pub(crate) fn advance(
        &self,
        current: &ErasurePosition,
        next: &ErasurePosition,
    ) -> io::Result<bool> {
        if current.install != next.install || current.sequence.checked_add(1) != Some(next.sequence)
        {
            return Err(unavailable());
        }
        let sequence = i64::try_from(current.sequence).map_err(|_| unavailable())?;
        self.runtime.block_on(async {
            tokio::time::timeout(TIMEOUT, async {
                let mut connection = PgConnection::connect_with(&self.options).await?;
                let mut transaction = connection.begin().await?;
                sqlx::query_file!("queries/erasure_witness_sync.sql")
                    .execute(&mut *transaction)
                    .await?;
                if !sqlx::query_file_scalar!("queries/erasure_witness_caller_safe.sql")
                    .fetch_one(&mut *transaction)
                    .await?
                {
                    return Err(sqlx::Error::Protocol("unsafe witness privileges".into()));
                }
                let applied = sqlx::query_file_scalar!(
                    "queries/erasure_witness_advance.sql",
                    current.install,
                    sequence,
                    &current.tip[..],
                    &next.tip[..]
                )
                .fetch_one(&mut *transaction)
                .await?;
                transaction.commit().await?;
                Ok::<bool, sqlx::Error>(applied)
            })
            .await
            .map_err(|_| unavailable())?
            .map_err(|_| unavailable())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::RemoteWitness;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "需要隔離備援見證資料庫"]
    async fn rejects_standby_witness() {
        let url = std::env::var("TEST_ERASURE_WITNESS_STANDBY_URL").unwrap();
        let options: sqlx::postgres::PgConnectOptions = url.parse().unwrap();
        assert_eq!(options.get_host(), "127.0.0.1");
        assert!(
            options
                .get_database()
                .is_some_and(|name| name.starts_with("gateway_witness_test_"))
        );
        assert!(matches!(
            options.get_ssl_mode(),
            sqlx::postgres::PgSslMode::VerifyFull
        ));
        let remote =
            RemoteWitness::new(&url, "postgres://gateway@primary.invalid/gateway").unwrap();
        let result = tokio::task::spawn_blocking(move || remote.read())
            .await
            .unwrap();
        assert!(result.is_err());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "需要隔離見證資料庫"]
    async fn rejects_later_privilege_changes() {
        let url = std::env::var("TEST_ERASURE_WITNESS_URL").unwrap();
        let options: sqlx::postgres::PgConnectOptions = url.parse().unwrap();
        assert_eq!(options.get_host(), "127.0.0.1");
        assert!(
            options
                .get_database()
                .is_some_and(|name| name.starts_with("gateway_witness_test_"))
        );
        assert!(matches!(
            options.get_ssl_mode(),
            sqlx::postgres::PgSslMode::VerifyFull
        ));
        let safe = std::env::var("TEST_WITNESS_EXPECT_SAFE").unwrap() == "1";
        let remote =
            RemoteWitness::new(&url, "postgres://gateway@primary.invalid/gateway").unwrap();
        tokio::task::spawn_blocking(move || {
            let result = remote.read();
            assert_eq!(result.is_ok(), safe);
            if !safe {
                let current = crate::capture_erasure::ErasurePosition {
                    install: uuid::Uuid::parse_str("00000000-0000-4000-8000-000000000001").unwrap(),
                    sequence: 0,
                    tip: [1; 32],
                };
                let next = crate::capture_erasure::ErasurePosition {
                    install: current.install,
                    sequence: 1,
                    tip: [2; 32],
                };
                assert!(remote.advance(&current, &next).is_err());
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn requires_verified_tls_and_a_different_database_host() {
        let database = "postgres://gateway@primary.example/gateway";
        assert!(
            RemoteWitness::new(
                "postgres://witness@witness.example/witness?sslmode=disable",
                database
            )
            .is_err()
        );
        assert!(
            RemoteWitness::new(
                "postgres://witness@witness.example/witness?sslmode=require",
                database
            )
            .is_err()
        );
        assert!(RemoteWitness::new("postgres:///witness?sslmode=verify-full", database).is_err());
        assert!(
            RemoteWitness::new(
                "postgres://witness@witness.example/witness?sslmode=verify-full&host=/tmp",
                database
            )
            .is_err()
        );
        assert!(
            RemoteWitness::new(
                "postgres://witness@primary.example/witness?sslmode=verify-full",
                database
            )
            .is_err()
        );
        assert!(
            RemoteWitness::new(
                "postgres://witness@witness.example/witness?sslmode=verify-full",
                database
            )
            .is_ok()
        );
    }
}
