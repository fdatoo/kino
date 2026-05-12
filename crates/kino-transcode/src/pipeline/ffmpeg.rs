//! Typed FFmpeg command builders for the transcode pipeline.

use std::{ffi::OsString, fmt, path::Path, path::PathBuf, time::Duration};

use kino_core::probe::{MasterDisplay, MaxCll};
use serde::Deserialize;
use tokio::process::Command;
use tokio::sync::oneshot;

use crate::{Error, Result, VideoCodec, pipeline::PipelineRunner};

/// Encoder speed/quality preset rendered as FFmpeg `-preset`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preset {
    /// Render `-preset ultrafast` for the fastest, lowest-compression encode.
    Ultrafast,
    /// Render `-preset superfast` for very fast, low-compression encodes.
    Superfast,
    /// Render `-preset veryfast` for fast compatibility encodes.
    Veryfast,
    /// Render `-preset faster` for faster-than-default encodes.
    Faster,
    /// Render `-preset fast` for modest speed bias over compression.
    Fast,
    /// Render `-preset medium`, FFmpeg encoder defaults for balanced output.
    Medium,
    /// Render `-preset slow` for higher-compression offline encodes.
    Slow,
    /// Render `-preset slower` for slower, higher-compression offline encodes.
    Slower,
    /// Render `-preset veryslow` for the slowest, highest-compression encode.
    Veryslow,
}

impl Preset {
    /// Return the value passed to FFmpeg after `-preset`.
    pub const fn as_ffmpeg(&self) -> &'static str {
        match self {
            Self::Ultrafast => "ultrafast",
            Self::Superfast => "superfast",
            Self::Veryfast => "veryfast",
            Self::Faster => "faster",
            Self::Fast => "fast",
            Self::Medium => "medium",
            Self::Slow => "slow",
            Self::Slower => "slower",
            Self::Veryslow => "veryslow",
        }
    }
}

/// Output color metadata policy rendered as FFmpeg color flags.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ColorOutput {
    /// Preserve input color flags by omitting explicit `-color_*` arguments.
    CopyFromInput,
    /// Force SDR BT.709 via `-color_primaries bt709 -color_trc bt709 -colorspace bt709`.
    SdrBt709,
    /// Preserve HDR10 via BT.2020/PQ flags and codec `master-display`/`max-cll` params.
    Hdr10 {
        /// SMPTE ST 2086 mastering display metadata for `master-display=...`.
        master_display: MasterDisplay,
        /// CTA-861.3 content light metadata for `max-cll=...`.
        max_cll: MaxCll,
    },
}

/// Render SMPTE ST 2086 metadata as the x265 `master-display` value.
fn render_master_display(md: &MasterDisplay) -> String {
    format!(
        "G({},{})B({},{})R({},{})WP({},{})L({},{})",
        md.green_x,
        md.green_y,
        md.blue_x,
        md.blue_y,
        md.red_x,
        md.red_y,
        md.white_x,
        md.white_y,
        md.max_luminance,
        md.min_luminance
    )
}

/// Render CTA-861.3 content light metadata as the x265 `max-cll` value.
fn render_max_cll(cll: &MaxCll) -> String {
    format!("{},{}", cll.max_content, cll.max_average)
}

/// One FFmpeg `-vf` filtergraph atom.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VideoFilter {
    /// Render `scale=width:height` to resize video to fixed dimensions.
    Scale(u32, u32),
    /// Render the HDR-to-SDR zscale/tonemap chain used for compatibility variants.
    HdrToSdrTonemap,
    /// Render `hwdownload,format=...` to move hardware frames into system memory.
    HwDownload {
        /// Software pixel format requested after `hwdownload`.
        format: String,
    },
    /// Render an FFmpeg `format=...` pixel-format conversion.
    Format(String),
}

impl VideoFilter {
    /// Return the FFmpeg filtergraph atom for this filter.
    pub fn as_ffmpeg(&self) -> String {
        match self {
            Self::Scale(width, height) => format!("scale={width}:{height}"),
            Self::HdrToSdrTonemap => concat!(
                "zscale=t=linear:npl=100,format=gbrpf32le,",
                "zscale=p=bt709,tonemap=tonemap=hable:desat=0,",
                "zscale=t=bt709:m=bt709:r=tv,format=yuv420p"
            )
            .to_owned(),
            Self::HwDownload { format } => format!("hwdownload,format={format}"),
            Self::Format(format) => format!("format={format}"),
        }
    }
}

/// Audio output policy rendered as FFmpeg audio mapping and codec flags.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AudioPolicy {
    /// Render first-audio-stream stereo AAC with `-c:a aac -ac 2 -b:a Nk`.
    StereoAac {
        /// AAC bitrate in kilobits per second.
        bitrate_kbps: u32,
    },
    /// Render first audio as stereo AAC and preserve additional mapped audio with copy.
    StereoAacWithSurroundPassthrough {
        /// AAC bitrate in kilobits per second for the stereo track.
        bitrate_kbps: u32,
    },
    /// Render `-c:a copy` for audio passthrough.
    Copy,
    /// Render `-an` to omit audio from the output.
    None,
}

/// HLS fMP4/CMAF output settings rendered as FFmpeg HLS muxer flags.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HlsOutputSpec {
    /// Directory receiving the playlist, init segment, and media segments.
    pub output_dir: PathBuf,
    /// Segment target duration rendered as `-hls_time`.
    pub segment_duration: Duration,
    /// Media segment filename pattern rendered as `-hls_segment_filename`.
    pub segment_filename: String,
    /// Initialization segment filename rendered as `-hls_fmp4_init_filename`.
    pub init_filename: String,
    /// Media playlist filename used as the final FFmpeg output path.
    pub playlist_filename: String,
}

impl HlsOutputSpec {
    /// Construct a CMAF VOD output with Kino's default segment, init, and playlist names.
    pub fn cmaf_vod(output_dir: impl Into<PathBuf>, segment_duration: Duration) -> Self {
        Self {
            output_dir: output_dir.into(),
            segment_duration,
            segment_filename: "seg-%05d.m4s".to_owned(),
            init_filename: "init.mp4".to_owned(),
            playlist_filename: "media.m3u8".to_owned(),
        }
    }
}

/// Input media settings rendered as FFmpeg input seek/duration flags and `-i`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputSpec {
    /// Source media path passed to FFmpeg after `-i`.
    pub path: PathBuf,
    /// Optional input seek start rendered as `-ss` in seconds.
    pub start_us: Option<u64>,
    /// Optional input duration rendered as `-t` in seconds.
    pub duration_us: Option<u64>,
}

impl InputSpec {
    /// Construct a file input for the given source path.
    pub fn file(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            start_us: None,
            duration_us: None,
        }
    }

    /// Render input-specific FFmpeg arguments.
    pub fn to_args(&self) -> Vec<OsString> {
        let mut args = Vec::new();
        if let Some(start_us) = self.start_us {
            push_arg(&mut args, "-ss");
            push_arg(&mut args, render_microseconds(start_us));
        }
        if let Some(duration_us) = self.duration_us {
            push_arg(&mut args, "-t");
            push_arg(&mut args, render_microseconds(duration_us));
        }
        push_arg(&mut args, "-i");
        args.push(self.path.as_os_str().to_owned());
        args
    }
}

/// Video output settings rendered as FFmpeg video codec, quality, pixel, and color flags.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoOutputSpec {
    /// Video codec target rendered as `-c:v`.
    pub codec: VideoCodec,
    /// Optional constant-rate-factor value rendered as `-crf`.
    pub crf: Option<u8>,
    /// Encoder preset rendered as `-preset`.
    pub preset: Preset,
    /// Output bit depth used to select `-pix_fmt`.
    pub bit_depth: u8,
    /// Output color metadata rendered through `-color_*` and codec HDR params.
    pub color: ColorOutput,
    /// Maximum planned output dimensions, kept as typed plan data for encoder call sites.
    pub max_resolution: Option<(u32, u32)>,
}

/// FFmpeg logging level rendered as `-loglevel`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    /// Render `-loglevel error`.
    Error,
    /// Render `-loglevel warning`.
    Warning,
    /// Render `-loglevel info`.
    Info,
}

impl LogLevel {
    /// Return the value passed to FFmpeg after `-loglevel`.
    pub const fn as_ffmpeg(&self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
            Self::Info => "info",
        }
    }
}

/// Typed FFmpeg encode invocation rendered to argv, `Command`, or shell form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FfmpegEncodeCommand {
    binary: PathBuf,
    input: InputSpec,
    video: VideoOutputSpec,
    audio: AudioPolicy,
    filters: Vec<VideoFilter>,
    hls: Option<HlsOutputSpec>,
    file_output: Option<PathBuf>,
    progress_pipe: bool,
    log_level: LogLevel,
}

impl FfmpegEncodeCommand {
    /// Construct an encode command with copy defaults and FFmpeg warning logs.
    pub fn new(binary: impl Into<PathBuf>, input: InputSpec) -> Self {
        Self {
            binary: binary.into(),
            input,
            video: VideoOutputSpec {
                codec: VideoCodec::Copy,
                crf: None,
                preset: Preset::Medium,
                bit_depth: 8,
                color: ColorOutput::CopyFromInput,
                max_resolution: None,
            },
            audio: AudioPolicy::Copy,
            filters: Vec::new(),
            hls: None,
            file_output: None,
            progress_pipe: true,
            log_level: LogLevel::Warning,
        }
    }

    /// Replace the video output spec rendered by this command.
    pub fn video(mut self, spec: VideoOutputSpec) -> Self {
        self.video = spec;
        self
    }

    /// Replace the audio output policy rendered by this command.
    pub fn audio(mut self, spec: AudioPolicy) -> Self {
        self.audio = spec;
        self
    }

    /// Append a video filter to the rendered `-vf` filtergraph.
    pub fn add_filter(mut self, filter: VideoFilter) -> Self {
        self.filters.push(filter);
        self
    }

    /// Append a video filter only when the condition is true.
    pub fn add_filter_if(mut self, cond: bool, filter: VideoFilter) -> Self {
        if cond {
            self.filters.push(filter);
        }
        self
    }

    /// Set HLS output settings rendered by this command.
    pub fn hls(mut self, spec: HlsOutputSpec) -> Self {
        self.hls = Some(spec);
        self.file_output = None;
        self
    }

    /// Set a single-file output path rendered as the final FFmpeg argument.
    pub fn file_output(mut self, path: impl Into<PathBuf>) -> Self {
        self.file_output = Some(path.into());
        self.hls = None;
        self
    }

    /// Set the FFmpeg logging level rendered by this command.
    pub fn log_level(mut self, level: LogLevel) -> Self {
        self.log_level = level;
        self
    }

    /// Render the argv and construct a `tokio::process::Command`.
    pub fn into_command(self) -> Command {
        let mut command = Command::new(&self.binary);
        command.args(self.to_args());
        command
    }

    /// Render FFmpeg arguments after the binary for diagnostics and snapshot tests.
    pub fn to_args(&self) -> Vec<OsString> {
        let mut args = Vec::new();
        push_arg(&mut args, "-hide_banner");
        push_arg(&mut args, "-nostdin");
        push_arg(&mut args, "-loglevel");
        push_arg(&mut args, self.log_level.as_ffmpeg());
        if self.progress_pipe {
            push_arg(&mut args, "-progress");
            push_arg(&mut args, "pipe:1");
        }

        args.extend(self.input.to_args());

        if self.video.codec == VideoCodec::Copy {
            push_arg(&mut args, "-map");
            push_arg(&mut args, "0");
            push_arg(&mut args, "-c");
            push_arg(&mut args, "copy");
        } else {
            push_arg(&mut args, "-map");
            push_arg(&mut args, "0:v:0");
            render_video_args(&self.video, &mut args);

            if !self.filters.is_empty() {
                push_arg(&mut args, "-vf");
                push_arg(&mut args, render_filters(&self.filters));
            }

            render_audio_args(&self.audio, &mut args);
        }

        if let Some(hls) = &self.hls {
            render_hls_args(hls, &mut args);
        } else if let Some(path) = &self.file_output {
            args.push(path.as_os_str().to_owned());
        }

        args
    }
}

impl fmt::Display for FfmpegEncodeCommand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let words = std::iter::once(self.binary.as_os_str().to_string_lossy().into_owned())
            .chain(
                self.to_args()
                    .into_iter()
                    .map(|arg| arg.to_string_lossy().into_owned()),
            )
            .map(|word| shell_words::quote(&word).into_owned())
            .collect::<Vec<_>>();
        f.write_str(&words.join(" "))
    }
}

fn render_video_args(video: &VideoOutputSpec, args: &mut Vec<OsString>) {
    push_arg(args, "-c:v");
    push_arg(args, video_codec_arg(video.codec));

    if video.codec != VideoCodec::Copy {
        if let Some(crf) = video.crf {
            push_arg(args, "-crf");
            push_arg(args, crf.to_string());
        }
        push_arg(args, "-preset");
        push_arg(args, video.preset.as_ffmpeg());
        push_arg(args, "-pix_fmt");
        push_arg(args, pixel_format(video.bit_depth));
    }

    render_color_args(video, args);
}

fn render_color_args(video: &VideoOutputSpec, args: &mut Vec<OsString>) {
    match &video.color {
        ColorOutput::CopyFromInput => {}
        ColorOutput::SdrBt709 => {
            push_arg(args, "-color_primaries");
            push_arg(args, "bt709");
            push_arg(args, "-color_trc");
            push_arg(args, "bt709");
            push_arg(args, "-colorspace");
            push_arg(args, "bt709");
        }
        ColorOutput::Hdr10 {
            master_display,
            max_cll,
        } => {
            push_arg(args, "-color_primaries");
            push_arg(args, "bt2020");
            push_arg(args, "-color_trc");
            push_arg(args, "smpte2084");
            push_arg(args, "-colorspace");
            push_arg(args, "bt2020nc");
            if video.codec == VideoCodec::Hevc {
                push_arg(args, "-x265-params");
                push_arg(
                    args,
                    format!(
                        "master-display={}:max-cll={}",
                        render_master_display(master_display),
                        render_max_cll(max_cll)
                    ),
                );
            }
        }
    }
}

fn render_audio_args(audio: &AudioPolicy, args: &mut Vec<OsString>) {
    match audio {
        AudioPolicy::StereoAac { bitrate_kbps } => {
            push_arg(args, "-map");
            push_arg(args, "0:a:0");
            push_arg(args, "-c:a");
            push_arg(args, "aac");
            push_arg(args, "-ac");
            push_arg(args, "2");
            push_arg(args, "-b:a");
            push_arg(args, format!("{bitrate_kbps}k"));
        }
        AudioPolicy::StereoAacWithSurroundPassthrough { bitrate_kbps } => {
            push_arg(args, "-map");
            push_arg(args, "0:a");
            push_arg(args, "-c:a");
            push_arg(args, "copy");
            push_arg(args, "-c:a:0");
            push_arg(args, "aac");
            push_arg(args, "-ac:a:0");
            push_arg(args, "2");
            push_arg(args, "-b:a:0");
            push_arg(args, format!("{bitrate_kbps}k"));
        }
        AudioPolicy::Copy => {
            push_arg(args, "-map");
            push_arg(args, "0:a?");
            push_arg(args, "-c:a");
            push_arg(args, "copy");
        }
        AudioPolicy::None => {
            push_arg(args, "-an");
        }
    }
}

/// Typed FFmpeg libvmaf invocation rendered to argv, `Command`, or shell form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FfmpegVmafCommand {
    binary: PathBuf,
    reference: InputSpec,
    distorted: InputSpec,
    log_path: PathBuf,
    log_level: LogLevel,
}

impl FfmpegVmafCommand {
    /// Construct a VMAF measurement command for a reference and distorted input.
    pub fn new(binary: impl Into<PathBuf>, reference: InputSpec, distorted: InputSpec) -> Self {
        Self {
            binary: binary.into(),
            reference,
            distorted,
            log_path: PathBuf::from("vmaf.json"),
            log_level: LogLevel::Warning,
        }
    }

    /// Set the libvmaf JSON log path parsed by [`Self::measure`].
    pub fn log_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.log_path = path.into();
        self
    }

    /// Set the FFmpeg logging level rendered by this command.
    pub fn log_level(mut self, level: LogLevel) -> Self {
        self.log_level = level;
        self
    }

    /// Return the libvmaf JSON log path.
    pub fn vmaf_log_path(&self) -> &Path {
        &self.log_path
    }

    /// Render the argv and construct a `tokio::process::Command`.
    pub fn into_command(self) -> Command {
        let mut command = Command::new(&self.binary);
        command.args(self.to_args());
        command
    }

    /// Render FFmpeg arguments after the binary for diagnostics and snapshot tests.
    pub fn to_args(&self) -> Vec<OsString> {
        let mut args = Vec::new();
        push_arg(&mut args, "-hide_banner");
        push_arg(&mut args, "-nostdin");
        push_arg(&mut args, "-loglevel");
        push_arg(&mut args, self.log_level.as_ffmpeg());
        args.extend(self.reference.to_args());
        args.extend(self.distorted.to_args());
        push_arg(&mut args, "-lavfi");
        push_arg(
            &mut args,
            format!(
                "[1:v][0:v]libvmaf=log_path={}:log_fmt=json",
                escape_filter_value(&self.log_path)
            ),
        );
        push_arg(&mut args, "-f");
        push_arg(&mut args, "null");
        push_arg(&mut args, "-");
        args
    }

    /// Run FFmpeg, read the libvmaf JSON log, and return the pooled mean VMAF.
    pub async fn measure(&self, runner: &PipelineRunner) -> Result<f32> {
        if let Some(parent) = self
            .log_path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            tokio::fs::create_dir_all(parent).await?;
        }
        match tokio::fs::remove_file(&self.log_path).await {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(Error::Io(err)),
        }

        let (cancel_tx, cancel_rx) = oneshot::channel();
        let result = runner
            .run_process(self.clone().into_command(), cancel_rx)
            .await;
        drop(cancel_tx);
        result?;

        let json = tokio::fs::read_to_string(&self.log_path).await?;
        parse_vmaf_mean(&json)
    }
}

impl fmt::Display for FfmpegVmafCommand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let words = std::iter::once(self.binary.as_os_str().to_string_lossy().into_owned())
            .chain(
                self.to_args()
                    .into_iter()
                    .map(|arg| arg.to_string_lossy().into_owned()),
            )
            .map(|word| shell_words::quote(&word).into_owned())
            .collect::<Vec<_>>();
        f.write_str(&words.join(" "))
    }
}

#[derive(Debug, Deserialize)]
struct VmafLog {
    pooled_metrics: VmafPooledMetrics,
}

#[derive(Debug, Deserialize)]
struct VmafPooledMetrics {
    vmaf: VmafMetric,
}

#[derive(Debug, Deserialize)]
struct VmafMetric {
    mean: f32,
}

fn parse_vmaf_mean(json: &str) -> Result<f32> {
    let log: VmafLog = serde_json::from_str(json)
        .map_err(|err| Error::VmafFailed(format!("invalid libvmaf json: {err}")))?;
    if !log.pooled_metrics.vmaf.mean.is_finite() {
        return Err(Error::VmafFailed(
            "libvmaf mean score was not finite".to_owned(),
        ));
    }
    Ok(log.pooled_metrics.vmaf.mean)
}

fn escape_filter_value(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "\\\\")
        .replace(':', "\\:")
        .replace('\'', "\\'")
}

fn render_hls_args(hls: &HlsOutputSpec, args: &mut Vec<OsString>) {
    push_arg(args, "-f");
    push_arg(args, "hls");
    push_arg(args, "-hls_segment_type");
    push_arg(args, "fmp4");
    push_arg(args, "-hls_time");
    push_arg(args, render_duration(hls.segment_duration));
    push_arg(args, "-hls_playlist_type");
    push_arg(args, "vod");
    push_arg(args, "-hls_segment_filename");
    args.push(hls.output_dir.join(&hls.segment_filename).into_os_string());
    push_arg(args, "-hls_fmp4_init_filename");
    push_arg(args, hls.init_filename.clone());
    args.push(hls.output_dir.join(&hls.playlist_filename).into_os_string());
}

fn render_filters(filters: &[VideoFilter]) -> String {
    filters
        .iter()
        .map(VideoFilter::as_ffmpeg)
        .collect::<Vec<_>>()
        .join(",")
}

fn video_codec_arg(codec: VideoCodec) -> &'static str {
    match codec {
        VideoCodec::Hevc => "libx265",
        VideoCodec::H264 => "libx264",
        VideoCodec::Av1 => "libsvtav1",
        VideoCodec::Copy => "copy",
    }
}

fn pixel_format(bit_depth: u8) -> &'static str {
    if bit_depth > 8 {
        "yuv420p10le"
    } else {
        "yuv420p"
    }
}

fn render_duration(duration: Duration) -> String {
    if duration.subsec_nanos() == 0 {
        duration.as_secs().to_string()
    } else {
        trim_fractional_seconds(format!(
            "{}.{:09}",
            duration.as_secs(),
            duration.subsec_nanos()
        ))
    }
}

fn render_microseconds(microseconds: u64) -> String {
    let seconds = microseconds / 1_000_000;
    let fractional = microseconds % 1_000_000;
    if fractional == 0 {
        seconds.to_string()
    } else {
        trim_fractional_seconds(format!("{seconds}.{fractional:06}"))
    }
}

fn trim_fractional_seconds(mut value: String) -> String {
    while value.ends_with('0') {
        value.pop();
    }
    value
}

fn push_arg(args: &mut Vec<OsString>, value: impl Into<OsString>) {
    args.push(value.into());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> InputSpec {
        InputSpec::file("/library/Some Movie/source.mkv")
    }

    fn sample_input(path: &str, start_us: u64, duration_us: u64) -> InputSpec {
        InputSpec {
            path: PathBuf::from(path),
            start_us: Some(start_us),
            duration_us: Some(duration_us),
        }
    }

    fn hls_output(name: &str) -> HlsOutputSpec {
        HlsOutputSpec::cmaf_vod(
            format!("/library/Some Movie/transcodes/{name}"),
            Duration::from_secs(6),
        )
    }

    #[test]
    fn snapshot_sdr_1080p_h264_aac_cmaf_software() {
        let command = FfmpegEncodeCommand::new("ffmpeg", input())
            .video(VideoOutputSpec {
                codec: VideoCodec::H264,
                crf: Some(20),
                preset: Preset::Medium,
                bit_depth: 8,
                color: ColorOutput::SdrBt709,
                max_resolution: Some((1920, 1080)),
            })
            .audio(AudioPolicy::StereoAac { bitrate_kbps: 192 })
            .add_filter(VideoFilter::Scale(1920, 1080))
            .hls(hls_output("h264-1080p"));

        insta::assert_snapshot!(format!("{command}"));
    }

    #[test]
    fn snapshot_sdr_1080p_hevc_10_bit_aac_cmaf() {
        let command = FfmpegEncodeCommand::new("ffmpeg", input())
            .video(VideoOutputSpec {
                codec: VideoCodec::Hevc,
                crf: Some(23),
                preset: Preset::Medium,
                bit_depth: 10,
                color: ColorOutput::SdrBt709,
                max_resolution: Some((1920, 1080)),
            })
            .audio(AudioPolicy::StereoAac { bitrate_kbps: 192 })
            .add_filter(VideoFilter::Scale(1920, 1080))
            .hls(hls_output("hevc-1080p"));

        insta::assert_snapshot!(format!("{command}"));
    }

    #[test]
    fn snapshot_hdr10_4k_hevc_preserve() {
        let command = FfmpegEncodeCommand::new("ffmpeg", input())
            .video(VideoOutputSpec {
                codec: VideoCodec::Hevc,
                crf: Some(20),
                preset: Preset::Slow,
                bit_depth: 10,
                color: ColorOutput::Hdr10 {
                    master_display: MasterDisplay {
                        red_x: 34_000,
                        red_y: 16_000,
                        green_x: 13_250,
                        green_y: 34_500,
                        blue_x: 7_500,
                        blue_y: 3_000,
                        white_x: 15_635,
                        white_y: 16_450,
                        min_luminance: 50,
                        max_luminance: 10_000_000,
                    },
                    max_cll: MaxCll {
                        max_content: 1_000,
                        max_average: 400,
                    },
                },
                max_resolution: Some((3840, 2160)),
            })
            .audio(AudioPolicy::StereoAac { bitrate_kbps: 256 })
            .hls(hls_output("hevc-4k-hdr10"));

        insta::assert_snapshot!(format!("{command}"));
    }

    #[test]
    fn snapshot_hdr_to_sdr_1080p_tonemap() {
        let command = FfmpegEncodeCommand::new("ffmpeg", input())
            .video(VideoOutputSpec {
                codec: VideoCodec::H264,
                crf: Some(20),
                preset: Preset::Medium,
                bit_depth: 8,
                color: ColorOutput::SdrBt709,
                max_resolution: Some((1920, 1080)),
            })
            .audio(AudioPolicy::StereoAac { bitrate_kbps: 192 })
            .add_filter(VideoFilter::HdrToSdrTonemap)
            .add_filter(VideoFilter::Scale(1920, 1080))
            .hls(hls_output("h264-1080p-tonemap"));

        insta::assert_snapshot!(format!("{command}"));
    }

    #[test]
    fn snapshot_copy_passthrough_maps_all_streams() {
        let command = FfmpegEncodeCommand::new("ffmpeg", input())
            .video(VideoOutputSpec {
                codec: VideoCodec::Copy,
                crf: None,
                preset: Preset::Medium,
                bit_depth: 10,
                color: ColorOutput::CopyFromInput,
                max_resolution: None,
            })
            .audio(AudioPolicy::Copy)
            .hls(hls_output("original"));

        insta::assert_snapshot!(format!("{command}"));
    }

    #[test]
    fn snapshot_vmaf_measurement() {
        let command = FfmpegVmafCommand::new(
            "ffmpeg",
            sample_input("/library/Some Movie/source.mkv", 25_000_000, 12_000_000),
            InputSpec::file("/tmp/kino-vmaf/sample-0-crf-24.mkv"),
        )
        .log_path("/tmp/kino-vmaf/sample-0-crf-24.vmaf.json");

        insta::assert_snapshot!(format!("{command}"));
    }

    #[test]
    fn parse_vmaf_mean_rejects_missing_mean() {
        let err = parse_vmaf_mean(r#"{"pooled_metrics":{"vmaf":{}}}"#);

        assert!(matches!(err, Err(Error::VmafFailed(_))));
    }
}
