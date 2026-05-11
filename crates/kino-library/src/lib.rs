//! Library media catalog support.
//!
//! This crate owns persisted library-facing data that is attached to
//! `MediaItem` records, including subtitle sidecars extracted by ingestion.

use std::{
    collections::HashSet,
    fmt,
    path::{Path, PathBuf},
};

use kino_core::{Id, Timestamp};
use kino_db::Db;
use sqlx::Row;

/// Errors produced by `kino-library`.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A database operation failed.
    #[error("database error: {0}")]
    Sqlx(#[from] sqlx::Error),

    /// A filesystem operation failed for a subtitle sidecar path.
    #[error("subtitle sidecar io failed for {path}: {source}")]
    Io {
        /// Path involved in the failed operation.
        path: PathBuf,
        /// Underlying filesystem error.
        #[source]
        source: std::io::Error,
    },

    /// A subtitle sidecar path could not be stored as database text.
    #[error("subtitle sidecar path is not utf-8: {path}")]
    NonUtf8Path {
        /// Non-UTF-8 path.
        path: PathBuf,
    },

    /// A probed subtitle track did not include a language.
    #[error("subtitle track {track_index} has no language")]
    EmptyLanguage {
        /// Probed subtitle stream index.
        track_index: u32,
    },

    /// A subtitle language contained characters unsafe for indexing and paths.
    #[error("subtitle language is invalid: {language}")]
    InvalidLanguage {
        /// Invalid language value.
        language: String,
    },

    /// A text subtitle track was present but contained no extracted text.
    #[error("subtitle track {track_index} has no text")]
    EmptySubtitleText {
        /// Probed subtitle stream index.
        track_index: u32,
    },

    /// A single extraction request included the same text track more than once.
    #[error("duplicate subtitle track: {language} {format} track {track_index}")]
    DuplicateSubtitleTrack {
        /// Normalized language.
        language: String,
        /// Text subtitle format.
        format: SubtitleFormat,
        /// Probed subtitle stream index.
        track_index: u32,
    },
}

/// Crate-local `Result` alias.
pub type Result<T> = std::result::Result<T, Error>;

/// Subtitle formats discovered by the probe step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProbedSubtitleFormat {
    /// SubRip text subtitles.
    Srt,
    /// Advanced SubStation Alpha text subtitles.
    Ass,
    /// Presentation Graphic Stream image subtitles.
    Pgs,
    /// VOBSUB image subtitles.
    VobSub,
}

impl ProbedSubtitleFormat {
    fn text_format(self) -> Option<SubtitleFormat> {
        match self {
            Self::Srt => Some(SubtitleFormat::Srt),
            Self::Ass => Some(SubtitleFormat::Ass),
            Self::Pgs | Self::VobSub => None,
        }
    }
}

/// Text subtitle formats persisted as sidecars.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SubtitleFormat {
    /// SubRip text subtitles.
    Srt,
    /// Advanced SubStation Alpha text subtitles.
    Ass,
}

impl SubtitleFormat {
    /// Database representation for this subtitle format.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Srt => "srt",
            Self::Ass => "ass",
        }
    }

    /// File extension used for sidecar files of this format.
    pub fn extension(self) -> &'static str {
        self.as_str()
    }
}

impl fmt::Display for SubtitleFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Subtitle stream data supplied by the probe and extraction layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbedSubtitleTrack {
    /// Probed subtitle stream index in the source file.
    pub track_index: u32,
    /// Language reported for the subtitle stream.
    pub language: String,
    /// Detected subtitle format.
    pub format: ProbedSubtitleFormat,
    /// Extracted text for SRT and ASS tracks.
    pub text: String,
}

impl ProbedSubtitleTrack {
    /// Construct a probed subtitle track.
    pub fn new(
        track_index: u32,
        language: impl Into<String>,
        format: ProbedSubtitleFormat,
        text: impl Into<String>,
    ) -> Self {
        Self {
            track_index,
            language: language.into(),
            format,
            text: text.into(),
        }
    }
}

/// Input for extracting text subtitle sidecars for one media item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubtitleExtractionInput {
    /// Media item that owns the subtitle sidecars.
    pub media_item_id: Id,
    /// Directory where sidecars should be written.
    pub sidecar_dir: PathBuf,
    /// Probed subtitle tracks from the source file.
    pub tracks: Vec<ProbedSubtitleTrack>,
}

impl SubtitleExtractionInput {
    /// Construct subtitle extraction input.
    pub fn new(
        media_item_id: Id,
        sidecar_dir: impl Into<PathBuf>,
        tracks: Vec<ProbedSubtitleTrack>,
    ) -> Self {
        Self {
            media_item_id,
            sidecar_dir: sidecar_dir.into(),
            tracks,
        }
    }
}

/// Persisted subtitle sidecar record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubtitleSidecar {
    /// Sidecar record id.
    pub id: Id,
    /// Media item that owns the sidecar.
    pub media_item_id: Id,
    /// Normalized language for query and display.
    pub language: String,
    /// Text subtitle format.
    pub format: SubtitleFormat,
    /// Probed subtitle stream index in the source file.
    pub track_index: u32,
    /// Filesystem path to the sidecar file.
    pub path: PathBuf,
    /// Creation timestamp.
    pub created_at: Timestamp,
    /// Last update timestamp.
    pub updated_at: Timestamp,
}

/// Result of a subtitle extraction run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubtitleExtractionResult {
    /// Sidecars written and indexed by this run.
    pub sidecars: Vec<SubtitleSidecar>,
    /// Distinct indexed subtitle languages for the media item after extraction.
    pub languages: Vec<String>,
}

/// Library service for subtitle sidecar extraction and indexing.
#[derive(Clone)]
pub struct SubtitleService {
    db: Db,
}

impl SubtitleService {
    /// Construct a subtitle service backed by Kino's database.
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// Write text subtitle tracks to sidecars and index them by language.
    pub async fn extract_text_subtitles(
        &self,
        input: SubtitleExtractionInput,
    ) -> Result<SubtitleExtractionResult> {
        create_dir_all(&input.sidecar_dir).await?;

        let mut seen = HashSet::new();
        let mut sidecars = Vec::new();
        let now = Timestamp::now();
        let mut tx = self.db.write_pool().begin().await?;

        for track in input.tracks {
            let Some(format) = track.format.text_format() else {
                continue;
            };

            let language = normalize_language(&track.language, track.track_index)?;
            if track.text.is_empty() {
                return Err(Error::EmptySubtitleText {
                    track_index: track.track_index,
                });
            }

            if !seen.insert((language.clone(), format, track.track_index)) {
                return Err(Error::DuplicateSubtitleTrack {
                    language,
                    format,
                    track_index: track.track_index,
                });
            }

            let path = input.sidecar_dir.join(sidecar_file_name(
                input.media_item_id,
                &language,
                track.track_index,
                format,
            ));
            write_sidecar(&path, &track.text).await?;
            let path_text = path_to_db_text(&path)?;
            let sidecar = SubtitleSidecar {
                id: Id::new(),
                media_item_id: input.media_item_id,
                language,
                format,
                track_index: track.track_index,
                path,
                created_at: now,
                updated_at: now,
            };

            sqlx::query(
                r#"
                INSERT INTO subtitle_sidecars (
                    id,
                    media_item_id,
                    language,
                    format,
                    track_index,
                    path,
                    created_at,
                    updated_at
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                "#,
            )
            .bind(sidecar.id)
            .bind(sidecar.media_item_id)
            .bind(&sidecar.language)
            .bind(sidecar.format.as_str())
            .bind(i64::from(sidecar.track_index))
            .bind(path_text)
            .bind(sidecar.created_at)
            .bind(sidecar.updated_at)
            .execute(&mut *tx)
            .await?;

            sidecars.push(sidecar);
        }

        tx.commit().await?;
        let languages = self.subtitle_languages(input.media_item_id).await?;

        Ok(SubtitleExtractionResult {
            sidecars,
            languages,
        })
    }

    /// Return distinct subtitle languages indexed for a media item.
    pub async fn subtitle_languages(&self, media_item_id: Id) -> Result<Vec<String>> {
        let rows = sqlx::query(
            r#"
            SELECT DISTINCT language
            FROM subtitle_sidecars
            WHERE media_item_id = ?1
            ORDER BY language
            "#,
        )
        .bind(media_item_id)
        .fetch_all(self.db.read_pool())
        .await?;

        rows.iter()
            .map(|row| row.try_get("language").map_err(Error::Sqlx))
            .collect()
    }
}

async fn create_dir_all(path: &Path) -> Result<()> {
    tokio::fs::create_dir_all(path)
        .await
        .map_err(|source| Error::Io {
            path: path.to_path_buf(),
            source,
        })
}

async fn write_sidecar(path: &Path, text: &str) -> Result<()> {
    tokio::fs::write(path, text.as_bytes())
        .await
        .map_err(|source| Error::Io {
            path: path.to_path_buf(),
            source,
        })
}

fn normalize_language(language: &str, track_index: u32) -> Result<String> {
    let language = language.trim();
    if language.is_empty() {
        return Err(Error::EmptyLanguage { track_index });
    }

    if !language
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(Error::InvalidLanguage {
            language: language.to_owned(),
        });
    }

    Ok(language.to_ascii_lowercase())
}

fn sidecar_file_name(
    media_item_id: Id,
    language: &str,
    track_index: u32,
    format: SubtitleFormat,
) -> String {
    format!(
        "{media_item_id}.{language}.{track_index}.{}",
        format.extension()
    )
}

fn path_to_db_text(path: &Path) -> Result<String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| Error::NonUtf8Path {
            path: path.to_path_buf(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn insert_personal_media_item(db: &Db) -> std::result::Result<Id, sqlx::Error> {
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

    #[tokio::test]
    async fn extracts_text_subtitles_and_indexes_languages()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = kino_db::test_db().await?;
        let media_item_id = insert_personal_media_item(&db).await?;
        let sidecar_dir = tempfile::tempdir()?;
        let service = SubtitleService::new(db);

        let result = service
            .extract_text_subtitles(SubtitleExtractionInput::new(
                media_item_id,
                sidecar_dir.path(),
                vec![
                    ProbedSubtitleTrack::new(
                        2,
                        "ENG",
                        ProbedSubtitleFormat::Srt,
                        "1\n00:00:01,000 --> 00:00:02,000\nHello\n",
                    ),
                    ProbedSubtitleTrack::new(
                        3,
                        "jpn",
                        ProbedSubtitleFormat::Ass,
                        "[Script Info]\nTitle: Kino\n",
                    ),
                ],
            ))
            .await?;

        assert_eq!(result.languages, vec!["eng", "jpn"]);
        assert_eq!(result.sidecars.len(), 2);
        assert_eq!(result.sidecars[0].language, "eng");
        assert_eq!(result.sidecars[0].format, SubtitleFormat::Srt);
        assert_eq!(result.sidecars[1].language, "jpn");
        assert_eq!(result.sidecars[1].format, SubtitleFormat::Ass);

        let srt = tokio::fs::read_to_string(&result.sidecars[0].path).await?;
        let ass = tokio::fs::read_to_string(&result.sidecars[1].path).await?;
        assert_eq!(srt, "1\n00:00:01,000 --> 00:00:02,000\nHello\n");
        assert_eq!(ass, "[Script Info]\nTitle: Kino\n");

        Ok(())
    }

    #[tokio::test]
    async fn skips_deferred_image_subtitles() -> std::result::Result<(), Box<dyn std::error::Error>>
    {
        let db = kino_db::test_db().await?;
        let media_item_id = insert_personal_media_item(&db).await?;
        let sidecar_dir = tempfile::tempdir()?;
        let service = SubtitleService::new(db);

        let result = service
            .extract_text_subtitles(SubtitleExtractionInput::new(
                media_item_id,
                sidecar_dir.path(),
                vec![
                    ProbedSubtitleTrack::new(4, "eng", ProbedSubtitleFormat::Pgs, ""),
                    ProbedSubtitleTrack::new(5, "spa", ProbedSubtitleFormat::VobSub, ""),
                ],
            ))
            .await?;

        assert!(result.sidecars.is_empty());
        assert!(result.languages.is_empty());

        Ok(())
    }

    #[tokio::test]
    async fn rejects_duplicate_text_tracks() -> std::result::Result<(), Box<dyn std::error::Error>>
    {
        let db = kino_db::test_db().await?;
        let media_item_id = insert_personal_media_item(&db).await?;
        let sidecar_dir = tempfile::tempdir()?;
        let service = SubtitleService::new(db);

        let result = service
            .extract_text_subtitles(SubtitleExtractionInput::new(
                media_item_id,
                sidecar_dir.path(),
                vec![
                    ProbedSubtitleTrack::new(2, "eng", ProbedSubtitleFormat::Srt, "one"),
                    ProbedSubtitleTrack::new(2, "ENG", ProbedSubtitleFormat::Srt, "two"),
                ],
            ))
            .await;

        let Err(err) = result else {
            panic!("duplicate track should fail");
        };

        assert!(matches!(err, Error::DuplicateSubtitleTrack { .. }));

        Ok(())
    }
}
