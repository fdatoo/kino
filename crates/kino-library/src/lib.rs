//! Library media catalog support.
//!
//! This crate owns persisted library-facing data that is attached to
//! `MediaItem` records, including subtitle sidecars extracted by ingestion.

use std::{
    collections::HashSet,
    fmt,
    path::{Path, PathBuf},
};

use kino_core::{CanonicalLayoutTransfer, Config, Id, Timestamp};
use kino_db::Db;
use sqlx::Row;

/// Errors produced by `kino-library`.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A database operation failed.
    #[error("database error: {0}")]
    Sqlx(#[from] sqlx::Error),

    /// A filesystem operation failed for a library path.
    #[error("library filesystem io failed for {path}: {source}")]
    Io {
        /// Path involved in the failed operation.
        path: PathBuf,
        /// Underlying filesystem error.
        #[source]
        source: std::io::Error,
    },

    /// A library path could not be stored or rendered as text.
    #[error("library path is not utf-8: {path}")]
    NonUtf8Path {
        /// Non-UTF-8 path.
        path: PathBuf,
    },

    /// A source file does not have an extension to preserve.
    #[error("source file has no extension: {path}")]
    MissingExtension {
        /// Source path without an extension.
        path: PathBuf,
    },

    /// A source path exists but is not a regular file.
    #[error("source path is not a file: {path}")]
    SourceNotFile {
        /// Source path that is not a regular file.
        path: PathBuf,
    },

    /// A canonical path already exists.
    #[error("canonical path already exists: {path}")]
    DestinationExists {
        /// Existing destination path.
        path: PathBuf,
    },

    /// A canonical path segment became empty after normalization.
    #[error("canonical path segment {field} is empty")]
    EmptyPathSegment {
        /// Field used to build the segment.
        field: &'static str,
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

/// Library media target for canonical layout placement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CanonicalMediaTarget {
    /// A movie identified by display title and release year.
    Movie {
        /// Movie title used in the canonical path.
        title: String,
        /// Release year used in the canonical path.
        year: u16,
    },

    /// A TV episode identified by show title, season, and episode number.
    TvEpisode {
        /// Show title used in the canonical path.
        show_title: String,
        /// Season number used in `Season XX` and `SXXEYY`.
        season_number: u32,
        /// Episode number used in `SXXEYY`.
        episode_number: u32,
    },
}

impl CanonicalMediaTarget {
    /// Construct a movie canonical target.
    pub fn movie(title: impl Into<String>, year: u16) -> Self {
        Self::Movie {
            title: title.into(),
            year,
        }
    }

    /// Construct a TV episode canonical target.
    pub fn tv_episode(
        show_title: impl Into<String>,
        season_number: u32,
        episode_number: u32,
    ) -> Self {
        Self::TvEpisode {
            show_title: show_title.into(),
            season_number,
            episode_number,
        }
    }
}

/// Input for canonical layout placement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalLayoutInput {
    /// Source file accepted by ingestion.
    pub source_path: PathBuf,
    /// Canonical media target that determines the library path.
    pub target: CanonicalMediaTarget,
}

impl CanonicalLayoutInput {
    /// Construct canonical layout input.
    pub fn new(source_path: impl Into<PathBuf>, target: CanonicalMediaTarget) -> Self {
        Self {
            source_path: source_path.into(),
            target,
        }
    }
}

/// Result of a canonical layout placement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalLayoutResult {
    /// Original source path provided to the writer.
    pub source_path: PathBuf,
    /// Canonical path that now points at the media file.
    pub canonical_path: PathBuf,
    /// Filesystem operation used for placement.
    pub transfer: CanonicalLayoutTransfer,
}

/// Writer that places accepted source files into Kino's canonical layout.
#[derive(Debug, Clone)]
pub struct CanonicalLayoutWriter {
    library_root: PathBuf,
    transfer: CanonicalLayoutTransfer,
}

impl CanonicalLayoutWriter {
    /// Construct a canonical layout writer.
    pub fn new(library_root: impl Into<PathBuf>, transfer: CanonicalLayoutTransfer) -> Self {
        Self {
            library_root: library_root.into(),
            transfer,
        }
    }

    /// Construct a canonical layout writer from process configuration.
    pub fn from_config(config: &Config) -> Self {
        Self::new(
            config.library_root.clone(),
            config.library.canonical_transfer,
        )
    }

    /// Return the canonical path for an input without touching the filesystem.
    pub fn canonical_path(&self, input: &CanonicalLayoutInput) -> Result<PathBuf> {
        let extension = source_extension(&input.source_path)?;
        match &input.target {
            CanonicalMediaTarget::Movie { title, year } => {
                let title = normalize_path_segment(title);
                let title = require_path_segment("title", title)?;
                let title_year = format!("{title} ({year})");

                Ok(self
                    .library_root
                    .join("Movies")
                    .join(&title_year)
                    .join(format!("{title_year}.{extension}")))
            }
            CanonicalMediaTarget::TvEpisode {
                show_title,
                season_number,
                episode_number,
            } => {
                let show_title = normalize_path_segment(show_title);
                let show_title = require_path_segment("show_title", show_title)?;
                let season = format!("{season_number:02}");
                let episode = format!("{episode_number:02}");

                Ok(self
                    .library_root
                    .join("TV")
                    .join(&show_title)
                    .join(format!("Season {season}"))
                    .join(format!("{show_title} - S{season}E{episode}.{extension}")))
            }
        }
    }

    /// Place a source file at its canonical path.
    pub async fn place(&self, input: CanonicalLayoutInput) -> Result<CanonicalLayoutResult> {
        let metadata = tokio::fs::metadata(&input.source_path)
            .await
            .map_err(|source| Error::Io {
                path: input.source_path.clone(),
                source,
            })?;
        if !metadata.is_file() {
            return Err(Error::SourceNotFile {
                path: input.source_path,
            });
        }

        let canonical_path = self.canonical_path(&input)?;
        let parent = canonical_path.parent().unwrap_or_else(|| Path::new(""));
        create_dir_all(parent).await?;
        if try_exists(&canonical_path).await? {
            return Err(Error::DestinationExists {
                path: canonical_path,
            });
        }

        match self.transfer {
            CanonicalLayoutTransfer::HardLink => {
                hard_link(&input.source_path, &canonical_path).await?;
            }
            CanonicalLayoutTransfer::Move => {
                rename(&input.source_path, &canonical_path).await?;
            }
        }

        Ok(CanonicalLayoutResult {
            source_path: input.source_path,
            canonical_path,
            transfer: self.transfer,
        })
    }
}

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

async fn try_exists(path: &Path) -> Result<bool> {
    tokio::fs::try_exists(path)
        .await
        .map_err(|source| Error::Io {
            path: path.to_path_buf(),
            source,
        })
}

async fn hard_link(source: &Path, destination: &Path) -> Result<()> {
    tokio::fs::hard_link(source, destination)
        .await
        .map_err(|source| Error::Io {
            path: destination.to_path_buf(),
            source,
        })
}

async fn rename(source: &Path, destination: &Path) -> Result<()> {
    tokio::fs::rename(source, destination)
        .await
        .map_err(|source| Error::Io {
            path: destination.to_path_buf(),
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

fn source_extension(path: &Path) -> Result<&str> {
    let extension = path.extension().ok_or_else(|| Error::MissingExtension {
        path: path.to_path_buf(),
    })?;

    extension.to_str().ok_or_else(|| Error::NonUtf8Path {
        path: path.to_path_buf(),
    })
}

fn normalize_path_segment(value: &str) -> String {
    let mut normalized = String::with_capacity(value.len());
    let mut previous_was_space = false;

    for ch in value.trim().chars() {
        let replacement = if ch == '/' || ch == '\\' || ch.is_control() {
            ' '
        } else {
            ch
        };

        if replacement.is_whitespace() {
            if !previous_was_space {
                normalized.push(' ');
                previous_was_space = true;
            }
        } else {
            normalized.push(replacement);
            previous_was_space = false;
        }
    }

    normalized.trim().to_owned()
}

fn require_path_segment(field: &'static str, value: String) -> Result<String> {
    if value.is_empty() {
        return Err(Error::EmptyPathSegment { field });
    }

    Ok(value)
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

    #[tokio::test]
    async fn hard_links_movie_into_canonical_layout()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let library_root = tempfile::tempdir()?;
        let source = library_root.path().join("incoming.mkv");
        tokio::fs::write(&source, b"movie bytes").await?;
        let writer =
            CanonicalLayoutWriter::new(library_root.path(), CanonicalLayoutTransfer::HardLink);

        let result = writer
            .place(CanonicalLayoutInput::new(
                &source,
                CanonicalMediaTarget::movie("The Matrix", 1999),
            ))
            .await?;
        let expected = library_root
            .path()
            .join("Movies")
            .join("The Matrix (1999)")
            .join("The Matrix (1999).mkv");

        assert_eq!(result.canonical_path, expected);
        assert_eq!(result.transfer, CanonicalLayoutTransfer::HardLink);
        assert!(source.exists());
        assert_eq!(tokio::fs::read(&source).await?, b"movie bytes");
        assert_eq!(
            tokio::fs::read(&result.canonical_path).await?,
            b"movie bytes"
        );

        Ok(())
    }

    #[tokio::test]
    async fn moves_tv_episode_into_canonical_layout()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let library_root = tempfile::tempdir()?;
        let source = library_root.path().join("episode.mkv");
        tokio::fs::write(&source, b"episode bytes").await?;
        let writer = CanonicalLayoutWriter::new(library_root.path(), CanonicalLayoutTransfer::Move);

        let result = writer
            .place(CanonicalLayoutInput::new(
                &source,
                CanonicalMediaTarget::tv_episode("Twin/Peaks", 2, 3),
            ))
            .await?;
        let expected = library_root
            .path()
            .join("TV")
            .join("Twin Peaks")
            .join("Season 02")
            .join("Twin Peaks - S02E03.mkv");

        assert_eq!(result.canonical_path, expected);
        assert_eq!(result.transfer, CanonicalLayoutTransfer::Move);
        assert!(!source.exists());
        assert_eq!(
            tokio::fs::read(&result.canonical_path).await?,
            b"episode bytes"
        );

        Ok(())
    }

    #[tokio::test]
    async fn refuses_to_overwrite_existing_canonical_path()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let library_root = tempfile::tempdir()?;
        let source = library_root.path().join("movie.mkv");
        tokio::fs::write(&source, b"new bytes").await?;
        let writer =
            CanonicalLayoutWriter::new(library_root.path(), CanonicalLayoutTransfer::HardLink);
        let input = CanonicalLayoutInput::new(&source, CanonicalMediaTarget::movie("Alien", 1979));
        let destination = writer.canonical_path(&input)?;
        let parent = match destination.parent() {
            Some(parent) => parent,
            None => panic!("destination should have a parent"),
        };
        tokio::fs::create_dir_all(parent).await?;
        tokio::fs::write(&destination, b"existing").await?;

        let result = writer.place(input).await;
        let Err(err) = result else {
            panic!("existing destination should fail");
        };

        assert!(matches!(err, Error::DestinationExists { .. }));
        assert_eq!(tokio::fs::read(&source).await?, b"new bytes");
        assert_eq!(tokio::fs::read(&destination).await?, b"existing");

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
