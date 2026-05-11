use std::{
    path::{Component, Path as FsPath, PathBuf},
    sync::Arc,
};

use axum::{
    Json, Router,
    extract::{Path, Query, State, rejection::QueryRejection},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use kino_core::{CanonicalIdentityId, Id, MediaItemKind, id::ParseIdError};
use kino_db::Db;
use kino_fulfillment::{
    FulfillmentPlanDecision, FulfillmentProvider, FulfillmentProviderArgs,
    FulfillmentProviderError, ManualImportProvider, NewFulfillmentPlan, NewRequest, RequestDetail,
    RequestEventActor, RequestListPage, RequestListQuery, RequestMatchCandidateInput,
    RequestService, RequestState, RequestTransition,
};
use kino_library::{
    CatalogArtworkKind, CatalogListPage, CatalogListQuery, CatalogMediaItem, CatalogService,
    CatalogSort, LibraryScanReport, LibraryScanService, ReocrJob, SubtitleReocrService,
};
use serde::{Deserialize, Serialize};

use crate::auth::AuthenticatedUser;

#[derive(Clone)]
pub(crate) struct AppState {
    requests: RequestService,
    manual_imports: Arc<ManualImportProvider>,
    catalog: CatalogService,
    library_scans: LibraryScanService,
    subtitle_reocr: SubtitleReocrService,
    artwork_cache_dir: PathBuf,
}

#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct CreateRequest {
    /// Raw title or query text requested by the user.
    target: String,
    /// Optional status-event message.
    message: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub(crate) struct ListRequestsQuery {
    /// Request state to include.
    state: Option<RequestState>,
    /// Maximum number of requests to return.
    limit: Option<u32>,
    /// Number of matching requests to skip.
    offset: Option<u64>,
}

#[derive(Debug, Clone, Default, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub(crate) struct ListCatalogItemsQuery {
    /// Media item kind to include.
    pub(crate) kind: Option<String>,
    /// Legacy media item kind query parameter.
    #[serde(rename = "type")]
    #[param(rename = "type")]
    pub(crate) media_type: Option<String>,
    /// Release year to include.
    pub(crate) year: Option<i32>,
    /// Watched-state filter for the current user.
    pub(crate) watched: Option<bool>,
    /// Stable sort order: recently_added, title, or year.
    pub(crate) sort: Option<String>,
    /// Full-text search across title and cast names.
    pub(crate) search: Option<String>,
    /// Case-insensitive title substring.
    pub(crate) title_contains: Option<String>,
    /// Filter by source-file presence.
    pub(crate) has_source_file: Option<bool>,
    /// Maximum number of items to return.
    pub(crate) limit: Option<u32>,
    /// Number of matching items to skip.
    pub(crate) offset: Option<u64>,
    /// Opaque cursor returned by the previous page.
    pub(crate) cursor: Option<String>,
}

#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ScoreMatchesRequest {
    /// Candidate identities to score for this request.
    candidates: Vec<RequestMatchCandidateInput>,
    /// Optional status-event message.
    message: Option<String>,
}

#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReResolveRequest {
    /// Canonical identity selected by the resolver or operator.
    canonical_identity_id: CanonicalIdentityId,
    /// Optional status-event message.
    message: Option<String>,
}

#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct RecordPlanRequest {
    /// Planner decision to persist.
    decision: FulfillmentPlanDecision,
    /// Human-readable reason for the decision.
    summary: String,
}

#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ManualImportRequest {
    /// Existing source path to ingest for the request.
    #[schema(value_type = String)]
    path: PathBuf,
    /// Optional status-event message.
    message: Option<String>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct ManualImportResponse {
    request: RequestDetail,
    provider_id: String,
    job_id: String,
    #[schema(value_type = String)]
    path: PathBuf,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct ReocrTrackResponse {
    /// Tracking id for this synchronous re-OCR execution.
    job_id: Id,
}

impl From<ReocrJob> for ReocrTrackResponse {
    fn from(job: ReocrJob) -> Self {
        Self { job_id: job.job_id }
    }
}

pub(crate) fn router(
    db: Db,
    library_root: PathBuf,
    artwork_cache_dir: PathBuf,
    subtitle_reocr: SubtitleReocrService,
) -> Router {
    let state = AppState {
        requests: RequestService::new(db.clone()),
        manual_imports: Arc::new(ManualImportProvider::new()),
        catalog: CatalogService::new(db.clone()),
        library_scans: LibraryScanService::new(db.clone(), library_root),
        subtitle_reocr,
        artwork_cache_dir,
    };

    Router::new()
        .route("/api/v1/requests", post(create_request).get(list_requests))
        .route(
            "/api/v1/requests/{id}",
            get(get_request).delete(cancel_request),
        )
        .route("/api/v1/requests/{id}/matches", post(score_matches))
        .route("/api/v1/requests/{id}/plans", post(record_plan))
        .route("/api/v1/requests/{id}/re-resolution", post(re_resolve))
        .route(
            "/api/v1/admin/requests/{id}/manual-import",
            post(manual_import),
        )
        .route("/api/v1/library/items", get(list_catalog_items))
        .route("/api/v1/library/items/{id}", get(get_catalog_item))
        .route(
            "/api/v1/library/items/{id}/images/{kind}",
            get(get_catalog_item_image),
        )
        .route("/api/v1/admin/library/scan", get(scan_library))
        .route(
            "/api/v1/admin/items/{id}/subtitles/{track}/re-ocr",
            post(reocr_subtitle_track),
        )
        .with_state(state)
}

#[utoipa::path(
    post,
    path = "/api/v1/requests",
    tag = "requests",
    request_body = CreateRequest,
    responses(
        (status = 201, description = "Request created", body = RequestDetail),
        (status = 500, description = "Request creation failed", body = ErrorResponse)
    )
)]
pub(crate) async fn create_request(
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

#[utoipa::path(
    get,
    path = "/api/v1/requests",
    tag = "requests",
    params(ListRequestsQuery),
    responses(
        (status = 200, description = "Requests visible to the requester", body = RequestListPage),
        (status = 400, description = "Invalid request list query", body = ErrorResponse),
        (status = 500, description = "Request list failed", body = ErrorResponse)
    )
)]
pub(crate) async fn list_requests(
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

#[utoipa::path(
    get,
    path = "/api/v1/requests/{id}",
    tag = "requests",
    params(
        ("id" = Id, Path, description = "Request id")
    ),
    responses(
        (status = 200, description = "Request detail", body = RequestDetail),
        (status = 400, description = "Invalid request id", body = ErrorResponse),
        (status = 404, description = "Request not found", body = ErrorResponse),
        (status = 500, description = "Request read failed", body = ErrorResponse)
    )
)]
pub(crate) async fn get_request(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<RequestDetail>> {
    let id = parse_id(id)?;
    let detail = state.requests.get(id).await?;
    Ok(Json(detail))
}

#[utoipa::path(
    delete,
    path = "/api/v1/requests/{id}",
    tag = "requests",
    params(
        ("id" = Id, Path, description = "Request id")
    ),
    responses(
        (status = 200, description = "Request cancelled", body = RequestDetail),
        (status = 400, description = "Invalid request id", body = ErrorResponse),
        (status = 404, description = "Request not found", body = ErrorResponse),
        (status = 409, description = "Request cannot be cancelled from its current state", body = ErrorResponse),
        (status = 500, description = "Request cancellation failed", body = ErrorResponse)
    )
)]
pub(crate) async fn cancel_request(
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

#[utoipa::path(
    post,
    path = "/api/v1/requests/{id}/matches",
    tag = "requests",
    params(
        ("id" = Id, Path, description = "Request id")
    ),
    request_body = ScoreMatchesRequest,
    responses(
        (status = 200, description = "Request match candidates scored", body = RequestDetail),
        (status = 400, description = "Invalid request id or match candidates", body = ErrorResponse),
        (status = 404, description = "Request not found", body = ErrorResponse),
        (status = 409, description = "Request cannot resolve matches from its current state", body = ErrorResponse),
        (status = 500, description = "Match scoring failed", body = ErrorResponse)
    )
)]
pub(crate) async fn score_matches(
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

#[utoipa::path(
    post,
    path = "/api/v1/requests/{id}/plans",
    tag = "requests",
    params(
        ("id" = Id, Path, description = "Request id")
    ),
    request_body = RecordPlanRequest,
    responses(
        (status = 200, description = "Fulfillment plan recorded", body = RequestDetail),
        (status = 400, description = "Invalid request id or fulfillment plan", body = ErrorResponse),
        (status = 404, description = "Request not found", body = ErrorResponse),
        (status = 409, description = "Request cannot record a plan from its current state", body = ErrorResponse),
        (status = 500, description = "Plan persistence failed", body = ErrorResponse)
    )
)]
pub(crate) async fn record_plan(
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

#[utoipa::path(
    post,
    path = "/api/v1/requests/{id}/re-resolution",
    tag = "requests",
    params(
        ("id" = Id, Path, description = "Request id")
    ),
    request_body = ReResolveRequest,
    responses(
        (status = 200, description = "Request identity re-resolved", body = RequestDetail),
        (status = 400, description = "Invalid request id or canonical identity", body = ErrorResponse),
        (status = 404, description = "Request not found", body = ErrorResponse),
        (status = 409, description = "Request cannot be re-resolved from its current state", body = ErrorResponse),
        (status = 500, description = "Request re-resolution failed", body = ErrorResponse)
    )
)]
pub(crate) async fn re_resolve(
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

#[utoipa::path(
    post,
    path = "/api/v1/admin/requests/{id}/manual-import",
    tag = "admin",
    params(
        ("id" = Id, Path, description = "Request id")
    ),
    request_body = ManualImportRequest,
    responses(
        (status = 200, description = "Manual import started", body = ManualImportResponse),
        (status = 400, description = "Invalid manual import request", body = ErrorResponse),
        (status = 401, description = "Missing or invalid bearer token"),
        (status = 404, description = "Request not found", body = ErrorResponse),
        (status = 409, description = "Request cannot start ingesting", body = ErrorResponse),
        (status = 503, description = "Manual import provider unavailable", body = ErrorResponse)
    )
)]
pub(crate) async fn manual_import(
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

#[utoipa::path(
    get,
    path = "/api/v1/admin/library/scan",
    tag = "admin",
    responses(
        (status = 200, description = "Library scan report", body = LibraryScanReport),
        (status = 401, description = "Missing or invalid bearer token"),
        (status = 500, description = "Library scan failed", body = ErrorResponse)
    )
)]
pub(crate) async fn scan_library(
    State(state): State<AppState>,
) -> ApiResult<Json<LibraryScanReport>> {
    let report = state.library_scans.scan().await?;
    Ok(Json(report))
}

#[utoipa::path(
    post,
    path = "/api/v1/admin/items/{id}/subtitles/{track}/re-ocr",
    tag = "admin",
    params(
        ("id" = Id, Path, description = "Media item id"),
        ("track" = u32, Path, description = "Probed subtitle stream index")
    ),
    responses(
        (status = 202, description = "Synchronous re-OCR accepted", body = ReocrTrackResponse),
        (status = 401, description = "Missing or invalid bearer token"),
        (status = 404, description = "Media item or current OCR sidecar not found", body = ErrorResponse),
        (status = 500, description = "Re-OCR failed", body = ErrorResponse)
    )
)]
pub(crate) async fn reocr_subtitle_track(
    State(state): State<AppState>,
    Path((id, track_index)): Path<(String, u32)>,
) -> ApiResult<(StatusCode, Json<ReocrTrackResponse>)> {
    let id = parse_id(id)?;
    let job = state.subtitle_reocr.reocr_track(id, track_index).await?;
    Ok((StatusCode::ACCEPTED, Json(job.into())))
}

#[utoipa::path(
    get,
    path = "/api/v1/library/items",
    tag = "library",
    params(ListCatalogItemsQuery),
    responses(
        (status = 200, description = "Catalog media items", body = CatalogListPage),
        (status = 400, description = "Invalid catalog list query", body = ErrorResponse)
    )
)]
pub(crate) async fn list_catalog_items(
    State(state): State<AppState>,
    AuthenticatedUser { user, .. }: AuthenticatedUser,
    query: Result<Query<ListCatalogItemsQuery>, QueryRejection>,
) -> ApiResult<Json<CatalogListPage>> {
    let Query(query) = query.map_err(invalid_catalog_query)?;
    let mut catalog_query = CatalogListQuery::new();
    if let Some(media_kind) =
        parse_catalog_kind(query.kind.as_deref(), query.media_type.as_deref())?
    {
        catalog_query = catalog_query.with_media_kind(media_kind);
    }
    if let Some(year) = query.year {
        catalog_query = catalog_query.with_year(year);
    }
    if let Some(watched) = query.watched {
        catalog_query = catalog_query.with_watched_for_user(watched, user.id);
    }
    if let Some(sort) = query.sort {
        catalog_query = catalog_query.with_sort(parse_catalog_sort(&sort)?);
    }
    if let Some(search) = query.search {
        catalog_query = catalog_query.with_search(search);
    }
    if let Some(title_contains) = query.title_contains {
        catalog_query = catalog_query.with_title_contains(title_contains);
    }
    if let Some(has_source_file) = query.has_source_file {
        catalog_query = catalog_query.with_has_source_file(has_source_file);
    }
    if let Some(limit) = query.limit {
        catalog_query = catalog_query.with_limit(limit);
    }
    if let Some(offset) = query.offset {
        catalog_query = catalog_query.with_offset(offset);
    }
    if let Some(cursor) = query.cursor {
        catalog_query = catalog_query.with_cursor(cursor);
    }

    let page = state.catalog.list(catalog_query).await?;
    Ok(Json(page))
}

#[utoipa::path(
    get,
    path = "/api/v1/library/items/{id}",
    tag = "library",
    params(
        ("id" = Id, Path, description = "Media item id")
    ),
    responses(
        (status = 200, description = "Catalog media item", body = CatalogMediaItem),
        (status = 400, description = "Invalid media item id", body = ErrorResponse),
        (status = 404, description = "Catalog media item not found", body = ErrorResponse)
    )
)]
pub(crate) async fn get_catalog_item(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<CatalogMediaItem>> {
    let id = parse_id(id)?;
    let item = state.catalog.get(id).await?;
    Ok(Json(item))
}

#[utoipa::path(
    get,
    path = "/api/v1/library/items/{id}/images/{kind}",
    tag = "library",
    params(
        ("id" = Id, Path, description = "Media item id"),
        ("kind" = String, Path, description = "Artwork kind: poster, backdrop, or logo")
    ),
    responses(
        (status = 200, description = "Cached artwork image"),
        (status = 401, description = "Missing or invalid bearer token"),
        (status = 404, description = "Cached artwork image not found", body = ErrorResponse)
    )
)]
pub(crate) async fn get_catalog_item_image(
    State(state): State<AppState>,
    Path((id, kind)): Path<(String, String)>,
) -> ApiResult<Response> {
    let id = parse_id(id)?;
    let kind = CatalogArtworkKind::parse(&kind).ok_or(ApiError::ArtworkNotFound)?;
    let Some(local_path) = state.catalog.artwork_local_path(id, kind).await? else {
        return Err(ApiError::ArtworkNotFound);
    };
    let path = artwork_cache_path(&state.artwork_cache_dir, &local_path)
        .ok_or(ApiError::ArtworkNotFound)?;
    let bytes = tokio::fs::read(&path)
        .await
        .map_err(|source| artwork_read_error(path, source))?;
    let content_type = artwork_content_type(&local_path).ok_or(ApiError::ArtworkNotFound)?;

    Ok((
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
        ],
        bytes,
    )
        .into_response())
}

pub(crate) type ApiResult<T> = std::result::Result<T, ApiError>;

#[derive(Debug, thiserror::Error)]
pub(crate) enum ApiError {
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

    #[error("invalid media item type: {value}")]
    InvalidMediaItemType { value: String },

    #[error("invalid catalog sort: {value}")]
    InvalidCatalogSort { value: String },

    #[error("invalid catalog query: {reason}")]
    InvalidCatalogQuery { reason: String },

    #[error("artwork image not found")]
    ArtworkNotFound,

    #[error("reading artwork image {path}: {source}")]
    ArtworkRead {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

#[derive(Serialize, utoipa::ToSchema)]
pub(crate) struct ErrorResponse {
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
            Self::Library(kino_library::Error::MediaItemNotFound { .. }) => StatusCode::NOT_FOUND,
            Self::Library(kino_library::Error::CurrentOcrSidecarNotFound { .. }) => {
                StatusCode::NOT_FOUND
            }
            Self::Library(kino_library::Error::InvalidCatalogListLimit { .. }) => {
                StatusCode::BAD_REQUEST
            }
            Self::Library(kino_library::Error::InvalidCatalogListOffset { .. }) => {
                StatusCode::BAD_REQUEST
            }
            Self::Library(kino_library::Error::InvalidCatalogListCursor) => StatusCode::BAD_REQUEST,
            Self::Library(kino_library::Error::CatalogWatchedFilterRequiresUser) => {
                StatusCode::BAD_REQUEST
            }
            Self::InvalidMediaItemType { .. } => StatusCode::BAD_REQUEST,
            Self::InvalidCatalogSort { .. } => StatusCode::BAD_REQUEST,
            Self::InvalidCatalogQuery { .. } => StatusCode::BAD_REQUEST,
            Self::ArtworkNotFound => StatusCode::NOT_FOUND,
            Self::ArtworkRead { source, .. } if source.kind() == std::io::ErrorKind::NotFound => {
                StatusCode::NOT_FOUND
            }
            Self::ArtworkRead { .. } => {
                tracing::error!(error = %self, "library artwork api failed");
                StatusCode::INTERNAL_SERVER_ERROR
            }
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

fn parse_media_item_kind(value: &str) -> ApiResult<MediaItemKind> {
    match value {
        "movie" => Ok(MediaItemKind::Movie),
        "show" | "tv" | "tv_episode" | "tv_series" => Ok(MediaItemKind::TvEpisode),
        "personal" => Ok(MediaItemKind::Personal),
        _ => Err(ApiError::InvalidMediaItemType {
            value: value.to_owned(),
        }),
    }
}

fn parse_catalog_kind(
    kind: Option<&str>,
    media_type: Option<&str>,
) -> ApiResult<Option<MediaItemKind>> {
    match (kind, media_type) {
        (Some(kind), Some(media_type)) => {
            let kind = parse_media_item_kind(kind)?;
            let media_type = parse_media_item_kind(media_type)?;
            if kind == media_type {
                Ok(Some(kind))
            } else {
                Err(ApiError::InvalidCatalogQuery {
                    reason: String::from("kind and type must match when both are present"),
                })
            }
        }
        (Some(kind), None) => parse_media_item_kind(kind).map(Some),
        (None, Some(media_type)) => parse_media_item_kind(media_type).map(Some),
        (None, None) => Ok(None),
    }
}

fn parse_catalog_sort(value: &str) -> ApiResult<CatalogSort> {
    match value {
        "recently_added" => Ok(CatalogSort::RecentlyAdded),
        "title" => Ok(CatalogSort::Title),
        "year" => Ok(CatalogSort::Year),
        _ => Err(ApiError::InvalidCatalogSort {
            value: value.to_owned(),
        }),
    }
}

fn invalid_catalog_query(rejection: QueryRejection) -> ApiError {
    ApiError::InvalidCatalogQuery {
        reason: rejection.body_text(),
    }
}

fn manual_import_message(message: Option<&str>, path: &std::path::Path, job_id: &str) -> String {
    let import_message = format!("manual import {} accepted as {job_id}", path.display());
    match message.map(str::trim).filter(|message| !message.is_empty()) {
        Some(message) => format!("{message}; {import_message}"),
        None => import_message,
    }
}

fn artwork_cache_path(cache_dir: &FsPath, local_path: &FsPath) -> Option<PathBuf> {
    if local_path.is_absolute() {
        return None;
    }

    for component in local_path.components() {
        if !matches!(component, Component::Normal(_)) {
            return None;
        }
    }

    Some(cache_dir.join(local_path))
}

fn artwork_content_type(path: &FsPath) -> Option<&'static str> {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("avif") => Some("image/avif"),
        Some("gif") => Some("image/gif"),
        Some("jpeg" | "jpg") => Some("image/jpeg"),
        Some("png") => Some("image/png"),
        Some("webp") => Some("image/webp"),
        _ => None,
    }
}

fn artwork_read_error(path: PathBuf, source: std::io::Error) -> ApiError {
    ApiError::ArtworkRead { path, source }
}
