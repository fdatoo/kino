use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use kino_core::{Id, PlaybackProgress, Timestamp};
use kino_db::Db;
use serde::{Deserialize, Serialize};

use crate::auth::AuthenticatedUser;

#[derive(Clone)]
pub(crate) struct PlaybackState {
    db: Db,
}

#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct PlaybackProgressRequest {
    /// Media item receiving the heartbeat.
    media_item_id: Id,
    /// Non-negative playback position in seconds.
    position_seconds: i64,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(crate) struct PlaybackProgressResponse {
    /// Latest recorded playback position in seconds.
    position_seconds: i64,
    /// Timestamp of the latest recorded progress update.
    updated_at: Timestamp,
    /// Whether the media item is marked watched for the current user.
    watched: bool,
}

pub(crate) fn router(db: Db) -> Router {
    Router::new()
        .route("/api/v1/playback/progress", post(record_progress))
        .route(
            "/api/v1/playback/progress/{media_item_id}",
            get(get_progress),
        )
        .with_state(PlaybackState { db })
}

#[utoipa::path(
    post,
    path = "/api/v1/playback/progress",
    tag = "playback",
    request_body = PlaybackProgressRequest,
    responses(
        (status = 204, description = "Playback progress recorded"),
        (status = 400, description = "Invalid playback progress", body = PlaybackErrorResponse),
        (status = 500, description = "Playback progress write failed", body = PlaybackErrorResponse)
    )
)]
pub(crate) async fn record_progress(
    State(state): State<PlaybackState>,
    AuthenticatedUser { user, token_id }: AuthenticatedUser,
    Json(payload): Json<PlaybackProgressRequest>,
) -> PlaybackResult<StatusCode> {
    let updated_at = Timestamp::now();
    let progress = PlaybackProgress::new(
        user.id,
        payload.media_item_id,
        payload.position_seconds,
        updated_at,
        Some(token_id),
    )?;

    sqlx::query(
        r#"
        INSERT INTO playback_progress (
            user_id,
            media_item_id,
            position_seconds,
            updated_at,
            source_device_token_id
        )
        VALUES (?1, ?2, ?3, ?4, ?5)
        ON CONFLICT(user_id, media_item_id) DO UPDATE SET
            position_seconds = excluded.position_seconds,
            updated_at = excluded.updated_at,
            source_device_token_id = excluded.source_device_token_id
        WHERE excluded.position_seconds > playback_progress.position_seconds
        "#,
    )
    .bind(progress.user_id)
    .bind(progress.media_item_id)
    .bind(progress.position_seconds)
    .bind(progress.updated_at)
    .bind(progress.source_device_token_id)
    .execute(state.db.write_pool())
    .await?;

    sqlx::query(
        r#"
        UPDATE playback_sessions
        SET last_seen_at = ?1
        WHERE user_id = ?2
            AND media_item_id = ?3
            AND status = 'active'
        "#,
    )
    .bind(updated_at)
    .bind(progress.user_id)
    .bind(progress.media_item_id)
    .execute(state.db.write_pool())
    .await?;

    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    get,
    path = "/api/v1/playback/progress/{media_item_id}",
    tag = "playback",
    params(
        ("media_item_id" = Id, Path, description = "Media item id")
    ),
    responses(
        (status = 200, description = "Playback progress found", body = PlaybackProgressResponse),
        (status = 404, description = "Playback progress not found"),
        (status = 500, description = "Playback progress read failed", body = PlaybackErrorResponse)
    )
)]
pub(crate) async fn get_progress(
    State(state): State<PlaybackState>,
    AuthenticatedUser { user, .. }: AuthenticatedUser,
    Path(media_item_id): Path<Id>,
) -> PlaybackResult<Response> {
    let Some((position_seconds, updated_at)) = sqlx::query_as::<_, (i64, Timestamp)>(
        r#"
        SELECT position_seconds, updated_at
        FROM playback_progress
        WHERE user_id = ?1 AND media_item_id = ?2
        "#,
    )
    .bind(user.id)
    .bind(media_item_id)
    .fetch_optional(state.db.read_pool())
    .await?
    else {
        return Ok(StatusCode::NOT_FOUND.into_response());
    };

    let watched = sqlx::query_scalar::<_, i64>(
        r#"
        SELECT 1
        FROM watched
        WHERE user_id = ?1 AND media_item_id = ?2
        LIMIT 1
        "#,
    )
    .bind(user.id)
    .bind(media_item_id)
    .fetch_optional(state.db.read_pool())
    .await?
    .is_some();

    Ok(Json(PlaybackProgressResponse {
        position_seconds,
        updated_at,
        watched,
    })
    .into_response())
}

pub(crate) type PlaybackResult<T> = std::result::Result<T, PlaybackApiError>;

#[derive(Debug, thiserror::Error)]
pub(crate) enum PlaybackApiError {
    #[error(transparent)]
    InvalidPosition(#[from] kino_core::InvalidPlaybackPosition),

    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
}

#[derive(Serialize, utoipa::ToSchema)]
pub(crate) struct PlaybackErrorResponse {
    error: String,
}

impl IntoResponse for PlaybackApiError {
    fn into_response(self) -> Response {
        let status = match &self {
            Self::InvalidPosition(_) => StatusCode::BAD_REQUEST,
            Self::Sqlx(_) => {
                tracing::error!(error = %self, "playback api failed");
                StatusCode::INTERNAL_SERVER_ERROR
            }
        };

        (
            status,
            Json(PlaybackErrorResponse {
                error: self.to_string(),
            }),
        )
            .into_response()
    }
}
