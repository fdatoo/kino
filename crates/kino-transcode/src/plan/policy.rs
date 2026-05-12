//! Output policy implementations.

use kino_core::{Config, Id, ProbeResult, ProbeVideoStream};

use super::variant::{AudioPolicyKind, ColorTarget, Container, PlannedVariant, VariantKind};
use crate::{Result, VideoCodec};

/// Probe-backed source-file context passed into output policies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceContext {
    /// Durable source-file id assigned by ingestion.
    pub source_file_id: Id,
    /// Probe facts for the source media file.
    pub probe: ProbeResult,
}

/// Selects the output variants Kino should produce for a source file.
pub trait OutputPolicy: Send + Sync {
    /// Plan output variants for the probed source file.
    fn plan(&self, source: &SourceContext) -> Vec<PlannedVariant>;
}

/// Tunable values used by [`DefaultPolicy`].
#[derive(Debug, Clone, PartialEq)]
pub struct DefaultPolicyConfig {
    /// Codec used for the high-quality variant.
    pub high_codec: VideoCodec,
    /// Bit depth used for the high-quality variant.
    pub high_bit_depth: u8,
    /// VMAF target used for the high-quality variant.
    pub high_vmaf_target: f32,
    /// Encoder preset key carried for the high-quality variant.
    pub high_preset: String,
    /// Codec used for the compatibility variant.
    pub compat_codec: VideoCodec,
    /// VMAF target used for the compatibility variant.
    pub compat_vmaf_target: f32,
    /// Maximum compatibility variant height.
    pub compat_max_height: u32,
}

impl Default for DefaultPolicyConfig {
    fn default() -> Self {
        Self {
            high_codec: VideoCodec::Hevc,
            high_bit_depth: 10,
            high_vmaf_target: 95.0,
            high_preset: "medium".to_owned(),
            compat_codec: VideoCodec::H264,
            compat_vmaf_target: 90.0,
            compat_max_height: 1080,
        }
    }
}

/// Default three-variant output policy for Phase 3 transcoding.
#[derive(Debug, Clone, PartialEq)]
pub struct DefaultPolicy {
    config: DefaultPolicyConfig,
}

impl DefaultPolicy {
    /// Construct a default output policy from explicit settings.
    pub fn new(config: DefaultPolicyConfig) -> Self {
        Self { config }
    }

    /// Construct a default output policy from loaded Kino config.
    pub fn from_config(config: &Config) -> Result<Self> {
        let policy = &config.transcode.policy;
        Ok(Self::new(DefaultPolicyConfig {
            high_codec: policy.high.codec.parse()?,
            high_bit_depth: policy.high.bit_depth,
            high_vmaf_target: policy.high.vmaf_target,
            high_preset: policy.high.preset.clone(),
            compat_codec: policy.compat.codec.parse()?,
            compat_vmaf_target: policy.compat.vmaf_target,
            compat_max_height: policy.compat.max_height,
        }))
    }

    /// Return the policy settings.
    pub const fn config(&self) -> &DefaultPolicyConfig {
        &self.config
    }
}

impl Default for DefaultPolicy {
    fn default() -> Self {
        Self::new(DefaultPolicyConfig::default())
    }
}

impl OutputPolicy for DefaultPolicy {
    fn plan(&self, source: &SourceContext) -> Vec<PlannedVariant> {
        let video = source.probe.video_streams.first();
        let color = if video.is_some_and(is_hdr_or_dolby_vision) {
            ColorTarget::Hdr10
        } else {
            ColorTarget::Sdr
        };
        let compat_width = compatibility_width(video, self.config.compat_max_height);

        vec![
            PlannedVariant {
                kind: VariantKind::Original,
                codec: VideoCodec::Copy,
                container: Container::Fmp4Cmaf,
                width: None,
                bit_depth: original_bit_depth(color),
                color,
                audio: AudioPolicyKind::Copy,
                vmaf_target: None,
            },
            PlannedVariant {
                kind: VariantKind::High,
                codec: self.config.high_codec,
                container: Container::Fmp4Cmaf,
                width: None,
                bit_depth: self.config.high_bit_depth,
                color,
                audio: AudioPolicyKind::StereoAacWithSurroundPassthrough,
                vmaf_target: Some(self.config.high_vmaf_target),
            },
            PlannedVariant {
                kind: VariantKind::Compatibility,
                codec: self.config.compat_codec,
                container: Container::Fmp4Cmaf,
                width: Some(compat_width),
                bit_depth: 8,
                color: ColorTarget::Sdr,
                audio: AudioPolicyKind::StereoAac,
                vmaf_target: Some(self.config.compat_vmaf_target),
            },
        ]
    }
}

fn is_hdr_or_dolby_vision(video: &ProbeVideoStream) -> bool {
    video.dolby_vision.is_some()
        || video.master_display.is_some()
        || video
            .color_transfer
            .as_deref()
            .is_some_and(|transfer| matches!(transfer, "smpte2084" | "arib-std-b67"))
}

const fn original_bit_depth(color: ColorTarget) -> u8 {
    match color {
        ColorTarget::Hdr10 => 10,
        ColorTarget::Sdr => 8,
    }
}

fn compatibility_width(video: Option<&ProbeVideoStream>, compat_max_height: u32) -> u32 {
    let max_width = compat_max_height.saturating_mul(16) / 9;
    video
        .and_then(|stream| stream.width)
        .map_or(max_width, |width| width.min(max_width))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use kino_core::{DolbyVision, MasterDisplay, ProbeVideoStream};

    use super::*;

    #[test]
    fn sdr_1080p_source_plans_three_sdr_variants() {
        let source = source_context(video_stream(1920, 1080));

        let variants = DefaultPolicy::default().plan(&source);

        assert_eq!(variants.len(), 3);
        assert_eq!(variants[0].kind, VariantKind::Original);
        assert_eq!(variants[0].codec, VideoCodec::Copy);
        assert_eq!(variants[0].color, ColorTarget::Sdr);
        assert_eq!(variants[0].audio, AudioPolicyKind::Copy);
        assert_eq!(variants[1].kind, VariantKind::High);
        assert_eq!(variants[1].codec, VideoCodec::Hevc);
        assert_eq!(variants[1].bit_depth, 10);
        assert_eq!(variants[1].color, ColorTarget::Sdr);
        assert_eq!(
            variants[1].audio,
            AudioPolicyKind::StereoAacWithSurroundPassthrough
        );
        assert_eq!(variants[1].vmaf_target, Some(95.0));
        assert_eq!(variants[2].kind, VariantKind::Compatibility);
        assert_eq!(variants[2].codec, VideoCodec::H264);
        assert_eq!(variants[2].width, Some(1920));
        assert_eq!(variants[2].bit_depth, 8);
        assert_eq!(variants[2].color, ColorTarget::Sdr);
        assert_eq!(variants[2].audio, AudioPolicyKind::StereoAac);
        assert_eq!(variants[2].vmaf_target, Some(90.0));
    }

    #[test]
    fn hdr10_4k_source_plans_hdr_original_high_and_sdr_compat() {
        let mut video = video_stream(3840, 2160);
        video.color_transfer = Some("smpte2084".to_owned());
        video.master_display = Some(master_display());
        let source = source_context(video);

        let variants = DefaultPolicy::default().plan(&source);

        assert_eq!(variants[0].color, ColorTarget::Hdr10);
        assert_eq!(variants[1].color, ColorTarget::Hdr10);
        assert_eq!(variants[2].color, ColorTarget::Sdr);
        assert_eq!(variants[2].width, Some(1920));
    }

    #[test]
    fn dolby_vision_source_is_treated_as_hdr_for_planning() {
        let mut video = video_stream(3840, 2160);
        video.dolby_vision = Some(DolbyVision {
            profile: 8,
            level: 6,
            rpu_present: true,
            el_present: false,
            bl_present: true,
        });
        let source = source_context(video);

        let variants = DefaultPolicy::default().plan(&source);

        assert_eq!(variants[0].codec, VideoCodec::Copy);
        assert_eq!(variants[0].color, ColorTarget::Hdr10);
        assert_eq!(variants[1].color, ColorTarget::Hdr10);
        assert_eq!(variants[2].color, ColorTarget::Sdr);
    }

    fn source_context(video: ProbeVideoStream) -> SourceContext {
        SourceContext {
            source_file_id: Id::new(),
            probe: ProbeResult {
                source_path: PathBuf::from("/library/source.mkv"),
                container: None,
                title: None,
                duration: None,
                video_streams: vec![video],
                audio_streams: Vec::new(),
                subtitle_streams: Vec::new(),
            },
        }
    }

    fn video_stream(width: u32, height: u32) -> ProbeVideoStream {
        ProbeVideoStream {
            index: 0,
            codec_name: Some("hevc".to_owned()),
            codec_long_name: None,
            width: Some(width),
            height: Some(height),
            language: None,
            color_primaries: Some("bt709".to_owned()),
            color_transfer: Some("bt709".to_owned()),
            color_space: Some("bt709".to_owned()),
            master_display: None,
            max_cll: None,
            dolby_vision: None,
        }
    }

    const fn master_display() -> MasterDisplay {
        MasterDisplay {
            red_x: 34000,
            red_y: 16000,
            green_x: 13250,
            green_y: 34500,
            blue_x: 7500,
            blue_y: 3000,
            white_x: 15635,
            white_y: 16450,
            min_luminance: 50,
            max_luminance: 10000000,
        }
    }
}
