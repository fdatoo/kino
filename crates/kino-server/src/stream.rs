use std::{
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use axum::{
    Json, Router,
    body::Body,
    extract::{Path as AxumPath, State},
    http::{
        HeaderMap, HeaderValue, StatusCode,
        header::{ACCEPT_RANGES, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, ETAG, RANGE},
    },
    response::{IntoResponse, Response},
    routing::get,
};
use kino_core::Id;
use kino_db::Db;
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::Row;
use tokio::io::{AsyncReadExt as _, AsyncSeekExt as _, SeekFrom};
use tokio_util::io::ReaderStream;

use crate::{auth::AuthenticatedUser, session_service};

#[derive(Clone)]
pub(crate) struct StreamState {
    db: Db,
}

pub(crate) fn router(db: Db) -> Router {
    Router::new()
        .route("/api/v1/stream/sourcefile/{id}", get(source_file))
        .route("/api/v1/stream/transcode/{id}", get(transcode_output))
        .with_state(StreamState { db })
}

/// Serve a source file with single-range support.
///
/// Multi-range requests such as `Range: bytes=0-99,200-299` are rejected with
/// `416 Range Not Satisfiable` until segment callers need multipart responses.
#[utoipa::path(
    get,
    path = "/api/v1/stream/sourcefile/{id}",
    tag = "stream",
    params(
        ("id" = Id, Path, description = "Source-file id"),
        ("Range" = Option<String>, Header, description = "Single HTTP byte range")
    ),
    responses(
        (status = 200, description = "Full source file"),
        (status = 206, description = "Requested byte range"),
        (status = 404, description = "Source file not found", body = StreamErrorResponse),
        (status = 416, description = "Requested range is not satisfiable", body = StreamErrorResponse),
        (status = 500, description = "Source file stream failed", body = StreamErrorResponse)
    )
)]
pub(crate) async fn source_file(
    State(state): State<StreamState>,
    AuthenticatedUser { user, token_id }: AuthenticatedUser,
    AxumPath(id): AxumPath<Id>,
    headers: HeaderMap,
) -> StreamResult<Response> {
    let source_file = lookup_source_file(&state.db, id).await?;
    session_service::heartbeat_or_open_session(
        &state.db,
        user.id,
        token_id,
        source_file.media_item_id,
        id.to_string(),
    )
    .await?;
    stream_file(source_file.path, headers).await
}

/// Serve a transcode output with single-range support.
#[utoipa::path(
    get,
    path = "/api/v1/stream/transcode/{id}",
    tag = "stream",
    params(
        ("id" = Id, Path, description = "Transcode-output id"),
        ("Range" = Option<String>, Header, description = "Single HTTP byte range")
    ),
    responses(
        (status = 200, description = "Full transcode output"),
        (status = 206, description = "Requested byte range"),
        (status = 404, description = "Transcode output not found", body = StreamErrorResponse),
        (status = 416, description = "Requested range is not satisfiable", body = StreamErrorResponse),
        (status = 500, description = "Transcode output stream failed", body = StreamErrorResponse)
    )
)]
pub(crate) async fn transcode_output(
    State(state): State<StreamState>,
    AuthenticatedUser { user, token_id }: AuthenticatedUser,
    AxumPath(id): AxumPath<Id>,
    headers: HeaderMap,
) -> StreamResult<Response> {
    let transcode_output = lookup_transcode_output(&state.db, id).await?;
    session_service::heartbeat_or_open_session(
        &state.db,
        user.id,
        token_id,
        transcode_output.media_item_id,
        id.to_string(),
    )
    .await?;
    stream_file(transcode_output.path, headers).await
}

async fn stream_file(path: PathBuf, headers: HeaderMap) -> StreamResult<Response> {
    let mut file = tokio::fs::File::open(&path).await?;
    let metadata = file.metadata().await?;
    let file_size = metadata.len();
    let range = resolve_range(&headers, file_size)?;

    file.seek(SeekFrom::Start(range.start)).await?;
    let stream = ReaderStream::new(file.take(range.length));
    let body = Body::from_stream(stream);

    let mut builder = Response::builder()
        .status(range.status())
        .header(ACCEPT_RANGES, "bytes")
        .header(CONTENT_LENGTH, range.length.to_string())
        .header(CONTENT_TYPE, content_type(&path))
        .header(ETAG, etag(&path, file_size, metadata.modified()?));

    if range.partial {
        builder = builder.header(
            CONTENT_RANGE,
            format!("bytes {}-{}/{}", range.start, range.end, file_size),
        );
    }

    builder.body(body).map_err(StreamError::Response)
}

#[derive(Debug, Clone)]
struct SourceFileRow {
    media_item_id: Id,
    path: PathBuf,
}

#[derive(Debug, Clone, Copy)]
struct ResolvedRange {
    start: u64,
    end: u64,
    length: u64,
    partial: bool,
}

impl ResolvedRange {
    fn full(file_size: u64) -> Self {
        Self {
            start: 0,
            end: file_size.saturating_sub(1),
            length: file_size,
            partial: false,
        }
    }

    fn partial(start: u64, end: u64) -> Self {
        Self {
            start,
            end,
            length: end - start + 1,
            partial: true,
        }
    }

    const fn status(self) -> StatusCode {
        if self.partial {
            StatusCode::PARTIAL_CONTENT
        } else {
            StatusCode::OK
        }
    }
}

pub(crate) type StreamResult<T> = std::result::Result<T, StreamError>;

#[derive(Debug, thiserror::Error)]
pub(crate) enum StreamError {
    #[error("source file not found")]
    NotFound,

    #[error("range not satisfiable for source file of {file_size} bytes: {reason}")]
    RangeNotSatisfiable { file_size: u64, reason: String },

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),

    #[error(transparent)]
    Session(#[from] session_service::Error),

    #[error("response build failed: {0}")]
    Response(#[from] axum::http::Error),
}

#[derive(Serialize, utoipa::ToSchema)]
pub(crate) struct StreamErrorResponse {
    error: String,
}

impl IntoResponse for StreamError {
    fn into_response(self) -> Response {
        let status = match &self {
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::RangeNotSatisfiable { .. } => StatusCode::RANGE_NOT_SATISFIABLE,
            Self::Io(_) | Self::Sqlx(_) | Self::Session(_) | Self::Response(_) => {
                tracing::error!(error = %self, "stream api failed");
                StatusCode::INTERNAL_SERVER_ERROR
            }
        };

        let mut response = (
            status,
            Json(StreamErrorResponse {
                error: self.to_string(),
            }),
        )
            .into_response();

        response
            .headers_mut()
            .insert(ACCEPT_RANGES, HeaderValue::from_static("bytes"));
        if let Self::RangeNotSatisfiable { file_size, .. } = self {
            let content_range = format!("bytes */{file_size}");
            match HeaderValue::from_str(&content_range) {
                Ok(value) => {
                    response.headers_mut().insert(CONTENT_RANGE, value);
                }
                Err(error) => {
                    tracing::error!(
                        error = %error,
                        content_range,
                        "failed to build range rejection header"
                    );
                }
            }
        }

        response
    }
}

async fn lookup_source_file(db: &Db, id: Id) -> StreamResult<SourceFileRow> {
    let Some(row) = sqlx::query(
        r#"
        SELECT media_item_id, path
        FROM source_files
        WHERE id = ?1
        "#,
    )
    .bind(id)
    .fetch_optional(db.read_pool())
    .await?
    else {
        return Err(StreamError::NotFound);
    };

    let path: String = row.try_get("path")?;
    Ok(SourceFileRow {
        media_item_id: row.try_get("media_item_id")?,
        path: PathBuf::from(path),
    })
}

async fn lookup_transcode_output(db: &Db, id: Id) -> StreamResult<SourceFileRow> {
    let Some(row) = sqlx::query(
        r#"
        SELECT source_files.media_item_id, transcode_outputs.path
        FROM transcode_outputs
        JOIN source_files ON source_files.id = transcode_outputs.source_file_id
        WHERE transcode_outputs.id = ?1
        "#,
    )
    .bind(id)
    .fetch_optional(db.read_pool())
    .await?
    else {
        return Err(StreamError::NotFound);
    };

    let path: String = row.try_get("path")?;
    Ok(SourceFileRow {
        media_item_id: row.try_get("media_item_id")?,
        path: PathBuf::from(path),
    })
}

fn resolve_range(headers: &HeaderMap, file_size: u64) -> StreamResult<ResolvedRange> {
    let Some(range) = headers.get(RANGE) else {
        return Ok(ResolvedRange::full(file_size));
    };

    let range = range
        .to_str()
        .map_err(|_| unsatisfiable(file_size, "range header is not valid UTF-8"))?
        .trim();
    let range = range
        .strip_prefix("bytes=")
        .ok_or_else(|| unsatisfiable(file_size, "range unit is not bytes"))?;

    if range.contains(',') {
        return Err(unsatisfiable(
            file_size,
            "multiple ranges are not supported",
        ));
    }
    if file_size == 0 {
        return Err(unsatisfiable(
            file_size,
            "empty files have no satisfiable ranges",
        ));
    }

    let (start, end) = if let Some(suffix) = range.strip_prefix('-') {
        let suffix_length = parse_range_bound(suffix.trim(), file_size)?;
        if suffix_length == 0 {
            return Err(unsatisfiable(
                file_size,
                "suffix range length must be positive",
            ));
        }
        (
            file_size.saturating_sub(suffix_length),
            file_size.saturating_sub(1),
        )
    } else {
        let (start, end) = range
            .split_once('-')
            .ok_or_else(|| unsatisfiable(file_size, "range is missing '-' separator"))?;
        let start = parse_range_bound(start.trim(), file_size)?;
        let end = match end.trim() {
            "" => file_size.saturating_sub(1),
            value => parse_range_bound(value, file_size)?.min(file_size.saturating_sub(1)),
        };
        (start, end)
    };

    if start >= file_size {
        return Err(unsatisfiable(file_size, "range starts beyond end of file"));
    }
    if start > end {
        return Err(unsatisfiable(file_size, "range start is after range end"));
    }

    Ok(ResolvedRange::partial(start, end))
}

fn parse_range_bound(value: &str, file_size: u64) -> StreamResult<u64> {
    if value.is_empty() {
        return Err(unsatisfiable(file_size, "range bound is empty"));
    }

    value
        .parse()
        .map_err(|_| unsatisfiable(file_size, "range bound is not an unsigned integer"))
}

fn unsatisfiable(file_size: u64, reason: impl Into<String>) -> StreamError {
    StreamError::RangeNotSatisfiable {
        file_size,
        reason: reason.into(),
    }
}

fn content_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("mkv") | Some("mk3d") | Some("mka") | Some("mks") => "video/x-matroska",
        Some("mp4") => "video/mp4",
        Some("m4v") => "video/x-m4v",
        Some("mov") => "video/quicktime",
        Some("webm") => "video/webm",
        Some("avi") => "video/x-msvideo",
        Some("ts") | Some("m2ts") | Some("mts") => "video/mp2t",
        Some("mpeg") | Some("mpg") => "video/mpeg",
        _ => "application/octet-stream",
    }
}

fn etag(path: &Path, file_size: u64, modified: SystemTime) -> String {
    let mut hasher = Sha256::new();
    hasher.update(path.to_string_lossy().as_bytes());
    hasher.update(file_size.to_be_bytes());

    match modified.duration_since(UNIX_EPOCH) {
        Ok(duration) => {
            hasher.update(b"+");
            hasher.update(duration.as_secs().to_be_bytes());
            hasher.update(duration.subsec_nanos().to_be_bytes());
        }
        Err(error) => {
            let duration = error.duration();
            hasher.update(b"-");
            hasher.update(duration.as_secs().to_be_bytes());
            hasher.update(duration.subsec_nanos().to_be_bytes());
        }
    }

    format!("\"{:x}\"", hasher.finalize())
}
