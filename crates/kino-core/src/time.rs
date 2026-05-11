//! UTC timestamps used across the workspace.

use std::{fmt, str::FromStr};

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sqlx::{
    Decode, Encode, Sqlite, Type,
    encode::IsNull,
    error::BoxDynError,
    sqlite::{SqliteArgumentValue, SqliteTypeInfo, SqliteValueRef},
};
use time::{OffsetDateTime, UtcOffset, format_description::well_known::Rfc3339};

/// A UTC timestamp.
///
/// The UTC invariant is enforced at construction — once you have a
/// `Timestamp`, the offset is always UTC. Wire format is RFC3339.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Timestamp(OffsetDateTime);

impl Timestamp {
    /// The current UTC time.
    pub fn now() -> Self {
        Self(OffsetDateTime::now_utc())
    }

    /// Convert any `OffsetDateTime` to a UTC `Timestamp`.
    pub fn from_offset(t: OffsetDateTime) -> Self {
        Self(t.to_offset(UtcOffset::UTC))
    }

    /// The wrapped `OffsetDateTime` (always UTC).
    pub fn as_offset(&self) -> OffsetDateTime {
        self.0
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = self.0.format(&Rfc3339).map_err(|_| fmt::Error)?;
        f.write_str(&s)
    }
}

impl fmt::Debug for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl utoipa::PartialSchema for Timestamp {
    fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
        utoipa::openapi::schema::ObjectBuilder::new()
            .schema_type(utoipa::openapi::schema::Type::String)
            .format(Some(utoipa::openapi::schema::SchemaFormat::KnownFormat(
                utoipa::openapi::schema::KnownFormat::DateTime,
            )))
            .build()
            .into()
    }
}

impl utoipa::ToSchema for Timestamp {}

impl FromStr for Timestamp {
    type Err = ParseTimestampError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let t = OffsetDateTime::parse(s, &Rfc3339)?;
        Ok(Self::from_offset(t))
    }
}

/// Returned when a string is not a valid RFC3339 timestamp.
#[derive(Debug, thiserror::Error)]
#[error("invalid timestamp: {0}")]
pub struct ParseTimestampError(#[from] time::error::Parse);

impl Serialize for Timestamp {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        time::serde::rfc3339::serialize(&self.0, s)
    }
}

impl<'de> Deserialize<'de> for Timestamp {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let t = time::serde::rfc3339::deserialize(d)?;
        Ok(Self::from_offset(t))
    }
}

impl Type<Sqlite> for Timestamp {
    fn type_info() -> SqliteTypeInfo {
        <String as Type<Sqlite>>::type_info()
    }

    fn compatible(ty: &SqliteTypeInfo) -> bool {
        <String as Type<Sqlite>>::compatible(ty)
    }
}

impl<'q> Encode<'q, Sqlite> for Timestamp {
    fn encode_by_ref(&self, buf: &mut Vec<SqliteArgumentValue<'q>>) -> Result<IsNull, BoxDynError> {
        Encode::<Sqlite>::encode(self.0.format(&Rfc3339)?, buf)
    }
}

impl<'r> Decode<'r, Sqlite> for Timestamp {
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
    fn now_is_utc() {
        let t = Timestamp::now();
        assert_eq!(t.as_offset().offset(), UtcOffset::UTC);
    }

    #[test]
    fn from_offset_normalizes_to_utc() {
        let plus_two = UtcOffset::from_hms(2, 0, 0).unwrap();
        let dt = OffsetDateTime::now_utc().to_offset(plus_two);
        let ts = Timestamp::from_offset(dt);

        assert_eq!(ts.as_offset().offset(), UtcOffset::UTC);
        assert_eq!(ts.as_offset().unix_timestamp(), dt.unix_timestamp());
    }

    #[test]
    fn display_fromstr_roundtrip() {
        let t = Timestamp::now();
        let s = t.to_string();
        let parsed: Timestamp = s.parse().unwrap();
        assert_eq!(
            t.as_offset().unix_timestamp_nanos(),
            parsed.as_offset().unix_timestamp_nanos()
        );
    }

    #[test]
    fn serde_emits_rfc3339_with_z() {
        let t = Timestamp::now();
        let json = serde_json::to_string(&t).unwrap();
        assert!(json.ends_with("Z\""), "got: {json}");
        let back: Timestamp = serde_json::from_str(&json).unwrap();
        assert_eq!(
            t.as_offset().unix_timestamp_nanos(),
            back.as_offset().unix_timestamp_nanos()
        );
    }

    #[test]
    fn deserialize_normalizes_non_utc_input() {
        let json = "\"2026-05-05T12:00:00+02:00\"";
        let ts: Timestamp = serde_json::from_str(json).unwrap();
        assert_eq!(ts.as_offset().offset(), UtcOffset::UTC);
        assert_eq!(ts.as_offset().hour(), 10);
    }

    #[test]
    fn fromstr_rejects_garbage() {
        let err = "not-a-timestamp".parse::<Timestamp>().unwrap_err();
        assert!(
            err.to_string().starts_with("invalid timestamp:"),
            "got: {err}"
        );
    }

    #[tokio::test]
    async fn sqlx_roundtrip_uses_utc_rfc3339_text() -> Result<(), Box<dyn std::error::Error>> {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await?;
        let ts = Timestamp::from_offset(OffsetDateTime::parse(
            "2026-05-05T12:00:00+02:00",
            &Rfc3339,
        )?);

        sqlx::query("CREATE TABLE timestamps (at TEXT NOT NULL)")
            .execute(&pool)
            .await?;
        sqlx::query("INSERT INTO timestamps (at) VALUES (?1)")
            .bind(ts)
            .execute(&pool)
            .await?;

        let stored: String = sqlx::query_scalar("SELECT at FROM timestamps")
            .fetch_one(&pool)
            .await?;
        let decoded: Timestamp = sqlx::query_scalar("SELECT at FROM timestamps")
            .fetch_one(&pool)
            .await?;

        assert_eq!(stored, "2026-05-05T10:00:00Z");
        assert_eq!(decoded, ts);

        Ok(())
    }
}
