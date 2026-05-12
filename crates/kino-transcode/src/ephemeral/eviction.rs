//! LRU and TTL eviction for the ephemeral transcode cache.

use std::time::Duration;

use kino_core::Timestamp;
use tokio::{task::JoinHandle, time::sleep};
use tracing::error;

use super::store::{EphemeralOutput, EphemeralStore};
use crate::Result;

/// Runtime eviction tuning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EvictionConfig {
    /// Maximum total cached bytes before LRU rows are culled.
    pub max_size_bytes: u64,
    /// Maximum age since last access before rows are culled.
    pub max_age: Duration,
    /// Delay between background sweeps.
    pub eviction_tick: Duration,
}

impl From<&kino_core::EphemeralConfig> for EvictionConfig {
    fn from(config: &kino_core::EphemeralConfig) -> Self {
        Self {
            max_size_bytes: config.max_size_bytes,
            max_age: config.max_age,
            eviction_tick: config.eviction_tick,
        }
    }
}

/// Background task that evicts expired and least-recently-used live outputs.
#[derive(Clone)]
pub struct EvictionSweeper {
    store: EphemeralStore,
    config: EvictionConfig,
}

impl EvictionSweeper {
    /// Construct an eviction sweeper.
    pub fn new(store: EphemeralStore, config: EvictionConfig) -> Self {
        Self { store, config }
    }

    /// Spawn the eviction loop on the current Tokio runtime.
    pub fn spawn(self) -> JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                if let Err(err) = self.run_once().await {
                    error!(error = %err, "ephemeral transcode eviction failed");
                }
                sleep(self.config.eviction_tick).await;
            }
        })
    }

    /// Run one eviction sweep.
    pub async fn run_once(&self) -> Result<()> {
        self.evict_expired().await?;
        self.evict_over_size().await?;
        Ok(())
    }

    async fn evict_expired(&self) -> Result<()> {
        let cutoff = Timestamp::from_offset(
            Timestamp::now()
                .as_offset()
                .checked_sub(
                    time::Duration::try_from(self.config.max_age)
                        .map_err(|_| crate::Error::RetryBackoffTooLarge)?,
                )
                .ok_or(crate::Error::RetryTimestampOutOfRange)?,
        );

        for row in self.store.list_lru().await? {
            if row.last_access_at < cutoff {
                self.delete_output(row).await?;
            }
        }

        Ok(())
    }

    async fn evict_over_size(&self) -> Result<()> {
        let mut total = self.store.total_size_bytes().await?;
        while total > self.config.max_size_bytes {
            let Some(row) = self.store.oldest_row().await? else {
                break;
            };
            let size = row.size_bytes;
            self.delete_output(row).await?;
            total = total.saturating_sub(size);
        }

        Ok(())
    }

    async fn delete_output(&self, row: EphemeralOutput) -> Result<()> {
        let deleted = self.store.delete(row.id).await?;
        if let Some(output) = deleted {
            match tokio::fs::remove_dir_all(&output.directory_path).await {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => return Err(crate::Error::Io(err)),
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, time::Duration};

    use kino_core::{Id, Timestamp};
    use kino_db::Db;
    use tempfile::TempDir;

    use super::*;
    use crate::{EphemeralStore, NewEphemeralOutput};

    #[tokio::test]
    async fn size_cap_eviction_removes_lru_rows_and_directories()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = kino_db::test_db().await?;
        let source_file_id = insert_source_file(&db, "/library/source.mkv").await?;
        let temp = TempDir::new()?;
        let store = EphemeralStore::new(db);
        let first_dir = cache_dir(temp.path(), "first")?;
        let second_dir = cache_dir(temp.path(), "second")?;
        let first = store
            .insert(&new_output(source_file_id, 1, &first_dir, 80))
            .await?;
        let second = store
            .insert(&new_output(source_file_id, 2, &second_dir, 80))
            .await?;
        store.bump_access(second.id).await?;
        let sweeper = EvictionSweeper::new(
            store.clone(),
            EvictionConfig {
                max_size_bytes: 100,
                max_age: Duration::from_secs(60),
                eviction_tick: Duration::from_secs(60),
            },
        );

        sweeper.run_once().await?;

        assert!(store.fetch_by_key(source_file_id, [1; 32]).await?.is_none());
        assert!(store.fetch_by_key(source_file_id, [2; 32]).await?.is_some());
        assert!(!first_dir.exists());
        assert!(second_dir.exists());
        assert_ne!(first.id, second.id);
        Ok(())
    }

    #[tokio::test]
    async fn ttl_eviction_removes_expired_rows()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = kino_db::test_db().await?;
        let source_file_id = insert_source_file(&db, "/library/source.mkv").await?;
        let temp = TempDir::new()?;
        let store = EphemeralStore::new(db.clone());
        let old_dir = cache_dir(temp.path(), "old")?;
        let fresh_dir = cache_dir(temp.path(), "fresh")?;
        let old = store
            .insert(&new_output(source_file_id, 1, &old_dir, 80))
            .await?;
        let _fresh = store
            .insert(&new_output(source_file_id, 2, &fresh_dir, 80))
            .await?;
        let expired = Timestamp::from_offset(
            Timestamp::now()
                .as_offset()
                .checked_sub(time::Duration::hours(2))
                .ok_or("timestamp underflow")?,
        );
        sqlx::query("UPDATE ephemeral_transcodes SET last_access_at = ?1 WHERE id = ?2")
            .bind(expired)
            .bind(old.id)
            .execute(db.write_pool())
            .await?;
        let sweeper = EvictionSweeper::new(
            store.clone(),
            EvictionConfig {
                max_size_bytes: 1_000,
                max_age: Duration::from_secs(60),
                eviction_tick: Duration::from_secs(60),
            },
        );

        sweeper.run_once().await?;

        assert!(store.fetch_by_key(source_file_id, [1; 32]).await?.is_none());
        assert!(store.fetch_by_key(source_file_id, [2; 32]).await?.is_some());
        assert!(!old_dir.exists());
        assert!(fresh_dir.exists());
        Ok(())
    }

    fn cache_dir(root: &std::path::Path, name: &str) -> std::io::Result<std::path::PathBuf> {
        let path = root.join(name);
        fs::create_dir(&path)?;
        fs::write(path.join("media.m3u8"), b"#EXTM3U\n")?;
        Ok(path)
    }

    fn new_output(
        source_file_id: Id,
        seed: u8,
        directory_path: &std::path::Path,
        size_bytes: u64,
    ) -> NewEphemeralOutput {
        NewEphemeralOutput {
            id: Id::new(),
            source_file_id,
            profile_hash: [seed; 32],
            profile_json: format!(r#"{{"seed":{seed}}}"#),
            directory_path: directory_path.to_path_buf(),
            size_bytes,
        }
    }

    async fn insert_source_file(db: &Db, path: &str) -> std::result::Result<Id, sqlx::Error> {
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
}
