//! Planned output variant types.

use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::{Error, Result, VideoCodec};

/// Stable output variant role produced by an output policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VariantKind {
    /// Source remux or stream copy preserving the original media intent.
    Original,
    /// High-quality primary transcode for modern playback targets.
    High,
    /// Broadly compatible SDR transcode for constrained playback targets.
    Compatibility,
}

impl VariantKind {
    /// Return the stable lowercase identifier for this variant kind.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Original => "original",
            Self::High => "high",
            Self::Compatibility => "compatibility",
        }
    }
}

impl FromStr for VariantKind {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "original" => Ok(Self::Original),
            "high" => Ok(Self::High),
            "compatibility" => Ok(Self::Compatibility),
            other => Err(Error::InvalidVariantKind(other.to_owned())),
        }
    }
}

/// Output container and segmenting family for a planned variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Container {
    /// Fragmented MP4 CMAF output.
    Fmp4Cmaf,
}

impl Container {
    /// Return the stable lowercase identifier for this container.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Fmp4Cmaf => "fmp4_cmaf",
        }
    }
}

/// Output color target for a planned variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColorTarget {
    /// HDR10-compatible BT.2020/PQ output.
    Hdr10,
    /// SDR BT.709 output.
    Sdr,
}

impl ColorTarget {
    /// Return the stable lowercase identifier for this color target.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Hdr10 => "hdr10",
            Self::Sdr => "sdr",
        }
    }
}

impl FromStr for ColorTarget {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "hdr10" => Ok(Self::Hdr10),
            "sdr" => Ok(Self::Sdr),
            other => Err(Error::InvalidColorTarget(other.to_owned())),
        }
    }
}

/// Serializable audio policy selected by the output planner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioPolicyKind {
    /// Produce first-audio-stream stereo AAC.
    StereoAac,
    /// Produce stereo AAC and preserve surround tracks with passthrough.
    StereoAacWithSurroundPassthrough,
    /// Copy source audio streams without re-encoding.
    Copy,
}

impl AudioPolicyKind {
    /// Return the stable lowercase identifier for this audio policy kind.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::StereoAac => "stereo_aac",
            Self::StereoAacWithSurroundPassthrough => "stereo_aac_with_surround_passthrough",
            Self::Copy => "copy",
        }
    }
}

impl FromStr for AudioPolicyKind {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "stereo_aac" => Ok(Self::StereoAac),
            "stereo_aac_with_surround_passthrough" => Ok(Self::StereoAacWithSurroundPassthrough),
            "copy" => Ok(Self::Copy),
            other => Err(Error::InvalidAudioPolicyKind(other.to_owned())),
        }
    }
}

/// A concrete output variant requested by an [`crate::OutputPolicy`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlannedVariant {
    /// Stable output role.
    pub kind: VariantKind,
    /// Requested video codec or copy mode.
    pub codec: VideoCodec,
    /// Output container family.
    pub container: Container,
    /// Planned output width, or source width when omitted.
    pub width: Option<u32>,
    /// Planned output video bit depth.
    pub bit_depth: u8,
    /// Planned output color target.
    pub color: ColorTarget,
    /// Planned audio handling policy.
    pub audio: AudioPolicyKind,
    /// Target VMAF quality, omitted for passthrough variants.
    pub vmaf_target: Option<f32>,
}
