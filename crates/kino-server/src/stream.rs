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
use kino_fulfillment::{FfprobeFileProbe, ProbeAudioStream, ProbeResult, ProbeVideoStream};
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::Row;
use tokio::io::{AsyncReadExt as _, AsyncSeekExt as _, SeekFrom};
use tokio_util::io::ReaderStream;

use crate::{auth::AuthenticatedUser, session_service};

const HLS_CONTENT_TYPE: &str = "application/vnd.apple.mpegurl";
const HLS_SEGMENT_TARGET_SECONDS: u64 = 6;
const DEFAULT_HLS_BANDWIDTH: u64 = 2_000_000;

#[derive(Clone)]
pub(crate) struct StreamState {
    db: Db,
}

pub(crate) fn router(db: Db) -> Router {
    Router::new()
        .route(
            "/api/v1/stream/items/{id}/{variant_id}/master.m3u8",
            get(master_playlist),
        )
        .route(
            "/api/v1/stream/items/{id}/{variant_id}/media.m3u8",
            get(media_playlist),
        )
        .route("/api/v1/stream/sourcefile/{id}", get(source_file))
        .route("/api/v1/stream/transcode/{id}", get(transcode_output))
        .with_state(StreamState { db })
}

/// Serve an HLS master playlist for a media item stream variant.
#[utoipa::path(
    get,
    path = "/api/v1/stream/items/{id}/{variant_id}/master.m3u8",
    tag = "stream",
    params(
        ("id" = Id, Path, description = "Media item id"),
        ("variant_id" = String, Path, description = "Catalog stream variant id")
    ),
    responses(
        (status = 200, description = "HLS master playlist", content_type = "application/vnd.apple.mpegurl"),
        (status = 404, description = "Stream variant not found", body = StreamErrorResponse),
        (status = 500, description = "Master playlist generation failed", body = StreamErrorResponse)
    )
)]
pub(crate) async fn master_playlist(
    State(state): State<StreamState>,
    AxumPath((media_item_id, variant_id)): AxumPath<(Id, Id)>,
) -> StreamResult<Response> {
    let source_file = lookup_source_variant(&state.db, media_item_id, variant_id).await?;
    let probe = FfprobeFileProbe::new().probe(&source_file.path).await?;
    let metadata = tokio::fs::metadata(&source_file.path).await?;
    let subtitles = lookup_subtitle_tracks(&state.db, media_item_id).await?;
    let playlist = build_master_playlist(
        media_item_id,
        variant_id,
        metadata.len(),
        source_file.probe_duration_seconds,
        &probe,
        &subtitles,
    );

    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, HLS_CONTENT_TYPE)
        .body(Body::from(playlist))
        .map_err(StreamError::Response)
}

/// Serve a VOD HLS media playlist backed by source-file byte ranges.
#[utoipa::path(
    get,
    path = "/api/v1/stream/items/{id}/{variant_id}/media.m3u8",
    tag = "stream",
    params(
        ("id" = Id, Path, description = "Media item id"),
        ("variant_id" = Id, Path, description = "Source-file variant id")
    ),
    responses(
        (status = 200, description = "HLS media playlist", body = String, content_type = "application/vnd.apple.mpegurl"),
        (status = 404, description = "Media item variant not found", body = StreamErrorResponse),
        (status = 422, description = "Media item variant is not playlist-ready", body = StreamErrorResponse),
        (status = 500, description = "Media playlist generation failed", body = StreamErrorResponse)
    )
)]
pub(crate) async fn media_playlist(
    State(state): State<StreamState>,
    AuthenticatedUser { user, token_id }: AuthenticatedUser,
    AxumPath((id, variant_id)): AxumPath<(Id, Id)>,
) -> StreamResult<Response> {
    let source_file = lookup_source_variant(&state.db, id, variant_id).await?;
    session_service::heartbeat_or_open_session(
        &state.db,
        user.id,
        token_id,
        source_file.media_item_id,
        variant_id.to_string(),
    )
    .await?;

    let metadata = tokio::fs::metadata(&source_file.path).await?;
    let playlist = build_media_playlist(
        variant_id,
        source_file
            .probe_duration_seconds
            .ok_or_else(|| playlist_unavailable("source file has no probed duration"))?,
        metadata.len(),
    )?;

    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, HLS_CONTENT_TYPE)
        .body(Body::from(playlist))
        .map_err(StreamError::Response)
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
    probe_duration_seconds: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SubtitleTrackRow {
    language: String,
    track_index: u32,
    forced: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ByteRangeSegment {
    offset: u64,
    length: u64,
    duration_seconds: u64,
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

    #[error("media playlist unavailable: {reason}")]
    PlaylistUnavailable { reason: String },

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Probe(#[from] kino_fulfillment::ProbeError),

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
            Self::PlaylistUnavailable { .. } => StatusCode::UNPROCESSABLE_ENTITY,
            Self::Io(_) | Self::Probe(_) | Self::Sqlx(_) | Self::Session(_) | Self::Response(_) => {
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
        SELECT media_item_id, path, probe_duration_seconds
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

    source_file_row(&row)
}

async fn lookup_transcode_output(db: &Db, id: Id) -> StreamResult<SourceFileRow> {
    let Some(row) = sqlx::query(
        r#"
        SELECT source_files.media_item_id, transcode_outputs.path, NULL AS probe_duration_seconds
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

    source_file_row(&row)
}

async fn lookup_source_variant(
    db: &Db,
    media_item_id: Id,
    variant_id: Id,
) -> StreamResult<SourceFileRow> {
    let Some(row) = sqlx::query(
        r#"
        SELECT media_item_id, path, probe_duration_seconds
        FROM source_files
        WHERE media_item_id = ?1 AND id = ?2
        "#,
    )
    .bind(media_item_id)
    .bind(variant_id)
    .fetch_optional(db.read_pool())
    .await?
    else {
        return Err(StreamError::NotFound);
    };

    source_file_row(&row)
}

fn source_file_row(row: &sqlx::sqlite::SqliteRow) -> StreamResult<SourceFileRow> {
    let path: String = row.try_get("path")?;
    let probe_duration_seconds = row
        .try_get::<Option<i64>, _>("probe_duration_seconds")?
        .and_then(|value| u64::try_from(value).ok());
    Ok(SourceFileRow {
        media_item_id: row.try_get("media_item_id")?,
        path: PathBuf::from(path),
        probe_duration_seconds,
    })
}

async fn lookup_subtitle_tracks(db: &Db, media_item_id: Id) -> StreamResult<Vec<SubtitleTrackRow>> {
    let rows = sqlx::query(
        r#"
        SELECT language, track_index, forced
        FROM subtitle_sidecars
        WHERE media_item_id = ?1 AND archived_at IS NULL
        ORDER BY language, track_index, provenance, id
        "#,
    )
    .bind(media_item_id)
    .fetch_all(db.read_pool())
    .await?;

    rows.iter()
        .map(|row| {
            let track_index: i64 = row.try_get("track_index")?;
            Ok(SubtitleTrackRow {
                language: row.try_get("language")?,
                track_index: u32::try_from(track_index).map_err(|_| sqlx::Error::ColumnDecode {
                    index: "track_index".to_owned(),
                    source: Box::new(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "subtitle track index is outside u32 range",
                    )),
                })?,
                forced: row.try_get::<i64, _>("forced")? != 0,
            })
        })
        .collect::<std::result::Result<_, sqlx::Error>>()
        .map_err(StreamError::Sqlx)
}

fn build_master_playlist(
    media_item_id: Id,
    variant_id: Id,
    file_size_bytes: u64,
    source_duration_seconds: Option<u64>,
    probe: &ProbeResult,
    subtitles: &[SubtitleTrackRow],
) -> String {
    let mut playlist = String::from("#EXTM3U\n#EXT-X-VERSION:7\n");
    let media_playlist_uri =
        format!("/api/v1/stream/items/{media_item_id}/{variant_id}/media.m3u8");

    for (position, audio) in probe.audio_streams.iter().enumerate() {
        let language = audio.language.as_deref();
        let name = language
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| format!("Audio {}", position + 1));
        playlist.push_str("#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"audio\"");
        push_quoted_attribute(&mut playlist, "NAME", &name);
        if let Some(language) = language.filter(|value| !value.is_empty()) {
            push_quoted_attribute(&mut playlist, "LANGUAGE", language);
        }
        playlist.push_str(",DEFAULT=");
        playlist.push_str(yes_no(position == 0));
        playlist.push_str(",AUTOSELECT=YES");
        push_quoted_attribute(
            &mut playlist,
            "URI",
            &format!("{media_playlist_uri}?audio={}", audio.index),
        );
        playlist.push('\n');
    }

    for subtitle in subtitles {
        playlist.push_str("#EXT-X-MEDIA:TYPE=SUBTITLES,GROUP-ID=\"subs\"");
        push_quoted_attribute(&mut playlist, "NAME", &subtitle.language);
        push_quoted_attribute(&mut playlist, "LANGUAGE", &subtitle.language);
        playlist.push_str(",FORCED=");
        playlist.push_str(yes_no(subtitle.forced));
        push_quoted_attribute(
            &mut playlist,
            "URI",
            &format!(
                "/api/v1/stream/items/{media_item_id}/subtitles/{}.m3u8",
                subtitle.track_index
            ),
        );
        playlist.push('\n');
    }

    playlist.push_str("#EXT-X-STREAM-INF:");
    playlist.push_str(&format!(
        "BANDWIDTH={},CODECS=\"{}\"",
        estimate_bandwidth(
            file_size_bytes,
            probe.duration_seconds().or(source_duration_seconds)
        ),
        codec_string(probe)
    ));
    if let Some((width, height)) = primary_resolution(&probe.video_streams) {
        playlist.push_str(&format!(",RESOLUTION={width}x{height}"));
    }
    if !probe.audio_streams.is_empty() {
        playlist.push_str(",AUDIO=\"audio\"");
    }
    if !subtitles.is_empty() {
        playlist.push_str(",SUBTITLES=\"subs\"");
    }
    playlist.push('\n');
    playlist.push_str(&media_playlist_uri);
    playlist.push('\n');

    playlist
}

fn build_media_playlist(
    source_file_id: Id,
    duration_seconds: u64,
    file_size: u64,
) -> StreamResult<String> {
    if duration_seconds == 0 {
        return Err(playlist_unavailable("source file duration is zero"));
    }
    if file_size == 0 {
        return Err(playlist_unavailable("source file is empty"));
    }

    let segments = byte_range_segments(duration_seconds, file_size);
    let target_duration = segments
        .iter()
        .map(|segment| segment.duration_seconds)
        .max()
        .unwrap_or(HLS_SEGMENT_TARGET_SECONDS);
    let source_uri = format!("/api/v1/stream/sourcefile/{source_file_id}");
    let mut playlist = String::new();
    playlist.push_str("#EXTM3U\n");
    playlist.push_str("#EXT-X-VERSION:4\n");
    playlist.push_str(&format!("#EXT-X-TARGETDURATION:{target_duration}\n"));
    playlist.push_str("#EXT-X-PLAYLIST-TYPE:VOD\n");

    for segment in segments {
        playlist.push_str(&format!(
            "#EXTINF:{:.3},\n",
            segment.duration_seconds as f64
        ));
        playlist.push_str(&format!(
            "#EXT-X-BYTERANGE:{}@{}\n",
            segment.length, segment.offset
        ));
        playlist.push_str(&source_uri);
        playlist.push('\n');
    }

    playlist.push_str("#EXT-X-ENDLIST\n");
    Ok(playlist)
}

fn byte_range_segments(duration_seconds: u64, file_size: u64) -> Vec<ByteRangeSegment> {
    let duration_segment_count = duration_seconds.div_ceil(HLS_SEGMENT_TARGET_SECONDS);
    let segment_count = duration_segment_count.min(file_size);
    let base_length = file_size / segment_count;
    let remainder = file_size % segment_count;
    let mut segments = Vec::with_capacity(segment_count as usize);
    let mut offset = 0;

    for index in 0..segment_count {
        let length = base_length + if index < remainder { 1 } else { 0 };
        let duration_seconds = if index + 1 == segment_count {
            duration_seconds - (HLS_SEGMENT_TARGET_SECONDS * index)
        } else {
            HLS_SEGMENT_TARGET_SECONDS
        };
        segments.push(ByteRangeSegment {
            offset,
            length,
            duration_seconds,
        });
        offset += length;
    }

    segments
}

trait ProbeResultExt {
    fn duration_seconds(&self) -> Option<u64>;
}

impl ProbeResultExt for ProbeResult {
    fn duration_seconds(&self) -> Option<u64> {
        self.duration
            .and_then(|duration| (duration.as_secs() > 0).then_some(duration.as_secs()))
    }
}

fn estimate_bandwidth(file_size_bytes: u64, duration_seconds: Option<u64>) -> u64 {
    let Some(duration_seconds) = duration_seconds.filter(|duration| *duration > 0) else {
        return DEFAULT_HLS_BANDWIDTH;
    };

    file_size_bytes
        .saturating_mul(8)
        .checked_div(duration_seconds)
        .filter(|bandwidth| *bandwidth > 0)
        .unwrap_or(DEFAULT_HLS_BANDWIDTH)
}

fn codec_string(probe: &ProbeResult) -> String {
    let video_codec = probe
        .video_streams
        .iter()
        .find_map(|stream| video_codec_string(stream));
    let audio_codec = probe
        .audio_streams
        .iter()
        .find_map(|stream| audio_codec_string(stream));

    match (video_codec, audio_codec) {
        (Some(video), Some(audio)) => format!("{video},{audio}"),
        (Some(video), None) => format!("{video},mp4a"),
        (None, Some(audio)) => format!("avc1,{audio}"),
        (None, None) => "avc1,mp4a".to_owned(),
    }
}

fn video_codec_string(stream: &ProbeVideoStream) -> Option<&'static str> {
    match stream.codec_name.as_deref() {
        Some("h264") => Some("avc1.64001f"),
        Some("hevc" | "h265") => Some("hvc1.1.6.L93.B0"),
        Some("av1") => Some("av01.0.08M.08"),
        Some("vp9") => Some("vp09.00.10.08"),
        _ => None,
    }
}

fn audio_codec_string(stream: &ProbeAudioStream) -> Option<&'static str> {
    match stream.codec_name.as_deref() {
        Some("aac") => Some("mp4a.40.2"),
        Some("mp3") => Some("mp4a.40.34"),
        Some("ac3") => Some("ac-3"),
        Some("eac3") => Some("ec-3"),
        Some("alac") => Some("alac"),
        Some("flac") => Some("fLaC"),
        Some("opus") => Some("opus"),
        _ => None,
    }
}

fn primary_resolution(video_streams: &[ProbeVideoStream]) -> Option<(u32, u32)> {
    video_streams
        .iter()
        .find_map(|stream| stream.width.zip(stream.height))
        .filter(|(width, height)| *width > 0 && *height > 0)
}

fn push_quoted_attribute(playlist: &mut String, name: &str, value: &str) {
    playlist.push(',');
    playlist.push_str(name);
    playlist.push_str("=\"");
    for character in value.chars() {
        if matches!(character, '"' | '\\') {
            playlist.push('\\');
        }
        playlist.push(character);
    }
    playlist.push('"');
}

const fn yes_no(value: bool) -> &'static str {
    if value { "YES" } else { "NO" }
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

fn playlist_unavailable(reason: impl Into<String>) -> StreamError {
    StreamError::PlaylistUnavailable {
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
