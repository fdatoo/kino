//! Software encoder backend.

use std::path::PathBuf;

use super::{Capabilities, Encoder, EncoderKind, LaneId, VideoCodec};
use crate::pipeline::{
    AudioPolicy, FfmpegEncodeCommand, HlsOutputSpec, InputSpec, VideoFilter, VideoOutputSpec,
};
use crate::plan::{VmafSampleEncoder, VmafTrialEncodeRequest};

/// Software encode inputs used until the planner-owned encode context exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SoftwareEncodeContext {
    /// Source media path passed to FFmpeg.
    pub input_path: PathBuf,
    /// Video output settings rendered through the shared FFmpeg builder.
    pub video: VideoOutputSpec,
    /// Audio output policy rendered through the shared FFmpeg builder.
    pub audio: AudioPolicy,
    /// Video filtergraph atoms rendered in order.
    pub filters: Vec<VideoFilter>,
    /// HLS CMAF output settings rendered through the shared FFmpeg builder.
    pub hls: HlsOutputSpec,
}

/// Software (CPU) encoder backend. Always available.
///
/// Produces HEVC via libx265 and H.264 via libx264 through the shared FFmpeg
/// binary. AV1 (libsvtav1) is declared in [`VideoCodec::Av1`] but not yet
/// supported here -- Phase 5.
pub struct SoftwareEncoder {
    binary: PathBuf,
    capabilities: Capabilities,
}

impl SoftwareEncoder {
    /// Construct a software encoder that resolves `ffmpeg` from `PATH` at runtime.
    pub fn new() -> Self {
        Self::with_binary("ffmpeg")
    }

    /// Construct a software encoder using an explicit FFmpeg binary path.
    pub fn with_binary(binary: impl Into<PathBuf>) -> Self {
        Self {
            binary: binary.into(),
            capabilities: Capabilities::new(
                [VideoCodec::Hevc, VideoCodec::H264, VideoCodec::Copy],
                7680,
                4320,
                true,
                true,
                true,
            ),
        }
    }

    /// Build the FFmpeg command for a software encode context.
    pub fn build_command(&self, ctx: &SoftwareEncodeContext) -> FfmpegEncodeCommand {
        let mut command =
            FfmpegEncodeCommand::new(self.binary.clone(), InputSpec::file(ctx.input_path.clone()))
                .video(ctx.video.clone())
                .audio(ctx.audio.clone());

        for filter in ctx.filters.iter().cloned() {
            command = command.add_filter(filter);
        }

        command.hls(ctx.hls.clone())
    }
}

impl Default for SoftwareEncoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Encoder for SoftwareEncoder {
    fn kind(&self) -> EncoderKind {
        EncoderKind::Software
    }

    fn lane(&self) -> LaneId {
        LaneId::Cpu
    }

    fn capabilities(&self) -> &Capabilities {
        &self.capabilities
    }

    fn supports_codec(&self, codec: VideoCodec, width: u32, height: u32, bit_depth: u8) -> bool {
        self.capabilities.codecs().contains(&codec)
            && width <= self.capabilities.max_width()
            && height <= self.capabilities.max_height()
            && match codec {
                VideoCodec::Hevc | VideoCodec::Copy => bit_depth <= 10,
                VideoCodec::H264 => bit_depth <= 8,
                VideoCodec::Av1 => false,
            }
    }
}

impl VmafSampleEncoder for SoftwareEncoder {
    fn build_vmaf_trial_encode(
        &self,
        request: &VmafTrialEncodeRequest,
    ) -> crate::Result<FfmpegEncodeCommand> {
        let mut command = FfmpegEncodeCommand::new(self.binary.clone(), request.input.clone())
            .video(request.video.clone())
            .audio(AudioPolicy::None);

        for filter in request.filters.iter().cloned() {
            command = command.add_filter(filter);
        }

        Ok(command.file_output(request.output_path.clone()))
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use kino_core::probe::{MasterDisplay, MaxCll};

    use crate::pipeline::{ColorOutput, Preset};

    fn hls_output(name: &str) -> HlsOutputSpec {
        HlsOutputSpec::cmaf_vod(
            format!("/library/Some Movie/transcodes/{name}"),
            Duration::from_secs(6),
        )
    }

    #[test]
    fn software_encoder_exposes_expected_capabilities() {
        let encoder = SoftwareEncoder::new();

        assert_eq!(encoder.kind(), EncoderKind::Software);
        assert_eq!(encoder.lane(), LaneId::Cpu);
        assert!(encoder.supports_codec(VideoCodec::Hevc, 7680, 4320, 10));
        assert!(encoder.supports_codec(VideoCodec::H264, 7680, 4320, 8));
        assert!(encoder.supports_codec(VideoCodec::Copy, 7680, 4320, 10));
        assert!(!encoder.supports_codec(VideoCodec::H264, 1920, 1080, 10));
        assert!(!encoder.supports_codec(VideoCodec::Av1, 1920, 1080, 8));
        assert!(!encoder.supports_codec(VideoCodec::Hevc, 7681, 4320, 10));
    }

    #[test]
    fn snapshot_hevc_10_bit_sdr_1080p_cmaf_vod() {
        let encoder = SoftwareEncoder::new();
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
            hls: hls_output("hevc-1080p"),
        });

        insta::assert_snapshot!(format!("{command}"));
    }

    #[test]
    fn snapshot_h264_8_bit_sdr_1080p_cmaf_vod() {
        let encoder = SoftwareEncoder::new();
        let command = encoder.build_command(&SoftwareEncodeContext {
            input_path: PathBuf::from("/library/Some Movie/source.mkv"),
            video: VideoOutputSpec {
                codec: VideoCodec::H264,
                crf: Some(20),
                preset: Preset::Medium,
                bit_depth: 8,
                color: ColorOutput::SdrBt709,
                max_resolution: Some((1920, 1080)),
            },
            audio: AudioPolicy::StereoAac { bitrate_kbps: 192 },
            filters: vec![VideoFilter::Scale(1920, 1080)],
            hls: hls_output("h264-1080p"),
        });

        insta::assert_snapshot!(format!("{command}"));
    }

    #[test]
    fn snapshot_hdr10_4k_hevc_preserve_cmaf_vod() {
        let encoder = SoftwareEncoder::new();
        let command = encoder.build_command(&SoftwareEncodeContext {
            input_path: PathBuf::from("/library/Some Movie/source.mkv"),
            video: VideoOutputSpec {
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
            },
            audio: AudioPolicy::StereoAac { bitrate_kbps: 256 },
            filters: Vec::new(),
            hls: hls_output("hevc-4k-hdr10"),
        });

        insta::assert_snapshot!(format!("{command}"));
    }

    #[test]
    fn snapshot_hdr_to_sdr_1080p_hevc_8_bit_tonemap_cmaf_vod() {
        let encoder = SoftwareEncoder::new();
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
            hls: hls_output("hevc-1080p-tonemap"),
        });

        insta::assert_snapshot!(format!("{command}"));
    }
}
