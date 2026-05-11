use axum::{
    Json, Router,
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use kino_core::{Id, PlaybackSessionStatus, Timestamp};
use kino_db::Db;
use serde::{Deserialize, Serialize};

#[derive(Clone)]
pub(crate) struct SessionAdminState {
    db: Db,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawListSessionsQuery {
    status: Option<String>,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(crate) struct AdminPlaybackSession {
    /// Playback session id.
    id: Id,
    /// User that owns the session.
    user_id: Id,
    /// Device token associated with the session.
    token_id: Id,
    /// Media item being watched.
    media_item_id: Id,
    /// Opaque playable variant identifier.
    variant_id: String,
    /// Current playback session status.
    status: PlaybackSessionStatus,
    /// Session start timestamp.
    started_at: Timestamp,
    /// Most recent heartbeat timestamp.
    last_seen_at: Timestamp,
    /// Session end timestamp, present only once status is ended.
    ended_at: Option<Timestamp>,
}

struct PlaybackSessionRow {
    id: Id,
    user_id: Id,
    token_id: Id,
    media_item_id: Id,
    variant_id: String,
    started_at: Timestamp,
    last_seen_at: Timestamp,
    ended_at: Option<Timestamp>,
    status: String,
}

pub(crate) fn router(db: Db) -> Router {
    Router::new()
        .route("/api/v1/admin/sessions", get(list_sessions))
        .with_state(SessionAdminState { db })
}

#[utoipa::path(
    get,
    path = "/api/v1/admin/sessions",
    tag = "admin",
    params(
        ("status" = Option<PlaybackSessionStatus>, Query, description = "Playback session status to include")
    ),
    responses(
        (status = 200, description = "Playback sessions visible to admin", body = [AdminPlaybackSession]),
        (status = 400, description = "Invalid playback session filter", body = SessionAdminErrorResponse),
        (status = 500, description = "Playback session list failed", body = SessionAdminErrorResponse)
    )
)]
pub(crate) async fn list_sessions(
    State(state): State<SessionAdminState>,
    Query(query): Query<RawListSessionsQuery>,
) -> SessionAdminResult<Json<Vec<AdminPlaybackSession>>> {
    let status = query
        .status
        .as_deref()
        .map(parse_requested_status)
        .transpose()?;

    let sessions = match status {
        Some(status) => fetch_sessions(&state.db, &[status]).await?,
        None => {
            fetch_sessions(
                &state.db,
                &[PlaybackSessionStatus::Active, PlaybackSessionStatus::Idle],
            )
            .await?
        }
    };

    Ok(Json(sessions))
}

async fn fetch_sessions(
    db: &Db,
    statuses: &[PlaybackSessionStatus],
) -> SessionAdminResult<Vec<AdminPlaybackSession>> {
    let mut query = sqlx::QueryBuilder::new(
        r#"
        SELECT
            id,
            user_id,
            token_id,
            media_item_id,
            variant_id,
            started_at,
            last_seen_at,
            ended_at,
            status
        FROM playback_sessions
        WHERE status IN (
        "#,
    );

    let mut separated = query.separated(", ");
    for status in statuses {
        separated.push_bind(status.as_str());
    }
    separated.push_unseparated(
        r#"
        )
        ORDER BY last_seen_at DESC, id
        "#,
    );

    let rows = query
        .build_query_as::<(
            Id,
            Id,
            Id,
            Id,
            String,
            Timestamp,
            Timestamp,
            Option<Timestamp>,
            String,
        )>()
        .fetch_all(db.read_pool())
        .await?;

    rows.into_iter()
        .map(|row| {
            AdminPlaybackSession::try_from(PlaybackSessionRow {
                id: row.0,
                user_id: row.1,
                token_id: row.2,
                media_item_id: row.3,
                variant_id: row.4,
                started_at: row.5,
                last_seen_at: row.6,
                ended_at: row.7,
                status: row.8,
            })
        })
        .collect()
}

pub(crate) type SessionAdminResult<T> = std::result::Result<T, SessionAdminApiError>;

#[derive(Debug, thiserror::Error)]
pub(crate) enum SessionAdminApiError {
    #[error("invalid playback session status filter: {0}")]
    InvalidRequestedStatus(String),

    #[error("invalid playback session status in database: {0}")]
    InvalidPersistedStatus(String),

    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
}

#[derive(Serialize, utoipa::ToSchema)]
pub(crate) struct SessionAdminErrorResponse {
    error: String,
}

impl IntoResponse for SessionAdminApiError {
    fn into_response(self) -> Response {
        let status = match &self {
            Self::InvalidRequestedStatus(_) => StatusCode::BAD_REQUEST,
            Self::InvalidPersistedStatus(_) | Self::Sqlx(_) => {
                tracing::error!(error = %self, "session admin api failed");
                StatusCode::INTERNAL_SERVER_ERROR
            }
        };

        (
            status,
            Json(SessionAdminErrorResponse {
                error: self.to_string(),
            }),
        )
            .into_response()
    }
}

fn parse_requested_status(status: &str) -> SessionAdminResult<PlaybackSessionStatus> {
    PlaybackSessionStatus::parse(status)
        .ok_or_else(|| SessionAdminApiError::InvalidRequestedStatus(status.to_owned()))
}

impl TryFrom<PlaybackSessionRow> for AdminPlaybackSession {
    type Error = SessionAdminApiError;

    fn try_from(row: PlaybackSessionRow) -> SessionAdminResult<Self> {
        let status = PlaybackSessionStatus::parse(&row.status)
            .ok_or_else(|| SessionAdminApiError::InvalidPersistedStatus(row.status.clone()))?;

        Ok(Self {
            id: row.id,
            user_id: row.user_id,
            token_id: row.token_id,
            media_item_id: row.media_item_id,
            variant_id: row.variant_id,
            status,
            started_at: row.started_at,
            last_seen_at: row.last_seen_at,
            ended_at: row.ended_at,
        })
    }
}
