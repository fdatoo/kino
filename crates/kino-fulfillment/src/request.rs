//! Request persistence, status events, matching, and state transitions.

use std::{collections::HashSet, fmt};

use kino_core::{
    Id, Request, RequestFailureReason, RequestRequester, RequestState, RequestTarget, Timestamp,
};
use kino_db::Db;
use serde::{Deserialize, Serialize};
use sqlx::{QueryBuilder, Row, Sqlite, sqlite::SqliteRow};
use thiserror::Error;

/// Default request list page size.
pub const REQUEST_LIST_DEFAULT_LIMIT: u32 = 50;
/// Maximum request list page size accepted by the persistence API.
pub const REQUEST_LIST_MAX_LIMIT: u32 = 250;
/// Maximum disambiguation candidates stored on a parked request.
pub const REQUEST_MATCH_CANDIDATE_LIMIT: usize = 5;
/// Minimum score for automatic request resolution.
pub const REQUEST_AUTO_RESOLVE_MIN_SCORE: f64 = 0.86;
/// Minimum lead over the next candidate for automatic request resolution.
pub const REQUEST_AUTO_RESOLVE_MIN_MARGIN: f64 = 0.08;

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

    /// Match scoring cannot run from the current request state.
    #[error("request match resolution from {from} is invalid")]
    InvalidMatchResolutionState {
        /// Current request state.
        from: RequestState,
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

    /// A persisted requester is malformed.
    #[error("request requester is invalid")]
    InvalidRequester,

    /// A request list limit is outside the accepted range.
    #[error("request list limit {limit} is invalid; expected 1..={max}")]
    InvalidListLimit {
        /// Requested page size.
        limit: u32,
        /// Maximum accepted page size.
        max: u32,
    },

    /// A request list offset is too large to bind for SQLite.
    #[error("request list offset {offset} is invalid")]
    InvalidListOffset {
        /// Requested row offset.
        offset: u64,
    },

    /// Match resolution received no candidates to score.
    #[error("request match resolution requires at least one candidate")]
    NoMatchCandidates,

    /// A candidate appears more than once in one scoring request.
    #[error("request match candidate {canonical_identity_id} is duplicated")]
    DuplicateMatchCandidate {
        /// Duplicated canonical identity id.
        canonical_identity_id: Id,
    },

    /// A candidate field is invalid.
    #[error("request match candidate {canonical_identity_id} is invalid: {reason}")]
    InvalidMatchCandidate {
        /// Candidate identity id.
        canonical_identity_id: Id,
        /// Human-readable validation failure.
        reason: &'static str,
    },
}

/// Crate-local `Result` alias.
pub type Result<T> = std::result::Result<T, Error>;

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
            Self::Resolve => matches!(
                from,
                RequestState::Pending | RequestState::NeedsDisambiguation
            ),
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

/// Candidate supplied by a resolver before request-level scoring.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RequestMatchCandidateInput {
    /// Candidate canonical identity id.
    pub canonical_identity_id: Id,
    /// Candidate display title.
    pub title: String,
    /// Candidate release or first-air year when known.
    pub year: Option<i32>,
    /// Provider popularity value used only as a tiebreaker.
    pub popularity: f64,
}

/// Ranked candidate stored when a request needs disambiguation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RequestMatchCandidate {
    /// Request-local rank starting at one.
    pub rank: u32,
    /// Candidate canonical identity id.
    pub canonical_identity_id: Id,
    /// Candidate display title.
    pub title: String,
    /// Candidate release or first-air year when known.
    pub year: Option<i32>,
    /// Provider popularity value used only as a tiebreaker.
    pub popularity: f64,
    /// Confidence score in the inclusive range `0.0..=1.0`.
    pub score: f64,
}

/// Internal detail projection for request reads.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RequestDetail {
    /// Current request projection.
    pub request: Request,
    /// Status events ordered by occurrence.
    pub status_events: Vec<RequestStatusEvent>,
    /// Ranked candidates when the request needs disambiguation.
    pub candidates: Vec<RequestMatchCandidate>,
}

/// Query parameters for listing requests.
///
/// Pagination uses offset semantics. Results are ordered by `(created_at, id)`;
/// `offset` skips that many rows after applying the optional state filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestListQuery {
    /// Optional state filter.
    pub state: Option<RequestState>,
    /// Maximum number of requests to return.
    pub limit: u32,
    /// Number of matching requests to skip.
    pub offset: u64,
}

impl RequestListQuery {
    /// Construct a list query with default pagination and no filters.
    pub const fn new() -> Self {
        Self {
            state: None,
            limit: REQUEST_LIST_DEFAULT_LIMIT,
            offset: 0,
        }
    }

    /// Return only requests in `state`.
    pub const fn with_state(self, state: RequestState) -> Self {
        Self {
            state: Some(state),
            ..self
        }
    }

    /// Set the page size.
    pub const fn with_limit(self, limit: u32) -> Self {
        Self { limit, ..self }
    }

    /// Set the row offset.
    pub const fn with_offset(self, offset: u64) -> Self {
        Self { offset, ..self }
    }

    fn validate(self) -> Result<()> {
        if self.limit == 0 || self.limit > REQUEST_LIST_MAX_LIMIT {
            return Err(Error::InvalidListLimit {
                limit: self.limit,
                max: REQUEST_LIST_MAX_LIMIT,
            });
        }

        i64::try_from(self.offset)
            .map(|_| ())
            .map_err(|_| Error::InvalidListOffset {
                offset: self.offset,
            })
    }
}

impl Default for RequestListQuery {
    fn default() -> Self {
        Self::new()
    }
}

/// One page of request list results.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestListPage {
    /// Default request projections.
    pub requests: Vec<Request>,
    /// Applied page size.
    pub limit: u32,
    /// Applied row offset.
    pub offset: u64,
    /// Offset for the next page when another page may exist.
    pub next_offset: Option<u64>,
}

/// Data required to create a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NewRequest<'a> {
    /// Raw query text supplied by the requester.
    pub target_raw_query: &'a str,
    /// Principal that owns the request.
    pub requester: RequestRequester,
    /// Optional actor for the initial status event.
    pub actor: Option<RequestEventActor>,
    /// Optional human-readable context for the initial status event.
    pub message: Option<&'a str>,
}

impl<'a> NewRequest<'a> {
    /// Construct an anonymous request for `target_raw_query`.
    pub const fn anonymous(target_raw_query: &'a str) -> Self {
        Self {
            target_raw_query,
            requester: RequestRequester::Anonymous,
            actor: None,
            message: None,
        }
    }

    /// Set the requester.
    pub const fn with_requester(self, requester: RequestRequester) -> Self {
        Self { requester, ..self }
    }

    /// Set the initial status-event actor.
    pub const fn with_actor(self, actor: RequestEventActor) -> Self {
        Self {
            actor: Some(actor),
            ..self
        }
    }

    /// Set the initial status-event message.
    pub const fn with_message(self, message: &'a str) -> Self {
        Self {
            message: Some(message),
            ..self
        }
    }
}

/// Mutable request model links.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RequestModelUpdate {
    /// Resolved canonical identity id.
    pub canonical_identity_id: Option<Id>,
    /// Fulfillment plan id.
    pub plan_id: Option<Id>,
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
    pub async fn create(&self, request: NewRequest<'_>) -> Result<RequestDetail> {
        let id = Id::new();
        let event_id = Id::new();
        let now = Timestamp::now();
        let mut tx = self.db.write_pool().begin().await?;

        sqlx::query(
            r#"
            INSERT INTO requests (
                id,
                requester_kind,
                requester_id,
                target_raw_query,
                canonical_identity_id,
                state,
                created_at,
                updated_at,
                plan_id,
                failure_reason
            )
            VALUES (?1, ?2, ?3, ?4, NULL, ?5, ?6, ?7, NULL, NULL)
            "#,
        )
        .bind(id)
        .bind(request.requester.kind())
        .bind(request.requester.id())
        .bind(request.target_raw_query)
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
                message: request.message,
                actor: request.actor,
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
            SELECT
                id,
                requester_kind,
                requester_id,
                target_raw_query,
                canonical_identity_id,
                state,
                created_at,
                updated_at,
                plan_id,
                failure_reason
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
        let candidate_rows = sqlx::query(
            r#"
            SELECT rank, canonical_identity_id, title, year, popularity, score
            FROM request_match_candidates
            WHERE request_id = ?1
            ORDER BY rank
            "#,
        )
        .bind(id)
        .fetch_all(self.db.read_pool())
        .await?;
        let candidates = candidate_rows
            .iter()
            .map(match_candidate_from_row)
            .collect::<Result<Vec<_>>>()?;

        Ok(RequestDetail {
            request,
            status_events,
            candidates,
        })
    }

    /// List requests using the default projection ordered by creation time.
    pub async fn list(&self, query: RequestListQuery) -> Result<RequestListPage> {
        query.validate()?;

        let mut builder = QueryBuilder::<Sqlite>::new(
            r#"
            SELECT
                id,
                requester_kind,
                requester_id,
                target_raw_query,
                canonical_identity_id,
                state,
                created_at,
                updated_at,
                plan_id,
                failure_reason
            FROM requests
            "#,
        );

        if let Some(state) = query.state {
            builder.push(" WHERE state = ");
            builder.push_bind(state.as_str());
        }

        builder.push(" ORDER BY created_at, id LIMIT ");
        builder.push_bind(i64::from(query.limit) + 1);
        builder.push(" OFFSET ");
        builder.push_bind(
            i64::try_from(query.offset).map_err(|_| Error::InvalidListOffset {
                offset: query.offset,
            })?,
        );

        let rows = builder.build().fetch_all(self.db.read_pool()).await?;
        let has_more = rows.len() > query.limit as usize;
        let requests = rows
            .iter()
            .take(query.limit as usize)
            .map(request_from_row)
            .collect::<Result<Vec<_>>>()?;

        Ok(RequestListPage {
            requests,
            limit: query.limit,
            offset: query.offset,
            next_offset: has_more.then_some(query.offset + u64::from(query.limit)),
        })
    }

    /// Update nullable links on the current request projection.
    pub async fn update_model(
        &self,
        request_id: Id,
        update: RequestModelUpdate,
    ) -> Result<RequestDetail> {
        let now = Timestamp::now();
        let result = sqlx::query(
            r#"
            UPDATE requests
            SET canonical_identity_id = ?2,
                plan_id = ?3,
                updated_at = ?4
            WHERE id = ?1
            "#,
        )
        .bind(request_id)
        .bind(update.canonical_identity_id)
        .bind(update.plan_id)
        .bind(now)
        .execute(self.db.write_pool())
        .await?;

        if result.rows_affected() == 0 {
            return Err(Error::RequestNotFound { id: request_id });
        }

        self.get(request_id).await
    }

    /// Score resolver candidates and either resolve or park the request.
    pub async fn resolve_matches(
        &self,
        request_id: Id,
        candidates: Vec<RequestMatchCandidateInput>,
        actor: Option<RequestEventActor>,
        message: Option<&str>,
    ) -> Result<RequestDetail> {
        validate_match_candidates(&candidates)?;

        let mut tx = self.db.write_pool().begin().await?;
        let row = sqlx::query(
            r#"
            SELECT
                id,
                requester_kind,
                requester_id,
                target_raw_query,
                canonical_identity_id,
                state,
                created_at,
                updated_at,
                plan_id,
                failure_reason
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
        if !matches!(
            current.state,
            RequestState::Pending | RequestState::NeedsDisambiguation
        ) {
            return Err(Error::InvalidMatchResolutionState {
                from: current.state,
            });
        }

        let scored = score_match_candidates(&current.target.raw_query, candidates);
        let top = scored.first().ok_or(Error::NoMatchCandidates)?;
        let next_score = scored.get(1).map_or(0.0, |candidate| candidate.score);
        let auto_resolve = top.score >= REQUEST_AUTO_RESOLVE_MIN_SCORE
            && top.score - next_score >= REQUEST_AUTO_RESOLVE_MIN_MARGIN;
        let now = Timestamp::now();

        sqlx::query("DELETE FROM request_match_candidates WHERE request_id = ?1")
            .bind(request_id)
            .execute(&mut *tx)
            .await?;

        if auto_resolve {
            let next = RequestState::Resolved;
            if !RequestTransition::Resolve.can_apply_from(current.state) {
                return Err(Error::InvalidTransition {
                    from: current.state,
                    transition: RequestTransition::Resolve,
                    to: next,
                });
            }

            sqlx::query(
                r#"
                UPDATE requests
                SET canonical_identity_id = ?2,
                    state = ?3,
                    updated_at = ?4,
                    failure_reason = NULL
                WHERE id = ?1
                "#,
            )
            .bind(request_id)
            .bind(top.canonical_identity_id)
            .bind(next.as_str())
            .bind(now)
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
        } else {
            for candidate in scored.iter().take(REQUEST_MATCH_CANDIDATE_LIMIT) {
                insert_match_candidate(&mut tx, request_id, candidate, now).await?;
            }

            if current.state == RequestState::NeedsDisambiguation {
                sqlx::query(
                    r#"
                    UPDATE requests
                    SET canonical_identity_id = NULL,
                        updated_at = ?2,
                        failure_reason = NULL
                    WHERE id = ?1
                    "#,
                )
                .bind(request_id)
                .bind(now)
                .execute(&mut *tx)
                .await?;
            } else {
                let next = RequestState::NeedsDisambiguation;

                sqlx::query(
                    r#"
                    UPDATE requests
                    SET canonical_identity_id = NULL,
                        state = ?2,
                        updated_at = ?3,
                        failure_reason = NULL
                    WHERE id = ?1
                    "#,
                )
                .bind(request_id)
                .bind(next.as_str())
                .bind(now)
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
            }
        }

        tx.commit().await?;
        self.get(request_id).await
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
            SELECT
                id,
                requester_kind,
                requester_id,
                target_raw_query,
                canonical_identity_id,
                state,
                created_at,
                updated_at,
                plan_id,
                failure_reason
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

async fn insert_match_candidate(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    request_id: Id,
    candidate: &RequestMatchCandidate,
    created_at: Timestamp,
) -> Result<()> {
    sqlx::query(
        r#"
        INSERT INTO request_match_candidates (
            request_id,
            rank,
            canonical_identity_id,
            title,
            year,
            popularity,
            score,
            created_at
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
        "#,
    )
    .bind(request_id)
    .bind(i64::from(candidate.rank))
    .bind(candidate.canonical_identity_id)
    .bind(candidate.title.as_str())
    .bind(candidate.year)
    .bind(candidate.popularity)
    .bind(candidate.score)
    .bind(created_at)
    .execute(&mut **tx)
    .await?;

    Ok(())
}

fn request_from_row(row: &SqliteRow) -> Result<Request> {
    let failure_reason = row
        .try_get::<Option<&str>, _>("failure_reason")?
        .map(parse_failure_reason)
        .transpose()?;
    let requester = RequestRequester::from_parts(
        row.try_get("requester_kind")?,
        row.try_get::<Option<Id>, _>("requester_id")?,
    )
    .ok_or(Error::InvalidRequester)?;

    Ok(Request {
        id: row.try_get("id")?,
        requester,
        target: RequestTarget {
            raw_query: row.try_get("target_raw_query")?,
            canonical_identity_id: row.try_get("canonical_identity_id")?,
        },
        state: parse_request_state(row.try_get("state")?)?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
        plan_id: row.try_get("plan_id")?,
        failure_reason,
    })
}

fn match_candidate_from_row(row: &SqliteRow) -> Result<RequestMatchCandidate> {
    let canonical_identity_id = row.try_get("canonical_identity_id")?;
    let rank =
        row.try_get::<i64, _>("rank")?
            .try_into()
            .map_err(|_| Error::InvalidMatchCandidate {
                canonical_identity_id,
                reason: "rank is outside u32 range",
            })?;

    Ok(RequestMatchCandidate {
        rank,
        canonical_identity_id,
        title: row.try_get("title")?,
        year: row.try_get("year")?,
        popularity: row.try_get("popularity")?,
        score: row.try_get("score")?,
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
        .map(parse_request_state)
        .transpose()?;

    Ok(RequestStatusEvent {
        id: row.try_get("id")?,
        request_id: row.try_get("request_id")?,
        from_state,
        to_state: parse_request_state(row.try_get("to_state")?)?,
        occurred_at: row.try_get("occurred_at")?,
        message: row.try_get("message")?,
        actor,
    })
}

fn parse_request_state(value: &str) -> Result<RequestState> {
    RequestState::parse(value).ok_or_else(|| Error::InvalidRequestState {
        value: value.to_owned(),
    })
}

fn parse_failure_reason(value: &str) -> Result<RequestFailureReason> {
    RequestFailureReason::parse(value).ok_or_else(|| Error::InvalidFailureReason {
        value: value.to_owned(),
    })
}

fn validate_match_candidates(candidates: &[RequestMatchCandidateInput]) -> Result<()> {
    if candidates.is_empty() {
        return Err(Error::NoMatchCandidates);
    }

    let mut seen = HashSet::with_capacity(candidates.len());
    for candidate in candidates {
        if candidate.title.trim().is_empty() {
            return Err(Error::InvalidMatchCandidate {
                canonical_identity_id: candidate.canonical_identity_id,
                reason: "title is empty",
            });
        }
        if candidate.year.is_some_and(|year| year <= 0) {
            return Err(Error::InvalidMatchCandidate {
                canonical_identity_id: candidate.canonical_identity_id,
                reason: "year must be positive",
            });
        }
        if !candidate.popularity.is_finite() || candidate.popularity < 0.0 {
            return Err(Error::InvalidMatchCandidate {
                canonical_identity_id: candidate.canonical_identity_id,
                reason: "popularity must be finite and non-negative",
            });
        }
        if !seen.insert(candidate.canonical_identity_id) {
            return Err(Error::DuplicateMatchCandidate {
                canonical_identity_id: candidate.canonical_identity_id,
            });
        }
    }

    Ok(())
}

fn score_match_candidates(
    raw_query: &str,
    candidates: Vec<RequestMatchCandidateInput>,
) -> Vec<RequestMatchCandidate> {
    let query_title = title_without_year(raw_query);
    let query_year = extract_year(raw_query);
    let max_popularity = candidates
        .iter()
        .map(|candidate| candidate.popularity)
        .fold(0.0, f64::max);

    let mut scored = candidates
        .into_iter()
        .map(|candidate| {
            let title_score = title_similarity(&query_title, &candidate.title);
            let year_score = year_match_score(query_year, candidate.year);
            let popularity_score = if max_popularity > 0.0 {
                candidate.popularity / max_popularity
            } else {
                0.0
            };
            let score =
                (title_score * 0.80 + year_score * 0.15 + popularity_score * 0.05).clamp(0.0, 1.0);

            RequestMatchCandidate {
                rank: 0,
                canonical_identity_id: candidate.canonical_identity_id,
                title: candidate.title,
                year: candidate.year,
                popularity: candidate.popularity,
                score,
            }
        })
        .collect::<Vec<_>>();

    scored.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| right.popularity.total_cmp(&left.popularity))
            .then_with(|| left.title.cmp(&right.title))
            .then_with(|| left.canonical_identity_id.cmp(&right.canonical_identity_id))
    });

    for (index, candidate) in scored.iter_mut().enumerate() {
        if index >= u32::MAX as usize {
            candidate.rank = u32::MAX;
        } else {
            candidate.rank = index as u32 + 1;
        };
    }

    scored
}

fn extract_year(value: &str) -> Option<i32> {
    let bytes = value.as_bytes();
    let mut index = 0usize;
    while index < bytes.len() {
        if !bytes[index].is_ascii_digit() {
            index += 1;
            continue;
        }

        let start = index;
        while index < bytes.len() && bytes[index].is_ascii_digit() {
            index += 1;
        }
        let end = index;
        if end - start != 4 || !year_position_looks_intentional(value, start, end) {
            continue;
        }

        if let Ok(year) = value[start..end].parse::<i32>()
            && (1888..=2100).contains(&year)
        {
            return Some(year);
        }
    }

    None
}

fn year_position_looks_intentional(value: &str, start: usize, end: usize) -> bool {
    let previous = value[..start].chars().next_back();
    let next = value[end..].chars().next();
    let parenthesized = previous == Some('(') && next == Some(')');
    let trailing = next.is_none()
        && previous.is_some_and(|character| {
            character.is_ascii_whitespace() || matches!(character, '-' | '/' | ':')
        });

    parenthesized || trailing
}

fn title_without_year(value: &str) -> String {
    let mut title = value.to_owned();
    if let Some(year) = extract_year(value) {
        title = title.replace(&format!("({year})"), " ");
        title = title.replace(&year.to_string(), " ");
    }
    title
}

fn year_match_score(query_year: Option<i32>, candidate_year: Option<i32>) -> f64 {
    match (query_year, candidate_year) {
        (Some(query), Some(candidate)) if query == candidate => 1.0,
        (Some(query), Some(candidate)) if (query - candidate).abs() == 1 => 0.65,
        (Some(_), Some(_)) => 0.0,
        (Some(_), None) => 0.35,
        (None, _) => 0.5,
    }
}

fn title_similarity(left: &str, right: &str) -> f64 {
    let left_tokens = normalized_tokens(left);
    let right_tokens = normalized_tokens(right);
    if left_tokens.is_empty() || right_tokens.is_empty() {
        return 0.0;
    }
    if left_tokens == right_tokens {
        return 1.0;
    }

    let mut unmatched = right_tokens.clone();
    let mut matches = 0usize;
    for token in &left_tokens {
        if let Some(index) = unmatched.iter().position(|candidate| candidate == token) {
            unmatched.swap_remove(index);
            matches += 1;
        }
    }

    (2.0 * matches as f64) / (left_tokens.len() + right_tokens.len()) as f64
}

fn normalized_tokens(value: &str) -> Vec<String> {
    value
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|part| !part.is_empty())
        .map(str::to_ascii_lowercase)
        .collect()
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
                NewRequest::anonymous("Inception (2010)")
                    .with_actor(RequestEventActor::User(user_id))
                    .with_message("requested by user"),
            )
            .await?;

        assert_eq!(detail.request.requester, RequestRequester::Anonymous);
        assert_eq!(detail.request.target.raw_query, "Inception (2010)");
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
            .create(
                NewRequest::anonymous("Inception (2010)")
                    .with_actor(RequestEventActor::System)
                    .with_message("accepted"),
            )
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
        let first = service
            .create(NewRequest::anonymous("first").with_message("first"))
            .await?;
        let second = service
            .create(NewRequest::anonymous("second").with_message("second"))
            .await?;

        let page = service.list(RequestListQuery::new()).await?;

        assert_eq!(page.requests.len(), 2);
        assert_eq!(page.limit, REQUEST_LIST_DEFAULT_LIMIT);
        assert_eq!(page.offset, 0);
        assert_eq!(page.next_offset, None);
        assert_eq!(page.requests[0].id, first.request.id);
        assert_eq!(page.requests[1].id, second.request.id);
        assert_eq!(page.requests[0].state, RequestState::Pending);
        assert_eq!(page.requests[1].state, RequestState::Pending);

        Ok(())
    }

    #[tokio::test]
    async fn request_model_fields_round_trip_through_crud()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = kino_db::test_db().await?;
        let service = RequestService::new(db);
        let canonical_identity_id = Id::new();
        let plan_id = Id::new();

        let created = service
            .create(
                NewRequest::anonymous("Inception (2010)")
                    .with_requester(RequestRequester::System)
                    .with_actor(RequestEventActor::System)
                    .with_message("accepted"),
            )
            .await?;

        assert_eq!(created.request.requester, RequestRequester::System);
        assert_eq!(created.request.target.raw_query, "Inception (2010)");
        assert_eq!(created.request.target.canonical_identity_id, None);
        assert_eq!(created.request.plan_id, None);

        let updated = service
            .update_model(
                created.request.id,
                RequestModelUpdate {
                    canonical_identity_id: Some(canonical_identity_id),
                    plan_id: Some(plan_id),
                },
            )
            .await?;
        let loaded = service.get(created.request.id).await?;
        let listed = service.list(RequestListQuery::new()).await?;

        assert_eq!(
            updated.request.target.canonical_identity_id,
            Some(canonical_identity_id)
        );
        assert_eq!(updated.request.plan_id, Some(plan_id));
        assert_eq!(loaded.request, updated.request);
        assert_eq!(listed.requests.len(), 1);
        assert_eq!(listed.requests[0], updated.request);

        Ok(())
    }

    #[tokio::test]
    async fn high_confidence_match_resolves_request()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = kino_db::test_db().await?;
        let service = RequestService::new(db);
        let winner_id = Id::new();
        let created = service
            .create(NewRequest::anonymous("Inception (2010)"))
            .await?;

        let detail = service
            .resolve_matches(
                created.request.id,
                vec![
                    candidate(winner_id, "Inception", Some(2010), 80.0),
                    candidate(Id::new(), "Interstellar", Some(2014), 70.0),
                ],
                Some(RequestEventActor::System),
                Some("matched canonical media"),
            )
            .await?;

        assert_eq!(detail.request.state, RequestState::Resolved);
        assert_eq!(detail.request.target.canonical_identity_id, Some(winner_id));
        assert!(detail.candidates.is_empty());
        assert_eq!(detail.status_events.len(), 2);
        assert_eq!(detail.status_events[1].to_state, RequestState::Resolved);

        Ok(())
    }

    #[tokio::test]
    async fn low_confidence_match_parks_request_with_ranked_candidates()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = kino_db::test_db().await?;
        let service = RequestService::new(db);
        let newer_id = Id::new();
        let older_id = Id::new();
        let created = service.create(NewRequest::anonymous("Dune")).await?;

        let detail = service
            .resolve_matches(
                created.request.id,
                vec![
                    candidate(older_id, "Dune", Some(1984), 60.0),
                    candidate(newer_id, "Dune", Some(2021), 90.0),
                    candidate(Id::new(), "Dune World", Some(2021), 10.0),
                ],
                Some(RequestEventActor::System),
                Some("needs user choice"),
            )
            .await?;
        let loaded = service.get(created.request.id).await?;

        assert_eq!(detail.request.state, RequestState::NeedsDisambiguation);
        assert_eq!(detail.request.target.canonical_identity_id, None);
        assert_eq!(detail.candidates.len(), 3);
        assert_eq!(detail.candidates[0].rank, 1);
        assert_eq!(detail.candidates[0].canonical_identity_id, newer_id);
        assert_eq!(detail.candidates[1].rank, 2);
        assert_eq!(detail.candidates[1].canonical_identity_id, older_id);
        assert_eq!(loaded.candidates, detail.candidates);
        assert_eq!(detail.status_events.len(), 2);
        assert_eq!(
            detail.status_events[1].to_state,
            RequestState::NeedsDisambiguation
        );

        Ok(())
    }

    #[tokio::test]
    async fn list_filters_by_state() -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = kino_db::test_db().await?;
        let service = RequestService::new(db);
        let pending = service
            .create(NewRequest::anonymous("pending").with_message("pending"))
            .await?;
        let resolved = service
            .create(NewRequest::anonymous("resolved").with_message("resolved"))
            .await?;
        service
            .transition(resolved.request.id, RequestTransition::Resolve, None, None)
            .await?;

        let page = service
            .list(RequestListQuery::new().with_state(RequestState::Pending))
            .await?;

        assert_eq!(page.requests.len(), 1);
        assert_eq!(page.requests[0].id, pending.request.id);
        assert_eq!(page.requests[0].state, RequestState::Pending);

        Ok(())
    }

    #[tokio::test]
    async fn list_uses_offset_pagination() -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = kino_db::test_db().await?;
        let service = RequestService::new(db);
        let first = service
            .create(NewRequest::anonymous("first").with_message("first"))
            .await?;
        let second = service
            .create(NewRequest::anonymous("second").with_message("second"))
            .await?;
        let third = service
            .create(NewRequest::anonymous("third").with_message("third"))
            .await?;

        let first_page = service
            .list(RequestListQuery::new().with_limit(2).with_offset(0))
            .await?;
        let second_page = service
            .list(RequestListQuery::new().with_limit(2).with_offset(2))
            .await?;

        assert_eq!(
            first_page
                .requests
                .iter()
                .map(|request| request.id)
                .collect::<Vec<_>>(),
            vec![first.request.id, second.request.id]
        );
        assert_eq!(first_page.next_offset, Some(2));
        assert_eq!(
            second_page
                .requests
                .iter()
                .map(|request| request.id)
                .collect::<Vec<_>>(),
            vec![third.request.id]
        );
        assert_eq!(second_page.next_offset, None);

        Ok(())
    }

    #[tokio::test]
    async fn list_rejects_invalid_limit() -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = kino_db::test_db().await?;
        let service = RequestService::new(db);

        let err = match service.list(RequestListQuery::new().with_limit(0)).await {
            Ok(_) => panic!("invalid list limit was accepted"),
            Err(err) => err,
        };

        assert!(matches!(
            err,
            Error::InvalidListLimit {
                limit: 0,
                max: REQUEST_LIST_MAX_LIMIT
            }
        ));

        Ok(())
    }

    #[tokio::test]
    async fn invalid_transition_does_not_write_event()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = kino_db::test_db().await?;
        let service = RequestService::new(db);
        let created = service
            .create(NewRequest::anonymous("Inception (2010)"))
            .await?;

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
        let created = service
            .create(NewRequest::anonymous("Inception (2010)"))
            .await?;
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
        let created = service
            .create(NewRequest::anonymous("Inception (2010)"))
            .await?;

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
            RequestState::NeedsDisambiguation,
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
            (
                RequestState::NeedsDisambiguation,
                RequestTransition::Resolve,
            ),
            (
                RequestState::NeedsDisambiguation,
                RequestTransition::Fail(RequestFailureReason::NoProviderAccepted),
            ),
            (RequestState::NeedsDisambiguation, RequestTransition::Cancel),
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
        let created = service
            .create(NewRequest::anonymous("Inception (2010)"))
            .await?;
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
        let created = service
            .create(NewRequest::anonymous("Inception (2010)"))
            .await?;
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

    fn candidate(
        canonical_identity_id: Id,
        title: &str,
        year: Option<i32>,
        popularity: f64,
    ) -> RequestMatchCandidateInput {
        RequestMatchCandidateInput {
            canonical_identity_id,
            title: title.to_owned(),
            year,
            popularity,
        }
    }
}
