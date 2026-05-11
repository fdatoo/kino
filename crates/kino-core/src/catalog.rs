//! Shared library catalog data model.

use std::{fmt, path::PathBuf};

use serde::{Deserialize, Serialize};

use crate::{CanonicalIdentityId, Id, Timestamp};

/// User-facing media item kind stored in the catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum MediaItemKind {
    /// A movie identified directly by a canonical identity.
    Movie,
    /// A single TV episode identified by series identity plus season/episode.
    TvEpisode,
    /// User-owned media without provider identity.
    Personal,
}

/// Streamable catalog variant kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum VariantKind {
    /// Original source file stream.
    Source,
    /// Derived transcode output stream.
    Transcoded,
}

/// Capability hints clients can use when choosing a stream variant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct VariantCapabilities {
    /// Video codec label, or `unknown` when not yet probed.
    pub codec: String,
    /// Container format label, or `unknown` when not yet known.
    pub container: String,
    /// Resolution label such as `1080p`, when known.
    pub resolution: Option<String>,
    /// HDR format label, when known.
    pub hdr: Option<String>,
}

/// Streamable variant exposed on catalog item responses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct CatalogStreamVariant {
    /// Stable variant id within the catalog.
    pub variant_id: String,
    /// Variant source kind.
    pub kind: VariantKind,
    /// Best available capability hints for client selection.
    pub capabilities: VariantCapabilities,
    /// Relative API URL used to stream this variant.
    pub stream_url: String,
}

impl MediaItemKind {
    /// The persisted string representation.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Movie => "movie",
            Self::TvEpisode => "tv_episode",
            Self::Personal => "personal",
        }
    }

    /// Parse a persisted media item kind.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "movie" => Some(Self::Movie),
            "tv_episode" => Some(Self::TvEpisode),
            "personal" => Some(Self::Personal),
            _ => None,
        }
    }
}

impl fmt::Display for MediaItemKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Canonical user-facing catalog object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaItem {
    /// Media item id.
    pub id: Id,
    /// User-facing media item kind.
    pub kind: MediaItemKind,
    /// Provider identity for movies and TV episodes.
    pub canonical_identity_id: Option<CanonicalIdentityId>,
    /// TV season number for episode rows.
    pub season_number: Option<u32>,
    /// TV episode number for episode rows.
    pub episode_number: Option<u32>,
    /// Creation timestamp.
    pub created_at: Timestamp,
    /// Last update timestamp.
    pub updated_at: Timestamp,
}

impl MediaItem {
    /// Construct a movie media item.
    pub fn movie(id: Id, canonical_identity_id: CanonicalIdentityId, now: Timestamp) -> Self {
        Self {
            id,
            kind: MediaItemKind::Movie,
            canonical_identity_id: Some(canonical_identity_id),
            season_number: None,
            episode_number: None,
            created_at: now,
            updated_at: now,
        }
    }

    /// Construct a TV episode media item.
    pub fn tv_episode(
        id: Id,
        canonical_identity_id: CanonicalIdentityId,
        season_number: u32,
        episode_number: u32,
        now: Timestamp,
    ) -> Self {
        Self {
            id,
            kind: MediaItemKind::TvEpisode,
            canonical_identity_id: Some(canonical_identity_id),
            season_number: Some(season_number),
            episode_number: Some(episode_number),
            created_at: now,
            updated_at: now,
        }
    }

    /// Construct a personal media item.
    pub fn personal(id: Id, now: Timestamp) -> Self {
        Self {
            id,
            kind: MediaItemKind::Personal,
            canonical_identity_id: None,
            season_number: None,
            episode_number: None,
            created_at: now,
            updated_at: now,
        }
    }
}

/// Original ingested file attached to a media item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceFile {
    /// Source-file id.
    pub id: Id,
    /// Media item that owns this source file.
    pub media_item_id: Id,
    /// Canonical source path.
    pub path: PathBuf,
    /// Creation timestamp.
    pub created_at: Timestamp,
    /// Last update timestamp.
    pub updated_at: Timestamp,
}

impl SourceFile {
    /// Construct a source file row projection.
    pub fn new(id: Id, media_item_id: Id, path: impl Into<PathBuf>, now: Timestamp) -> Self {
        Self {
            id,
            media_item_id,
            path: path.into(),
            created_at: now,
            updated_at: now,
        }
    }
}

/// Derived stream output produced from a source file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct TranscodeOutput {
    /// Transcode output id.
    pub id: Id,
    /// Source file this output was derived from.
    pub source_file_id: Id,
    /// Output path.
    #[schema(value_type = String)]
    pub path: PathBuf,
    /// Creation timestamp.
    pub created_at: Timestamp,
    /// Last update timestamp.
    pub updated_at: Timestamp,
}

impl TranscodeOutput {
    /// Construct a transcode output row projection.
    pub fn new(id: Id, source_file_id: Id, path: impl Into<PathBuf>, now: Timestamp) -> Self {
        Self {
            id,
            source_file_id,
            path: path.into(),
            created_at: now,
            updated_at: now,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn media_item_kind_round_trips_storage_value() {
        assert_eq!(MediaItemKind::parse("movie"), Some(MediaItemKind::Movie));
        assert_eq!(
            MediaItemKind::parse("tv_episode"),
            Some(MediaItemKind::TvEpisode)
        );
        assert_eq!(
            MediaItemKind::parse("personal"),
            Some(MediaItemKind::Personal)
        );
        assert_eq!(MediaItemKind::Movie.as_str(), "movie");
        assert_eq!(MediaItemKind::TvEpisode.to_string(), "tv_episode");
        assert_eq!(MediaItemKind::parse("tv_series"), None);
    }

    #[test]
    fn stream_variants_serialize_source_and_transcoded_kinds() -> Result<(), serde_json::Error> {
        let variants = vec![
            CatalogStreamVariant {
                variant_id: "source-file-id".to_owned(),
                kind: VariantKind::Source,
                capabilities: VariantCapabilities {
                    codec: "unknown".to_owned(),
                    container: "mkv".to_owned(),
                    resolution: Some("2160p".to_owned()),
                    hdr: Some("hdr10".to_owned()),
                },
                stream_url: "/api/v1/stream/sourcefile/source-file-id".to_owned(),
            },
            CatalogStreamVariant {
                variant_id: "transcode-output-id".to_owned(),
                kind: VariantKind::Transcoded,
                capabilities: VariantCapabilities {
                    codec: "h264".to_owned(),
                    container: "mp4".to_owned(),
                    resolution: Some("1080p".to_owned()),
                    hdr: None,
                },
                stream_url: "/api/v1/stream/transcode/transcode-output-id".to_owned(),
            },
        ];

        let value = serde_json::to_value(&variants)?;

        assert_eq!(
            value,
            json!([
                {
                    "variant_id": "source-file-id",
                    "kind": "source",
                    "capabilities": {
                        "codec": "unknown",
                        "container": "mkv",
                        "resolution": "2160p",
                        "hdr": "hdr10",
                    },
                    "stream_url": "/api/v1/stream/sourcefile/source-file-id",
                },
                {
                    "variant_id": "transcode-output-id",
                    "kind": "transcoded",
                    "capabilities": {
                        "codec": "h264",
                        "container": "mp4",
                        "resolution": "1080p",
                        "hdr": null,
                    },
                    "stream_url": "/api/v1/stream/transcode/transcode-output-id",
                },
            ])
        );
        let decoded: Vec<CatalogStreamVariant> = serde_json::from_value(value)?;
        assert_eq!(decoded, variants);

        Ok(())
    }
}
