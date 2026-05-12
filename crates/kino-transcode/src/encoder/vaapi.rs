//! Linux VA-API encoder backend.

use std::path::PathBuf;

use super::{Capabilities, Encoder, EncoderKind, LaneId, SoftwareEncodeContext, VideoCodec};
use crate::pipeline::{
    FfmpegEncodeCommand, HardwareAccel, InputSpec, VideoFilter, VideoQualityArg,
};

/// Linux VA-API encoder backend.
pub struct VaapiEncoder {
    /// FFmpeg binary used for VA-API encodes.
    pub binary: PathBuf,
    /// DRM render node used by FFmpeg's VA-API device.
    pub render_node: PathBuf,
    /// Static VA-API capability declaration.
    pub capabilities: Capabilities,
}

impl VaapiEncoder {
    /// Construct a VA-API encoder that resolves `ffmpeg` from `PATH` at runtime.
    pub fn new(render_node: impl Into<PathBuf>) -> Self {
        Self::with_binary("ffmpeg", render_node)
    }

    /// Construct a VA-API encoder using an explicit FFmpeg binary path.
    pub fn with_binary(binary: impl Into<PathBuf>, render_node: impl Into<PathBuf>) -> Self {
        Self {
            binary: binary.into(),
            render_node: render_node.into(),
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

    /// Build the FFmpeg command for a VA-API encode context.
    pub fn build_command(&self, ctx: &SoftwareEncodeContext) -> FfmpegEncodeCommand {
        let mut command =
            FfmpegEncodeCommand::new(self.binary.clone(), InputSpec::file(ctx.input_path.clone()))
                .hardware_accel(HardwareAccel::Vaapi {
                    render_node: self.render_node.clone(),
                })
                .video(ctx.video.clone())
                .video_encoder(vaapi_encoder(ctx.video.codec))
                .video_quality_arg(VideoQualityArg::Qp)
                .video_pixel_format("vaapi")
                .without_video_preset()
                .audio(ctx.audio.clone());

        for filter in ctx.filters.iter().map(vaapi_filter) {
            command = command.add_filter(filter);
        }

        command.hls(ctx.hls.clone())
    }
}

impl Encoder for VaapiEncoder {
    fn kind(&self) -> EncoderKind {
        EncoderKind::Vaapi
    }

    fn lane(&self) -> LaneId {
        LaneId::GpuIntel
    }

    fn capabilities(&self) -> &Capabilities {
        &self.capabilities
    }

    fn supports_codec(&self, codec: VideoCodec, width: u32, height: u32, bit_depth: u8) -> bool {
        self.capabilities.codecs().contains(&codec)
            && width <= self.capabilities.max_width()
            && height <= self.capabilities.max_height()
            && (bit_depth <= 8 || self.capabilities.ten_bit())
    }

    fn build_command(&self, ctx: &SoftwareEncodeContext) -> crate::Result<FfmpegEncodeCommand> {
        Ok(VaapiEncoder::build_command(self, ctx))
    }
}

fn vaapi_encoder(codec: VideoCodec) -> &'static str {
    match codec {
        VideoCodec::Hevc => "hevc_vaapi",
        VideoCodec::H264 => "h264_vaapi",
        VideoCodec::Av1 => "av1_vaapi",
        VideoCodec::Copy => "copy",
    }
}

fn vaapi_filter(filter: &VideoFilter) -> VideoFilter {
    match filter {
        VideoFilter::Scale(width, height) => VideoFilter::VaapiScale(*width, *height),
        VideoFilter::HdrToSdrTonemap => VideoFilter::VaapiHdrToSdrTonemap,
        other => other.clone(),
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
    fn vaapi_encoder_exposes_expected_capabilities() {
        let encoder = VaapiEncoder::new("/dev/dri/renderD128");

        assert_eq!(encoder.kind(), EncoderKind::Vaapi);
        assert_eq!(encoder.lane(), LaneId::GpuIntel);
        assert!(encoder.supports_codec(VideoCodec::Hevc, 3840, 2160, 10));
        assert!(encoder.supports_codec(VideoCodec::H264, 3840, 2160, 8));
        assert!(!encoder.supports_codec(VideoCodec::Av1, 3840, 2160, 10));
        assert!(!encoder.supports_codec(VideoCodec::Copy, 3840, 2160, 10));
        assert!(!encoder.supports_codec(VideoCodec::Hevc, 3841, 2160, 10));
    }

    #[test]
    fn snapshot_sdr_hevc() {
        let encoder = VaapiEncoder::new("/dev/dri/renderD128");
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
            hls: hls_output("vaapi-hevc-1080p"),
        });

        insta::assert_snapshot!(format!("{command}"));
    }

    #[test]
    fn snapshot_hdr10_hevc_preserve() {
        let encoder = VaapiEncoder::new("/dev/dri/renderD128");
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
            hls: hls_output("vaapi-hevc-4k-hdr10"),
        });

        insta::assert_snapshot!(format!("{command}"));
    }

    #[test]
    fn snapshot_hdr_to_sdr_tonemap() {
        let encoder = VaapiEncoder::new("/dev/dri/renderD128");
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
            hls: hls_output("vaapi-hevc-1080p-tonemap"),
        });

        insta::assert_snapshot!(format!("{command}"));
    }
}
