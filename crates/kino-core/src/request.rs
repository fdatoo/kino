//! Shared request tracking data model.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{Id, Timestamp};

/// Durable request state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestState {
    /// A request has been accepted but not resolved to canonical media.
    Pending,
    /// Resolver candidates need a user choice before fulfillment can continue.
    NeedsDisambiguation,
    /// A request has been resolved to canonical media.
    Resolved,
    /// Kino is choosing how to satisfy the request.
    Planning,
    /// A provider is producing candidate media for the request.
    Fulfilling,
    /// Candidate media is being ingested into the library.
    Ingesting,
    /// The requested media is available in the library.
    Satisfied,
    /// The request cannot be completed.
    Failed,
    /// The request was cancelled before completion.
    Cancelled,
}

impl RequestState {
    /// The persisted string representation.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::NeedsDisambiguation => "needs_disambiguation",
            Self::Resolved => "resolved",
            Self::Planning => "planning",
            Self::Fulfilling => "fulfilling",
            Self::Ingesting => "ingesting",
            Self::Satisfied => "satisfied",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    /// Parse a persisted request state.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "pending" => Some(Self::Pending),
            "needs_disambiguation" => Some(Self::NeedsDisambiguation),
            "resolved" => Some(Self::Resolved),
            "planning" => Some(Self::Planning),
            "fulfilling" => Some(Self::Fulfilling),
            "ingesting" => Some(Self::Ingesting),
            "satisfied" => Some(Self::Satisfied),
            "failed" => Some(Self::Failed),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }

    /// Whether at least one transition command can move from this state to `next`.
    pub const fn can_transition_to(self, next: Self) -> bool {
        match self {
            Self::Pending => matches!(
                next,
                Self::NeedsDisambiguation | Self::Resolved | Self::Failed | Self::Cancelled
            ),
            Self::NeedsDisambiguation => {
                matches!(next, Self::Resolved | Self::Failed | Self::Cancelled)
            }
            Self::Resolved => matches!(next, Self::Planning | Self::Failed | Self::Cancelled),
            Self::Planning => {
                matches!(
                    next,
                    Self::Resolved | Self::Fulfilling | Self::Failed | Self::Cancelled
                )
            }
            Self::Fulfilling => {
                matches!(
                    next,
                    Self::Resolved | Self::Ingesting | Self::Failed | Self::Cancelled
                )
            }
            Self::Ingesting => {
                matches!(
                    next,
                    Self::Resolved | Self::Satisfied | Self::Failed | Self::Cancelled
                )
            }
            Self::Satisfied | Self::Failed | Self::Cancelled => false,
        }
    }

    /// Whether this state can still transition to a terminal outcome.
    pub const fn is_active(self) -> bool {
        matches!(
            self,
            Self::Pending
                | Self::NeedsDisambiguation
                | Self::Resolved
                | Self::Planning
                | Self::Fulfilling
                | Self::Ingesting
        )
    }
}

impl fmt::Display for RequestState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Typed reason for a failed request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestFailureReason {
    /// No configured provider accepted the request.
    NoProviderAccepted,
    /// The selected provider could not acquire media.
    AcquisitionFailed,
    /// Candidate media could not be ingested.
    IngestFailed,
    /// The request was cancelled through a path represented as failure.
    Cancelled,
}

impl RequestFailureReason {
    /// The persisted string representation.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NoProviderAccepted => "no_provider_accepted",
            Self::AcquisitionFailed => "acquisition_failed",
            Self::IngestFailed => "ingest_failed",
            Self::Cancelled => "cancelled",
        }
    }

    /// Parse a persisted failure reason.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "no_provider_accepted" => Some(Self::NoProviderAccepted),
            "acquisition_failed" => Some(Self::AcquisitionFailed),
            "ingest_failed" => Some(Self::IngestFailed),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }
}

impl fmt::Display for RequestFailureReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Principal that owns a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestRequester {
    /// Request submitted without an authenticated user.
    Anonymous,
    /// Request created by Kino itself.
    System,
    /// Request submitted by an authenticated user.
    User(Id),
}

impl RequestRequester {
    /// Anonymous requester used until external user identity exists.
    pub const fn anonymous() -> Self {
        Self::Anonymous
    }

    /// The persisted requester kind.
    pub const fn kind(self) -> &'static str {
        match self {
            Self::Anonymous => "anonymous",
            Self::System => "system",
            Self::User(_) => "user",
        }
    }

    /// The persisted requester id, present only for authenticated users.
    pub const fn id(self) -> Option<Id> {
        match self {
            Self::Anonymous | Self::System => None,
            Self::User(id) => Some(id),
        }
    }

    /// Parse persisted requester columns.
    pub fn from_parts(kind: &str, id: Option<Id>) -> Option<Self> {
        match (kind, id) {
            ("anonymous", None) => Some(Self::Anonymous),
            ("system", None) => Some(Self::System),
            ("user", Some(id)) => Some(Self::User(id)),
            _ => None,
        }
    }
}

/// Target media identity captured by a request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestTarget {
    /// Raw query text supplied by the requester.
    pub raw_query: String,
    /// Resolved canonical identity id once media identity persistence exists.
    pub canonical_identity_id: Option<Id>,
}

/// Current request projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    /// Request id.
    pub id: Id,
    /// Principal that owns the request.
    pub requester: RequestRequester,
    /// Media target requested by the principal.
    pub target: RequestTarget,
    /// Current request state.
    pub state: RequestState,
    /// Creation timestamp.
    pub created_at: Timestamp,
    /// Last model or state-change timestamp.
    pub updated_at: Timestamp,
    /// Fulfillment plan id once planning persistence exists.
    pub plan_id: Option<Id>,
    /// Failure reason when `state` is `Failed`.
    pub failure_reason: Option<RequestFailureReason>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_state_parses_persisted_values() {
        assert_eq!(RequestState::parse("pending"), Some(RequestState::Pending));
        assert_eq!(
            RequestState::parse("needs_disambiguation"),
            Some(RequestState::NeedsDisambiguation)
        );
        assert_eq!(RequestState::parse("not-real"), None);
    }

    #[test]
    fn requester_round_trips_storage_parts() {
        let user_id = Id::new();
        let requester = RequestRequester::User(user_id);

        assert_eq!(
            RequestRequester::from_parts(requester.kind(), requester.id()),
            Some(requester)
        );
        assert_eq!(RequestRequester::from_parts("user", None), None);
    }
}
