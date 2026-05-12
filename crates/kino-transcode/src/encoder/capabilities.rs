//! Encoder capability declarations.

use std::collections::BTreeSet;

use super::VideoCodec;

/// Static capability declaration for one detected encoder backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capabilities {
    codecs: BTreeSet<VideoCodec>,
    max_width: u32,
    max_height: u32,
    ten_bit: bool,
    hdr10: bool,
    dv_passthrough: bool,
}

impl Capabilities {
    /// Construct a complete capability declaration.
    ///
    /// The constructor takes all fields because backend detection produces a
    /// complete snapshot, and required arguments make accidental default
    /// capabilities harder to create.
    pub fn new(
        codecs: impl IntoIterator<Item = VideoCodec>,
        max_width: u32,
        max_height: u32,
        ten_bit: bool,
        hdr10: bool,
        dv_passthrough: bool,
    ) -> Self {
        Self {
            codecs: codecs.into_iter().collect(),
            max_width,
            max_height,
            ten_bit,
            hdr10,
            dv_passthrough,
        }
    }

    /// Return the supported video codec set.
    pub fn codecs(&self) -> &BTreeSet<VideoCodec> {
        &self.codecs
    }

    /// Return the maximum supported output width in pixels.
    pub const fn max_width(&self) -> u32 {
        self.max_width
    }

    /// Return the maximum supported output height in pixels.
    pub const fn max_height(&self) -> u32 {
        self.max_height
    }

    /// Return whether this encoder can produce 10-bit output.
    pub const fn ten_bit(&self) -> bool {
        self.ten_bit
    }

    /// Return whether this encoder can produce HDR10 output.
    pub const fn hdr10(&self) -> bool {
        self.hdr10
    }

    /// Return whether this encoder can preserve Dolby Vision on stream-copy paths.
    pub const fn dv_passthrough(&self) -> bool {
        self.dv_passthrough
    }
}
