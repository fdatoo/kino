//! Shared user data model.

use serde::{Deserialize, Serialize};

use crate::{Id, Timestamp};

/// Stable id for the owner user created by the initial users migration.
pub const SEEDED_USER_ID: Id = Id::from_uuid(uuid::Uuid::from_u128(
    0x019e_1455_6000_7000_8000_0000_0000_0001,
));

/// User account stored by Kino.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct User {
    /// User id.
    pub id: Id,
    /// User-facing display name.
    pub display_name: String,
    /// Creation timestamp.
    pub created_at: Timestamp,
}

impl User {
    /// Construct a user row projection.
    pub fn new(id: Id, display_name: impl Into<String>, created_at: Timestamp) -> Self {
        Self {
            id,
            display_name: display_name.into(),
            created_at,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seeded_user_id_is_stable_uuid_v7() {
        assert_eq!(
            SEEDED_USER_ID.to_string(),
            "019e1455-6000-7000-8000-000000000001"
        );
        assert_eq!(SEEDED_USER_ID.as_uuid().get_version_num(), 7);
    }

    #[test]
    fn user_carries_standard_id_and_timestamp_fields() {
        let id = Id::new();
        let created_at = Timestamp::now();
        let user = User::new(id, "Owner", created_at);

        assert_eq!(user.id, id);
        assert_eq!(user.display_name, "Owner");
        assert_eq!(user.created_at, created_at);
    }
}
