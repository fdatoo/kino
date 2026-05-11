//! Request tracking and fulfillment orchestration.

pub mod disc_rip;
pub mod ingestion;
pub mod manual_import;
pub mod movie;
mod planning;
mod provider;
mod request;
pub mod tmdb;
pub mod tv;
pub mod watch_folder;

pub use disc_rip::{DISC_RIP_PROVIDER_ID, DiscRipProvider};
pub use ingestion::{IngestSourceFile, IngestedSourceFile, IngestionPipeline};
pub use kino_core::{Request, RequestFailureReason, RequestRequester, RequestState, RequestTarget};
pub use manual_import::{MANUAL_IMPORT_PROVIDER_ID, ManualImportProvider};
pub use planning::{
    ComputedFulfillmentPlan, FulfillmentLibraryState, FulfillmentPlanningInput,
    FulfillmentProviderArgs, FulfillmentUserInputReason, compute_fulfillment_plan,
};
pub use provider::{
    ConfiguredFulfillmentProvider, FulfillmentProvider, FulfillmentProviderCancelResult,
    FulfillmentProviderCapabilities, FulfillmentProviderCapability, FulfillmentProviderCleanup,
    FulfillmentProviderError, FulfillmentProviderFuture, FulfillmentProviderJobHandle,
    FulfillmentProviderJobStatus, FulfillmentProviderProgress, FulfillmentProviderResult,
    PROVIDER_RETRY_INITIAL_BACKOFF, PROVIDER_RETRY_MAX_BACKOFF, PROVIDER_RETRY_MAX_FAILURES,
    ProviderRetryPolicy, ProviderSelectionContext, ProviderSelectionPlan,
    RankedFulfillmentProvider, select_fulfillment_provider,
};
pub use request::{
    Error, FulfillmentPlan, FulfillmentPlanDecision, NewFulfillmentPlan, NewRequest,
    ProviderErrorHandlingResult, ProviderSelectionPlanningResult, REQUEST_AUTO_RESOLVE_MIN_MARGIN,
    REQUEST_AUTO_RESOLVE_MIN_SCORE, REQUEST_LIST_DEFAULT_LIMIT, REQUEST_LIST_MAX_LIMIT,
    REQUEST_MATCH_CANDIDATE_LIMIT, RequestDetail, RequestEventActor, RequestIdentityProvenance,
    RequestIdentityVersion, RequestListPage, RequestListQuery, RequestMatchCandidate,
    RequestMatchCandidateInput, RequestModelUpdate, RequestService, RequestStatusEvent,
    RequestTransition, Result,
};
pub use watch_folder::{WATCH_FOLDER_PROVIDER_ID, WatchFolderProvider};
