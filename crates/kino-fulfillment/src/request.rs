//! Request status, matching, and state transitions.

use std::{collections::HashSet, fmt, time::Duration};

use kino_core::{
    CanonicalIdentityId, CanonicalIdentitySource, Id, Request, RequestFailureReason,
    RequestRequester, RequestState, Timestamp,
};
use kino_db::Db;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    ingestion::{ExpectedProbedFile, ProbedFile, ProbedFileMatch, match_probed_file},
    planning::{
        ComputedFulfillmentPlan, FulfillmentLibraryState, FulfillmentPlanningInput,
        compute_fulfillment_plan,
    },
    provider::{
        ConfiguredFulfillmentProvider, FulfillmentProviderError, ProviderRetryPolicy,
        ProviderSelectionPlan,
    },
};

mod store;

use store::{NewFulfillmentPlanRecord, NewIdentityVersion, NewStatusEvent, RequestStore};

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

    /// Transcode handoff failed during ingestion.
    #[error("transcode handoff failed: {0}")]
    Transcode(#[from] kino_transcode::Error),

    /// Library filesystem placement failed during ingestion.
    #[error("library placement failed: {0}")]
    Library(#[from] kino_library::Error),

    /// Canonical ingestion was requested without a layout writer.
    #[error("canonical layout writer is not configured")]
    CanonicalLayoutNotConfigured,

    /// Probed-file verification cannot run from the current request state.
    #[error("request probed-file verification from {from} is invalid")]
    InvalidProbedFileMatchState {
        /// Current request state.
        from: RequestState,
    },

    /// Probed-file verification requires a resolved request identity.
    #[error("request {request_id} probed-file verification requires a canonical identity")]
    ProbedFileMatchRequiresIdentity {
        /// Request missing a canonical identity.
        request_id: Id,
    },

    /// Probed-file expectations do not match the request identity.
    #[error(
        "request {request_id} probed-file expectation {expected} does not match request identity {actual}"
    )]
    ProbedFileIdentityMismatch {
        /// Request id.
        request_id: Id,
        /// Expected identity supplied to verification.
        expected: CanonicalIdentityId,
        /// Current request identity.
        actual: CanonicalIdentityId,
    },

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

    /// Resolution transitions require an explicit canonical identity action.
    #[error("request transition {transition} requires canonical identity resolution")]
    ResolutionRequiresIdentity {
        /// Requested resolution transition.
        transition: RequestTransition,
    },

    /// A persisted identity provenance is outside the known enum.
    #[error("request identity provenance {value} is invalid")]
    InvalidIdentityProvenance {
        /// Persisted provenance value.
        value: String,
    },

    /// A persisted identity version is outside the accepted range.
    #[error("request identity version {version} is invalid")]
    InvalidIdentityVersion {
        /// Persisted version value.
        version: i64,
    },

    /// A persisted fulfillment plan decision is outside the known enum.
    #[error("request fulfillment plan decision {value} is invalid")]
    InvalidFulfillmentPlanDecision {
        /// Persisted decision value.
        value: String,
    },

    /// A persisted fulfillment plan version is outside the accepted range.
    #[error("request fulfillment plan version {version} is invalid")]
    InvalidFulfillmentPlanVersion {
        /// Persisted version value.
        version: i64,
    },

    /// Plan persistence cannot run from the current request state.
    #[error("request fulfillment planning from {from} is invalid")]
    InvalidFulfillmentPlanState {
        /// Current request state.
        from: RequestState,
    },

    /// A fulfillment plan summary is empty after trimming whitespace.
    #[error("request fulfillment plan summary is empty")]
    EmptyFulfillmentPlanSummary,

    /// Fulfillment planning requires a resolved canonical identity.
    #[error("request {request_id} fulfillment planning requires a canonical identity")]
    FulfillmentPlanningRequiresIdentity {
        /// Request missing a canonical identity.
        request_id: Id,
    },

    /// A configured fulfillment provider is invalid.
    #[error("configured fulfillment provider {provider_id:?} is invalid: {reason}")]
    InvalidFulfillmentProvider {
        /// Stable configured provider id, when present.
        provider_id: String,
        /// Human-readable validation failure.
        reason: &'static str,
    },

    /// A configured fulfillment provider id appears more than once.
    #[error("configured fulfillment provider {provider_id} is duplicated")]
    DuplicateFulfillmentProvider {
        /// Duplicated configured provider id.
        provider_id: String,
    },

    /// A candidate appears more than once in one scoring request.
    #[error("request match candidate {canonical_identity_id} is duplicated")]
    DuplicateMatchCandidate {
        /// Duplicated canonical identity id.
        canonical_identity_id: CanonicalIdentityId,
    },

    /// A candidate field is invalid.
    #[error("request match candidate {canonical_identity_id} is invalid: {reason}")]
    InvalidMatchCandidate {
        /// Candidate identity id.
        canonical_identity_id: CanonicalIdentityId,
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
    /// Move from planning or ingesting to satisfied.
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
            Self::Satisfy => matches!(from, RequestState::Planning | RequestState::Ingesting),
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
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

/// Provenance for a request canonical identity version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RequestIdentityProvenance {
    /// Resolver match scoring selected the identity.
    MatchScoring,
    /// A user or admin deliberately selected the identity.
    Manual,
}

impl RequestIdentityProvenance {
    fn as_str(self) -> &'static str {
        match self {
            Self::MatchScoring => "match_scoring",
            Self::Manual => "manual",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "match_scoring" => Some(Self::MatchScoring),
            "manual" => Some(Self::Manual),
            _ => None,
        }
    }

    fn identity_source(self) -> CanonicalIdentitySource {
        match self {
            Self::MatchScoring => CanonicalIdentitySource::MatchScoring,
            Self::Manual => CanonicalIdentitySource::Manual,
        }
    }
}

/// Versioned canonical identity selected for a request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct RequestIdentityVersion {
    /// Request id.
    pub request_id: Id,
    /// Monotonic per-request identity version.
    pub version: u32,
    /// Selected canonical identity id.
    pub canonical_identity_id: CanonicalIdentityId,
    /// Reason the identity was selected.
    pub provenance: RequestIdentityProvenance,
    /// Status event written by the same resolution transition.
    pub status_event_id: Option<Id>,
    /// Version creation timestamp.
    pub created_at: Timestamp,
    /// Optional actor responsible for the identity choice.
    pub actor: Option<RequestEventActor>,
}

/// Top-level decision produced by fulfillment planning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum FulfillmentPlanDecision {
    /// The requested media already exists in the library.
    AlreadySatisfied,
    /// A configured provider must produce candidate media.
    NeedsProvider,
    /// The user must provide more input before fulfillment can proceed.
    NeedsUserInput,
}

impl FulfillmentPlanDecision {
    fn as_str(self) -> &'static str {
        match self {
            Self::AlreadySatisfied => "already_satisfied",
            Self::NeedsProvider => "needs_provider",
            Self::NeedsUserInput => "needs_user_input",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "already_satisfied" => Some(Self::AlreadySatisfied),
            "needs_provider" => Some(Self::NeedsProvider),
            "needs_user_input" => Some(Self::NeedsUserInput),
            _ => None,
        }
    }
}

/// Versioned fulfillment plan recorded for a request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct FulfillmentPlan {
    /// Plan id.
    pub id: Id,
    /// Request id.
    pub request_id: Id,
    /// Monotonic per-request plan version.
    pub version: u32,
    /// Planner decision.
    pub decision: FulfillmentPlanDecision,
    /// Human-readable reason for the decision.
    pub summary: String,
    /// Status event associated with planning, when applicable.
    pub status_event_id: Option<Id>,
    /// Version creation timestamp.
    pub created_at: Timestamp,
    /// Optional actor responsible for the plan.
    pub actor: Option<RequestEventActor>,
}

/// Data required to persist a computed fulfillment plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NewFulfillmentPlan<'a> {
    /// Planner decision.
    pub decision: FulfillmentPlanDecision,
    /// Human-readable reason for the decision.
    pub summary: &'a str,
    /// Optional actor responsible for the plan.
    pub actor: Option<RequestEventActor>,
}

impl<'a> NewFulfillmentPlan<'a> {
    /// Construct a new fulfillment plan input.
    pub const fn new(decision: FulfillmentPlanDecision, summary: &'a str) -> Self {
        Self {
            decision,
            summary,
            actor: None,
        }
    }

    /// Set the plan actor.
    pub const fn with_actor(self, actor: RequestEventActor) -> Self {
        Self {
            actor: Some(actor),
            ..self
        }
    }
}

/// Candidate supplied by a resolver before request-level scoring.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct RequestMatchCandidateInput {
    /// Candidate canonical identity id.
    pub canonical_identity_id: CanonicalIdentityId,
    /// Candidate display title.
    pub title: String,
    /// Candidate release or first-air year when known.
    pub year: Option<i32>,
    /// Provider popularity value used only as a tiebreaker.
    pub popularity: f64,
}

/// Ranked candidate stored when a request needs disambiguation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct RequestMatchCandidate {
    /// Request-local rank starting at one.
    pub rank: u32,
    /// Candidate canonical identity id.
    pub canonical_identity_id: CanonicalIdentityId,
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct RequestDetail {
    /// Current request projection.
    pub request: Request,
    /// Current fulfillment plan, if planning has run.
    pub current_plan: Option<FulfillmentPlan>,
    /// Fulfillment plan history ordered by version.
    pub plan_history: Vec<FulfillmentPlan>,
    /// Status events ordered by occurrence.
    pub status_events: Vec<RequestStatusEvent>,
    /// Canonical identity history ordered by version.
    pub identity_versions: Vec<RequestIdentityVersion>,
    /// Ranked candidates when the request needs disambiguation.
    pub candidates: Vec<RequestMatchCandidate>,
}

/// Result of planning provider selection for a request.
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderSelectionPlanningResult {
    /// Provider selection decision and ranked candidates.
    pub selection: ProviderSelectionPlan,
    /// Request detail after persisting the fulfillment plan.
    pub detail: RequestDetail,
}

/// Result of recording a provider error against a request.
#[derive(Debug, Clone, PartialEq)]
pub enum ProviderErrorHandlingResult {
    /// The provider error was transient and another attempt should be scheduled.
    Retry {
        /// Provider that returned the transient error.
        provider_id: String,
        /// Number of consecutive failures including the one just recorded.
        failure_count: u32,
        /// Delay to wait before the next attempt.
        retry_after: Duration,
        /// Human-readable status context.
        message: String,
    },
    /// The request was transitioned to failed.
    Failed {
        /// Request detail after failure transition.
        detail: Box<RequestDetail>,
    },
}

/// Result of verifying a probed file against its request.
#[derive(Debug, Clone, PartialEq)]
pub enum ProbedFileVerificationResult {
    /// The file matched and ingestion may continue.
    Matched {
        /// Request detail after verification.
        detail: Box<RequestDetail>,
        /// Matcher result.
        checked: ProbedFileMatch,
    },

    /// The file mismatched and the request was failed.
    Mismatched {
        /// Request detail after the failure transition.
        detail: Box<RequestDetail>,
        /// Matcher result.
        checked: ProbedFileMatch,
    },
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
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

/// Mutable non-identity request model links.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RequestModelUpdate {
    /// Canonical identity writes are rejected; use resolution APIs instead.
    pub canonical_identity_id: Option<CanonicalIdentityId>,
    /// Fulfillment plan id.
    pub plan_id: Option<Id>,
}

struct CurrentPlanInput<'a> {
    request_id: Id,
    decision: FulfillmentPlanDecision,
    summary: &'a str,
    status_event_id: Option<Id>,
    created_at: Timestamp,
    actor: Option<RequestEventActor>,
}

/// Internal request API backed by `kino-db`.
#[derive(Clone)]
pub struct RequestService {
    store: RequestStore,
}

impl RequestService {
    /// Construct a request service from an open database handle.
    pub fn new(db: Db) -> Self {
        Self {
            store: RequestStore::new(db),
        }
    }

    /// Create a new pending request.
    pub async fn create(&self, request: NewRequest<'_>) -> Result<RequestDetail> {
        let id = Id::new();
        let event_id = Id::new();
        let now = Timestamp::now();
        self.store
            .create_pending(
                id,
                request,
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
        self.get(id).await
    }

    /// Read a request with its full status-event log.
    pub async fn get(&self, id: Id) -> Result<RequestDetail> {
        self.store.get(id).await
    }

    /// List requests using the default projection ordered by creation time.
    pub async fn list(&self, query: RequestListQuery) -> Result<RequestListPage> {
        query.validate()?;

        let rows = self.store.list(query).await?;
        let has_more = rows.len() > query.limit as usize;
        let requests = rows.into_iter().take(query.limit as usize).collect();

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
        if update.canonical_identity_id.is_some() {
            return Err(Error::ResolutionRequiresIdentity {
                transition: RequestTransition::Resolve,
            });
        }

        let rows_affected = self
            .store
            .update_model_links(request_id, update.plan_id, Timestamp::now())
            .await?;
        if rows_affected == 0 {
            return Err(Error::RequestNotFound { id: request_id });
        }

        self.get(request_id).await
    }

    /// Persist a newly computed fulfillment plan and make it current.
    pub async fn record_plan(
        &self,
        request_id: Id,
        plan: NewFulfillmentPlan<'_>,
    ) -> Result<RequestDetail> {
        let summary = validate_plan_summary(plan.summary)?;
        let mut tx = self.store.begin().await?;
        let current = self.store.request_in_tx(&mut tx, request_id).await?;
        if current.state != RequestState::Planning {
            return Err(Error::InvalidFulfillmentPlanState {
                from: current.state,
            });
        }

        let now = Timestamp::now();
        self.insert_current_plan(
            &mut tx,
            CurrentPlanInput {
                request_id,
                decision: plan.decision,
                summary,
                status_event_id: None,
                created_at: now,
                actor: plan.actor,
            },
        )
        .await?;

        tx.commit().await?;
        self.get(request_id).await
    }

    async fn insert_current_plan(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        plan: CurrentPlanInput<'_>,
    ) -> Result<Id> {
        let plan_id = Id::new();
        let version = self.store.next_plan_version(tx, plan.request_id).await?;
        self.store
            .insert_fulfillment_plan(
                tx,
                NewFulfillmentPlanRecord {
                    id: plan_id,
                    request_id: plan.request_id,
                    version,
                    decision: plan.decision,
                    summary: plan.summary,
                    status_event_id: plan.status_event_id,
                    created_at: plan.created_at,
                    actor: plan.actor,
                },
            )
            .await?;
        self.store
            .update_current_plan(tx, plan.request_id, plan_id, plan.created_at)
            .await?;

        Ok(plan_id)
    }

    /// Select a provider for a planning request and persist the resulting plan.
    pub async fn plan_provider_selection(
        &self,
        request_id: Id,
        providers: &[ConfiguredFulfillmentProvider<'_>],
        rejected_provider_ids: &[&str],
        actor: Option<RequestEventActor>,
    ) -> Result<ProviderSelectionPlanningResult> {
        let mut tx = self.store.begin().await?;
        let current = self.store.request_in_tx(&mut tx, request_id).await?;
        if current.state != RequestState::Planning {
            return Err(Error::InvalidFulfillmentPlanState {
                from: current.state,
            });
        }

        let canonical_identity_id = current
            .target
            .canonical_identity_id
            .ok_or(Error::FulfillmentPlanningRequiresIdentity { request_id })?;
        let contains_resolved_identity = self
            .store
            .media_item_exists_for_identity(&mut tx, canonical_identity_id)
            .await?;
        let computed = compute_fulfillment_plan(
            FulfillmentPlanningInput::new(
                &current,
                FulfillmentLibraryState::new(contains_resolved_identity),
                providers,
            )
            .with_rejected_provider_ids(rejected_provider_ids),
        )?;
        let selection = computed.provider_selection_plan();
        let summary = validate_plan_summary(computed.summary())?;
        let now = Timestamp::now();
        let status_event_id =
            if matches!(computed, ComputedFulfillmentPlan::AlreadySatisfied { .. }) {
                let event_id = Id::new();
                self.store
                    .update_state(&mut tx, request_id, RequestState::Satisfied, now, None)
                    .await?;
                self.store
                    .insert_status_event(
                        &mut tx,
                        NewStatusEvent {
                            id: event_id,
                            request_id,
                            from_state: Some(current.state),
                            to_state: RequestState::Satisfied,
                            occurred_at: now,
                            message: Some(summary),
                            actor,
                        },
                    )
                    .await?;
                Some(event_id)
            } else {
                None
            };

        self.insert_current_plan(
            &mut tx,
            CurrentPlanInput {
                request_id,
                decision: computed.decision(),
                summary,
                status_event_id,
                created_at: now,
                actor,
            },
        )
        .await?;

        tx.commit().await?;
        let detail = self.get(request_id).await?;

        Ok(ProviderSelectionPlanningResult { selection, detail })
    }

    /// Apply provider error semantics to a request.
    pub async fn handle_provider_error(
        &self,
        request_id: Id,
        provider_id: &str,
        error: FulfillmentProviderError,
        failure_count: u32,
        retry_policy: ProviderRetryPolicy,
        actor: Option<RequestEventActor>,
    ) -> Result<ProviderErrorHandlingResult> {
        let provider_id = validate_provider_error_provider_id(provider_id)?;
        let message = error.status_message(provider_id);
        if error.is_transient()
            && let Some(retry_after) = retry_policy.retry_after(failure_count)
        {
            return Ok(ProviderErrorHandlingResult::Retry {
                provider_id: provider_id.to_owned(),
                failure_count,
                retry_after,
                message,
            });
        }

        let detail = self
            .transition(
                request_id,
                RequestTransition::Fail(RequestFailureReason::AcquisitionFailed),
                actor,
                Some(message.as_str()),
            )
            .await?;

        Ok(ProviderErrorHandlingResult::Failed {
            detail: Box::new(detail),
        })
    }

    /// Verify a probed provider file against an ingesting request.
    pub async fn verify_probed_file(
        &self,
        request_id: Id,
        expected: ExpectedProbedFile,
        probed: ProbedFile,
        actor: Option<RequestEventActor>,
    ) -> Result<ProbedFileVerificationResult> {
        let current = self.get(request_id).await?;
        if current.request.state != RequestState::Ingesting {
            return Err(Error::InvalidProbedFileMatchState {
                from: current.request.state,
            });
        }

        let actual_identity = current
            .request
            .target
            .canonical_identity_id
            .ok_or(Error::ProbedFileMatchRequiresIdentity { request_id })?;
        if actual_identity != expected.canonical_identity_id {
            return Err(Error::ProbedFileIdentityMismatch {
                request_id,
                expected: expected.canonical_identity_id,
                actual: actual_identity,
            });
        }

        let checked = match_probed_file(&expected, &probed);
        if checked.is_match() {
            return Ok(ProbedFileVerificationResult::Matched {
                detail: Box::new(current),
                checked,
            });
        }

        let message = format!("probed file mismatch: {}", checked.summary());
        let detail = self
            .transition(
                request_id,
                RequestTransition::Fail(RequestFailureReason::IngestFailed),
                actor,
                Some(&message),
            )
            .await?;

        Ok(ProbedFileVerificationResult::Mismatched {
            detail: Box::new(detail),
            checked,
        })
    }

    /// Resolve a pending or disambiguated request to a canonical identity.
    pub async fn resolve_identity(
        &self,
        request_id: Id,
        canonical_identity_id: CanonicalIdentityId,
        provenance: RequestIdentityProvenance,
        actor: Option<RequestEventActor>,
        message: Option<&str>,
    ) -> Result<RequestDetail> {
        self.resolve_identity_with_transition(
            request_id,
            canonical_identity_id,
            provenance,
            RequestTransition::Resolve,
            actor,
            message,
        )
        .await
    }

    /// Deliberately replace a post-resolution canonical identity.
    pub async fn re_resolve_identity(
        &self,
        request_id: Id,
        canonical_identity_id: CanonicalIdentityId,
        actor: Option<RequestEventActor>,
        message: Option<&str>,
    ) -> Result<RequestDetail> {
        self.resolve_identity_with_transition(
            request_id,
            canonical_identity_id,
            RequestIdentityProvenance::Manual,
            RequestTransition::ReResolve,
            actor,
            message,
        )
        .await
    }

    async fn resolve_identity_with_transition(
        &self,
        request_id: Id,
        canonical_identity_id: CanonicalIdentityId,
        provenance: RequestIdentityProvenance,
        transition: RequestTransition,
        actor: Option<RequestEventActor>,
        message: Option<&str>,
    ) -> Result<RequestDetail> {
        let mut tx = self.store.begin().await?;
        let current = self.store.request_in_tx(&mut tx, request_id).await?;
        let default_next = transition.to_state();
        if !transition.can_apply_from(current.state) {
            return Err(Error::InvalidTransition {
                from: current.state,
                transition,
                to: default_next,
            });
        }

        let now = Timestamp::now();
        let event_id = Id::new();
        let version = self
            .store
            .next_identity_version(&mut tx, request_id)
            .await?;
        self.store
            .ensure_canonical_identity(
                &mut tx,
                canonical_identity_id,
                provenance.identity_source(),
                now,
            )
            .await?;
        let already_satisfied = self
            .store
            .media_item_exists_for_identity(&mut tx, canonical_identity_id)
            .await?;
        let next = if already_satisfied {
            RequestState::Satisfied
        } else {
            default_next
        };
        self.store
            .update_resolved(&mut tx, request_id, canonical_identity_id, next, now)
            .await?;
        self.store
            .insert_status_event(
                &mut tx,
                NewStatusEvent {
                    id: event_id,
                    request_id,
                    from_state: Some(current.state),
                    to_state: next,
                    occurred_at: now,
                    message,
                    actor,
                },
            )
            .await?;
        self.store
            .insert_identity_version(
                &mut tx,
                NewIdentityVersion {
                    request_id,
                    version,
                    canonical_identity_id,
                    provenance,
                    status_event_id: Some(event_id),
                    created_at: now,
                    actor,
                },
            )
            .await?;
        if already_satisfied {
            let summary = format!("library already contains {canonical_identity_id}");
            let summary = validate_plan_summary(summary.as_str())?;
            self.insert_current_plan(
                &mut tx,
                CurrentPlanInput {
                    request_id,
                    decision: FulfillmentPlanDecision::AlreadySatisfied,
                    summary,
                    status_event_id: Some(event_id),
                    created_at: now,
                    actor,
                },
            )
            .await?;
        }

        tx.commit().await?;
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

        let mut tx = self.store.begin().await?;
        let current = self.store.request_in_tx(&mut tx, request_id).await?;
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

        self.store
            .delete_match_candidates(&mut tx, request_id)
            .await?;

        if auto_resolve {
            let already_satisfied = self
                .store
                .media_item_exists_for_identity(&mut tx, top.canonical_identity_id)
                .await?;
            let next = if already_satisfied {
                RequestState::Satisfied
            } else {
                RequestState::Resolved
            };
            if !RequestTransition::Resolve.can_apply_from(current.state) {
                return Err(Error::InvalidTransition {
                    from: current.state,
                    transition: RequestTransition::Resolve,
                    to: next,
                });
            }

            let event_id = Id::new();
            let version = self
                .store
                .next_identity_version(&mut tx, request_id)
                .await?;
            self.store
                .ensure_canonical_identity(
                    &mut tx,
                    top.canonical_identity_id,
                    CanonicalIdentitySource::MatchScoring,
                    now,
                )
                .await?;
            self.store
                .update_resolved(&mut tx, request_id, top.canonical_identity_id, next, now)
                .await?;
            self.store
                .insert_status_event(
                    &mut tx,
                    NewStatusEvent {
                        id: event_id,
                        request_id,
                        from_state: Some(current.state),
                        to_state: next,
                        occurred_at: now,
                        message,
                        actor,
                    },
                )
                .await?;
            self.store
                .insert_identity_version(
                    &mut tx,
                    NewIdentityVersion {
                        request_id,
                        version,
                        canonical_identity_id: top.canonical_identity_id,
                        provenance: RequestIdentityProvenance::MatchScoring,
                        status_event_id: Some(event_id),
                        created_at: now,
                        actor,
                    },
                )
                .await?;
            if already_satisfied {
                let summary = format!("library already contains {}", top.canonical_identity_id);
                let summary = validate_plan_summary(summary.as_str())?;
                self.insert_current_plan(
                    &mut tx,
                    CurrentPlanInput {
                        request_id,
                        decision: FulfillmentPlanDecision::AlreadySatisfied,
                        summary,
                        status_event_id: Some(event_id),
                        created_at: now,
                        actor,
                    },
                )
                .await?;
            }
        } else {
            for candidate in scored.iter().take(REQUEST_MATCH_CANDIDATE_LIMIT) {
                self.store
                    .ensure_canonical_identity(
                        &mut tx,
                        candidate.canonical_identity_id,
                        CanonicalIdentitySource::MatchScoring,
                        now,
                    )
                    .await?;
                self.store
                    .insert_match_candidate(&mut tx, request_id, candidate, now)
                    .await?;
            }

            if current.state == RequestState::NeedsDisambiguation {
                self.store
                    .refresh_disambiguation(&mut tx, request_id, now)
                    .await?;
            } else {
                let next = RequestState::NeedsDisambiguation;

                self.store
                    .move_to_disambiguation(&mut tx, request_id, next, now)
                    .await?;
                self.store
                    .insert_status_event(
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
        if matches!(
            transition,
            RequestTransition::Resolve | RequestTransition::ReResolve
        ) {
            return Err(Error::ResolutionRequiresIdentity { transition });
        }

        let mut tx = self.store.begin().await?;
        let current = self.store.request_in_tx(&mut tx, request_id).await?;
        let next = transition.to_state();
        if !transition.can_apply_from(current.state) {
            return Err(Error::InvalidTransition {
                from: current.state,
                transition,
                to: next,
            });
        }

        let now = Timestamp::now();
        self.store
            .update_state(&mut tx, request_id, next, now, transition.failure_reason())
            .await?;

        self.store
            .insert_status_event(
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

fn validate_plan_summary(summary: &str) -> Result<&str> {
    let trimmed = summary.trim();
    if trimmed.is_empty() {
        return Err(Error::EmptyFulfillmentPlanSummary);
    }

    Ok(trimmed)
}

fn validate_provider_error_provider_id(provider_id: &str) -> Result<&str> {
    let trimmed = provider_id.trim();
    if trimmed.is_empty() {
        return Err(Error::InvalidFulfillmentProvider {
            provider_id: provider_id.to_owned(),
            reason: "id is empty",
        });
    }

    Ok(trimmed)
}

/// Rank resolver candidates for a request without mutating request state.
pub fn rank_match_candidates(
    raw_query: &str,
    candidates: Vec<RequestMatchCandidateInput>,
) -> Result<Vec<RequestMatchCandidate>> {
    validate_match_candidates(&candidates)?;
    Ok(score_match_candidates(raw_query, candidates))
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
    use crate::provider::{
        FulfillmentProviderCapabilities, FulfillmentProviderCapability, FulfillmentProviderError,
        ProviderRetryPolicy,
    };
    use kino_core::{CanonicalIdentityKind, TmdbId};
    use std::time::Duration;

    const MOVIE_PROVIDER: &[FulfillmentProviderCapability] =
        &[FulfillmentProviderCapability::MediaKind(
            CanonicalIdentityKind::Movie,
        )];
    const TV_PROVIDER: &[FulfillmentProviderCapability] =
        &[FulfillmentProviderCapability::MediaKind(
            CanonicalIdentityKind::TvSeries,
        )];
    const MOVIE_CAPS: FulfillmentProviderCapabilities<'static> =
        FulfillmentProviderCapabilities::new(MOVIE_PROVIDER);
    const TV_CAPS: FulfillmentProviderCapabilities<'static> =
        FulfillmentProviderCapabilities::new(TV_PROVIDER);

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
    async fn resolve_identity_updates_state_and_appends_event()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = kino_db::test_db().await?;
        let service = RequestService::new(db);
        let canonical_identity_id = identity(550);
        let created = service
            .create(
                NewRequest::anonymous("Inception (2010)")
                    .with_actor(RequestEventActor::System)
                    .with_message("accepted"),
            )
            .await?;

        let detail = service
            .resolve_identity(
                created.request.id,
                canonical_identity_id,
                RequestIdentityProvenance::Manual,
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
        assert_eq!(detail.identity_versions.len(), 1);
        assert_eq!(detail.identity_versions[0].version, 1);
        assert_eq!(
            detail.identity_versions[0].canonical_identity_id,
            canonical_identity_id
        );
        assert_eq!(
            detail.identity_versions[0].provenance,
            RequestIdentityProvenance::Manual
        );
        assert_eq!(
            detail.identity_versions[0].status_event_id,
            Some(detail.status_events[1].id)
        );

        Ok(())
    }

    #[tokio::test]
    async fn resolve_identity_short_circuits_existing_media_item()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = kino_db::test_db().await?;
        let service = RequestService::new(db.clone());
        let canonical_identity_id = identity(550);
        insert_media_item(&db, canonical_identity_id).await?;
        let created = service
            .create(NewRequest::anonymous("Inception (2010)"))
            .await?;

        let detail = service
            .resolve_identity(
                created.request.id,
                canonical_identity_id,
                RequestIdentityProvenance::Manual,
                Some(RequestEventActor::System),
                Some("matched canonical media"),
            )
            .await?;

        assert_eq!(detail.request.state, RequestState::Satisfied);
        assert_eq!(
            detail.request.target.canonical_identity_id,
            Some(canonical_identity_id)
        );
        assert_eq!(detail.status_events.len(), 2);
        assert_eq!(
            detail.status_events[1].from_state,
            Some(RequestState::Pending)
        );
        assert_eq!(detail.status_events[1].to_state, RequestState::Satisfied);
        assert_eq!(detail.plan_history.len(), 1);
        assert_eq!(
            detail.plan_history[0].decision,
            FulfillmentPlanDecision::AlreadySatisfied
        );
        assert_eq!(
            detail.plan_history[0].summary,
            "library already contains tmdb:movie:550"
        );
        assert_eq!(
            detail.plan_history[0].status_event_id,
            Some(detail.status_events[1].id)
        );
        assert_eq!(detail.current_plan, Some(detail.plan_history[0].clone()));

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
                    canonical_identity_id: None,
                    plan_id: Some(plan_id),
                },
            )
            .await?;
        let loaded = service.get(created.request.id).await?;
        let listed = service.list(RequestListQuery::new()).await?;

        assert_eq!(updated.request.target.canonical_identity_id, None);
        assert_eq!(updated.request.plan_id, Some(plan_id));
        assert_eq!(loaded.request, updated.request);
        assert_eq!(listed.requests.len(), 1);
        assert_eq!(listed.requests[0], updated.request);

        Ok(())
    }

    #[tokio::test]
    async fn request_model_update_rejects_identity_overwrite()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = kino_db::test_db().await?;
        let service = RequestService::new(db);
        let created = service
            .create(NewRequest::anonymous("Inception (2010)"))
            .await?;

        let err = match service
            .update_model(
                created.request.id,
                RequestModelUpdate {
                    canonical_identity_id: Some(identity(550)),
                    plan_id: None,
                },
            )
            .await
        {
            Ok(_) => panic!("unversioned identity update was accepted"),
            Err(err) => err,
        };

        assert!(matches!(
            err,
            Error::ResolutionRequiresIdentity {
                transition: RequestTransition::Resolve
            }
        ));

        let detail = service.get(created.request.id).await?;
        assert_eq!(detail.request.target.canonical_identity_id, None);
        assert!(detail.identity_versions.is_empty());

        Ok(())
    }

    #[tokio::test]
    async fn record_plan_creates_current_plan_and_history()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = kino_db::test_db().await?;
        let service = RequestService::new(db);
        let created = service
            .create(NewRequest::anonymous("Inception (2010)"))
            .await?;
        service
            .resolve_identity(
                created.request.id,
                identity(550),
                RequestIdentityProvenance::Manual,
                None,
                None,
            )
            .await?;
        service
            .transition(
                created.request.id,
                RequestTransition::StartPlanning,
                None,
                None,
            )
            .await?;

        let first = service
            .record_plan(
                created.request.id,
                NewFulfillmentPlan::new(
                    FulfillmentPlanDecision::NeedsProvider,
                    "watch-folder provider can satisfy this request",
                )
                .with_actor(RequestEventActor::System),
            )
            .await?;
        let second = service
            .record_plan(
                created.request.id,
                NewFulfillmentPlan::new(
                    FulfillmentPlanDecision::NeedsUserInput,
                    "provider candidates require a user choice",
                )
                .with_actor(RequestEventActor::System),
            )
            .await?;
        let loaded = service.get(created.request.id).await?;

        assert_eq!(first.plan_history.len(), 1);
        assert_eq!(first.plan_history[0].version, 1);
        assert_eq!(
            first.plan_history[0].decision,
            FulfillmentPlanDecision::NeedsProvider
        );
        assert_eq!(first.current_plan, Some(first.plan_history[0].clone()));
        assert_eq!(first.request.plan_id, Some(first.plan_history[0].id));

        assert_eq!(second.plan_history.len(), 2);
        assert_eq!(second.plan_history[0].version, 1);
        assert_eq!(second.plan_history[1].version, 2);
        assert_eq!(
            second.plan_history[1].decision,
            FulfillmentPlanDecision::NeedsUserInput
        );
        assert_eq!(second.current_plan, Some(second.plan_history[1].clone()));
        assert_eq!(second.request.plan_id, Some(second.plan_history[1].id));
        assert_eq!(loaded.plan_history, second.plan_history);
        assert_eq!(loaded.current_plan, second.current_plan);

        Ok(())
    }

    #[tokio::test]
    async fn record_plan_requires_planning_state_and_summary()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = kino_db::test_db().await?;
        let service = RequestService::new(db);
        let created = service
            .create(NewRequest::anonymous("Inception (2010)"))
            .await?;

        let state_err = match service
            .record_plan(
                created.request.id,
                NewFulfillmentPlan::new(FulfillmentPlanDecision::NeedsProvider, "provider"),
            )
            .await
        {
            Ok(_) => panic!("plan outside planning was accepted"),
            Err(err) => err,
        };

        assert!(matches!(
            state_err,
            Error::InvalidFulfillmentPlanState {
                from: RequestState::Pending
            }
        ));

        service
            .resolve_identity(
                created.request.id,
                identity(550),
                RequestIdentityProvenance::Manual,
                None,
                None,
            )
            .await?;
        service
            .transition(
                created.request.id,
                RequestTransition::StartPlanning,
                None,
                None,
            )
            .await?;

        let summary_err = match service
            .record_plan(
                created.request.id,
                NewFulfillmentPlan::new(FulfillmentPlanDecision::NeedsProvider, "  "),
            )
            .await
        {
            Ok(_) => panic!("empty plan summary was accepted"),
            Err(err) => err,
        };

        assert!(matches!(summary_err, Error::EmptyFulfillmentPlanSummary));

        let loaded = service.get(created.request.id).await?;
        assert!(loaded.current_plan.is_none());
        assert!(loaded.plan_history.is_empty());

        Ok(())
    }

    #[tokio::test]
    async fn plan_provider_selection_records_selected_provider()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = kino_db::test_db().await?;
        let service = RequestService::new(db);
        let created = service
            .create(NewRequest::anonymous("Inception (2010)"))
            .await?;
        service
            .resolve_identity(
                created.request.id,
                identity(550),
                RequestIdentityProvenance::Manual,
                None,
                None,
            )
            .await?;
        service
            .transition(
                created.request.id,
                RequestTransition::StartPlanning,
                None,
                None,
            )
            .await?;
        let providers = [
            ConfiguredFulfillmentProvider::new("tv-provider", 100, TV_CAPS),
            ConfiguredFulfillmentProvider::new("movie-provider", 0, MOVIE_CAPS),
        ];

        let planned = service
            .plan_provider_selection(
                created.request.id,
                &providers,
                &[],
                Some(RequestEventActor::System),
            )
            .await?;

        assert_eq!(
            planned.selection.decision,
            FulfillmentPlanDecision::NeedsProvider
        );
        assert_eq!(
            planned.selection.selected_provider_id.as_deref(),
            Some("movie-provider")
        );
        assert_eq!(planned.detail.plan_history.len(), 1);
        assert_eq!(
            planned
                .detail
                .current_plan
                .as_ref()
                .map(|plan| plan.decision),
            Some(FulfillmentPlanDecision::NeedsProvider)
        );
        assert_eq!(
            planned
                .detail
                .current_plan
                .as_ref()
                .map(|plan| plan.summary.as_str()),
            Some("selected provider movie-provider for tmdb:movie:550")
        );
        assert_eq!(
            planned
                .detail
                .current_plan
                .as_ref()
                .and_then(|plan| plan.actor),
            Some(RequestEventActor::System)
        );

        Ok(())
    }

    #[tokio::test]
    async fn plan_provider_selection_records_needs_user_input_when_none_match()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = kino_db::test_db().await?;
        let service = RequestService::new(db);
        let created = service
            .create(NewRequest::anonymous("Inception (2010)"))
            .await?;
        service
            .resolve_identity(
                created.request.id,
                identity(550),
                RequestIdentityProvenance::Manual,
                None,
                None,
            )
            .await?;
        service
            .transition(
                created.request.id,
                RequestTransition::StartPlanning,
                None,
                None,
            )
            .await?;
        let providers = [ConfiguredFulfillmentProvider::new(
            "tv-provider",
            100,
            TV_CAPS,
        )];

        let planned = service
            .plan_provider_selection(created.request.id, &providers, &[], None)
            .await?;

        assert_eq!(
            planned.selection.decision,
            FulfillmentPlanDecision::NeedsUserInput
        );
        assert_eq!(planned.selection.selected_provider_id, None);
        assert!(planned.selection.ranked_providers.is_empty());
        assert_eq!(
            planned
                .detail
                .current_plan
                .as_ref()
                .map(|plan| plan.decision),
            Some(FulfillmentPlanDecision::NeedsUserInput)
        );
        assert_eq!(
            planned
                .detail
                .current_plan
                .as_ref()
                .map(|plan| plan.summary.as_str()),
            Some("no configured provider can satisfy tmdb:movie:550")
        );

        Ok(())
    }

    #[tokio::test]
    async fn plan_provider_selection_checks_library_before_provider_validation()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = kino_db::test_db().await?;
        let service = RequestService::new(db.clone());
        let canonical_identity_id = identity(550);
        let created = service
            .create(NewRequest::anonymous("Inception (2010)"))
            .await?;
        service
            .resolve_identity(
                created.request.id,
                canonical_identity_id,
                RequestIdentityProvenance::Manual,
                None,
                None,
            )
            .await?;
        service
            .transition(
                created.request.id,
                RequestTransition::StartPlanning,
                None,
                None,
            )
            .await?;
        insert_media_item(&db, canonical_identity_id).await?;
        let providers = [
            ConfiguredFulfillmentProvider::new("duplicate", 0, MOVIE_CAPS),
            ConfiguredFulfillmentProvider::new(" duplicate ", 1, MOVIE_CAPS),
        ];

        let planned = service
            .plan_provider_selection(
                created.request.id,
                &providers,
                &[],
                Some(RequestEventActor::System),
            )
            .await?;

        assert_eq!(
            planned.selection.decision,
            FulfillmentPlanDecision::AlreadySatisfied
        );
        assert_eq!(planned.selection.selected_provider_id, None);
        assert!(planned.selection.ranked_providers.is_empty());
        assert_eq!(planned.detail.request.state, RequestState::Satisfied);
        assert_eq!(
            planned
                .detail
                .current_plan
                .as_ref()
                .map(|plan| plan.decision),
            Some(FulfillmentPlanDecision::AlreadySatisfied)
        );

        Ok(())
    }

    #[tokio::test]
    async fn transient_provider_error_returns_retry_without_state_transition()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = kino_db::test_db().await?;
        let service = RequestService::new(db);
        let request_id = fulfilling_request(&service).await?;
        let policy = ProviderRetryPolicy::new(3, Duration::from_secs(5), Duration::from_secs(30));

        let result = service
            .handle_provider_error(
                request_id,
                "movie-provider",
                FulfillmentProviderError::transient("timeout", "provider timed out"),
                1,
                policy,
                Some(RequestEventActor::System),
            )
            .await?;

        assert_eq!(
            result,
            ProviderErrorHandlingResult::Retry {
                provider_id: String::from("movie-provider"),
                failure_count: 1,
                retry_after: Duration::from_secs(5),
                message: String::from(
                    "provider movie-provider returned transient error timeout: provider timed out"
                ),
            }
        );

        let detail = service.get(request_id).await?;
        assert_eq!(detail.request.state, RequestState::Fulfilling);
        assert_eq!(detail.request.failure_reason, None);

        Ok(())
    }

    #[tokio::test]
    async fn permanent_provider_error_fails_request_with_error_message()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = kino_db::test_db().await?;
        let service = RequestService::new(db);
        let request_id = fulfilling_request(&service).await?;

        let result = service
            .handle_provider_error(
                request_id,
                "movie-provider",
                FulfillmentProviderError::permanent("not_found", "provider rejected id"),
                1,
                ProviderRetryPolicy::default(),
                Some(RequestEventActor::System),
            )
            .await?;

        let ProviderErrorHandlingResult::Failed { detail } = result else {
            panic!("permanent errors should fail the request");
        };
        let event = detail
            .status_events
            .last()
            .ok_or("failed request should have a status event")?;

        assert_eq!(detail.request.state, RequestState::Failed);
        assert_eq!(
            detail.request.failure_reason,
            Some(RequestFailureReason::AcquisitionFailed)
        );
        assert_eq!(event.to_state, RequestState::Failed);
        assert_eq!(
            event.message.as_deref(),
            Some(
                "provider movie-provider returned permanent error not_found: provider rejected id"
            )
        );

        Ok(())
    }

    #[tokio::test]
    async fn exhausted_transient_provider_error_fails_request()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = kino_db::test_db().await?;
        let service = RequestService::new(db);
        let request_id = fulfilling_request(&service).await?;

        let result = service
            .handle_provider_error(
                request_id,
                "movie-provider",
                FulfillmentProviderError::transient("rate_limited", "provider is rate limited"),
                3,
                ProviderRetryPolicy::default(),
                Some(RequestEventActor::System),
            )
            .await?;

        let ProviderErrorHandlingResult::Failed { detail } = result else {
            panic!("exhausted transient errors should fail the request");
        };

        assert_eq!(detail.request.state, RequestState::Failed);
        assert_eq!(
            detail.request.failure_reason,
            Some(RequestFailureReason::AcquisitionFailed)
        );
        assert_eq!(
            detail
                .status_events
                .last()
                .and_then(|event| event.message.as_deref()),
            Some(
                "provider movie-provider returned transient error rate_limited: provider is rate limited"
            )
        );

        Ok(())
    }

    #[tokio::test]
    async fn matching_probed_file_keeps_request_ingesting()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = kino_db::test_db().await?;
        let service = RequestService::new(db);
        let request_id = ingesting_request(&service).await?;

        let result = service
            .verify_probed_file(
                request_id,
                ExpectedProbedFile::new(identity(550))
                    .with_title("Inception")
                    .with_runtime_seconds(8880)
                    .with_required_audio_languages(["eng"]),
                ProbedFile::new()
                    .with_title("Inception")
                    .with_duration_seconds(9000)
                    .with_audio_languages(["eng", "jpn"]),
                Some(RequestEventActor::System),
            )
            .await?;

        let ProbedFileVerificationResult::Matched { detail, checked } = result else {
            panic!("matching file should pass verification");
        };
        assert!(checked.is_match());
        assert_eq!(detail.request.state, RequestState::Ingesting);
        assert_eq!(detail.request.failure_reason, None);

        Ok(())
    }

    #[tokio::test]
    async fn mismatched_probed_file_fails_request_with_ingest_reason()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = kino_db::test_db().await?;
        let service = RequestService::new(db);
        let request_id = ingesting_request(&service).await?;

        let result = service
            .verify_probed_file(
                request_id,
                ExpectedProbedFile::new(identity(550))
                    .with_title("Inception")
                    .with_runtime_seconds(8880),
                ProbedFile::new()
                    .with_title("Finding Nemo")
                    .with_duration_seconds(600),
                Some(RequestEventActor::System),
            )
            .await?;

        let ProbedFileVerificationResult::Mismatched { detail, checked } = result else {
            panic!("mismatched file should fail verification");
        };
        let event = detail
            .status_events
            .last()
            .ok_or("failed request should have a status event")?;

        assert!(!checked.is_match());
        assert_eq!(detail.request.state, RequestState::Failed);
        assert_eq!(
            detail.request.failure_reason,
            Some(RequestFailureReason::IngestFailed)
        );
        assert_eq!(event.to_state, RequestState::Failed);
        let message = event
            .message
            .as_deref()
            .ok_or("mismatch event should explain the failure")?;
        assert!(message.contains("title mismatch"), "got: {message}");
        assert!(message.contains("duration mismatch"), "got: {message}");

        Ok(())
    }

    #[tokio::test]
    async fn high_confidence_match_resolves_request()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = kino_db::test_db().await?;
        let service = RequestService::new(db);
        let winner_id = identity(550);
        let created = service
            .create(NewRequest::anonymous("Inception (2010)"))
            .await?;

        let detail = service
            .resolve_matches(
                created.request.id,
                vec![
                    candidate(winner_id, "Inception", Some(2010), 80.0),
                    candidate(identity(157_336), "Interstellar", Some(2014), 70.0),
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
        assert_eq!(detail.identity_versions.len(), 1);
        assert_eq!(detail.identity_versions[0].version, 1);
        assert_eq!(detail.identity_versions[0].canonical_identity_id, winner_id);
        assert_eq!(
            detail.identity_versions[0].provenance,
            RequestIdentityProvenance::MatchScoring
        );

        Ok(())
    }

    #[tokio::test]
    async fn low_confidence_match_parks_request_with_ranked_candidates()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = kino_db::test_db().await?;
        let service = RequestService::new(db);
        let newer_id = identity(438_631);
        let older_id = identity(841);
        let created = service.create(NewRequest::anonymous("Dune")).await?;

        let detail = service
            .resolve_matches(
                created.request.id,
                vec![
                    candidate(older_id, "Dune", Some(1984), 60.0),
                    candidate(newer_id, "Dune", Some(2021), 90.0),
                    candidate(identity(999_999), "Dune World", Some(2021), 10.0),
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
            .resolve_identity(
                resolved.request.id,
                identity(550),
                RequestIdentityProvenance::Manual,
                None,
                None,
            )
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
    async fn generic_resolution_transition_requires_identity_action()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = kino_db::test_db().await?;
        let service = RequestService::new(db);
        let created = service
            .create(NewRequest::anonymous("Inception (2010)"))
            .await?;

        let err = match service
            .transition(created.request.id, RequestTransition::Resolve, None, None)
            .await
        {
            Ok(_) => panic!("unversioned resolution transition was accepted"),
            Err(err) => err,
        };

        assert!(matches!(
            err,
            Error::ResolutionRequiresIdentity {
                transition: RequestTransition::Resolve
            }
        ));

        let detail = service.get(created.request.id).await?;
        assert_eq!(detail.request.state, RequestState::Pending);
        assert_eq!(detail.status_events.len(), 1);
        assert!(detail.identity_versions.is_empty());

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
            .resolve_identity(
                cancelled.request.id,
                identity(550),
                RequestIdentityProvenance::Manual,
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
            (RequestState::Planning, RequestTransition::Satisfy),
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
            .resolve_identity(
                created.request.id,
                identity(550),
                RequestIdentityProvenance::Manual,
                None,
                None,
            )
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
            .re_resolve_identity(
                created.request.id,
                identity(551),
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
        assert_eq!(detail.identity_versions.len(), 2);
        assert_eq!(detail.identity_versions[1].version, 2);
        assert_eq!(
            detail.identity_versions[1].provenance,
            RequestIdentityProvenance::Manual
        );
        assert_eq!(
            detail.identity_versions[1].status_event_id,
            Some(detail.status_events[3].id)
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

    #[tokio::test]
    async fn identity_versions_are_append_only()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = kino_db::test_db().await?;
        let service = RequestService::new(db.clone());
        let detail = service
            .resolve_identity(
                service
                    .create(NewRequest::anonymous("Inception (2010)"))
                    .await?
                    .request
                    .id,
                identity(550),
                RequestIdentityProvenance::Manual,
                None,
                None,
            )
            .await?;

        let update_result = sqlx::query(
            "UPDATE request_identity_versions SET provenance = ?3 WHERE request_id = ?1 AND version = ?2",
        )
        .bind(detail.request.id)
        .bind(i64::from(detail.identity_versions[0].version))
        .bind(RequestIdentityProvenance::MatchScoring.as_str())
        .execute(db.write_pool())
        .await;
        let delete_result =
            sqlx::query("DELETE FROM request_identity_versions WHERE request_id = ?1")
                .bind(detail.request.id)
                .execute(db.write_pool())
                .await;

        assert!(update_result.is_err());
        assert!(delete_result.is_err());

        let loaded = service.get(detail.request.id).await?;
        assert_eq!(loaded.identity_versions, detail.identity_versions);

        Ok(())
    }

    #[tokio::test]
    async fn fulfillment_plans_are_append_only()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = kino_db::test_db().await?;
        let service = RequestService::new(db.clone());
        let created = service
            .create(NewRequest::anonymous("Inception (2010)"))
            .await?;
        service
            .resolve_identity(
                created.request.id,
                identity(550),
                RequestIdentityProvenance::Manual,
                None,
                None,
            )
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
            .record_plan(
                created.request.id,
                NewFulfillmentPlan::new(
                    FulfillmentPlanDecision::NeedsProvider,
                    "provider can satisfy this request",
                ),
            )
            .await?;

        let update_result =
            sqlx::query("UPDATE request_fulfillment_plans SET summary = ?2 WHERE id = ?1")
                .bind(detail.plan_history[0].id)
                .bind("changed")
                .execute(db.write_pool())
                .await;
        let delete_result = sqlx::query("DELETE FROM request_fulfillment_plans WHERE id = ?1")
            .bind(detail.plan_history[0].id)
            .execute(db.write_pool())
            .await;

        assert!(update_result.is_err());
        assert!(delete_result.is_err());

        let loaded = service.get(detail.request.id).await?;
        assert_eq!(loaded.plan_history, detail.plan_history);

        Ok(())
    }

    fn candidate(
        canonical_identity_id: CanonicalIdentityId,
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

    fn identity(tmdb_id: u32) -> CanonicalIdentityId {
        match TmdbId::new(tmdb_id) {
            Some(tmdb_id) => CanonicalIdentityId::tmdb_movie(tmdb_id),
            None => panic!("test tmdb id must be positive"),
        }
    }

    async fn fulfilling_request(
        service: &RequestService,
    ) -> std::result::Result<Id, Box<dyn std::error::Error>> {
        let created = service
            .create(NewRequest::anonymous("Inception (2010)"))
            .await?;
        service
            .resolve_identity(
                created.request.id,
                identity(550),
                RequestIdentityProvenance::Manual,
                None,
                None,
            )
            .await?;
        service
            .transition(
                created.request.id,
                RequestTransition::StartPlanning,
                None,
                None,
            )
            .await?;
        service
            .transition(
                created.request.id,
                RequestTransition::StartFulfilling,
                None,
                None,
            )
            .await?;

        Ok(created.request.id)
    }

    async fn ingesting_request(
        service: &RequestService,
    ) -> std::result::Result<Id, Box<dyn std::error::Error>> {
        let request_id = fulfilling_request(service).await?;
        service
            .transition(request_id, RequestTransition::StartIngesting, None, None)
            .await?;

        Ok(request_id)
    }

    async fn insert_media_item(
        db: &kino_db::Db,
        canonical_identity_id: CanonicalIdentityId,
    ) -> std::result::Result<(), sqlx::Error> {
        let now = Timestamp::now();
        sqlx::query(
            r#"
            INSERT OR IGNORE INTO canonical_identities (
                id,
                provider,
                media_kind,
                tmdb_id,
                source,
                created_at,
                updated_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            "#,
        )
        .bind(canonical_identity_id)
        .bind(canonical_identity_id.provider().as_str())
        .bind(canonical_identity_id.kind().as_str())
        .bind(i64::from(canonical_identity_id.tmdb_id().get()))
        .bind(CanonicalIdentitySource::Manual.as_str())
        .bind(now)
        .bind(now)
        .execute(db.write_pool())
        .await?;

        sqlx::query(
            r#"
            INSERT INTO media_items (
                id,
                media_kind,
                canonical_identity_id,
                season_number,
                episode_number,
                created_at,
                updated_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            "#,
        )
        .bind(Id::new())
        .bind(media_item_kind_for_identity(canonical_identity_id).as_str())
        .bind(canonical_identity_id)
        .bind(media_item_season_number(canonical_identity_id))
        .bind(media_item_episode_number(canonical_identity_id))
        .bind(now)
        .bind(now)
        .execute(db.write_pool())
        .await?;

        Ok(())
    }

    fn media_item_kind_for_identity(
        canonical_identity_id: CanonicalIdentityId,
    ) -> kino_core::MediaItemKind {
        match canonical_identity_id.kind() {
            kino_core::CanonicalIdentityKind::Movie => kino_core::MediaItemKind::Movie,
            kino_core::CanonicalIdentityKind::TvSeries => kino_core::MediaItemKind::TvEpisode,
        }
    }

    fn media_item_season_number(canonical_identity_id: CanonicalIdentityId) -> Option<i64> {
        match canonical_identity_id.kind() {
            kino_core::CanonicalIdentityKind::Movie => None,
            kino_core::CanonicalIdentityKind::TvSeries => Some(1),
        }
    }

    fn media_item_episode_number(canonical_identity_id: CanonicalIdentityId) -> Option<i64> {
        match canonical_identity_id.kind() {
            kino_core::CanonicalIdentityKind::Movie => None,
            kino_core::CanonicalIdentityKind::TvSeries => Some(1),
        }
    }
}
