//! UTC timestamps used across the workspace.

use std::{fmt, str::FromStr};

use serde::{Deserialize, Deserializer, Serialize, Serializer};
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
}
