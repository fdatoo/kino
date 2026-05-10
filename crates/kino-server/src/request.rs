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
    FulfillmentPlanDecision, NewFulfillmentPlan, NewRequest, RequestDetail, RequestEventActor,
    RequestListPage, RequestListQuery, RequestMatchCandidateInput, RequestService, RequestState,
    RequestTransition,
};
use serde::{Deserialize, Serialize};

#[derive(Clone)]
struct AppState {
    requests: RequestService,
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

pub(crate) fn router(db: Db) -> Router {
    let state = AppState {
        requests: RequestService::new(db),
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

type ApiResult<T> = std::result::Result<T, ApiError>;

#[derive(Debug, thiserror::Error)]
enum ApiError {
    #[error("invalid request id {value}: {source}")]
    InvalidId { value: String, source: ParseIdError },

    #[error(transparent)]
    Fulfillment(#[from] kino_fulfillment::Error),
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
