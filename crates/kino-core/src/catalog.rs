//! Shared library catalog data model.

use std::{fmt, path::PathBuf};

use serde::{Deserialize, Serialize};

use crate::{CanonicalIdentityId, Id, Timestamp};

/// User-facing media item kind stored in the catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaItemKind {
    /// A movie identified directly by a canonical identity.
    Movie,
    /// A single TV episode identified by series identity plus season/episode.
    TvEpisode,
    /// User-owned media without provider identity.
    Personal,
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscodeOutput {
    /// Transcode output id.
    pub id: Id,
    /// Source file this output was derived from.
    pub source_file_id: Id,
    /// Output path.
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
}
