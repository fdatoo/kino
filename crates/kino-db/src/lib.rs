//! SQLite access for Kino.
//!
//! Wraps `sqlx` and exposes the connection pool and forward-only migration
//! runner used across the workspace. The driver choice and offline-cache
//! workflow are recorded in `docs/adrs/0001-sqlite-access-via-sqlx.md`.
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

use std::path::Path;
#[cfg(any(test, feature = "test-helpers"))]
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use kino_core::Config;
use sqlx::migrate::{Migration, MigrationType, Migrator};
use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions, SqliteSynchronous,
};
use thiserror::Error;

const BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const READER_CONNECTIONS: u32 = 8;
const WRITER_CONNECTIONS: u32 = 1;

static MIGRATOR: Migrator = sqlx::migrate!("./migrations");
#[cfg(any(test, feature = "test-helpers"))]
static TEST_DB_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Errors produced by `kino_db`.
#[derive(Debug, Error)]
pub enum Error {
    /// A query, pool, or migration operation failed.
    #[error("database error: {0}")]
    Sqlx(#[from] sqlx::Error),

    /// An embedded migration has a duplicate version number.
    #[error("database migration {version} is duplicated")]
    DuplicateMigrationVersion {
        /// Duplicate migration version.
        version: i64,
    },

    /// An embedded migration version is not valid.
    #[error("database migration {version} is invalid; versions must be greater than zero")]
    InvalidMigrationVersion {
        /// Invalid migration version.
        version: i64,
    },

    /// A down migration was embedded even though Kino only supports forward migrations.
    #[error(
        "database migration {version} is a down migration; only forward migrations are supported"
    )]
    DownMigration {
        /// Down migration version.
        version: i64,
    },

    /// A migration used reversible sqlx naming instead of Kino's simple forward-only naming.
    #[error(
        "database migration {version} uses reversible migration naming; use 0001_description.sql"
    )]
    ReversibleMigration {
        /// Reversible migration version.
        version: i64,
    },

    /// The database has an applied migration that is not embedded in this binary.
    #[error("database migration {version} was previously applied but is missing from this binary")]
    MissingMigration {
        /// Missing migration version.
        version: i64,
    },

    /// An applied migration's checksum no longer matches the embedded SQL.
    #[error("database migration {version} was previously applied but has been modified")]
    ModifiedMigration {
        /// Modified migration version.
        version: i64,
    },

    /// A pending migration is older than a migration that has already been applied.
    #[error(
        "database migration {version} is older than the latest applied migration {latest_version}"
    )]
    OutOfOrderMigration {
        /// Pending migration version.
        version: i64,
        /// Latest applied migration version.
        latest_version: i64,
    },

    /// Executing or recording a migration failed.
    #[error("database migration {version} failed: {source}")]
    MigrationFailed {
        /// Failed migration version.
        version: i64,
        /// Underlying SQL error.
        #[source]
        source: sqlx::Error,
    },
}

/// Crate-local `Result` alias.
pub type Result<T> = std::result::Result<T, Error>;

/// Open a fresh, fully migrated in-memory SQLite database for tests.
///
/// Each call uses a unique shared-cache database name, so tests can run in
/// parallel without sharing state.
#[cfg(any(test, feature = "test-helpers"))]
pub async fn test_db() -> Result<Db> {
    let sequence = TEST_DB_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let filename =
        std::path::PathBuf::from(format!("file:kino-test-{}-{sequence}", std::process::id()));
    let options = connect_options_for_filename(filename)
        .in_memory(true)
        .shared_cache(true);

    Db::open_with_options(options, None).await
}

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
        Db::open_with_options(connect_options(config), Some(SqliteJournalMode::Wal)).await
    }

    async fn open_with_options(
        options: SqliteConnectOptions,
        journal_mode: Option<SqliteJournalMode>,
    ) -> Result<Self> {
        let mut writer_options = options.clone().create_if_missing(true);
        if let Some(journal_mode) = journal_mode {
            writer_options = writer_options.journal_mode(journal_mode);
        }

        let writer = SqlitePoolOptions::new()
            .max_connections(WRITER_CONNECTIONS)
            .min_connections(WRITER_CONNECTIONS)
            .connect_with(writer_options)
            .await?;

        run_migrations(&writer, &MIGRATOR).await?;

        let reader_options = options.read_only(true);
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

    /// Run any pending embedded migrations on the writer pool.
    pub async fn migrate(&self) -> Result<()> {
        run_migrations(&self.writer, &MIGRATOR).await
    }

    /// Close both pools and wait for their worker connections to stop.
    pub async fn close(self) {
        self.readers.close().await;
        self.writer.close().await;
    }
}

fn connect_options(config: &Config) -> SqliteConnectOptions {
    connect_options_for_filename(&config.database_path)
}

fn connect_options_for_filename(filename: impl AsRef<Path>) -> SqliteConnectOptions {
    SqliteConnectOptions::new()
        .filename(filename)
        .foreign_keys(true)
        .synchronous(SqliteSynchronous::Normal)
        .busy_timeout(BUSY_TIMEOUT)
        .optimize_on_close(true, None)
}

#[derive(Debug)]
struct AppliedMigration {
    version: i64,
    checksum: Vec<u8>,
}

async fn run_migrations(pool: &SqlitePool, migrator: &Migrator) -> Result<()> {
    validate_migrations(migrator)?;
    ensure_schema_migrations(pool).await?;

    let applied = applied_migrations(pool).await?;
    validate_applied_migrations(&applied, migrator)?;

    let latest_version = applied.iter().map(|migration| migration.version).max();
    for migration in migrator.iter() {
        if applied
            .iter()
            .any(|applied| applied.version == migration.version)
        {
            continue;
        }

        if let Some(latest_version) = latest_version
            && migration.version < latest_version
        {
            return Err(Error::OutOfOrderMigration {
                version: migration.version,
                latest_version,
            });
        }

        apply_migration(pool, migration).await?;
    }

    Ok(())
}

fn validate_migrations(migrator: &Migrator) -> Result<()> {
    let mut versions = std::collections::HashSet::new();
    for migration in migrator.iter() {
        if migration.version <= 0 {
            return Err(Error::InvalidMigrationVersion {
                version: migration.version,
            });
        }

        match migration.migration_type {
            MigrationType::Simple => {}
            MigrationType::ReversibleDown => {
                return Err(Error::DownMigration {
                    version: migration.version,
                });
            }
            MigrationType::ReversibleUp => {
                return Err(Error::ReversibleMigration {
                    version: migration.version,
                });
            }
        }

        if !versions.insert(migration.version) {
            return Err(Error::DuplicateMigrationVersion {
                version: migration.version,
            });
        }
    }

    Ok(())
}

async fn ensure_schema_migrations(pool: &SqlitePool) -> Result<()> {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS schema_migrations (
            version INTEGER PRIMARY KEY NOT NULL,
            description TEXT NOT NULL,
            checksum BLOB NOT NULL,
            applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            execution_time_ns INTEGER NOT NULL
        )
        "#,
    )
    .execute(pool)
    .await?;

    Ok(())
}

async fn applied_migrations(pool: &SqlitePool) -> Result<Vec<AppliedMigration>> {
    let rows = sqlx::query_as::<_, (i64, Vec<u8>)>(
        "SELECT version, checksum FROM schema_migrations ORDER BY version",
    )
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|(version, checksum)| AppliedMigration { version, checksum })
        .collect())
}

fn validate_applied_migrations(applied: &[AppliedMigration], migrator: &Migrator) -> Result<()> {
    for applied_migration in applied {
        let Some(embedded) = migrator
            .iter()
            .find(|migration| migration.version == applied_migration.version)
        else {
            return Err(Error::MissingMigration {
                version: applied_migration.version,
            });
        };

        if applied_migration.checksum != embedded.checksum.as_ref() {
            return Err(Error::ModifiedMigration {
                version: applied_migration.version,
            });
        }
    }

    Ok(())
}

async fn apply_migration(pool: &SqlitePool, migration: &Migration) -> Result<()> {
    let start = std::time::Instant::now();
    let mut tx = pool.begin().await?;

    sqlx::query(migration.sql.as_ref())
        .execute(&mut *tx)
        .await
        .map_err(|source| Error::MigrationFailed {
            version: migration.version,
            source,
        })?;

    sqlx::query(
        r#"
        INSERT INTO schema_migrations (version, description, checksum, execution_time_ns)
        VALUES (?1, ?2, ?3, ?4)
        "#,
    )
    .bind(migration.version)
    .bind(migration.description.as_ref())
    .bind(migration.checksum.as_ref())
    .bind(elapsed_nanos(start.elapsed()))
    .execute(&mut *tx)
    .await
    .map_err(|source| Error::MigrationFailed {
        version: migration.version,
        source,
    })?;

    tx.commit().await.map_err(|source| Error::MigrationFailed {
        version: migration.version,
        source,
    })?;

    Ok(())
}

fn elapsed_nanos(duration: Duration) -> i64 {
    i64::try_from(duration.as_nanos()).unwrap_or(i64::MAX)
}

#[cfg(test)]
#[allow(clippy::result_large_err)]
mod tests {
    use std::borrow::Cow;
    use std::path::PathBuf;

    use kino_core::Config;
    use sqlx::migrate::{Migration, MigrationType, Migrator};

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

    #[tokio::test]
    async fn open_applies_embedded_migrations()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let config = config(dir.path().join("kino.db"));

        let db = super::Db::open(&config).await?;

        let applied: Vec<(i64, String)> =
            sqlx::query_as("SELECT version, description FROM schema_migrations ORDER BY version")
                .fetch_all(db.write_pool())
                .await?;

        assert_eq!(
            applied,
            vec![
                (1, String::from("initial")),
                (2, String::from("request status events")),
                (3, String::from("request list index")),
                (4, String::from("request model fields")),
                (5, String::from("request match candidates")),
                (6, String::from("request identity versions")),
            ]
        );

        db.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn migration_runner_rejects_modified_applied_migration()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let config = config(dir.path().join("kino.db"));
        let db = super::Db::open(&config).await?;
        let migrator = test_migrator(
            1,
            "initial",
            "CREATE TABLE changed_migration_test (id INTEGER PRIMARY KEY)",
        );

        let err = match super::run_migrations(db.write_pool(), &migrator).await {
            Ok(()) => panic!("modified migration was accepted"),
            Err(err) => err,
        };

        assert!(matches!(
            err,
            super::Error::ModifiedMigration { version: 1 }
        ));

        db.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn migration_runner_records_test_migrations()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let config = config(dir.path().join("kino.db"));
        let db = super::Db::open(&config).await?;
        let migrator = test_migrator_with_embedded(
            7,
            "test migration",
            "CREATE TABLE migration_runner_test (id INTEGER PRIMARY KEY)",
        );

        super::run_migrations(db.write_pool(), &migrator).await?;

        let table_name: String = sqlx::query_scalar(
            "SELECT name FROM sqlite_master WHERE name = 'migration_runner_test'",
        )
        .fetch_one(db.write_pool())
        .await?;
        let recorded: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM schema_migrations WHERE version = 7")
                .fetch_one(db.write_pool())
                .await?;

        assert_eq!(table_name, "migration_runner_test");
        assert_eq!(recorded, 1);

        db.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn migration_runner_fails_fast_on_sql_error()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let config = config(dir.path().join("kino.db"));
        let db = super::Db::open(&config).await?;
        let migrator = test_migrator_with_embedded(7, "broken", "CREATE TABLE");

        let err = match super::run_migrations(db.write_pool(), &migrator).await {
            Ok(()) => panic!("broken migration was accepted"),
            Err(err) => err,
        };
        let recorded: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM schema_migrations WHERE version = 7")
                .fetch_one(db.write_pool())
                .await?;

        assert!(matches!(
            err,
            super::Error::MigrationFailed { version: 7, .. }
        ));
        assert!(err.to_string().contains("database migration 7 failed"));
        assert_eq!(recorded, 0);

        db.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn test_db_returns_fresh_migrated_in_memory_db()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let first = super::test_db().await?;
        let second = super::test_db().await?;

        let applied: Vec<(i64, String)> =
            sqlx::query_as("SELECT version, description FROM schema_migrations ORDER BY version")
                .fetch_all(first.write_pool())
                .await?;
        assert_eq!(
            applied,
            vec![
                (1, String::from("initial")),
                (2, String::from("request status events")),
                (3, String::from("request list index")),
                (4, String::from("request model fields")),
                (5, String::from("request match candidates")),
                (6, String::from("request identity versions")),
            ]
        );

        sqlx::query("CREATE TABLE fixture_test (id INTEGER PRIMARY KEY)")
            .execute(first.write_pool())
            .await?;

        let visible_from_first_reader: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'fixture_test'",
        )
        .fetch_one(first.read_pool())
        .await?;
        let visible_from_second_db: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'fixture_test'",
        )
        .fetch_one(second.write_pool())
        .await?;

        assert_eq!(visible_from_first_reader, 1);
        assert_eq!(visible_from_second_db, 0);

        first.close().await;
        second.close().await;
        Ok(())
    }

    fn config(database_path: PathBuf) -> Config {
        Config {
            database_path,
            library_root: PathBuf::from("/srv/media"),
            server: Default::default(),
            log_level: "info".into(),
            log_format: Default::default(),
        }
    }

    fn test_migrator(version: i64, description: &str, sql: &str) -> Migrator {
        Migrator {
            migrations: Cow::Owned(vec![Migration::new(
                version,
                Cow::Owned(description.to_owned()),
                MigrationType::Simple,
                Cow::Owned(sql.to_owned()),
                false,
            )]),
            ..Migrator::DEFAULT
        }
    }

    fn test_migrator_with_embedded(version: i64, description: &str, sql: &str) -> Migrator {
        let mut migrations = super::MIGRATOR.iter().cloned().collect::<Vec<_>>();
        migrations.push(Migration::new(
            version,
            Cow::Owned(description.to_owned()),
            MigrationType::Simple,
            Cow::Owned(sql.to_owned()),
            false,
        ));

        Migrator {
            migrations: Cow::Owned(migrations),
            ..Migrator::DEFAULT
        }
    }
}
