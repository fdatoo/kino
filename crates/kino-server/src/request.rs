use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use kino_core::{Id, id::ParseIdError};
use kino_db::Db;
use kino_fulfillment::{Request, RequestDetail, RequestService, RequestTransition};
use serde::{Deserialize, Serialize};

#[derive(Clone)]
struct AppState {
    requests: RequestService,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateRequest {
    message: Option<String>,
}

/// JSON response for listing requests.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListRequestsResponse {
    /// Default request projections.
    pub requests: Vec<Request>,
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
        .with_state(state)
}

async fn create_request(
    State(state): State<AppState>,
    payload: Option<Json<CreateRequest>>,
) -> ApiResult<(StatusCode, Json<RequestDetail>)> {
    let payload = payload.map(|Json(payload)| payload).unwrap_or_default();
    let detail = state
        .requests
        .create(None, payload.message.as_deref())
        .await?;
    Ok((StatusCode::CREATED, Json(detail)))
}

async fn list_requests(State(state): State<AppState>) -> ApiResult<Json<ListRequestsResponse>> {
    let requests = state.requests.list().await?;
    Ok(Json(ListRequestsResponse { requests }))
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
