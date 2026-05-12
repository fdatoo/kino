//! SQLx accessors for the ephemeral transcode cache.

use std::path::PathBuf;

use kino_core::{Id, Timestamp};
use kino_db::Db;
use sqlx::{Row, sqlite::SqliteRow};

use crate::{Error, Result};

const EPHEMERAL_FIELDS: &str = r#"
    id,
    source_file_id,
    profile_hash,
    profile_json,
    directory_path,
    size_bytes,
    created_at,
    last_access_at
"#;

/// Row representation of an `ephemeral_transcodes` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EphemeralOutput {
    /// Ephemeral output id.
    pub id: Id,
    /// Source file this output transcodes.
    pub source_file_id: Id,
    /// SHA-256 digest of the canonical transcode profile JSON.
    pub profile_hash: [u8; 32],
    /// Canonical transcode profile JSON.
    pub profile_json: String,
    /// Directory containing `media.m3u8`, `init.mp4`, and CMAF media segments.
    pub directory_path: PathBuf,
    /// Total on-disk size of files in `directory_path`.
    pub size_bytes: u64,
    /// Row creation timestamp.
    pub created_at: Timestamp,
    /// Most recent cache access timestamp.
    pub last_access_at: Timestamp,
}

/// Insert payload for `EphemeralStore::insert`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewEphemeralOutput {
    /// Ephemeral output id.
    pub id: Id,
    /// Source file this output transcodes.
    pub source_file_id: Id,
    /// SHA-256 digest of the canonical transcode profile JSON.
    pub profile_hash: [u8; 32],
    /// Canonical transcode profile JSON.
    pub profile_json: String,
    /// Directory containing `media.m3u8`, `init.mp4`, and CMAF media segments.
    pub directory_path: PathBuf,
    /// Total on-disk size of files in `directory_path`.
    pub size_bytes: u64,
}

/// Persistent ephemeral-transcode query layer.
#[derive(Clone)]
pub struct EphemeralStore {
    db: Db,
}

impl EphemeralStore {
    /// Construct an ephemeral store backed by `db`.
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// Return the backing database handle for route-level compatibility lookups.
    pub fn db(&self) -> &Db {
        &self.db
    }

    /// Insert or replace a completed ephemeral output for a source/profile key.
    pub async fn insert(&self, new: &NewEphemeralOutput) -> Result<EphemeralOutput> {
        let now = Timestamp::now();
        let size_bytes =
            i64::try_from(new.size_bytes).map_err(|_| Error::InvalidEphemeralSize {
                value: new.size_bytes,
            })?;
        let directory_path = new.directory_path.to_string_lossy().into_owned();

        let row = sqlx::query(&format!(
            r#"
            INSERT INTO ephemeral_transcodes (
                id,
                source_file_id,
                profile_hash,
                profile_json,
                directory_path,
                size_bytes,
                created_at,
                last_access_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            ON CONFLICT(source_file_id, profile_hash) DO UPDATE SET
                id = excluded.id,
                profile_json = excluded.profile_json,
                directory_path = excluded.directory_path,
                size_bytes = excluded.size_bytes,
                created_at = excluded.created_at,
                last_access_at = excluded.last_access_at
            RETURNING {EPHEMERAL_FIELDS}
            "#
        ))
        .bind(new.id)
        .bind(new.source_file_id)
        .bind(new.profile_hash.as_slice())
        .bind(&new.profile_json)
        .bind(directory_path)
        .bind(size_bytes)
        .bind(now)
        .bind(now)
        .fetch_one(self.db.write_pool())
        .await?;

        ephemeral_from_row(&row)
    }

    /// Fetch a cached ephemeral output by source/profile key.
    pub async fn fetch_by_key(
        &self,
        source_file_id: Id,
        profile_hash: [u8; 32],
    ) -> Result<Option<EphemeralOutput>> {
        let row = sqlx::query(&format!(
            r#"
            SELECT {EPHEMERAL_FIELDS}
            FROM ephemeral_transcodes
            WHERE source_file_id = ?1 AND profile_hash = ?2
            "#
        ))
        .bind(source_file_id)
        .bind(profile_hash.as_slice())
        .fetch_optional(self.db.read_pool())
        .await?;

        row.as_ref().map(ephemeral_from_row).transpose()
    }

    /// Update the last-access timestamp for an ephemeral output.
    pub async fn bump_access(&self, id: Id) -> Result<()> {
        let now = Timestamp::now();
        let result = sqlx::query(
            r#"
            UPDATE ephemeral_transcodes
            SET last_access_at = ?1
            WHERE id = ?2
            "#,
        )
        .bind(now)
        .bind(id)
        .execute(self.db.write_pool())
        .await?;

        if result.rows_affected() == 0 {
            return Err(Error::EphemeralOutputNotFound { id });
        }

        Ok(())
    }

    /// Delete an ephemeral output row and return the deleted row when present.
    pub async fn delete(&self, id: Id) -> Result<Option<EphemeralOutput>> {
        let row = sqlx::query(&format!(
            r#"
            SELECT {EPHEMERAL_FIELDS}
            FROM ephemeral_transcodes
            WHERE id = ?1
            "#
        ))
        .bind(id)
        .fetch_optional(self.db.write_pool())
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let output = ephemeral_from_row(&row)?;

        sqlx::query("DELETE FROM ephemeral_transcodes WHERE id = ?1")
            .bind(id)
            .execute(self.db.write_pool())
            .await?;

        Ok(Some(output))
    }

    /// Return the total size of all cached ephemeral outputs.
    pub async fn total_size_bytes(&self) -> Result<u64> {
        let value: Option<i64> =
            sqlx::query_scalar("SELECT SUM(size_bytes) FROM ephemeral_transcodes")
                .fetch_one(self.db.read_pool())
                .await?;
        let value = value.unwrap_or(0);
        u64::try_from(value).map_err(|_| Error::InvalidEphemeralPersistedSize { value })
    }

    /// Return the least recently accessed output, if any.
    pub async fn oldest_row(&self) -> Result<Option<EphemeralOutput>> {
        let row = sqlx::query(&format!(
            r#"
            SELECT {EPHEMERAL_FIELDS}
            FROM ephemeral_transcodes
            ORDER BY last_access_at ASC, created_at ASC, id ASC
            LIMIT 1
            "#
        ))
        .fetch_optional(self.db.read_pool())
        .await?;

        row.as_ref().map(ephemeral_from_row).transpose()
    }

    /// List all cached outputs from least to most recently used.
    pub async fn list_lru(&self) -> Result<Vec<EphemeralOutput>> {
        let rows = sqlx::query(&format!(
            r#"
            SELECT {EPHEMERAL_FIELDS}
            FROM ephemeral_transcodes
            ORDER BY last_access_at ASC, created_at ASC, id ASC
            "#
        ))
        .fetch_all(self.db.read_pool())
        .await?;

        rows.iter().map(ephemeral_from_row).collect()
    }
}

fn ephemeral_from_row(row: &SqliteRow) -> Result<EphemeralOutput> {
    let profile_hash: Vec<u8> = row.try_get("profile_hash")?;
    let profile_hash = profile_hash
        .try_into()
        .map_err(|hash: Vec<u8>| Error::InvalidEphemeralProfileHashLength { len: hash.len() })?;
    let size_value: i64 = row.try_get("size_bytes")?;
    let size_bytes = u64::try_from(size_value)
        .map_err(|_| Error::InvalidEphemeralPersistedSize { value: size_value })?;
    let directory_path: String = row.try_get("directory_path")?;

    Ok(EphemeralOutput {
        id: row.try_get("id")?,
        source_file_id: row.try_get("source_file_id")?,
        profile_hash,
        profile_json: row.try_get("profile_json")?,
        directory_path: PathBuf::from(directory_path),
        size_bytes,
        created_at: row.try_get("created_at")?,
        last_access_at: row.try_get("last_access_at")?,
    })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use kino_core::Timestamp;
    use kino_db::Db;

    use super::*;

    #[tokio::test]
    async fn insert_fetch_delete_round_trips_output()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = kino_db::test_db().await?;
        let source_file_id = insert_source_file(&db, "/library/source.mkv").await?;
        let store = EphemeralStore::new(db);
        let new = new_output(source_file_id, 1, 128);

        let inserted = store.insert(&new).await?;
        let fetched = store.fetch_by_key(source_file_id, [1; 32]).await?;
        let deleted = store.delete(inserted.id).await?;
        let missing = store.fetch_by_key(source_file_id, [1; 32]).await?;

        assert_eq!(Some(inserted.clone()), fetched);
        assert_eq!(Some(inserted), deleted);
        assert_eq!(missing, None);
        Ok(())
    }

    #[tokio::test]
    async fn lru_order_tracks_last_access() -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = kino_db::test_db().await?;
        let source_file_id = insert_source_file(&db, "/library/source.mkv").await?;
        let store = EphemeralStore::new(db);
        let first = store.insert(&new_output(source_file_id, 1, 128)).await?;
        tokio::time::sleep(Duration::from_millis(2)).await;
        let second = store.insert(&new_output(source_file_id, 2, 128)).await?;

        store.bump_access(first.id).await?;
        let rows = store.list_lru().await?;
        let oldest = store.oldest_row().await?;

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, second.id);
        assert_eq!(rows[1].id, first.id);
        assert_eq!(oldest.map(|row| row.id), Some(second.id));
        Ok(())
    }

    pub(crate) async fn insert_source_file(
        db: &Db,
        path: &str,
    ) -> std::result::Result<Id, sqlx::Error> {
        let media_item_id = Id::new();
        let source_file_id = Id::new();
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
        .bind(media_item_id)
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
            VALUES (?1, ?2, ?3, ?4, ?5)
            "#,
        )
        .bind(source_file_id)
        .bind(media_item_id)
        .bind(path)
        .bind(now)
        .bind(now)
        .execute(db.write_pool())
        .await?;

        Ok(source_file_id)
    }

    pub(crate) fn new_output(source_file_id: Id, seed: u8, size_bytes: u64) -> NewEphemeralOutput {
        NewEphemeralOutput {
            id: Id::new(),
            source_file_id,
            profile_hash: [seed; 32],
            profile_json: format!(r#"{{"seed":{seed}}}"#),
            directory_path: PathBuf::from(format!("/tmp/kino-ephemeral-{seed}")),
            size_bytes,
        }
    }
}
