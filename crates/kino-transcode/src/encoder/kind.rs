//! Encoder and lane identifiers.

use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::Error;

/// Concrete FFmpeg encoder backend family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum EncoderKind {
    /// CPU software encoder backend.
    Software,
    /// Intel Quick Sync Video backend.
    Qsv,
    /// Linux VA-API hardware backend.
    Vaapi,
    /// macOS VideoToolbox hardware backend.
    VideoToolbox,
}

impl EncoderKind {
    /// Return the stable lowercase identifier for this encoder kind.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Software => "software",
            Self::Qsv => "qsv",
            Self::Vaapi => "vaapi",
            Self::VideoToolbox => "videotoolbox",
        }
    }
}

impl FromStr for EncoderKind {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "software" => Ok(Self::Software),
            "qsv" => Ok(Self::Qsv),
            "vaapi" => Ok(Self::Vaapi),
            "videotoolbox" => Ok(Self::VideoToolbox),
            other => Err(Error::InvalidEncoderKind(other.to_owned())),
        }
    }
}

/// Resource lane used to serialize work against a constrained encoder resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LaneId {
    /// CPU encode lane.
    Cpu,
    /// Intel GPU encode lane shared by QSV and VA-API.
    GpuIntel,
    /// Apple VideoToolbox encode lane.
    GpuVideoToolbox,
}

impl LaneId {
    /// Return the stable lowercase identifier for this lane.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cpu => "cpu",
            Self::GpuIntel => "gpu_intel",
            Self::GpuVideoToolbox => "gpu_videotoolbox",
        }
    }
}

impl FromStr for LaneId {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "cpu" => Ok(Self::Cpu),
            "gpu_intel" => Ok(Self::GpuIntel),
            "gpu_videotoolbox" => Ok(Self::GpuVideoToolbox),
            other => Err(Error::InvalidLaneId(other.to_owned())),
        }
    }
}
