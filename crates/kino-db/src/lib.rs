//! SQLite access for Kino.
//!
//! Wraps `sqlx` and exposes the connection pool used across the workspace. The
//! driver choice and offline-cache workflow are recorded in
//! `docs/adrs/0001-sqlite-access-via-sqlx.md`.
//!
//! `Db::open` applies the SQLite settings Kino relies on:
//!
//! - `journal_mode = WAL` on the writer connection so readers can continue
//!   while a writer commits.
//! - `synchronous = NORMAL`, SQLite's recommended durability/throughput tradeoff
//!   for WAL databases.
//! - `foreign_keys = ON` so schema constraints are enforced on every
//!   connection.
//! - `busy_timeout = 5s` so transient writer contention waits briefly instead
//!   of failing immediately.
//! - one writer connection and eight read-only reader connections, matching
//!   SQLite's single-writer/many-reader concurrency model.

use std::time::Duration;

use kino_core::Config;
use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions, SqliteSynchronous,
};
use thiserror::Error;

const BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const READER_CONNECTIONS: u32 = 8;
const WRITER_CONNECTIONS: u32 = 1;

/// Errors produced by `kino_db`.
#[derive(Debug, Error)]
pub enum Error {
    /// A query, pool, or migration operation failed.
    #[error("database error: {0}")]
    Sqlx(#[from] sqlx::Error),
}

/// Crate-local `Result` alias.
pub type Result<T> = std::result::Result<T, Error>;

/// SQLite connection pools for Kino.
#[derive(Clone)]
pub struct Db {
    readers: SqlitePool,
    writer: SqlitePool,
}

impl Db {
    /// Open Kino's SQLite database using the configured database path.
    ///
    /// The writer pool is opened first to create the database file if needed
    /// and enable WAL mode before read-only connections are established.
    pub async fn open(config: &Config) -> Result<Self> {
        let writer_options = connect_options(config)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal);

        let writer = SqlitePoolOptions::new()
            .max_connections(WRITER_CONNECTIONS)
            .min_connections(WRITER_CONNECTIONS)
            .connect_with(writer_options)
            .await?;

        let reader_options = connect_options(config).read_only(true);
        let readers = SqlitePoolOptions::new()
            .max_connections(READER_CONNECTIONS)
            .min_connections(WRITER_CONNECTIONS)
            .connect_with(reader_options)
            .await?;

        Ok(Self { readers, writer })
    }

    /// Access the read-only pool for queries that do not mutate state.
    pub fn read_pool(&self) -> &SqlitePool {
        &self.readers
    }

    /// Access the single-connection writer pool for schema and state changes.
    pub fn write_pool(&self) -> &SqlitePool {
        &self.writer
    }

    /// Close both pools and wait for their worker connections to stop.
    pub async fn close(self) {
        self.readers.close().await;
        self.writer.close().await;
    }
}

fn connect_options(config: &Config) -> SqliteConnectOptions {
    SqliteConnectOptions::new()
        .filename(&config.database_path)
        .foreign_keys(true)
        .synchronous(SqliteSynchronous::Normal)
        .busy_timeout(BUSY_TIMEOUT)
        .optimize_on_close(true, None)
}

#[cfg(test)]
#[allow(clippy::result_large_err)]
mod tests {
    use std::path::PathBuf;

    use kino_core::Config;

    #[tokio::test]
    async fn open_applies_sqlite_pragmas() -> std::result::Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let config = config(dir.path().join("kino.db"));

        let db = super::Db::open(&config).await?;

        let journal_mode: String = sqlx::query_scalar("PRAGMA journal_mode")
            .fetch_one(db.write_pool())
            .await?;
        let synchronous: i64 = sqlx::query_scalar("PRAGMA synchronous")
            .fetch_one(db.write_pool())
            .await?;
        let foreign_keys: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
            .fetch_one(db.read_pool())
            .await?;
        let busy_timeout: i64 = sqlx::query_scalar("PRAGMA busy_timeout")
            .fetch_one(db.read_pool())
            .await?;

        assert_eq!(journal_mode, "wal");
        assert_eq!(synchronous, 1);
        assert_eq!(foreign_keys, 1);
        assert_eq!(busy_timeout, 5_000);

        db.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn reader_pool_is_read_only() -> std::result::Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let config = config(dir.path().join("kino.db"));
        let db = super::Db::open(&config).await?;

        sqlx::query("CREATE TABLE media (id INTEGER PRIMARY KEY)")
            .execute(db.write_pool())
            .await?;

        let write_result = sqlx::query("INSERT INTO media DEFAULT VALUES")
            .execute(db.read_pool())
            .await;

        assert!(write_result.is_err());

        db.close().await;
        Ok(())
    }

    fn config(database_path: PathBuf) -> Config {
        Config {
            database_path,
            library_root: PathBuf::from("/srv/media"),
            server: Default::default(),
            log_level: "info".into(),
        }
    }
}
