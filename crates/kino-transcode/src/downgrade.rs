//! Durable color downgrade classification for transcode outputs.

use std::str::FromStr;

use kino_core::{Id, Timestamp};
use kino_db::Db;

use crate::{ColorDowngrade, Result};

/// Persistent color downgrade query layer.
#[derive(Clone)]
pub struct DowngradeStore {
    db: Db,
}

impl DowngradeStore {
    /// Construct a downgrade store backed by `db`.
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// Insert or replace the downgrade classification for one transcode output.
    pub async fn insert_color_downgrade(
        &self,
        transcode_output_id: Id,
        kind: ColorDowngrade,
        note: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            r#"
            INSERT INTO transcode_color_downgrades (
                transcode_output_id,
                kind,
                note,
                created_at
            )
            VALUES (?1, ?2, ?3, ?4)
            ON CONFLICT(transcode_output_id) DO UPDATE SET
                kind = excluded.kind,
                note = excluded.note,
                created_at = excluded.created_at
            "#,
        )
        .bind(transcode_output_id)
        .bind(kind.as_str())
        .bind(note)
        .bind(Timestamp::now())
        .execute(self.db.write_pool())
        .await?;

        Ok(())
    }
}

impl FromStr for ColorDowngrade {
    type Err = crate::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "dv_to_hdr10" => Ok(Self::DvToHdr10),
            "hdr10_to_sdr" => Ok(Self::Hdr10ToSdr),
            "dv_to_sdr" => Ok(Self::DvToSdr),
            other => Err(crate::Error::InvalidColorDowngrade(other.to_owned())),
        }
    }
}

#[cfg(test)]
mod tests {
    use kino_core::{Id, Timestamp};
    use kino_db::Db;
    use sqlx::Row;

    use super::*;

    #[tokio::test]
    async fn insert_color_downgrade_round_trips()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = kino_db::test_db().await?;
        let source_file_id = insert_source_file(&db, "/library/source.mkv").await?;
        let output_id =
            insert_transcode_output(&db, source_file_id, "/library/out/media.m3u8").await?;
        let store = DowngradeStore::new(db.clone());

        store
            .insert_color_downgrade(output_id, ColorDowngrade::Hdr10ToSdr, Some("compatibility"))
            .await?;

        let row = sqlx::query(
            r#"
            SELECT kind, note
            FROM transcode_color_downgrades
            WHERE transcode_output_id = ?1
            "#,
        )
        .bind(output_id)
        .fetch_one(db.read_pool())
        .await?;

        let kind: String = row.try_get("kind")?;
        let note: Option<String> = row.try_get("note")?;
        assert_eq!(kind.parse::<ColorDowngrade>()?, ColorDowngrade::Hdr10ToSdr);
        assert_eq!(note.as_deref(), Some("compatibility"));
        Ok(())
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

    async fn insert_transcode_output(
        db: &Db,
        source_file_id: Id,
        path: &str,
    ) -> std::result::Result<Id, sqlx::Error> {
        let id = Id::new();
        let now = Timestamp::now();
        sqlx::query(
            r#"
            INSERT INTO transcode_outputs (
                id,
                source_file_id,
                path,
                created_at,
                updated_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5)
            "#,
        )
        .bind(id)
        .bind(source_file_id)
        .bind(path)
        .bind(now)
        .bind(now)
        .execute(db.write_pool())
        .await?;

        Ok(id)
    }
}
