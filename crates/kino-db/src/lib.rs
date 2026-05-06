//! SQLite access for Kino.
//!
//! Wraps `sqlx` and (in F-188) exposes the connection pool, migration runner,
//! and test-fixture helpers used across the workspace. The driver choice and
//! offline-cache workflow are recorded in `docs/adrs/0001-sqlite-access-via-sqlx.md`.

use thiserror::Error;

/// Errors produced by `kino_db`.
#[derive(Debug, Error)]
pub enum Error {
    /// A query, pool, or migration operation failed.
    #[error("database error: {0}")]
    Sqlx(#[from] sqlx::Error),
}

/// Crate-local `Result` alias.
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use sqlx::{Connection, SqliteConnection};

    #[tokio::test]
    async fn smoke_select_one() {
        let mut conn = SqliteConnection::connect("sqlite::memory:").await.unwrap();
        let row = sqlx::query!("SELECT 1 AS one")
            .fetch_one(&mut conn)
            .await
            .unwrap();
        assert_eq!(row.one, 1);
    }
}
