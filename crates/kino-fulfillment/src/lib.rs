//! Request tracking and fulfillment orchestration.

pub mod movie;
mod request;
pub mod tmdb;
pub mod tv;

pub use kino_core::{Request, RequestFailureReason, RequestRequester, RequestState, RequestTarget};
pub use request::{
    Error, FulfillmentPlan, FulfillmentPlanDecision, NewFulfillmentPlan, NewRequest,
    REQUEST_AUTO_RESOLVE_MIN_MARGIN, REQUEST_AUTO_RESOLVE_MIN_SCORE, REQUEST_LIST_DEFAULT_LIMIT,
    REQUEST_LIST_MAX_LIMIT, REQUEST_MATCH_CANDIDATE_LIMIT, RequestDetail, RequestEventActor,
    RequestIdentityProvenance, RequestIdentityVersion, RequestListPage, RequestListQuery,
    RequestMatchCandidate, RequestMatchCandidateInput, RequestModelUpdate, RequestService,
    RequestStatusEvent, RequestTransition, Result,
};
