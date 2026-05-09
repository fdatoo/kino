//! UUID v7 identifiers used for every persisted entity.

use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};
use sqlx::{
    Decode, Encode, Sqlite, Type,
    encode::IsNull,
    error::BoxDynError,
    sqlite::{SqliteArgumentValue, SqliteTypeInfo, SqliteValueRef},
};

/// A UUID v7 identifier, used for every entity persisted by Kino.
///
/// Ids are time-prefixed, so they sort lexicographically by creation time.
/// Construct new ids with [`Id::new`]; round-trip persisted ids through
/// [`Id::from_uuid`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Id(uuid::Uuid);

impl Id {
    /// Generate a fresh UUID v7 id from the current time.
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self(uuid::Uuid::now_v7())
    }
}

impl Id {
    /// Wrap an existing UUID without checking its version.
    ///
    /// Use this on the persistence read path where the value has already
    /// been validated. New ids should be created with [`Id::new`].
    pub fn from_uuid(uuid: uuid::Uuid) -> Self {
        Self(uuid)
    }

    /// The wrapped UUID.
    pub fn as_uuid(&self) -> uuid::Uuid {
        self.0
    }
}

impl fmt::Display for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0.hyphenated(), f)
    }
}

impl fmt::Debug for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Id({})", self.0.hyphenated())
    }
}

impl FromStr for Id {
    type Err = ParseIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(uuid::Uuid::parse_str(s)?))
    }
}

/// Returned when a string is not a valid UUID.
#[derive(Debug, thiserror::Error)]
#[error("invalid id: {0}")]
pub struct ParseIdError(#[from] uuid::Error);

impl Type<Sqlite> for Id {
    fn type_info() -> SqliteTypeInfo {
        <String as Type<Sqlite>>::type_info()
    }

    fn compatible(ty: &SqliteTypeInfo) -> bool {
        <String as Type<Sqlite>>::compatible(ty)
    }
}

impl<'q> Encode<'q, Sqlite> for Id {
    fn encode_by_ref(&self, buf: &mut Vec<SqliteArgumentValue<'q>>) -> Result<IsNull, BoxDynError> {
        Encode::<Sqlite>::encode(self.to_string(), buf)
    }
}

impl<'r> Decode<'r, Sqlite> for Id {
    fn decode(value: SqliteValueRef<'r>) -> Result<Self, BoxDynError> {
        let value = <&str as Decode<Sqlite>>::decode(value)?;
        value.parse::<Self>().map_err(Into::into)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn new_produces_uuid_v7() {
        let id = Id::new();
        assert_eq!(id.as_uuid().get_version_num(), 7);
    }

    #[test]
    fn ids_sort_by_creation_time() {
        let a = Id::new();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let b = Id::new();
        assert!(a < b, "expected {a} < {b}");
    }

    #[test]
    fn display_fromstr_roundtrip() {
        let id = Id::new();
        let s = id.to_string();
        let parsed: Id = s.parse().unwrap();
        assert_eq!(id, parsed);
    }

    #[test]
    fn fromstr_rejects_garbage() {
        let err = "not-a-uuid".parse::<Id>().unwrap_err();
        assert!(err.to_string().starts_with("invalid id:"), "got: {err}");
    }

    #[test]
    fn serde_roundtrip_is_bare_string() {
        let id = Id::new();
        let json = serde_json::to_string(&id).unwrap();
        assert!(json.starts_with('"') && json.ends_with('"'), "got: {json}");
        let back: Id = serde_json::from_str(&json).unwrap();
        assert_eq!(id, back);
    }

    #[test]
    fn from_uuid_preserves_value() {
        let raw = uuid::Uuid::now_v7();
        let id = Id::from_uuid(raw);
        assert_eq!(id.as_uuid(), raw);
    }

    #[tokio::test]
    async fn sqlx_roundtrip_uses_text_uuid() -> Result<(), Box<dyn std::error::Error>> {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await?;
        let id = Id::new();

        sqlx::query("CREATE TABLE ids (id TEXT NOT NULL)")
            .execute(&pool)
            .await?;
        sqlx::query("INSERT INTO ids (id) VALUES (?1)")
            .bind(id)
            .execute(&pool)
            .await?;

        let stored: String = sqlx::query_scalar("SELECT id FROM ids")
            .fetch_one(&pool)
            .await?;
        let decoded: Id = sqlx::query_scalar("SELECT id FROM ids")
            .fetch_one(&pool)
            .await?;

        assert_eq!(stored, id.to_string());
        assert_eq!(decoded, id);

        Ok(())
    }
}
