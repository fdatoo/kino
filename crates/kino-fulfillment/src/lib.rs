//! Request tracking and fulfillment orchestration.

pub mod movie;
mod planning;
mod provider;
mod request;
pub mod tmdb;
pub mod tv;

pub use kino_core::{Request, RequestFailureReason, RequestRequester, RequestState, RequestTarget};
pub use planning::{
    ComputedFulfillmentPlan, FulfillmentLibraryState, FulfillmentPlanningInput,
    FulfillmentProviderArgs, FulfillmentUserInputReason, compute_fulfillment_plan,
};
pub use provider::{
    ConfiguredFulfillmentProvider, FulfillmentProviderCapability, ProviderSelectionContext,
    ProviderSelectionPlan, RankedFulfillmentProvider, select_fulfillment_provider,
};
pub use request::{
    Error, FulfillmentPlan, FulfillmentPlanDecision, NewFulfillmentPlan, NewRequest,
    ProviderSelectionPlanningResult, REQUEST_AUTO_RESOLVE_MIN_MARGIN,
    REQUEST_AUTO_RESOLVE_MIN_SCORE, REQUEST_LIST_DEFAULT_LIMIT, REQUEST_LIST_MAX_LIMIT,
    REQUEST_MATCH_CANDIDATE_LIMIT, RequestDetail, RequestEventActor, RequestIdentityProvenance,
    RequestIdentityVersion, RequestListPage, RequestListQuery, RequestMatchCandidate,
    RequestMatchCandidateInput, RequestModelUpdate, RequestService, RequestStatusEvent,
    RequestTransition, Result,
};
