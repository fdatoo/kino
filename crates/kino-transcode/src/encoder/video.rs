//! Video codec identifiers.

use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::Error;

/// Video codec requested by the transcode planner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum VideoCodec {
    /// HEVC/H.265 encode or copy target, including 10-bit and HDR10-capable variants.
    Hevc,
    /// H.264/AVC encode or copy target used for broad device compatibility.
    H264,
    /// AV1 encode or copy target for high-efficiency modern playback paths.
    Av1,
    /// Video stream copy with no re-encode.
    Copy,
}

impl VideoCodec {
    /// Return the stable lowercase identifier for this codec.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Hevc => "hevc",
            Self::H264 => "h264",
            Self::Av1 => "av1",
            Self::Copy => "copy",
        }
    }
}

impl FromStr for VideoCodec {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "hevc" => Ok(Self::Hevc),
            "h264" => Ok(Self::H264),
            "av1" => Ok(Self::Av1),
            "copy" => Ok(Self::Copy),
            other => Err(Error::InvalidVideoCodec(other.to_owned())),
        }
    }
}
