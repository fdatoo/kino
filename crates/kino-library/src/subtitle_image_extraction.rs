//! Image-subtitle frame extraction for OCR staging.
//!
//! The ffmpeg-backed extractor writes one deterministic staging directory per
//! input-file hash and subtitle stream index. It runs:
//!
//! `ffmpeg -hide_banner -nostdin -y -i <input> -map 0:<stream_index> -f image2 -frame_pts true <track_dir>/frame-%020d.png`
//!
//! It also runs ffprobe to read packet timestamps for the same absolute stream
//! index, because the OCR step needs start and end times alongside the images.

use std::{
    fmt,
    future::Future,
    io,
    path::{Path, PathBuf},
    pin::Pin,
    process::ExitStatus,
    time::Duration,
};

use kino_core::Config;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::process::Command;

use crate::{Error, Result};

/// Default ffmpeg executable resolved from the process path.
pub const DEFAULT_FFMPEG_PROGRAM: &str = "ffmpeg";
/// Default ffprobe executable resolved from the process path for subtitle timing.
pub const DEFAULT_FFPROBE_PROGRAM: &str = "ffprobe";

/// Subtitle codec classification consumed by image extraction policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProbeSubtitleKind {
    /// SubRip text subtitles.
    Srt,
    /// Advanced SubStation Alpha text subtitles.
    Ass,
    /// Presentation Graphic Stream image subtitles.
    ImagePgs,
    /// VOBSUB image subtitles.
    ImageVobSub,
    /// DVB image subtitles.
    ImageDvb,
    /// Subtitle codec is not classified by Kino.
    Other,
}

impl ProbeSubtitleKind {
    /// Whether this subtitle stream can be extracted as OCR image frames.
    pub const fn is_image(self) -> bool {
        matches!(self, Self::ImagePgs | Self::ImageVobSub | Self::ImageDvb)
    }
}

impl fmt::Display for ProbeSubtitleKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Srt => "srt",
            Self::Ass => "ass",
            Self::ImagePgs => "image_pgs",
            Self::ImageVobSub => "image_vob_sub",
            Self::ImageDvb => "image_dvb",
            Self::Other => "other",
        })
    }
}

/// One bitmap subtitle event rendered to an image file for OCR.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageSubtitleFrame {
    /// Subtitle display start time relative to the source media timeline.
    pub start: Duration,
    /// Subtitle display end time relative to the source media timeline.
    pub end: Duration,
    /// Extracted image file on disk.
    pub image_path: PathBuf,
}

impl ImageSubtitleFrame {
    /// Construct an image subtitle frame.
    pub fn new(start: Duration, end: Duration, image_path: impl Into<PathBuf>) -> Self {
        Self {
            start,
            end,
            image_path: image_path.into(),
        }
    }
}

/// Input for extracting one image subtitle stream from a source file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageSubtitleExtractionInput {
    /// Source media file that contains the subtitle stream.
    pub source_path: PathBuf,
    /// Stable content hash of the source media file.
    pub input_file_hash: String,
    /// Absolute ffprobe stream index of the subtitle stream.
    pub stream_index: u32,
    /// Subtitle format classification from the probe step.
    pub kind: ProbeSubtitleKind,
}

impl ImageSubtitleExtractionInput {
    /// Construct image subtitle extraction input.
    pub fn new(
        source_path: impl Into<PathBuf>,
        input_file_hash: impl Into<String>,
        stream_index: u32,
        kind: ProbeSubtitleKind,
    ) -> Self {
        Self {
            source_path: source_path.into(),
            input_file_hash: input_file_hash.into(),
            stream_index,
            kind,
        }
    }
}

/// Boxed future returned by image subtitle extractors.
pub type ImageSubtitleExtractionFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Vec<ImageSubtitleFrame>>> + Send + 'a>>;

/// Extracts image subtitle tracks into OCR-ready frames.
pub trait ImageSubtitleExtraction: Send + Sync {
    /// Extract one image subtitle stream into frame images and timings.
    fn extract_image_subtitle_track<'a>(
        &'a self,
        input: ImageSubtitleExtractionInput,
    ) -> ImageSubtitleExtractionFuture<'a>;
}

/// ffmpeg-backed image subtitle extractor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FfmpegImageSubtitleExtractor {
    ffmpeg_program: PathBuf,
    ffprobe_program: PathBuf,
    staging_dir: PathBuf,
}

impl FfmpegImageSubtitleExtractor {
    /// Construct an extractor using `ffmpeg` and `ffprobe` from `PATH`.
    pub fn new(staging_dir: impl Into<PathBuf>) -> Self {
        Self {
            ffmpeg_program: PathBuf::from(DEFAULT_FFMPEG_PROGRAM),
            ffprobe_program: PathBuf::from(DEFAULT_FFPROBE_PROGRAM),
            staging_dir: staging_dir.into(),
        }
    }

    /// Construct an extractor with explicit ffmpeg-compatible executables.
    pub fn with_programs(
        ffmpeg_program: impl Into<PathBuf>,
        ffprobe_program: impl Into<PathBuf>,
        staging_dir: impl Into<PathBuf>,
    ) -> Self {
        Self {
            ffmpeg_program: ffmpeg_program.into(),
            ffprobe_program: ffprobe_program.into(),
            staging_dir: staging_dir.into(),
        }
    }

    /// Construct an extractor from process configuration.
    pub fn from_config(config: &Config) -> Self {
        let staging_dir = config
            .library
            .subtitle_staging_dir
            .clone()
            .unwrap_or_else(|| default_subtitle_staging_dir(&config.library_root));
        Self::new(staging_dir)
    }

    /// Return the configured ffmpeg-compatible executable.
    pub fn ffmpeg_program(&self) -> &Path {
        &self.ffmpeg_program
    }

    /// Return the configured ffprobe-compatible executable.
    pub fn ffprobe_program(&self) -> &Path {
        &self.ffprobe_program
    }

    /// Return the root directory used for subtitle frame staging.
    pub fn staging_dir(&self) -> &Path {
        &self.staging_dir
    }

    async fn extract(
        &self,
        input: ImageSubtitleExtractionInput,
    ) -> Result<Vec<ImageSubtitleFrame>> {
        if !input.kind.is_image() {
            return Err(Error::SubtitleExtractionUnsupportedFormat { kind: input.kind });
        }

        let timings = self.probe_timings(&input).await?;
        let output_dir = image_subtitle_track_output_dir(
            &self.staging_dir,
            &input.input_file_hash,
            input.stream_index,
        );
        reset_output_dir(&output_dir).await?;

        self.run_ffmpeg(&input, &output_dir).await?;
        let image_paths = image_paths(&output_dir, input.stream_index).await?;
        if image_paths.is_empty() && !timings.is_empty() {
            return Err(subtitle_extraction_error(
                input.stream_index,
                "ffmpeg produced no image frames for subtitle packets",
            ));
        }
        if timings.len() < image_paths.len() {
            return Err(subtitle_extraction_error(
                input.stream_index,
                format!(
                    "ffprobe reported {} subtitle packets for {} extracted image frames",
                    timings.len(),
                    image_paths.len()
                ),
            ));
        }

        Ok(image_paths
            .into_iter()
            .zip(timings)
            .map(|(image_path, timing)| ImageSubtitleFrame {
                start: timing.start,
                end: timing.end,
                image_path,
            })
            .collect())
    }

    async fn run_ffmpeg(
        &self,
        input: &ImageSubtitleExtractionInput,
        output_dir: &Path,
    ) -> Result<()> {
        let output_pattern = output_dir.join("frame-%020d.png");
        let output = Command::new(&self.ffmpeg_program)
            .arg("-hide_banner")
            .arg("-nostdin")
            .arg("-y")
            .arg("-i")
            .arg(&input.source_path)
            .arg("-map")
            .arg(format!("0:{}", input.stream_index))
            .arg("-f")
            .arg("image2")
            .arg("-frame_pts")
            .arg("true")
            .arg(&output_pattern)
            .output()
            .await
            .map_err(|source| Error::SubtitleExtractionFailed {
                stream_index: input.stream_index,
                source,
            })?;

        if !output.status.success() {
            return Err(Error::SubtitleExtractionFailed {
                stream_index: input.stream_index,
                source: io::Error::other(process_failure(
                    &self.ffmpeg_program,
                    output.status,
                    &output.stderr,
                )),
            });
        }

        Ok(())
    }

    async fn probe_timings(
        &self,
        input: &ImageSubtitleExtractionInput,
    ) -> Result<Vec<SubtitlePacketTiming>> {
        let output = Command::new(&self.ffprobe_program)
            .arg("-v")
            .arg("error")
            .arg("-show_streams")
            .arg("-show_packets")
            .arg("-show_entries")
            .arg("stream=index,codec_type:packet=stream_index,pts_time,duration_time")
            .arg("-of")
            .arg("json")
            .arg(&input.source_path)
            .output()
            .await
            .map_err(|source| Error::SubtitleExtractionFailed {
                stream_index: input.stream_index,
                source,
            })?;

        if !output.status.success() {
            return Err(Error::SubtitleExtractionFailed {
                stream_index: input.stream_index,
                source: io::Error::other(process_failure(
                    &self.ffprobe_program,
                    output.status,
                    &output.stderr,
                )),
            });
        }

        let output: FfprobePacketOutput =
            serde_json::from_slice(&output.stdout).map_err(|source| {
                Error::SubtitleExtractionFailed {
                    stream_index: input.stream_index,
                    source: io::Error::new(io::ErrorKind::InvalidData, source),
                }
            })?;

        let stream = output
            .streams
            .iter()
            .find(|stream| stream.index == input.stream_index)
            .ok_or(Error::SubtitleTrackMissing {
                stream_index: input.stream_index,
            })?;
        if stream.codec_type.as_deref() != Some("subtitle") {
            return Err(Error::SubtitleTrackMissing {
                stream_index: input.stream_index,
            });
        }

        subtitle_packet_timings(input.stream_index, &output.packets)
    }
}

impl ImageSubtitleExtraction for FfmpegImageSubtitleExtractor {
    fn extract_image_subtitle_track<'a>(
        &'a self,
        input: ImageSubtitleExtractionInput,
    ) -> ImageSubtitleExtractionFuture<'a> {
        Box::pin(async move { self.extract(input).await })
    }
}

/// Return the default subtitle staging directory for a library root.
pub fn default_subtitle_staging_dir(library_root: &Path) -> PathBuf {
    library_root.join(".kino").join("subtitles")
}

/// Return the deterministic staging directory for one source hash and stream.
pub fn image_subtitle_track_output_dir(
    staging_dir: &Path,
    input_file_hash: &str,
    stream_index: u32,
) -> PathBuf {
    staging_dir.join(image_subtitle_track_address(input_file_hash, stream_index))
}

fn image_subtitle_track_address(input_file_hash: &str, stream_index: u32) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"kino:image-subtitle-track:v1\0");
    hasher.update(input_file_hash.as_bytes());
    hasher.update(b"\0");
    hasher.update(stream_index.to_be_bytes());
    hex_lower(&hasher.finalize())
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(hex_digit(byte >> 4));
        output.push(hex_digit(byte & 0x0f));
    }
    output
}

fn hex_digit(value: u8) -> char {
    match value {
        0..=9 => char::from(b'0' + value),
        10..=15 => char::from(b'a' + value - 10),
        _ => '0',
    }
}

async fn reset_output_dir(path: &Path) -> Result<()> {
    if tokio::fs::try_exists(path)
        .await
        .map_err(|source| Error::Io {
            path: path.to_path_buf(),
            source,
        })?
    {
        tokio::fs::remove_dir_all(path)
            .await
            .map_err(|source| Error::Io {
                path: path.to_path_buf(),
                source,
            })?;
    }
    tokio::fs::create_dir_all(path)
        .await
        .map_err(|source| Error::Io {
            path: path.to_path_buf(),
            source,
        })
}

async fn image_paths(output_dir: &Path, stream_index: u32) -> Result<Vec<PathBuf>> {
    let mut entries = tokio::fs::read_dir(output_dir)
        .await
        .map_err(|source| Error::Io {
            path: output_dir.to_path_buf(),
            source,
        })?;
    let mut paths = Vec::new();
    while let Some(entry) = entries.next_entry().await.map_err(|source| Error::Io {
        path: output_dir.to_path_buf(),
        source,
    })? {
        let path = entry.path();
        let file_type = entry.file_type().await.map_err(|source| Error::Io {
            path: path.clone(),
            source,
        })?;
        if file_type.is_file() && path.extension().and_then(|value| value.to_str()) == Some("png") {
            paths.push(path);
        }
    }
    paths.sort();

    if paths.is_empty() {
        return Ok(paths);
    }

    for path in &paths {
        if path.file_name().and_then(|value| value.to_str()).is_none() {
            return Err(Error::SubtitleExtractionFailed {
                stream_index,
                source: io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("non-utf8 subtitle image path: {}", path.display()),
                ),
            });
        }
    }

    Ok(paths)
}

fn subtitle_packet_timings(
    stream_index: u32,
    packets: &[FfprobePacket],
) -> Result<Vec<SubtitlePacketTiming>> {
    let packets = packets
        .iter()
        .filter(|packet| packet.stream_index == stream_index)
        .map(|packet| {
            let start = required_seconds(stream_index, packet.pts_time.as_deref(), "pts_time")?;
            let duration = packet
                .duration_time
                .as_deref()
                .map(|duration| parse_seconds(stream_index, duration, "duration_time"))
                .transpose()?;
            Ok((start, duration))
        })
        .collect::<Result<Vec<_>>>()?;

    let mut timings = Vec::with_capacity(packets.len());
    for (index, (start, duration)) in packets.iter().enumerate() {
        let fallback_end = packets
            .get(index + 1)
            .map(|(next_start, _)| *next_start)
            .unwrap_or(*start);
        let end = duration
            .and_then(|duration| start.checked_add(duration))
            .unwrap_or(fallback_end);
        timings.push(SubtitlePacketTiming {
            start: *start,
            end: end.max(*start),
        });
    }

    Ok(timings)
}

fn required_seconds(
    stream_index: u32,
    value: Option<&str>,
    field: &'static str,
) -> Result<Duration> {
    let Some(value) = value else {
        return Err(subtitle_extraction_error(
            stream_index,
            format!("ffprobe packet is missing {field}"),
        ));
    };
    parse_seconds(stream_index, value, field)
}

fn parse_seconds(stream_index: u32, value: &str, field: &'static str) -> Result<Duration> {
    let seconds = value
        .parse::<f64>()
        .map_err(|source| Error::SubtitleExtractionFailed {
            stream_index,
            source: io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid ffprobe {field} {value}: {source}"),
            ),
        })?;
    if !seconds.is_finite() || seconds < 0.0 {
        return Err(subtitle_extraction_error(
            stream_index,
            format!("invalid ffprobe {field} {value}"),
        ));
    }

    Ok(Duration::from_secs_f64(seconds))
}

fn subtitle_extraction_error(stream_index: u32, message: impl Into<String>) -> Error {
    Error::SubtitleExtractionFailed {
        stream_index,
        source: io::Error::other(message.into()),
    }
}

fn process_failure(program: &Path, status: ExitStatus, stderr: &[u8]) -> String {
    let status = status
        .code()
        .map_or_else(|| status.to_string(), |code| format!("exit code {code}"));
    let stderr = String::from_utf8_lossy(stderr).trim().to_owned();
    format!("{} failed with {status}: {stderr}", program.display())
}

#[derive(Debug, Deserialize)]
struct FfprobePacketOutput {
    #[serde(default)]
    streams: Vec<FfprobeStream>,
    #[serde(default)]
    packets: Vec<FfprobePacket>,
}

#[derive(Debug, Deserialize)]
struct FfprobeStream {
    index: u32,
    codec_type: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FfprobePacket {
    stream_index: u32,
    pts_time: Option<String>,
    duration_time: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SubtitlePacketTiming {
    start: Duration,
    end: Duration,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_directory_is_content_addressed_by_input_hash_and_stream() {
        let root = Path::new("/tmp/kino-subtitles");
        let first = image_subtitle_track_output_dir(root, "source-sha256", 3);
        let again = image_subtitle_track_output_dir(root, "source-sha256", 3);
        let different_stream = image_subtitle_track_output_dir(root, "source-sha256", 4);
        let different_hash = image_subtitle_track_output_dir(root, "other-source", 3);

        assert_eq!(first, again);
        assert_ne!(first, different_stream);
        assert_ne!(first, different_hash);
        assert_eq!(first.parent(), Some(root));
        assert_eq!(
            first
                .file_name()
                .and_then(|value| value.to_str())
                .map(str::len),
            Some(64)
        );
    }

    #[test]
    fn packet_timings_use_duration_when_present()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let timings = subtitle_packet_timings(
            2,
            &[
                FfprobePacket {
                    stream_index: 2,
                    pts_time: Some("1.500000".to_owned()),
                    duration_time: Some("2.250000".to_owned()),
                },
                FfprobePacket {
                    stream_index: 1,
                    pts_time: Some("3.000000".to_owned()),
                    duration_time: Some("1.000000".to_owned()),
                },
            ],
        )?;

        assert_eq!(
            timings,
            vec![SubtitlePacketTiming {
                start: Duration::from_millis(1500),
                end: Duration::from_millis(3750),
            }]
        );
        Ok(())
    }

    #[tokio::test]
    async fn trait_accepts_synthetic_image_frames()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let extractor = FakeImageSubtitleExtractor {
            frames: vec![
                ImageSubtitleFrame::new(
                    Duration::from_millis(100),
                    Duration::from_millis(900),
                    "frame-1.png",
                ),
                ImageSubtitleFrame::new(
                    Duration::from_millis(1200),
                    Duration::from_millis(1600),
                    "frame-2.png",
                ),
            ],
        };

        let frames = extractor
            .extract_image_subtitle_track(ImageSubtitleExtractionInput::new(
                "movie.mkv",
                "source-sha256",
                4,
                ProbeSubtitleKind::ImagePgs,
            ))
            .await?;

        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].start, Duration::from_millis(100));
        assert_eq!(frames[1].image_path, PathBuf::from("frame-2.png"));
        Ok(())
    }

    struct FakeImageSubtitleExtractor {
        frames: Vec<ImageSubtitleFrame>,
    }

    impl ImageSubtitleExtraction for FakeImageSubtitleExtractor {
        fn extract_image_subtitle_track<'a>(
            &'a self,
            _input: ImageSubtitleExtractionInput,
        ) -> ImageSubtitleExtractionFuture<'a> {
            Box::pin(async move { Ok(self.frames.clone()) })
        }
    }
}
