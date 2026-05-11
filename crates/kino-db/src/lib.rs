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

    use kino_core::{
        CanonicalIdentityId, Config, DeviceToken, Id, PlaybackProgress, PlaybackSession,
        PlaybackSessionStatus, SEEDED_USER_ID, Timestamp, TmdbId, Watched, WatchedSource,
    };
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
                (7, String::from("canonical identities")),
                (8, String::from("request fulfillment plans")),
                (9, String::from("minimal media items")),
                (10, String::from("subtitle sidecars")),
                (11, String::from("metadata cache")),
                (12, String::from("source files")),
                (13, String::from("core catalog schemas")),
                (14, String::from("users")),
                (15, String::from("device tokens")),
                (16, String::from("playback state")),
                (17, String::from("playback sessions")),
                (18, String::from("subtitle provenance")),
                (19, String::from("catalog fts")),
                (20, String::from("metadata artwork")),
                (21, String::from("subtitle archive")),
                (22, String::from("watched transitions")),
                (23, String::from("source file probe data")),
                (24, String::from("subtitle forced flag")),
            ]
        );

        db.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn users_migration_seeds_owner_user_once()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = super::test_db().await?;

        let seeded: (Id, String, Timestamp) =
            sqlx::query_as("SELECT id, display_name, created_at FROM users")
                .fetch_one(db.read_pool())
                .await?;
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
            .fetch_one(db.read_pool())
            .await?;

        assert_eq!(seeded.0, SEEDED_USER_ID);
        assert_eq!(seeded.1, "Owner");
        assert_eq!(seeded.2.to_string(), "2026-05-11T00:00:00Z");
        assert_eq!(count, 1);

        db.migrate().await?;

        let count_after_rerun: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
            .fetch_one(db.read_pool())
            .await?;
        let seeded_after_rerun: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM users WHERE id = ?1 AND display_name = 'Owner'",
        )
        .bind(SEEDED_USER_ID)
        .fetch_one(db.read_pool())
        .await?;

        assert_eq!(count_after_rerun, 1);
        assert_eq!(seeded_after_rerun, 1);

        db.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn device_tokens_support_insert_hash_lookup_and_revocation()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = super::test_db().await?;
        let token = DeviceToken::new(
            Id::new(),
            SEEDED_USER_ID,
            "Living room Apple TV",
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            Timestamp::now(),
        );

        sqlx::query(
            r#"
            INSERT INTO device_tokens (
                id,
                user_id,
                label,
                hash,
                last_seen_at,
                revoked_at,
                created_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            "#,
        )
        .bind(token.id)
        .bind(token.user_id)
        .bind(&token.label)
        .bind(&token.hash)
        .bind(token.last_seen_at)
        .bind(token.revoked_at)
        .bind(token.created_at)
        .execute(db.write_pool())
        .await?;

        let stored: (
            Id,
            Id,
            String,
            String,
            Option<Timestamp>,
            Option<Timestamp>,
            Timestamp,
        ) = sqlx::query_as(
            r#"
                SELECT
                    id,
                    user_id,
                    label,
                    hash,
                    last_seen_at,
                    revoked_at,
                    created_at
                FROM device_tokens
                WHERE hash = ?1
                "#,
        )
        .bind(&token.hash)
        .fetch_one(db.read_pool())
        .await?;

        assert_eq!(stored.0, token.id);
        assert_eq!(stored.1, token.user_id);
        assert_eq!(stored.2, token.label);
        assert_eq!(stored.3, token.hash);
        assert_eq!(stored.4, None);
        assert_eq!(stored.5, None);
        assert_eq!(stored.6, token.created_at);

        let revoked_at = Timestamp::now();
        sqlx::query("UPDATE device_tokens SET revoked_at = ?1 WHERE hash = ?2")
            .bind(revoked_at)
            .bind(&token.hash)
            .execute(db.write_pool())
            .await?;

        let revoked_lookup: (String, Option<Timestamp>) =
            sqlx::query_as("SELECT hash, revoked_at FROM device_tokens WHERE hash = ?1")
                .bind(&token.hash)
                .fetch_one(db.read_pool())
                .await?;

        assert_eq!(revoked_lookup.0, token.hash);
        assert_eq!(revoked_lookup.1, Some(revoked_at));

        db.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn playback_sessions_support_heartbeat_and_status_transitions()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = super::test_db().await?;
        let identity = movie_identity(604);
        let media_item_id = Id::new();
        let token = DeviceToken::new(
            Id::new(),
            SEEDED_USER_ID,
            "Bedroom iPad",
            "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789",
            "2026-05-11T00:55:00Z".parse()?,
        );
        let started_at: Timestamp = "2026-05-11T01:00:00Z".parse()?;
        let heartbeat_at: Timestamp = "2026-05-11T01:05:00Z".parse()?;
        let idle_at: Timestamp = "2026-05-11T01:10:00Z".parse()?;
        let ended_at: Timestamp = "2026-05-11T01:12:00Z".parse()?;
        let session = PlaybackSession::active(
            Id::new(),
            SEEDED_USER_ID,
            token.id,
            media_item_id,
            Id::new().to_string(),
            started_at,
        );

        sqlx::query(
            r#"
            INSERT INTO canonical_identities (
                id,
                provider,
                media_kind,
                tmdb_id,
                source,
                created_at,
                updated_at
            )
            VALUES (?1, ?2, ?3, ?4, 'manual', ?5, ?6)
            "#,
        )
        .bind(identity)
        .bind(identity.provider().as_str())
        .bind(identity.kind().as_str())
        .bind(i64::from(identity.tmdb_id().get()))
        .bind(started_at)
        .bind(started_at)
        .execute(db.write_pool())
        .await?;

        sqlx::query(
            r#"
            INSERT INTO media_items (
                id,
                media_kind,
                canonical_identity_id,
                created_at,
                updated_at
            )
            VALUES (?1, 'movie', ?2, ?3, ?4)
            "#,
        )
        .bind(media_item_id)
        .bind(identity)
        .bind(started_at)
        .bind(started_at)
        .execute(db.write_pool())
        .await?;

        sqlx::query(
            r#"
            INSERT INTO device_tokens (
                id,
                user_id,
                label,
                hash,
                last_seen_at,
                revoked_at,
                created_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            "#,
        )
        .bind(token.id)
        .bind(token.user_id)
        .bind(&token.label)
        .bind(&token.hash)
        .bind(token.last_seen_at)
        .bind(token.revoked_at)
        .bind(token.created_at)
        .execute(db.write_pool())
        .await?;

        sqlx::query(
            r#"
            INSERT INTO playback_sessions (
                id,
                user_id,
                token_id,
                media_item_id,
                variant_id,
                started_at,
                last_seen_at,
                ended_at,
                status
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
            "#,
        )
        .bind(session.id)
        .bind(session.user_id)
        .bind(session.token_id)
        .bind(session.media_item_id)
        .bind(&session.variant_id)
        .bind(session.started_at)
        .bind(session.last_seen_at)
        .bind(session.ended_at)
        .bind(session.status.as_str())
        .execute(db.write_pool())
        .await?;

        let inserted: (String, Timestamp, Option<Timestamp>) = sqlx::query_as(
            "SELECT status, last_seen_at, ended_at FROM playback_sessions WHERE id = ?1",
        )
        .bind(session.id)
        .fetch_one(db.read_pool())
        .await?;
        assert_eq!(
            PlaybackSessionStatus::parse(&inserted.0),
            Some(PlaybackSessionStatus::Active)
        );
        assert_eq!(inserted.1, started_at);
        assert_eq!(inserted.2, None);

        sqlx::query("UPDATE playback_sessions SET last_seen_at = ?1 WHERE id = ?2")
            .bind(heartbeat_at)
            .bind(session.id)
            .execute(db.write_pool())
            .await?;

        let heartbeated: Timestamp =
            sqlx::query_scalar("SELECT last_seen_at FROM playback_sessions WHERE id = ?1")
                .bind(session.id)
                .fetch_one(db.read_pool())
                .await?;
        assert_eq!(heartbeated, heartbeat_at);

        sqlx::query("UPDATE playback_sessions SET status = ?1, last_seen_at = ?2 WHERE id = ?3")
            .bind(PlaybackSessionStatus::Idle.as_str())
            .bind(idle_at)
            .bind(session.id)
            .execute(db.write_pool())
            .await?;

        let idled: (String, Timestamp, Option<Timestamp>) = sqlx::query_as(
            "SELECT status, last_seen_at, ended_at FROM playback_sessions WHERE id = ?1",
        )
        .bind(session.id)
        .fetch_one(db.read_pool())
        .await?;
        assert_eq!(
            PlaybackSessionStatus::parse(&idled.0),
            Some(PlaybackSessionStatus::Idle)
        );
        assert_eq!(idled.1, idle_at);
        assert_eq!(idled.2, None);

        sqlx::query("UPDATE playback_sessions SET status = ?1, ended_at = ?2 WHERE id = ?3")
            .bind(PlaybackSessionStatus::Ended.as_str())
            .bind(ended_at)
            .bind(session.id)
            .execute(db.write_pool())
            .await?;

        let ended: (String, Option<Timestamp>) =
            sqlx::query_as("SELECT status, ended_at FROM playback_sessions WHERE id = ?1")
                .bind(session.id)
                .fetch_one(db.read_pool())
                .await?;
        assert_eq!(
            PlaybackSessionStatus::parse(&ended.0),
            Some(PlaybackSessionStatus::Ended)
        );
        assert_eq!(ended.1, Some(ended_at));

        db.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn playback_progress_upsert_preserves_higher_position()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = super::test_db().await?;
        let media_item_id = insert_personal_media_item(&db).await?;
        let initial_updated_at = Timestamp::now();
        let progress =
            PlaybackProgress::new(SEEDED_USER_ID, media_item_id, 100, initial_updated_at, None)?;

        insert_playback_progress(&db, &progress).await?;

        let lower_position =
            PlaybackProgress::new(SEEDED_USER_ID, media_item_id, 50, Timestamp::now(), None)?;
        upsert_playback_progress(&db, &lower_position).await?;

        let stored: (i64, Timestamp, Option<Id>) = sqlx::query_as(
            r#"
            SELECT position_seconds, updated_at, source_device_token_id
            FROM playback_progress
            WHERE user_id = ?1 AND media_item_id = ?2
            "#,
        )
        .bind(SEEDED_USER_ID)
        .bind(media_item_id)
        .fetch_one(db.read_pool())
        .await?;

        assert_eq!(stored.0, 100);
        assert_eq!(stored.1, initial_updated_at);
        assert_eq!(stored.2, None);

        db.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn playback_progress_upsert_accepts_higher_position()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = super::test_db().await?;
        let media_item_id = insert_personal_media_item(&db).await?;
        let progress =
            PlaybackProgress::new(SEEDED_USER_ID, media_item_id, 100, Timestamp::now(), None)?;

        insert_playback_progress(&db, &progress).await?;

        let updated_at = Timestamp::now();
        let higher_position =
            PlaybackProgress::new(SEEDED_USER_ID, media_item_id, 200, updated_at, None)?;
        upsert_playback_progress(&db, &higher_position).await?;

        let stored: (i64, Timestamp) = sqlx::query_as(
            r#"
            SELECT position_seconds, updated_at
            FROM playback_progress
            WHERE user_id = ?1 AND media_item_id = ?2
            "#,
        )
        .bind(SEEDED_USER_ID)
        .bind(media_item_id)
        .fetch_one(db.read_pool())
        .await?;

        assert_eq!(stored.0, 200);
        assert_eq!(stored.1, updated_at);

        db.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn watched_rows_support_insert_manual_toggle_and_delete()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = super::test_db().await?;
        let media_item_id = insert_personal_media_item(&db).await?;
        let watched = Watched::new(
            SEEDED_USER_ID,
            media_item_id,
            Timestamp::now(),
            WatchedSource::Auto,
        );

        sqlx::query(
            r#"
            INSERT INTO watched (
                user_id,
                media_item_id,
                watched_at,
                source,
                unmarked
            )
            VALUES (?1, ?2, ?3, ?4, ?5)
            "#,
        )
        .bind(watched.user_id)
        .bind(watched.media_item_id)
        .bind(watched.watched_at)
        .bind(watched.source.as_str())
        .bind(watched.unmarked)
        .execute(db.write_pool())
        .await?;

        let inserted: (String, bool) = sqlx::query_as(
            "SELECT source, unmarked FROM watched WHERE user_id = ?1 AND media_item_id = ?2",
        )
        .bind(SEEDED_USER_ID)
        .bind(media_item_id)
        .fetch_one(db.read_pool())
        .await?;
        assert_eq!(WatchedSource::parse(&inserted.0), Some(WatchedSource::Auto));
        assert!(!inserted.1);

        let watched_at = Timestamp::now();
        sqlx::query(
            r#"
            UPDATE watched
            SET watched_at = ?1, source = ?2, unmarked = ?3
            WHERE user_id = ?4 AND media_item_id = ?5
            "#,
        )
        .bind(watched_at)
        .bind(WatchedSource::Manual.as_str())
        .bind(false)
        .bind(SEEDED_USER_ID)
        .bind(media_item_id)
        .execute(db.write_pool())
        .await?;

        let updated: (Timestamp, String, bool) = sqlx::query_as(
            "SELECT watched_at, source, unmarked FROM watched WHERE user_id = ?1 AND media_item_id = ?2",
        )
        .bind(SEEDED_USER_ID)
        .bind(media_item_id)
        .fetch_one(db.read_pool())
        .await?;
        assert_eq!(updated.0, watched_at);
        assert_eq!(
            WatchedSource::parse(&updated.1),
            Some(WatchedSource::Manual)
        );
        assert!(!updated.2);

        sqlx::query("DELETE FROM watched WHERE user_id = ?1 AND media_item_id = ?2")
            .bind(SEEDED_USER_ID)
            .bind(media_item_id)
            .execute(db.write_pool())
            .await?;

        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM watched")
            .fetch_one(db.read_pool())
            .await?;
        assert_eq!(count, 0);

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
            999,
            "test migration",
            "CREATE TABLE migration_runner_test (id INTEGER PRIMARY KEY)",
        );

        super::run_migrations(db.write_pool(), &migrator).await?;

        let table_name: String = sqlx::query_scalar(
            "SELECT name FROM sqlite_master WHERE name = 'migration_runner_test'",
        )
        .fetch_one(db.write_pool())
        .await?;
        assert_eq!(table_name, "migration_runner_test");

        let recorded: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM schema_migrations WHERE version = 999")
                .fetch_one(db.write_pool())
                .await?;
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
        let migrator = test_migrator_with_embedded(999, "broken", "CREATE TABLE");

        let err = match super::run_migrations(db.write_pool(), &migrator).await {
            Ok(()) => panic!("broken migration was accepted"),
            Err(err) => err,
        };
        let recorded: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM schema_migrations WHERE version = 999")
                .fetch_one(db.write_pool())
                .await?;

        assert!(matches!(
            err,
            super::Error::MigrationFailed { version: 999, .. }
        ));
        assert!(err.to_string().contains("database migration 999 failed"));
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
                (7, String::from("canonical identities")),
                (8, String::from("request fulfillment plans")),
                (9, String::from("minimal media items")),
                (10, String::from("subtitle sidecars")),
                (11, String::from("metadata cache")),
                (12, String::from("source files")),
                (13, String::from("core catalog schemas")),
                (14, String::from("users")),
                (15, String::from("device tokens")),
                (16, String::from("playback state")),
                (17, String::from("playback sessions")),
                (18, String::from("subtitle provenance")),
                (19, String::from("catalog fts")),
                (20, String::from("metadata artwork")),
                (21, String::from("subtitle archive")),
                (22, String::from("watched transitions")),
                (23, String::from("source file probe data")),
                (24, String::from("subtitle forced flag")),
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

    #[tokio::test]
    async fn catalog_schema_enforces_source_and_transcode_foreign_keys()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = super::test_db().await?;
        let identity = movie_identity(603);
        let media_item_id = Id::new();
        let source_file_id = Id::new();
        let transcode_output_id = Id::new();
        let now = Timestamp::now();

        sqlx::query(
            r#"
            INSERT INTO canonical_identities (
                id,
                provider,
                media_kind,
                tmdb_id,
                source,
                created_at,
                updated_at
            )
            VALUES (?1, ?2, ?3, ?4, 'manual', ?5, ?6)
            "#,
        )
        .bind(identity)
        .bind(identity.provider().as_str())
        .bind(identity.kind().as_str())
        .bind(i64::from(identity.tmdb_id().get()))
        .bind(now)
        .bind(now)
        .execute(db.write_pool())
        .await?;

        sqlx::query(
            r#"
            INSERT INTO media_items (
                id,
                media_kind,
                canonical_identity_id,
                created_at,
                updated_at
            )
            VALUES (?1, 'movie', ?2, ?3, ?4)
            "#,
        )
        .bind(media_item_id)
        .bind(identity)
        .bind(now)
        .bind(now)
        .execute(db.write_pool())
        .await?;

        sqlx::query(
            r#"
            INSERT INTO source_files (
                id,
                media_item_id,
                path,
                created_at,
                updated_at
            )
            VALUES (?1, ?2, '/srv/media/Movies/The Matrix (1999)/The Matrix (1999).mkv', ?3, ?4)
            "#,
        )
        .bind(source_file_id)
        .bind(media_item_id)
        .bind(now)
        .bind(now)
        .execute(db.write_pool())
        .await?;

        sqlx::query(
            r#"
            INSERT INTO transcode_outputs (
                id,
                source_file_id,
                path,
                created_at,
                updated_at
            )
            VALUES (?1, ?2, '/srv/media/Movies/The Matrix (1999)/Streams/main.m3u8', ?3, ?4)
            "#,
        )
        .bind(transcode_output_id)
        .bind(source_file_id)
        .bind(now)
        .bind(now)
        .execute(db.write_pool())
        .await?;

        let missing_media_item = sqlx::query(
            r#"
            INSERT INTO source_files (
                id,
                media_item_id,
                path,
                created_at,
                updated_at
            )
            VALUES (?1, ?2, '/srv/media/orphan.mkv', ?3, ?4)
            "#,
        )
        .bind(Id::new())
        .bind(Id::new())
        .bind(now)
        .bind(now)
        .execute(db.write_pool())
        .await;

        let missing_source_file = sqlx::query(
            r#"
            INSERT INTO transcode_outputs (
                id,
                source_file_id,
                path,
                created_at,
                updated_at
            )
            VALUES (?1, ?2, '/srv/media/orphan.m3u8', ?3, ?4)
            "#,
        )
        .bind(Id::new())
        .bind(Id::new())
        .bind(now)
        .bind(now)
        .execute(db.write_pool())
        .await;

        assert!(missing_media_item.is_err());
        assert!(missing_source_file.is_err());

        db.close().await;
        Ok(())
    }

    async fn insert_personal_media_item(db: &super::Db) -> std::result::Result<Id, sqlx::Error> {
        let id = Id::new();
        let now = Timestamp::now();

        sqlx::query(
            r#"
            INSERT INTO media_items (
                id,
                media_kind,
                canonical_identity_id,
                created_at,
                updated_at
            )
            VALUES (?1, 'personal', NULL, ?2, ?3)
            "#,
        )
        .bind(id)
        .bind(now)
        .bind(now)
        .execute(db.write_pool())
        .await?;

        Ok(id)
    }

    async fn insert_playback_progress(
        db: &super::Db,
        progress: &PlaybackProgress,
    ) -> std::result::Result<(), sqlx::Error> {
        sqlx::query(
            r#"
            INSERT INTO playback_progress (
                user_id,
                media_item_id,
                position_seconds,
                updated_at,
                source_device_token_id
            )
            VALUES (?1, ?2, ?3, ?4, ?5)
            "#,
        )
        .bind(progress.user_id)
        .bind(progress.media_item_id)
        .bind(progress.position_seconds)
        .bind(progress.updated_at)
        .bind(progress.source_device_token_id)
        .execute(db.write_pool())
        .await?;

        Ok(())
    }

    async fn upsert_playback_progress(
        db: &super::Db,
        progress: &PlaybackProgress,
    ) -> std::result::Result<(), sqlx::Error> {
        sqlx::query(
            r#"
            INSERT INTO playback_progress (
                user_id,
                media_item_id,
                position_seconds,
                updated_at,
                source_device_token_id
            )
            VALUES (?1, ?2, ?3, ?4, ?5)
            ON CONFLICT(user_id, media_item_id) DO UPDATE SET
                position_seconds = excluded.position_seconds,
                updated_at = excluded.updated_at,
                source_device_token_id = excluded.source_device_token_id
            WHERE excluded.position_seconds > playback_progress.position_seconds
            "#,
        )
        .bind(progress.user_id)
        .bind(progress.media_item_id)
        .bind(progress.position_seconds)
        .bind(progress.updated_at)
        .bind(progress.source_device_token_id)
        .execute(db.write_pool())
        .await?;

        Ok(())
    }

    fn config(database_path: PathBuf) -> Config {
        Config {
            database_path,
            library_root: PathBuf::from("/srv/media"),
            library: Default::default(),
            server: Default::default(),
            tmdb: Default::default(),
            ocr: Default::default(),
            providers: Default::default(),
            log_level: "info".into(),
            log_format: Default::default(),
        }
    }

    fn movie_identity(tmdb_id: u32) -> CanonicalIdentityId {
        match TmdbId::new(tmdb_id) {
            Some(tmdb_id) => CanonicalIdentityId::tmdb_movie(tmdb_id),
            None => panic!("test tmdb id should be valid"),
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
