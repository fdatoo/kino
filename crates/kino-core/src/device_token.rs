//! Shared device token data model.

use serde::{Deserialize, Serialize};

use crate::{Id, Timestamp};

/// Hashed authentication token for a device owned by a user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceToken {
    /// Device token id.
    pub id: Id,
    /// User id that owns the token, persisted as a foreign key to `users.id`.
    pub user_id: Id,
    /// Operator-facing label for the device.
    pub label: String,
    /// Hex-encoded SHA-256 hash of the plaintext token.
    pub hash: String,
    /// Last successful authentication time.
    pub last_seen_at: Option<Timestamp>,
    /// Revocation time, if the token has been revoked.
    pub revoked_at: Option<Timestamp>,
    /// Creation timestamp.
    pub created_at: Timestamp,
}

impl DeviceToken {
    /// Construct a device token row projection.
    pub fn new(
        id: Id,
        user_id: Id,
        label: impl Into<String>,
        hash: impl Into<String>,
        created_at: Timestamp,
    ) -> Self {
        Self {
            id,
            user_id,
            label: label.into(),
            hash: hash.into(),
            last_seen_at: None,
            revoked_at: None,
            created_at,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_token_carries_standard_auth_fields() {
        let id = Id::new();
        let user_id = Id::new();
        let created_at = Timestamp::now();
        let token = DeviceToken::new(
            id,
            user_id,
            "Living room Apple TV",
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            created_at,
        );

        assert_eq!(token.id, id);
        assert_eq!(token.user_id, user_id);
        assert_eq!(token.label, "Living room Apple TV");
        assert_eq!(
            token.hash,
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
        );
        assert_eq!(token.last_seen_at, None);
        assert_eq!(token.revoked_at, None);
        assert_eq!(token.created_at, created_at);
    }
}
