use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
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
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use kino_core::{FfprobeFileProbe, Id, ProbeAudioStream, ProbeResult, ProbeVideoStream};
use kino_db::Db;
use kino_library::{SubtitleFormat, SubtitleProvenance, SubtitleService, SubtitleSidecar};
use kino_transcode::{
    ActiveEncodeRequest, ActiveEncodes, AudioPolicy, ColorOutput, EphemeralStore,
    FfmpegEncodeCommand, HlsOutputSpec, InputSpec, PipelineRunner, Preset, TranscodeProfile,
    VideoFilter, VideoOutputSpec,
    plan::{AudioPolicyKind, ColorTarget},
};
use serde::{Deserialize, Serialize};
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
    subtitles: SubtitleService,
    live: Option<LiveStreamState>,
}

#[derive(Clone)]
pub(crate) struct LiveStreamState {
    store: EphemeralStore,
    active: ActiveEncodes,
    enabled: bool,
    cache_root: PathBuf,
}

pub(crate) fn router_with_live(db: Db, live: Option<LiveStreamState>) -> Router {
    let subtitles = SubtitleService::new(db.clone());
    Router::new()
        .route(
            "/api/v1/stream/items/{id}/master.m3u8",
            get(master_playlist),
        )
        .route(
            "/api/v1/stream/items/{id}/{variant_id}/media.m3u8",
            get(media_playlist),
        )
        .route(
            "/api/v1/stream/sourcefile/{id}/file.{ext}",
            get(source_file),
        )
        .route("/api/v1/stream/transcode/{id}", get(transcode_output))
        .route(
            "/api/v1/stream/transcodes/{id}/media.m3u8",
            get(transcode_media_playlist),
        )
        .route(
            "/api/v1/stream/transcodes/{id}/init.mp4",
            get(transcode_init_segment),
        )
        .route(
            "/api/v1/stream/transcodes/{id}/{segment}",
            get(transcode_media_segment),
        )
        .route(
            "/api/v1/stream/live/{source_file_id}/{profile}/media.m3u8",
            get(live_media_playlist),
        )
        .route(
            "/api/v1/stream/live/{source_file_id}/{profile}/init.mp4",
            get(live_init_segment),
        )
        .route(
            "/api/v1/stream/live/{source_file_id}/{profile}/{segment}",
            get(live_media_segment),
        )
        .route(
            "/api/v1/stream/items/{id}/subtitles/{track_vtt}",
            get(subtitle_track),
        )
        .with_state(StreamState {
            db,
            subtitles,
            live,
        })
}

impl LiveStreamState {
    pub(crate) fn new(db: Db, config: &kino_core::EphemeralConfig) -> Self {
        let store = EphemeralStore::new(db);
        let active = ActiveEncodes::new(store.clone(), Arc::new(PipelineRunner::new()));
        Self {
            store,
            active,
            enabled: config.enabled,
            cache_root: config.cache_root.clone(),
        }
    }
}

/// Serve an HLS master playlist for a media item.
#[utoipa::path(
    get,
    path = "/api/v1/stream/items/{id}/master.m3u8",
    tag = "stream",
    params(
        ("id" = Id, Path, description = "Media item id")
    ),
    responses(
        (status = 200, description = "HLS master playlist", body = String, content_type = "application/vnd.apple.mpegurl"),
        (status = 404, description = "Media item source file not found", body = StreamErrorResponse),
        (status = 422, description = "Packaged transcode output is not playlist-ready", body = StreamErrorResponse),
        (status = 500, description = "Master playlist generation failed", body = StreamErrorResponse)
    )
)]
pub(crate) async fn master_playlist(
    State(state): State<StreamState>,
    AxumPath(media_item_id): AxumPath<Id>,
) -> StreamResult<Response> {
    let source_file = lookup_primary_source_file(&state.db, media_item_id).await?;
    let probe = FfprobeFileProbe::new().probe(&source_file.path).await?;
    let subtitles = lookup_subtitle_tracks(&state.db, media_item_id).await?;
    let transcode_outputs = lookup_packaged_transcode_outputs(&state.db, media_item_id).await?;
    let playlist = if transcode_outputs.is_empty() {
        let metadata = tokio::fs::metadata(&source_file.path).await?;
        build_master_playlist(
            media_item_id,
            source_media_playlist_uri(media_item_id, source_file.id),
            vec![StreamVariant::source_fallback(
                source_media_playlist_uri(media_item_id, source_file.id),
                metadata.len(),
                source_file.probe_duration_seconds,
                &probe,
            )],
            &probe,
            &subtitles,
        )
    } else {
        let variants = packaged_stream_variants(&transcode_outputs).await?;
        build_master_playlist(
            media_item_id,
            transcode_media_playlist_uri(transcode_outputs[0].id),
            variants,
            &probe,
            &subtitles,
        )
    };

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
        (status = 503, description = "Media item variant is missing persisted probe data", body = StreamErrorResponse),
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
        &source_file.path,
        source_file.probe_duration_seconds.ok_or_else(|| {
            probe_data_missing(source_file.id, "source file has no probed duration")
        })?,
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
    path = "/api/v1/stream/sourcefile/{id}/file.{ext}",
    tag = "stream",
    params(
        ("id" = Id, Path, description = "Source-file id"),
        ("ext" = String, Path, description = "Source-file extension"),
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
    AxumPath((id, ext)): AxumPath<(Id, String)>,
    headers: HeaderMap,
) -> StreamResult<Response> {
    let source_file = lookup_source_file(&state.db, id).await?;
    validate_source_file_extension(&source_file.path, &ext)?;
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

/// Serve a packaged transcode output HLS media playlist.
#[utoipa::path(
    get,
    path = "/api/v1/stream/transcodes/{transcode_output_id}/media.m3u8",
    tag = "stream",
    params(
        ("transcode_output_id" = Id, Path, description = "Packaged transcode-output id")
    ),
    responses(
        (status = 200, description = "Packaged HLS media playlist", body = String, content_type = "application/vnd.apple.mpegurl"),
        (status = 401, description = "Missing or invalid bearer token"),
        (status = 404, description = "Transcode output not found", body = StreamErrorResponse),
        (status = 422, description = "Transcode output is not playlist-ready", body = StreamErrorResponse),
        (status = 500, description = "Transcode media playlist failed", body = StreamErrorResponse)
    )
)]
pub(crate) async fn transcode_media_playlist(
    State(state): State<StreamState>,
    AuthenticatedUser { user, token_id }: AuthenticatedUser,
    AxumPath(id): AxumPath<Id>,
) -> StreamResult<Response> {
    let transcode_output = lookup_packaged_transcode_output(&state.db, id).await?;
    session_service::heartbeat_or_open_session(
        &state.db,
        user.id,
        token_id,
        transcode_output.media_item_id,
        id.to_string(),
    )
    .await?;

    let playlist_path = transcode_output.playlist_path()?;
    let playlist = tokio::fs::read_to_string(&playlist_path).await?;
    let playlist = rewrite_transcode_playlist_uris(id, &playlist)?;

    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, HLS_CONTENT_TYPE)
        .body(Body::from(playlist))
        .map_err(StreamError::Response)
}

/// Serve a packaged transcode output init segment with single-range support.
#[utoipa::path(
    get,
    path = "/api/v1/stream/transcodes/{transcode_output_id}/init.mp4",
    tag = "stream",
    params(
        ("transcode_output_id" = Id, Path, description = "Packaged transcode-output id"),
        ("Range" = Option<String>, Header, description = "Single HTTP byte range")
    ),
    responses(
        (status = 200, description = "Full init segment", content_type = "video/mp4"),
        (status = 206, description = "Requested init segment byte range", content_type = "video/mp4"),
        (status = 401, description = "Missing or invalid bearer token"),
        (status = 404, description = "Transcode output not found", body = StreamErrorResponse),
        (status = 416, description = "Requested range is not satisfiable", body = StreamErrorResponse),
        (status = 422, description = "Transcode output is missing init segment metadata", body = StreamErrorResponse),
        (status = 500, description = "Init segment stream failed", body = StreamErrorResponse)
    )
)]
pub(crate) async fn transcode_init_segment(
    State(state): State<StreamState>,
    AuthenticatedUser { user, token_id }: AuthenticatedUser,
    AxumPath(id): AxumPath<Id>,
    headers: HeaderMap,
) -> StreamResult<Response> {
    let transcode_output = lookup_packaged_transcode_output(&state.db, id).await?;
    session_service::heartbeat_or_open_session(
        &state.db,
        user.id,
        token_id,
        transcode_output.media_item_id,
        id.to_string(),
    )
    .await?;

    stream_file(transcode_output.init_path()?, headers).await
}

/// Serve a packaged transcode output media segment with single-range support.
#[utoipa::path(
    get,
    path = "/api/v1/stream/transcodes/{transcode_output_id}/seg-{segment}.m4s",
    tag = "stream",
    params(
        ("transcode_output_id" = Id, Path, description = "Packaged transcode-output id"),
        ("segment" = String, Path, description = "Five-digit segment number"),
        ("Range" = Option<String>, Header, description = "Single HTTP byte range")
    ),
    responses(
        (status = 200, description = "Full media segment"),
        (status = 206, description = "Requested media segment byte range"),
        (status = 401, description = "Missing or invalid bearer token"),
        (status = 404, description = "Transcode output or segment not found", body = StreamErrorResponse),
        (status = 416, description = "Requested range is not satisfiable", body = StreamErrorResponse),
        (status = 500, description = "Media segment stream failed", body = StreamErrorResponse)
    )
)]
pub(crate) async fn transcode_media_segment(
    State(state): State<StreamState>,
    AuthenticatedUser { user, token_id }: AuthenticatedUser,
    AxumPath((id, segment)): AxumPath<(Id, String)>,
    headers: HeaderMap,
) -> StreamResult<Response> {
    let segment_filename = route_segment_filename(&segment)?;
    let transcode_output = lookup_packaged_transcode_output(&state.db, id).await?;
    session_service::heartbeat_or_open_session(
        &state.db,
        user.id,
        token_id,
        transcode_output.media_item_id,
        id.to_string(),
    )
    .await?;

    stream_file(
        transcode_output.directory_path.join(segment_filename),
        headers,
    )
    .await
}

/// Serve a live transcode HLS media playlist.
#[utoipa::path(
    get,
    path = "/api/v1/stream/live/{source_file_id}/{profile}/media.m3u8",
    tag = "stream",
    params(
        ("source_file_id" = Id, Path, description = "Source-file id"),
        ("profile" = String, Path, description = "Base64url canonical TranscodeProfile JSON")
    ),
    responses(
        (status = 200, description = "Live HLS media playlist", body = String, content_type = "application/vnd.apple.mpegurl"),
        (status = 401, description = "Missing or invalid bearer token"),
        (status = 404, description = "Live profile or source file not found", body = StreamErrorResponse),
        (status = 500, description = "Live media playlist failed", body = StreamErrorResponse)
    )
)]
pub(crate) async fn live_media_playlist(
    State(state): State<StreamState>,
    AuthenticatedUser { user, token_id }: AuthenticatedUser,
    AxumPath((source_file_id, encoded_profile)): AxumPath<(Id, String)>,
) -> StreamResult<Response> {
    let live = state.live.clone().ok_or(StreamError::NotFound)?;
    let request = live_request(&state.db, &live, source_file_id, &encoded_profile).await?;
    session_service::heartbeat_or_open_session(
        &state.db,
        user.id,
        token_id,
        request.media_item_id,
        request.profile_hash_hex.clone(),
    )
    .await?;

    let output = resolve_live_output(&live, request, Some(0)).await?;
    let playlist = tokio::fs::read_to_string(output.directory_path.join("media.m3u8")).await?;
    let playlist = rewrite_live_playlist_uris(source_file_id, &encoded_profile, &playlist)?;

    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, HLS_CONTENT_TYPE)
        .body(Body::from(playlist))
        .map_err(StreamError::Response)
}

/// Serve a live transcode init segment.
#[utoipa::path(
    get,
    path = "/api/v1/stream/live/{source_file_id}/{profile}/init.mp4",
    tag = "stream",
    params(
        ("source_file_id" = Id, Path, description = "Source-file id"),
        ("profile" = String, Path, description = "Base64url canonical TranscodeProfile JSON"),
        ("Range" = Option<String>, Header, description = "Single HTTP byte range")
    ),
    responses(
        (status = 200, description = "Full live init segment", content_type = "video/mp4"),
        (status = 206, description = "Requested init segment byte range", content_type = "video/mp4"),
        (status = 401, description = "Missing or invalid bearer token"),
        (status = 404, description = "Live profile or source file not found", body = StreamErrorResponse),
        (status = 416, description = "Requested range is not satisfiable", body = StreamErrorResponse),
        (status = 500, description = "Live init segment failed", body = StreamErrorResponse)
    )
)]
pub(crate) async fn live_init_segment(
    State(state): State<StreamState>,
    AuthenticatedUser { user, token_id }: AuthenticatedUser,
    AxumPath((source_file_id, encoded_profile)): AxumPath<(Id, String)>,
    headers: HeaderMap,
) -> StreamResult<Response> {
    let live = state.live.clone().ok_or(StreamError::NotFound)?;
    let request = live_request(&state.db, &live, source_file_id, &encoded_profile).await?;
    session_service::heartbeat_or_open_session(
        &state.db,
        user.id,
        token_id,
        request.media_item_id,
        request.profile_hash_hex.clone(),
    )
    .await?;

    let output = resolve_live_output(&live, request, Some(0)).await?;
    stream_file(output.directory_path.join("init.mp4"), headers).await
}

/// Serve a live transcode media segment.
#[utoipa::path(
    get,
    path = "/api/v1/stream/live/{source_file_id}/{profile}/seg-{segment}.m4s",
    tag = "stream",
    params(
        ("source_file_id" = Id, Path, description = "Source-file id"),
        ("profile" = String, Path, description = "Base64url canonical TranscodeProfile JSON"),
        ("segment" = String, Path, description = "Five-digit segment number"),
        ("Range" = Option<String>, Header, description = "Single HTTP byte range")
    ),
    responses(
        (status = 200, description = "Full live media segment"),
        (status = 206, description = "Requested media segment byte range"),
        (status = 401, description = "Missing or invalid bearer token"),
        (status = 404, description = "Live profile, source file, or segment not found", body = StreamErrorResponse),
        (status = 416, description = "Requested range is not satisfiable", body = StreamErrorResponse),
        (status = 500, description = "Live media segment failed", body = StreamErrorResponse)
    )
)]
pub(crate) async fn live_media_segment(
    State(state): State<StreamState>,
    AuthenticatedUser { user, token_id }: AuthenticatedUser,
    AxumPath((source_file_id, encoded_profile, segment)): AxumPath<(Id, String, String)>,
    headers: HeaderMap,
) -> StreamResult<Response> {
    let segment_filename = route_segment_filename(&segment)?;
    let segment_number = parse_route_segment_number(&segment_filename)?;
    let live = state.live.clone().ok_or(StreamError::NotFound)?;
    let request = live_request(&state.db, &live, source_file_id, &encoded_profile).await?;
    session_service::heartbeat_or_open_session(
        &state.db,
        user.id,
        token_id,
        request.media_item_id,
        request.profile_hash_hex.clone(),
    )
    .await?;

    let output = resolve_live_output(&live, request, Some(segment_number)).await?;
    stream_file(output.directory_path.join(segment_filename), headers).await
}

/// Serve a subtitle sidecar as a WebVTT rendition.
#[utoipa::path(
    get,
    path = "/api/v1/stream/items/{id}/subtitles/{track}.vtt",
    tag = "stream",
    params(
        ("id" = Id, Path, description = "Media item id"),
        ("track" = Id, Path, description = "Subtitle sidecar id")
    ),
    responses(
        (status = 200, description = "WebVTT subtitle rendition", content_type = "text/vtt"),
        (status = 401, description = "Missing or invalid bearer token"),
        (status = 404, description = "Subtitle sidecar not found", body = StreamErrorResponse),
        (status = 500, description = "Subtitle conversion failed", body = StreamErrorResponse)
    )
)]
pub(crate) async fn subtitle_track(
    State(state): State<StreamState>,
    _auth: AuthenticatedUser,
    AxumPath((media_item_id, track_vtt)): AxumPath<(Id, String)>,
) -> StreamResult<Response> {
    let track_id = parse_subtitle_track_id(&track_vtt)?;
    let Some(sidecar) = state.subtitles.get(track_id).await? else {
        return Err(StreamError::NotFound);
    };
    if sidecar.media_item_id != media_item_id {
        return Err(StreamError::NotFound);
    }

    let text = read_subtitle_text(&sidecar).await?;
    let body = subtitle_to_webvtt(&sidecar, &text)?;

    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, "text/vtt")
        .body(Body::from(body))
        .map_err(StreamError::Response)
}

fn parse_subtitle_track_id(track_vtt: &str) -> StreamResult<Id> {
    let Some(track) = track_vtt.strip_suffix(".vtt") else {
        return Err(StreamError::NotFound);
    };
    track.parse().map_err(|_| StreamError::NotFound)
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
    id: Id,
    media_item_id: Id,
    path: PathBuf,
    probe_duration_seconds: Option<u64>,
}

#[derive(Debug, Clone)]
struct PackagedTranscodeOutputRow {
    id: Id,
    media_item_id: Id,
    directory_path: PathBuf,
    playlist_filename: Option<String>,
    init_filename: Option<String>,
    encode_metadata_json: Option<String>,
}

#[derive(Debug, Clone)]
struct LiveRequest {
    source_file: SourceFileRow,
    profile: TranscodeProfile,
    profile_json: String,
    profile_hash: [u8; 32],
    profile_hash_hex: String,
    media_item_id: Id,
}

#[derive(Debug, Clone)]
struct LiveOutput {
    directory_path: PathBuf,
}

impl PackagedTranscodeOutputRow {
    fn playlist_path(&self) -> StreamResult<PathBuf> {
        Ok(self
            .directory_path
            .join(playlist_filename(self.playlist_filename.as_deref())?))
    }

    fn init_path(&self) -> StreamResult<PathBuf> {
        Ok(self
            .directory_path
            .join(init_filename(self.init_filename.as_deref())?))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SubtitleTrackRow {
    language: String,
    track_index: u32,
    forced: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StreamVariant {
    uri: String,
    bandwidth: u64,
    codecs: String,
    resolution: Option<(u32, u32)>,
    video_range: Option<VideoRange>,
}

impl StreamVariant {
    fn source_fallback(
        uri: String,
        file_size_bytes: u64,
        source_duration_seconds: Option<u64>,
        probe: &ProbeResult,
    ) -> Self {
        Self {
            uri,
            bandwidth: estimate_bandwidth(
                file_size_bytes,
                probe.duration_seconds().or(source_duration_seconds),
            ),
            codecs: codec_string(probe),
            resolution: primary_resolution(&probe.video_streams),
            video_range: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VideoRange {
    Sdr,
    Pq,
    Hlg,
}

impl VideoRange {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "SDR" => Some(Self::Sdr),
            "PQ" => Some(Self::Pq),
            "HLG" => Some(Self::Hlg),
            _ => None,
        }
    }

    const fn as_hls_value(self) -> &'static str {
        match self {
            Self::Sdr => "SDR",
            Self::Pq => "PQ",
            Self::Hlg => "HLG",
        }
    }
}

#[derive(Debug, Deserialize)]
struct EncodeMetadata {
    codecs: String,
    resolution: Option<EncodeMetadataResolution>,
    width: Option<u32>,
    height: Option<u32>,
    video_range: Option<String>,
    duration_seconds: Option<u64>,
    duration_us: Option<u64>,
}

impl EncodeMetadata {
    fn duration_seconds(&self) -> Option<u64> {
        self.duration_seconds.or_else(|| {
            self.duration_us
                .map(|duration_us| duration_us.div_ceil(1_000_000))
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum EncodeMetadataResolution {
    Text(String),
    Pair((u32, u32)),
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct PackagedPlaylistMetrics {
    bytes: u64,
    duration_seconds: Option<u64>,
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

    #[error("probe data missing for source file {source_file_id}: {reason}")]
    ProbeDataMissing { source_file_id: Id, reason: String },

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Probe(#[from] kino_core::ProbeError),

    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),

    #[error(transparent)]
    Library(#[from] kino_library::Error),

    #[error(transparent)]
    Transcode(#[from] kino_transcode::Error),

    #[error(transparent)]
    Session(#[from] session_service::Error),

    #[error("invalid subtitle sidecar {path}: {reason}", path = .path.display())]
    InvalidSubtitle { path: PathBuf, reason: String },

    #[error("response build failed: {0}")]
    Response(#[from] axum::http::Error),
}

#[derive(Serialize, utoipa::ToSchema)]
pub(crate) struct StreamErrorResponse {
    error: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_file_id: Option<Id>,
}

impl IntoResponse for StreamError {
    fn into_response(self) -> Response {
        let status = match &self {
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::RangeNotSatisfiable { .. } => StatusCode::RANGE_NOT_SATISFIABLE,
            Self::PlaylistUnavailable { .. } => StatusCode::UNPROCESSABLE_ENTITY,
            Self::ProbeDataMissing { .. } => StatusCode::SERVICE_UNAVAILABLE,
            Self::Io(_)
            | Self::Probe(_)
            | Self::Sqlx(_)
            | Self::Library(_)
            | Self::Transcode(_)
            | Self::Session(_)
            | Self::InvalidSubtitle { .. }
            | Self::Response(_) => {
                tracing::error!(error = %self, "stream api failed");
                StatusCode::INTERNAL_SERVER_ERROR
            }
        };

        let mut response = (status, Json(stream_error_response(&self))).into_response();

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
        SELECT id, media_item_id, path, probe_duration_seconds
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
        SELECT
            transcode_outputs.id,
            source_files.media_item_id,
            transcode_outputs.path,
            NULL AS probe_duration_seconds
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

async fn lookup_packaged_transcode_output(
    db: &Db,
    id: Id,
) -> StreamResult<PackagedTranscodeOutputRow> {
    let Some(row) = sqlx::query(
        r#"
        SELECT
            transcode_outputs.id,
            source_files.media_item_id,
            transcode_outputs.directory_path,
            transcode_outputs.playlist_filename,
            transcode_outputs.init_filename,
            transcode_outputs.encode_metadata_json
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

    packaged_transcode_output_row(&row)
}

async fn lookup_packaged_transcode_outputs(
    db: &Db,
    media_item_id: Id,
) -> StreamResult<Vec<PackagedTranscodeOutputRow>> {
    let rows = sqlx::query(
        r#"
        SELECT
            transcode_outputs.id,
            source_files.media_item_id,
            transcode_outputs.directory_path,
            transcode_outputs.playlist_filename,
            transcode_outputs.init_filename,
            transcode_outputs.encode_metadata_json
        FROM transcode_outputs
        JOIN source_files ON source_files.id = transcode_outputs.source_file_id
        WHERE source_files.media_item_id = ?1
          AND transcode_outputs.directory_path IS NOT NULL
        ORDER BY source_files.created_at, source_files.id, transcode_outputs.created_at, transcode_outputs.id
        "#,
    )
    .bind(media_item_id)
    .fetch_all(db.read_pool())
    .await?;

    rows.iter()
        .map(packaged_transcode_output_row)
        .collect::<StreamResult<Vec<_>>>()
}

async fn lookup_primary_source_file(db: &Db, media_item_id: Id) -> StreamResult<SourceFileRow> {
    let Some(row) = sqlx::query(
        r#"
        SELECT id, media_item_id, path, probe_duration_seconds
        FROM source_files
        WHERE media_item_id = ?1
        ORDER BY created_at, id
        LIMIT 1
        "#,
    )
    .bind(media_item_id)
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
        SELECT id, media_item_id, path, probe_duration_seconds
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
        id: row.try_get("id")?,
        media_item_id: row.try_get("media_item_id")?,
        path: PathBuf::from(path),
        probe_duration_seconds,
    })
}

fn packaged_transcode_output_row(
    row: &sqlx::sqlite::SqliteRow,
) -> StreamResult<PackagedTranscodeOutputRow> {
    let directory_path = row.try_get::<Option<String>, _>("directory_path")?;
    let Some(directory_path) = directory_path.filter(|value| !value.is_empty()) else {
        return Err(playlist_unavailable(
            "transcode output has no package directory",
        ));
    };

    Ok(PackagedTranscodeOutputRow {
        id: row.try_get("id")?,
        media_item_id: row.try_get("media_item_id")?,
        directory_path: PathBuf::from(directory_path),
        playlist_filename: row.try_get("playlist_filename")?,
        init_filename: row.try_get("init_filename")?,
        encode_metadata_json: row.try_get("encode_metadata_json")?,
    })
}

async fn live_request(
    db: &Db,
    live: &LiveStreamState,
    source_file_id: Id,
    encoded_profile: &str,
) -> StreamResult<LiveRequest> {
    let profile = decode_live_profile(source_file_id, encoded_profile)?;
    let source_file = lookup_source_file(db, source_file_id).await?;
    let profile_json = profile.profile_json();
    let profile_hash = profile.profile_hash();
    let profile_hash_hex = hex_hash(&profile_hash);

    tokio::fs::create_dir_all(&live.cache_root).await?;

    Ok(LiveRequest {
        media_item_id: source_file.media_item_id,
        source_file,
        profile,
        profile_json,
        profile_hash,
        profile_hash_hex,
    })
}

fn decode_live_profile(
    source_file_id: Id,
    encoded_profile: &str,
) -> StreamResult<TranscodeProfile> {
    let bytes = URL_SAFE_NO_PAD
        .decode(encoded_profile)
        .map_err(|_| StreamError::NotFound)?;
    let profile =
        serde_json::from_slice::<TranscodeProfile>(&bytes).map_err(|_| StreamError::NotFound)?;
    if profile.source_file_id != source_file_id {
        return Err(StreamError::NotFound);
    }
    Ok(profile)
}

async fn resolve_live_output(
    live: &LiveStreamState,
    request: LiveRequest,
    segment: Option<u64>,
) -> StreamResult<LiveOutput> {
    if let Some(output) =
        lookup_durable_profile_output(&live.store, request.source_file.id, request.profile_hash)
            .await?
    {
        return Ok(output);
    }

    if let Some(output) = live
        .store
        .fetch_by_key(request.source_file.id, request.profile_hash)
        .await?
    {
        live.store.bump_access(output.id).await?;
        return Ok(LiveOutput {
            directory_path: output.directory_path,
        });
    }

    if let Some(active) = live
        .active
        .get(request.source_file.id, request.profile_hash)
    {
        if let Some(segment) = segment {
            live.active.await_segment(active.id, segment).await?;
        }
        return Ok(LiveOutput {
            directory_path: active.output_dir.clone(),
        });
    }

    if !live.enabled {
        return Err(StreamError::NotFound);
    }

    let output_id = Id::new();
    let output_dir = live.cache_root.join(output_id.to_string());
    let command = live_encode_command(&request, &output_dir);
    let active = live
        .active
        .get_or_spawn(ActiveEncodeRequest {
            source_file_id: request.source_file.id,
            profile_hash: request.profile_hash,
            profile_json: request.profile_json,
            output_dir,
            command,
        })
        .await?;

    if let Some(segment) = segment {
        live.active.await_segment(active.id, segment).await?;
    }

    Ok(LiveOutput {
        directory_path: active.output_dir.clone(),
    })
}

async fn lookup_durable_profile_output(
    store: &EphemeralStore,
    source_file_id: Id,
    profile_hash: [u8; 32],
) -> StreamResult<Option<LiveOutput>> {
    let rows = sqlx::query(
        r#"
        SELECT transcode_outputs.directory_path
        FROM transcode_jobs
        JOIN transcode_outputs ON transcode_outputs.source_file_id = transcode_jobs.source_file_id
        WHERE transcode_jobs.source_file_id = ?1
          AND transcode_jobs.profile_hash = ?2
          AND transcode_jobs.state = 'completed'
          AND transcode_outputs.directory_path IS NOT NULL
        ORDER BY transcode_outputs.created_at ASC, transcode_outputs.id ASC
        LIMIT 1
        "#,
    )
    .bind(source_file_id)
    .bind(profile_hash.as_slice())
    .fetch_optional(store.db().read_pool())
    .await?;

    rows.map(|row| {
        let directory_path: String = row.try_get("directory_path")?;
        Ok(LiveOutput {
            directory_path: PathBuf::from(directory_path),
        })
    })
    .transpose()
}

fn live_encode_command(request: &LiveRequest, output_dir: &Path) -> FfmpegEncodeCommand {
    let (width, height) = live_dimensions(&request.profile);
    let mut command =
        FfmpegEncodeCommand::new("ffmpeg", InputSpec::file(request.source_file.path.clone()))
            .video(VideoOutputSpec {
                codec: request.profile.codec,
                crf: request.profile.vmaf_target.map(|_| 23),
                preset: Preset::Veryfast,
                bit_depth: request.profile.bit_depth,
                color: match request.profile.color {
                    ColorTarget::Sdr => ColorOutput::SdrBt709,
                    ColorTarget::Hdr10 => ColorOutput::CopyFromInput,
                },
                max_resolution: Some((width, height)),
            })
            .audio(match request.profile.audio {
                AudioPolicyKind::StereoAac => AudioPolicy::StereoAac { bitrate_kbps: 192 },
                AudioPolicyKind::StereoAacWithSurroundPassthrough => {
                    AudioPolicy::StereoAacWithSurroundPassthrough { bitrate_kbps: 192 }
                }
                AudioPolicyKind::Copy => AudioPolicy::Copy,
            });
    if let Some(profile_width) = request.profile.width {
        command = command.add_filter(VideoFilter::Scale(profile_width, height));
    }
    command.hls(HlsOutputSpec::cmaf_vod(
        output_dir.to_path_buf(),
        Duration::from_secs(HLS_SEGMENT_TARGET_SECONDS),
    ))
}

fn live_dimensions(profile: &TranscodeProfile) -> (u32, u32) {
    let width = profile.width.unwrap_or(1920);
    let height = width.saturating_mul(9).div_ceil(16).max(1);
    (width, height)
}

fn rewrite_live_playlist_uris(
    source_file_id: Id,
    encoded_profile: &str,
    playlist: &str,
) -> StreamResult<String> {
    let mut rewritten = String::with_capacity(playlist.len());
    for line in playlist.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("#EXT-X-MAP:") {
            rewritten.push_str(&rewrite_live_hls_map_uri(
                source_file_id,
                encoded_profile,
                line,
            )?);
        } else if trimmed.is_empty() || trimmed.starts_with('#') {
            rewritten.push_str(line);
        } else {
            let filename = segment_uri_filename(trimmed)?;
            rewritten.push_str(&format!(
                "/api/v1/stream/live/{source_file_id}/{encoded_profile}/{filename}"
            ));
        }
        rewritten.push('\n');
    }
    Ok(rewritten)
}

fn rewrite_live_hls_map_uri(
    source_file_id: Id,
    encoded_profile: &str,
    line: &str,
) -> StreamResult<String> {
    let Some(attribute_start) = line.find("URI=\"") else {
        return Err(playlist_unavailable("media playlist EXT-X-MAP has no URI"));
    };
    let value_start = attribute_start + "URI=\"".len();
    let Some(value_end) = line[value_start..]
        .find('"')
        .map(|offset| value_start + offset)
    else {
        return Err(playlist_unavailable(
            "media playlist EXT-X-MAP URI is not quoted",
        ));
    };

    let mut rewritten = String::with_capacity(line.len() + 96);
    rewritten.push_str(&line[..value_start]);
    rewritten.push_str(&format!(
        "/api/v1/stream/live/{source_file_id}/{encoded_profile}/init.mp4"
    ));
    rewritten.push_str(&line[value_end..]);
    Ok(rewritten)
}

fn parse_route_segment_number(filename: &str) -> StreamResult<u64> {
    let segment = filename
        .strip_prefix("seg-")
        .and_then(|value| value.strip_suffix(".m4s"))
        .ok_or(StreamError::NotFound)?;
    segment.parse().map_err(|_| StreamError::NotFound)
}

fn hex_hash(hash: &[u8; 32]) -> String {
    let mut value = String::with_capacity(64);
    for byte in hash {
        value.push_str(&format!("{byte:02x}"));
    }
    value
}

fn stream_error_response(error: &StreamError) -> StreamErrorResponse {
    match error {
        StreamError::ProbeDataMissing {
            source_file_id,
            reason,
        } => StreamErrorResponse {
            error: "probe_data_missing".to_owned(),
            message: Some(reason.clone()),
            source_file_id: Some(*source_file_id),
        },
        _ => StreamErrorResponse {
            error: error.to_string(),
            message: None,
            source_file_id: None,
        },
    }
}

fn probe_data_missing(source_file_id: Id, reason: impl Into<String>) -> StreamError {
    let reason = reason.into();
    tracing::warn!(
        source_file_id = %source_file_id,
        reason = %reason,
        "source file is missing persisted probe data"
    );
    StreamError::ProbeDataMissing {
        source_file_id,
        reason,
    }
}

async fn read_subtitle_text(sidecar: &SubtitleSidecar) -> StreamResult<String> {
    tokio::fs::read_to_string(&sidecar.path)
        .await
        .map_err(StreamError::Io)
}

fn subtitle_to_webvtt(sidecar: &SubtitleSidecar, text: &str) -> StreamResult<String> {
    match (sidecar.format, sidecar.provenance) {
        (SubtitleFormat::Srt, SubtitleProvenance::Text) => Ok(srt_to_webvtt(text)),
        (SubtitleFormat::Ass, SubtitleProvenance::Text) => ass_to_webvtt(&sidecar.path, text),
        (SubtitleFormat::Json, SubtitleProvenance::Ocr) => ocr_json_to_webvtt(&sidecar.path, text),
        _ => Err(invalid_subtitle(
            &sidecar.path,
            format!(
                "unsupported subtitle format {} with provenance {}",
                sidecar.format, sidecar.provenance
            ),
        )),
    }
}

fn srt_to_webvtt(text: &str) -> String {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut webvtt = String::from("WEBVTT\n\n");
    for line in text.lines() {
        if line.contains("-->") {
            webvtt.push_str(&line.replace(',', "."));
        } else {
            webvtt.push_str(line);
        }
        webvtt.push('\n');
    }
    webvtt
}

fn ass_to_webvtt(path: &Path, text: &str) -> StreamResult<String> {
    let mut start_index = 1;
    let mut end_index = 2;
    let mut text_index = 9;
    let mut field_count = 10;
    let mut webvtt = String::from("WEBVTT\n\n");
    let mut cues = 0usize;

    for line in text.lines() {
        if let Some(format) = line.trim().strip_prefix("Format:") {
            let fields = format
                .split(',')
                .map(|field| field.trim().to_ascii_lowercase())
                .collect::<Vec<_>>();
            start_index = ass_format_index(path, &fields, "start")?;
            end_index = ass_format_index(path, &fields, "end")?;
            text_index = ass_format_index(path, &fields, "text")?;
            field_count = fields.len();
            continue;
        }

        let Some(dialogue) = line.trim().strip_prefix("Dialogue:") else {
            continue;
        };
        let split_limit = field_count.max(text_index + 1);
        let fields = dialogue
            .splitn(split_limit, ',')
            .map(str::trim)
            .collect::<Vec<_>>();
        let Some(start) = fields.get(start_index) else {
            return Err(invalid_subtitle(
                path,
                "dialogue line is missing start time",
            ));
        };
        let Some(end) = fields.get(end_index) else {
            return Err(invalid_subtitle(path, "dialogue line is missing end time"));
        };
        let Some(cue_text) = fields.get(text_index) else {
            return Err(invalid_subtitle(path, "dialogue line is missing text"));
        };

        webvtt.push_str(&format!(
            "{} --> {}\n{}\n\n",
            ass_timestamp_to_webvtt(path, start)?,
            ass_timestamp_to_webvtt(path, end)?,
            clean_ass_text(cue_text)
        ));
        cues += 1;
    }

    if cues == 0 {
        return Err(invalid_subtitle(path, "ass sidecar has no dialogue cues"));
    }

    Ok(webvtt)
}

fn ass_format_index(path: &Path, fields: &[String], name: &str) -> StreamResult<usize> {
    fields
        .iter()
        .position(|field| field == name)
        .ok_or_else(|| invalid_subtitle(path, format!("ass format is missing {name} field")))
}

fn ass_timestamp_to_webvtt(path: &Path, timestamp: &str) -> StreamResult<String> {
    let (hours, rest) = timestamp
        .split_once(':')
        .ok_or_else(|| invalid_subtitle(path, format!("invalid ass timestamp: {timestamp}")))?;
    let (minutes, rest) = rest
        .split_once(':')
        .ok_or_else(|| invalid_subtitle(path, format!("invalid ass timestamp: {timestamp}")))?;
    let (seconds, fraction) = rest
        .split_once('.')
        .ok_or_else(|| invalid_subtitle(path, format!("invalid ass timestamp: {timestamp}")))?;
    let hours = parse_ass_time_part(path, "hour", hours, timestamp)?;
    let minutes = parse_ass_time_part(path, "minute", minutes, timestamp)?;
    let seconds = parse_ass_time_part(path, "second", seconds, timestamp)?;
    let centiseconds = parse_ass_centiseconds(path, fraction, timestamp)?;

    Ok(format!(
        "{hours:02}:{minutes:02}:{seconds:02}.{milliseconds:03}",
        milliseconds = centiseconds * 10
    ))
}

fn parse_ass_time_part(
    path: &Path,
    field: &'static str,
    value: &str,
    timestamp: &str,
) -> StreamResult<u64> {
    value.parse().map_err(|_| {
        invalid_subtitle(
            path,
            format!("invalid ass timestamp {field} in {timestamp}: {value}"),
        )
    })
}

fn parse_ass_centiseconds(path: &Path, fraction: &str, timestamp: &str) -> StreamResult<u64> {
    if fraction.is_empty() || !fraction.chars().all(|value| value.is_ascii_digit()) {
        return Err(invalid_subtitle(
            path,
            format!("invalid ass timestamp fraction in {timestamp}: {fraction}"),
        ));
    }

    let mut padded = String::from(fraction);
    while padded.len() < 2 {
        padded.push('0');
    }
    padded[..2].parse().map_err(|_| {
        invalid_subtitle(
            path,
            format!("invalid ass timestamp fraction in {timestamp}: {fraction}"),
        )
    })
}

fn clean_ass_text(text: &str) -> String {
    let mut cleaned = String::with_capacity(text.len());
    let mut in_override = false;
    let mut chars = text.chars().peekable();

    while let Some(ch) = chars.next() {
        match ch {
            '{' => in_override = true,
            '}' => in_override = false,
            '\\' if !in_override => match chars.peek().copied() {
                Some('N' | 'n') => {
                    chars.next();
                    cleaned.push('\n');
                }
                Some('h') => {
                    chars.next();
                    cleaned.push(' ');
                }
                _ => cleaned.push(ch),
            },
            _ if !in_override => cleaned.push(ch),
            _ => {}
        }
    }

    cleaned
}

fn ocr_json_to_webvtt(path: &Path, text: &str) -> StreamResult<String> {
    let sidecar = serde_json::from_str::<OcrSubtitleSidecar>(text).map_err(|error| {
        invalid_subtitle(path, format!("ocr sidecar json parse failed: {error}"))
    })?;
    if sidecar.provenance.as_deref() != Some(SubtitleProvenance::Ocr.as_str()) {
        return Err(invalid_subtitle(
            path,
            "ocr sidecar provenance is missing or invalid",
        ));
    }

    let mut webvtt = String::from("WEBVTT\n\n");
    for cue in sidecar.cues {
        webvtt.push_str(&format!("{} --> {}\n{}\n\n", cue.start, cue.end, cue.text));
    }
    Ok(webvtt)
}

#[derive(Debug, Deserialize)]
struct OcrSubtitleSidecar {
    provenance: Option<String>,
    cues: Vec<OcrSubtitleCue>,
}

#[derive(Debug, Deserialize)]
struct OcrSubtitleCue {
    start: String,
    end: String,
    text: String,
}

fn invalid_subtitle(path: &Path, reason: impl Into<String>) -> StreamError {
    StreamError::InvalidSubtitle {
        path: path.to_path_buf(),
        reason: reason.into(),
    }
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

async fn packaged_stream_variants(
    outputs: &[PackagedTranscodeOutputRow],
) -> StreamResult<Vec<StreamVariant>> {
    let mut variants = Vec::with_capacity(outputs.len());
    for output in outputs {
        let Some(metadata_json) = output
            .encode_metadata_json
            .as_deref()
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        let Some(media_playlist_filename) = output
            .playlist_filename
            .as_deref()
            .filter(|value| !value.is_empty())
        else {
            continue;
        };

        let metadata = parse_encode_metadata(output.id, metadata_json)?;
        let playlist_path = output
            .directory_path
            .join(playlist_filename(Some(media_playlist_filename))?);
        let playlist = tokio::fs::read_to_string(&playlist_path).await?;
        let metrics = packaged_playlist_metrics(output, &playlist).await?;
        let metadata_duration = metadata.duration_seconds();
        let resolution = encode_metadata_resolution(output.id, &metadata)?;
        let codecs = required_metadata_text(output.id, "codecs", metadata.codecs)?;
        let video_range = encode_metadata_video_range(output.id, metadata.video_range.as_deref())?;
        variants.push(StreamVariant {
            uri: transcode_media_playlist_uri(output.id),
            bandwidth: estimate_bandwidth(
                metrics.bytes,
                metrics
                    .duration_seconds
                    .or(metadata_duration)
                    .filter(|duration| *duration > 0),
            ),
            codecs,
            resolution,
            video_range,
        });
    }

    if variants.is_empty() {
        return Err(playlist_unavailable(
            "packaged transcode outputs are missing playlists or encode metadata",
        ));
    }

    Ok(variants)
}

fn parse_encode_metadata(id: Id, metadata_json: &str) -> StreamResult<EncodeMetadata> {
    serde_json::from_str(metadata_json).map_err(|error| {
        playlist_unavailable(format!(
            "transcode output {id} encode metadata json is invalid: {error}"
        ))
    })
}

fn encode_metadata_resolution(
    id: Id,
    metadata: &EncodeMetadata,
) -> StreamResult<Option<(u32, u32)>> {
    if let Some(resolution) = &metadata.resolution {
        return match resolution {
            EncodeMetadataResolution::Text(resolution) => {
                let resolution = required_metadata_text(id, "resolution", resolution.to_owned())?;
                let Some((width, height)) = resolution.split_once('x') else {
                    return Err(invalid_encode_metadata(
                        id,
                        "resolution must be formatted as WIDTHxHEIGHT",
                    ));
                };
                let width = parse_positive_u32(id, "resolution width", width)?;
                let height = parse_positive_u32(id, "resolution height", height)?;
                Ok(Some((width, height)))
            }
            EncodeMetadataResolution::Pair((width, height)) if *width > 0 && *height > 0 => {
                Ok(Some((*width, *height)))
            }
            EncodeMetadataResolution::Pair(_) => Err(invalid_encode_metadata(
                id,
                "resolution width and height must be positive",
            )),
        };
    }

    match (metadata.width, metadata.height) {
        (Some(width), Some(height)) if width > 0 && height > 0 => Ok(Some((width, height))),
        (None, None) => Err(invalid_encode_metadata(id, "resolution is missing")),
        _ => Err(invalid_encode_metadata(
            id,
            "width and height must both be present and positive",
        )),
    }
}

fn encode_metadata_video_range(id: Id, value: Option<&str>) -> StreamResult<Option<VideoRange>> {
    let value = value.ok_or_else(|| invalid_encode_metadata(id, "video_range is missing"))?;
    let value = required_metadata_text(id, "video_range", value.to_owned())?;
    VideoRange::parse(&value)
        .map(Some)
        .ok_or_else(|| invalid_encode_metadata(id, "video_range must be SDR, PQ, or HLG"))
}

fn required_metadata_text(id: Id, field: &'static str, value: String) -> StreamResult<String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(invalid_encode_metadata(id, format!("{field} is empty")));
    }
    Ok(value.to_owned())
}

fn parse_positive_u32(id: Id, field: &'static str, value: &str) -> StreamResult<u32> {
    value
        .parse::<u32>()
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| invalid_encode_metadata(id, format!("{field} must be positive")))
}

fn invalid_encode_metadata(id: Id, reason: impl Into<String>) -> StreamError {
    playlist_unavailable(format!(
        "transcode output {id} encode metadata is invalid: {}",
        reason.into()
    ))
}

async fn packaged_playlist_metrics(
    output: &PackagedTranscodeOutputRow,
    playlist: &str,
) -> StreamResult<PackagedPlaylistMetrics> {
    let mut bytes = 0_u64;
    let mut duration = 0_f64;

    if let Some(init_file) = output
        .init_filename
        .as_deref()
        .filter(|value| !value.is_empty())
    {
        bytes = bytes.saturating_add(
            tokio::fs::metadata(output.directory_path.join(init_filename(Some(init_file))?))
                .await?
                .len(),
        );
    }

    for line in playlist.lines().map(str::trim) {
        if let Some(extinf) = line.strip_prefix("#EXTINF:") {
            let value = extinf.split_once(',').map_or(extinf, |(value, _)| value);
            if let Ok(seconds) = value.parse::<f64>() {
                duration += seconds;
            }
            continue;
        }
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let filename = segment_uri_filename(line)?;
        bytes = bytes.saturating_add(
            tokio::fs::metadata(output.directory_path.join(filename))
                .await?
                .len(),
        );
    }

    Ok(PackagedPlaylistMetrics {
        bytes,
        duration_seconds: (duration > 0.0).then(|| duration.ceil() as u64),
    })
}

fn rewrite_transcode_playlist_uris(id: Id, playlist: &str) -> StreamResult<String> {
    let mut rewritten = String::with_capacity(playlist.len());
    for line in playlist.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("#EXT-X-MAP:") {
            rewritten.push_str(&rewrite_hls_map_uri(id, line)?);
        } else if trimmed.is_empty() || trimmed.starts_with('#') {
            rewritten.push_str(line);
        } else {
            let filename = segment_uri_filename(trimmed)?;
            rewritten.push_str(&format!("/api/v1/stream/transcodes/{id}/{filename}"));
        }
        rewritten.push('\n');
    }
    Ok(rewritten)
}

fn rewrite_hls_map_uri(id: Id, line: &str) -> StreamResult<String> {
    let Some(attribute_start) = line.find("URI=\"") else {
        return Err(playlist_unavailable("media playlist EXT-X-MAP has no URI"));
    };
    let value_start = attribute_start + "URI=\"".len();
    let Some(value_end) = line[value_start..]
        .find('"')
        .map(|offset| value_start + offset)
    else {
        return Err(playlist_unavailable(
            "media playlist EXT-X-MAP URI is not quoted",
        ));
    };

    let mut rewritten = String::with_capacity(line.len() + 64);
    rewritten.push_str(&line[..value_start]);
    rewritten.push_str(&format!("/api/v1/stream/transcodes/{id}/init.mp4"));
    rewritten.push_str(&line[value_end..]);
    Ok(rewritten)
}

fn segment_uri_filename(uri: &str) -> StreamResult<String> {
    let filename = uri.rsplit('/').next().unwrap_or(uri);
    let filename = filename
        .split_once('?')
        .map_or(filename, |(filename, _)| filename);
    if !filename.starts_with("seg-") || !filename.ends_with(".m4s") {
        return Err(playlist_unavailable(
            "media playlist segment URI must use seg-{n}.m4s",
        ));
    }
    let segment = &filename["seg-".len()..filename.len() - ".m4s".len()];
    segment_filename(segment)
}

fn playlist_filename(value: Option<&str>) -> StreamResult<&str> {
    path_filename(value, "playlist_filename")
}

fn init_filename(value: Option<&str>) -> StreamResult<&str> {
    path_filename(value, "init_filename")
}

fn path_filename<'a>(value: Option<&'a str>, field: &'static str) -> StreamResult<&'a str> {
    let Some(value) = value.filter(|value| !value.is_empty()) else {
        return Err(playlist_unavailable(format!(
            "transcode output is missing {field}"
        )));
    };
    if value.contains('/') || value.contains('\\') || value == "." || value == ".." {
        return Err(playlist_unavailable(format!(
            "transcode output {field} must be a file name"
        )));
    }
    Ok(value)
}

fn segment_filename(segment: &str) -> StreamResult<String> {
    if segment.len() != 5 || !segment.chars().all(|character| character.is_ascii_digit()) {
        return Err(StreamError::NotFound);
    }
    Ok(format!("seg-{segment}.m4s"))
}

fn route_segment_filename(segment: &str) -> StreamResult<String> {
    if !segment.starts_with("seg-") || !segment.ends_with(".m4s") {
        return Err(StreamError::NotFound);
    }
    let segment = &segment["seg-".len()..segment.len() - ".m4s".len()];
    segment_filename(segment)
}

fn source_media_playlist_uri(media_item_id: Id, source_file_id: Id) -> String {
    format!("/api/v1/stream/items/{media_item_id}/{source_file_id}/media.m3u8")
}

fn transcode_media_playlist_uri(transcode_output_id: Id) -> String {
    format!("/api/v1/stream/transcodes/{transcode_output_id}/media.m3u8")
}

fn build_master_playlist(
    media_item_id: Id,
    audio_playlist_uri: String,
    variants: Vec<StreamVariant>,
    probe: &ProbeResult,
    subtitles: &[SubtitleTrackRow],
) -> String {
    let mut playlist = String::from("#EXTM3U\n#EXT-X-VERSION:7\n");

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
            &format!("{audio_playlist_uri}?audio={}", audio.index),
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

    for variant in variants {
        playlist.push_str("#EXT-X-STREAM-INF:");
        playlist.push_str(&format!(
            "BANDWIDTH={},CODECS=\"{}\"",
            variant.bandwidth, variant.codecs
        ));
        if let Some((width, height)) = variant.resolution {
            playlist.push_str(&format!(",RESOLUTION={width}x{height}"));
        }
        if let Some(video_range) = variant.video_range {
            playlist.push_str(",VIDEO-RANGE=");
            playlist.push_str(video_range.as_hls_value());
        }
        if !probe.audio_streams.is_empty() {
            playlist.push_str(",AUDIO=\"audio\"");
        }
        if !subtitles.is_empty() {
            playlist.push_str(",SUBTITLES=\"subs\"");
        }
        playlist.push('\n');
        playlist.push_str(&variant.uri);
        playlist.push('\n');
    }

    playlist
}

fn build_media_playlist(
    source_file_id: Id,
    source_file_path: &Path,
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
    let source_uri = source_file_uri(source_file_id, source_file_path)?;
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

fn source_file_uri(source_file_id: Id, source_file_path: &Path) -> StreamResult<String> {
    let extension = source_file_extension(source_file_path)
        .ok_or_else(|| playlist_unavailable("source file has no extension"))?;
    Ok(format!(
        "/api/v1/stream/sourcefile/{source_file_id}/file.{extension}"
    ))
}

fn validate_source_file_extension(source_file_path: &Path, requested: &str) -> StreamResult<()> {
    let Some(extension) = source_file_extension(source_file_path) else {
        return Err(StreamError::NotFound);
    };
    if extension.eq_ignore_ascii_case(requested) {
        Ok(())
    } else {
        Err(StreamError::NotFound)
    }
}

fn source_file_extension(path: &Path) -> Option<&str> {
    path.extension()
        .and_then(|extension| extension.to_str())
        .filter(|extension| !extension.is_empty())
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
        Some("m4s") => "video/iso.segment",
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
