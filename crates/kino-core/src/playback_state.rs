//! Shared playback state data model.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{Id, Timestamp};

/// Returned when a playback position violates the non-negative invariant.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("playback position_seconds must be non-negative, got {position_seconds}")]
pub struct InvalidPlaybackPosition {
    /// Invalid position in seconds.
    pub position_seconds: i64,
}

/// Last known playback position for a user and media item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaybackProgress {
    /// User id that owns the playback position.
    pub user_id: Id,
    /// Media item the playback position belongs to.
    pub media_item_id: Id,
    /// Non-negative playback position in seconds.
    pub position_seconds: i64,
    /// Last update timestamp.
    pub updated_at: Timestamp,
    /// Device token that wrote this position, if the source was a device.
    pub source_device_token_id: Option<Id>,
}

impl PlaybackProgress {
    /// Construct a playback progress row projection.
    pub fn new(
        user_id: Id,
        media_item_id: Id,
        position_seconds: i64,
        updated_at: Timestamp,
        source_device_token_id: Option<Id>,
    ) -> Result<Self, InvalidPlaybackPosition> {
        if position_seconds < 0 {
            return Err(InvalidPlaybackPosition { position_seconds });
        }

        Ok(Self {
            user_id,
            media_item_id,
            position_seconds,
            updated_at,
            source_device_token_id,
        })
    }
}

/// Source that marked a user and media item as watched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WatchedSource {
    /// Playback completion marked the item watched.
    Auto,
    /// A user explicitly marked the item watched.
    Manual,
}

impl WatchedSource {
    /// The persisted string representation.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Manual => "manual",
        }
    }

    /// Parse a persisted watched source value.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "auto" => Some(Self::Auto),
            "manual" => Some(Self::Manual),
            _ => None,
        }
    }
}

impl fmt::Display for WatchedSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Watched marker for a user and media item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Watched {
    /// User id that owns the watched marker.
    pub user_id: Id,
    /// Media item the watched marker belongs to.
    pub media_item_id: Id,
    /// Timestamp when the media item was marked watched.
    pub watched_at: Timestamp,
    /// Source that marked the media item watched.
    pub source: WatchedSource,
    /// Manual-unmark tombstone; when true, auto heartbeats must not recreate the marker.
    pub unmarked: bool,
}

impl Watched {
    /// Construct an active watched row projection.
    pub const fn new(
        user_id: Id,
        media_item_id: Id,
        watched_at: Timestamp,
        source: WatchedSource,
    ) -> Self {
        Self {
            user_id,
            media_item_id,
            watched_at,
            source,
            unmarked: false,
        }
    }

    /// Construct the durable manual-unmark tombstone used to block auto re-marking.
    pub const fn manual_unmarked(user_id: Id, media_item_id: Id, watched_at: Timestamp) -> Self {
        Self {
            user_id,
            media_item_id,
            watched_at,
            source: WatchedSource::Manual,
            unmarked: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playback_progress_requires_non_negative_position() {
        let user_id = Id::new();
        let media_item_id = Id::new();
        let updated_at = Timestamp::now();

        let progress = PlaybackProgress::new(user_id, media_item_id, 120, updated_at, None);
        assert!(progress.is_ok());

        let err = PlaybackProgress::new(user_id, media_item_id, -1, updated_at, None)
            .err()
            .unwrap_or_else(|| panic!("negative position should fail"));
        assert_eq!(err.position_seconds, -1);
    }

    #[test]
    fn playback_progress_carries_standard_state_fields() {
        let user_id = Id::new();
        let media_item_id = Id::new();
        let updated_at = Timestamp::now();
        let source_device_token_id = Some(Id::new());
        let progress = PlaybackProgress::new(
            user_id,
            media_item_id,
            42,
            updated_at,
            source_device_token_id,
        )
        .unwrap_or_else(|err| panic!("{err}"));

        assert_eq!(progress.user_id, user_id);
        assert_eq!(progress.media_item_id, media_item_id);
        assert_eq!(progress.position_seconds, 42);
        assert_eq!(progress.updated_at, updated_at);
        assert_eq!(progress.source_device_token_id, source_device_token_id);
    }

    #[test]
    fn watched_source_round_trips_storage_value() {
        assert_eq!(WatchedSource::parse("auto"), Some(WatchedSource::Auto));
        assert_eq!(WatchedSource::parse("manual"), Some(WatchedSource::Manual));
        assert_eq!(WatchedSource::Auto.as_str(), "auto");
        assert_eq!(WatchedSource::Manual.to_string(), "manual");
        assert_eq!(WatchedSource::parse("system"), None);
    }

    #[test]
    fn watched_carries_standard_state_fields() {
        let user_id = Id::new();
        let media_item_id = Id::new();
        let watched_at = Timestamp::now();
        let watched = Watched::new(user_id, media_item_id, watched_at, WatchedSource::Manual);

        assert_eq!(watched.user_id, user_id);
        assert_eq!(watched.media_item_id, media_item_id);
        assert_eq!(watched.watched_at, watched_at);
        assert_eq!(watched.source, WatchedSource::Manual);
        assert!(!watched.unmarked);
    }

    #[test]
    fn watched_manual_unmark_is_a_manual_tombstone() {
        let user_id = Id::new();
        let media_item_id = Id::new();
        let watched = Watched::manual_unmarked(user_id, media_item_id, Timestamp::UNIX_EPOCH);

        assert_eq!(watched.user_id, user_id);
        assert_eq!(watched.media_item_id, media_item_id);
        assert_eq!(watched.watched_at, Timestamp::UNIX_EPOCH);
        assert_eq!(watched.source, WatchedSource::Manual);
        assert!(watched.unmarked);
    }
}
