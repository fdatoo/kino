use std::{
    path::{Path, PathBuf},
    process::ExitStatus,
};

use kino_core::{
    CanonicalIdentityId, CanonicalIdentityKind, CanonicalLayoutTransfer, Id, RequestFailureReason,
};
use kino_fulfillment::{
    ExpectedProbedFile, FfprobeFileProbe, ProbeResult, ProbeSubtitleKind,
    ProbedFileVerificationResult, RequestDetail, RequestEventActor, RequestTransition,
    movie::TmdbMovieId, tmdb::TmdbClient,
};
use kino_library::{
    CanonicalLayoutInput, CanonicalLayoutResult, CanonicalMediaTarget,
    FfmpegImageSubtitleExtractor, ImageSubtitleExtraction, ImageSubtitleExtractionInput,
    MetadataService, OcrSubtitleExtractionInput, OcrSubtitleTrack, ProbedSubtitleFormat,
    ProbedSubtitleTrack, RegisterMediaItemInput, RegisterSourceFileInput,
    SourceFileAudioTrackInput, SourceFileProbeInput, SourceFileSubtitleTrackInput,
    SubtitleExtractionInput, SubtitleFormat, SubtitleProvenance, SubtitleService,
    subtitle_ocr::{TesseractOcrEngine, ocr_subtitle_track},
};
use sha2::{Digest, Sha256};
use tokio::{io::AsyncReadExt, process::Command};

use crate::request::{ApiResult, AppState};

/// Ingest a manual-import source path and transition the request to satisfied or failed.
pub(crate) async fn ingest_request(
    state: &AppState,
    request_id: Id,
    source_path: PathBuf,
    job_id: String,
) -> ApiResult<RequestDetail> {
    let mut media_item_id = None;
    let mut layout_result = None;
    let result = ingest_request_inner(
        state,
        request_id,
        &source_path,
        &job_id,
        &mut media_item_id,
        &mut layout_result,
    )
    .await;

    match result {
        Ok(detail) => Ok(detail),
        Err(error) => {
            cleanup_failed_ingest(state, media_item_id, layout_result.as_ref()).await;
            let message = format!("manual import {job_id} failed: {error}");
            tracing::error!(
                request_id = %request_id,
                job_id,
                source_path = %source_path.display(),
                error = %error,
                "manual import ingestion failed"
            );
            Ok(state
                .requests
                .transition(
                    request_id,
                    RequestTransition::Fail(RequestFailureReason::IngestFailed),
                    Some(RequestEventActor::System),
                    Some(&message),
                )
                .await?)
        }
    }
}

async fn ingest_request_inner(
    state: &AppState,
    request_id: Id,
    source_path: &Path,
    job_id: &str,
    media_item_id: &mut Option<Id>,
    layout_result: &mut Option<CanonicalLayoutResult>,
) -> Result<RequestDetail, IngestError> {
    let current = state.requests.get(request_id).await?;
    let canonical_identity_id = current
        .request
        .target
        .canonical_identity_id
        .ok_or(IngestError::MissingCanonicalIdentity { request_id })?;
    let probe = FfprobeFileProbe::new().probe(source_path).await?;
    let tmdb = state.tmdb.as_ref().ok_or(IngestError::TmdbNotConfigured)?;
    let expected = expected_probed_file(tmdb, canonical_identity_id).await?;
    let verification = state
        .requests
        .verify_probed_file(
            request_id,
            expected,
            probe.as_probed_file(),
            Some(RequestEventActor::System),
        )
        .await?;
    match verification {
        ProbedFileVerificationResult::Matched { .. } => {}
        ProbedFileVerificationResult::Mismatched { detail, .. } => return Ok(*detail),
    }

    let item = state
        .catalog
        .register_media_item(RegisterMediaItemInput::new(canonical_identity_id))
        .await?;
    *media_item_id = Some(item.id);

    let metadata = MetadataService::with_artwork_cache_dir(
        state.db.clone(),
        state.library_root.join("Metadata"),
        state.artwork_cache_dir.clone(),
    )
    .enrich_tmdb_media_item(item.id, canonical_identity_id, tmdb)
    .await?;
    let target = canonical_target(canonical_identity_id, &metadata)?;
    let placed = state
        .canonical_layout
        .place(CanonicalLayoutInput::new(source_path, target))
        .await?;
    let canonical_path = placed.canonical_path.clone();
    *layout_result = Some(placed);

    let sidecar_dir = sidecar_dir(&canonical_path)?;
    let text_tracks = extract_text_subtitles(&canonical_path, &probe).await?;
    let text_source_tracks = source_text_subtitle_tracks(&text_tracks);
    let subtitle_service = SubtitleService::new(state.db.clone());
    subtitle_service
        .extract_text_subtitles(SubtitleExtractionInput::new(
            item.id,
            &sidecar_dir,
            text_tracks,
        ))
        .await?;
    let ocr_tracks = extract_ocr_subtitles(&canonical_path, &probe).await?;
    let ocr_source_tracks = source_ocr_subtitle_tracks(&ocr_tracks);
    subtitle_service
        .extract_ocr_subtitles(OcrSubtitleExtractionInput::new(
            item.id,
            &sidecar_dir,
            ocr_tracks,
        ))
        .await?;

    let mut source_probe = source_file_probe(&probe);
    source_probe.subtitle_tracks = text_source_tracks
        .into_iter()
        .chain(ocr_source_tracks)
        .collect();
    let source_file = state
        .catalog
        .register_source_file(RegisterSourceFileInput::new(
            item.id,
            &canonical_path,
            source_probe,
        ))
        .await?;

    let message = format!(
        "manual import {job_id} ingested source file {}",
        source_file.path.display()
    );
    Ok(state
        .requests
        .transition(
            request_id,
            RequestTransition::Satisfy,
            Some(RequestEventActor::System),
            Some(&message),
        )
        .await?)
}

async fn expected_probed_file(
    tmdb: &TmdbClient,
    canonical_identity_id: CanonicalIdentityId,
) -> Result<ExpectedProbedFile, IngestError> {
    let mut expected = ExpectedProbedFile::new(canonical_identity_id);
    if canonical_identity_id.kind() == CanonicalIdentityKind::Movie {
        let movie_id = TmdbMovieId::new(canonical_identity_id.tmdb_id().get()).ok_or(
            IngestError::InvalidTmdbMovieId {
                canonical_identity_id,
            },
        )?;
        if let Some(runtime_minutes) = tmdb.movie_details(movie_id).await?.runtime_minutes {
            expected = expected.with_runtime_seconds(runtime_minutes.saturating_mul(60));
        }
    }

    Ok(expected)
}

fn canonical_target(
    canonical_identity_id: CanonicalIdentityId,
    metadata: &kino_library::CachedMediaMetadata,
) -> Result<CanonicalMediaTarget, IngestError> {
    match canonical_identity_id.kind() {
        CanonicalIdentityKind::Movie => Ok(CanonicalMediaTarget::movie(
            metadata.title.clone(),
            release_year(metadata.release_date.as_deref()).ok_or(
                IngestError::MissingReleaseYear {
                    canonical_identity_id,
                },
            )?,
        )),
        CanonicalIdentityKind::TvSeries => Ok(CanonicalMediaTarget::tv_episode(
            metadata.title.clone(),
            1,
            1,
        )),
    }
}

fn release_year(release_date: Option<&str>) -> Option<u16> {
    let year = release_date?.get(..4)?;
    if !year.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }

    year.parse().ok()
}

pub(crate) fn source_file_probe(probe: &ProbeResult) -> SourceFileProbeInput {
    let mut input = SourceFileProbeInput::new();
    input.duration_seconds = probe
        .duration
        .and_then(|duration| (duration.as_secs() > 0).then_some(duration.as_secs()));
    input.container = probe
        .container
        .as_ref()
        .and_then(|container| container.format_names.first().cloned())
        .or_else(|| {
            probe
                .container
                .as_ref()
                .and_then(|container| container.format_long_name.clone())
        });
    if let Some(video) = probe.video_streams.first() {
        input.video_codec = video.codec_name.clone();
        input.video_width = video.width;
        input.video_height = video.height;
    }
    input.audio_tracks = probe
        .audio_streams
        .iter()
        .map(|track| SourceFileAudioTrackInput {
            track_index: track.index,
            codec: track.codec_name.clone(),
            language: track.language.clone(),
            channels: track.channels,
        })
        .collect();
    input
}

async fn extract_text_subtitles(
    source_path: &Path,
    probe: &ProbeResult,
) -> Result<Vec<ProbedSubtitleTrack>, IngestError> {
    let mut tracks = Vec::new();
    for stream in probe
        .subtitle_streams
        .iter()
        .filter(|stream| stream.kind.is_text())
    {
        let format = match stream.kind {
            ProbeSubtitleKind::Srt => ProbedSubtitleFormat::Srt,
            ProbeSubtitleKind::Ass => ProbedSubtitleFormat::Ass,
            ProbeSubtitleKind::ImagePgs
            | ProbeSubtitleKind::ImageVobSub
            | ProbeSubtitleKind::ImageDvb
            | ProbeSubtitleKind::Other => continue,
        };
        let text = extract_text_subtitle(source_path, stream.index, format).await?;
        if text.trim().is_empty() {
            tracing::warn!(
                source_path = %source_path.display(),
                track_index = stream.index,
                "skipping empty text subtitle track"
            );
            continue;
        }
        tracks.push(ProbedSubtitleTrack::new(
            stream.index,
            subtitle_language(stream.language.as_deref()),
            format,
            text,
        ));
    }

    Ok(tracks)
}

async fn extract_text_subtitle(
    source_path: &Path,
    stream_index: u32,
    format: ProbedSubtitleFormat,
) -> Result<String, IngestError> {
    let muxer = match format {
        ProbedSubtitleFormat::Srt => "srt",
        ProbedSubtitleFormat::Ass => "ass",
        ProbedSubtitleFormat::Pgs | ProbedSubtitleFormat::VobSub => {
            return Err(IngestError::UnsupportedTextSubtitleFormat);
        }
    };
    let output = Command::new("ffmpeg")
        .arg("-hide_banner")
        .arg("-nostdin")
        .arg("-v")
        .arg("error")
        .arg("-i")
        .arg(source_path)
        .arg("-map")
        .arg(format!("0:{stream_index}"))
        .arg("-f")
        .arg(muxer)
        .arg("-")
        .output()
        .await
        .map_err(|source| IngestError::TextSubtitleExtractionIo {
            stream_index,
            source,
        })?;

    if !output.status.success() {
        return Err(IngestError::TextSubtitleExtractionFailed {
            stream_index,
            status: status_string(output.status),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }

    String::from_utf8(output.stdout).map_err(IngestError::TextSubtitleUtf8)
}

async fn extract_ocr_subtitles(
    source_path: &Path,
    probe: &ProbeResult,
) -> Result<Vec<OcrSubtitleTrack>, IngestError> {
    let image_streams = probe
        .subtitle_streams
        .iter()
        .filter(|stream| stream.kind.is_image())
        .collect::<Vec<_>>();
    if image_streams.is_empty() {
        return Ok(Vec::new());
    }
    if !tesseract_available().await {
        tracing::warn!(
            source_path = %source_path.display(),
            "image subtitles found but tesseract is unavailable; OCR deferred"
        );
        return Ok(Vec::new());
    }

    let source_hash = file_sha256_hex(source_path).await?;
    let extractor = FfmpegImageSubtitleExtractor::new(
        source_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(".kino")
            .join("subtitle-images"),
    );
    let engine = TesseractOcrEngine::from_env();
    let mut tracks = Vec::new();
    for stream in image_streams {
        let frames = extractor
            .extract_image_subtitle_track(ImageSubtitleExtractionInput::new(
                source_path,
                &source_hash,
                stream.index,
                stream.kind.into(),
            ))
            .await?;
        let cues = ocr_subtitle_track(&engine, &frames).await?;
        tracks.push(OcrSubtitleTrack::new(
            stream.index,
            subtitle_language(stream.language.as_deref()),
            cues,
        ));
    }

    Ok(tracks)
}

fn source_text_subtitle_tracks(
    tracks: &[ProbedSubtitleTrack],
) -> Vec<SourceFileSubtitleTrackInput> {
    tracks
        .iter()
        .filter_map(|track| {
            let format = match track.format {
                ProbedSubtitleFormat::Srt => SubtitleFormat::Srt,
                ProbedSubtitleFormat::Ass => SubtitleFormat::Ass,
                ProbedSubtitleFormat::Pgs | ProbedSubtitleFormat::VobSub => return None,
            };
            Some(SourceFileSubtitleTrackInput {
                track_index: track.track_index,
                format,
                provenance: SubtitleProvenance::Text,
                language: track.language.clone(),
                forced: false,
            })
        })
        .collect()
}

fn source_ocr_subtitle_tracks(tracks: &[OcrSubtitleTrack]) -> Vec<SourceFileSubtitleTrackInput> {
    tracks
        .iter()
        .map(|track| SourceFileSubtitleTrackInput {
            track_index: track.track_index,
            format: SubtitleFormat::Json,
            provenance: SubtitleProvenance::Ocr,
            language: track.language.clone(),
            forced: false,
        })
        .collect()
}

fn subtitle_language(language: Option<&str>) -> String {
    language
        .map(str::trim)
        .filter(|language| !language.is_empty())
        .unwrap_or("und")
        .to_owned()
}

fn sidecar_dir(canonical_path: &Path) -> Result<PathBuf, IngestError> {
    let parent = canonical_path
        .parent()
        .ok_or_else(|| IngestError::InvalidCanonicalPath {
            path: canonical_path.to_path_buf(),
        })?;
    Ok(parent.join(".kino").join("subtitles"))
}

async fn file_sha256_hex(path: &Path) -> Result<String, IngestError> {
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|source| IngestError::FileHashIo {
            path: path.to_path_buf(),
            source,
        })?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let read = file
            .read(&mut buffer)
            .await
            .map_err(|source| IngestError::FileHashIo {
                path: path.to_path_buf(),
                source,
            })?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }

    Ok(format!("{:x}", hasher.finalize()))
}

async fn tesseract_available() -> bool {
    let binary_path = std::env::var_os("KINO_OCR__TESSERACT_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("tesseract"));
    Command::new(binary_path)
        .arg("--version")
        .output()
        .await
        .is_ok_and(|output| output.status.success())
}

async fn cleanup_failed_ingest(
    state: &AppState,
    media_item_id: Option<Id>,
    layout_result: Option<&CanonicalLayoutResult>,
) {
    if let Some(media_item_id) = media_item_id
        && let Err(error) = state.catalog.remove_media_item(media_item_id).await
    {
        tracing::warn!(media_item_id = %media_item_id, error = %error, "failed to clean up media item after ingest failure");
    }
    if let Some(layout) = layout_result {
        match layout.transfer {
            CanonicalLayoutTransfer::HardLink => {
                if let Err(error) = tokio::fs::remove_file(&layout.canonical_path).await
                    && error.kind() != std::io::ErrorKind::NotFound
                {
                    tracing::warn!(
                        path = %layout.canonical_path.display(),
                        error = %error,
                        "failed to clean up canonical hard link after ingest failure"
                    );
                }
            }
            CanonicalLayoutTransfer::Move => {
                if let Err(error) =
                    tokio::fs::rename(&layout.canonical_path, &layout.source_path).await
                    && error.kind() != std::io::ErrorKind::NotFound
                {
                    tracing::warn!(
                        source = %layout.canonical_path.display(),
                        destination = %layout.source_path.display(),
                        error = %error,
                        "failed to move canonical file back after ingest failure"
                    );
                }
            }
        }
    }
}

fn status_string(status: ExitStatus) -> String {
    status
        .code()
        .map_or_else(|| status.to_string(), |code| format!("exit code {code}"))
}

#[derive(Debug, thiserror::Error)]
enum IngestError {
    #[error(transparent)]
    Fulfillment(#[from] kino_fulfillment::Error),

    #[error(transparent)]
    Probe(#[from] kino_fulfillment::ProbeError),

    #[error(transparent)]
    Library(#[from] kino_library::Error),

    #[error("tmdb client is not configured")]
    TmdbNotConfigured,

    #[error(transparent)]
    Tmdb(#[from] kino_fulfillment::tmdb::Error),

    #[error("canonical identity {canonical_identity_id} is not a valid TMDB movie id")]
    InvalidTmdbMovieId {
        canonical_identity_id: CanonicalIdentityId,
    },

    #[error("request {request_id} does not have a canonical identity")]
    MissingCanonicalIdentity { request_id: Id },

    #[error("metadata for {canonical_identity_id} does not include a release year")]
    MissingReleaseYear {
        canonical_identity_id: CanonicalIdentityId,
    },

    #[error("canonical path has no parent: {path}", path = .path.display())]
    InvalidCanonicalPath { path: PathBuf },

    #[error("unsupported text subtitle format")]
    UnsupportedTextSubtitleFormat,

    #[error("extracting subtitle track {stream_index}: {source}")]
    TextSubtitleExtractionIo {
        stream_index: u32,
        #[source]
        source: std::io::Error,
    },

    #[error("extracting subtitle track {stream_index} failed with {status}: {stderr}")]
    TextSubtitleExtractionFailed {
        stream_index: u32,
        status: String,
        stderr: String,
    },

    #[error("subtitle text is not utf-8: {0}")]
    TextSubtitleUtf8(#[from] std::string::FromUtf8Error),

    #[error("hashing source file {path}: {source}", path = .path.display())]
    FileHashIo {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}
