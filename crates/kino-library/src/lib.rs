//! Library media catalog support.
//!
//! This crate owns persisted library-facing data that is attached to
//! `MediaItem` records, including subtitle sidecars extracted by ingestion.

use std::{
    collections::{HashMap, HashSet},
    fmt,
    future::Future,
    io,
    path::{Path, PathBuf},
    pin::Pin,
    time::Duration,
};

pub mod subtitle_image_extraction;
pub mod subtitle_ocr;

use kino_core::{
    CanonicalIdentityId, CanonicalIdentityProvider, CanonicalLayoutTransfer, Config, Id,
    MediaItemKind, Timestamp, TranscodeOutput,
};
use kino_db::Db;
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::Row;

pub use subtitle_image_extraction::{
    DEFAULT_FFMPEG_PROGRAM, DEFAULT_FFPROBE_PROGRAM, FfmpegImageSubtitleExtractor,
    ImageSubtitleExtraction, ImageSubtitleExtractionFuture, ImageSubtitleExtractionInput,
    ImageSubtitleFrame, ProbeSubtitleKind, default_subtitle_staging_dir,
    image_subtitle_track_output_dir,
};

/// Errors produced by `kino-library`.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A database operation failed.
    #[error("database error: {0}")]
    Sqlx(#[from] sqlx::Error),

    /// A metadata sidecar could not be serialized.
    #[error("metadata sidecar serialization failed: {0}")]
    Json(#[from] serde_json::Error),

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

    /// A media item does not exist with the requested canonical identity.
    #[error("media item {media_item_id} does not match canonical identity {canonical_identity_id}")]
    MediaItemIdentityMismatch {
        /// Media item id.
        media_item_id: Id,
        /// Expected canonical identity id.
        canonical_identity_id: CanonicalIdentityId,
    },

    /// A metadata field is empty after trimming whitespace.
    #[error("metadata field {field} is empty")]
    EmptyMetadataField {
        /// Metadata field name.
        field: &'static str,
    },

    /// A metadata enrichment response did not include cast members.
    #[error("metadata cast is empty")]
    EmptyMetadataCast,

    /// A cached metadata cast order is outside the accepted range.
    #[error("metadata cast order {position} is invalid")]
    InvalidMetadataCastOrder {
        /// Persisted cast position.
        position: i64,
    },

    /// A metadata image asset did not contain bytes.
    #[error("metadata asset {asset} is empty")]
    EmptyMetadataAsset {
        /// Asset name.
        asset: &'static str,
    },

    /// A metadata image asset extension is not safe for sidecar filenames.
    #[error("metadata asset {asset} extension is invalid: {extension}")]
    InvalidMetadataAssetExtension {
        /// Asset name.
        asset: &'static str,
        /// Invalid extension.
        extension: String,
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

    /// A requested image subtitle stream is not present in the source file.
    #[error("subtitle track {stream_index} is missing")]
    SubtitleTrackMissing {
        /// Probed subtitle stream index.
        stream_index: u32,
    },

    /// Image subtitle extraction failed while running ffmpeg or reading its output.
    #[error("subtitle extraction failed for track {stream_index}: {source}")]
    SubtitleExtractionFailed {
        /// Probed subtitle stream index.
        stream_index: u32,
        /// Underlying process or output error.
        #[source]
        source: io::Error,
    },

    /// Image subtitle extraction was requested for a non-image subtitle format.
    #[error("subtitle extraction unsupported for format {kind}")]
    SubtitleExtractionUnsupportedFormat {
        /// Unsupported subtitle classification.
        kind: ProbeSubtitleKind,
    },

    /// A single extraction request included the same text track more than once.
    #[error("duplicate subtitle track: {language} {format} track {track_index}")]
    DuplicateSubtitleTrack {
        /// Normalized language.
        language: String,
        /// Persisted subtitle format.
        format: SubtitleFormat,
        /// Probed subtitle stream index.
        track_index: u32,
    },

    /// An OCR command could not be started or waited on.
    #[error("ocr command {binary_path} failed to run: {source}", binary_path = .binary_path.display())]
    OcrCommandIo {
        /// Tesseract binary path.
        binary_path: PathBuf,
        /// Underlying process error.
        #[source]
        source: std::io::Error,
    },

    /// An OCR command exited unsuccessfully.
    #[error("ocr command {binary_path} exited with status {status}: {stderr}", binary_path = .binary_path.display())]
    OcrCommandFailed {
        /// Tesseract binary path.
        binary_path: PathBuf,
        /// Process exit status.
        status: String,
        /// Standard error output.
        stderr: String,
    },

    /// OCR output was not valid UTF-8.
    #[error("ocr output is not utf-8: {0}")]
    OcrUtf8(#[from] std::string::FromUtf8Error),

    /// OCR TSV output was malformed.
    #[error("ocr tsv is invalid: {reason}")]
    InvalidOcrTsv {
        /// Human-readable parse failure.
        reason: String,
    },

    /// OCR TSV output contained an invalid scalar field.
    #[error("ocr tsv field {field} has invalid value {value}: {reason}")]
    InvalidOcrTsvField {
        /// TSV field name.
        field: &'static str,
        /// Invalid field value.
        value: String,
        /// Underlying parse error.
        reason: String,
    },

    /// A media item kind read from storage is not recognized.
    #[error("invalid media item kind: {value}")]
    InvalidMediaItemKind {
        /// Persisted media item kind value.
        value: String,
    },

    /// The requested media item does not exist.
    #[error("media item not found: {id}")]
    MediaItemNotFound {
        /// Missing media item id.
        id: Id,
    },

    /// A media item has no source file to attach a transcode output to.
    #[error("media item has no source file: {id}")]
    MediaItemSourceFileNotFound {
        /// Media item id without source files.
        id: Id,
    },

    /// A catalog list limit was outside the accepted range.
    #[error("invalid catalog list limit {limit}; expected 1..={max}")]
    InvalidCatalogListLimit {
        /// Requested limit.
        limit: u32,
        /// Maximum accepted limit.
        max: u32,
    },

    /// A catalog list offset was outside the accepted range.
    #[error("invalid catalog list offset {offset}; maximum is {max}")]
    InvalidCatalogListOffset {
        /// Requested offset.
        offset: u64,
        /// Maximum accepted offset.
        max: u64,
    },

    /// A positive catalog number could not fit into the public type.
    #[error("invalid catalog {field} value: {value}")]
    InvalidCatalogNumber {
        /// Field name.
        field: &'static str,
        /// Persisted value.
        value: i64,
    },

    /// A subtitle format read from storage is not recognized.
    #[error("invalid subtitle format: {value}")]
    InvalidSubtitleFormat {
        /// Persisted subtitle format value.
        value: String,
    },

    /// A subtitle provenance read from storage is not recognized.
    #[error("invalid subtitle provenance: {value}")]
    InvalidSubtitleProvenance {
        /// Persisted subtitle provenance value.
        value: String,
    },

    /// A subtitle track index could not fit into the public type.
    #[error("invalid subtitle track index: {value}")]
    InvalidSubtitleTrackIndex {
        /// Persisted track index.
        value: i64,
    },
}

/// Crate-local `Result` alias.
pub type Result<T> = std::result::Result<T, Error>;

const DEFAULT_CATALOG_LIST_LIMIT: u32 = 50;
const MAX_CATALOG_LIST_LIMIT: u32 = 200;
const MAX_CATALOG_LIST_OFFSET: u64 = i64::MAX as u64;

/// Boxed future returned by metadata provider implementations.
pub type MetadataFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;

/// Provider that fetches TMDB metadata and image bytes for enrichment.
pub trait TmdbMetadataProvider: Send + Sync {
    /// Fetch metadata for a TMDB canonical identity.
    fn fetch_metadata<'a>(
        &'a self,
        identity_id: CanonicalIdentityId,
    ) -> MetadataFuture<'a, TmdbMetadata>;
}

/// Image bytes returned by the TMDB metadata provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetadataAsset {
    /// TMDB source URL for debugging and cache provenance.
    pub source_url: Option<String>,
    /// File extension to use when storing the image sidecar.
    pub extension: String,
    /// Raw asset bytes.
    pub bytes: Vec<u8>,
}

impl MetadataAsset {
    /// Construct a metadata asset from an extension and bytes.
    pub fn new(extension: impl Into<String>, bytes: impl Into<Vec<u8>>) -> Self {
        Self {
            source_url: None,
            extension: extension.into(),
            bytes: bytes.into(),
        }
    }

    /// Attach the original provider URL for this asset.
    pub fn with_source_url(mut self, source_url: impl Into<String>) -> Self {
        self.source_url = Some(source_url.into());
        self
    }
}

/// Ordered cast member returned by the TMDB metadata provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MetadataCastMember {
    /// Cast order from TMDB.
    pub order: u32,
    /// Person display name.
    pub name: String,
    /// Character display name.
    pub character: String,
    /// Optional local or provider profile path.
    pub profile_path: Option<String>,
}

impl MetadataCastMember {
    /// Construct a metadata cast member.
    pub fn new(
        order: u32,
        name: impl Into<String>,
        character: impl Into<String>,
        profile_path: Option<String>,
    ) -> Self {
        Self {
            order,
            name: name.into(),
            character: character.into(),
            profile_path,
        }
    }
}

/// TMDB metadata payload used for write-through enrichment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TmdbMetadata {
    /// TMDB display title or series name.
    pub title: String,
    /// Overview or description text.
    pub description: String,
    /// Release or first-air date, when TMDB provided one.
    pub release_date: Option<String>,
    /// Poster image bytes.
    pub poster: MetadataAsset,
    /// Backdrop image bytes.
    pub backdrop: MetadataAsset,
    /// Logo image bytes, when available.
    pub logo: Option<MetadataAsset>,
    /// Ordered cast members.
    pub cast: Vec<MetadataCastMember>,
}

impl TmdbMetadata {
    /// Construct a TMDB metadata payload.
    pub fn new(
        title: impl Into<String>,
        description: impl Into<String>,
        release_date: Option<String>,
        poster: MetadataAsset,
        backdrop: MetadataAsset,
        logo: Option<MetadataAsset>,
        cast: Vec<MetadataCastMember>,
    ) -> Self {
        Self {
            title: title.into(),
            description: description.into(),
            release_date,
            poster,
            backdrop,
            logo,
            cast,
        }
    }
}

/// Cached metadata served by catalog reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedMediaMetadata {
    /// Media item id.
    pub media_item_id: Id,
    /// Canonical identity this metadata describes.
    pub canonical_identity_id: CanonicalIdentityId,
    /// TMDB display title or series name.
    pub title: String,
    /// Overview or description text.
    pub description: String,
    /// Release or first-air date, when known.
    pub release_date: Option<String>,
    /// Provider poster image URL, when known.
    pub poster_source_url: Option<String>,
    /// Local poster cache path.
    pub poster_path: PathBuf,
    /// Relative poster path inside the artwork cache.
    pub poster_local_path: PathBuf,
    /// Provider backdrop image URL, when known.
    pub backdrop_source_url: Option<String>,
    /// Local backdrop cache path.
    pub backdrop_path: PathBuf,
    /// Relative backdrop path inside the artwork cache.
    pub backdrop_local_path: PathBuf,
    /// Provider logo image URL, when known.
    pub logo_source_url: Option<String>,
    /// Local logo cache path, when present.
    pub logo_path: Option<PathBuf>,
    /// Relative logo path inside the artwork cache, when present.
    pub logo_local_path: Option<PathBuf>,
    /// Local JSON metadata sidecar path.
    pub metadata_path: PathBuf,
    /// Ordered cast members.
    pub cast: Vec<MetadataCastMember>,
    /// Cache creation timestamp.
    pub created_at: Timestamp,
    /// Cache update timestamp.
    pub updated_at: Timestamp,
}

/// Library service for write-through TMDB metadata enrichment.
#[derive(Clone)]
pub struct MetadataService {
    db: Db,
    metadata_root: PathBuf,
    artwork_cache_root: PathBuf,
}

impl MetadataService {
    /// Construct a metadata service with an explicit metadata root.
    pub fn new(db: Db, metadata_root: impl Into<PathBuf>) -> Self {
        let metadata_root = metadata_root.into();
        Self {
            db,
            artwork_cache_root: metadata_root.clone(),
            metadata_root,
        }
    }

    /// Construct a metadata service with explicit metadata and artwork roots.
    pub fn with_artwork_cache_dir(
        db: Db,
        metadata_root: impl Into<PathBuf>,
        artwork_cache_root: impl Into<PathBuf>,
    ) -> Self {
        Self {
            db,
            metadata_root: metadata_root.into(),
            artwork_cache_root: artwork_cache_root.into(),
        }
    }

    /// Construct a metadata service from process configuration.
    pub fn from_config(db: Db, config: &Config) -> Self {
        Self::with_artwork_cache_dir(
            db,
            config.library_root.join("Metadata"),
            config.artwork_cache_dir(),
        )
    }

    /// Enrich a media item from TMDB, returning the local cache projection.
    pub async fn enrich_tmdb_media_item<P>(
        &self,
        media_item_id: Id,
        canonical_identity_id: CanonicalIdentityId,
        provider: &P,
    ) -> Result<CachedMediaMetadata>
    where
        P: TmdbMetadataProvider,
    {
        if let Some(cached) = self.cached_metadata(media_item_id).await? {
            if cached.canonical_identity_id != canonical_identity_id {
                return Err(Error::MediaItemIdentityMismatch {
                    media_item_id,
                    canonical_identity_id,
                });
            }

            return Ok(cached);
        }

        self.ensure_media_item_identity(media_item_id, canonical_identity_id)
            .await?;

        let metadata = provider.fetch_metadata(canonical_identity_id).await?;
        let metadata = validate_tmdb_metadata(metadata)?;
        let directory = self.metadata_directory(canonical_identity_id);
        create_dir_all(&directory).await?;

        let poster_local_path =
            write_artwork_asset(&self.artwork_cache_root, "poster", &metadata.poster).await?;
        let poster_path = self.artwork_cache_root.join(&poster_local_path);
        let backdrop_local_path =
            write_artwork_asset(&self.artwork_cache_root, "backdrop", &metadata.backdrop).await?;
        let backdrop_path = self.artwork_cache_root.join(&backdrop_local_path);
        let (logo_local_path, logo_path) = match &metadata.logo {
            Some(asset) => {
                let local_path =
                    write_artwork_asset(&self.artwork_cache_root, "logo", asset).await?;
                let path = self.artwork_cache_root.join(&local_path);
                (Some(local_path), Some(path))
            }
            None => (None, None),
        };
        let metadata_path = directory.join("metadata.json");
        write_metadata_sidecar(
            &metadata_path,
            canonical_identity_id,
            &metadata,
            &poster_path,
            &backdrop_path,
            logo_path.as_deref(),
        )
        .await?;

        let now = Timestamp::now();
        let cached = CachedMediaMetadata {
            media_item_id,
            canonical_identity_id,
            title: metadata.title,
            description: metadata.description,
            release_date: metadata.release_date,
            poster_source_url: metadata.poster.source_url,
            poster_path,
            poster_local_path,
            backdrop_source_url: metadata.backdrop.source_url,
            backdrop_path,
            backdrop_local_path,
            logo_source_url: metadata.logo.and_then(|asset| asset.source_url),
            logo_path,
            logo_local_path,
            metadata_path,
            cast: metadata.cast,
            created_at: now,
            updated_at: now,
        };
        self.insert_metadata_cache(&cached).await?;

        Ok(cached)
    }

    /// Return cached media metadata without calling TMDB.
    pub async fn cached_metadata(&self, media_item_id: Id) -> Result<Option<CachedMediaMetadata>> {
        let row = sqlx::query(
            r#"
            SELECT
                media_item_id,
                canonical_identity_id,
                title,
                description,
                release_date,
                poster_path,
                poster_local_path,
                backdrop_path,
                backdrop_local_path,
                logo_path,
                logo_local_path,
                metadata_path,
                created_at,
                updated_at
            FROM media_metadata_cache
            WHERE media_item_id = ?1
            "#,
        )
        .bind(media_item_id)
        .fetch_optional(self.db.read_pool())
        .await?;

        let Some(row) = row else {
            return Ok(None);
        };

        let cast_rows = sqlx::query(
            r#"
            SELECT position, name, character, profile_path
            FROM media_metadata_cast_members
            WHERE media_item_id = ?1
            ORDER BY position
            "#,
        )
        .bind(media_item_id)
        .fetch_all(self.db.read_pool())
        .await?;
        let cast = cast_rows
            .iter()
            .map(cast_member_from_row)
            .collect::<Result<Vec<_>>>()?;

        Ok(Some(metadata_from_row(
            &row,
            cast,
            &self.artwork_cache_root,
        )?))
    }

    async fn ensure_media_item_identity(
        &self,
        media_item_id: Id,
        canonical_identity_id: CanonicalIdentityId,
    ) -> Result<()> {
        let exists = sqlx::query_scalar::<_, bool>(
            r#"
            SELECT EXISTS(
                SELECT 1
                FROM media_items
                WHERE id = ?1 AND canonical_identity_id = ?2
            )
            "#,
        )
        .bind(media_item_id)
        .bind(canonical_identity_id)
        .fetch_one(self.db.read_pool())
        .await?;

        if !exists {
            return Err(Error::MediaItemIdentityMismatch {
                media_item_id,
                canonical_identity_id,
            });
        }

        Ok(())
    }

    async fn insert_metadata_cache(&self, cached: &CachedMediaMetadata) -> Result<()> {
        let mut tx = self.db.write_pool().begin().await?;
        sqlx::query(
            r#"
            INSERT INTO media_metadata_cache (
                media_item_id,
                canonical_identity_id,
                provider,
                title,
                description,
                release_date,
                poster_path,
                poster_local_path,
                backdrop_path,
                backdrop_local_path,
                logo_path,
                logo_local_path,
                metadata_path,
                created_at,
                updated_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
            "#,
        )
        .bind(cached.media_item_id)
        .bind(cached.canonical_identity_id)
        .bind(CanonicalIdentityProvider::Tmdb.as_str())
        .bind(&cached.title)
        .bind(&cached.description)
        .bind(&cached.release_date)
        .bind(cached.poster_source_url.as_deref().unwrap_or_default())
        .bind(path_to_db_text(&cached.poster_local_path)?)
        .bind(cached.backdrop_source_url.as_deref().unwrap_or_default())
        .bind(path_to_db_text(&cached.backdrop_local_path)?)
        .bind(cached.logo_source_url.as_deref().unwrap_or_default())
        .bind(path_option_to_db_text(cached.logo_local_path.as_deref())?)
        .bind(path_to_db_text(&cached.metadata_path)?)
        .bind(cached.created_at)
        .bind(cached.updated_at)
        .execute(&mut *tx)
        .await?;

        for cast in &cached.cast {
            sqlx::query(
                r#"
                INSERT INTO media_metadata_cast_members (
                    media_item_id,
                    position,
                    name,
                    character,
                    profile_path
                )
                VALUES (?1, ?2, ?3, ?4, ?5)
                "#,
            )
            .bind(cached.media_item_id)
            .bind(i64::from(cast.order))
            .bind(&cast.name)
            .bind(&cast.character)
            .bind(&cast.profile_path)
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;
        Ok(())
    }

    fn metadata_directory(&self, identity_id: CanonicalIdentityId) -> PathBuf {
        self.metadata_root
            .join(identity_id.provider().as_str())
            .join(identity_id.kind().as_str())
            .join(identity_id.tmdb_id().to_string())
    }
}

/// Query filters and pagination for catalog item listing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CatalogListQuery {
    /// Optional media kind filter.
    pub media_kind: Option<MediaItemKind>,
    /// Optional full-text search across metadata title and cast names.
    pub search: Option<String>,
    /// Optional case-insensitive metadata title substring filter.
    pub title_contains: Option<String>,
    /// Optional filter for items with or without at least one source file.
    pub has_source_file: Option<bool>,
    /// Maximum number of items to return.
    pub limit: Option<u32>,
    /// Number of matching items to skip.
    pub offset: Option<u64>,
}

impl CatalogListQuery {
    /// Construct an empty catalog list query.
    pub fn new() -> Self {
        Self::default()
    }

    /// Filter by media item kind.
    pub const fn with_media_kind(mut self, media_kind: MediaItemKind) -> Self {
        self.media_kind = Some(media_kind);
        self
    }

    /// Search cached metadata title and cast names.
    pub fn with_search(mut self, search: impl Into<String>) -> Self {
        self.search = Some(search.into());
        self
    }

    /// Filter by cached metadata title substring.
    pub fn with_title_contains(mut self, title_contains: impl Into<String>) -> Self {
        self.title_contains = Some(title_contains.into());
        self
    }

    /// Filter by source-file presence.
    pub const fn with_has_source_file(mut self, has_source_file: bool) -> Self {
        self.has_source_file = Some(has_source_file);
        self
    }

    /// Set the page size.
    pub const fn with_limit(mut self, limit: u32) -> Self {
        self.limit = Some(limit);
        self
    }

    /// Set the page offset.
    pub const fn with_offset(mut self, offset: u64) -> Self {
        self.offset = Some(offset);
        self
    }
}

/// One page of catalog media items.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct CatalogListPage {
    /// Media items in this page.
    pub items: Vec<CatalogMediaItem>,
    /// Offset to request the next page, when more matches exist.
    pub next_offset: Option<u64>,
}

/// Catalog projection for one media item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct CatalogMediaItem {
    /// Media item id.
    pub id: Id,
    /// Media item kind.
    pub media_kind: MediaItemKind,
    /// Canonical identity for provider-backed media.
    pub canonical_identity_id: Option<CanonicalIdentityId>,
    /// TV season number for episode rows.
    pub season_number: Option<u32>,
    /// TV episode number for episode rows.
    pub episode_number: Option<u32>,
    /// Cached display title, when metadata has been enriched.
    pub title: Option<String>,
    /// Cached artwork URLs for this item.
    pub artwork: CatalogArtwork,
    /// Source files attached to this media item.
    pub source_files: Vec<LibrarySourceFile>,
    /// Subtitle tracks attached to this media item.
    pub subtitle_tracks: Vec<CatalogSubtitleTrack>,
    /// Creation timestamp.
    pub created_at: Timestamp,
    /// Last update timestamp.
    pub updated_at: Timestamp,
}

/// Catalog projection for one subtitle track.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct CatalogSubtitleTrack {
    /// Subtitle sidecar id.
    pub id: Id,
    /// Normalized subtitle language.
    pub language: String,
    /// Display label for client subtitle menus.
    pub label: String,
    /// Persisted subtitle format.
    pub format: SubtitleFormat,
    /// How the subtitle text was derived.
    pub provenance: SubtitleProvenance,
    /// Probed subtitle stream index in the source file.
    pub track_index: u32,
}

/// Catalog artwork URLs for one media item.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct CatalogArtwork {
    /// Poster artwork, when cached.
    pub poster: Option<CatalogArtworkImage>,
    /// Backdrop artwork, when cached.
    pub backdrop: Option<CatalogArtworkImage>,
    /// Logo artwork, when cached.
    pub logo: Option<CatalogArtworkImage>,
}

/// Catalog artwork image URLs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct CatalogArtworkImage {
    /// Provider URL used as the original image source.
    pub source_url: Option<String>,
    /// Kino-owned URL clients should prefer.
    pub internal_url: Option<String>,
}

/// Cached artwork image kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogArtworkKind {
    /// Poster artwork.
    Poster,
    /// Backdrop artwork.
    Backdrop,
    /// Logo artwork.
    Logo,
}

impl CatalogArtworkKind {
    /// Parse a route image kind.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "poster" => Some(Self::Poster),
            "backdrop" => Some(Self::Backdrop),
            "logo" => Some(Self::Logo),
            _ => None,
        }
    }

    /// Return the route path segment for this image kind.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Poster => "poster",
            Self::Backdrop => "backdrop",
            Self::Logo => "logo",
        }
    }
}

/// Capability hints describing a registered transcode output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscodeCapabilities {
    /// Video codec used by the output.
    pub codec: String,
    /// Container format used by the output.
    pub container: String,
    /// Resolution label for the output, when known.
    pub resolution: Option<String>,
    /// HDR format label for the output, when known.
    pub hdr: Option<String>,
}

/// Library catalog read and transcode-output registration service.
#[derive(Clone)]
pub struct CatalogService {
    db: Db,
}

impl CatalogService {
    /// Construct a catalog service backed by Kino's database.
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// Persist a transcode output for an existing media item's source file.
    pub async fn register_transcode_output(
        &self,
        media_item_id: Id,
        _capabilities: TranscodeCapabilities,
        file_path: impl Into<PathBuf>,
    ) -> Result<TranscodeOutput> {
        let source_file_id = self
            .source_file_id_for_transcode_output(media_item_id)
            .await?;
        let now = Timestamp::now();
        let output = TranscodeOutput::new(Id::new(), source_file_id, file_path, now);
        let path_text = path_to_db_text(&output.path)?;

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
        .bind(output.id)
        .bind(output.source_file_id)
        .bind(path_text)
        .bind(output.created_at)
        .bind(output.updated_at)
        .execute(self.db.write_pool())
        .await?;

        Ok(output)
    }

    /// List media items using filters and offset pagination.
    pub async fn list(&self, query: CatalogListQuery) -> Result<CatalogListPage> {
        let limit = validated_catalog_limit(query.limit)?;
        let offset = validated_catalog_offset(query.offset)?;
        let fetch_limit = limit + 1;
        let rows = self.fetch_media_items(&query, fetch_limit, offset).await?;
        let mut items = rows
            .iter()
            .map(catalog_media_item_from_row)
            .collect::<Result<Vec<_>>>()?;
        let has_next = items.len() > limit as usize;
        if has_next {
            items.truncate(limit as usize);
        }
        self.attach_source_files(&mut items).await?;
        self.attach_subtitle_tracks(&mut items).await?;

        let next_offset = if has_next {
            Some(offset + u64::from(limit))
        } else {
            None
        };

        Ok(CatalogListPage { items, next_offset })
    }

    /// Get one media item by id.
    pub async fn get(&self, id: Id) -> Result<CatalogMediaItem> {
        let row = sqlx::query(
            r#"
            SELECT
                media_items.id,
                media_items.media_kind,
                media_items.canonical_identity_id,
                media_items.season_number,
                media_items.episode_number,
                media_items.created_at,
                media_items.updated_at,
                media_metadata_cache.title,
                media_metadata_cache.poster_path AS poster_source_url,
                media_metadata_cache.poster_local_path,
                media_metadata_cache.backdrop_path AS backdrop_source_url,
                media_metadata_cache.backdrop_local_path,
                media_metadata_cache.logo_path AS logo_source_url,
                media_metadata_cache.logo_local_path
            FROM media_items
            LEFT JOIN media_metadata_cache
                ON media_metadata_cache.media_item_id = media_items.id
            WHERE media_items.id = ?1
            "#,
        )
        .bind(id)
        .fetch_optional(self.db.read_pool())
        .await?;

        let Some(row) = row else {
            return Err(Error::MediaItemNotFound { id });
        };

        let mut item = catalog_media_item_from_row(&row)?;
        self.attach_source_files(std::slice::from_mut(&mut item))
            .await?;
        self.attach_subtitle_tracks(std::slice::from_mut(&mut item))
            .await?;
        Ok(item)
    }

    /// Return the cached relative artwork path for a media item and kind.
    pub async fn artwork_local_path(
        &self,
        media_item_id: Id,
        kind: CatalogArtworkKind,
    ) -> Result<Option<PathBuf>> {
        let row = sqlx::query(
            r#"
            SELECT poster_local_path, backdrop_local_path, logo_local_path
            FROM media_metadata_cache
            WHERE media_item_id = ?1
            "#,
        )
        .bind(media_item_id)
        .fetch_optional(self.db.read_pool())
        .await?;

        let Some(row) = row else {
            return Ok(None);
        };

        let value: Option<String> = match kind {
            CatalogArtworkKind::Poster => row.try_get("poster_local_path")?,
            CatalogArtworkKind::Backdrop => row.try_get("backdrop_local_path")?,
            CatalogArtworkKind::Logo => row.try_get("logo_local_path")?,
        };

        Ok(value.filter(|path| !path.is_empty()).map(PathBuf::from))
    }

    async fn fetch_media_items(
        &self,
        query: &CatalogListQuery,
        limit: u32,
        offset: u64,
    ) -> Result<Vec<sqlx::sqlite::SqliteRow>> {
        let search = query.search.as_deref().and_then(fts_prefix_query);
        let has_search = search.is_some();
        let mut builder = sqlx::QueryBuilder::new(
            r#"
            SELECT
                media_items.id,
                media_items.media_kind,
                media_items.canonical_identity_id,
                media_items.season_number,
                media_items.episode_number,
                media_items.created_at,
                media_items.updated_at,
                media_metadata_cache.title,
                media_metadata_cache.poster_path AS poster_source_url,
                media_metadata_cache.poster_local_path,
                media_metadata_cache.backdrop_path AS backdrop_source_url,
                media_metadata_cache.backdrop_local_path,
                media_metadata_cache.logo_path AS logo_source_url,
                media_metadata_cache.logo_local_path
            FROM media_items
            "#,
        );

        if has_search {
            builder.push(
                r#"
                JOIN media_items_fts
                    ON media_items_fts.rowid = media_items.rowid
                "#,
            );
        }

        builder.push(
            r#"
            LEFT JOIN media_metadata_cache
                ON media_metadata_cache.media_item_id = media_items.id
            WHERE 1 = 1
            "#,
        );

        if let Some(media_kind) = query.media_kind {
            builder.push(" AND media_items.media_kind = ");
            builder.push_bind(media_kind.as_str());
        }

        if let Some(search) = search {
            builder.push(" AND media_items_fts MATCH ");
            builder.push_bind(search);
        }

        if let Some(title_contains) = query
            .title_contains
            .as_deref()
            .map(str::trim)
            .filter(|title| !title.is_empty())
        {
            builder.push(r#" AND media_metadata_cache.title LIKE "#);
            builder.push_bind(like_contains_pattern(title_contains));
            builder.push(r#" ESCAPE '\' "#);
        }

        if let Some(has_source_file) = query.has_source_file {
            if has_source_file {
                builder.push(
                    r#"
                    AND EXISTS (
                        SELECT 1
                        FROM source_files
                        WHERE source_files.media_item_id = media_items.id
                    )
                    "#,
                );
            } else {
                builder.push(
                    r#"
                    AND NOT EXISTS (
                        SELECT 1
                        FROM source_files
                        WHERE source_files.media_item_id = media_items.id
                    )
                    "#,
                );
            }
        }

        if has_search {
            builder.push(" ORDER BY bm25(media_items_fts, 8.0, 1.0), media_items.created_at, media_items.id LIMIT ");
        } else {
            builder.push(" ORDER BY media_items.created_at, media_items.id LIMIT ");
        }
        builder.push_bind(i64::from(limit));
        builder.push(" OFFSET ");
        builder.push_bind(
            i64::try_from(offset).map_err(|_| Error::InvalidCatalogListOffset {
                offset,
                max: MAX_CATALOG_LIST_OFFSET,
            })?,
        );

        Ok(builder.build().fetch_all(self.db.read_pool()).await?)
    }

    async fn attach_source_files(&self, items: &mut [CatalogMediaItem]) -> Result<()> {
        if items.is_empty() {
            return Ok(());
        }

        let mut builder = sqlx::QueryBuilder::new(
            r#"
            SELECT id, media_item_id, path
            FROM source_files
            WHERE media_item_id IN (
            "#,
        );
        let mut separated = builder.separated(", ");
        for item in items.iter() {
            separated.push_bind(item.id);
        }
        separated.push_unseparated(") ORDER BY media_item_id, path, id");

        let rows = builder.build().fetch_all(self.db.read_pool()).await?;
        let mut source_files = rows
            .iter()
            .map(library_source_file_from_row)
            .collect::<Result<Vec<_>>>()?;
        self.attach_transcode_outputs(&mut source_files).await?;

        let mut by_media_item = HashMap::<Id, Vec<LibrarySourceFile>>::new();
        for source_file in source_files {
            by_media_item
                .entry(source_file.media_item_id)
                .or_default()
                .push(source_file);
        }

        for item in items {
            item.source_files = by_media_item.remove(&item.id).unwrap_or_default();
        }

        Ok(())
    }

    async fn attach_subtitle_tracks(&self, items: &mut [CatalogMediaItem]) -> Result<()> {
        if items.is_empty() {
            return Ok(());
        }

        let mut builder = sqlx::QueryBuilder::new(
            r#"
            SELECT
                id,
                media_item_id,
                language,
                format,
                provenance,
                track_index
            FROM subtitle_sidecars
            WHERE media_item_id IN (
            "#,
        );
        let mut separated = builder.separated(", ");
        for item in items.iter() {
            separated.push_bind(item.id);
        }
        separated
            .push_unseparated(") ORDER BY media_item_id, language, track_index, provenance, id");

        let rows = builder.build().fetch_all(self.db.read_pool()).await?;
        let mut by_media_item = HashMap::<Id, Vec<CatalogSubtitleTrack>>::new();
        for row in &rows {
            let media_item_id = row.try_get("media_item_id")?;
            by_media_item
                .entry(media_item_id)
                .or_default()
                .push(catalog_subtitle_track_from_row(row)?);
        }

        for item in items {
            item.subtitle_tracks = by_media_item.remove(&item.id).unwrap_or_default();
        }

        Ok(())
    }

    async fn source_file_id_for_transcode_output(&self, media_item_id: Id) -> Result<Id> {
        let row = sqlx::query(
            r#"
            SELECT source_files.id AS source_file_id
            FROM media_items
            LEFT JOIN source_files
                ON source_files.media_item_id = media_items.id
            WHERE media_items.id = ?1
            ORDER BY source_files.created_at, source_files.id
            LIMIT 1
            "#,
        )
        .bind(media_item_id)
        .fetch_optional(self.db.read_pool())
        .await?;

        let Some(row) = row else {
            return Err(Error::MediaItemNotFound { id: media_item_id });
        };

        let source_file_id: Option<Id> = row.try_get("source_file_id")?;
        source_file_id.ok_or(Error::MediaItemSourceFileNotFound { id: media_item_id })
    }

    async fn attach_transcode_outputs(&self, source_files: &mut [LibrarySourceFile]) -> Result<()> {
        if source_files.is_empty() {
            return Ok(());
        }

        let mut builder = sqlx::QueryBuilder::new(
            r#"
            SELECT id, source_file_id, path, created_at, updated_at
            FROM transcode_outputs
            WHERE source_file_id IN (
            "#,
        );
        let mut separated = builder.separated(", ");
        for source_file in source_files.iter() {
            separated.push_bind(source_file.id);
        }
        separated.push_unseparated(") ORDER BY source_file_id, path, id");

        let rows = builder.build().fetch_all(self.db.read_pool()).await?;
        let mut by_source_file = HashMap::<Id, Vec<TranscodeOutput>>::new();
        for row in &rows {
            let output = transcode_output_from_row(row)?;
            by_source_file
                .entry(output.source_file_id)
                .or_default()
                .push(output);
        }

        for source_file in source_files {
            source_file.transcode_outputs =
                by_source_file.remove(&source_file.id).unwrap_or_default();
        }

        Ok(())
    }
}

const MOVIES_DIRECTORY: &str = "Movies";
const TV_DIRECTORY: &str = "TV";
const METADATA_DIRECTORY: &str = "Metadata";
const SEASON_PREFIX: &str = "Season ";

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

/// Shared storage layout rules for Kino-owned library directories.
#[derive(Debug, Clone)]
pub struct StorageLayoutPolicy {
    library_root: PathBuf,
}

impl StorageLayoutPolicy {
    /// Construct storage layout policy for a library root.
    pub fn new(library_root: impl Into<PathBuf>) -> Self {
        Self {
            library_root: library_root.into(),
        }
    }

    /// Construct storage layout policy from process configuration.
    pub fn from_config(config: &Config) -> Self {
        Self::new(config.library_root.clone())
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
                    .join(MOVIES_DIRECTORY)
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
                    .join(TV_DIRECTORY)
                    .join(&show_title)
                    .join(format!("{SEASON_PREFIX}{season}"))
                    .join(format!("{show_title} - S{season}E{episode}.{extension}")))
            }
        }
    }

    /// Return the root path governed by this policy.
    pub fn library_root(&self) -> &Path {
        &self.library_root
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
    policy: StorageLayoutPolicy,
    transfer: CanonicalLayoutTransfer,
}

impl CanonicalLayoutWriter {
    /// Construct a canonical layout writer.
    pub fn new(library_root: impl Into<PathBuf>, transfer: CanonicalLayoutTransfer) -> Self {
        Self {
            policy: StorageLayoutPolicy::new(library_root),
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
        self.policy.canonical_path(input)
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

/// Scanner that accepts only Kino's canonical library layout.
#[derive(Debug, Clone)]
pub struct StorageLayoutScanner {
    policy: StorageLayoutPolicy,
}

impl StorageLayoutScanner {
    /// Construct a scanner for a library root.
    pub fn new(library_root: impl Into<PathBuf>) -> Self {
        Self {
            policy: StorageLayoutPolicy::new(library_root),
        }
    }

    /// Construct a scanner from process configuration.
    pub fn from_config(config: &Config) -> Self {
        Self {
            policy: StorageLayoutPolicy::from_config(config),
        }
    }

    /// Scan the library root for canonical media files and layout violations.
    pub async fn scan(&self) -> Result<StorageLayoutScan> {
        let mut scan = StorageLayoutScan::default();
        let mut entries = read_dir(self.policy.library_root()).await?;

        while let Some(entry) = entries.next_entry().await.map_err(|source| Error::Io {
            path: self.policy.library_root().to_path_buf(),
            source,
        })? {
            let path = entry.path();
            let name = path_file_name(&path)?;
            match name {
                MOVIES_DIRECTORY => self.scan_movies_root(&path, &mut scan).await?,
                TV_DIRECTORY => self.scan_tv_root(&path, &mut scan).await?,
                METADATA_DIRECTORY => {}
                _ => scan.violate(path, StorageLayoutViolationKind::UnknownTopLevelEntry),
            }
        }

        Ok(scan)
    }

    async fn scan_movies_root(&self, path: &Path, scan: &mut StorageLayoutScan) -> Result<()> {
        let mut entries = read_dir(path).await?;
        while let Some(entry) = entries.next_entry().await.map_err(|source| Error::Io {
            path: path.to_path_buf(),
            source,
        })? {
            let path = entry.path();
            let file_type = entry.file_type().await.map_err(|source| Error::Io {
                path: path.clone(),
                source,
            })?;
            if !file_type.is_dir() {
                scan.violate(path, StorageLayoutViolationKind::NonCanonicalMoviePath);
                continue;
            }

            self.scan_movie_directory(&path, scan).await?;
        }

        Ok(())
    }

    async fn scan_movie_directory(
        &self,
        directory: &Path,
        scan: &mut StorageLayoutScan,
    ) -> Result<()> {
        let movie_name = path_file_name(directory)?.to_owned();
        let mut entries = read_dir(directory).await?;
        while let Some(entry) = entries.next_entry().await.map_err(|source| Error::Io {
            path: directory.to_path_buf(),
            source,
        })? {
            let path = entry.path();
            let file_type = entry.file_type().await.map_err(|source| Error::Io {
                path: path.clone(),
                source,
            })?;
            if file_type.is_file() && movie_file_matches(&path, &movie_name)? {
                scan.media_files.push(CanonicalLayoutFile {
                    path,
                    kind: CanonicalLayoutFileKind::Movie,
                });
            } else {
                scan.violate(path, StorageLayoutViolationKind::NonCanonicalMoviePath);
            }
        }

        Ok(())
    }

    async fn scan_tv_root(&self, path: &Path, scan: &mut StorageLayoutScan) -> Result<()> {
        let mut entries = read_dir(path).await?;
        while let Some(entry) = entries.next_entry().await.map_err(|source| Error::Io {
            path: path.to_path_buf(),
            source,
        })? {
            let path = entry.path();
            let file_type = entry.file_type().await.map_err(|source| Error::Io {
                path: path.clone(),
                source,
            })?;
            if !file_type.is_dir() {
                scan.violate(path, StorageLayoutViolationKind::NonCanonicalTvPath);
                continue;
            }

            self.scan_show_directory(&path, scan).await?;
        }

        Ok(())
    }

    async fn scan_show_directory(
        &self,
        directory: &Path,
        scan: &mut StorageLayoutScan,
    ) -> Result<()> {
        let show_name = path_file_name(directory)?.to_owned();
        let mut entries = read_dir(directory).await?;
        while let Some(entry) = entries.next_entry().await.map_err(|source| Error::Io {
            path: directory.to_path_buf(),
            source,
        })? {
            let path = entry.path();
            let file_type = entry.file_type().await.map_err(|source| Error::Io {
                path: path.clone(),
                source,
            })?;
            if !file_type.is_dir() {
                scan.violate(path, StorageLayoutViolationKind::NonCanonicalTvPath);
                continue;
            }

            self.scan_season_directory(&show_name, &path, scan).await?;
        }

        Ok(())
    }

    async fn scan_season_directory(
        &self,
        show_name: &str,
        directory: &Path,
        scan: &mut StorageLayoutScan,
    ) -> Result<()> {
        let season_name = path_file_name(directory)?;
        let Some(season) = season_name.strip_prefix(SEASON_PREFIX) else {
            scan.violate(
                directory.to_path_buf(),
                StorageLayoutViolationKind::NonCanonicalTvPath,
            );
            return Ok(());
        };
        if season.is_empty() || !season.bytes().all(|b| b.is_ascii_digit()) {
            scan.violate(
                directory.to_path_buf(),
                StorageLayoutViolationKind::NonCanonicalTvPath,
            );
            return Ok(());
        }

        let mut entries = read_dir(directory).await?;
        while let Some(entry) = entries.next_entry().await.map_err(|source| Error::Io {
            path: directory.to_path_buf(),
            source,
        })? {
            let path = entry.path();
            let file_type = entry.file_type().await.map_err(|source| Error::Io {
                path: path.clone(),
                source,
            })?;
            if file_type.is_file() && tv_episode_file_matches(&path, show_name, season)? {
                scan.media_files.push(CanonicalLayoutFile {
                    path,
                    kind: CanonicalLayoutFileKind::TvEpisode,
                });
            } else {
                scan.violate(path, StorageLayoutViolationKind::NonCanonicalTvPath);
            }
        }

        Ok(())
    }
}

/// Result of scanning a Kino-owned library directory.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct StorageLayoutScan {
    /// Canonical media files found under the owned layout.
    pub media_files: Vec<CanonicalLayoutFile>,
    /// Filesystem entries that do not follow Kino's storage policy.
    pub violations: Vec<StorageLayoutViolation>,
}

impl StorageLayoutScan {
    fn violate(&mut self, path: PathBuf, kind: StorageLayoutViolationKind) {
        self.violations.push(StorageLayoutViolation { path, kind });
    }
}

/// A media file that follows Kino's canonical library layout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CanonicalLayoutFile {
    /// Filesystem path to the canonical media file.
    pub path: PathBuf,
    /// Media path family matched by the scanner.
    pub kind: CanonicalLayoutFileKind,
}

/// Media path family matched by the scanner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CanonicalLayoutFileKind {
    /// Movie path under `Movies/Title (Year)/Title (Year).ext`.
    Movie,
    /// TV episode path like `TV/Show/Season 02/Show - S02E03.ext`.
    TvEpisode,
}

/// A filesystem entry that does not follow Kino's storage policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StorageLayoutViolation {
    /// Filesystem path that violated the policy.
    pub path: PathBuf,
    /// Stable reason for the violation.
    pub kind: StorageLayoutViolationKind,
}

/// Stable reason a filesystem entry violated the storage policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageLayoutViolationKind {
    /// Entry is not one of Kino's owned top-level directories.
    UnknownTopLevelEntry,
    /// Entry under `Movies` does not match the movie layout.
    NonCanonicalMoviePath,
    /// Entry under `TV` does not match the TV layout.
    NonCanonicalTvPath,
}

/// Service that scans Kino's library layout and reconciles it with source-file rows.
#[derive(Clone)]
pub struct LibraryScanService {
    db: Db,
    scanner: StorageLayoutScanner,
}

impl LibraryScanService {
    /// Construct a scan service for an explicit library root.
    pub fn new(db: Db, library_root: impl Into<PathBuf>) -> Self {
        Self {
            db,
            scanner: StorageLayoutScanner::new(library_root),
        }
    }

    /// Construct a scan service from process configuration.
    pub fn from_config(db: Db, config: &Config) -> Self {
        Self {
            db,
            scanner: StorageLayoutScanner::from_config(config),
        }
    }

    /// Scan the canonical library directory and report DB/disk drift.
    pub async fn scan(&self) -> Result<LibraryScanReport> {
        let layout = self.scanner.scan().await?;
        let source_files = self.source_files().await?;
        let db_paths = source_files
            .iter()
            .map(|source_file| source_file.path.clone())
            .collect::<HashSet<_>>();
        let mut canonical_files = layout.media_files;
        canonical_files.sort_by(|left, right| left.path.cmp(&right.path));

        let mut orphans = canonical_files
            .iter()
            .filter(|file| !db_paths.contains(&file.path))
            .map(|file| LibraryScanOrphan {
                path: file.path.clone(),
                kind: file.kind,
            })
            .collect::<Vec<_>>();

        let mut missing = Vec::new();
        for source_file in source_files {
            if !try_exists(&source_file.path).await? {
                missing.push(LibraryScanMissingFile { source_file });
            }
        }

        let mut layout_violations = layout.violations;
        orphans.sort_by(|left, right| left.path.cmp(&right.path));
        missing.sort_by(|left, right| left.source_file.path.cmp(&right.source_file.path));
        layout_violations.sort_by(|left, right| left.path.cmp(&right.path));

        Ok(LibraryScanReport {
            scanned_at: Timestamp::now(),
            canonical_files,
            orphans,
            missing,
            layout_violations,
        })
    }

    async fn source_files(&self) -> Result<Vec<LibrarySourceFile>> {
        let rows = sqlx::query(
            r#"
            SELECT id, media_item_id, path
            FROM source_files
            ORDER BY path, id
            "#,
        )
        .fetch_all(self.db.read_pool())
        .await?;

        rows.iter().map(library_source_file_from_row).collect()
    }
}

/// Complete output from a library scan and DB reconciliation run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LibraryScanReport {
    /// Time the report was produced.
    pub scanned_at: Timestamp,
    /// Canonical media files found on disk.
    pub canonical_files: Vec<CanonicalLayoutFile>,
    /// Canonical media files with no matching `source_files.path` row.
    pub orphans: Vec<LibraryScanOrphan>,
    /// Source-file rows whose filesystem path no longer exists.
    pub missing: Vec<LibraryScanMissingFile>,
    /// Filesystem entries that do not follow Kino's canonical layout.
    pub layout_violations: Vec<StorageLayoutViolation>,
}

/// Canonical media file on disk with no matching source-file row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LibraryScanOrphan {
    /// Filesystem path found by the canonical layout scan.
    pub path: PathBuf,
    /// Media path family matched by the scanner.
    pub kind: CanonicalLayoutFileKind,
}

/// Source-file row whose filesystem path no longer exists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LibraryScanMissingFile {
    /// Source-file row that points at the missing path.
    pub source_file: LibrarySourceFile,
}

/// Persisted source-file row used for library reconciliation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct LibrarySourceFile {
    /// Source-file id.
    pub id: Id,
    /// Media item that owns this source file.
    pub media_item_id: Id,
    /// Canonical source path stored in the catalog.
    #[schema(value_type = String)]
    pub path: PathBuf,
    /// Transcode outputs derived from this source file.
    pub transcode_outputs: Vec<TranscodeOutput>,
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

/// Subtitle formats persisted as sidecars.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(rename_all = "snake_case")]
pub enum SubtitleFormat {
    /// SubRip text subtitles.
    Srt,
    /// Advanced SubStation Alpha text subtitles.
    Ass,
    /// JSON sidecar containing OCR cues and confidence metadata.
    Json,
}

impl SubtitleFormat {
    /// Database representation for this subtitle format.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Srt => "srt",
            Self::Ass => "ass",
            Self::Json => "json",
        }
    }

    /// File extension used for sidecar files of this format.
    pub fn extension(self) -> &'static str {
        self.as_str()
    }

    /// Parse a persisted subtitle format.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "srt" => Some(Self::Srt),
            "ass" => Some(Self::Ass),
            "json" => Some(Self::Json),
            _ => None,
        }
    }
}

impl fmt::Display for SubtitleFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How a subtitle sidecar's text was derived.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(rename_all = "snake_case")]
pub enum SubtitleProvenance {
    /// Text came directly from a text subtitle stream.
    Text,
    /// Text was derived from image subtitle frames through OCR.
    Ocr,
}

impl SubtitleProvenance {
    /// Database representation for this sidecar provenance.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Ocr => "ocr",
        }
    }

    /// Parse a persisted subtitle provenance.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "text" => Some(Self::Text),
            "ocr" => Some(Self::Ocr),
            _ => None,
        }
    }
}

impl fmt::Display for SubtitleProvenance {
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

/// OCR-derived subtitle stream data ready for sidecar persistence.
#[derive(Debug, Clone, PartialEq)]
pub struct OcrSubtitleTrack {
    /// Probed subtitle stream index in the source file.
    pub track_index: u32,
    /// Language reported for the subtitle stream.
    pub language: String,
    /// Time-coded cues recognized from extracted image subtitle frames.
    pub cues: Vec<subtitle_ocr::OcrCue>,
}

impl OcrSubtitleTrack {
    /// Construct an OCR-derived subtitle track.
    pub fn new(
        track_index: u32,
        language: impl Into<String>,
        cues: Vec<subtitle_ocr::OcrCue>,
    ) -> Self {
        Self {
            track_index,
            language: language.into(),
            cues,
        }
    }
}

/// Input for persisting OCR-derived subtitle sidecars for one media item.
#[derive(Debug, Clone, PartialEq)]
pub struct OcrSubtitleExtractionInput {
    /// Media item that owns the subtitle sidecars.
    pub media_item_id: Id,
    /// Directory where sidecars should be written.
    pub sidecar_dir: PathBuf,
    /// OCR-derived subtitle tracks.
    pub tracks: Vec<OcrSubtitleTrack>,
}

impl OcrSubtitleExtractionInput {
    /// Construct OCR-derived subtitle extraction input.
    pub fn new(
        media_item_id: Id,
        sidecar_dir: impl Into<PathBuf>,
        tracks: Vec<OcrSubtitleTrack>,
    ) -> Self {
        Self {
            media_item_id,
            sidecar_dir: sidecar_dir.into(),
            tracks,
        }
    }
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
    /// Persisted subtitle format.
    pub format: SubtitleFormat,
    /// How the subtitle text was derived.
    pub provenance: SubtitleProvenance,
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
                provenance: SubtitleProvenance::Text,
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
                    provenance,
                    track_index,
                    path,
                    created_at,
                    updated_at
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                "#,
            )
            .bind(sidecar.id)
            .bind(sidecar.media_item_id)
            .bind(&sidecar.language)
            .bind(sidecar.format.as_str())
            .bind(sidecar.provenance.as_str())
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

    /// Write OCR-derived subtitle tracks to JSON sidecars and index them by language.
    pub async fn extract_ocr_subtitles(
        &self,
        input: OcrSubtitleExtractionInput,
    ) -> Result<SubtitleExtractionResult> {
        create_dir_all(&input.sidecar_dir).await?;

        let mut seen = HashSet::new();
        let mut sidecars = Vec::new();
        let now = Timestamp::now();
        let mut tx = self.db.write_pool().begin().await?;

        for track in input.tracks {
            let language = normalize_language(&track.language, track.track_index)?;
            let format = SubtitleFormat::Json;
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
            write_ocr_sidecar(&path, &track.cues).await?;
            let path_text = path_to_db_text(&path)?;
            let sidecar = SubtitleSidecar {
                id: Id::new(),
                media_item_id: input.media_item_id,
                language,
                format,
                provenance: SubtitleProvenance::Ocr,
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
                    provenance,
                    track_index,
                    path,
                    created_at,
                    updated_at
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                "#,
            )
            .bind(sidecar.id)
            .bind(sidecar.media_item_id)
            .bind(&sidecar.language)
            .bind(sidecar.format.as_str())
            .bind(sidecar.provenance.as_str())
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

    /// Return indexed subtitle sidecars for a media item.
    pub async fn sidecars(&self, media_item_id: Id) -> Result<Vec<SubtitleSidecar>> {
        let rows = sqlx::query(
            r#"
            SELECT
                id,
                media_item_id,
                language,
                format,
                provenance,
                track_index,
                path,
                created_at,
                updated_at
            FROM subtitle_sidecars
            WHERE media_item_id = ?1
            ORDER BY language, track_index, provenance, id
            "#,
        )
        .bind(media_item_id)
        .fetch_all(self.db.read_pool())
        .await?;

        rows.iter().map(subtitle_sidecar_from_row).collect()
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

async fn read_dir(path: &Path) -> Result<tokio::fs::ReadDir> {
    tokio::fs::read_dir(path).await.map_err(|source| Error::Io {
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

async fn write_artwork_asset(
    cache_root: &Path,
    asset: &'static str,
    metadata_asset: &MetadataAsset,
) -> Result<PathBuf> {
    let extension = validate_asset_extension(asset, &metadata_asset.extension)?;
    let digest = Sha256::digest(&metadata_asset.bytes);
    let hash = format!("{digest:x}");
    let local_path =
        PathBuf::from(&hash[0..2])
            .join(&hash[2..4])
            .join(format!("{}.{}", &hash[4..], extension));
    let path = cache_root.join(&local_path);
    let parent = path.parent().unwrap_or(cache_root);
    create_dir_all(parent).await?;

    if !try_exists(&path).await? {
        tokio::fs::write(&path, &metadata_asset.bytes)
            .await
            .map_err(|source| Error::Io {
                path: path.clone(),
                source,
            })?;
    }

    Ok(local_path)
}

async fn write_metadata_sidecar(
    path: &Path,
    identity_id: CanonicalIdentityId,
    metadata: &TmdbMetadata,
    poster_path: &Path,
    backdrop_path: &Path,
    logo_path: Option<&Path>,
) -> Result<()> {
    let sidecar = MetadataSidecar {
        provider: identity_id.provider().as_str(),
        media_kind: identity_id.kind().as_str(),
        tmdb_id: identity_id.tmdb_id().get(),
        title: &metadata.title,
        description: &metadata.description,
        release_date: metadata.release_date.as_deref(),
        poster_source_url: metadata.poster.source_url.as_deref(),
        poster_path: path_to_db_text(poster_path)?,
        backdrop_source_url: metadata.backdrop.source_url.as_deref(),
        backdrop_path: path_to_db_text(backdrop_path)?,
        logo_source_url: metadata
            .logo
            .as_ref()
            .and_then(|asset| asset.source_url.as_deref()),
        logo_path: path_option_to_db_text(logo_path)?,
        cast: &metadata.cast,
    };
    let bytes = serde_json::to_vec_pretty(&sidecar)?;

    tokio::fs::write(path, bytes)
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

async fn write_ocr_sidecar(path: &Path, cues: &[subtitle_ocr::OcrCue]) -> Result<()> {
    let sidecar = OcrSubtitleSidecar {
        provenance: SubtitleProvenance::Ocr.as_str(),
        cues: cues.iter().map(OcrSubtitleCue::from).collect(),
    };
    let bytes = serde_json::to_vec_pretty(&sidecar)?;

    tokio::fs::write(path, bytes)
        .await
        .map_err(|source| Error::Io {
            path: path.to_path_buf(),
            source,
        })
}

#[derive(Serialize)]
struct MetadataSidecar<'a> {
    provider: &'static str,
    media_kind: &'static str,
    tmdb_id: u32,
    title: &'a str,
    description: &'a str,
    release_date: Option<&'a str>,
    poster_source_url: Option<&'a str>,
    poster_path: String,
    backdrop_source_url: Option<&'a str>,
    backdrop_path: String,
    logo_source_url: Option<&'a str>,
    logo_path: Option<String>,
    cast: &'a [MetadataCastMember],
}

#[derive(Serialize)]
struct OcrSubtitleSidecar {
    provenance: &'static str,
    cues: Vec<OcrSubtitleCue>,
}

#[derive(Serialize)]
struct OcrSubtitleCue {
    start: String,
    end: String,
    text: String,
    confidence: f32,
}

impl From<&subtitle_ocr::OcrCue> for OcrSubtitleCue {
    fn from(cue: &subtitle_ocr::OcrCue) -> Self {
        Self {
            start: format_duration(cue.start),
            end: format_duration(cue.end),
            text: cue.text.clone(),
            confidence: cue.confidence,
        }
    }
}

fn source_extension(path: &Path) -> Result<&str> {
    let extension = path.extension().ok_or_else(|| Error::MissingExtension {
        path: path.to_path_buf(),
    })?;

    extension.to_str().ok_or_else(|| Error::NonUtf8Path {
        path: path.to_path_buf(),
    })
}

fn path_file_name(path: &Path) -> Result<&str> {
    path.file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| Error::NonUtf8Path {
            path: path.to_path_buf(),
        })
}

fn path_file_stem(path: &Path) -> Result<&str> {
    path.file_stem()
        .and_then(|value| value.to_str())
        .ok_or_else(|| Error::NonUtf8Path {
            path: path.to_path_buf(),
        })
}

fn movie_file_matches(path: &Path, movie_name: &str) -> Result<bool> {
    if path.extension().is_none() {
        return Ok(false);
    }

    Ok(path_file_stem(path)? == movie_name)
}

fn tv_episode_file_matches(path: &Path, show_name: &str, season: &str) -> Result<bool> {
    if path.extension().is_none() {
        return Ok(false);
    }

    let stem = path_file_stem(path)?;
    let Some(episode) = stem.strip_prefix(&format!("{show_name} - S{season}E")) else {
        return Ok(false);
    };

    Ok(!episode.is_empty() && episode.bytes().all(|b| b.is_ascii_digit()))
}

fn validate_tmdb_metadata(mut metadata: TmdbMetadata) -> Result<TmdbMetadata> {
    metadata.title = require_metadata_text("title", metadata.title)?;
    metadata.description = require_metadata_text("description", metadata.description)?;
    validate_asset("poster", &metadata.poster)?;
    validate_asset("backdrop", &metadata.backdrop)?;
    if let Some(logo) = &metadata.logo {
        validate_asset("logo", logo)?;
    }
    if metadata.cast.is_empty() {
        return Err(Error::EmptyMetadataCast);
    }

    for member in &mut metadata.cast {
        member.name = require_metadata_text("cast.name", std::mem::take(&mut member.name))?;
    }
    metadata.cast.sort_by_key(|member| member.order);

    Ok(metadata)
}

fn require_metadata_text(field: &'static str, value: String) -> Result<String> {
    let value = value.trim().to_owned();
    if value.is_empty() {
        return Err(Error::EmptyMetadataField { field });
    }

    Ok(value)
}

fn validate_asset(asset: &'static str, metadata_asset: &MetadataAsset) -> Result<()> {
    if metadata_asset.bytes.is_empty() {
        return Err(Error::EmptyMetadataAsset { asset });
    }

    validate_asset_extension(asset, &metadata_asset.extension).map(|_| ())
}

fn validate_asset_extension(asset: &'static str, extension: &str) -> Result<String> {
    let extension = extension
        .trim()
        .trim_start_matches('.')
        .to_ascii_lowercase();
    if extension.is_empty()
        || !extension
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return Err(Error::InvalidMetadataAssetExtension { asset, extension });
    }

    Ok(extension)
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

fn format_duration(duration: Duration) -> String {
    let total_millis = duration.as_millis();
    let hours = total_millis / 3_600_000;
    let minutes = (total_millis % 3_600_000) / 60_000;
    let seconds = (total_millis % 60_000) / 1_000;
    let millis = total_millis % 1_000;

    format!("{hours:02}:{minutes:02}:{seconds:02}.{millis:03}")
}

fn path_to_db_text(path: &Path) -> Result<String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| Error::NonUtf8Path {
            path: path.to_path_buf(),
        })
}

fn path_option_to_db_text(path: Option<&Path>) -> Result<Option<String>> {
    path.map(path_to_db_text).transpose()
}

fn validated_catalog_limit(limit: Option<u32>) -> Result<u32> {
    let limit = limit.unwrap_or(DEFAULT_CATALOG_LIST_LIMIT);
    if !(1..=MAX_CATALOG_LIST_LIMIT).contains(&limit) {
        return Err(Error::InvalidCatalogListLimit {
            limit,
            max: MAX_CATALOG_LIST_LIMIT,
        });
    }

    Ok(limit)
}

fn validated_catalog_offset(offset: Option<u64>) -> Result<u64> {
    let offset = offset.unwrap_or(0);
    if offset > MAX_CATALOG_LIST_OFFSET {
        return Err(Error::InvalidCatalogListOffset {
            offset,
            max: MAX_CATALOG_LIST_OFFSET,
        });
    }

    Ok(offset)
}

fn like_contains_pattern(value: &str) -> String {
    let mut pattern = String::with_capacity(value.len() + 2);
    pattern.push('%');
    for ch in value.chars() {
        match ch {
            '%' | '_' | '\\' => {
                pattern.push('\\');
                pattern.push(ch);
            }
            _ => pattern.push(ch),
        }
    }
    pattern.push('%');
    pattern
}

fn fts_prefix_query(value: &str) -> Option<String> {
    let mut query = String::new();
    for token in value
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|token| !token.is_empty())
    {
        if !query.is_empty() {
            query.push(' ');
        }
        query.push('"');
        query.push_str(token);
        query.push_str("\"*");
    }

    if query.is_empty() { None } else { Some(query) }
}

fn catalog_media_item_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<CatalogMediaItem> {
    let media_kind: String = row.try_get("media_kind")?;
    let id = row.try_get("id")?;
    Ok(CatalogMediaItem {
        id,
        media_kind: MediaItemKind::parse(&media_kind)
            .ok_or(Error::InvalidMediaItemKind { value: media_kind })?,
        canonical_identity_id: row.try_get("canonical_identity_id")?,
        season_number: optional_u32_from_row(row, "season_number")?,
        episode_number: optional_u32_from_row(row, "episode_number")?,
        title: row.try_get("title")?,
        artwork: catalog_artwork_from_row(row, id)?,
        source_files: Vec::new(),
        subtitle_tracks: Vec::new(),
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

fn catalog_artwork_from_row(
    row: &sqlx::sqlite::SqliteRow,
    media_item_id: Id,
) -> Result<CatalogArtwork> {
    Ok(CatalogArtwork {
        poster: catalog_artwork_image_from_row(row, media_item_id, CatalogArtworkKind::Poster)?,
        backdrop: catalog_artwork_image_from_row(row, media_item_id, CatalogArtworkKind::Backdrop)?,
        logo: catalog_artwork_image_from_row(row, media_item_id, CatalogArtworkKind::Logo)?,
    })
}

fn catalog_artwork_image_from_row(
    row: &sqlx::sqlite::SqliteRow,
    media_item_id: Id,
    kind: CatalogArtworkKind,
) -> Result<Option<CatalogArtworkImage>> {
    let (source_field, local_field) = match kind {
        CatalogArtworkKind::Poster => ("poster_source_url", "poster_local_path"),
        CatalogArtworkKind::Backdrop => ("backdrop_source_url", "backdrop_local_path"),
        CatalogArtworkKind::Logo => ("logo_source_url", "logo_local_path"),
    };
    let source_url: Option<String> = source_url_from_db(row.try_get(source_field)?);
    let local_path: Option<String> = row.try_get(local_field)?;
    let internal_url = local_path.filter(|path| !path.is_empty()).map(|_| {
        format!(
            "/api/v1/library/items/{media_item_id}/images/{}",
            kind.as_str()
        )
    });

    if source_url.is_none() && internal_url.is_none() {
        return Ok(None);
    }

    Ok(Some(CatalogArtworkImage {
        source_url,
        internal_url,
    }))
}

fn source_url_from_db(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_owned())
        .filter(|value| value.starts_with("http://") || value.starts_with("https://"))
}

fn local_path_from_db(local_path: Option<String>, legacy_path: Option<String>) -> Option<String> {
    local_path.filter(|path| !path.is_empty()).or_else(|| {
        legacy_path.filter(|path| !path.starts_with("http://") && !path.starts_with("https://"))
    })
}

fn optional_u32_from_row(
    row: &sqlx::sqlite::SqliteRow,
    field: &'static str,
) -> Result<Option<u32>> {
    let value: Option<i64> = row.try_get(field)?;
    value
        .map(|value| u32::try_from(value).map_err(|_| Error::InvalidCatalogNumber { field, value }))
        .transpose()
}

fn subtitle_track_index_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<u32> {
    let value: i64 = row.try_get("track_index")?;
    u32::try_from(value).map_err(|_| Error::InvalidSubtitleTrackIndex { value })
}

fn library_source_file_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<LibrarySourceFile> {
    let path: String = row.try_get("path")?;
    Ok(LibrarySourceFile {
        id: row.try_get("id")?,
        media_item_id: row.try_get("media_item_id")?,
        path: PathBuf::from(path),
        transcode_outputs: Vec::new(),
    })
}

fn catalog_subtitle_track_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<CatalogSubtitleTrack> {
    let format = subtitle_format_from_row(row)?;
    let provenance = subtitle_provenance_from_row(row)?;
    let language: String = row.try_get("language")?;

    Ok(CatalogSubtitleTrack {
        id: row.try_get("id")?,
        language: language.clone(),
        label: subtitle_track_label(&language, provenance),
        format,
        provenance,
        track_index: subtitle_track_index_from_row(row)?,
    })
}

fn subtitle_sidecar_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<SubtitleSidecar> {
    let path: String = row.try_get("path")?;

    Ok(SubtitleSidecar {
        id: row.try_get("id")?,
        media_item_id: row.try_get("media_item_id")?,
        language: row.try_get("language")?,
        format: subtitle_format_from_row(row)?,
        provenance: subtitle_provenance_from_row(row)?,
        track_index: subtitle_track_index_from_row(row)?,
        path: PathBuf::from(path),
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

fn subtitle_format_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<SubtitleFormat> {
    let value: String = row.try_get("format")?;
    SubtitleFormat::parse(&value).ok_or(Error::InvalidSubtitleFormat { value })
}

fn subtitle_provenance_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<SubtitleProvenance> {
    let value: String = row.try_get("provenance")?;
    SubtitleProvenance::parse(&value).ok_or(Error::InvalidSubtitleProvenance { value })
}

fn subtitle_track_label(language: &str, provenance: SubtitleProvenance) -> String {
    let language = language.to_ascii_uppercase();
    match provenance {
        SubtitleProvenance::Text => language,
        SubtitleProvenance::Ocr => format!("{language} (OCR)"),
    }
}

fn transcode_output_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<TranscodeOutput> {
    let path: String = row.try_get("path")?;
    Ok(TranscodeOutput {
        id: row.try_get("id")?,
        source_file_id: row.try_get("source_file_id")?,
        path: PathBuf::from(path),
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

fn metadata_from_row(
    row: &sqlx::sqlite::SqliteRow,
    cast: Vec<MetadataCastMember>,
    artwork_cache_root: &Path,
) -> Result<CachedMediaMetadata> {
    let poster_path: Option<String> = row.try_get("poster_path")?;
    let poster_source_url = source_url_from_db(poster_path.clone());
    let poster_local_path =
        local_path_from_db(row.try_get("poster_local_path")?, poster_path).unwrap_or_default();
    let backdrop_path: Option<String> = row.try_get("backdrop_path")?;
    let backdrop_source_url = source_url_from_db(backdrop_path.clone());
    let backdrop_local_path =
        local_path_from_db(row.try_get("backdrop_local_path")?, backdrop_path).unwrap_or_default();
    let logo_path: Option<String> = row.try_get("logo_path")?;
    let logo_source_url = source_url_from_db(logo_path.clone());
    let logo_local_path = local_path_from_db(row.try_get("logo_local_path")?, logo_path);
    let metadata_path: String = row.try_get("metadata_path")?;
    let poster_local_path = PathBuf::from(poster_local_path);
    let backdrop_local_path = PathBuf::from(backdrop_local_path);
    let logo_local_path = logo_local_path.map(PathBuf::from);

    Ok(CachedMediaMetadata {
        media_item_id: row.try_get("media_item_id")?,
        canonical_identity_id: row.try_get("canonical_identity_id")?,
        title: row.try_get("title")?,
        description: row.try_get("description")?,
        release_date: row.try_get("release_date")?,
        poster_source_url,
        poster_path: artwork_cache_root.join(&poster_local_path),
        poster_local_path,
        backdrop_source_url,
        backdrop_path: artwork_cache_root.join(&backdrop_local_path),
        backdrop_local_path,
        logo_source_url,
        logo_path: logo_local_path
            .as_ref()
            .map(|local_path| artwork_cache_root.join(local_path)),
        logo_local_path,
        metadata_path: PathBuf::from(metadata_path),
        cast,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

fn cast_member_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<MetadataCastMember> {
    let position: i64 = row.try_get("position")?;
    let order =
        u32::try_from(position).map_err(|_| Error::InvalidMetadataCastOrder { position })?;
    Ok(MetadataCastMember {
        order,
        name: row.try_get("name")?,
        character: row.try_get("character")?,
        profile_path: row.try_get("profile_path")?,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use super::*;

    #[tokio::test]
    async fn enriches_metadata_to_disk_and_cache()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = kino_db::test_db().await?;
        let identity_id = movie_identity(550);
        let media_item_id = insert_tmdb_media_item(&db, identity_id).await?;
        let metadata_root = tempfile::tempdir()?;
        let provider = CountingMetadataProvider::new(sample_metadata());
        let service = MetadataService::new(db, metadata_root.path());

        let cached = service
            .enrich_tmdb_media_item(media_item_id, identity_id, &provider)
            .await?;

        assert_eq!(provider.calls(), 1);
        assert_eq!(cached.media_item_id, media_item_id);
        assert_eq!(cached.canonical_identity_id, identity_id);
        assert_eq!(cached.title, "Fight Club");
        assert_eq!(
            cached.description,
            "An insomniac office worker changes course."
        );
        assert_eq!(cached.release_date.as_deref(), Some("1999-10-15"));
        assert_eq!(cached.cast.len(), 2);
        assert_eq!(cached.cast[0].name, "Edward Norton");
        assert_eq!(cached.cast[1].name, "Brad Pitt");
        assert_eq!(tokio::fs::read(&cached.poster_path).await?, b"poster-bytes");
        assert_eq!(
            tokio::fs::read(&cached.backdrop_path).await?,
            b"backdrop-bytes"
        );
        let logo_path = match &cached.logo_path {
            Some(path) => path,
            None => panic!("logo should be cached"),
        };
        assert_eq!(tokio::fs::read(logo_path).await?, b"logo-bytes");

        let sidecar = tokio::fs::read_to_string(&cached.metadata_path).await?;
        assert!(sidecar.contains("\"title\": \"Fight Club\""));
        assert!(sidecar.contains("\"name\": \"Edward Norton\""));

        Ok(())
    }

    #[tokio::test]
    async fn cached_metadata_reads_without_calling_provider()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = kino_db::test_db().await?;
        let identity_id = movie_identity(27205);
        let media_item_id = insert_tmdb_media_item(&db, identity_id).await?;
        let metadata_root = tempfile::tempdir()?;
        let provider = CountingMetadataProvider::new(sample_metadata());
        let service = MetadataService::new(db, metadata_root.path());

        service
            .enrich_tmdb_media_item(media_item_id, identity_id, &provider)
            .await?;
        let cached = service
            .enrich_tmdb_media_item(media_item_id, identity_id, &provider)
            .await?;
        let readback = service.cached_metadata(media_item_id).await?;

        assert_eq!(provider.calls(), 1);
        assert_eq!(readback, Some(cached));

        Ok(())
    }

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

    #[tokio::test]
    async fn scans_only_canonical_layout_media_files()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let library_root = tempfile::tempdir()?;
        let movie = library_root
            .path()
            .join(MOVIES_DIRECTORY)
            .join("The Matrix (1999)")
            .join("The Matrix (1999).mkv");
        let episode = library_root
            .path()
            .join(TV_DIRECTORY)
            .join("Twin Peaks")
            .join("Season 02")
            .join("Twin Peaks - S02E03.mkv");
        let metadata = library_root.path().join(METADATA_DIRECTORY).join("tmdb");

        let movie_parent = movie.parent().ok_or("movie path should have parent")?;
        let episode_parent = episode.parent().ok_or("episode path should have parent")?;
        tokio::fs::create_dir_all(movie_parent).await?;
        tokio::fs::create_dir_all(episode_parent).await?;
        tokio::fs::create_dir_all(metadata).await?;
        tokio::fs::write(&movie, b"movie").await?;
        tokio::fs::write(&episode, b"episode").await?;

        let scanner = StorageLayoutScanner::new(library_root.path());
        let mut scan = scanner.scan().await?;
        scan.media_files
            .sort_by(|left, right| left.path.cmp(&right.path));

        assert_eq!(scan.violations, Vec::new());
        assert_eq!(
            scan.media_files,
            vec![
                CanonicalLayoutFile {
                    path: movie,
                    kind: CanonicalLayoutFileKind::Movie,
                },
                CanonicalLayoutFile {
                    path: episode,
                    kind: CanonicalLayoutFileKind::TvEpisode,
                },
            ]
        );

        Ok(())
    }

    #[tokio::test]
    async fn reports_storage_layout_violations()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let library_root = tempfile::tempdir()?;
        let unknown = library_root.path().join("Alien (1979).mkv");
        let movie = library_root
            .path()
            .join(MOVIES_DIRECTORY)
            .join("Alien (1979)")
            .join("wrong-name.mkv");
        let episode = library_root
            .path()
            .join(TV_DIRECTORY)
            .join("Twin Peaks")
            .join("Season Two")
            .join("Twin Peaks - S02E03.mkv");

        let movie_parent = movie.parent().ok_or("movie path should have parent")?;
        let episode_parent = episode.parent().ok_or("episode path should have parent")?;
        tokio::fs::create_dir_all(movie_parent).await?;
        tokio::fs::create_dir_all(episode_parent).await?;
        tokio::fs::write(&unknown, b"loose").await?;
        tokio::fs::write(&movie, b"movie").await?;
        tokio::fs::write(&episode, b"episode").await?;

        let scanner = StorageLayoutScanner::new(library_root.path());
        let mut scan = scanner.scan().await?;
        scan.violations
            .sort_by(|left, right| left.path.cmp(&right.path));

        assert_eq!(scan.media_files, Vec::new());
        assert_eq!(
            scan.violations,
            vec![
                StorageLayoutViolation {
                    path: unknown,
                    kind: StorageLayoutViolationKind::UnknownTopLevelEntry,
                },
                StorageLayoutViolation {
                    path: movie,
                    kind: StorageLayoutViolationKind::NonCanonicalMoviePath,
                },
                StorageLayoutViolation {
                    path: episode_parent.to_path_buf(),
                    kind: StorageLayoutViolationKind::NonCanonicalTvPath,
                },
            ]
        );

        Ok(())
    }

    #[tokio::test]
    async fn reconciles_canonical_files_with_source_file_rows()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = kino_db::test_db().await?;
        let media_item_id = insert_personal_media_item(&db).await?;
        let library_root = tempfile::tempdir()?;
        let known = library_root
            .path()
            .join(MOVIES_DIRECTORY)
            .join("Known (2001)")
            .join("Known (2001).mkv");
        let orphan = library_root
            .path()
            .join(MOVIES_DIRECTORY)
            .join("Orphan (2002)")
            .join("Orphan (2002).mkv");
        let missing = library_root
            .path()
            .join(TV_DIRECTORY)
            .join("Missing")
            .join("Season 01")
            .join("Missing - S01E01.mkv");
        let violation = library_root.path().join("loose.mkv");

        let known_parent = known.parent().ok_or("known path should have parent")?;
        let orphan_parent = orphan.parent().ok_or("orphan path should have parent")?;
        tokio::fs::create_dir_all(known_parent).await?;
        tokio::fs::create_dir_all(orphan_parent).await?;
        tokio::fs::write(&known, b"known").await?;
        tokio::fs::write(&orphan, b"orphan").await?;
        tokio::fs::write(&violation, b"loose").await?;
        let known_source_id = insert_source_file(&db, media_item_id, &known).await?;
        let missing_source_id = insert_source_file(&db, media_item_id, &missing).await?;

        let service = LibraryScanService::new(db, library_root.path());
        let report = service.scan().await?;

        assert_eq!(
            report.canonical_files,
            vec![
                CanonicalLayoutFile {
                    path: known.clone(),
                    kind: CanonicalLayoutFileKind::Movie,
                },
                CanonicalLayoutFile {
                    path: orphan.clone(),
                    kind: CanonicalLayoutFileKind::Movie,
                },
            ]
        );
        assert_eq!(
            report.orphans,
            vec![LibraryScanOrphan {
                path: orphan,
                kind: CanonicalLayoutFileKind::Movie,
            }]
        );
        assert_eq!(
            report.missing,
            vec![LibraryScanMissingFile {
                source_file: LibrarySourceFile {
                    id: missing_source_id,
                    media_item_id,
                    path: missing,
                    transcode_outputs: Vec::new(),
                },
            }]
        );
        assert_eq!(
            report.layout_violations,
            vec![StorageLayoutViolation {
                path: violation,
                kind: StorageLayoutViolationKind::UnknownTopLevelEntry,
            }]
        );
        assert_ne!(known_source_id, missing_source_id);

        Ok(())
    }

    #[tokio::test]
    async fn catalog_lists_filters_and_gets_media_items()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = kino_db::test_db().await?;
        let fight_club_identity = movie_identity(550);
        let matrix_identity = movie_identity(603);
        let breaking_bad_identity = tv_identity(1396);
        let fight_club = insert_tmdb_media_item(&db, fight_club_identity).await?;
        let matrix = insert_tmdb_media_item(&db, matrix_identity).await?;
        let breaking_bad = insert_tmdb_media_item(&db, breaking_bad_identity).await?;
        insert_catalog_title(&db, fight_club, fight_club_identity, "Fight Club").await?;
        insert_catalog_title(&db, matrix, matrix_identity, "The Matrix").await?;
        insert_catalog_title(&db, breaking_bad, breaking_bad_identity, "Breaking Bad").await?;
        let matrix_path = PathBuf::from("/library/Movies/The Matrix (1999)/The Matrix (1999).mkv");
        insert_source_file(&db, matrix, &matrix_path).await?;
        let sidecar_dir = tempfile::tempdir()?;
        let subtitle_service = SubtitleService::new(db.clone());
        let text_subtitles = subtitle_service
            .extract_text_subtitles(SubtitleExtractionInput::new(
                matrix,
                sidecar_dir.path(),
                vec![ProbedSubtitleTrack::new(
                    2,
                    "ENG",
                    ProbedSubtitleFormat::Srt,
                    "1\n00:00:01,000 --> 00:00:02,000\nWake up\n",
                )],
            ))
            .await?;
        let ocr_subtitles = subtitle_service
            .extract_ocr_subtitles(OcrSubtitleExtractionInput::new(
                matrix,
                sidecar_dir.path(),
                vec![OcrSubtitleTrack::new(
                    4,
                    "JPN",
                    vec![subtitle_ocr::OcrCue {
                        start: Duration::from_millis(1_250),
                        end: Duration::from_millis(2_750),
                        text: String::from("KONNICHIWA"),
                        confidence: 91.25,
                    }],
                )],
            ))
            .await?;
        let service = CatalogService::new(db);

        let movies = service
            .list(CatalogListQuery::new().with_media_kind(MediaItemKind::Movie))
            .await?;
        assert_eq!(
            movies.items.iter().map(|item| item.id).collect::<Vec<_>>(),
            vec![fight_club, matrix]
        );

        let title_match = service
            .list(CatalogListQuery::new().with_title_contains("matrix"))
            .await?;
        assert_eq!(title_match.items.len(), 1);
        assert_eq!(title_match.items[0].id, matrix);

        let with_source = service
            .list(CatalogListQuery::new().with_has_source_file(true))
            .await?;
        assert_eq!(with_source.items.len(), 1);
        assert_eq!(with_source.items[0].id, matrix);
        assert_eq!(with_source.items[0].source_files[0].path, matrix_path);

        let first_page = service
            .list(CatalogListQuery::new().with_limit(2).with_offset(0))
            .await?;
        assert_eq!(first_page.items.len(), 2);
        assert_eq!(first_page.next_offset, Some(2));

        let fetched = service.get(matrix).await?;
        assert_eq!(fetched.id, matrix);
        assert_eq!(fetched.title.as_deref(), Some("The Matrix"));
        assert_eq!(fetched.source_files.len(), 1);
        assert_eq!(
            fetched.subtitle_tracks,
            vec![
                CatalogSubtitleTrack {
                    id: text_subtitles.sidecars[0].id,
                    language: String::from("eng"),
                    label: String::from("ENG"),
                    format: SubtitleFormat::Srt,
                    provenance: SubtitleProvenance::Text,
                    track_index: 2,
                },
                CatalogSubtitleTrack {
                    id: ocr_subtitles.sidecars[0].id,
                    language: String::from("jpn"),
                    label: String::from("JPN (OCR)"),
                    format: SubtitleFormat::Json,
                    provenance: SubtitleProvenance::Ocr,
                    track_index: 4,
                },
            ]
        );

        Ok(())
    }

    #[tokio::test]
    async fn catalog_get_returns_internal_artwork_url()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = kino_db::test_db().await?;
        let identity_id = movie_identity(550);
        let media_item_id = insert_tmdb_media_item(&db, identity_id).await?;
        insert_catalog_artwork(
            &db,
            media_item_id,
            identity_id,
            "Fight Club",
            "https://image.tmdb.org/t/p/original/poster.jpg",
            "ab/cd/poster.jpg",
        )
        .await?;
        let service = CatalogService::new(db);

        let item = service.get(media_item_id).await?;
        let poster = item.artwork.poster.ok_or("poster should be present")?;

        assert_eq!(
            poster.source_url.as_deref(),
            Some("https://image.tmdb.org/t/p/original/poster.jpg")
        );
        assert_eq!(
            poster.internal_url.as_deref(),
            Some(format!("/api/v1/library/items/{media_item_id}/images/poster").as_str())
        );

        Ok(())
    }

    #[tokio::test]
    async fn catalog_search_matches_titles_and_cast_with_prefix_and_diacritics()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = kino_db::test_db().await?;
        let matrix_identity = movie_identity(603);
        let cafe_identity = movie_identity(110);
        let matrix = insert_tmdb_media_item(&db, matrix_identity).await?;
        let cafe = insert_tmdb_media_item(&db, cafe_identity).await?;
        insert_catalog_title(&db, matrix, matrix_identity, "The Matrix").await?;
        insert_catalog_title(&db, cafe, cafe_identity, "Café Society").await?;
        insert_catalog_cast_member(&db, matrix, 0, "Keanu Reeves").await?;
        insert_catalog_cast_member(&db, cafe, 0, "Matrix Runner").await?;
        let service = CatalogService::new(db);

        let partial_title = service
            .list(CatalogListQuery::new().with_search("matr"))
            .await?;
        assert_eq!(partial_title.items.len(), 2);
        assert_eq!(partial_title.items[0].id, matrix);

        let folded = service
            .list(CatalogListQuery::new().with_search("cafe"))
            .await?;
        assert_eq!(folded.items.len(), 1);
        assert_eq!(folded.items[0].id, cafe);

        let empty = service
            .list(CatalogListQuery::new().with_search("zzzz"))
            .await?;
        assert!(empty.items.is_empty());

        Ok(())
    }

    #[tokio::test]
    async fn catalog_search_tracks_metadata_updates()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = kino_db::test_db().await?;
        let identity = movie_identity(111);
        let item = insert_tmdb_media_item(&db, identity).await?;
        insert_catalog_title(&db, item, identity, "Old Title").await?;
        insert_catalog_cast_member(&db, item, 0, "Old Actor").await?;
        let service = CatalogService::new(db.clone());

        sqlx::query("UPDATE media_metadata_cache SET title = 'New Title' WHERE media_item_id = ?1")
            .bind(item)
            .execute(db.write_pool())
            .await?;
        sqlx::query(
            "UPDATE media_metadata_cast_members SET name = 'New Actor' WHERE media_item_id = ?1",
        )
        .bind(item)
        .execute(db.write_pool())
        .await?;

        let stale = service
            .list(CatalogListQuery::new().with_search("old"))
            .await?;
        assert!(stale.items.is_empty());

        let title = service
            .list(CatalogListQuery::new().with_search("new tit"))
            .await?;
        assert_eq!(
            title.items.iter().map(|item| item.id).collect::<Vec<_>>(),
            vec![item]
        );

        let cast = service
            .list(CatalogListQuery::new().with_search("actor"))
            .await?;
        assert_eq!(
            cast.items.iter().map(|item| item.id).collect::<Vec<_>>(),
            vec![item]
        );

        Ok(())
    }

    #[tokio::test]
    async fn register_transcode_output_persists_row_visible_in_catalog_read()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = kino_db::test_db().await?;
        let media_item_id = insert_personal_media_item(&db).await?;
        let source_path = PathBuf::from("/library/Movies/Fake (2026)/Fake (2026).mkv");
        let source_file_id = insert_source_file(&db, media_item_id, &source_path).await?;
        let service = CatalogService::new(db);

        let output = service
            .register_transcode_output(
                media_item_id,
                TranscodeCapabilities {
                    codec: "h264".to_owned(),
                    container: "mp4".to_owned(),
                    resolution: Some("1080p".to_owned()),
                    hdr: None,
                },
                "/tmp/fake.mp4",
            )
            .await?;
        let item = service.get(media_item_id).await?;

        assert_eq!(output.source_file_id, source_file_id);
        assert_eq!(output.path, PathBuf::from("/tmp/fake.mp4"));
        assert_eq!(item.source_files.len(), 1);
        assert_eq!(item.source_files[0].transcode_outputs, vec![output]);

        Ok(())
    }

    #[tokio::test]
    async fn catalog_get_reports_missing_media_item()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = kino_db::test_db().await?;
        let service = CatalogService::new(db);
        let missing = Id::new();
        let result = service.get(missing).await;

        assert!(matches!(result, Err(Error::MediaItemNotFound { id }) if id == missing));
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

    async fn insert_catalog_title(
        db: &Db,
        media_item_id: Id,
        canonical_identity_id: CanonicalIdentityId,
        title: &str,
    ) -> std::result::Result<(), sqlx::Error> {
        let now = Timestamp::now();

        sqlx::query(
            r#"
            INSERT INTO media_metadata_cache (
                media_item_id,
                canonical_identity_id,
                provider,
                title,
                description,
                release_date,
                poster_path,
                poster_local_path,
                backdrop_path,
                backdrop_local_path,
                logo_path,
                logo_local_path,
                metadata_path,
                created_at,
                updated_at
            )
            VALUES (?1, ?2, ?3, ?4, 'description', NULL, '', 'poster.jpg', '', 'backdrop.jpg', NULL, NULL, 'metadata.json', ?5, ?6)
            "#,
        )
        .bind(media_item_id)
        .bind(canonical_identity_id)
        .bind(canonical_identity_id.provider().as_str())
        .bind(title)
        .bind(now)
        .bind(now)
        .execute(db.write_pool())
        .await?;

        Ok(())
    }

    async fn insert_catalog_artwork(
        db: &Db,
        media_item_id: Id,
        canonical_identity_id: CanonicalIdentityId,
        title: &str,
        poster_source_url: &str,
        poster_local_path: &str,
    ) -> std::result::Result<(), sqlx::Error> {
        let now = Timestamp::now();

        sqlx::query(
            r#"
            INSERT INTO media_metadata_cache (
                media_item_id,
                canonical_identity_id,
                provider,
                title,
                description,
                release_date,
                poster_path,
                poster_local_path,
                backdrop_path,
                backdrop_local_path,
                logo_path,
                logo_local_path,
                metadata_path,
                created_at,
                updated_at
            )
            VALUES (?1, ?2, ?3, ?4, 'description', NULL, ?5, ?6, '', NULL, '', NULL, 'metadata.json', ?7, ?8)
            "#,
        )
        .bind(media_item_id)
        .bind(canonical_identity_id)
        .bind(canonical_identity_id.provider().as_str())
        .bind(title)
        .bind(poster_source_url)
        .bind(poster_local_path)
        .bind(now)
        .bind(now)
        .execute(db.write_pool())
        .await?;

        Ok(())
    }

    async fn insert_catalog_cast_member(
        db: &Db,
        media_item_id: Id,
        position: u32,
        name: &str,
    ) -> std::result::Result<(), sqlx::Error> {
        sqlx::query(
            r#"
            INSERT INTO media_metadata_cast_members (
                media_item_id,
                position,
                name,
                character,
                profile_path
            )
            VALUES (?1, ?2, ?3, 'character', NULL)
            "#,
        )
        .bind(media_item_id)
        .bind(i64::from(position))
        .bind(name)
        .execute(db.write_pool())
        .await?;

        Ok(())
    }

    async fn insert_tmdb_media_item(
        db: &Db,
        canonical_identity_id: CanonicalIdentityId,
    ) -> std::result::Result<Id, sqlx::Error> {
        let media_item_id = Id::new();
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
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            "#,
        )
        .bind(canonical_identity_id)
        .bind(canonical_identity_id.provider().as_str())
        .bind(canonical_identity_id.kind().as_str())
        .bind(i64::from(canonical_identity_id.tmdb_id().get()))
        .bind(kino_core::CanonicalIdentitySource::Manual.as_str())
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
                season_number,
                episode_number,
                created_at,
                updated_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            "#,
        )
        .bind(media_item_id)
        .bind(media_item_kind_for_identity(canonical_identity_id).as_str())
        .bind(canonical_identity_id)
        .bind(media_item_season_number(canonical_identity_id))
        .bind(media_item_episode_number(canonical_identity_id))
        .bind(now)
        .bind(now)
        .execute(db.write_pool())
        .await?;

        Ok(media_item_id)
    }

    fn movie_identity(value: u32) -> CanonicalIdentityId {
        let Some(tmdb_id) = kino_core::TmdbId::new(value) else {
            panic!("test tmdb id should be valid");
        };
        CanonicalIdentityId::tmdb_movie(tmdb_id)
    }

    fn tv_identity(value: u32) -> CanonicalIdentityId {
        let Some(tmdb_id) = kino_core::TmdbId::new(value) else {
            panic!("test tmdb id should be valid");
        };
        CanonicalIdentityId::tmdb_tv_series(tmdb_id)
    }

    fn media_item_kind_for_identity(canonical_identity_id: CanonicalIdentityId) -> MediaItemKind {
        match canonical_identity_id.kind() {
            kino_core::CanonicalIdentityKind::Movie => MediaItemKind::Movie,
            kino_core::CanonicalIdentityKind::TvSeries => MediaItemKind::TvEpisode,
        }
    }

    fn media_item_season_number(canonical_identity_id: CanonicalIdentityId) -> Option<i64> {
        match canonical_identity_id.kind() {
            kino_core::CanonicalIdentityKind::Movie => None,
            kino_core::CanonicalIdentityKind::TvSeries => Some(1),
        }
    }

    fn media_item_episode_number(canonical_identity_id: CanonicalIdentityId) -> Option<i64> {
        match canonical_identity_id.kind() {
            kino_core::CanonicalIdentityKind::Movie => None,
            kino_core::CanonicalIdentityKind::TvSeries => Some(1),
        }
    }

    fn sample_metadata() -> TmdbMetadata {
        TmdbMetadata::new(
            "Fight Club",
            "An insomniac office worker changes course.",
            Some("1999-10-15".to_owned()),
            MetadataAsset::new("jpg", b"poster-bytes".to_vec()),
            MetadataAsset::new(".jpg", b"backdrop-bytes".to_vec()),
            Some(MetadataAsset::new("png", b"logo-bytes".to_vec())),
            vec![
                MetadataCastMember::new(1, "Brad Pitt", "Tyler Durden", None),
                MetadataCastMember::new(0, "Edward Norton", "The Narrator", None),
            ],
        )
    }

    struct CountingMetadataProvider {
        metadata: TmdbMetadata,
        calls: Arc<AtomicUsize>,
    }

    impl CountingMetadataProvider {
        fn new(metadata: TmdbMetadata) -> Self {
            Self {
                metadata,
                calls: Arc::new(AtomicUsize::new(0)),
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    impl TmdbMetadataProvider for CountingMetadataProvider {
        fn fetch_metadata<'a>(
            &'a self,
            _identity_id: CanonicalIdentityId,
        ) -> MetadataFuture<'a, TmdbMetadata> {
            Box::pin(async move {
                self.calls.fetch_add(1, Ordering::SeqCst);
                Ok(self.metadata.clone())
            })
        }
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
        assert_eq!(result.sidecars[0].provenance, SubtitleProvenance::Text);
        assert_eq!(result.sidecars[1].language, "jpn");
        assert_eq!(result.sidecars[1].format, SubtitleFormat::Ass);
        assert_eq!(result.sidecars[1].provenance, SubtitleProvenance::Text);

        let srt = tokio::fs::read_to_string(&result.sidecars[0].path).await?;
        let ass = tokio::fs::read_to_string(&result.sidecars[1].path).await?;
        assert_eq!(srt, "1\n00:00:01,000 --> 00:00:02,000\nHello\n");
        assert_eq!(ass, "[Script Info]\nTitle: Kino\n");

        Ok(())
    }

    #[tokio::test]
    async fn extracts_ocr_subtitles_to_json_and_indexes_provenance()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = kino_db::test_db().await?;
        let media_item_id = insert_personal_media_item(&db).await?;
        let sidecar_dir = tempfile::tempdir()?;
        let service = SubtitleService::new(db.clone());

        let result = service
            .extract_ocr_subtitles(OcrSubtitleExtractionInput::new(
                media_item_id,
                sidecar_dir.path(),
                vec![OcrSubtitleTrack::new(
                    4,
                    "ENG",
                    vec![subtitle_ocr::OcrCue {
                        start: Duration::from_millis(1_250),
                        end: Duration::from_millis(2_750),
                        text: String::from("HELLO KINO"),
                        confidence: 93.5,
                    }],
                )],
            ))
            .await?;

        assert_eq!(result.languages, vec!["eng"]);
        assert_eq!(result.sidecars.len(), 1);
        assert_eq!(result.sidecars[0].format, SubtitleFormat::Json);
        assert_eq!(result.sidecars[0].provenance, SubtitleProvenance::Ocr);

        let json = tokio::fs::read_to_string(&result.sidecars[0].path).await?;
        let sidecar: serde_json::Value = serde_json::from_str(&json)?;
        assert_eq!(sidecar["provenance"], "ocr");
        assert_eq!(sidecar["cues"][0]["start"], "00:00:01.250");
        assert_eq!(sidecar["cues"][0]["end"], "00:00:02.750");
        assert_eq!(sidecar["cues"][0]["text"], "HELLO KINO");
        assert_eq!(sidecar["cues"][0]["confidence"], 93.5);

        let row: (String, String) = sqlx::query_as(
            "SELECT format, provenance FROM subtitle_sidecars WHERE media_item_id = ?1",
        )
        .bind(media_item_id)
        .fetch_one(db.read_pool())
        .await?;
        assert_eq!(row, (String::from("json"), String::from("ocr")));

        Ok(())
    }

    #[tokio::test]
    async fn lists_mixed_text_and_ocr_sidecars_with_provenance()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = kino_db::test_db().await?;
        let media_item_id = insert_personal_media_item(&db).await?;
        let sidecar_dir = tempfile::tempdir()?;
        let service = SubtitleService::new(db);

        service
            .extract_text_subtitles(SubtitleExtractionInput::new(
                media_item_id,
                sidecar_dir.path(),
                vec![ProbedSubtitleTrack::new(
                    2,
                    "ENG",
                    ProbedSubtitleFormat::Srt,
                    "1\n00:00:01,000 --> 00:00:02,000\nHello\n",
                )],
            ))
            .await?;
        service
            .extract_ocr_subtitles(OcrSubtitleExtractionInput::new(
                media_item_id,
                sidecar_dir.path(),
                vec![OcrSubtitleTrack::new(
                    4,
                    "SPA",
                    vec![subtitle_ocr::OcrCue {
                        start: Duration::from_millis(3_500),
                        end: Duration::from_millis(4_250),
                        text: String::from("HOLA KINO"),
                        confidence: 88.75,
                    }],
                )],
            ))
            .await?;

        let sidecars = service.sidecars(media_item_id).await?;

        assert_eq!(sidecars.len(), 2);
        assert_eq!(sidecars[0].language, "eng");
        assert_eq!(sidecars[0].format, SubtitleFormat::Srt);
        assert_eq!(sidecars[0].provenance, SubtitleProvenance::Text);
        assert_eq!(sidecars[1].language, "spa");
        assert_eq!(sidecars[1].format, SubtitleFormat::Json);
        assert_eq!(sidecars[1].provenance, SubtitleProvenance::Ocr);

        let json = tokio::fs::read_to_string(&sidecars[1].path).await?;
        let sidecar: serde_json::Value = serde_json::from_str(&json)?;
        assert_eq!(sidecar["cues"][0]["confidence"], 88.75);

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
