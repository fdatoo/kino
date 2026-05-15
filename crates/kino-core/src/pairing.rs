//! Shared pairing flow data model.

use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};

use crate::{Id, Timestamp};

/// Client platform requesting a pairing code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PairingPlatform {
    /// iOS client.
    Ios,
    /// tvOS client.
    Tvos,
    /// macOS client.
    Macos,
}

impl PairingPlatform {
    /// The persisted string representation.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ios => "ios",
            Self::Tvos => "tvos",
            Self::Macos => "macos",
        }
    }

    /// Parse a persisted pairing platform.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "ios" => Some(Self::Ios),
            "tvos" => Some(Self::Tvos),
            "macos" => Some(Self::Macos),
            _ => None,
        }
    }
}

impl fmt::Display for PairingPlatform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for PairingPlatform {
    type Err = ParsePairingPlatformError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value).ok_or(ParsePairingPlatformError)
    }
}

/// Returned when a string is not a valid pairing platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("invalid pairing platform")]
pub struct ParsePairingPlatformError;

/// Lifecycle state for a pairing request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PairingStatus {
    /// Pairing code is waiting for admin approval.
    Pending,
    /// Pairing was approved and has an attached token.
    Approved,
    /// Pairing expired or was rejected before approval.
    Expired,
    /// Approved token was read by the client.
    Consumed,
}

impl PairingStatus {
    /// The persisted string representation.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Approved => "approved",
            Self::Expired => "expired",
            Self::Consumed => "consumed",
        }
    }

    /// Parse a persisted pairing status.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "pending" => Some(Self::Pending),
            "approved" => Some(Self::Approved),
            "expired" => Some(Self::Expired),
            "consumed" => Some(Self::Consumed),
            _ => None,
        }
    }
}

impl fmt::Display for PairingStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for PairingStatus {
    type Err = ParsePairingStatusError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value).ok_or(ParsePairingStatusError)
    }
}

/// Returned when a string is not a valid pairing status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("invalid pairing status")]
pub struct ParsePairingStatusError;

/// Short-lived pairing request created by an unauthenticated client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pairing {
    /// Pairing id.
    pub id: Id,
    /// Six-digit base10 pairing code.
    pub code: String,
    /// Client-supplied device name.
    pub device_name: String,
    /// Platform requesting the pairing.
    pub platform: PairingPlatform,
    /// Current pairing lifecycle state.
    pub status: PairingStatus,
    /// Device token linked on approval.
    pub token_id: Option<Id>,
    /// Creation timestamp.
    pub created_at: Timestamp,
    /// Pairing code expiry timestamp.
    pub expires_at: Timestamp,
    /// Approval timestamp, present only after approval.
    pub approved_at: Option<Timestamp>,
}

impl Pairing {
    /// Construct a pending pairing row projection.
    pub fn new(
        id: Id,
        code: impl Into<String>,
        device_name: impl Into<String>,
        platform: PairingPlatform,
        created_at: Timestamp,
        expires_at: Timestamp,
    ) -> Self {
        Self {
            id,
            code: code.into(),
            device_name: device_name.into(),
            platform,
            status: PairingStatus::Pending,
            token_id: None,
            created_at,
            expires_at,
            approved_at: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairing_platform_round_trips_storage_value() {
        assert_eq!(PairingPlatform::parse("ios"), Some(PairingPlatform::Ios));
        assert_eq!(PairingPlatform::Tvos.as_str(), "tvos");
        assert_eq!(PairingPlatform::Macos.to_string(), "macos");
        assert_eq!("ios".parse::<PairingPlatform>(), Ok(PairingPlatform::Ios));
        assert_eq!(PairingPlatform::parse("visionos"), None);
    }

    #[test]
    fn pairing_status_round_trips_storage_value() {
        assert_eq!(
            PairingStatus::parse("pending"),
            Some(PairingStatus::Pending)
        );
        assert_eq!(PairingStatus::Approved.as_str(), "approved");
        assert_eq!(PairingStatus::Expired.to_string(), "expired");
        assert_eq!(
            "consumed".parse::<PairingStatus>(),
            Ok(PairingStatus::Consumed)
        );
        assert_eq!(PairingStatus::parse("rejected"), None);
    }

    #[test]
    fn new_pairing_starts_pending_without_token() {
        let id = Id::new();
        let created_at = Timestamp::now();
        let expires_at = Timestamp::now();
        let pairing = Pairing::new(
            id,
            "123456",
            "Living room Apple TV",
            PairingPlatform::Tvos,
            created_at,
            expires_at,
        );

        assert_eq!(pairing.id, id);
        assert_eq!(pairing.code, "123456");
        assert_eq!(pairing.device_name, "Living room Apple TV");
        assert_eq!(pairing.platform, PairingPlatform::Tvos);
        assert_eq!(pairing.status, PairingStatus::Pending);
        assert_eq!(pairing.token_id, None);
        assert_eq!(pairing.created_at, created_at);
        assert_eq!(pairing.expires_at, expires_at);
        assert_eq!(pairing.approved_at, None);
    }
}
