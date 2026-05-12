//! macOS VideoToolbox encoder backend.

use std::path::PathBuf;

use super::{Capabilities, Encoder, EncoderKind, LaneId, SoftwareEncodeContext, VideoCodec};
use crate::pipeline::{FfmpegEncodeCommand, HardwareVideoEncoder, InputHardwareAccel, InputSpec};
use crate::{Error, Result};

/// macOS VideoToolbox hardware encoder backend.
pub struct VideoToolboxEncoder {
    binary: PathBuf,
    capabilities: Capabilities,
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
    pub fn build_command(&self, ctx: &SoftwareEncodeContext) -> Result<FfmpegEncodeCommand> {
        let hardware_encoder = hardware_encoder_for(ctx.video.codec, ctx.video.bit_depth)?;
        let mut command =
            FfmpegEncodeCommand::new(self.binary.clone(), InputSpec::file(ctx.input_path.clone()))
                .input_hardware_accel(InputHardwareAccel::VideoToolbox)
                .hardware_video_encoder(hardware_encoder)
                .video(ctx.video.clone())
                .audio(ctx.audio.clone());

        for filter in ctx.filters.iter().cloned() {
            command = command.add_filter(filter);
        }

        Ok(command.hls(ctx.hls.clone()))
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

    fn build_command(&self, ctx: &SoftwareEncodeContext) -> Result<FfmpegEncodeCommand> {
        VideoToolboxEncoder::build_command(self, ctx)
    }
}

fn hardware_encoder_for(codec: VideoCodec, bit_depth: u8) -> Result<HardwareVideoEncoder> {
    match (codec, bit_depth) {
        (VideoCodec::Hevc, 0..=10) => Ok(HardwareVideoEncoder::HevcVideoToolbox),
        (VideoCodec::H264, 0..=8) => Ok(HardwareVideoEncoder::H264VideoToolbox),
        _ => Err(Error::UnsupportedVideoToolboxCodec { codec, bit_depth }),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use kino_core::probe::{MasterDisplay, MaxCll};

    use crate::pipeline::{
        AudioPolicy, ColorOutput, HlsOutputSpec, Preset, VideoFilter, VideoOutputSpec,
    };

    fn hls_output(name: &str) -> HlsOutputSpec {
        HlsOutputSpec::cmaf_vod(
            format!("/library/Some Movie/transcodes/{name}"),
            Duration::from_secs(6),
        )
    }

    #[test]
    fn videotoolbox_encoder_exposes_expected_capabilities() {
        let encoder = VideoToolboxEncoder::new();

        assert_eq!(encoder.kind(), EncoderKind::VideoToolbox);
        assert_eq!(encoder.lane(), LaneId::GpuVideoToolbox);
        assert!(encoder.supports_codec(VideoCodec::Hevc, 3840, 2160, 10));
        assert!(encoder.supports_codec(VideoCodec::H264, 3840, 2160, 8));
        assert!(!encoder.supports_codec(VideoCodec::H264, 1920, 1080, 10));
        assert!(!encoder.supports_codec(VideoCodec::Copy, 3840, 2160, 10));
        assert!(!encoder.supports_codec(VideoCodec::Av1, 1920, 1080, 8));
        assert!(!encoder.supports_codec(VideoCodec::Hevc, 3841, 2160, 10));
    }

    #[test]
    fn snapshot_sdr_hevc_1080p_cmaf_vod() -> crate::Result<()> {
        let encoder = VideoToolboxEncoder::new();
        let command = encoder.build_command(&SoftwareEncodeContext {
            input_path: PathBuf::from("/library/Some Movie/source.mkv"),
            video: VideoOutputSpec {
                codec: VideoCodec::Hevc,
                crf: Some(23),
                preset: Preset::Medium,
                bit_depth: 8,
                color: ColorOutput::SdrBt709,
                max_resolution: Some((1920, 1080)),
            },
            audio: AudioPolicy::StereoAac { bitrate_kbps: 192 },
            filters: vec![VideoFilter::Scale(1920, 1080)],
            hls: hls_output("hevc-1080p-videotoolbox"),
        })?;

        insta::assert_snapshot!(format!("{command}"));
        Ok(())
    }

    #[test]
    fn snapshot_hdr10_hevc_preserve_cmaf_vod() -> crate::Result<()> {
        let encoder = VideoToolboxEncoder::new();
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
            hls: hls_output("hevc-4k-hdr10-videotoolbox"),
        })?;

        insta::assert_snapshot!(format!("{command}"));
        Ok(())
    }

    #[test]
    fn snapshot_hdr_to_sdr_hevc_1080p_tonemap_cmaf_vod() -> crate::Result<()> {
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
            hls: hls_output("hevc-1080p-tonemap-videotoolbox"),
        })?;

        insta::assert_snapshot!(format!("{command}"));
        Ok(())
    }
}
