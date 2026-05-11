//! Deliberate re-OCR for existing image subtitle sidecars.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use kino_core::{Id, Timestamp};
use kino_db::Db;
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::Row;
use tokio::io::AsyncReadExt;

use crate::{
    Error, FfmpegImageSubtitleExtractor, ImageSubtitleExtraction, ImageSubtitleExtractionInput,
    ProbeSubtitleKind, Result, SubtitleFormat, SubtitleProvenance, SubtitleSidecar, create_dir_all,
    default_subtitle_staging_dir, path_to_db_text,
    subtitle_ocr::{OcrEngine, TesseractOcrEngine, ocr_subtitle_track},
    subtitle_sidecar_from_row, write_ocr_sidecar,
};

/// Tracking record returned when a re-OCR action is accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct ReocrJob {
    /// Tracking id for this synchronous re-OCR execution.
    pub job_id: Id,
    /// Time the re-OCR action started.
    pub started_at: Timestamp,
}

/// Library service for deliberate OCR regeneration of subtitle sidecars.
#[derive(Clone)]
pub struct SubtitleReocrService {
    db: Db,
    extractor: Arc<dyn ImageSubtitleExtraction>,
    engine: Arc<dyn OcrEngine>,
}

impl SubtitleReocrService {
    /// Construct a re-OCR service from explicit extraction and OCR engines.
    pub fn new(
        db: Db,
        extractor: Arc<dyn ImageSubtitleExtraction>,
        engine: Arc<dyn OcrEngine>,
    ) -> Self {
        Self {
            db,
            extractor,
            engine,
        }
    }

    /// Construct a re-OCR service using process-path ffmpeg and Tesseract defaults.
    pub fn with_default_tools(db: Db, library_root: impl AsRef<Path>) -> Self {
        Self::new(
            db,
            Arc::new(FfmpegImageSubtitleExtractor::new(
                default_subtitle_staging_dir(library_root.as_ref()),
            )),
            Arc::new(TesseractOcrEngine::from_env()),
        )
    }

    /// Re-run OCR for the current OCR sidecar on `track_index`.
    ///
    /// Phase 2 runs this synchronously; callers still receive a stable `job_id`
    /// so the HTTP contract can move to a real queue without changing shape.
    pub async fn reocr_track(&self, media_item_id: Id, track_index: u32) -> Result<ReocrJob> {
        reocr_track(
            &self.db,
            self.extractor.as_ref(),
            self.engine.as_ref(),
            media_item_id,
            track_index,
        )
        .await
    }
}

/// Re-run OCR for one media item's current OCR sidecar and archive the old row.
///
/// The existing sidecar remains queryable with `archived_at` set, while the new
/// JSON sidecar is inserted as the current row for the same language and track.
pub async fn reocr_track(
    db: &Db,
    extractor: &dyn ImageSubtitleExtraction,
    engine: &dyn OcrEngine,
    media_item_id: Id,
    track_index: u32,
) -> Result<ReocrJob> {
    let job = ReocrJob {
        job_id: Id::new(),
        started_at: Timestamp::now(),
    };
    let current = current_ocr_sidecar(db, media_item_id, track_index).await?;
    let source_path = source_path_for_media_item(db, media_item_id).await?;
    let input_file_hash = file_sha256_hex(&source_path).await?;
    // Sidecars do not persist the original image subtitle codec yet; extraction
    // only needs an image classification to accept the stream.
    let frames = extractor
        .extract_image_subtitle_track(ImageSubtitleExtractionInput::new(
            &source_path,
            input_file_hash,
            track_index,
            ProbeSubtitleKind::ImagePgs,
        ))
        .await?;
    let cues = ocr_subtitle_track(engine, &frames).await?;
    let sidecar_dir = current_sidecar_dir(&current.path);
    create_dir_all(&sidecar_dir).await?;
    let new_path = sidecar_dir.join(versioned_sidecar_file_name(
        media_item_id,
        &current.language,
        track_index,
        job.job_id,
    ));
    write_ocr_sidecar(&new_path, &cues).await?;
    let path_text = path_to_db_text(&new_path)?;
    let mut tx = db.write_pool().begin().await?;

    let archive_result = sqlx::query(
        r#"
        UPDATE subtitle_sidecars
        SET archived_at = ?1,
            updated_at = ?1
        WHERE id = ?2
            AND archived_at IS NULL
        "#,
    )
    .bind(job.started_at)
    .bind(current.id)
    .execute(&mut *tx)
    .await?;
    if archive_result.rows_affected() == 0 {
        return Err(Error::CurrentOcrSidecarNotFound {
            media_item_id,
            track_index,
        });
    }

    sqlx::query(
        r#"
        INSERT INTO subtitle_sidecars (
            id,
            media_item_id,
            language,
            format,
            provenance,
            track_index,
            path,
            archived_at,
            created_at,
            updated_at
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, ?8, ?9)
        "#,
    )
    .bind(job.job_id)
    .bind(media_item_id)
    .bind(&current.language)
    .bind(SubtitleFormat::Json.as_str())
    .bind(SubtitleProvenance::Ocr.as_str())
    .bind(i64::from(track_index))
    .bind(path_text)
    .bind(job.started_at)
    .bind(job.started_at)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    tracing::info!(
        media_item_id = %media_item_id,
        track_index,
        job_id = %job.job_id,
        "subtitle re-ocr completed"
    );

    Ok(job)
}

async fn current_ocr_sidecar(
    db: &Db,
    media_item_id: Id,
    track_index: u32,
) -> Result<SubtitleSidecar> {
    let row = sqlx::query(
        r#"
        SELECT
            id,
            media_item_id,
            language,
            format,
            provenance,
            track_index,
            path,
            archived_at,
            created_at,
            updated_at
        FROM subtitle_sidecars
        WHERE media_item_id = ?1
            AND track_index = ?2
            AND format = 'json'
            AND provenance = 'ocr'
            AND archived_at IS NULL
        ORDER BY created_at DESC, id DESC
        LIMIT 1
        "#,
    )
    .bind(media_item_id)
    .bind(i64::from(track_index))
    .fetch_optional(db.read_pool())
    .await?;

    row.as_ref()
        .map(subtitle_sidecar_from_row)
        .transpose()?
        .ok_or(Error::CurrentOcrSidecarNotFound {
            media_item_id,
            track_index,
        })
}

async fn source_path_for_media_item(db: &Db, media_item_id: Id) -> Result<PathBuf> {
    let row = sqlx::query(
        r#"
        SELECT source_files.path AS source_path
        FROM media_items
        LEFT JOIN source_files
            ON source_files.media_item_id = media_items.id
        WHERE media_items.id = ?1
        ORDER BY source_files.created_at, source_files.id
        LIMIT 1
        "#,
    )
    .bind(media_item_id)
    .fetch_optional(db.read_pool())
    .await?;

    let Some(row) = row else {
        return Err(Error::MediaItemNotFound { id: media_item_id });
    };
    let source_path: Option<String> = row.try_get("source_path")?;
    source_path
        .map(PathBuf::from)
        .ok_or(Error::MediaItemSourceFileNotFound { id: media_item_id })
}

async fn file_sha256_hex(path: &Path) -> Result<String> {
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|source| Error::Io {
            path: path.to_path_buf(),
            source,
        })?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];

    loop {
        let read = file.read(&mut buffer).await.map_err(|source| Error::Io {
            path: path.to_path_buf(),
            source,
        })?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }

    Ok(format!("{:x}", hasher.finalize()))
}

fn current_sidecar_dir(path: &Path) -> PathBuf {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn versioned_sidecar_file_name(
    media_item_id: Id,
    language: &str,
    track_index: u32,
    job_id: Id,
) -> String {
    let format = SubtitleFormat::Json;
    format!(
        "{media_item_id}.{language}.{track_index}.{job_id}.{}",
        format.extension()
    )
}

#[cfg(test)]
mod tests {
    use std::{future::Future, path::Path, pin::Pin, time::Duration};

    use crate::{
        CatalogService, ImageSubtitleFrame, SubtitleService, subtitle_ocr::OcrFrameResult,
    };

    use super::*;

    struct FakeExtractor {
        frames: Vec<ImageSubtitleFrame>,
    }

    impl ImageSubtitleExtraction for FakeExtractor {
        fn extract_image_subtitle_track<'a>(
            &'a self,
            input: ImageSubtitleExtractionInput,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<ImageSubtitleFrame>>> + Send + 'a>> {
            Box::pin(async move {
                assert_eq!(input.stream_index, 7);
                assert_eq!(input.kind, ProbeSubtitleKind::ImagePgs);
                Ok(self.frames.clone())
            })
        }
    }

    struct FakeOcrEngine;

    impl OcrEngine for FakeOcrEngine {
        fn ocr(&self, _image_path: &Path) -> Result<OcrFrameResult> {
            Ok(OcrFrameResult {
                text: String::from("NEW OCR"),
                avg_confidence: 97.25,
            })
        }
    }

    #[tokio::test]
    async fn reocr_track_archives_old_sidecar_and_inserts_new_current()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = kino_db::test_db().await?;
        let media_item_id = insert_personal_media_item(&db).await?;
        let temp = tempfile::tempdir()?;
        let source_path = temp.path().join("movie.mkv");
        tokio::fs::write(&source_path, b"media bytes").await?;
        insert_source_file(&db, media_item_id, &source_path).await?;
        let sidecar_dir = temp.path().join("sidecars");
        tokio::fs::create_dir_all(&sidecar_dir).await?;
        let old_path = sidecar_dir.join("old.json");
        tokio::fs::write(&old_path, br#"{"provenance":"ocr","cues":[]}"#).await?;
        let old_id = insert_ocr_sidecar(&db, media_item_id, 7, "eng", &old_path).await?;
        let frame_path = temp.path().join("frame.png");
        let service = SubtitleReocrService::new(
            db.clone(),
            Arc::new(FakeExtractor {
                frames: vec![ImageSubtitleFrame::new(
                    Duration::from_secs(1),
                    Duration::from_secs(2),
                    &frame_path,
                )],
            }),
            Arc::new(FakeOcrEngine),
        );

        let job = service.reocr_track(media_item_id, 7).await?;

        let archived_at: Option<Timestamp> =
            sqlx::query_scalar("SELECT archived_at FROM subtitle_sidecars WHERE id = ?1")
                .bind(old_id)
                .fetch_one(db.read_pool())
                .await?;
        assert_eq!(archived_at, Some(job.started_at));

        let current: (Id, Option<Timestamp>, String) = sqlx::query_as(
            "SELECT id, archived_at, path FROM subtitle_sidecars WHERE media_item_id = ?1 AND track_index = 7 AND archived_at IS NULL",
        )
        .bind(media_item_id)
        .fetch_one(db.read_pool())
        .await?;
        assert_eq!(current.0, job.job_id);
        assert_eq!(current.1, None);
        assert!(current.2.contains(&job.job_id.to_string()));

        let json = tokio::fs::read_to_string(current.2).await?;
        let sidecar: serde_json::Value = serde_json::from_str(&json)?;
        assert_eq!(sidecar["cues"][0]["text"], "NEW OCR");
        assert_eq!(sidecar["cues"][0]["confidence"], 97.25);

        let sidecars = SubtitleService::new(db.clone())
            .sidecars(media_item_id)
            .await?;
        assert_eq!(sidecars.len(), 2);
        assert_eq!(
            sidecars
                .iter()
                .filter(|sidecar| sidecar.archived_at.is_none())
                .count(),
            1
        );

        let catalog_item = CatalogService::new(db).get(media_item_id).await?;
        assert_eq!(catalog_item.subtitle_tracks.len(), 1);
        assert_eq!(catalog_item.subtitle_tracks[0].id, job.job_id);

        Ok(())
    }

    async fn insert_personal_media_item(db: &Db) -> std::result::Result<Id, sqlx::Error> {
        let id = Id::new();
        let now = Timestamp::now();
        sqlx::query(
            r#"
            INSERT INTO media_items (
                id,
                media_kind,
                canonical_identity_id,
                season_number,
                episode_number,
                created_at,
                updated_at
            )
            VALUES (?1, 'personal', NULL, NULL, NULL, ?2, ?3)
            "#,
        )
        .bind(id)
        .bind(now)
        .bind(now)
        .execute(db.write_pool())
        .await?;
        Ok(id)
    }

    async fn insert_source_file(
        db: &Db,
        media_item_id: Id,
        path: &Path,
    ) -> std::result::Result<Id, sqlx::Error> {
        let id = Id::new();
        let now = Timestamp::now();
        let path = path.to_string_lossy().into_owned();
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
        .bind(id)
        .bind(media_item_id)
        .bind(path)
        .bind(now)
        .bind(now)
        .execute(db.write_pool())
        .await?;
        Ok(id)
    }

    async fn insert_ocr_sidecar(
        db: &Db,
        media_item_id: Id,
        track_index: u32,
        language: &str,
        path: &Path,
    ) -> std::result::Result<Id, sqlx::Error> {
        let id = Id::new();
        let now = Timestamp::now();
        let path = path.to_string_lossy().into_owned();
        sqlx::query(
            r#"
            INSERT INTO subtitle_sidecars (
                id,
                media_item_id,
                language,
                format,
                provenance,
                track_index,
                path,
                created_at,
                updated_at
            )
            VALUES (?1, ?2, ?3, 'json', 'ocr', ?4, ?5, ?6, ?7)
            "#,
        )
        .bind(id)
        .bind(media_item_id)
        .bind(language)
        .bind(i64::from(track_index))
        .bind(path)
        .bind(now)
        .bind(now)
        .execute(db.write_pool())
        .await?;
        Ok(id)
    }
}
