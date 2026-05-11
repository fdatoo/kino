use std::{path::PathBuf, sync::Arc};

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use kino_core::{CanonicalIdentityId, Id, id::ParseIdError};
use kino_db::Db;
use kino_fulfillment::{
    FulfillmentPlanDecision, FulfillmentProvider, FulfillmentProviderArgs,
    FulfillmentProviderError, ManualImportProvider, NewFulfillmentPlan, NewRequest, RequestDetail,
    RequestEventActor, RequestListPage, RequestListQuery, RequestMatchCandidateInput,
    RequestService, RequestState, RequestTransition,
};
use kino_library::{LibraryScanReport, LibraryScanService};
use serde::{Deserialize, Serialize};

#[derive(Clone)]
struct AppState {
    requests: RequestService,
    manual_imports: Arc<ManualImportProvider>,
    library_scans: LibraryScanService,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateRequest {
    target: String,
    message: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ListRequestsQuery {
    state: Option<RequestState>,
    limit: Option<u32>,
    offset: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScoreMatchesRequest {
    candidates: Vec<RequestMatchCandidateInput>,
    message: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReResolveRequest {
    canonical_identity_id: CanonicalIdentityId,
    message: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RecordPlanRequest {
    decision: FulfillmentPlanDecision,
    summary: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManualImportRequest {
    path: PathBuf,
    message: Option<String>,
}

#[derive(Debug, Serialize)]
struct ManualImportResponse {
    request: RequestDetail,
    provider_id: String,
    job_id: String,
    path: PathBuf,
}

pub(crate) fn router(db: Db, library_root: PathBuf) -> Router {
    let state = AppState {
        requests: RequestService::new(db.clone()),
        manual_imports: Arc::new(ManualImportProvider::new()),
        library_scans: LibraryScanService::new(db, library_root),
    };

    Router::new()
        .route("/api/requests", post(create_request).get(list_requests))
        .route(
            "/api/requests/{id}",
            get(get_request).delete(cancel_request),
        )
        .route("/api/requests/{id}/matches", post(score_matches))
        .route("/api/requests/{id}/plans", post(record_plan))
        .route("/api/requests/{id}/re-resolution", post(re_resolve))
        .route(
            "/api/admin/requests/{id}/manual-import",
            post(manual_import),
        )
        .route("/api/admin/library/scan", get(scan_library))
        .with_state(state)
}

async fn create_request(
    State(state): State<AppState>,
    Json(payload): Json<CreateRequest>,
) -> ApiResult<(StatusCode, Json<RequestDetail>)> {
    let detail = state
        .requests
        .create(NewRequest {
            target_raw_query: payload.target.as_str(),
            requester: kino_fulfillment::RequestRequester::Anonymous,
            actor: None,
            message: payload.message.as_deref(),
        })
        .await?;
    Ok((StatusCode::CREATED, Json(detail)))
}

async fn list_requests(
    State(state): State<AppState>,
    Query(query): Query<ListRequestsQuery>,
) -> ApiResult<Json<RequestListPage>> {
    let mut request_query = RequestListQuery::new();
    if let Some(filter_state) = query.state {
        request_query = request_query.with_state(filter_state);
    }
    if let Some(limit) = query.limit {
        request_query = request_query.with_limit(limit);
    }
    if let Some(offset) = query.offset {
        request_query = request_query.with_offset(offset);
    }

    let page = state.requests.list(request_query).await?;
    Ok(Json(page))
}

async fn get_request(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<RequestDetail>> {
    let id = parse_id(id)?;
    let detail = state.requests.get(id).await?;
    Ok(Json(detail))
}

async fn cancel_request(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<RequestDetail>> {
    let id = parse_id(id)?;
    let detail = state
        .requests
        .transition(id, RequestTransition::Cancel, None, None)
        .await?;
    Ok(Json(detail))
}

async fn score_matches(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(payload): Json<ScoreMatchesRequest>,
) -> ApiResult<Json<RequestDetail>> {
    let id = parse_id(id)?;
    let detail = state
        .requests
        .resolve_matches(
            id,
            payload.candidates,
            Some(RequestEventActor::System),
            payload.message.as_deref(),
        )
        .await?;
    Ok(Json(detail))
}

async fn record_plan(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(payload): Json<RecordPlanRequest>,
) -> ApiResult<Json<RequestDetail>> {
    let id = parse_id(id)?;
    let detail = state
        .requests
        .record_plan(
            id,
            NewFulfillmentPlan::new(payload.decision, payload.summary.as_str())
                .with_actor(RequestEventActor::System),
        )
        .await?;
    Ok(Json(detail))
}

async fn re_resolve(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(payload): Json<ReResolveRequest>,
) -> ApiResult<Json<RequestDetail>> {
    let id = parse_id(id)?;
    let detail = state
        .requests
        .re_resolve_identity(
            id,
            payload.canonical_identity_id,
            Some(RequestEventActor::System),
            payload.message.as_deref(),
        )
        .await?;
    Ok(Json(detail))
}

async fn manual_import(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(payload): Json<ManualImportRequest>,
) -> ApiResult<Json<ManualImportResponse>> {
    let id = parse_id(id)?;
    let current = state.requests.get(id).await?;
    if !RequestTransition::StartIngesting.can_apply_from(current.request.state) {
        return Err(ApiError::InvalidManualImportState {
            from: current.request.state,
        });
    }

    let canonical_identity_id =
        current
            .request
            .target
            .canonical_identity_id
            .ok_or(ApiError::ManualImport(FulfillmentProviderError::permanent(
                "request_unresolved",
                format!("request {id} does not have a canonical identity"),
            )))?;
    let path = payload.path;
    let handle = state
        .manual_imports
        .start(FulfillmentProviderArgs::new(canonical_identity_id).with_source_path(&path))
        .await
        .map_err(ApiError::ManualImport)?;
    let message = manual_import_message(payload.message.as_deref(), &path, &handle.job_id);
    let detail = state
        .requests
        .transition(
            id,
            RequestTransition::StartIngesting,
            Some(RequestEventActor::System),
            Some(message.as_str()),
        )
        .await?;

    Ok(Json(ManualImportResponse {
        request: detail,
        provider_id: handle.provider_id,
        job_id: handle.job_id,
        path,
    }))
}

async fn scan_library(State(state): State<AppState>) -> ApiResult<Json<LibraryScanReport>> {
    let report = state.library_scans.scan().await?;
    Ok(Json(report))
}

type ApiResult<T> = std::result::Result<T, ApiError>;

#[derive(Debug, thiserror::Error)]
enum ApiError {
    #[error("invalid request id {value}: {source}")]
    InvalidId { value: String, source: ParseIdError },

    #[error(transparent)]
    Fulfillment(#[from] kino_fulfillment::Error),

    #[error("manual import from {from} is invalid")]
    InvalidManualImportState { from: RequestState },

    #[error(transparent)]
    ManualImport(FulfillmentProviderError),

    #[error(transparent)]
    Library(#[from] kino_library::Error),
}

#[derive(Serialize)]
struct ErrorResponse {
    error: String,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = match &self {
            Self::InvalidId { .. } => StatusCode::BAD_REQUEST,
            Self::Fulfillment(kino_fulfillment::Error::RequestNotFound { .. }) => {
                StatusCode::NOT_FOUND
            }
            Self::Fulfillment(kino_fulfillment::Error::InvalidTransition { .. }) => {
                StatusCode::CONFLICT
            }
            Self::Fulfillment(kino_fulfillment::Error::InvalidMatchResolutionState { .. }) => {
                StatusCode::CONFLICT
            }
            Self::Fulfillment(kino_fulfillment::Error::InvalidFulfillmentPlanState { .. }) => {
                StatusCode::CONFLICT
            }
            Self::Fulfillment(kino_fulfillment::Error::ResolutionRequiresIdentity { .. }) => {
                StatusCode::CONFLICT
            }
            Self::Fulfillment(kino_fulfillment::Error::InvalidListLimit { .. }) => {
                StatusCode::BAD_REQUEST
            }
            Self::Fulfillment(kino_fulfillment::Error::InvalidListOffset { .. }) => {
                StatusCode::BAD_REQUEST
            }
            Self::Fulfillment(kino_fulfillment::Error::NoMatchCandidates) => {
                StatusCode::BAD_REQUEST
            }
            Self::Fulfillment(kino_fulfillment::Error::DuplicateMatchCandidate { .. }) => {
                StatusCode::BAD_REQUEST
            }
            Self::Fulfillment(kino_fulfillment::Error::InvalidMatchCandidate { .. }) => {
                StatusCode::BAD_REQUEST
            }
            Self::Fulfillment(kino_fulfillment::Error::EmptyFulfillmentPlanSummary) => {
                StatusCode::BAD_REQUEST
            }
            Self::InvalidManualImportState { .. } => StatusCode::CONFLICT,
            Self::ManualImport(error) if error.is_transient() => StatusCode::SERVICE_UNAVAILABLE,
            Self::ManualImport(_) => StatusCode::BAD_REQUEST,
            Self::Library(_) => {
                tracing::error!(error = %self, "library admin api failed");
                StatusCode::INTERNAL_SERVER_ERROR
            }
            Self::Fulfillment(_) => {
                tracing::error!(error = %self, "request api failed");
                StatusCode::INTERNAL_SERVER_ERROR
            }
        };

        (
            status,
            Json(ErrorResponse {
                error: self.to_string(),
            }),
        )
            .into_response()
    }
}

fn parse_id(value: String) -> ApiResult<Id> {
    value
        .parse()
        .map_err(|source| ApiError::InvalidId { value, source })
}

fn manual_import_message(message: Option<&str>, path: &std::path::Path, job_id: &str) -> String {
    let import_message = format!("manual import {} accepted as {job_id}", path.display());
    match message.map(str::trim).filter(|message| !message.is_empty()) {
        Some(message) => format!("{message}; {import_message}"),
        None => import_message,
    }
}
