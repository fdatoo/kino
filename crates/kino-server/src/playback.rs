use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use kino_core::{Id, PlaybackProgress, Timestamp, Watched, WatchedSource};
use kino_db::Db;
use serde::{Deserialize, Serialize};

use crate::auth::AuthenticatedUser;

const WATCHED_THRESHOLD_NUMERATOR: i128 = 90;
const WATCHED_THRESHOLD_DENOMINATOR: i128 = 100;

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
        .route(
            "/api/v1/playback/watched/{media_item_id}",
            post(mark_watched).delete(unmark_watched),
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

    auto_mark_watched(
        &state.db,
        progress.user_id,
        progress.media_item_id,
        progress.position_seconds,
        updated_at,
    )
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
        WHERE user_id = ?1
            AND media_item_id = ?2
            AND unmarked = 0
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

#[utoipa::path(
    post,
    path = "/api/v1/playback/watched/{media_item_id}",
    tag = "playback",
    params(
        ("media_item_id" = Id, Path, description = "Media item to mark watched")
    ),
    responses(
        (status = 204, description = "Media item marked watched"),
        (status = 500, description = "Watched marker write failed", body = PlaybackErrorResponse)
    )
)]
pub(crate) async fn mark_watched(
    State(state): State<PlaybackState>,
    AuthenticatedUser { user, .. }: AuthenticatedUser,
    Path(media_item_id): Path<Id>,
) -> PlaybackResult<StatusCode> {
    let watched = Watched::new(
        user.id,
        media_item_id,
        Timestamp::now(),
        WatchedSource::Manual,
    );

    sqlx::query(
        r#"
        INSERT INTO watched (
            user_id,
            media_item_id,
            watched_at,
            source,
            unmarked
        )
        VALUES (?1, ?2, ?3, ?4, ?5)
        ON CONFLICT(user_id, media_item_id) DO UPDATE SET
            watched_at = excluded.watched_at,
            source = excluded.source,
            unmarked = excluded.unmarked
        "#,
    )
    .bind(watched.user_id)
    .bind(watched.media_item_id)
    .bind(watched.watched_at)
    .bind(watched.source.as_str())
    .bind(watched.unmarked)
    .execute(state.db.write_pool())
    .await?;

    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    delete,
    path = "/api/v1/playback/watched/{media_item_id}",
    tag = "playback",
    params(
        ("media_item_id" = Id, Path, description = "Media item to unmark watched")
    ),
    responses(
        (status = 204, description = "Media item unmarked watched"),
        (status = 500, description = "Watched marker write failed", body = PlaybackErrorResponse)
    )
)]
pub(crate) async fn unmark_watched(
    State(state): State<PlaybackState>,
    AuthenticatedUser { user, .. }: AuthenticatedUser,
    Path(media_item_id): Path<Id>,
) -> PlaybackResult<StatusCode> {
    let watched = Watched::manual_unmarked(user.id, media_item_id, Timestamp::UNIX_EPOCH);

    sqlx::query(
        r#"
        INSERT INTO watched (
            user_id,
            media_item_id,
            watched_at,
            source,
            unmarked
        )
        VALUES (?1, ?2, ?3, ?4, ?5)
        ON CONFLICT(user_id, media_item_id) DO UPDATE SET
            watched_at = excluded.watched_at,
            source = excluded.source,
            unmarked = excluded.unmarked
        "#,
    )
    .bind(watched.user_id)
    .bind(watched.media_item_id)
    .bind(watched.watched_at)
    .bind(watched.source.as_str())
    .bind(watched.unmarked)
    .execute(state.db.write_pool())
    .await?;

    Ok(StatusCode::NO_CONTENT)
}

async fn auto_mark_watched(
    db: &Db,
    user_id: Id,
    media_item_id: Id,
    position_seconds: i64,
    watched_at: Timestamp,
) -> PlaybackResult<()> {
    let Some(duration_seconds) = max_probe_duration_seconds(db, media_item_id).await? else {
        return Ok(());
    };

    if !meets_watched_threshold(position_seconds, duration_seconds) {
        return Ok(());
    }

    let watched = Watched::new(user_id, media_item_id, watched_at, WatchedSource::Auto);

    sqlx::query(
        r#"
        INSERT INTO watched (
            user_id,
            media_item_id,
            watched_at,
            source,
            unmarked
        )
        VALUES (?1, ?2, ?3, ?4, ?5)
        ON CONFLICT(user_id, media_item_id) DO NOTHING
        "#,
    )
    .bind(watched.user_id)
    .bind(watched.media_item_id)
    .bind(watched.watched_at)
    .bind(watched.source.as_str())
    .bind(watched.unmarked)
    .execute(db.write_pool())
    .await?;

    Ok(())
}

async fn max_probe_duration_seconds(db: &Db, media_item_id: Id) -> PlaybackResult<Option<i64>> {
    let duration = sqlx::query_scalar(
        r#"
        SELECT MAX(probe_duration_seconds)
        FROM source_files
        WHERE media_item_id = ?1
        "#,
    )
    .bind(media_item_id)
    .fetch_one(db.read_pool())
    .await?;

    Ok(duration)
}

fn meets_watched_threshold(position_seconds: i64, duration_seconds: i64) -> bool {
    if duration_seconds <= 0 {
        return false;
    }

    i128::from(position_seconds) * WATCHED_THRESHOLD_DENOMINATOR
        >= i128::from(duration_seconds) * WATCHED_THRESHOLD_NUMERATOR
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
