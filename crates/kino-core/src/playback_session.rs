//! Shared playback session data model.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{Id, Timestamp};

/// Lifecycle state for an observed playback session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaybackSessionStatus {
    /// Playback is currently active.
    Active,
    /// Playback has gone quiet but can still be reaped or resumed.
    Idle,
    /// Playback has completed or was explicitly closed.
    Ended,
}

impl PlaybackSessionStatus {
    /// The persisted string representation.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Idle => "idle",
            Self::Ended => "ended",
        }
    }

    /// Parse a persisted playback session status.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "active" => Some(Self::Active),
            "idle" => Some(Self::Idle),
            "ended" => Some(Self::Ended),
            _ => None,
        }
    }
}

impl fmt::Display for PlaybackSessionStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Playback session observed for one user, device token, media item, and variant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaybackSession {
    /// Playback session id.
    pub id: Id,
    /// User id persisted as a foreign key to `users.id`.
    pub user_id: Id,
    /// Device token id persisted as a foreign key to `device_tokens.id`.
    pub token_id: Id,
    /// Media item id persisted as a foreign key to `media_items.id`.
    pub media_item_id: Id,
    /// Opaque playable variant identifier.
    pub variant_id: String,
    /// Session start timestamp.
    pub started_at: Timestamp,
    /// Most recent heartbeat timestamp.
    pub last_seen_at: Timestamp,
    /// Session end timestamp, present only once status is ended.
    pub ended_at: Option<Timestamp>,
    /// Current playback session status.
    pub status: PlaybackSessionStatus,
}

impl PlaybackSession {
    /// Construct an active playback session row projection.
    pub fn active(
        id: Id,
        user_id: Id,
        token_id: Id,
        media_item_id: Id,
        variant_id: impl Into<String>,
        started_at: Timestamp,
    ) -> Self {
        Self {
            id,
            user_id,
            token_id,
            media_item_id,
            variant_id: variant_id.into(),
            started_at,
            last_seen_at: started_at,
            ended_at: None,
            status: PlaybackSessionStatus::Active,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playback_session_status_round_trips_storage_value() {
        assert_eq!(
            PlaybackSessionStatus::parse("active"),
            Some(PlaybackSessionStatus::Active)
        );
        assert_eq!(PlaybackSessionStatus::Idle.as_str(), "idle");
        assert_eq!(PlaybackSessionStatus::Ended.to_string(), "ended");
        assert_eq!(PlaybackSessionStatus::parse("paused"), None);
    }

    #[test]
    fn active_session_starts_without_end_timestamp() {
        let id = Id::new();
        let user_id = Id::new();
        let token_id = Id::new();
        let media_item_id = Id::new();
        let started_at = Timestamp::now();

        let session = PlaybackSession::active(
            id,
            user_id,
            token_id,
            media_item_id,
            "source-file-id",
            started_at,
        );

        assert_eq!(session.id, id);
        assert_eq!(session.user_id, user_id);
        assert_eq!(session.token_id, token_id);
        assert_eq!(session.media_item_id, media_item_id);
        assert_eq!(session.variant_id, "source-file-id");
        assert_eq!(session.started_at, started_at);
        assert_eq!(session.last_seen_at, started_at);
        assert_eq!(session.ended_at, None);
        assert_eq!(session.status, PlaybackSessionStatus::Active);
    }
}
