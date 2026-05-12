//! Source-file probing through ffprobe.

use std::{
    collections::HashMap,
    io,
    path::{Path, PathBuf},
    process::ExitStatus,
    time::Duration,
};

use serde::Deserialize;
use thiserror::Error;
use tokio::process::Command;

/// Default ffprobe executable resolved from the process path.
pub const DEFAULT_FFPROBE_PROGRAM: &str = "ffprobe";

/// File probe backed by the `ffprobe` command-line tool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FfprobeFileProbe {
    program: PathBuf,
}

impl FfprobeFileProbe {
    /// Construct a file probe using `ffprobe` from `PATH`.
    pub fn new() -> Self {
        Self {
            program: PathBuf::from(DEFAULT_FFPROBE_PROGRAM),
        }
    }

    /// Construct a file probe with an explicit ffprobe-compatible executable.
    pub fn with_program(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
        }
    }

    /// Return the configured ffprobe-compatible executable.
    pub fn program(&self) -> &Path {
        &self.program
    }

    /// Probe a source media file and return typed container facts.
    pub async fn probe(&self, path: impl AsRef<Path>) -> Result<ProbeResult, ProbeError> {
        let path = path.as_ref();
        let metadata = tokio::fs::metadata(path)
            .await
            .map_err(|source| ProbeError::Metadata {
                path: path.to_path_buf(),
                source,
            })?;
        if !metadata.is_file() {
            return Err(ProbeError::SourceNotFile {
                path: path.to_path_buf(),
            });
        }

        tokio::fs::File::open(path)
            .await
            .map_err(|source| ProbeError::OpenSource {
                path: path.to_path_buf(),
                source,
            })?;

        let mut command = Command::new(&self.program);
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            command.current_dir(parent);
        }
        let output = command
            .arg("-v")
            .arg("error")
            .arg("-print_format")
            .arg("json")
            .arg("-show_format")
            .arg("-show_streams")
            .arg(path)
            .output()
            .await
            .map_err(|source| ProbeError::Spawn {
                program: self.program.clone(),
                source,
            })?;

        if !output.status.success() {
            return Err(ProbeError::Failed {
                program: self.program.clone(),
                path: path.to_path_buf(),
                status: status_string(output.status),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            });
        }

        let raw: FfprobeOutput =
            serde_json::from_slice(&output.stdout).map_err(|source| ProbeError::InvalidJson {
                program: self.program.clone(),
                path: path.to_path_buf(),
                source,
            })?;
        ProbeResult::from_ffprobe_output(path.to_path_buf(), raw)
    }
}

impl Default for FfprobeFileProbe {
    fn default() -> Self {
        Self::new()
    }
}

/// Errors produced while probing a source file.
#[derive(Debug, Error)]
pub enum ProbeError {
    /// Source metadata could not be read.
    #[error("reading source metadata {path}: {source}")]
    Metadata {
        /// Source path.
        path: PathBuf,
        /// Underlying filesystem error.
        #[source]
        source: io::Error,
    },

    /// The probe target exists but is not a regular file.
    #[error("probe source is not a file: {path}", path = .path.display())]
    SourceNotFile {
        /// Source path.
        path: PathBuf,
    },

    /// The source file could not be opened for reading.
    #[error("opening probe source {path}: {source}", path = .path.display())]
    OpenSource {
        /// Source path.
        path: PathBuf,
        /// Underlying filesystem error.
        #[source]
        source: io::Error,
    },

    /// The ffprobe-compatible executable could not be launched.
    #[error("spawning ffprobe program {program}: {source}", program = .program.display())]
    Spawn {
        /// ffprobe-compatible executable path.
        program: PathBuf,
        /// Underlying process-spawn error.
        #[source]
        source: io::Error,
    },

    /// ffprobe rejected the file.
    #[error(
        "ffprobe program {program} failed for {path} with {status}: {stderr}",
        program = .program.display(),
        path = .path.display()
    )]
    Failed {
        /// ffprobe-compatible executable path.
        program: PathBuf,
        /// Source path.
        path: PathBuf,
        /// Process exit status.
        status: String,
        /// Standard error emitted by ffprobe.
        stderr: String,
    },

    /// ffprobe emitted JSON that did not match the expected shape.
    #[error(
        "ffprobe program {program} emitted invalid json for {path}: {source}",
        program = .program.display(),
        path = .path.display()
    )]
    InvalidJson {
        /// ffprobe-compatible executable path.
        program: PathBuf,
        /// Source path.
        path: PathBuf,
        /// JSON parse error.
        #[source]
        source: serde_json::Error,
    },

    /// ffprobe emitted an invalid duration value.
    #[error("ffprobe duration for {path} is invalid: {value}")]
    InvalidDuration {
        /// Source path.
        path: PathBuf,
        /// Invalid duration value.
        value: String,
    },

    /// ffprobe emitted an invalid mastering display metadata rational.
    #[error(
        "ffprobe mastering display value is invalid: {value}; expected denominator {expected_denominator}"
    )]
    InvalidMasterDisplayValue {
        /// Invalid rational value.
        value: String,
        /// Expected denominator for the ffprobe field.
        expected_denominator: u32,
    },

    /// ffprobe emitted incomplete content light level metadata.
    #[error("ffprobe content light level metadata is missing {field}")]
    InvalidContentLightLevelValue {
        /// Missing field name.
        field: &'static str,
    },

    /// ffprobe emitted incomplete or invalid Dolby Vision metadata.
    #[error("ffprobe dolby vision metadata field {field} is invalid")]
    InvalidDolbyVisionValue {
        /// Invalid field name.
        field: &'static str,
    },
}

/// Typed result produced by probing a media file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeResult {
    /// Source file that was probed.
    pub source_path: PathBuf,
    /// Container format reported by ffprobe.
    pub container: Option<ProbeContainer>,
    /// Best-effort title discovered from container metadata.
    pub title: Option<String>,
    /// Container duration, when reported.
    pub duration: Option<Duration>,
    /// Video streams embedded in the source file.
    pub video_streams: Vec<ProbeVideoStream>,
    /// Audio streams embedded in the source file.
    pub audio_streams: Vec<ProbeAudioStream>,
    /// Subtitle streams embedded in the source file.
    pub subtitle_streams: Vec<ProbeSubtitleStream>,
}

impl ProbeResult {
    fn from_ffprobe_output(
        source_path: PathBuf,
        output: FfprobeOutput,
    ) -> Result<Self, ProbeError> {
        let container = output.format.as_ref().and_then(ProbeContainer::from_format);
        let title = output
            .format
            .as_ref()
            .and_then(|format| tag_value(format.tags.as_ref(), "title"));
        let duration = output
            .format
            .as_ref()
            .and_then(|format| format.duration.as_ref())
            .map(|duration| parse_duration(&source_path, duration))
            .transpose()?;

        let mut video_streams = Vec::new();
        let mut audio_streams = Vec::new();
        let mut subtitle_streams = Vec::new();

        for stream in output.streams {
            match stream.codec_type.as_deref() {
                Some("video") => video_streams.push(ProbeVideoStream::from_stream(stream)?),
                Some("audio") => audio_streams.push(ProbeAudioStream::from_stream(stream)),
                Some("subtitle") => subtitle_streams.push(ProbeSubtitleStream::from_stream(stream)),
                _ => {}
            }
        }

        Ok(Self {
            source_path,
            container,
            title,
            duration,
            video_streams,
            audio_streams,
            subtitle_streams,
        })
    }
}

#[cfg(test)]
impl ProbeResult {
    fn as_probed_file(&self) -> ProbedFile {
        let mut probed = ProbedFile::new();
        probed.title = self.title.clone();
        probed.duration_seconds = self
            .duration
            .and_then(|duration| u32::try_from(duration.as_secs()).ok());
        probed.audio_languages = self
            .audio_streams
            .iter()
            .filter_map(|stream| stream.language.clone())
            .collect();
        probed.subtitle_languages = self
            .subtitle_streams
            .iter()
            .filter_map(|stream| stream.language.clone())
            .collect();
        probed
    }
}

#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct ProbedFile {
    title: Option<String>,
    duration_seconds: Option<u32>,
    audio_languages: Vec<String>,
    subtitle_languages: Vec<String>,
}

#[cfg(test)]
impl ProbedFile {
    fn new() -> Self {
        Self::default()
    }

    fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    const fn with_duration_seconds(mut self, duration_seconds: u32) -> Self {
        self.duration_seconds = Some(duration_seconds);
        self
    }

    fn with_audio_languages<I, S>(mut self, languages: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.audio_languages = languages.into_iter().map(Into::into).collect();
        self
    }

    fn with_subtitle_languages<I, S>(mut self, languages: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.subtitle_languages = languages.into_iter().map(Into::into).collect();
        self
    }
}

/// Container format reported for a probed file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeContainer {
    /// Short format names reported by ffprobe, split on commas.
    pub format_names: Vec<String>,
    /// Human-readable container name.
    pub format_long_name: Option<String>,
}

impl ProbeContainer {
    fn from_format(format: &FfprobeFormat) -> Option<Self> {
        let format_names = format
            .format_name
            .as_deref()
            .into_iter()
            .flat_map(|names| names.split(','))
            .filter_map(non_empty_string)
            .collect::<Vec<_>>();
        if format_names.is_empty() && empty_or_none(format.format_long_name.as_deref()).is_none() {
            return None;
        }

        Some(Self {
            format_names,
            format_long_name: empty_or_none(format.format_long_name.as_deref()).map(str::to_owned),
        })
    }
}

/// Video stream discovered in a source file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeVideoStream {
    /// ffprobe stream index.
    pub index: u32,
    /// Codec short name, for example `h264`.
    pub codec_name: Option<String>,
    /// Human-readable codec name.
    pub codec_long_name: Option<String>,
    /// Pixel width, when reported.
    pub width: Option<u32>,
    /// Pixel height, when reported.
    pub height: Option<u32>,
    /// Language tag reported for the stream.
    pub language: Option<String>,
    /// Color primaries reported by ffprobe, for example `bt2020`.
    pub color_primaries: Option<String>,
    /// Color transfer characteristic reported by ffprobe, for example `smpte2084`.
    pub color_transfer: Option<String>,
    /// Color space reported by ffprobe, for example `bt2020nc`.
    pub color_space: Option<String>,
    /// SMPTE ST 2086 mastering display metadata.
    pub master_display: Option<MasterDisplay>,
    /// CTA-861.3 content light level metadata.
    pub max_cll: Option<MaxCll>,
    /// Dolby Vision configuration metadata.
    pub dolby_vision: Option<DolbyVision>,
}

impl ProbeVideoStream {
    fn from_stream(stream: FfprobeStream) -> Result<Self, ProbeError> {
        let master_display = stream
            .side_data_list
            .iter()
            .find(|side_data| {
                side_data.side_data_type.as_deref() == Some("Mastering display metadata")
            })
            .map(MasterDisplay::from_side_data)
            .transpose()?;
        let max_cll = stream
            .side_data_list
            .iter()
            .find(|side_data| {
                side_data.side_data_type.as_deref() == Some("Content light level metadata")
            })
            .map(MaxCll::from_side_data)
            .transpose()?;
        let dolby_vision = stream
            .side_data_list
            .iter()
            .find(|side_data| {
                side_data.side_data_type.as_deref() == Some("DOVI configuration record")
            })
            .map(DolbyVision::from_side_data)
            .transpose()?;

        Ok(Self {
            index: stream.index,
            codec_name: stream.codec_name.and_then(non_empty_owned),
            codec_long_name: stream.codec_long_name.and_then(non_empty_owned),
            width: stream.width,
            height: stream.height,
            language: tag_value(stream.tags.as_ref(), "language"),
            color_primaries: stream.color_primaries.and_then(non_empty_owned),
            color_transfer: stream.color_transfer.and_then(non_empty_owned),
            color_space: stream.color_space.and_then(non_empty_owned),
            master_display,
            max_cll,
            dolby_vision,
        })
    }
}

/// SMPTE ST 2086 mastering display metadata from ffprobe side data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MasterDisplay {
    /// Red primary x chromaticity numerator.
    pub red_x: u32,
    /// Red primary y chromaticity numerator.
    pub red_y: u32,
    /// Green primary x chromaticity numerator.
    pub green_x: u32,
    /// Green primary y chromaticity numerator.
    pub green_y: u32,
    /// Blue primary x chromaticity numerator.
    pub blue_x: u32,
    /// Blue primary y chromaticity numerator.
    pub blue_y: u32,
    /// White point x chromaticity numerator.
    pub white_x: u32,
    /// White point y chromaticity numerator.
    pub white_y: u32,
    /// Minimum mastering display luminance numerator.
    pub min_luminance: u32,
    /// Maximum mastering display luminance numerator.
    pub max_luminance: u32,
}

impl MasterDisplay {
    fn from_side_data(side_data: &FfprobeSideData) -> Result<Self, ProbeError> {
        Ok(Self {
            red_x: parse_master_display_numerator(side_data.red_x.as_deref(), "red_x", 50_000)?,
            red_y: parse_master_display_numerator(side_data.red_y.as_deref(), "red_y", 50_000)?,
            green_x: parse_master_display_numerator(
                side_data.green_x.as_deref(),
                "green_x",
                50_000,
            )?,
            green_y: parse_master_display_numerator(
                side_data.green_y.as_deref(),
                "green_y",
                50_000,
            )?,
            blue_x: parse_master_display_numerator(side_data.blue_x.as_deref(), "blue_x", 50_000)?,
            blue_y: parse_master_display_numerator(side_data.blue_y.as_deref(), "blue_y", 50_000)?,
            white_x: parse_master_display_numerator(
                side_data.white_x.as_deref(),
                "white_x",
                50_000,
            )?,
            white_y: parse_master_display_numerator(
                side_data.white_y.as_deref(),
                "white_y",
                50_000,
            )?,
            min_luminance: parse_master_display_numerator(
                side_data.min_luminance.as_deref(),
                "min_luminance",
                10_000,
            )?,
            max_luminance: parse_master_display_numerator(
                side_data.max_luminance.as_deref(),
                "max_luminance",
                10_000,
            )?,
        })
    }
}

/// CTA-861.3 content light level metadata from ffprobe side data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaxCll {
    /// Maximum content light level.
    pub max_content: u32,
    /// Maximum frame-average light level.
    pub max_average: u32,
}

impl MaxCll {
    fn from_side_data(side_data: &FfprobeSideData) -> Result<Self, ProbeError> {
        Ok(Self {
            max_content: required_u32(side_data.max_content, "max_content")?,
            max_average: required_u32(side_data.max_average, "max_average")?,
        })
    }
}

/// Dolby Vision configuration metadata from ffprobe side data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DolbyVision {
    /// Dolby Vision profile number.
    pub profile: u8,
    /// Dolby Vision level number.
    pub level: u8,
    /// Whether the reference processing unit is present.
    pub rpu_present: bool,
    /// Whether the enhancement layer is present.
    pub el_present: bool,
    /// Whether the base layer is present.
    pub bl_present: bool,
}

impl DolbyVision {
    fn from_side_data(side_data: &FfprobeSideData) -> Result<Self, ProbeError> {
        Ok(Self {
            profile: required_u8(side_data.dv_profile, "dv_profile")?,
            level: required_u8(side_data.dv_level, "dv_level")?,
            rpu_present: required_flag(side_data.rpu_present_flag, "rpu_present_flag")?,
            el_present: required_flag(side_data.el_present_flag, "el_present_flag")?,
            bl_present: required_flag(side_data.bl_present_flag, "bl_present_flag")?,
        })
    }
}

/// Audio stream discovered in a source file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeAudioStream {
    /// ffprobe stream index.
    pub index: u32,
    /// Codec short name, for example `aac`.
    pub codec_name: Option<String>,
    /// Human-readable codec name.
    pub codec_long_name: Option<String>,
    /// Audio channel count, when reported.
    pub channels: Option<u32>,
    /// Language tag reported for the stream.
    pub language: Option<String>,
}

impl ProbeAudioStream {
    fn from_stream(stream: FfprobeStream) -> Self {
        Self {
            index: stream.index,
            codec_name: stream.codec_name.and_then(non_empty_owned),
            codec_long_name: stream.codec_long_name.and_then(non_empty_owned),
            channels: stream.channels,
            language: tag_value(stream.tags.as_ref(), "language"),
        }
    }
}

/// Subtitle stream discovered in a source file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeSubtitleStream {
    /// ffprobe stream index.
    pub index: u32,
    /// Codec short name, for example `subrip`.
    pub codec_name: Option<String>,
    /// Human-readable codec name.
    pub codec_long_name: Option<String>,
    /// Text or image classification for supported subtitle codecs.
    pub kind: ProbeSubtitleKind,
    /// Language tag reported for the stream.
    pub language: Option<String>,
}

impl ProbeSubtitleStream {
    fn from_stream(stream: FfprobeStream) -> Self {
        let codec_name = stream.codec_name.and_then(non_empty_owned);
        let kind = ProbeSubtitleKind::from_codec(codec_name.as_deref());
        Self {
            index: stream.index,
            codec_name,
            codec_long_name: stream.codec_long_name.and_then(non_empty_owned),
            kind,
            language: tag_value(stream.tags.as_ref(), "language"),
        }
    }
}

/// Subtitle codec classification used by downstream extraction policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeSubtitleKind {
    /// SubRip text subtitles.
    Srt,
    /// Advanced SubStation Alpha text subtitles.
    Ass,
    /// Presentation Graphic Stream image subtitles.
    ImagePgs,
    /// DVD VOBSUB image subtitles.
    ImageVobSub,
    /// DVB image subtitles.
    ImageDvb,
    /// Subtitle codec is not yet classified by Kino.
    Other,
}

impl ProbeSubtitleKind {
    /// Whether this subtitle stream is a text format.
    pub const fn is_text(self) -> bool {
        matches!(self, Self::Srt | Self::Ass)
    }

    /// Whether this subtitle stream is an image format.
    pub const fn is_image(self) -> bool {
        matches!(self, Self::ImagePgs | Self::ImageVobSub | Self::ImageDvb)
    }

    fn from_codec(codec_name: Option<&str>) -> Self {
        match codec_name {
            Some("subrip" | "srt") => Self::Srt,
            Some("ass" | "ssa") => Self::Ass,
            Some("hdmv_pgs_subtitle") => Self::ImagePgs,
            Some("dvd_subtitle") => Self::ImageVobSub,
            Some("dvb_subtitle") => Self::ImageDvb,
            _ => Self::Other,
        }
    }
}

#[derive(Debug, Deserialize)]
struct FfprobeOutput {
    #[serde(default)]
    streams: Vec<FfprobeStream>,
    format: Option<FfprobeFormat>,
}

#[derive(Debug, Deserialize)]
struct FfprobeFormat {
    format_name: Option<String>,
    format_long_name: Option<String>,
    duration: Option<String>,
    tags: Option<HashMap<String, String>>,
}

#[derive(Debug, Deserialize)]
struct FfprobeStream {
    index: u32,
    codec_type: Option<String>,
    codec_name: Option<String>,
    codec_long_name: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    channels: Option<u32>,
    color_primaries: Option<String>,
    color_transfer: Option<String>,
    color_space: Option<String>,
    #[serde(default)]
    side_data_list: Vec<FfprobeSideData>,
    tags: Option<HashMap<String, String>>,
}

#[derive(Debug, Deserialize)]
struct FfprobeSideData {
    side_data_type: Option<String>,
    red_x: Option<String>,
    red_y: Option<String>,
    green_x: Option<String>,
    green_y: Option<String>,
    blue_x: Option<String>,
    blue_y: Option<String>,
    white_x: Option<String>,
    white_y: Option<String>,
    min_luminance: Option<String>,
    max_luminance: Option<String>,
    max_content: Option<u32>,
    max_average: Option<u32>,
    dv_profile: Option<u8>,
    dv_level: Option<u8>,
    rpu_present_flag: Option<u8>,
    el_present_flag: Option<u8>,
    bl_present_flag: Option<u8>,
}

fn status_string(status: ExitStatus) -> String {
    status
        .code()
        .map_or_else(|| status.to_string(), |code| format!("exit code {code}"))
}

fn parse_duration(path: &Path, value: &str) -> Result<Duration, ProbeError> {
    let seconds = value
        .parse::<f64>()
        .map_err(|_| ProbeError::InvalidDuration {
            path: path.to_path_buf(),
            value: value.to_owned(),
        })?;
    if !seconds.is_finite() || seconds < 0.0 {
        return Err(ProbeError::InvalidDuration {
            path: path.to_path_buf(),
            value: value.to_owned(),
        });
    }

    Ok(Duration::from_secs_f64(seconds))
}

fn parse_numerator(value: &str, expected_denominator: u32) -> Result<u32, ProbeError> {
    let (numerator, denominator) =
        value
            .split_once('/')
            .ok_or_else(|| ProbeError::InvalidMasterDisplayValue {
                value: value.to_owned(),
                expected_denominator,
            })?;
    let numerator =
        numerator
            .parse::<u32>()
            .map_err(|_| ProbeError::InvalidMasterDisplayValue {
                value: value.to_owned(),
                expected_denominator,
            })?;
    let denominator =
        denominator
            .parse::<u32>()
            .map_err(|_| ProbeError::InvalidMasterDisplayValue {
                value: value.to_owned(),
                expected_denominator,
            })?;
    if denominator != expected_denominator {
        return Err(ProbeError::InvalidMasterDisplayValue {
            value: value.to_owned(),
            expected_denominator,
        });
    }

    Ok(numerator)
}

fn parse_master_display_numerator(
    value: Option<&str>,
    field: &'static str,
    expected_denominator: u32,
) -> Result<u32, ProbeError> {
    let value = value.ok_or_else(|| ProbeError::InvalidMasterDisplayValue {
        value: format!("<missing {field}>"),
        expected_denominator,
    })?;
    parse_numerator(value, expected_denominator)
}

fn required_u32(value: Option<u32>, field: &'static str) -> Result<u32, ProbeError> {
    value.ok_or(ProbeError::InvalidContentLightLevelValue { field })
}

fn required_u8(value: Option<u8>, field: &'static str) -> Result<u8, ProbeError> {
    value.ok_or(ProbeError::InvalidDolbyVisionValue { field })
}

fn required_flag(value: Option<u8>, field: &'static str) -> Result<bool, ProbeError> {
    match required_u8(value, field)? {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(ProbeError::InvalidDolbyVisionValue { field }),
    }
}

fn tag_value(tags: Option<&HashMap<String, String>>, key: &str) -> Option<String> {
    tags.and_then(|tags| {
        tags.iter()
            .find(|(tag_key, _)| tag_key.eq_ignore_ascii_case(key))
            .and_then(|(_, value)| non_empty_string(value))
    })
}

fn empty_or_none(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn non_empty_string(value: impl AsRef<str>) -> Option<String> {
    let value = value.as_ref().trim();
    (!value.is_empty()).then(|| value.to_owned())
}

fn non_empty_owned(value: String) -> Option<String> {
    non_empty_string(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[tokio::test]
    async fn representative_mkv_probe_produces_complete_result()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let media_path = temp.path().join("sample.mkv");
        tokio::fs::write(&media_path, b"representative mkv bytes").await?;
        let ffprobe = write_script(
            temp.path().join("ffprobe-ok"),
            r#"#!/bin/sh
cat <<'JSON'
{
  "streams": [
    {
      "index": 0,
      "codec_type": "video",
      "codec_name": "h264",
      "codec_long_name": "H.264 / AVC / MPEG-4 AVC / MPEG-4 part 10",
      "width": 1920,
      "height": 1080
    },
    {
      "index": 1,
      "codec_type": "audio",
      "codec_name": "truehd",
      "codec_long_name": "TrueHD",
      "channels": 8,
      "tags": {
        "language": "eng"
      }
    },
    {
      "index": 2,
      "codec_type": "audio",
      "codec_name": "aac",
      "channels": 2,
      "tags": {
        "LANGUAGE": "jpn"
      }
    },
    {
      "index": 3,
      "codec_type": "subtitle",
      "codec_name": "subrip",
      "tags": {
        "language": "spa"
      }
    },
    {
      "index": 4,
      "codec_type": "subtitle",
      "codec_name": "ass",
      "tags": {
        "language": "eng"
      }
    },
    {
      "index": 5,
      "codec_type": "subtitle",
      "codec_name": "hdmv_pgs_subtitle",
      "tags": {
        "language": "jpn"
      }
    }
  ],
  "format": {
    "format_name": "matroska,webm",
    "format_long_name": "Matroska / WebM",
    "duration": "8160.42",
    "tags": {
      "title": "The Matrix"
    }
  }
}
JSON
"#,
        )?;
        let probe = FfprobeFileProbe::with_program(ffprobe);

        let result = probe.probe(&media_path).await?;

        assert_eq!(result.source_path, media_path);
        assert_eq!(
            result.container,
            Some(ProbeContainer {
                format_names: vec![String::from("matroska"), String::from("webm")],
                format_long_name: Some(String::from("Matroska / WebM")),
            })
        );
        assert_eq!(result.title, Some(String::from("The Matrix")));
        assert_eq!(result.duration, Some(Duration::from_millis(8_160_420)));
        assert_eq!(result.video_streams.len(), 1);
        assert_eq!(
            result.video_streams[0].codec_name,
            Some(String::from("h264"))
        );
        assert_eq!(result.video_streams[0].width, Some(1920));
        assert_eq!(result.video_streams[0].height, Some(1080));
        assert_eq!(
            result
                .audio_streams
                .iter()
                .filter_map(|stream| stream.language.as_deref())
                .collect::<Vec<_>>(),
            vec!["eng", "jpn"]
        );
        assert_eq!(
            result
                .subtitle_streams
                .iter()
                .map(|stream| stream.kind)
                .collect::<Vec<_>>(),
            vec![
                ProbeSubtitleKind::Srt,
                ProbeSubtitleKind::Ass,
                ProbeSubtitleKind::ImagePgs,
            ]
        );
        assert!(result.subtitle_streams[0].kind.is_text());
        assert!(result.subtitle_streams[2].kind.is_image());
        assert_eq!(
            result.as_probed_file(),
            ProbedFile::new()
                .with_title("The Matrix")
                .with_duration_seconds(8160)
                .with_audio_languages(["eng", "jpn"])
                .with_subtitle_languages(["spa", "eng", "jpn"])
        );

        Ok(())
    }

    #[test]
    fn image_subtitle_codecs_keep_source_format() {
        let result = ProbeResult::from_ffprobe_output(
            PathBuf::from("movie.mkv"),
            FfprobeOutput {
                streams: vec![
                    subtitle_stream(1, "hdmv_pgs_subtitle"),
                    subtitle_stream(2, "dvd_subtitle"),
                    subtitle_stream(3, "dvb_subtitle"),
                ],
                format: None,
            },
        )
        .unwrap();

        assert_eq!(
            result
                .subtitle_streams
                .iter()
                .map(|stream| stream.kind)
                .collect::<Vec<_>>(),
            vec![
                ProbeSubtitleKind::ImagePgs,
                ProbeSubtitleKind::ImageVobSub,
                ProbeSubtitleKind::ImageDvb,
            ]
        );
        assert!(
            result
                .subtitle_streams
                .iter()
                .all(|stream| stream.kind.is_image())
        );
    }

    #[tokio::test]
    async fn corrupted_file_probe_returns_typed_failure()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let media_path = temp.path().join("corrupted.mkv");
        tokio::fs::write(&media_path, b"not a media container").await?;
        let ffprobe = write_script(
            temp.path().join("ffprobe-fail"),
            r#"#!/bin/sh
echo "Invalid data found when processing input" >&2
exit 66
"#,
        )?;
        let probe = FfprobeFileProbe::with_program(ffprobe.clone());

        let error = probe.probe(&media_path).await.err();

        let Some(ProbeError::Failed {
            program,
            path,
            status,
            stderr,
        }) = error
        else {
            panic!("expected ffprobe failure");
        };
        assert_eq!(program, ffprobe);
        assert_eq!(path, media_path);
        assert_eq!(status, "exit code 66");
        assert_eq!(stderr, "Invalid data found when processing input");

        Ok(())
    }

    #[tokio::test]
    async fn missing_file_probe_returns_typed_error()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let media_path = temp.path().join("missing.mkv");
        let probe = FfprobeFileProbe::with_program(temp.path().join("ffprobe-unused"));

        let error = probe.probe(&media_path).await.err();

        assert!(matches!(error, Some(ProbeError::Metadata { .. })));

        Ok(())
    }

    fn write_script(path: PathBuf, body: &str) -> std::result::Result<PathBuf, io::Error> {
        fs::write(&path, body)?;
        let mut permissions = fs::metadata(&path)?.permissions();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            permissions.set_mode(0o755);
        }
        fs::set_permissions(&path, permissions)?;
        Ok(path)
    }

    fn subtitle_stream(index: u32, codec_name: &str) -> FfprobeStream {
        FfprobeStream {
            index,
            codec_type: Some(String::from("subtitle")),
            codec_name: Some(String::from(codec_name)),
            codec_long_name: None,
            width: None,
            height: None,
            channels: None,
            color_primaries: None,
            color_transfer: None,
            color_space: None,
            side_data_list: Vec::new(),
            tags: None,
        }
    }

    #[test]
    fn sdr_probe_json_has_no_hdr_metadata() {
        let result = probe_result_from_json(
            r#"
{
  "streams": [
    {
      "index": 0,
      "codec_type": "video",
      "codec_name": "h264",
      "width": 1920,
      "height": 1080,
      "color_primaries": "bt709",
      "color_transfer": "bt709",
      "color_space": "bt709"
    }
  ]
}
"#,
        );

        let video = result.video_streams.first().unwrap();
        assert_eq!(video.color_primaries.as_deref(), Some("bt709"));
        assert_eq!(video.color_transfer.as_deref(), Some("bt709"));
        assert_eq!(video.color_space.as_deref(), Some("bt709"));
        assert_eq!(video.master_display, None);
        assert_eq!(video.max_cll, None);
        assert_eq!(video.dolby_vision, None);
    }

    #[test]
    fn hdr10_probe_json_populates_master_display_and_max_cll() {
        let result = probe_result_from_json(
            r#"
{
  "streams": [
    {
      "index": 0,
      "codec_type": "video",
      "codec_name": "hevc",
      "width": 3840,
      "height": 2160,
      "color_primaries": "bt2020",
      "color_transfer": "smpte2084",
      "color_space": "bt2020nc",
      "side_data_list": [
        {
          "side_data_type": "Mastering display metadata",
          "red_x": "13250/50000",
          "red_y": "34500/50000",
          "green_x": "7500/50000",
          "green_y": "3000/50000",
          "blue_x": "34000/50000",
          "blue_y": "16000/50000",
          "white_x": "15635/50000",
          "white_y": "16450/50000",
          "min_luminance": "50/10000",
          "max_luminance": "10000000/10000"
        },
        {
          "side_data_type": "Content light level metadata",
          "max_content": 1000,
          "max_average": 400
        }
      ]
    }
  ]
}
"#,
        );

        let video = result.video_streams.first().unwrap();
        assert_eq!(video.color_primaries.as_deref(), Some("bt2020"));
        assert_eq!(video.color_transfer.as_deref(), Some("smpte2084"));
        assert_eq!(video.color_space.as_deref(), Some("bt2020nc"));
        assert_eq!(
            video.master_display,
            Some(MasterDisplay {
                red_x: 13250,
                red_y: 34500,
                green_x: 7500,
                green_y: 3000,
                blue_x: 34000,
                blue_y: 16000,
                white_x: 15635,
                white_y: 16450,
                min_luminance: 50,
                max_luminance: 10_000_000,
            })
        );
        assert_eq!(
            video.max_cll,
            Some(MaxCll {
                max_content: 1000,
                max_average: 400,
            })
        );
        assert_eq!(video.dolby_vision, None);
    }

    #[test]
    fn dolby_vision_probe_json_populates_dovi_metadata() {
        let result = probe_result_from_json(
            r#"
{
  "streams": [
    {
      "index": 0,
      "codec_type": "video",
      "codec_name": "hevc",
      "width": 3840,
      "height": 2160,
      "color_primaries": "bt2020",
      "color_transfer": "smpte2084",
      "color_space": "bt2020nc",
      "side_data_list": [
        {
          "side_data_type": "Mastering display metadata",
          "red_x": "13250/50000",
          "red_y": "34500/50000",
          "green_x": "7500/50000",
          "green_y": "3000/50000",
          "blue_x": "34000/50000",
          "blue_y": "16000/50000",
          "white_x": "15635/50000",
          "white_y": "16450/50000",
          "min_luminance": "50/10000",
          "max_luminance": "10000000/10000"
        },
        {
          "side_data_type": "Content light level metadata",
          "max_content": 1000,
          "max_average": 400
        },
        {
          "side_data_type": "DOVI configuration record",
          "dv_version_major": 1,
          "dv_version_minor": 0,
          "dv_profile": 8,
          "dv_level": 9,
          "rpu_present_flag": 1,
          "el_present_flag": 0,
          "bl_present_flag": 1
        }
      ]
    }
  ]
}
"#,
        );

        let video = result.video_streams.first().unwrap();
        assert!(video.master_display.is_some());
        assert!(video.max_cll.is_some());
        assert_eq!(
            video.dolby_vision,
            Some(DolbyVision {
                profile: 8,
                level: 9,
                rpu_present: true,
                el_present: false,
                bl_present: true,
            })
        );
    }

    fn probe_result_from_json(json: &str) -> ProbeResult {
        let output = serde_json::from_str::<FfprobeOutput>(json).unwrap();
        ProbeResult::from_ffprobe_output(PathBuf::from("movie.mkv"), output).unwrap()
    }

    #[test]
    fn invalid_duration_is_typed_error() {
        let path = PathBuf::from("movie.mkv");
        let output = FfprobeOutput {
            streams: Vec::new(),
            format: Some(FfprobeFormat {
                format_name: Some(String::from("matroska")),
                format_long_name: None,
                duration: Some(String::from("nope")),
                tags: None,
            }),
        };

        let error = ProbeResult::from_ffprobe_output(path.clone(), output).err();

        assert!(matches!(
            error,
            Some(ProbeError::InvalidDuration {
                path: error_path,
                value
            }) if error_path == path && value == "nope"
        ));
    }
}
