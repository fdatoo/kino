use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
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

pub(crate) fn router(db: Db) -> Router {
    Router::new()
        .route("/api/v1/playback/progress", post(record_progress))
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
