//! Canonical media identity types.

use std::{fmt, num::NonZeroU32, str::FromStr};

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sqlx::{
    Decode, Encode, Sqlite, Type,
    encode::IsNull,
    error::BoxDynError,
    sqlite::{SqliteArgumentValue, SqliteTypeInfo, SqliteValueRef},
};

use crate::Timestamp;

/// Metadata provider namespace for canonical identities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CanonicalIdentityProvider {
    /// The Movie Database.
    Tmdb,
}

impl CanonicalIdentityProvider {
    /// The persisted string representation.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Tmdb => "tmdb",
        }
    }

    /// Parse a persisted provider value.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "tmdb" => Some(Self::Tmdb),
            _ => None,
        }
    }
}

impl fmt::Display for CanonicalIdentityProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Media kind within a canonical identity provider namespace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CanonicalIdentityKind {
    /// TMDB movie identity.
    Movie,
    /// TMDB TV series identity.
    TvSeries,
}

impl CanonicalIdentityKind {
    /// The persisted string representation.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Movie => "movie",
            Self::TvSeries => "tv_series",
        }
    }

    /// Parse a persisted media kind value.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "movie" => Some(Self::Movie),
            "tv_series" => Some(Self::TvSeries),
            _ => None,
        }
    }
}

impl fmt::Display for CanonicalIdentityKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Positive TMDB id used by canonical identities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TmdbId(NonZeroU32);

impl TmdbId {
    /// Construct a TMDB id when `value` is positive.
    pub const fn new(value: u32) -> Option<Self> {
        match NonZeroU32::new(value) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }

    /// Return the numeric TMDB id.
    pub const fn get(self) -> u32 {
        self.0.get()
    }
}

impl fmt::Display for TmdbId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.get())
    }
}

impl FromStr for TmdbId {
    type Err = ParseTmdbIdError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let parsed = value.parse::<u32>().map_err(|_| ParseTmdbIdError {
            value: value.to_owned(),
        })?;
        Self::new(parsed).ok_or_else(|| ParseTmdbIdError {
            value: value.to_owned(),
        })
    }
}

/// Returned when a value is not a positive TMDB id.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid tmdb id {value}")]
pub struct ParseTmdbIdError {
    /// Invalid value.
    pub value: String,
}

/// Stable primary key for a canonical identity.
///
/// The key is provider-tagged and kind-tagged because TMDB movie and TV ids
/// live in separate namespaces. Its persisted form is `tmdb:<kind>:<id>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CanonicalIdentityId {
    provider: CanonicalIdentityProvider,
    kind: CanonicalIdentityKind,
    tmdb_id: TmdbId,
}

impl CanonicalIdentityId {
    /// Construct a canonical identity id from its typed parts.
    pub const fn new(
        provider: CanonicalIdentityProvider,
        kind: CanonicalIdentityKind,
        tmdb_id: TmdbId,
    ) -> Self {
        Self {
            provider,
            kind,
            tmdb_id,
        }
    }

    /// Construct a TMDB movie canonical identity id.
    pub const fn tmdb_movie(tmdb_id: TmdbId) -> Self {
        Self::new(
            CanonicalIdentityProvider::Tmdb,
            CanonicalIdentityKind::Movie,
            tmdb_id,
        )
    }

    /// Construct a TMDB TV series canonical identity id.
    pub const fn tmdb_tv_series(tmdb_id: TmdbId) -> Self {
        Self::new(
            CanonicalIdentityProvider::Tmdb,
            CanonicalIdentityKind::TvSeries,
            tmdb_id,
        )
    }

    /// The metadata provider namespace.
    pub const fn provider(self) -> CanonicalIdentityProvider {
        self.provider
    }

    /// The media kind inside the provider namespace.
    pub const fn kind(self) -> CanonicalIdentityKind {
        self.kind
    }

    /// The TMDB id inside the provider and media-kind namespace.
    pub const fn tmdb_id(self) -> TmdbId {
        self.tmdb_id
    }
}

impl fmt::Display for CanonicalIdentityId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}:{}", self.provider, self.kind, self.tmdb_id)
    }
}

impl FromStr for CanonicalIdentityId {
    type Err = ParseCanonicalIdentityIdError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let mut parts = value.split(':');
        let provider = parts
            .next()
            .and_then(CanonicalIdentityProvider::parse)
            .ok_or_else(|| ParseCanonicalIdentityIdError::InvalidFormat {
                value: value.to_owned(),
            })?;
        let kind = parts
            .next()
            .and_then(CanonicalIdentityKind::parse)
            .ok_or_else(|| ParseCanonicalIdentityIdError::InvalidFormat {
                value: value.to_owned(),
            })?;
        let tmdb_id = parts
            .next()
            .ok_or_else(|| ParseCanonicalIdentityIdError::InvalidFormat {
                value: value.to_owned(),
            })?
            .parse::<TmdbId>()?;

        if parts.next().is_some() {
            return Err(ParseCanonicalIdentityIdError::InvalidFormat {
                value: value.to_owned(),
            });
        }

        Ok(Self::new(provider, kind, tmdb_id))
    }
}

impl Serialize for CanonicalIdentityId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for CanonicalIdentityId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(serde::de::Error::custom)
    }
}

impl Type<Sqlite> for CanonicalIdentityId {
    fn type_info() -> SqliteTypeInfo {
        <String as Type<Sqlite>>::type_info()
    }

    fn compatible(ty: &SqliteTypeInfo) -> bool {
        <String as Type<Sqlite>>::compatible(ty)
    }
}

impl<'q> Encode<'q, Sqlite> for CanonicalIdentityId {
    fn encode_by_ref(&self, buf: &mut Vec<SqliteArgumentValue<'q>>) -> Result<IsNull, BoxDynError> {
        Encode::<Sqlite>::encode(self.to_string(), buf)
    }
}

impl<'r> Decode<'r, Sqlite> for CanonicalIdentityId {
    fn decode(value: SqliteValueRef<'r>) -> Result<Self, BoxDynError> {
        let value = <&str as Decode<Sqlite>>::decode(value)?;
        value.parse::<Self>().map_err(Into::into)
    }
}

/// Returned when a string is not a canonical identity id.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseCanonicalIdentityIdError {
    /// The id does not match `tmdb:<kind>:<id>`.
    #[error("invalid canonical identity id {value}")]
    InvalidFormat {
        /// Invalid value.
        value: String,
    },

    /// The TMDB id component is not valid.
    #[error(transparent)]
    TmdbId(#[from] ParseTmdbIdError),
}

/// Source that first introduced a canonical identity to Kino.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CanonicalIdentitySource {
    /// Resolver match scoring introduced the identity.
    MatchScoring,
    /// A user or admin manually introduced the identity.
    Manual,
}

impl CanonicalIdentitySource {
    /// The persisted string representation.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MatchScoring => "match_scoring",
            Self::Manual => "manual",
        }
    }

    /// Parse a persisted source value.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "match_scoring" => Some(Self::MatchScoring),
            "manual" => Some(Self::Manual),
            _ => None,
        }
    }
}

impl fmt::Display for CanonicalIdentitySource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Canonical media identity known to Kino.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanonicalIdentity {
    /// Stable provider-derived identity key.
    pub id: CanonicalIdentityId,
    /// Metadata provider namespace.
    pub provider: CanonicalIdentityProvider,
    /// Media kind inside the provider namespace.
    pub kind: CanonicalIdentityKind,
    /// Numeric TMDB id for this media kind.
    pub tmdb_id: TmdbId,
    /// Source that first introduced this identity.
    pub source: CanonicalIdentitySource,
    /// Identity creation timestamp.
    pub created_at: Timestamp,
    /// Last timestamp attached to this identity row.
    pub updated_at: Timestamp,
}

impl CanonicalIdentity {
    /// Construct a canonical identity projection from its id and source.
    pub const fn new(
        id: CanonicalIdentityId,
        source: CanonicalIdentitySource,
        created_at: Timestamp,
        updated_at: Timestamp,
    ) -> Self {
        Self {
            id,
            provider: id.provider(),
            kind: id.kind(),
            tmdb_id: id.tmdb_id(),
            source,
            created_at,
            updated_at,
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn canonical_identity_id_round_trips_display() {
        let id = CanonicalIdentityId::tmdb_movie(TmdbId::new(550).unwrap());

        assert_eq!(id.to_string(), "tmdb:movie:550");
        assert_eq!("tmdb:movie:550".parse::<CanonicalIdentityId>().unwrap(), id);
    }

    #[test]
    fn canonical_identity_id_rejects_invalid_values() {
        assert!("tmdb:movie:0".parse::<CanonicalIdentityId>().is_err());
        assert!("tmdb:movie".parse::<CanonicalIdentityId>().is_err());
        assert!("tmdb:episode:1".parse::<CanonicalIdentityId>().is_err());
    }

    #[test]
    fn serde_roundtrip_is_bare_string() {
        let id = CanonicalIdentityId::tmdb_tv_series(TmdbId::new(1396).unwrap());
        let json = serde_json::to_string(&id).unwrap();
        let back: CanonicalIdentityId = serde_json::from_str(&json).unwrap();

        assert_eq!(json, "\"tmdb:tv_series:1396\"");
        assert_eq!(back, id);
    }

    #[tokio::test]
    async fn sqlx_roundtrip_uses_text_identity_id() -> Result<(), Box<dyn std::error::Error>> {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await?;
        let id = CanonicalIdentityId::tmdb_movie(TmdbId::new(11).unwrap());

        sqlx::query("CREATE TABLE identities (id TEXT NOT NULL)")
            .execute(&pool)
            .await?;
        sqlx::query("INSERT INTO identities (id) VALUES (?1)")
            .bind(id)
            .execute(&pool)
            .await?;

        let stored: String = sqlx::query_scalar("SELECT id FROM identities")
            .fetch_one(&pool)
            .await?;
        let decoded: CanonicalIdentityId = sqlx::query_scalar("SELECT id FROM identities")
            .fetch_one(&pool)
            .await?;

        assert_eq!(stored, "tmdb:movie:11");
        assert_eq!(decoded, id);

        Ok(())
    }
}
