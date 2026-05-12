//! macOS VideoToolbox encoder backend.

use std::path::PathBuf;

use super::{Capabilities, Encoder, EncoderKind, LaneId, SoftwareEncodeContext, VideoCodec};
use crate::pipeline::{
    FfmpegEncodeCommand, HardwareAccel, InputSpec, VideoFilter, VideoQualityArg,
};

/// macOS VideoToolbox encoder backend.
pub struct VideoToolboxEncoder {
    /// FFmpeg binary used for VideoToolbox encodes.
    pub binary: PathBuf,
    /// Static VideoToolbox capability declaration.
    pub capabilities: Capabilities,
}

impl VideoToolboxEncoder {
    /// Construct a VideoToolbox encoder that resolves `ffmpeg` from `PATH` at runtime.
    pub fn new() -> Self {
        Self::with_binary("ffmpeg")
    }

    /// Construct a VideoToolbox encoder using an explicit FFmpeg binary path.
    pub fn with_binary(binary: impl Into<PathBuf>) -> Self {
        Self {
            binary: binary.into(),
            capabilities: Capabilities::new(
                [VideoCodec::Hevc, VideoCodec::H264],
                3840,
                2160,
                true,
                true,
                false,
            ),
        }
    }

    /// Build the FFmpeg command for a VideoToolbox encode context.
    pub fn build_command(&self, ctx: &SoftwareEncodeContext) -> FfmpegEncodeCommand {
        let mut command =
            FfmpegEncodeCommand::new(self.binary.clone(), InputSpec::file(ctx.input_path.clone()))
                .hardware_accel(HardwareAccel::VideoToolbox)
                .video(ctx.video.clone())
                .video_encoder(videotoolbox_encoder(ctx.video.codec))
                .video_quality_arg(VideoQualityArg::None)
                .video_pixel_format(videotoolbox_pixel_format(ctx))
                .without_video_preset()
                .audio(ctx.audio.clone());

        for filter in videotoolbox_filters(ctx) {
            command = command.add_filter(filter);
        }

        command.hls(ctx.hls.clone())
    }
}

impl Default for VideoToolboxEncoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Encoder for VideoToolboxEncoder {
    fn kind(&self) -> EncoderKind {
        EncoderKind::VideoToolbox
    }

    fn lane(&self) -> LaneId {
        LaneId::GpuVideoToolbox
    }

    fn capabilities(&self) -> &Capabilities {
        &self.capabilities
    }

    fn supports_codec(&self, codec: VideoCodec, width: u32, height: u32, bit_depth: u8) -> bool {
        self.capabilities.codecs().contains(&codec)
            && width <= self.capabilities.max_width()
            && height <= self.capabilities.max_height()
            && match codec {
                VideoCodec::Hevc => bit_depth <= 10,
                VideoCodec::H264 => bit_depth <= 8,
                VideoCodec::Av1 | VideoCodec::Copy => false,
            }
    }

    fn build_command(&self, ctx: &SoftwareEncodeContext) -> crate::Result<FfmpegEncodeCommand> {
        Ok(VideoToolboxEncoder::build_command(self, ctx))
    }
}

fn videotoolbox_encoder(codec: VideoCodec) -> &'static str {
    match codec {
        VideoCodec::Hevc => "hevc_videotoolbox",
        VideoCodec::H264 => "h264_videotoolbox",
        VideoCodec::Av1 => "av1_videotoolbox",
        VideoCodec::Copy => "copy",
    }
}

fn videotoolbox_pixel_format(ctx: &SoftwareEncodeContext) -> &'static str {
    if ctx
        .filters
        .iter()
        .any(|filter| matches!(filter, VideoFilter::HdrToSdrTonemap))
    {
        "yuv420p"
    } else if ctx.video.codec == VideoCodec::Hevc && ctx.video.bit_depth > 8 {
        "p010le"
    } else {
        "nv12"
    }
}

fn videotoolbox_filters(ctx: &SoftwareEncodeContext) -> Vec<VideoFilter> {
    let mut filters = Vec::with_capacity(ctx.filters.len() + 1);
    filters.push(VideoFilter::HwDownload {
        format: videotoolbox_download_format(ctx).to_owned(),
    });
    filters.extend(ctx.filters.iter().cloned());
    filters
}

fn videotoolbox_download_format(ctx: &SoftwareEncodeContext) -> &'static str {
    if ctx
        .filters
        .iter()
        .any(|filter| matches!(filter, VideoFilter::HdrToSdrTonemap))
        || ctx.video.bit_depth > 8
    {
        "p010le"
    } else {
        "nv12"
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use kino_core::probe::{MasterDisplay, MaxCll};

    use crate::pipeline::{AudioPolicy, ColorOutput, HlsOutputSpec, Preset, VideoOutputSpec};

    fn hls_output(name: &str) -> HlsOutputSpec {
        HlsOutputSpec::cmaf_vod(
            format!("/library/Some Movie/transcodes/{name}"),
            Duration::from_secs(6),
        )
    }

    fn hdr10_color() -> ColorOutput {
        ColorOutput::Hdr10 {
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
        }
    }

    #[test]
    fn videotoolbox_encoder_exposes_expected_capabilities() {
        let encoder = VideoToolboxEncoder::new();

        assert_eq!(encoder.kind(), EncoderKind::VideoToolbox);
        assert_eq!(encoder.lane(), LaneId::GpuVideoToolbox);
        assert!(encoder.supports_codec(VideoCodec::Hevc, 3840, 2160, 10));
        assert!(encoder.supports_codec(VideoCodec::H264, 3840, 2160, 8));
        assert!(!encoder.supports_codec(VideoCodec::H264, 3840, 2160, 10));
        assert!(!encoder.supports_codec(VideoCodec::Av1, 3840, 2160, 10));
        assert!(!encoder.supports_codec(VideoCodec::Copy, 3840, 2160, 10));
        assert!(!encoder.supports_codec(VideoCodec::Hevc, 3841, 2160, 10));
    }

    #[test]
    fn snapshot_sdr_hevc() {
        let encoder = VideoToolboxEncoder::new();
        let command = encoder.build_command(&SoftwareEncodeContext {
            input_path: PathBuf::from("/library/Some Movie/source.mkv"),
            video: VideoOutputSpec {
                codec: VideoCodec::Hevc,
                crf: Some(23),
                preset: Preset::Medium,
                bit_depth: 10,
                color: ColorOutput::SdrBt709,
                max_resolution: Some((1920, 1080)),
            },
            audio: AudioPolicy::StereoAac { bitrate_kbps: 192 },
            filters: vec![VideoFilter::Scale(1920, 1080)],
            hls: hls_output("videotoolbox-hevc-1080p"),
        });

        insta::assert_snapshot!(format!("{command}"));
    }

    #[test]
    fn snapshot_hdr10_hevc_preserve() {
        let encoder = VideoToolboxEncoder::new();
        let command = encoder.build_command(&SoftwareEncodeContext {
            input_path: PathBuf::from("/library/Some Movie/source.mkv"),
            video: VideoOutputSpec {
                codec: VideoCodec::Hevc,
                crf: Some(20),
                preset: Preset::Slow,
                bit_depth: 10,
                color: hdr10_color(),
                max_resolution: Some((3840, 2160)),
            },
            audio: AudioPolicy::StereoAac { bitrate_kbps: 256 },
            filters: Vec::new(),
            hls: hls_output("videotoolbox-hevc-4k-hdr10"),
        });

        insta::assert_snapshot!(format!("{command}"));
    }

    #[test]
    fn snapshot_hdr_to_sdr_tonemap() {
        let encoder = VideoToolboxEncoder::new();
        let command = encoder.build_command(&SoftwareEncodeContext {
            input_path: PathBuf::from("/library/Some Movie/source.mkv"),
            video: VideoOutputSpec {
                codec: VideoCodec::Hevc,
                crf: Some(24),
                preset: Preset::Medium,
                bit_depth: 8,
                color: ColorOutput::SdrBt709,
                max_resolution: Some((1920, 1080)),
            },
            audio: AudioPolicy::StereoAac { bitrate_kbps: 192 },
            filters: vec![VideoFilter::HdrToSdrTonemap, VideoFilter::Scale(1920, 1080)],
            hls: hls_output("videotoolbox-hevc-1080p-tonemap"),
        });

        insta::assert_snapshot!(format!("{command}"));
    }
}
