//! Request tracking and fulfillment orchestration.

pub mod disc_rip;
pub mod ingestion;
pub mod manual_import;
pub mod movie;
mod planning;
pub mod probe;
mod provider;
mod request;
pub mod tmdb;
pub mod tv;
pub mod watch_folder;

pub use disc_rip::{DISC_RIP_PROVIDER_ID, DiscRipProvider};
pub use ingestion::{
    ExpectedProbedFile, IngestCanonicalSourceFile, IngestSourceFile, IngestedCanonicalSourceFile,
    IngestedSourceFile, IngestionPipeline, PROBED_FILE_DURATION_MIN_TOLERANCE_SECONDS,
    PROBED_FILE_DURATION_TOLERANCE_PERCENT, ProbedFile, ProbedFileMatch, ProbedFileMismatch,
    match_probed_file,
};
pub use kino_core::{Request, RequestFailureReason, RequestRequester, RequestState, RequestTarget};
pub use manual_import::{MANUAL_IMPORT_PROVIDER_ID, ManualImportProvider};
pub use planning::{
    ComputedFulfillmentPlan, FulfillmentLibraryState, FulfillmentPlanningInput,
    FulfillmentProviderArgs, FulfillmentUserInputReason, compute_fulfillment_plan,
};
pub use probe::{
    DEFAULT_FFPROBE_PROGRAM, FfprobeFileProbe, ProbeAudioStream, ProbeContainer, ProbeError,
    ProbeResult, ProbeSubtitleKind, ProbeSubtitleStream, ProbeVideoStream,
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
    ProbedFileVerificationResult, ProviderErrorHandlingResult, ProviderSelectionPlanningResult,
    REQUEST_AUTO_RESOLVE_MIN_MARGIN, REQUEST_AUTO_RESOLVE_MIN_SCORE, REQUEST_LIST_DEFAULT_LIMIT,
    REQUEST_LIST_MAX_LIMIT, REQUEST_MATCH_CANDIDATE_LIMIT, RequestDetail, RequestEventActor,
    RequestIdentityProvenance, RequestIdentityVersion, RequestListPage, RequestListQuery,
    RequestMatchCandidate, RequestMatchCandidateInput, RequestModelUpdate, RequestService,
    RequestStatusEvent, RequestTransition, Result, rank_match_candidates,
};
pub use watch_folder::{WATCH_FOLDER_PROVIDER_ID, WatchFolderProvider};
