//! Request persistence, status events, and state transitions.

use std::fmt;

use kino_core::{Id, Timestamp};
use kino_db::Db;
use serde::{Deserialize, Serialize};
use sqlx::{Row, sqlite::SqliteRow};
use thiserror::Error;

/// Errors produced by `kino_fulfillment`.
#[derive(Debug, Error)]
pub enum Error {
    /// A database operation failed.
    #[error("database error: {0}")]
    Sqlx(#[from] sqlx::Error),

    /// A requested entity does not exist.
    #[error("request {id} was not found")]
    RequestNotFound {
        /// Missing request id.
        id: Id,
    },

    /// A state transition is not allowed by the request state machine.
    #[error("request transition {transition} from {from} to {to} is invalid")]
    InvalidTransition {
        /// Current request state.
        from: RequestState,
        /// Requested transition command.
        transition: RequestTransition,
        /// Requested next state.
        to: RequestState,
    },

    /// A persisted request state is outside the known enum.
    #[error("request state {value} is invalid")]
    InvalidRequestState {
        /// Persisted state value.
        value: String,
    },

    /// A persisted failure reason is outside the known enum.
    #[error("request failure reason {value} is invalid")]
    InvalidFailureReason {
        /// Persisted failure reason value.
        value: String,
    },

    /// A persisted event actor is malformed.
    #[error("request event actor is invalid")]
    InvalidEventActor,
}

/// Crate-local `Result` alias.
pub type Result<T> = std::result::Result<T, Error>;

/// Durable request state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestState {
    /// A request has been accepted but not resolved to canonical media.
    Pending,
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
            Self::Resolved => "resolved",
            Self::Planning => "planning",
            Self::Fulfilling => "fulfilling",
            Self::Ingesting => "ingesting",
            Self::Satisfied => "satisfied",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    /// Whether at least one transition command can move from this state to `next`.
    pub const fn can_transition_to(self, next: Self) -> bool {
        match self {
            Self::Pending => matches!(next, Self::Resolved | Self::Failed | Self::Cancelled),
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

    const fn is_active(self) -> bool {
        matches!(
            self,
            Self::Pending | Self::Resolved | Self::Planning | Self::Fulfilling | Self::Ingesting
        )
    }

    fn parse(value: &str) -> Result<Self> {
        match value {
            "pending" => Ok(Self::Pending),
            "resolved" => Ok(Self::Resolved),
            "planning" => Ok(Self::Planning),
            "fulfilling" => Ok(Self::Fulfilling),
            "ingesting" => Ok(Self::Ingesting),
            "satisfied" => Ok(Self::Satisfied),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            _ => Err(Error::InvalidRequestState {
                value: value.to_owned(),
            }),
        }
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

    fn parse(value: &str) -> Result<Self> {
        match value {
            "no_provider_accepted" => Ok(Self::NoProviderAccepted),
            "acquisition_failed" => Ok(Self::AcquisitionFailed),
            "ingest_failed" => Ok(Self::IngestFailed),
            "cancelled" => Ok(Self::Cancelled),
            _ => Err(Error::InvalidFailureReason {
                value: value.to_owned(),
            }),
        }
    }
}

impl fmt::Display for RequestFailureReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A validated request transition command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestTransition {
    /// Move from pending to resolved.
    Resolve,
    /// Move an active post-resolution request back to resolved.
    ReResolve,
    /// Move from resolved to planning.
    StartPlanning,
    /// Move from planning to fulfilling.
    StartFulfilling,
    /// Move from fulfilling to ingesting.
    StartIngesting,
    /// Move from ingesting to satisfied.
    Satisfy,
    /// Move from any active state to failed with a typed reason.
    Fail(RequestFailureReason),
    /// Move from any active state to cancelled.
    Cancel,
}

impl RequestTransition {
    fn to_state(self) -> RequestState {
        match self {
            Self::Resolve => RequestState::Resolved,
            Self::ReResolve => RequestState::Resolved,
            Self::StartPlanning => RequestState::Planning,
            Self::StartFulfilling => RequestState::Fulfilling,
            Self::StartIngesting => RequestState::Ingesting,
            Self::Satisfy => RequestState::Satisfied,
            Self::Fail(_) => RequestState::Failed,
            Self::Cancel => RequestState::Cancelled,
        }
    }

    /// Whether this transition command can be applied from `from`.
    pub const fn can_apply_from(self, from: RequestState) -> bool {
        match self {
            Self::Resolve => matches!(from, RequestState::Pending),
            Self::ReResolve => matches!(
                from,
                RequestState::Planning | RequestState::Fulfilling | RequestState::Ingesting
            ),
            Self::StartPlanning => matches!(from, RequestState::Resolved),
            Self::StartFulfilling => matches!(from, RequestState::Planning),
            Self::StartIngesting => matches!(from, RequestState::Fulfilling),
            Self::Satisfy => matches!(from, RequestState::Ingesting),
            Self::Fail(_) | Self::Cancel => from.is_active(),
        }
    }

    fn failure_reason(self) -> Option<RequestFailureReason> {
        match self {
            Self::Fail(reason) => Some(reason),
            Self::Resolve
            | Self::ReResolve
            | Self::StartPlanning
            | Self::StartFulfilling
            | Self::StartIngesting
            | Self::Satisfy
            | Self::Cancel => None,
        }
    }
}

impl fmt::Display for RequestTransition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Resolve => f.write_str("resolve"),
            Self::ReResolve => f.write_str("re-resolve"),
            Self::StartPlanning => f.write_str("start planning"),
            Self::StartFulfilling => f.write_str("start fulfilling"),
            Self::StartIngesting => f.write_str("start ingesting"),
            Self::Satisfy => f.write_str("satisfy"),
            Self::Fail(reason) => write!(f, "fail ({reason})"),
            Self::Cancel => f.write_str("cancel"),
        }
    }
}

/// Actor associated with a request status event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestEventActor {
    /// Kino changed state automatically.
    System,
    /// A user initiated the state change.
    User(Id),
}

impl RequestEventActor {
    fn kind(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::User(_) => "user",
        }
    }

    fn id(self) -> Option<Id> {
        match self {
            Self::System => None,
            Self::User(id) => Some(id),
        }
    }
}

/// Current request projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    /// Request id.
    pub id: Id,
    /// Current request state.
    pub state: RequestState,
    /// Creation timestamp.
    pub created_at: Timestamp,
    /// Last state-change timestamp.
    pub updated_at: Timestamp,
    /// Failure reason when `state` is `Failed`.
    pub failure_reason: Option<RequestFailureReason>,
}

/// Durable status event for a request state change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestStatusEvent {
    /// Event id.
    pub id: Id,
    /// Request id.
    pub request_id: Id,
    /// Previous request state, absent for the creation event.
    pub from_state: Option<RequestState>,
    /// New request state.
    pub to_state: RequestState,
    /// Event timestamp.
    pub occurred_at: Timestamp,
    /// Optional human-readable context.
    pub message: Option<String>,
    /// Optional actor responsible for the transition.
    pub actor: Option<RequestEventActor>,
}

/// Internal detail projection for request reads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestDetail {
    /// Current request projection.
    pub request: Request,
    /// Status events ordered by occurrence.
    pub status_events: Vec<RequestStatusEvent>,
}

/// Internal request API backed by `kino-db`.
#[derive(Clone)]
pub struct RequestService {
    db: Db,
}

impl RequestService {
    /// Construct a request service from an open database handle.
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// Create a new pending request.
    pub async fn create(
        &self,
        actor: Option<RequestEventActor>,
        message: Option<&str>,
    ) -> Result<RequestDetail> {
        let id = Id::new();
        let event_id = Id::new();
        let now = Timestamp::now();
        let mut tx = self.db.write_pool().begin().await?;

        sqlx::query(
            r#"
            INSERT INTO requests (id, state, created_at, updated_at, failure_reason)
            VALUES (?1, ?2, ?3, ?4, NULL)
            "#,
        )
        .bind(id)
        .bind(RequestState::Pending.as_str())
        .bind(now)
        .bind(now)
        .execute(&mut *tx)
        .await?;

        insert_status_event(
            &mut tx,
            NewStatusEvent {
                id: event_id,
                request_id: id,
                from_state: None,
                to_state: RequestState::Pending,
                occurred_at: now,
                message,
                actor,
            },
        )
        .await?;

        tx.commit().await?;
        self.get(id).await
    }

    /// Read a request with its full status-event log.
    pub async fn get(&self, id: Id) -> Result<RequestDetail> {
        let request_row = sqlx::query(
            r#"
            SELECT id, state, created_at, updated_at, failure_reason
            FROM requests
            WHERE id = ?1
            "#,
        )
        .bind(id)
        .fetch_optional(self.db.read_pool())
        .await?;

        let Some(request_row) = request_row else {
            return Err(Error::RequestNotFound { id });
        };

        let request = request_from_row(&request_row)?;
        let event_rows = sqlx::query(
            r#"
            SELECT id, request_id, from_state, to_state, occurred_at, message, actor_kind, actor_id
            FROM request_status_events
            WHERE request_id = ?1
            ORDER BY occurred_at, id
            "#,
        )
        .bind(id)
        .fetch_all(self.db.read_pool())
        .await?;
        let status_events = event_rows
            .iter()
            .map(status_event_from_row)
            .collect::<Result<Vec<_>>>()?;

        Ok(RequestDetail {
            request,
            status_events,
        })
    }

    /// List requests using the default projection ordered by creation time.
    pub async fn list(&self) -> Result<Vec<Request>> {
        let rows = sqlx::query(
            r#"
            SELECT id, state, created_at, updated_at, failure_reason
            FROM requests
            ORDER BY created_at, id
            "#,
        )
        .fetch_all(self.db.read_pool())
        .await?;

        rows.iter().map(request_from_row).collect()
    }

    /// Apply a validated state transition and append its status event.
    pub async fn transition(
        &self,
        request_id: Id,
        transition: RequestTransition,
        actor: Option<RequestEventActor>,
        message: Option<&str>,
    ) -> Result<RequestDetail> {
        let mut tx = self.db.write_pool().begin().await?;
        let row = sqlx::query(
            r#"
            SELECT id, state, created_at, updated_at, failure_reason
            FROM requests
            WHERE id = ?1
            "#,
        )
        .bind(request_id)
        .fetch_optional(&mut *tx)
        .await?;

        let Some(row) = row else {
            return Err(Error::RequestNotFound { id: request_id });
        };

        let current = request_from_row(&row)?;
        let next = transition.to_state();
        if !transition.can_apply_from(current.state) {
            return Err(Error::InvalidTransition {
                from: current.state,
                transition,
                to: next,
            });
        }

        let now = Timestamp::now();
        let failure_reason = transition.failure_reason();
        sqlx::query(
            r#"
            UPDATE requests
            SET state = ?2,
                updated_at = ?3,
                failure_reason = ?4
            WHERE id = ?1
            "#,
        )
        .bind(request_id)
        .bind(next.as_str())
        .bind(now)
        .bind(failure_reason.map(RequestFailureReason::as_str))
        .execute(&mut *tx)
        .await?;

        insert_status_event(
            &mut tx,
            NewStatusEvent {
                id: Id::new(),
                request_id,
                from_state: Some(current.state),
                to_state: next,
                occurred_at: now,
                message,
                actor,
            },
        )
        .await?;

        tx.commit().await?;
        self.get(request_id).await
    }
}

struct NewStatusEvent<'a> {
    id: Id,
    request_id: Id,
    from_state: Option<RequestState>,
    to_state: RequestState,
    occurred_at: Timestamp,
    message: Option<&'a str>,
    actor: Option<RequestEventActor>,
}

async fn insert_status_event(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    event: NewStatusEvent<'_>,
) -> Result<()> {
    sqlx::query(
        r#"
        INSERT INTO request_status_events (
            id,
            request_id,
            from_state,
            to_state,
            occurred_at,
            message,
            actor_kind,
            actor_id
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
        "#,
    )
    .bind(event.id)
    .bind(event.request_id)
    .bind(event.from_state.map(RequestState::as_str))
    .bind(event.to_state.as_str())
    .bind(event.occurred_at)
    .bind(event.message)
    .bind(event.actor.map(RequestEventActor::kind))
    .bind(event.actor.and_then(RequestEventActor::id))
    .execute(&mut **tx)
    .await?;

    Ok(())
}

fn request_from_row(row: &SqliteRow) -> Result<Request> {
    let failure_reason = row
        .try_get::<Option<&str>, _>("failure_reason")?
        .map(RequestFailureReason::parse)
        .transpose()?;

    Ok(Request {
        id: row.try_get("id")?,
        state: RequestState::parse(row.try_get("state")?)?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
        failure_reason,
    })
}

fn status_event_from_row(row: &SqliteRow) -> Result<RequestStatusEvent> {
    let actor = match (
        row.try_get::<Option<&str>, _>("actor_kind")?,
        row.try_get::<Option<Id>, _>("actor_id")?,
    ) {
        (None, None) => None,
        (Some("system"), None) => Some(RequestEventActor::System),
        (Some("user"), Some(id)) => Some(RequestEventActor::User(id)),
        _ => return Err(Error::InvalidEventActor),
    };
    let from_state = row
        .try_get::<Option<&str>, _>("from_state")?
        .map(RequestState::parse)
        .transpose()?;

    Ok(RequestStatusEvent {
        id: row.try_get("id")?,
        request_id: row.try_get("request_id")?,
        from_state,
        to_state: RequestState::parse(row.try_get("to_state")?)?,
        occurred_at: row.try_get("occurred_at")?,
        message: row.try_get("message")?,
        actor,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn create_writes_initial_status_event()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = kino_db::test_db().await?;
        let service = RequestService::new(db);
        let user_id = Id::new();

        let detail = service
            .create(
                Some(RequestEventActor::User(user_id)),
                Some("requested by user"),
            )
            .await?;

        assert_eq!(detail.request.state, RequestState::Pending);
        assert_eq!(detail.status_events.len(), 1);
        let event = &detail.status_events[0];
        assert_eq!(event.from_state, None);
        assert_eq!(event.to_state, RequestState::Pending);
        assert_eq!(event.message.as_deref(), Some("requested by user"));
        assert_eq!(event.actor, Some(RequestEventActor::User(user_id)));

        Ok(())
    }

    #[tokio::test]
    async fn transition_updates_state_and_appends_event()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = kino_db::test_db().await?;
        let service = RequestService::new(db);
        let created = service
            .create(Some(RequestEventActor::System), Some("accepted"))
            .await?;

        let detail = service
            .transition(
                created.request.id,
                RequestTransition::Resolve,
                Some(RequestEventActor::System),
                Some("matched canonical media"),
            )
            .await?;

        assert_eq!(detail.request.state, RequestState::Resolved);
        assert_eq!(detail.status_events.len(), 2);
        assert_eq!(
            detail.status_events[1].from_state,
            Some(RequestState::Pending)
        );
        assert_eq!(detail.status_events[1].to_state, RequestState::Resolved);
        assert_eq!(
            detail.status_events[1].message.as_deref(),
            Some("matched canonical media")
        );

        Ok(())
    }

    #[tokio::test]
    async fn list_returns_default_projection_in_creation_order()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = kino_db::test_db().await?;
        let service = RequestService::new(db);
        let first = service.create(None, Some("first")).await?;
        let second = service.create(None, Some("second")).await?;

        let requests = service.list().await?;

        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].id, first.request.id);
        assert_eq!(requests[1].id, second.request.id);
        assert_eq!(requests[0].state, RequestState::Pending);
        assert_eq!(requests[1].state, RequestState::Pending);

        Ok(())
    }

    #[tokio::test]
    async fn invalid_transition_does_not_write_event()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = kino_db::test_db().await?;
        let service = RequestService::new(db);
        let created = service.create(None, None).await?;

        let err = match service
            .transition(
                created.request.id,
                RequestTransition::Satisfy,
                Some(RequestEventActor::System),
                None,
            )
            .await
        {
            Ok(_) => panic!("invalid transition was accepted"),
            Err(err) => err,
        };

        assert!(matches!(
            err,
            Error::InvalidTransition {
                from: RequestState::Pending,
                transition: RequestTransition::Satisfy,
                to: RequestState::Satisfied
            }
        ));

        let detail = service.get(created.request.id).await?;
        assert_eq!(detail.request.state, RequestState::Pending);
        assert_eq!(detail.status_events.len(), 1);

        Ok(())
    }

    #[tokio::test]
    async fn terminal_states_reject_later_transitions()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = kino_db::test_db().await?;
        let service = RequestService::new(db);
        let created = service.create(None, None).await?;
        let cancelled = service
            .transition(created.request.id, RequestTransition::Cancel, None, None)
            .await?;

        let err = match service
            .transition(
                cancelled.request.id,
                RequestTransition::Resolve,
                Some(RequestEventActor::System),
                None,
            )
            .await
        {
            Ok(_) => panic!("terminal transition was accepted"),
            Err(err) => err,
        };

        assert!(matches!(
            err,
            Error::InvalidTransition {
                from: RequestState::Cancelled,
                transition: RequestTransition::Resolve,
                to: RequestState::Resolved
            }
        ));

        Ok(())
    }

    #[tokio::test]
    async fn failed_request_records_typed_failure_reason()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = kino_db::test_db().await?;
        let service = RequestService::new(db);
        let created = service.create(None, None).await?;

        let detail = service
            .transition(
                created.request.id,
                RequestTransition::Fail(RequestFailureReason::NoProviderAccepted),
                Some(RequestEventActor::System),
                Some("no provider accepted"),
            )
            .await?;

        assert_eq!(detail.request.state, RequestState::Failed);
        assert_eq!(
            detail.request.failure_reason,
            Some(RequestFailureReason::NoProviderAccepted)
        );
        assert_eq!(detail.status_events.len(), 2);

        Ok(())
    }

    #[test]
    fn transition_matrix_enumerates_legal_and_illegal_transitions() {
        let states = [
            RequestState::Pending,
            RequestState::Resolved,
            RequestState::Planning,
            RequestState::Fulfilling,
            RequestState::Ingesting,
            RequestState::Satisfied,
            RequestState::Failed,
            RequestState::Cancelled,
        ];
        let transitions = [
            RequestTransition::Resolve,
            RequestTransition::ReResolve,
            RequestTransition::StartPlanning,
            RequestTransition::StartFulfilling,
            RequestTransition::StartIngesting,
            RequestTransition::Satisfy,
            RequestTransition::Fail(RequestFailureReason::NoProviderAccepted),
            RequestTransition::Cancel,
        ];
        let legal = [
            (RequestState::Pending, RequestTransition::Resolve),
            (
                RequestState::Pending,
                RequestTransition::Fail(RequestFailureReason::NoProviderAccepted),
            ),
            (RequestState::Pending, RequestTransition::Cancel),
            (RequestState::Resolved, RequestTransition::StartPlanning),
            (
                RequestState::Resolved,
                RequestTransition::Fail(RequestFailureReason::NoProviderAccepted),
            ),
            (RequestState::Resolved, RequestTransition::Cancel),
            (RequestState::Planning, RequestTransition::ReResolve),
            (RequestState::Planning, RequestTransition::StartFulfilling),
            (
                RequestState::Planning,
                RequestTransition::Fail(RequestFailureReason::NoProviderAccepted),
            ),
            (RequestState::Planning, RequestTransition::Cancel),
            (RequestState::Fulfilling, RequestTransition::ReResolve),
            (RequestState::Fulfilling, RequestTransition::StartIngesting),
            (
                RequestState::Fulfilling,
                RequestTransition::Fail(RequestFailureReason::NoProviderAccepted),
            ),
            (RequestState::Fulfilling, RequestTransition::Cancel),
            (RequestState::Ingesting, RequestTransition::ReResolve),
            (RequestState::Ingesting, RequestTransition::Satisfy),
            (
                RequestState::Ingesting,
                RequestTransition::Fail(RequestFailureReason::NoProviderAccepted),
            ),
            (RequestState::Ingesting, RequestTransition::Cancel),
        ];

        for state in states {
            for transition in transitions {
                let expected = legal.contains(&(state, transition));

                assert_eq!(
                    transition.can_apply_from(state),
                    expected,
                    "{transition:?} from {state:?}"
                );
            }
        }
    }

    #[tokio::test]
    async fn re_resolve_moves_active_request_back_to_resolved()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = kino_db::test_db().await?;
        let service = RequestService::new(db);
        let created = service.create(None, None).await?;
        service
            .transition(created.request.id, RequestTransition::Resolve, None, None)
            .await?;
        service
            .transition(
                created.request.id,
                RequestTransition::StartPlanning,
                None,
                None,
            )
            .await?;

        let detail = service
            .transition(
                created.request.id,
                RequestTransition::ReResolve,
                Some(RequestEventActor::System),
                Some("resolved again"),
            )
            .await?;

        assert_eq!(detail.request.state, RequestState::Resolved);
        assert_eq!(detail.status_events.len(), 4);
        assert_eq!(
            detail.status_events[3].from_state,
            Some(RequestState::Planning)
        );
        assert_eq!(detail.status_events[3].to_state, RequestState::Resolved);
        assert_eq!(
            detail.status_events[3].message.as_deref(),
            Some("resolved again")
        );

        Ok(())
    }

    #[tokio::test]
    async fn status_events_are_append_only() -> std::result::Result<(), Box<dyn std::error::Error>>
    {
        let db = kino_db::test_db().await?;
        let service = RequestService::new(db.clone());
        let created = service.create(None, None).await?;
        let event_id = created.status_events[0].id;

        let update_result =
            sqlx::query("UPDATE request_status_events SET message = ?2 WHERE id = ?1")
                .bind(event_id)
                .bind("changed")
                .execute(db.write_pool())
                .await;
        let delete_result = sqlx::query("DELETE FROM request_status_events WHERE id = ?1")
            .bind(event_id)
            .execute(db.write_pool())
            .await;

        assert!(update_result.is_err());
        assert!(delete_result.is_err());

        let detail = service.get(created.request.id).await?;
        assert_eq!(detail.status_events.len(), 1);
        assert_eq!(detail.status_events[0].message, None);

        Ok(())
    }
}
