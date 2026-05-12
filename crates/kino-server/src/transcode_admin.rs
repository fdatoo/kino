use std::{collections::BTreeSet, path::PathBuf, sync::Arc};

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use kino_core::{FfprobeFileProbe, Id, ProbeResult, Timestamp, id::ParseIdError};
use kino_db::Db;
use kino_transcode::{
    Capabilities, EncoderRegistry, JobState, JobStore, LaneId, ListJobsFilter, SourceContext,
    TranscodeJob, TranscodeService, VideoCodec,
};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use tokio::task::JoinError;

#[derive(Clone)]
pub(crate) struct TranscodeAdminState {
    db: Db,
    store: JobStore,
    service: Arc<TranscodeService>,
    encoders: Arc<EncoderRegistry>,
}

#[derive(Debug, Clone, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub(crate) struct ListTranscodeJobsQuery {
    /// Durable transcode job state to include.
    state: Option<String>,
    /// Encoder lane to include.
    lane: Option<String>,
    /// Source file id to include.
    source_file_id: Option<String>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct TranscodeJobSummary {
    /// Job id.
    id: Id,
    /// Source file this job transcodes.
    source_file_id: Id,
    /// Durable scheduler state.
    state: String,
    /// Resource lane this job must run on.
    lane: String,
    /// Number of recorded dispatch attempts.
    attempts: u32,
    /// Most recent runner progress percentage.
    progress_pct: Option<u8>,
    /// Last failure message recorded for operator visibility.
    last_error: Option<String>,
    /// Earliest time this job may be retried.
    next_attempt_at: Option<Timestamp>,
    /// Row creation timestamp.
    created_at: Timestamp,
    /// Last row update timestamp.
    updated_at: Timestamp,
    /// Time the current or most recent active attempt started.
    started_at: Option<Timestamp>,
    /// Terminal completion timestamp.
    completed_at: Option<Timestamp>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct TranscodeJobDetail {
    /// Job id.
    id: Id,
    /// Source file this job transcodes.
    source_file_id: Id,
    /// Canonical JSON profile used to plan the transcode.
    profile_json: String,
    /// Hex-encoded SHA-256 digest of `profile_json`.
    profile_hash: String,
    /// Durable scheduler state.
    state: String,
    /// Resource lane this job must run on.
    lane: String,
    /// Number of recorded dispatch attempts.
    attempts: u32,
    /// Most recent runner progress percentage.
    progress_pct: Option<u8>,
    /// Last failure message recorded for operator visibility.
    last_error: Option<String>,
    /// Earliest time this job may be retried.
    next_attempt_at: Option<Timestamp>,
    /// Row creation timestamp.
    created_at: Timestamp,
    /// Last row update timestamp.
    updated_at: Timestamp,
    /// Time the current or most recent active attempt started.
    started_at: Option<Timestamp>,
    /// Terminal completion timestamp.
    completed_at: Option<Timestamp>,
    /// Color downgrade records associated with this source's outputs.
    downgrades: Vec<TranscodeColorDowngrade>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct TranscodeColorDowngrade {
    /// Transcode output that recorded the downgrade.
    transcode_output_id: Id,
    /// Stable downgrade kind.
    kind: String,
    /// Optional operator note.
    note: Option<String>,
    /// Row creation timestamp.
    created_at: Timestamp,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct TranscodeActionResponse {
    /// Jobs newly planned by the action.
    job_ids: Vec<Id>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct TranscodeCancelResponse {
    /// Cancelled job id.
    job_id: Id,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct EncoderInfo {
    /// Encoder backend family.
    kind: String,
    /// Resource lane used by this backend.
    lane: String,
    /// Static capability declaration.
    capabilities: EncoderCapabilities,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct EncoderCapabilities {
    /// Supported video codec ids.
    codecs: Vec<String>,
    /// Maximum supported output width in pixels.
    max_width: u32,
    /// Maximum supported output height in pixels.
    max_height: u32,
    /// Whether this encoder can produce 10-bit output.
    ten_bit: bool,
    /// Whether this encoder can produce HDR10 output.
    hdr10: bool,
    /// Whether this encoder can preserve Dolby Vision on stream-copy paths.
    dv_passthrough: bool,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct TranscodeCacheStats {
    /// Durable transcode output cache stats.
    durable: DurableTranscodeCacheStats,
    /// Ephemeral cache stats, present when the ephemeral table exists.
    ephemeral: Option<EphemeralTranscodeCacheStats>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct DurableTranscodeCacheStats {
    /// Number of durable transcode output rows.
    row_count: u64,
    /// Total bytes currently present on disk under output directories.
    total_bytes: u64,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct EphemeralTranscodeCacheStats {
    /// Number of ephemeral transcode rows.
    row_count: u64,
    /// Total bytes recorded for ephemeral rows.
    total_bytes: u64,
    /// Oldest access timestamp among ephemeral rows.
    oldest_last_access_at: Option<Timestamp>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct TranscodeCachePurgeResponse {
    /// Number of ephemeral rows removed.
    purged_rows: u64,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(crate) struct TranscodeAdminErrorResponse {
    error: String,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum TranscodeAdminError {
    #[error("invalid id {value}: {source}")]
    InvalidId { value: String, source: ParseIdError },

    #[error(transparent)]
    Transcode(#[from] kino_transcode::Error),

    #[error("probing source file failed: {0}")]
    Probe(#[from] kino_core::ProbeError),

    #[error("transcode admin database operation failed: {0}")]
    Sqlx(#[from] sqlx::Error),

    #[error("transcode cache filesystem operation failed: {0}")]
    Io(#[from] std::io::Error),

    #[error("transcode cache filesystem task failed: {0}")]
    Join(#[from] JoinError),
}

type TranscodeAdminResult<T> = std::result::Result<T, TranscodeAdminError>;

pub(crate) fn router(
    db: Db,
    service: Arc<TranscodeService>,
    encoders: Arc<EncoderRegistry>,
) -> Router {
    let state = TranscodeAdminState {
        store: JobStore::new(db.clone()),
        db,
        service,
        encoders,
    };

    Router::new()
        .route("/api/v1/admin/transcodes/jobs", get(list_jobs))
        .route("/api/v1/admin/transcodes/jobs/{id}", get(get_job))
        .route(
            "/api/v1/admin/transcodes/jobs/{id}/cancel",
            post(cancel_job),
        )
        .route(
            "/api/v1/admin/transcodes/sources/{source_file_id}/replan",
            post(replan_source),
        )
        .route(
            "/api/v1/admin/transcodes/sources/{source_file_id}/retranscode",
            post(retranscode_source),
        )
        .route("/api/v1/admin/transcodes/encoders", get(list_encoders))
        .route("/api/v1/admin/transcodes/cache", get(cache_stats))
        .route("/api/v1/admin/transcodes/cache/purge", post(purge_cache))
        .with_state(state)
}

#[utoipa::path(
    get,
    path = "/api/v1/admin/transcodes/jobs",
    tag = "admin",
    params(ListTranscodeJobsQuery),
    responses(
        (status = 200, description = "Transcode jobs matching the filters", body = [TranscodeJobSummary]),
        (status = 400, description = "Invalid transcode job filter", body = TranscodeAdminErrorResponse),
        (status = 500, description = "Transcode job list failed", body = TranscodeAdminErrorResponse)
    )
)]
pub(crate) async fn list_jobs(
    State(state): State<TranscodeAdminState>,
    Query(query): Query<ListTranscodeJobsQuery>,
) -> TranscodeAdminResult<Json<Vec<TranscodeJobSummary>>> {
    let filter = ListJobsFilter {
        state: query
            .state
            .as_deref()
            .map(str::parse::<JobState>)
            .transpose()?,
        lane: query
            .lane
            .as_deref()
            .map(str::parse::<LaneId>)
            .transpose()?,
        source_file_id: query.source_file_id.map(parse_id).transpose()?,
    };
    let jobs = state.store.list_jobs(&filter).await?;

    Ok(Json(
        jobs.into_iter().map(TranscodeJobSummary::from).collect(),
    ))
}

#[utoipa::path(
    get,
    path = "/api/v1/admin/transcodes/jobs/{id}",
    tag = "admin",
    params(("id" = Id, Path, description = "Transcode job id")),
    responses(
        (status = 200, description = "Transcode job detail", body = TranscodeJobDetail),
        (status = 400, description = "Invalid transcode job id", body = TranscodeAdminErrorResponse),
        (status = 404, description = "Transcode job not found", body = TranscodeAdminErrorResponse),
        (status = 500, description = "Transcode job read failed", body = TranscodeAdminErrorResponse)
    )
)]
pub(crate) async fn get_job(
    State(state): State<TranscodeAdminState>,
    Path(id): Path<String>,
) -> TranscodeAdminResult<Json<TranscodeJobDetail>> {
    let job = state.store.fetch_job(parse_id(id)?).await?;
    let downgrades = source_downgrades(&state.db, job.source_file_id).await?;

    Ok(Json(TranscodeJobDetail::from_job(job, downgrades)))
}

#[utoipa::path(
    post,
    path = "/api/v1/admin/transcodes/jobs/{id}/cancel",
    tag = "admin",
    params(("id" = Id, Path, description = "Transcode job id")),
    responses(
        (status = 200, description = "Transcode job cancelled", body = TranscodeCancelResponse),
        (status = 400, description = "Invalid transcode job id", body = TranscodeAdminErrorResponse),
        (status = 404, description = "Transcode job not found", body = TranscodeAdminErrorResponse),
        (status = 409, description = "Transcode job cannot be cancelled from its current state", body = TranscodeAdminErrorResponse),
        (status = 500, description = "Transcode job cancellation failed", body = TranscodeAdminErrorResponse)
    )
)]
pub(crate) async fn cancel_job(
    State(state): State<TranscodeAdminState>,
    Path(id): Path<String>,
) -> TranscodeAdminResult<Json<TranscodeCancelResponse>> {
    let job_id = parse_id(id)?;
    state.service.cancel(job_id).await?;

    Ok(Json(TranscodeCancelResponse { job_id }))
}

#[utoipa::path(
    post,
    path = "/api/v1/admin/transcodes/sources/{source_file_id}/replan",
    tag = "admin",
    params(("source_file_id" = Id, Path, description = "Source file id")),
    responses(
        (status = 200, description = "Source file replanned", body = TranscodeActionResponse),
        (status = 400, description = "Invalid source file id", body = TranscodeAdminErrorResponse),
        (status = 404, description = "Source file not found", body = TranscodeAdminErrorResponse),
        (status = 503, description = "Source file probe failed", body = TranscodeAdminErrorResponse),
        (status = 500, description = "Source file replan failed", body = TranscodeAdminErrorResponse)
    )
)]
pub(crate) async fn replan_source(
    State(state): State<TranscodeAdminState>,
    Path(source_file_id): Path<String>,
) -> TranscodeAdminResult<Json<TranscodeActionResponse>> {
    let source = source_context(&state.store, parse_id(source_file_id)?).await?;
    let job_ids = state.service.replan(source).await?;

    Ok(Json(TranscodeActionResponse { job_ids }))
}

#[utoipa::path(
    post,
    path = "/api/v1/admin/transcodes/sources/{source_file_id}/retranscode",
    tag = "admin",
    params(("source_file_id" = Id, Path, description = "Source file id")),
    responses(
        (status = 200, description = "Source file retranscode planned", body = TranscodeActionResponse),
        (status = 400, description = "Invalid source file id", body = TranscodeAdminErrorResponse),
        (status = 404, description = "Source file not found", body = TranscodeAdminErrorResponse),
        (status = 503, description = "Source file probe failed", body = TranscodeAdminErrorResponse),
        (status = 500, description = "Source file retranscode failed", body = TranscodeAdminErrorResponse)
    )
)]
pub(crate) async fn retranscode_source(
    State(state): State<TranscodeAdminState>,
    Path(source_file_id): Path<String>,
) -> TranscodeAdminResult<Json<TranscodeActionResponse>> {
    let source = source_context(&state.store, parse_id(source_file_id)?).await?;
    let job_ids = state.service.retranscode(source).await?;

    Ok(Json(TranscodeActionResponse { job_ids }))
}

#[utoipa::path(
    get,
    path = "/api/v1/admin/transcodes/encoders",
    tag = "admin",
    responses(
        (status = 200, description = "Detected transcode encoders", body = [EncoderInfo])
    )
)]
pub(crate) async fn list_encoders(
    State(state): State<TranscodeAdminState>,
) -> Json<Vec<EncoderInfo>> {
    Json(
        state
            .encoders
            .encoders()
            .iter()
            .map(|encoder| EncoderInfo {
                kind: encoder.kind().as_str().to_owned(),
                lane: encoder.lane().as_str().to_owned(),
                capabilities: EncoderCapabilities::from(encoder.capabilities()),
            })
            .collect(),
    )
}

#[utoipa::path(
    get,
    path = "/api/v1/admin/transcodes/cache",
    tag = "admin",
    responses(
        (status = 200, description = "Transcode cache stats", body = TranscodeCacheStats),
        (status = 500, description = "Transcode cache stats failed", body = TranscodeAdminErrorResponse)
    )
)]
pub(crate) async fn cache_stats(
    State(state): State<TranscodeAdminState>,
) -> TranscodeAdminResult<Json<TranscodeCacheStats>> {
    let row_count = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM transcode_outputs")
        .fetch_one(state.db.read_pool())
        .await?;
    let durable_paths = durable_output_dirs(&state.db).await?;
    let total_bytes = total_directory_bytes(durable_paths).await?;
    let ephemeral = ephemeral_stats(&state.db).await?;

    Ok(Json(TranscodeCacheStats {
        durable: DurableTranscodeCacheStats {
            row_count: u64_from_i64(row_count),
            total_bytes,
        },
        ephemeral,
    }))
}

#[utoipa::path(
    post,
    path = "/api/v1/admin/transcodes/cache/purge",
    tag = "admin",
    responses(
        (status = 200, description = "Ephemeral transcode cache purged", body = TranscodeCachePurgeResponse),
        (status = 500, description = "Transcode cache purge failed", body = TranscodeAdminErrorResponse)
    )
)]
pub(crate) async fn purge_cache(
    State(state): State<TranscodeAdminState>,
) -> TranscodeAdminResult<Json<TranscodeCachePurgeResponse>> {
    if !ephemeral_table_exists(&state.db).await? {
        return Ok(Json(TranscodeCachePurgeResponse { purged_rows: 0 }));
    }

    let dirs = ephemeral_output_dirs(&state.db).await?;
    remove_directories(dirs).await?;
    let result = sqlx::query("DELETE FROM ephemeral_transcodes")
        .execute(state.db.write_pool())
        .await?;

    Ok(Json(TranscodeCachePurgeResponse {
        purged_rows: result.rows_affected(),
    }))
}

impl From<TranscodeJob> for TranscodeJobSummary {
    fn from(job: TranscodeJob) -> Self {
        Self {
            id: job.id,
            source_file_id: job.source_file_id,
            state: job.state.as_str().to_owned(),
            lane: job.lane.as_str().to_owned(),
            attempts: job.attempt,
            progress_pct: job.progress_pct,
            last_error: job.last_error,
            next_attempt_at: job.next_attempt_at,
            created_at: job.created_at,
            updated_at: job.updated_at,
            started_at: job.started_at,
            completed_at: job.completed_at,
        }
    }
}

impl TranscodeJobDetail {
    fn from_job(job: TranscodeJob, downgrades: Vec<TranscodeColorDowngrade>) -> Self {
        Self {
            id: job.id,
            source_file_id: job.source_file_id,
            profile_json: job.profile_json,
            profile_hash: hex_bytes(&job.profile_hash),
            state: job.state.as_str().to_owned(),
            lane: job.lane.as_str().to_owned(),
            attempts: job.attempt,
            progress_pct: job.progress_pct,
            last_error: job.last_error,
            next_attempt_at: job.next_attempt_at,
            created_at: job.created_at,
            updated_at: job.updated_at,
            started_at: job.started_at,
            completed_at: job.completed_at,
            downgrades,
        }
    }
}

impl From<&Capabilities> for EncoderCapabilities {
    fn from(capabilities: &Capabilities) -> Self {
        Self {
            codecs: codec_names(capabilities.codecs()),
            max_width: capabilities.max_width(),
            max_height: capabilities.max_height(),
            ten_bit: capabilities.ten_bit(),
            hdr10: capabilities.hdr10(),
            dv_passthrough: capabilities.dv_passthrough(),
        }
    }
}

impl IntoResponse for TranscodeAdminError {
    fn into_response(self) -> Response {
        let status = match &self {
            Self::InvalidId { .. }
            | Self::Transcode(kino_transcode::Error::InvalidJobState(_))
            | Self::Transcode(kino_transcode::Error::InvalidLaneId(_)) => StatusCode::BAD_REQUEST,
            Self::Transcode(kino_transcode::Error::JobNotFound { .. })
            | Self::Transcode(kino_transcode::Error::SourceFileNotFound { .. }) => {
                StatusCode::NOT_FOUND
            }
            Self::Transcode(kino_transcode::Error::InvalidTransition { .. }) => {
                StatusCode::CONFLICT
            }
            Self::Probe(_) => StatusCode::SERVICE_UNAVAILABLE,
            Self::Transcode(_) | Self::Sqlx(_) | Self::Io(_) | Self::Join(_) => {
                tracing::error!(error = %self, "transcode admin api failed");
                StatusCode::INTERNAL_SERVER_ERROR
            }
        };

        (
            status,
            Json(TranscodeAdminErrorResponse {
                error: self.to_string(),
            }),
        )
            .into_response()
    }
}

fn parse_id(value: String) -> TranscodeAdminResult<Id> {
    value
        .parse()
        .map_err(|source| TranscodeAdminError::InvalidId { value, source })
}

async fn source_context(
    store: &JobStore,
    source_file_id: Id,
) -> TranscodeAdminResult<SourceContext> {
    let path = store.source_path(source_file_id).await?;
    let probe = probe_source(&path).await?;

    Ok(SourceContext {
        source_file_id,
        probe,
    })
}

async fn probe_source(path: &std::path::Path) -> TranscodeAdminResult<ProbeResult> {
    Ok(FfprobeFileProbe::new().probe(path).await?)
}

async fn source_downgrades(
    db: &Db,
    source_file_id: Id,
) -> TranscodeAdminResult<Vec<TranscodeColorDowngrade>> {
    let rows = sqlx::query(
        r#"
        SELECT
            transcode_color_downgrades.transcode_output_id,
            transcode_color_downgrades.kind,
            transcode_color_downgrades.note,
            transcode_color_downgrades.created_at
        FROM transcode_color_downgrades
        INNER JOIN transcode_outputs
            ON transcode_outputs.id = transcode_color_downgrades.transcode_output_id
        WHERE transcode_outputs.source_file_id = ?1
        ORDER BY transcode_color_downgrades.created_at ASC
        "#,
    )
    .bind(source_file_id)
    .fetch_all(db.read_pool())
    .await?;

    rows.iter()
        .map(|row| {
            Ok(TranscodeColorDowngrade {
                transcode_output_id: row.try_get("transcode_output_id")?,
                kind: row.try_get("kind")?,
                note: row.try_get("note")?,
                created_at: row.try_get("created_at")?,
            })
        })
        .collect()
}

async fn durable_output_dirs(db: &Db) -> TranscodeAdminResult<Vec<PathBuf>> {
    let rows = sqlx::query(
        r#"
        SELECT directory_path, path
        FROM transcode_outputs
        "#,
    )
    .fetch_all(db.read_pool())
    .await?;

    rows.iter()
        .map(|row| {
            let directory_path: Option<String> = row.try_get("directory_path")?;
            if let Some(path) = directory_path.filter(|value| !value.is_empty()) {
                return Ok(PathBuf::from(path));
            }

            let path: String = row.try_get("path")?;
            Ok(PathBuf::from(path)
                .parent()
                .map(std::path::Path::to_path_buf)
                .unwrap_or_default())
        })
        .collect()
}

async fn ephemeral_stats(db: &Db) -> TranscodeAdminResult<Option<EphemeralTranscodeCacheStats>> {
    if !ephemeral_table_exists(db).await? {
        return Ok(None);
    }

    let row = sqlx::query(
        r#"
        SELECT
            COUNT(*) AS row_count,
            COALESCE(SUM(size_bytes), 0) AS total_bytes,
            MIN(last_access_at) AS oldest_last_access_at
        FROM ephemeral_transcodes
        "#,
    )
    .fetch_one(db.read_pool())
    .await?;

    Ok(Some(EphemeralTranscodeCacheStats {
        row_count: u64_from_i64(row.try_get("row_count")?),
        total_bytes: u64_from_i64(row.try_get("total_bytes")?),
        oldest_last_access_at: row.try_get("oldest_last_access_at")?,
    }))
}

async fn ephemeral_output_dirs(db: &Db) -> TranscodeAdminResult<Vec<PathBuf>> {
    if !ephemeral_table_exists(db).await? {
        return Ok(Vec::new());
    }

    let rows = sqlx::query("SELECT directory_path FROM ephemeral_transcodes")
        .fetch_all(db.read_pool())
        .await?;

    rows.iter()
        .map(|row| {
            let path: String = row.try_get("directory_path")?;
            Ok(PathBuf::from(path))
        })
        .collect()
}

async fn ephemeral_table_exists(db: &Db) -> TranscodeAdminResult<bool> {
    let exists = sqlx::query_scalar::<_, i64>(
        r#"
        SELECT COUNT(*)
        FROM sqlite_master
        WHERE type = 'table'
          AND name = 'ephemeral_transcodes'
        "#,
    )
    .fetch_one(db.read_pool())
    .await?;

    Ok(exists > 0)
}

async fn total_directory_bytes(paths: Vec<PathBuf>) -> TranscodeAdminResult<u64> {
    tokio::task::spawn_blocking(move || {
        let mut total = 0_u64;
        for path in unique_paths(paths) {
            total = total.saturating_add(directory_size(&path)?);
        }
        Ok::<u64, std::io::Error>(total)
    })
    .await?
    .map_err(Into::into)
}

async fn remove_directories(paths: Vec<PathBuf>) -> TranscodeAdminResult<()> {
    tokio::task::spawn_blocking(move || {
        for path in unique_paths(paths) {
            match std::fs::remove_dir_all(&path) {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => return Err(err),
            }
        }
        Ok::<(), std::io::Error>(())
    })
    .await?
    .map_err(Into::into)
}

fn unique_paths(paths: Vec<PathBuf>) -> BTreeSet<PathBuf> {
    paths
        .into_iter()
        .filter(|path| !path.as_os_str().is_empty())
        .collect()
}

fn directory_size(path: &std::path::Path) -> std::io::Result<u64> {
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => return Ok(metadata.len()),
        Ok(metadata) if !metadata.is_dir() => return Ok(0),
        Ok(_) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(err) => return Err(err),
    }

    let mut total = 0_u64;
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let metadata = entry.metadata()?;
        if metadata.is_dir() {
            total = total.saturating_add(directory_size(&entry.path())?);
        } else if metadata.is_file() {
            total = total.saturating_add(metadata.len());
        }
    }

    Ok(total)
}

fn codec_names(codecs: &BTreeSet<VideoCodec>) -> Vec<String> {
    codecs
        .iter()
        .map(|codec| codec.as_str().to_owned())
        .collect()
}

fn hex_bytes(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

fn u64_from_i64(value: i64) -> u64 {
    u64::try_from(value).unwrap_or_default()
}
